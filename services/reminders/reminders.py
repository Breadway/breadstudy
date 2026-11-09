# Gotify reminders and skip handling for breadstudy (STUDY-8).
#
# Gotify runs on hestia port 7601. This module never touches Bread Tracker
# FCM. Escalation ceiling is +5/+15 min re-ping and an incomplete-session
# note. Nothing further.
#
# Flow:
# - schedule(topic, duration_min, starts_at): persist a scheduled session.
# - due_events(now): T-10 ping, then +5/+15 re-pings while unacknowledged.
# - acknowledge(session_id): student tapped start; stops re-pings.
# - skip(session_id, reason): one-line reason required. Increments weekly
#   skip count, consumes one grace token, writes a replan request for the
#   missed skill, and logs an incomplete-session note when the ceiling hits.
#
# State lives in small JSON files under the state dir so this sidecar does
# not need server/ migrations. The planner consumes replan requests.

from __future__ import annotations

import json
import os
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path

GOTIFY_URL_DEFAULT = "http://127.0.0.1:7601"
REPING_OFFSETS = (timedelta(minutes=5), timedelta(minutes=15))
REMIND_BEFORE = timedelta(minutes=10)
GRACE_TOKENS_DEFAULT = 3


def utcnow() -> datetime:
    return datetime.now(timezone.utc)


def parse_ts(raw: str) -> datetime:
    dt = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt


@dataclass
class ScheduledSession:
    id: str
    topic: str
    duration_min: int
    skill_id: str
    starts_at: str  # ISO timestamp
    acked: bool = False
    pings_sent: int = 0
    skipped: bool = False


@dataclass
class GotifyClient:
    """Minimal Gotify sender. Token comes from env, never hardcoded."""

    base_url: str = GOTIFY_URL_DEFAULT
    token: str = ""
    sender: object = None  # injectable (title, message, extras) -> ok

    def send(self, title: str, message: str, extras: dict | None = None) -> bool:
        if self.sender is not None:
            return bool(self.sender(title, message, extras or {}))
        if not self.token:
            return False
        import json as _json
        payload = _json.dumps({"title": title, "message": message,
                               "priority": 5, "extras": extras or {}}).encode()
        url = self.base_url.rstrip("/") + "/message?token=" + self.token
        req = urllib.request.Request(url, data=payload,
                                     headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=10) as res:
                return 200 <= res.status < 300
        except Exception:
            return False


