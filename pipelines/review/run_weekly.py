"""Weekly runner: review from JSON exports plus countdown."""
import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from review import AttemptRecord, SkillMastery, countdown, generate_review

BASE = Path(__file__).resolve().parents[2]
STATE = BASE / "state"


def load_json(path, default):
    try:
        return json.loads(Path(path).read_text())
    except Exception:
        return default


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mastery", default=str(STATE / "mastery.json"))
    parser.add_argument("--attempts", default=str(STATE / "attempts.json"))
    parser.add_argument("--events", default=str(STATE / "events.jsonl"))
    parser.add_argument("--week", default="2026-W46")
    parser.add_argument("--out", default=str(STATE / "review.md"))
    args = parser.parse_args()
    mastery = [SkillMastery(**m) for m in load_json(args.mastery, [])]
    attempts = [AttemptRecord(skill_id=a.get("skill_id", ""),
                              correct=bool(a.get("correct")),
                              error_tags=a.get("error_tags", []))
                for a in load_json(args.attempts, [])]
    events = []
    try:
        for line in Path(args.events).read_text().splitlines():
            events.append(json.loads(line))
    except Exception:
        pass
    text = generate_review(mastery, attempts, events, args.week)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(text)
    print("wrote " + str(out))
    for row in countdown():
        print("Test %d %s: %s" % (row["test"], row["topic"], row["date"]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
