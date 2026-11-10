-- breadstudy schema v9: hint progress is scoped to a session run (STUDY-26).
--
-- hint_state was keyed by item alone, so one session's revealed rungs leaked
-- into every later session for the same item. The empty session key keeps
-- the legacy item-level cursor used by GET /api/items/{id}/hint, while a
-- real session id gets its own ladder progress that starts at zero.
CREATE TABLE hint_state_v9 (
    session_id TEXT NOT NULL DEFAULT '',
    item_id TEXT NOT NULL REFERENCES items(id),
    level INTEGER NOT NULL DEFAULT 0,
    total INTEGER NOT NULL DEFAULT 0,
    uses INTEGER NOT NULL DEFAULT 0,
    last_used_at TEXT,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (session_id, item_id)
);

INSERT INTO hint_state_v9(session_id, item_id, level, total, uses, last_used_at, updated_at)
SELECT '', item_id, level, total, uses, last_used_at, updated_at FROM hint_state;

DROP TABLE hint_state;
ALTER TABLE hint_state_v9 RENAME TO hint_state;
