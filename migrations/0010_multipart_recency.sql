-- breadstudy schema v10: multi-part exam items, served-item recency and
-- explain continuity (STUDY-44, STUDY-108).
--
-- Multi-part questions keep the single-part item row as the session-facing
-- unit: `items.answer` and `items.rubric` still describe the whole question,
-- while `item_parts` carries the (a), (b), (c) prompts, answers, marks and
-- rubrics. `items.mark_scheme` is the whole-question scheme the client may
-- show after grading. Nothing here replaces the single-part path.

ALTER TABLE items ADD COLUMN mark_scheme TEXT;

CREATE TABLE IF NOT EXISTS item_parts (
    item_id TEXT NOT NULL REFERENCES items(id),
    label TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    prompt TEXT NOT NULL,
    answer TEXT NOT NULL,
    marks REAL NOT NULL DEFAULT 0,
    rubric TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (item_id, label)
);
CREATE INDEX IF NOT EXISTS item_parts_item_ordinal ON item_parts(item_id, ordinal);

-- One row per item cached into a run. Selection reads the most recent row
-- to prefer items that have not been served in the trailing seven days,
-- then falls back to the least recently served item.
CREATE TABLE IF NOT EXISTS served_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    item_id TEXT NOT NULL REFERENCES items(id),
    session_id TEXT NOT NULL,
    phase TEXT NOT NULL,
    served_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS served_items_item_time ON served_items(item_id, served_at);
CREATE INDEX IF NOT EXISTS served_items_session ON served_items(session_id);

-- The practise item kept for the explain phase, so explain continues the
-- question the student just answered instead of switching to a new one.
ALTER TABLE session_runs ADD COLUMN practice_item_id TEXT;
