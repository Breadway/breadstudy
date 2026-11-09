# Nightly generation pipeline for breadstudy (STUDY-11).
#
# The 02:00 run picks due skills, generates 2-3 items per skill for the
# week, verifies every item with SymPy, dedupes against existing items,
# prefetches one web source per skill via SearXNG (127.0.0.1:7102), and
# pre-renders TTS audio for prompts and hints.
#
# Item math is parameterized Volume/SA content (Methods Pathway Test 6
# focus), so SymPy verification is exact rather than model-judged. The
# generator is deterministic per (date, skill): re-runs produce the same
# set, and dedupe keeps the bank stable.

from __future__ import annotations

import base64
import hashlib
import json
import os
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

SEARXNG_URL_DEFAULT = "http://127.0.0.1:7102"
BREEZE_URL_DEFAULT = "http://127.0.0.1:7312"
ITEMS_PER_SKILL = 3

SKILL_QUERIES = {
    "volume-prism-cylinder": "volume of a cylinder formula with example",
    "volume-cone-pyramid-sphere": "volume of a cone formula with example",
    "surface-area": "surface area of a cylinder formula with example",
    "unit-conversion-scaling": "unit conversion cubic centimetres to litres",
}

# (radius, height) variants per skill, deterministic and hand-checkable.
VARIANTS = {
    "volume-prism-cylinder": [(3, 7), (5, 4), (2, 9)],
    "volume-cone-pyramid-sphere": [(3, 6), (4, 5), (2, 8)],
    "surface-area": [(3, 7), (4, 6), (2, 5)],
    "unit-conversion-scaling": [(10, 10), (20, 5), (5, 40)],
}


def utcnow_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def normalize_prompt(prompt: str) -> str:
    return " ".join(prompt.strip().lower().split())


def item_hash(prompt: str) -> str:
    return hashlib.sha1(normalize_prompt(prompt).encode()).hexdigest()[:16]


def sympy_check(expr_text: str, expected: float, tol: float = 0.05) -> bool:
    """Verify a SymPy expression evaluates to the expected value."""
    try:
        import sympy
        value = float(sympy.N(sympy.sympify(expr_text)))
        return abs(value - expected) <= tol
    except Exception:
        return False


@dataclass
class Item:
    id: str
    skill_id: str
    prompt: str
    answer_expr: str
    answer_value: float
    hints: list
    source_url: str = ""
    source_snippet: str = ""
    fetched_at: str = ""
    audio: list = field(default_factory=list)


def build_items(skill_id: str, date_key: str) -> list[Item]:
    """Deterministic 2-3 items per skill. All SymPy-verifiable."""
    variants = VARIANTS.get(skill_id, [])[:ITEMS_PER_SKILL]
    items: list[Item] = []
    for i, (a, b) in enumerate(variants):
        if skill_id == "volume-prism-cylinder":
            prompt = ("A cylinder has radius %d cm and height %d cm. "
                      "Find its volume in cubic cm." % (a, b))
            expr = "pi*%d**2*%d" % (a, b)
            import math
            value = math.pi * a * a * b
            hints = ["Write V = pi r squared h first.",
                     "Substitute r = %d and h = %d, then square the radius." % (a, b),
                     "Final step only: %d pi cubic cm, about %.1f cm cubed." % (a * a * b, value)]
        elif skill_id == "volume-cone-pyramid-sphere":
            prompt = ("A cone has radius %d cm and height %d cm. "
                      "Find its volume in cubic cm." % (a, b))
            expr = "pi*%d**2*%d/3" % (a, b)
            import math
            value = math.pi * a * a * b / 3.0
            hints = ["Write V = one third pi r squared h first.",
                     "Substitute r = %d and h = %d, then divide by 3." % (a, b),
                     "Final step only: about %.1f cm cubed." % value]
        elif skill_id == "surface-area":
            prompt = ("A closed cylinder has radius %d cm and height %d cm. "
                      "Find its total surface area in square cm." % (a, b))
            expr = "2*pi*%d*(%d+%d)" % (a, a, b)
            import math
            value = 2 * math.pi * a * (a + b)
            hints = ["Write A = 2 pi r (r + h) for a closed cylinder.",
                     "Substitute r = %d and h = %d." % (a, b),
                     "Final step only: about %.1f square cm." % value]
        else:  # unit-conversion-scaling
            prompt = ("A rectangular tank measures %d cm by %d cm by 10 cm. "
                      "Find its capacity in litres." % (a, b))
            expr = "%d*%d*10/1000" % (a, b)
            value = a * b * 10 / 1000.0
            hints = ["Volume in cubic cm first: length times width times height.",
                     "Convert: 1000 cubic cm is 1 litre.",
                     "Final step only: %.2f litres." % value]
        items.append(Item(id="%s-%s-%d" % (skill_id, date_key, i), skill_id=skill_id,
                          prompt=prompt, answer_expr=expr, answer_value=value, hints=hints))
    return items


