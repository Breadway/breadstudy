"""STUDY-15 checks: review, write-back, countdown, queue."""
import sys
from datetime import date
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from review import (AttemptRecord, QueuedEntry, ReviewQueue, SkillMastery,
                    countdown, generate_review, write_back)


def main() -> int:
    failures = []
    import tempfile
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        vault = tmp / "vault"
        (vault / "Geometry").mkdir(parents=True)

        # Review generates from real mastery, error, and session data.
        mastery = [SkillMastery("volume-prism-cylinder", 0.42, 5, 2),
                   SkillMastery("surface-area", 0.81, 4, 3)]
        attempts = [AttemptRecord("volume-prism-cylinder", False, ["wrong-formula"]),
                    AttemptRecord("volume-prism-cylinder", False, ["wrong-formula"]),
                    AttemptRecord("surface-area", True, [])]
        events = [{"kind": "session-start"}, {"kind": "pause-nudge"},
                  {"kind": "skip"}]
        text = generate_review(mastery, attempts, events, "2026-W46")
        for token in ("volume-prism-cylinder", "0.42", "wrong-formula: 2",
                      "Sessions started: 1", "Skips: 1", "Puzzle warm-up"):
            if token not in text:
                failures.append("review missing " + token)

        # Countdown shows Tests 1-7 with Test 6 first.
        rows = countdown(date(2026, 11, 9))
        if [r["test"] for r in rows] != [6, 1, 2, 3, 4, 5, 7]:
            failures.append("countdown order wrong: " +
                            str([r["test"] for r in rows]))
        if len(rows) != 7 or rows[0]["date"] != "2026-11-10 (in 1 days)":
            failures.append("Test 6 first row wrong: " + str(rows[0]))

        # Write-back via the queue lands under a marked agent section.
        note = vault / "Area Volume and Surface Area.md"
        note.write_text("# Area notes\n\nKeep this line.\n")
        queue = ReviewQueue(tmp / "queue.json")
        queue.stage(QueuedEntry("q1", "Area Volume and Surface Area.md",
                               "Cylinder volume", "V = \\pi r^2 h"))
        dest = queue.approve("q1", vault)
        if dest != note:
            failures.append("queue applied to wrong path")
        body = note.read_text()
        if "Keep this line." not in body or "breadstudy-agent-start" not in body:
            failures.append("write-back mangled note or missing markers")
        if body.count("Keep this line.") != 1:
            failures.append("existing content duplicated")

        # Second approval replaces only the marked block, never the note.
        queue.stage(QueuedEntry("q2", "Area Volume and Surface Area.md",
                               "Cylinder volume v2", "V = 63\\pi"))
        queue.approve("q2", vault)
        body2 = note.read_text()
        if "V = 63\\pi" not in body2 or "Keep this line." not in body2:
            failures.append("marker replacement broke the note")
        if "Cylinder volume v2 (breadstudy agent note)" not in body2:
            failures.append("agent block title missing")

        # Missing target creates _agent file, touching nothing else.
        before = sorted(p.relative_to(vault) for p in vault.rglob("*") if p.is_file())
        dest2 = write_back(vault, "Nope Missing.md", "New", "x^2")
        if dest2.parent.name != "_agent":
            failures.append("missing note did not go to _agent: " + str(dest2))
        after = sorted(p.relative_to(vault) for p in vault.rglob("*") if p.is_file())
        touched = [p for p in after if p not in before and p != dest2.relative_to(vault)]
        if touched:
            failures.append("silent extra writes: " + str(touched))

    if failures:
        for f in failures:
            print("FAIL: " + f)
        return 1
    print("PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