@dataclass
class ReminderStore:
    """JSON sidecar store. No server/ migration needed."""

    path: Path
    sessions: dict[str, ScheduledSession] = field(default_factory=dict)
    skips_this_week: int = 0
    week_key: str = ""
    grace_tokens: int = GRACE_TOKENS_DEFAULT

    @classmethod
    def load(cls, path: Path) -> "ReminderStore":
        store = cls(path=path)
        if path.exists():
            try:
                raw = json.loads(path.read_text())
            except Exception:
                raw = {}
            store.week_key = raw.get("week_key", "")
            store.skips_this_week = int(raw.get("skips_this_week", 0))
            store.grace_tokens = int(raw.get("grace_tokens", GRACE_TOKENS_DEFAULT))
            for entry in raw.get("sessions", []):
                try:
                    store.sessions[entry["id"]] = ScheduledSession(**entry)
                except Exception:
                    continue
        current_week = utcnow().strftime("%G-W%V")
        if store.week_key != current_week:
            store.week_key = current_week
            store.skips_this_week = 0
        return store

    def save(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        raw = {"week_key": self.week_key, "skips_this_week": self.skips_this_week,
               "grace_tokens": self.grace_tokens,
               "sessions": [s.__dict__ for s in self.sessions.values()]}
        self.path.write_text(json.dumps(raw, indent=2))


def reminder_message(session: ScheduledSession) -> tuple[str, str, dict]:
    title = "breadstudy in 10 min: " + session.topic
    message = (session.topic + " for " + str(session.duration_min) +
               " min. Reply start or not-now.")
    extras = {"client::actions": {"actions": [
        {"action": "view", "title": "Start"},
        {"action": "view", "title": "Not now"},
    ]}}
    return title, message, extras


def reping_message(session: ScheduledSession, offset_min: int) -> tuple[str, str, dict]:
    title = "breadstudy nudge (+" + str(offset_min) + " min): " + session.topic
    message = ("Session started " + str(offset_min) +
               " min ago with no start. Reply start or skip with a reason.")
    return title, message, {}


def incomplete_note(session: ScheduledSession) -> str:
    return ("Incomplete session: " + session.topic + " at " + session.starts_at +
            " had no start after +5/+15 min re-pings. Skipped without reason.")


@dataclass
class Scheduler:
    store: ReminderStore
    client: GotifyClient
    log_path: Path | None = None

    def _log(self, event: dict) -> None:
        if self.log_path is None:
            return
        self.log_path.parent.mkdir(parents=True, exist_ok=True)
        with self.log_path.open("a") as f:
            f.write(json.dumps(event) + "\n")

    def schedule(self, session: ScheduledSession) -> None:
        self.store.sessions[session.id] = session
        self.store.save()

    def acknowledge(self, session_id: str) -> bool:
        session = self.store.sessions.get(session_id)
        if session is None:
            return False
        session.acked = True
        self.store.save()
        self._log({"at": utcnow().isoformat(), "kind": "ack", "id": session_id})
        return True

    def due_events(self, now: datetime) -> list[str]:
        """Fire T-10 pings and +5/+15 re-pings. Returns ids pinged."""
        fired: list[str] = []
        for session in self.store.sessions.values():
            if session.acked or session.skipped:
                continue
            starts = parse_ts(session.starts_at)
            if session.pings_sent == 0 and starts - REMIND_BEFORE <= now < starts:
                title, message, extras = reminder_message(session)
                if self.client.send(title, message, extras):
                    session.pings_sent = 1
                    fired.append(session.id)
                    self._log({"at": now.isoformat(), "kind": "remind", "id": session.id})
            elif session.pings_sent in (1, 2) and now >= starts:
                idx = session.pings_sent - 1
                if idx < len(REPING_OFFSETS) and now >= starts + REPING_OFFSETS[idx]:
                    offset = int(REPING_OFFSETS[idx].total_seconds() // 60)
                    title, message, extras = reping_message(session, offset)
                    if self.client.send(title, message, extras):
                        session.pings_sent += 1
                        fired.append(session.id)
                        self._log({"at": now.isoformat(), "kind": "reping",
                                   "id": session.id, "offset_min": offset})
                    if session.pings_sent > 2:
                        self._log({"at": now.isoformat(), "kind": "incomplete-note",
                                   "id": session.id, "note": incomplete_note(session)})
        if fired:
            self.store.save()
        return fired

    def skip(self, session_id: str, reason: str) -> dict:
        """Skip with a required one-line reason. Re-plans the missed skill."""
        session = self.store.sessions.get(session_id)
        if session is None:
            raise KeyError("unknown session: " + session_id)
        clean = (reason or "").strip().splitlines()[0].strip() if (reason or "").strip() else ""
        if not clean:
            raise ValueError("skip reason is required (one line)")
        session.skipped = True
        self.store.skips_this_week += 1
        if self.store.grace_tokens > 0:
            self.store.grace_tokens -= 1
        self.store.save()
        replan = {"at": utcnow().isoformat(), "kind": "replan",
                  "skill_id": session.skill_id, "from_session": session.id,
                  "reason": clean}
        state_dir = self.store.path.parent
        replan_path = state_dir / ("replan-" + session.id + ".json")
        replan_path.write_text(json.dumps(replan, indent=2))
        self._log({"at": utcnow().isoformat(), "kind": "skip", "id": session_id,
                   "reason": clean, "weekly_skips": self.store.skips_this_week,
                   "grace_left": self.store.grace_tokens})
        return {"replan_path": str(replan_path),
                "weekly_skips": self.store.skips_this_week,
                "grace_left": self.store.grace_tokens, "reason": clean}


def client_from_env(sender=None) -> GotifyClient:
    return GotifyClient(base_url=os.environ.get("GOTIFY_URL", GOTIFY_URL_DEFAULT),
                        token=os.environ.get("GOTIFY_TOKEN", ""), sender=sender)
