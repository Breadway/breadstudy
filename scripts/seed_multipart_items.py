#!/usr/bin/env python3
"""Seed the authored multi-part exam items (STUDY-44).

Every part carries its own prompt, answer, marks and rubric. Each fixture
includes a SymPy `check` expression that must evaluate to true before the
item is inserted, so an incorrect part cannot enter the bank in `verified`
state. The seed is idempotent: an existing item id or prompt hash is skipped.

Run inside the image (the same way generate_items.py is run):

  docker run --rm --network host --entrypoint python3 \
    -v breadstudy_breadstudy-data:/var/lib/breadstudy \
    breadstudy:local /app/scripts/seed_multipart_items.py

Pass --database to target a scratch copy instead of the live bank.
"""

import argparse
import hashlib
import json
import os
import sqlite3
import sys
from pathlib import Path

import sympy as sp

DEFAULT_DB = os.environ.get("BREADSTUDY_DATABASE_PATH", "/var/lib/breadstudy/breadstudy.db")
FIXTURES = Path(__file__).with_name("multipart_items.json")

SAFE_GLOBALS = {
    "__builtins__": {},
    "sp": sp,
    "x": sp.Symbol("x"),
    "abs": abs,
    "float": float,
    "int": int,
    "round": round,
    "sorted": sorted,
}


def prompt_hash(text: str) -> str:
    normalized = "".join(text.lower().split())
    return hashlib.sha256(normalized.encode()).hexdigest()[:16]


def verify_part(part: dict) -> str:
    check = str(part.get("check") or "").strip()
    if not check:
        raise ValueError("part %s has no SymPy check" % part.get("label"))
    result = eval(check, SAFE_GLOBALS)  # noqa: S307 - authored fixtures only
    if not bool(result):
        raise ValueError("part %s failed SymPy check: %s" % (part.get("label"), check))
    expr = str(part.get("answer_expr") or "").strip()
    if not expr:
        raise ValueError("part %s has no answer_expr" % part.get("label"))
    sp.sympify(expr)
    return check


def ensure_schema(connection: sqlite3.Connection) -> None:
    connection.executescript(
        """
        CREATE TABLE IF NOT EXISTS item_parts (
            item_id TEXT NOT NULL REFERENCES items(id),
            label TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            prompt TEXT NOT NULL,
            answer TEXT NOT NULL,
            marks REAL NOT NULL DEFAULT 0,
            rubric TEXT NOT NULL DEFAULT '[]',
            PRIMARY KEY (item_id, label)
        );
        CREATE INDEX IF NOT EXISTS item_parts_item_ordinal ON item_parts(item_id, ordinal);
        """
    )
    columns = {row[1] for row in connection.execute("PRAGMA table_info(items)")}
    if "mark_scheme" not in columns:
        connection.execute("ALTER TABLE items ADD COLUMN mark_scheme TEXT")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--database", default=DEFAULT_DB)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    fixtures = json.loads(FIXTURES.read_text())
    connection = sqlite3.connect(args.database, timeout=30)
    connection.row_factory = sqlite3.Row
    ensure_schema(connection)

    inserted = 0
    skipped = 0
    for item in fixtures["items"]:
        item_id = item["id"]
        exists = connection.execute("SELECT 1 FROM items WHERE id = ?", (item_id,)).fetchone()
        digest = prompt_hash(item["prompt"])
        duplicate = connection.execute(
            "SELECT 1 FROM items WHERE dedupe_hash = ?", (digest,)
        ).fetchone()
        if exists or duplicate:
            skipped += 1
            continue
        if not item.get("parts"):
            raise ValueError("item %s has no parts" % item_id)

        checks = {part["label"]: verify_part(part) for part in item["parts"]}
        marks_total = sum(float(part["marks"]) for part in item["parts"])
        detail = json.dumps({"method": "sympy-multipart", "marks_total": marks_total, "checks": checks})
        if args.dry_run:
            print("[verified] %s (%d parts, %s marks)" % (item_id, len(item["parts"]), marks_total))
            continue

        connection.execute(
            "INSERT INTO items "
            "(id, skill_id, prompt, answer, rubric, hints, mark_scheme, "
            " verify_method, verify_detail, dedupe_hash, status) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,'verified')",
            (
                item_id,
                item["skill_id"],
                item["prompt"],
                item["answer"],
                json.dumps({"method": [], "final": {"marks": 0, "answer": item["answer"]}}),
                json.dumps(item["hints"]),
                json.dumps(item["mark_scheme"]),
                "sympy-multipart",
                detail,
                digest,
            ),
        )
        for ordinal, part in enumerate(item["parts"]):
            connection.execute(
                "INSERT INTO item_parts(item_id, label, ordinal, prompt, answer, marks, rubric) "
                "VALUES (?,?,?,?,?,?,?)",
                (
                    item_id,
                    part["label"],
                    ordinal,
                    part["prompt"],
                    part["answer"],
                    float(part["marks"]),
                    json.dumps(part.get("rubric") or {}),
                ),
            )
        inserted += 1

    if not args.dry_run:
        connection.commit()
    print(
        "seed_multipart_items: inserted %d, skipped %d, parts verified with SymPy"
        % (inserted, skipped)
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print("seed_multipart_items failed: %s" % error, file=sys.stderr)
        raise
