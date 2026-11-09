-- breadstudy schema v4: FSRS scheduling, hint ladder state, error patterns
-- and the session/intervention logs the weekly review reads (STUDY-18).
--
-- Lifecycle numbers stay in the existing skills/items/attempts tables. This
-- migration only adds the per-skill scheduler, per-item hint cursor, error
-- aggregates and append-only logs.

CREATE TABLE IF NOT EXISTS skill_schedule (
    skill_id TEXT PRIMARY KEY REFERENCES skills(id),
    stability REAL NOT NULL DEFAULT 0.0,
    difficulty REAL NOT NULL DEFAULT 5.0,
    mastery REAL NOT NULL DEFAULT 0.4,
    accuracy REAL NOT NULL DEFAULT 0.4,
    reviews INTEGER NOT NULL DEFAULT 0,
    lapses INTEGER NOT NULL DEFAULT 0,
    last_review TEXT,
    due_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS skill_schedule_due ON skill_schedule(due_at);

-- Where the student is on one item's hint ladder. The ladder text itself
-- lives in items.hints as a JSON array; level 0 means no hint revealed.
CREATE TABLE IF NOT EXISTS hint_state (
    item_id TEXT PRIMARY KEY REFERENCES items(id),
    level INTEGER NOT NULL DEFAULT 0,
    total INTEGER NOT NULL DEFAULT 0,
    uses INTEGER NOT NULL DEFAULT 0,
    last_used_at TEXT,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS error_patterns (
    skill_id TEXT NOT NULL,
    tag TEXT NOT NULL,
    count INTEGER NOT NULL DEFAULT 1,
    first_seen TEXT NOT NULL,
    last_seen TEXT NOT NULL,
    PRIMARY KEY (skill_id, tag)
);
CREATE INDEX IF NOT EXISTS error_patterns_last_seen ON error_patterns(last_seen);

-- Every attempt, hint reveal and session lifecycle event.
CREATE TABLE IF NOT EXISTS session_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT,
    ts TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    kind TEXT NOT NULL,
    item_id TEXT,
    skill_id TEXT,
    detail TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS session_log_ts ON session_log(ts);

-- Anything the weekly review may want to treat as an intervention: hint
-- assists, repeated error tags, planner nudges.
CREATE TABLE IF NOT EXISTS intervention_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    kind TEXT NOT NULL,
    item_id TEXT,
    skill_id TEXT,
    hint_level INTEGER,
    detail TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS intervention_log_ts ON intervention_log(ts);

-- Attempts keep their own hint count and FSRS rating so the weekly review
-- can separate unaided success from hint-assisted success.
ALTER TABLE attempts ADD COLUMN hints_used INTEGER NOT NULL DEFAULT 0;
ALTER TABLE attempts ADD COLUMN rating INTEGER;
