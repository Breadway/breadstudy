"""STUDY-8 checks: reminder fire, +5/+15 ceiling, skip flow."""
import sys
from datetime import timedelta
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from reminders import (ReminderStore, Scheduler, GotifyClient, ScheduledSession,
                       utcnow)


def make(tmp: Path):
    sent = []
    client = GotifyClient(sender=lambda t, m, e: sent.append((t, m, e)) or True)
    store = ReminderStore.load(tmp / "reminders.json")
    sched = Scheduler(store=store, client=client, log_path=tmp / "events.jsonl")
    return sched, sent


def main() -> int:
    import tempfile
    failures = []
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        # 1. Reminder fires for a scheduled session with topic, duration, actions.
        sched, sent = make(tmp)
        now = utcnow()
        session = ScheduledSession(id="s1", topic="Volume/SA practice",
                                   duration_min=20, skill_id="volume-prism-cylinder",
                                   starts_at=(now + timedelta(minutes=9)).isoformat())
        sched.schedule(session)
        fired = sched.due_events(now)
        if fired != ["s1"]:
            failures.append("T-10 did not fire: " + str(fired))
        if not sent or "Volume/SA practice" not in sent[0][0] or "20 min" not in sent[0][1]:
            failures.append("reminder missing topic or duration")
        actions = str(sent[0][2])
        if "Start" not in actions or "Not now" not in actions:
            failures.append("reminder missing start/not-now actions")

        # 2. Unacknowledged re-pings at +5 and +15, then stops.
        sent.clear()
        starts = now - timedelta(minutes=16)
        session.starts_at = starts.isoformat()
        session.pings_sent = 1
        session.acked = False
        sched.store.save()
        fired = sched.due_events(now - timedelta(minutes=10))
        if fired != ["s1"]:
            failures.append("+5 re-ping did not fire: " + str(fired))
        fired = sched.due_events(now)
        if fired != ["s1"]:
            failures.append("+15 re-ping did not fire: " + str(fired))
        log_text = (tmp / "events.jsonl").read_text()
        if "incomplete-note" not in log_text:
            failures.append("missing incomplete-session note after ceiling")
        sent.clear()
        fired = sched.due_events(now + timedelta(minutes=30))
        if fired:
            failures.append("re-pings did not stop after ceiling: " + str(fired))

        # 3. Ack stops re-pings.
        sched2, sent2 = make(tmp)
        s2 = ScheduledSession(id="s2", topic="Recall", duration_min=20,
                              skill_id="surface-area",
                              starts_at=(utcnow() - timedelta(minutes=6)).isoformat())
        s2.pings_sent = 1
        sched2.schedule(s2)
        assert sched2.acknowledge("s2")
        if sched2.due_events(utcnow()):
            failures.append("acked session still re-pinged")

        # 4. Skip captures reason, counts week, consumes token, replans.
        sched3, _ = make(tmp)
        s3 = ScheduledSession(id="s3", topic="Practice", duration_min=20,
                              skill_id="surface-area",
                              starts_at=utcnow().isoformat())
        sched3.schedule(s3)
        before_tokens = sched3.store.grace_tokens
        try:
            sched3.skip("s3", "  ")
            failures.append("empty skip reason was accepted")
        except ValueError:
            pass
        result = sched3.skip("s3", "family dinner ran late")
        if result["weekly_skips"] < 1:
            failures.append("weekly skip count did not increment")
        if result["grace_left"] != max(0, before_tokens - 1):
            failures.append("grace token was not consumed")
        replan = Path(result["replan_path"])
        if not replan.exists() or "surface-area" not in replan.read_text():
            failures.append("skip did not re-plan the missed skill")
        if "family dinner ran late" not in replan.read_text():
            failures.append("skip reason missing from replan")

    if failures:
        for f in failures:
            print("FAIL: " + f)
        return 1
    print("PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