def fetch_source(skill_id: str, base_url: str = SEARXNG_URL_DEFAULT,
                 opener=None) -> tuple[str, str, str]:
    """Prefetch one web source via SearXNG. Returns (url, snippet, date)."""
    query = SKILL_QUERIES.get(skill_id, skill_id)
    params = urllib.parse.urlencode({"q": query, "format": "json"})
    url = base_url.rstrip("/") + "/search?" + params
    try:
        if opener is not None:
            data = opener(url)
        else:
            with urllib.request.urlopen(url, timeout=15) as res:
                data = json.loads(res.read().decode())
        results = data.get("results", [])
        if not results:
            return "", "", utcnow_iso()
        top = results[0]
        return (top.get("url", ""), (top.get("content", "") or "")[:280], utcnow_iso())
    except Exception:
        return "", "", utcnow_iso()


@dataclass
class TTSClient:
    """Pre-render audio for prompts and hints.
    Backend order: injected synth (tests), breeze-server speak API when
    BREEZE_URL is set (default http://127.0.0.1:7312, merged voice track
    broker), otherwise subtitle markers so the session continues text-only
    (null-TTS fallback)."""

    out_dir: Path
    synth: object = None  # (text, dest_path) -> bool
    backend: str = "subtitle"
    fallback: bool = False

    def render(self, text: str, dest: Path) -> str:
        dest.parent.mkdir(parents=True, exist_ok=True)
        if self.synth is not None:
            ok = bool(self.synth(text, dest))
            return "rendered" if ok else "pending"
        breeze = os.environ.get("BREEZE_URL", BREEZE_URL_DEFAULT)
        if breeze:
            try:
                payload = json.dumps({"text": text}).encode()
                req = urllib.request.Request(breeze.rstrip("/") + "/v1/speak",
                                             data=payload,
                                             headers={"Content-Type": "application/json"})
                with urllib.request.urlopen(req, timeout=60) as res:
                    reply = json.loads(res.read().decode())
                wav = base64.b64decode(reply["audio_base64"])
                if len(wav) < 44 or wav[0:4] != b"RIFF":
                    raise ValueError("not a WAV document")
                dest.write_bytes(wav)
                self.backend = "breeze-server"
                self.fallback = bool(reply.get("fallback", False))
                return "rendered"
            except Exception:
                self.fallback = False
                pass
        dest.with_suffix(".txt").write_text(text)
        return "pending"


@dataclass
class PipelineResult:
    items: list[dict]
    skipped_dupes: int
    failed_verify: int


def run_nightly(due_skills: list[str], existing: list[dict], date_key: str,
                audio_dir: Path, tts: TTSClient | None = None,
                source_fetcher=None) -> PipelineResult:
    """One unattended run over the due skills. Pure logic, injectable IO."""
    seen = {item_hash(e.get("prompt", "")) for e in existing}
    tts = tts or TTSClient(out_dir=audio_dir)
    out: list[dict] = []
    dupes = 0
    failed = 0
    for skill_id in due_skills:
        url, snippet, fetched_at = (source_fetcher(skill_id) if source_fetcher
                                    else fetch_source(skill_id))
        for item in build_items(skill_id, date_key):
            if item_hash(item.prompt) in seen:
                dupes += 1
                continue
            if not sympy_check(item.answer_expr, item.answer_value):
                failed += 1
                continue
            seen.add(item_hash(item.prompt))
            audio_files = []
            for n, text in enumerate([item.prompt] + item.hints):
                dest = audio_dir / (item.id + "-part%d.wav" % n)
                status = tts.render(text, dest)
                audio_files.append({"file": dest.name, "status": status,
                                    "backend": tts.backend,
                                    "fallback": tts.fallback})
            out.append({"id": item.id, "skill_id": item.skill_id, "prompt": item.prompt,
                        "answer_expr": item.answer_expr, "answer_value": item.answer_value,
                        "hints": item.hints, "source_url": url, "source_snippet": snippet,
                        "fetched_at": fetched_at, "audio": audio_files})
    return PipelineResult(items=out, skipped_dupes=dupes, failed_verify=failed)
