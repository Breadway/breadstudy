-- breadstudy schema v7: planner log and skill misses (STUDY-5)
CREATE TABLE IF NOT EXISTS plans (
    id TEXT PRIMARY KEY,
    date TEXT NOT NULL,
    skill_id TEXT NOT NULL REFERENCES skills(id),
    score REAL NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS plans_date ON plans(date);
CREATE TABLE IF NOT EXISTS skips (
    id TEXT PRIMARY KEY,
    skill_id TEXT NOT NULL REFERENCES skills(id),
    date TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS skips_skill_date ON skips(skill_id, date);
