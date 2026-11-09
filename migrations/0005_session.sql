-- breadstudy schema v5: server-driven session state machine (STUDY-16)
--
-- The server owns phase transitions and timer expiry. The client is a
-- thin renderer, so all mutable session state lives here.
CREATE TABLE IF NOT EXISTS session_runs (
    id TEXT PRIMARY KEY,
    template TEXT NOT NULL,
    phase TEXT NOT NULL,
    phase_index INTEGER NOT NULL,
    phase_remaining_seconds INTEGER NOT NULL,
    paused INTEGER NOT NULL DEFAULT 0,
    paused_at TEXT,
    total_paused_seconds INTEGER NOT NULL DEFAULT 0,
    off_task_pauses INTEGER NOT NULL DEFAULT 0,
    skip_count INTEGER NOT NULL DEFAULT 0,
    grace_tokens INTEGER NOT NULL DEFAULT 0,
    item_cursor INTEGER NOT NULL DEFAULT -1,
    hints_used INTEGER NOT NULL DEFAULT 0,
    content_json TEXT NOT NULL DEFAULT '[]',
    started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    ended_at TEXT,
    outcome TEXT
);

-- Append-only event log. Other modules (review, dashboard, planner)
-- read the log through session::events rather than the session tables.
CREATE TABLE IF NOT EXISTS session_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES session_runs(id),
    at TEXT NOT NULL,
    kind TEXT NOT NULL,
    phase TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS session_events_session_id
    ON session_events(session_id, id);
CREATE INDEX IF NOT EXISTS session_events_kind_at
    ON session_events(kind, at);

-- Pre-rendered audio paths resolved from the item bank. A missing row
-- means the client falls back to subtitles; nothing is synthesised live
-- during a session.
CREATE TABLE IF NOT EXISTS item_audio (
    item_id TEXT PRIMARY KEY REFERENCES items(id),
    path TEXT NOT NULL,
    voice TEXT NOT NULL DEFAULT 'patient-coach',
    generated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
