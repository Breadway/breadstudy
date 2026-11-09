# Weekly review, vault write-back, and assessment countdown (STUDY-15).
#
# The review generates from real mastery, error, and session data passed in
# as plain structures (the mastery store lives server-side; this module
# takes its JSON export so the review never invents numbers).
#
# Vault writes never silently rewrite notes. Approved explanations land
# either in a marked agent section inside the target note or as a new file
# under an _agent folder. Existing bytes outside the markers are preserved
# exactly. A small review queue stages entries before they touch the vault.
#
# Countdown covers Tests 1-7 from the Methods Pathway Overview with Test 6
# (2026-11-10) first. Other test dates are not published in the overview,
# so they show in urgency order with TBA until the planner sets them.

from __future__ import annotations

import json
from dataclasses import dataclass, field
from datetime import date, datetime, timezone
from pathlib import Path

AGENT_START = "<!-- breadstudy-agent-start -->"
AGENT_END = "<!-- breadstudy-agent-end -->"

TEST_ORDER = [6, 1, 2, 3, 4, 5, 7]
TEST_TOPICS = {
    1: "Algebraic Techniques",
    2: "Statistics and Probability",
    3: "Linear Inequalities and Simultaneous Equations",
    4: "Pythagoras, Trigonometry, Similarity",
    5: "Non-linear Relationships and Equations",
    6: "Volume, Capacity and Surface Area",
    7: "Bivariate Data and Boxplots",
}
TEST_DATES = {6: "2026-11-10"}

PUZZLES = [
    {"q": "I am an odd number. Take away one letter and I become even. What am I?",
     "a": "Seven (take away the s)."},
    {"q": "What has a face and two hands but no arms or legs?",
     "a": "A clock."},
    {"q": "The more of me you take, the more you leave behind. What am I?",
     "a": "Footsteps."},
]


@dataclass
class SkillMastery:
    skill_id: str
    mastery: float
    attempts: int = 0
    correct: int = 0


@dataclass
class AttemptRecord:
    skill_id: str
    correct: bool
    error_tags: list[str] = field(default_factory=list)


def summarize_errors(attempts: list[AttemptRecord]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for attempt in attempts:
        if not attempt.correct:
            for tag in attempt.error_tags:
                counts[tag] = counts.get(tag, 0) + 1
    return counts


def session_stats(events: list[dict]) -> dict:
    starts = sum(1 for e in events if e.get("kind") in ("session-start", "ack"))
    skips = sum(1 for e in events if e.get("kind") == "skip")
    bails = sum(1 for e in events if e.get("kind") == "bailed")
    pauses = sum(1 for e in events if e.get("kind") == "pause-nudge")
    return {"sessions": starts, "skips": skips, "bails": bails, "pauses": pauses}


def countdown(today: date | None = None) -> list[dict]:
    """Tests 1-7 with Test 6 first. Other dates are TBA."""
    ref = today or datetime.now(timezone.utc).date()
    rows = []
    for number in TEST_ORDER:
        topic = TEST_TOPICS[number]
        iso = TEST_DATES.get(number, "")
        if iso:
            target = date.fromisoformat(iso)
            days = (target - ref).days
            when = iso + (" (in %d days)" % days if days >= 0 else " (%d days ago)" % -days)
        else:
            days = 10 ** 6
            when = "TBA"
        rows.append({"test": number, "topic": topic, "date": when, "_sort": days if iso else days + number})
    rows.sort(key=lambda r: (r["_sort"], TEST_ORDER.index(r["test"])))
    for row in rows:
        del row["_sort"]
    return rows


def generate_review(mastery: list[SkillMastery], attempts: list[AttemptRecord],
                    events: list[dict], week_key: str) -> str:
    errors = summarize_errors(attempts)
    stats = session_stats(events)
    lines = ["# Weekly review " + week_key, ""]
    lines.append("Sessions started: %d. Skips: %d. Bails: %d. Off-task pauses: %d." % (
        stats["sessions"], stats["skips"], stats["bails"], stats["pauses"]))
    lines.append("")
    lines.append("## Mastery")
    for skill in sorted(mastery, key=lambda s: s.mastery):
        rate = ""
        if skill.attempts:
            rate = " (%d/%d correct)" % (skill.correct, skill.attempts)
        lines.append("- %s: %.2f%s" % (skill.skill_id, skill.mastery, rate))
    lines.append("")
    lines.append("## Error patterns")
    if errors:
        for tag, count in sorted(errors.items(), key=lambda kv: -kv[1]):
            lines.append("- %s: %d" % (tag, count))
    else:
        lines.append("- none recorded this week")
    lines.append("")
    lines.append("## Countdown")
    for row in countdown():
        lines.append("- Test %d %s: %s" % (row["test"], row["topic"], row["date"]))
    lines.append("")
    lines.append("## Reward")
    puzzle = PUZZLES[hash(week_key) % len(PUZZLES)]
    lines.append("Puzzle warm-up (post-block reward): " + puzzle["q"])
    return "\n".join(lines) + "\n"


@dataclass
class QueuedEntry:
    id: str
    target_note: str  # vault-relative path, e.g. Area Volume and Surface Area.md
    title: str
    body_latex: str


@dataclass
class ReviewQueue:
    """Small staging queue. Nothing touches the vault until approved."""

    path: Path

    def stage(self, entry: QueuedEntry) -> None:
        items = self.pending()
        items.append({"id": entry.id, "target_note": entry.target_note,
                      "title": entry.title, "body_latex": entry.body_latex,
                      "status": "pending"})
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.path.write_text(json.dumps(items, indent=2))

    def pending(self) -> list[dict]:
        try:
            raw = json.loads(self.path.read_text())
            return [e for e in raw if e.get("status") == "pending"]
        except Exception:
            return []

    def approve(self, entry_id: str, vault_root: Path) -> Path:
        items: list[dict] = json.loads(self.path.read_text())
        for entry in items:
            if entry.get("id") == entry_id:
                dest = write_back(vault_root, entry["target_note"],
                                  entry["title"], entry["body_latex"])
                entry["status"] = "applied"
                entry["applied_path"] = str(dest)
                self.path.write_text(json.dumps(items, indent=2))
                return dest
        raise KeyError("unknown queue entry: " + entry_id)


def render_agent_block(title: str, body_latex: str) -> str:
    return (AGENT_START + "\n## " + title + " (breadstudy agent note)\n\n" +
            body_latex.strip() + "\n" + AGENT_END + "\n")


def write_back(vault_root: Path, target_note: str, title: str,
               body_latex: str) -> Path:
    """Write under a marked agent section or _agent folder.

    - If the target note exists and already has markers, only the bytes
      between the markers are replaced; everything else is preserved.
    - If the target note exists without markers, the block is appended and
      no existing line is modified.
    - If the target note does not exist, it is created under _agent/ leaving
      the rest of the vault untouched."""
    block = render_agent_block(title, body_latex)
    direct = vault_root / target_note
    if direct.exists():
        original = direct.read_text()
        if AGENT_START in original and AGENT_END in original:
            before, _, rest = original.partition(AGENT_START)
            _, _, after = rest.partition(AGENT_END)
            direct.write_text(before + block + after)
            return direct
        if not original.endswith("\n"):
            original += "\n"
        direct.write_text(original + "\n" + block)
        return direct
    dest = vault_root / "_agent" / Path(target_note).name
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text("# " + title + "\n\n" + block)
    return dest
