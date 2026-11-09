-- breadstudy schema v3: verification gate columns (STUDY-2)
ALTER TABLE items ADD COLUMN verify_method TEXT NOT NULL DEFAULT 'unverified';
ALTER TABLE items ADD COLUMN verify_detail TEXT NOT NULL DEFAULT '{}';
ALTER TABLE items ADD COLUMN dedupe_hash TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS items_dedupe_hash ON items(dedupe_hash);
