#!/usr/bin/env python3
"""Generate verified practice items per skill into the SQLite item bank.

This is the database-backed counterpart to pipelines/nightly (which writes a
JSON file). For every skill it asks the configured model for original problems,
checks each answer with SymPy, attaches one SearXNG source, and inserts the
result. Answers that cannot be checked are stored as review_required instead of
being silently trusted.

Run inside the image so python3 and sympy are present:

  docker run --rm --entrypoint python3 \
    -v breadstudy_breadstudy-data:/var/lib/breadstudy \
    -v "$HOME/.config/breadstudy/opencode-go.key:/etc/breadstudy/llm.key:ro" \
    breadstudy:local /app/generate_items.py --per-skill 4
"""

import argparse
import hashlib
import json
import os
import sqlite3
import sys
import urllib.parse
import urllib.request
import uuid
from datetime import datetime, timezone

import sympy as sp

BASE_URL = os.environ.get("BREADSTUDY_LLM_BASE_URL", "https://opencode.ai/zen/go/v1")
MODEL = os.environ.get("BREADSTUDY_LLM_MODEL", "deepseek-v4.1-flash")
DB = os.environ.get("BREADSTUDY_DATABASE_PATH", "/var/lib/breadstudy/breadstudy.db")
SEARX = os.environ.get("BREADSTUDY_SEARXNG", "http://127.0.0.1:7102/search")
SESSION = os.environ.get("BREADSTUDY_LLM_SESSION", str(uuid.uuid4()))

SYSTEM = (
    "You write Year 10 Australian maths practice problems. Return ONLY JSON."
)

PROMPT = """Write {count} original practice problems for the skill "{name}" (strand: {strand}).

Return JSON: {{"items":[{{"prompt":str,"answer_expr":str,"answer_display":str,
"hints":[str,str,str],"steps":[str,str,str],"difficulty":int}}]}}

Rules:
- answer_expr must be a SymPy expression with no free symbols, for example "45000", "sqrt(2)", "3*pi/4". It must evaluate to the correct answer.
- answer_display is what a student would write, including units where relevant.
- hints escalate: the first must not reveal the answer or the final formula result.
- steps are three short method criteria a marker could award.
- One self-contained problem each, no diagrams, no multiple choice, no proofs.
- difficulty is 1 to 5 and must vary across the set."""


def api_key() -> str:
    if os.environ.get("BREADSTUDY_LLM_API_KEY"):
        return os.environ["BREADSTUDY_LLM_API_KEY"].strip()
    for path in (
        os.environ.get("BREADSTUDY_LLM_API_KEY_FILE"),
        "/etc/breadstudy/llm.key",
        os.path.expanduser("~/.config/breadstudy/opencode-go.key"),
    ):
        if path and os.path.exists(path):
            with open(path) as handle:
                key = handle.read().strip()
            if key:
                return key
    raise SystemExit("no LLM API key found")


def chat(prompt: str, key: str) -> dict:
    body = {
        "model": MODEL,
        "temperature": 0.7,
        "response_format": {"type": "json_object"},
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": prompt},
        ],
    }
    request = urllib.request.Request(
        BASE_URL.rstrip("/") + "/chat/completions",
        data=json.dumps(body).encode(),
        headers={
            "authorization": "Bearer " + key,
            "content-type": "application/json",
            "x-opencode-session": SESSION,
        },
    )
    with urllib.request.urlopen(request, timeout=180) as response:
        payload = json.loads(response.read())
    content = payload["choices"][0]["message"]["content"]
    return json.loads(content)


def search_source(query: str) -> tuple:
    url = SEARX + "?" + urllib.parse.urlencode({"q": query, "format": "json"})
    try:
        with urllib.request.urlopen(url, timeout=20) as response:
            results = json.loads(response.read()).get("results", [])
    except Exception:
        return None, None, None
    if not results:
        return None, None, None
    first = results[0]
    return (
        first.get("url"),
        (first.get("content") or "")[:400],
        datetime.now(timezone.utc).strftime("%Y-%m-%d"),
    )


def normalize(text: str) -> str:
    return "".join(text.lower().split())


def prompt_hash(text: str) -> str:
    return hashlib.sha256(normalize(text).encode()).hexdigest()[:16]


def verify_answer(expr_text: str):
    """Return the float value if the expression is checkable, else None."""
    try:
        expr = sp.sympify(expr_text, evaluate=True)
    except Exception:
        return None
    if expr.free_symbols:
        return None
    try:
        value = float(expr.evalf())
    except Exception:
        return None
    if value != value:  # NaN
        return None
    return value


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--per-skill", type=int, default=4)
    parser.add_argument("--skill", action="append", default=None)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    key = api_key()
    connection = sqlite3.connect(DB, timeout=30)
    connection.row_factory = sqlite3.Row

    skills = connection.execute(
        "SELECT id, strand, name FROM skills ORDER BY strand, name"
    ).fetchall()
    if args.skill:
        wanted = set(args.skill)
        skills = [s for s in skills if s["id"] in wanted]

    existing = {prompt_hash(row["prompt"]) for row in connection.execute("SELECT prompt FROM items")}

    inserted = 0
    verified = 0
    review = 0
    failed = 0

    for skill in skills:
        try:
            reply = chat(
                PROMPT.format(
                    count=args.per_skill,
                    name=skill["name"],
                    strand=skill["strand"],
                ),
                key,
            )
        except Exception as error:
            print("skip %s: model error %s" % (skill["id"], error), file=sys.stderr)
            failed += 1
            continue

        items = reply.get("items") or []
        source_url, source_snippet, fetched_at = search_source(
            "%s maths practice problems" % skill["name"]
        )

        for item in items:
            prompt = str(item.get("prompt") or "").strip()
            display = str(item.get("answer_display") or "").strip()
            expr = str(item.get("answer_expr") or "").strip()
            if not prompt or not display or not expr:
                continue
            digest = prompt_hash(prompt)
            if digest in existing:
                continue
            hints = [str(h) for h in (item.get("hints") or [])][:3]
            steps = [str(s) for s in (item.get("steps") or [])][:3]
            if len(hints) < 2 or len(steps) < 2:
                continue

            checkable = verify_answer(expr) is not None
            status = "verified" if (checkable and source_url) else "review_required"
            rubric = {
                "method": [
                    {"id": "m%d" % (n + 1), "description": text, "marks": 1}
                    for n, text in enumerate(steps)
                ],
                "final": {"marks": 1, "answer": display},
            }
            item_id = str(uuid.uuid4())
            if args.dry_run:
                print("[%s] %s -> %s" % (status, prompt[:70], display))
            else:
                connection.execute(
                    "INSERT INTO items (id, skill_id, prompt, answer, rubric, hints, source_url, source_snippet, fetched_at, status) "
                    "VALUES (?,?,?,?,?,?,?,?,?,?)",
                    (
                        item_id,
                        skill["id"],
                        prompt,
                        display,
                        json.dumps(rubric),
                        json.dumps(hints),
                        source_url,
                        source_snippet,
                        fetched_at,
                        status,
                    ),
                )
            existing.add(digest)
            inserted += 1
            if status == "verified":
                verified += 1
            else:
                review += 1

    if not args.dry_run:
        connection.commit()
    print(
        "inserted %d (verified %d, review_required %d), skill failures %d"
        % (inserted, verified, review, failed)
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
