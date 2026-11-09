-- breadstudy schema v2: test map and skill links (STUDY-1 indexer)
CREATE TABLE IF NOT EXISTS tests (
    id TEXT PRIMARY KEY,
    ord INTEGER NOT NULL,
    topic TEXT NOT NULL,
    main_notes TEXT NOT NULL DEFAULT '[]',
    assessment_date TEXT,
    UNIQUE(ord)
);
CREATE TABLE IF NOT EXISTS skill_tests (
    skill_id TEXT NOT NULL REFERENCES skills(id),
    test_id TEXT NOT NULL REFERENCES tests(id),
    PRIMARY KEY (skill_id, test_id)
);
