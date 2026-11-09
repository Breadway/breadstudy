"""Minute runner: fire whatever reminders are due right now."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from reminders import ReminderStore, Scheduler, client_from_env, utcnow

STATE = Path(__file__).resolve().parents[2] / "state"


def main() -> int:
    store = ReminderStore.load(STATE / "reminders.json")
    sched = Scheduler(store=store, client=client_from_env(),
                      log_path=STATE / "events.jsonl")
    fired = sched.due_events(utcnow())
    print("fired: " + ",".join(fired) if fired else "nothing due")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
