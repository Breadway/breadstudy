# Reminders and skip handling (STUDY-8)

Gotify on hestia port 7601. No Bread Tracker FCM dependency.

## Setup

```sh
export GOTIFY_URL="http://127.0.0.1:7601"
export GOTIFY_TOKEN="<app token from the Gotify UI>"
python3 services/reminders/run_due.py
```

`run_due.py` is a thin runner: it loads `state/reminders.json`, fires
whatever is due (T-10, +5, +15), and appends to `state/events.jsonl`.
Run it every minute from a user timer. The escalation ceiling is built in:
after the +15 re-ping the scheduler writes an incomplete-session note and
stops. No further escalation.

## Skip flow

Skips require a one-line reason. A skip increments the ISO weekly skip
count, consumes one grace token (floor zero), and writes
`state/replan-<session>.json` so the planner re-plans the missed skill.

## Checks

```sh
python3 services/reminders/tests/check_reminders.py
```
