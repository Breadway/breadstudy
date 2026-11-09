"""02:00 runner. Reads due skills, runs the pipeline, appends to the bank."""
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from pipeline import TTSClient, run_nightly

BASE = Path(__file__).resolve().parents[2]
STATE = BASE / "state"
BANK = STATE / "item-bank.json"
DUE = STATE / "due-skills.json"
AUDIO = STATE / "audio"


def load_json(path: Path, default):
    try:
        return json.loads(path.read_text())
    except Exception:
        return default


def main() -> int:
    date_key = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    due = load_json(DUE, ["volume-prism-cylinder", "volume-cone-pyramid-sphere",
                          "surface-area"])
    existing = load_json(BANK, [])
    tts = TTSClient(out_dir=AUDIO)
    result = run_nightly(due, existing, date_key, AUDIO, tts=tts)
    bank = existing + result.items
    STATE.mkdir(parents=True, exist_ok=True)
    BANK.write_text(json.dumps(bank, indent=2))
    manifest = {"date": date_key, "due_skills": due,
                "generated": len(result.items),
                "skipped_dupes": result.skipped_dupes,
                "failed_verify": result.failed_verify}
    (STATE / ("nightly-" + date_key + ".json")).write_text(json.dumps(manifest, indent=2))
    print("generated %d items, %d dupes, %d failed" % (
        len(result.items), result.skipped_dupes, result.failed_verify))
    return 0 if result.failed_verify == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
