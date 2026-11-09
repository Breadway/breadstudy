-- breadstudy schema v1: skills, items, attempts, sessions
CREATE TABLE IF NOT EXISTS skills (
    id TEXT PRIMARY KEY,
    strand TEXT NOT NULL,
    name TEXT NOT NULL,
    vault_note TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE TABLE IF NOT EXISTS items (
    id TEXT PRIMARY KEY,
    skill_id TEXT NOT NULL REFERENCES skills(id),
    prompt TEXT NOT NULL,
    answer TEXT NOT NULL,
    rubric TEXT NOT NULL DEFAULT '[]',
    hints TEXT NOT NULL DEFAULT '[]',
    source_url TEXT,
    source_snippet TEXT,
    fetched_at TEXT,
    status TEXT NOT NULL DEFAULT 'verified',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE TABLE IF NOT EXISTS attempts (
    id TEXT PRIMARY KEY,
    item_id TEXT NOT NULL REFERENCES items(id),
    input_kind TEXT NOT NULL,
    given TEXT NOT NULL,
    correct INTEGER NOT NULL,
    error_tags TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    template TEXT NOT NULL,
    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ended_at TEXT,
    outcome TEXT
);
