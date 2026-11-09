-- breadstudy schema v6: per-attempt rubric marks and grading provenance (STUDY-17)
ALTER TABLE attempts ADD COLUMN method_marks REAL NOT NULL DEFAULT 0;
ALTER TABLE attempts ADD COLUMN method_marks_total REAL NOT NULL DEFAULT 0;
ALTER TABLE attempts ADD COLUMN final_marks REAL NOT NULL DEFAULT 0;
ALTER TABLE attempts ADD COLUMN final_marks_total REAL NOT NULL DEFAULT 1;
ALTER TABLE attempts ADD COLUMN rubric_breakdown TEXT NOT NULL DEFAULT '[]';
ALTER TABLE attempts ADD COLUMN grader_model TEXT;
ALTER TABLE attempts ADD COLUMN degraded INTEGER NOT NULL DEFAULT 0;
ALTER TABLE attempts ADD COLUMN transcription_confidence REAL;
ALTER TABLE attempts ADD COLUMN feedback TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS attempts_item_created ON attempts(item_id, created_at);
