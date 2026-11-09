//! Vault indexer for Year 10 Maths (STUDY-1).
//!
//! Reads the Methods Pathway Overview test map (Tests 1-7), the strand
//! notes and the Practice sets from the vault checkout, resolves each
//! skill's vault note to a real file, and stores the derived structure
//! in SQLite. The vault stays the source of truth: re-running the
//! indexer must produce no duplicates and no content changes.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One row of the skill registry (seeded from spec/skills.md).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDef {
    pub id: String,
    pub strand: String,
    pub name: String,
    pub vault_note: String,
}

/// The six Year 10 Methods skills tracked for Test 6 and its neighbours.
pub fn skill_registry() -> Vec<SkillDef> {
    vec![
        SkillDef {
            id: "volume-prism-cylinder".into(),
            strand: "Geometry".into(),
            name: "Volume of prisms and cylinders".into(),
            vault_note: "Area Volume and Surface Area".into(),
        },
        SkillDef {
            id: "volume-cone-pyramid-sphere".into(),
            strand: "Geometry".into(),
            name: "Volume of cones, pyramids and spheres".into(),
            vault_note: "Area Volume and Surface Area".into(),
        },
        SkillDef {
            id: "surface-area".into(),
            strand: "Geometry".into(),
            name: "Surface area of solids".into(),
            vault_note: "Area Volume and Surface Area".into(),
        },
        SkillDef {
            id: "unit-conversion-scaling".into(),
            strand: "Geometry".into(),
            name: "Unit conversion and scale factors".into(),
            vault_note: "Area Volume and Surface Area".into(),
        },
        SkillDef {
            id: "measurement-error".into(),
            strand: "Number".into(),
            name: "Measurement error and accuracy".into(),
            vault_note: "Error and Accuracy".into(),
        },
        SkillDef {
            id: "pythagoras-3d".into(),
            strand: "Geometry".into(),
            name: "Pythagoras in 2D and 3D".into(),
            vault_note: "Pythagoras Theorem".into(),
        },
    ]
}

/// One entry of the assessment test map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestEntry {
    pub id: String,
    pub ord: i64,
    pub topic: String,
    pub notes: Vec<String>,
}

/// One numbered question from a Practice set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PracticeQuestion {
    pub number: u32,
    pub section: String,
    pub text: String,
}

/// One worked answer from a Practice set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PracticeAnswer {
    pub number: u32,
    pub text: String,
}

/// Parsed Practice set: questions plus worked answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PracticeSet {
    pub title: String,
    pub questions: Vec<PracticeQuestion>,
    pub answers: Vec<PracticeAnswer>,
}

/// Report from one indexer run over a vault checkout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexReport {
    pub tests: usize,
    pub skills: usize,
    pub linked: usize,
    pub resolved: Vec<String>,
    pub missing: Vec<String>,
    pub test6_skills: Vec<String>,
    pub content_hash: String,
}

/// Strip markdown bold markers and trim whitespace.
fn clean_cell(s: &str) -> String {
    s.replace("**", "").trim().to_string()
}

/// Extract `[[Wiki Links]]` from a table cell, in order.
fn extract_links(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        if let Some(end) = after.find("]]") {
            out.push(after[..end].to_string());
            rest = &after[end + 2..];
        } else {
            break;
        }
    }
    out
}

/// Parse the `## Test Map` table from the Methods Pathway Overview text.
pub fn parse_test_map(text: &str) -> Vec<TestEntry> {
    let mut entries = Vec::new();
    let mut in_table = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("## Test Map") {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        if line.starts_with("## ") {
            break;
        }
        if !line.starts_with('|') {
            continue;
        }
        if line.contains("---") {
            continue;
        }
        let cols: Vec<&str> = line.split('|').collect();
        if cols.len() < 4 {
            continue;
        }
        let first = clean_cell(cols[1]);
        if first.eq_ignore_ascii_case("Test") || first.eq_ignore_ascii_case("Topic") {
            continue;
        }
        let topic = clean_cell(cols[2]);
        let notes = extract_links(cols[3]);
        if let Some(rest) = first.strip_prefix("Test ") {
            if let Ok(n) = rest.trim().parse::<i64>() {
                entries.push(TestEntry {
                    id: format!("test-{n}"),
                    ord: n,
                    topic,
                    notes,
                });
                continue;
            }
        }
        if first.to_lowercase().contains("exam") {
            entries.push(TestEntry {
                id: "test-exam-s1".into(),
                ord: 0,
                topic,
                notes,
            });
        }
    }
    entries.sort_by_key(|e| e.ord);
    entries
}

/// Parse one Practice markdown file into questions and worked answers.
pub fn parse_practice_set(title: &str, text: &str) -> PracticeSet {
    let mut questions = Vec::new();
    let mut answers = Vec::new();
    #[derive(PartialEq)]
    enum Mode {
        None,
        Questions,
        Answers,
    }
    let mut mode = Mode::None;
    let mut section = String::new();
    let mut current_q: Option<PracticeQuestion> = None;
    let mut current_a: Option<PracticeAnswer> = None;

    let flush_q = |q: &mut Option<PracticeQuestion>, out: &mut Vec<PracticeQuestion>| {
        if let Some(q) = q.take() {
            out.push(q);
        }
    };
    let flush_a = |a: &mut Option<PracticeAnswer>, out: &mut Vec<PracticeAnswer>| {
        if let Some(a) = a.take() {
            out.push(a);
        }
    };

    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("## Questions") {
            flush_q(&mut current_q, &mut questions);
            flush_a(&mut current_a, &mut answers);
            mode = Mode::Questions;
            continue;
        }
        if line.starts_with("## Answers") {
            flush_q(&mut current_q, &mut questions);
            flush_a(&mut current_a, &mut answers);
            mode = Mode::Answers;
            continue;
        }
        if line.starts_with("## ") {
            flush_q(&mut current_q, &mut questions);
            flush_a(&mut current_a, &mut answers);
            mode = Mode::None;
            continue;
        }
        match mode {
            Mode::None => {}
            Mode::Questions => {
                if let Some(rest) = line.strip_prefix("### ") {
                    section = rest.trim().to_string();
                    continue;
                }
                if let Some((num, qtext)) = split_numbered(line) {
                    flush_q(&mut current_q, &mut questions);
                    current_q = Some(PracticeQuestion {
                        number: num,
                        section: section.clone(),
                        text: qtext,
                    });
                } else if !line.is_empty() {
                    if let Some(q) = current_q.as_mut() {
                        q.text.push(' ');
                        q.text.push_str(line);
                    }
                }
            }
            Mode::Answers => {
                if let Some((num, atext)) = split_answer(line) {
                    flush_a(&mut current_a, &mut answers);
                    current_a = Some(PracticeAnswer {
                        number: num,
                        text: atext,
                    });
                } else if !line.is_empty() {
                    if let Some(a) = current_a.as_mut() {
                        a.text.push(' ');
                        a.text.push_str(line);
                    }
                }
            }
        }
    }
    flush_q(&mut current_q, &mut questions);
    flush_a(&mut current_a, &mut answers);
    PracticeSet {
        title: title.to_string(),
        questions,
        answers,
    }
}

/// Split a `12. question text` line into its number and text.
fn split_numbered(line: &str) -> Option<(u32, String)> {
    let (num, rest) = line.split_once(". ")?;
    if num.trim().chars().all(|c| c.is_ascii_digit()) && !num.trim().is_empty() {
        num.trim()
            .parse::<u32>()
            .ok()
            .map(|n| (n, rest.trim().to_string()))
    } else {
        None
    }
}

/// Split a `**12.** answer text` line into its number and text.
fn split_answer(line: &str) -> Option<(u32, String)> {
    let line = line.strip_prefix("**")?;
    let (num, rest) = line.split_once(".**")?;
    if num.trim().chars().all(|c| c.is_ascii_digit()) && !num.trim().is_empty() {
        num.trim().parse::<u32>().ok().map(|n| {
            let text = rest.trim_start_matches([' ', ':']).trim().to_string();
            (n, text)
        })
    } else {
        None
    }
}

/// Recursively find `<title>.md` under the vault root.
pub fn find_note(vault_root: &Path, title: &str) -> Option<PathBuf> {
    let wanted = format!("{title}.md");
    let mut stack = vec![vault_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().map(|n| n == wanted.as_str()).unwrap_or(false) {
                return Some(path);
            }
        }
    }
    None
}

fn content_hash(rows: &[String]) -> String {
    let mut hasher = DefaultHasher::new();
    let mut sorted = rows.to_vec();
    sorted.sort();
    sorted.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Index the vault checkout into the database. Idempotent: re-running
/// stores the same rows, so a second run produces no dupes or changes.
pub async fn index_vault(
    pool: &sqlx::SqlitePool,
    vault_root: &Path,
) -> anyhow::Result<IndexReport> {
    let overview_path = vault_root.join("Methods Pathway Overview.md");
    let overview = std::fs::read_to_string(&overview_path).map_err(|e| {
        anyhow::anyhow!("cannot read {}: {e}", overview_path.display())
    })?;
    let tests = parse_test_map(&overview);
    if tests.is_empty() {
        anyhow::bail!("no test map rows parsed from {}", overview_path.display());
    }

    for t in &tests {
        let notes = serde_json::to_string(&t.notes)?;
        sqlx::query(
            "INSERT INTO tests(id, ord, topic, main_notes) VALUES(?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET ord=excluded.ord, topic=excluded.topic, main_notes=excluded.main_notes",
        )
        .bind(&t.id)
        .bind(t.ord)
        .bind(&t.topic)
        .bind(&notes)
        .execute(pool)
        .await?;
    }

    let skills = skill_registry();
    for s in &skills {
        sqlx::query(
            "INSERT INTO skills(id, strand, name, vault_note) VALUES(?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET strand=excluded.strand, name=excluded.name, vault_note=excluded.vault_note",
        )
        .bind(&s.id)
        .bind(&s.strand)
        .bind(&s.name)
        .bind(&s.vault_note)
        .execute(pool)
        .await?;
    }

    // Link each test to the skills whose vault note it lists as a main note.
    let mut linked = 0usize;
    for t in &tests {
        sqlx::query("DELETE FROM skill_tests WHERE test_id = ?")
            .bind(&t.id)
            .execute(pool)
            .await?;
        for s in &skills {
            if t.notes.iter().any(|n| n == &s.vault_note) {
                sqlx::query(
                    "INSERT INTO skill_tests(skill_id, test_id) VALUES(?,?)
                     ON CONFLICT DO NOTHING",
                )
                .bind(&s.id)
                .bind(&t.id)
                .execute(pool)
                .await?;
                linked += 1;
            }
        }
    }

    let mut resolved = Vec::new();
    let mut missing = Vec::new();
    for s in &skills {
        if find_note(vault_root, &s.vault_note).is_some() {
            resolved.push(s.id.clone());
        } else {
            missing.push(s.id.clone());
        }
    }

    let test6: Vec<String> =
        sqlx::query_scalar("SELECT skill_id FROM skill_tests WHERE test_id = 'test-6'")
            .fetch_all(pool)
            .await?;

    // Content hash over the derived rows, so a second run can prove
    // it produced no changes.
    let mut rows = Vec::new();
    let db_tests: Vec<(String, i64, String, String)> =
        sqlx::query_as("SELECT id, ord, topic, main_notes FROM tests")
            .fetch_all(pool)
            .await?;
    for (id, ord, topic, notes) in db_tests {
        rows.push(format!("test:{id}:{ord}:{topic}:{notes}"));
    }
    let db_skills: Vec<(String, String, String, String)> =
        sqlx::query_as("SELECT id, strand, name, vault_note FROM skills")
            .fetch_all(pool)
            .await?;
    for (id, strand, name, note) in db_skills {
        rows.push(format!("skill:{id}:{strand}:{name}:{note}"));
    }
    let db_links: Vec<(String, String)> =
        sqlx::query_as("SELECT skill_id, test_id FROM skill_tests")
            .fetch_all(pool)
            .await?;
    for (s, t) in db_links {
        rows.push(format!("link:{s}:{t}"));
    }

    Ok(IndexReport {
        tests: tests.len(),
        skills: skills.len(),
        linked,
        resolved,
        missing,
        test6_skills: test6,
        content_hash: content_hash(&rows),
    })
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    const OVERVIEW_FIXTURE: &str = "## Test Map\n\n| Test | Topic | Main note(s) |\n|------|-------|--------------|\n| **Test 1** | Algebraic Techniques | [[Algebraic Techniques]], [[Real Numbers and Surds]] |\n| **Semester 1 Exam** | Everything in Tests 1 to 3 | all of the above |\n| **Test 6** | Volume, Capacity and Surface Area | [[Area Volume and Surface Area]] |\n";

    #[test]
    fn parses_test_rows_and_exam_row() {
        let entries = parse_test_map(OVERVIEW_FIXTURE);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].id, "test-exam-s1");
        assert_eq!(entries[1].id, "test-1");
        assert_eq!(
            entries[1].notes,
            vec!["Algebraic Techniques", "Real Numbers and Surds"]
        );
        let t6 = entries.iter().find(|e| e.id == "test-6").unwrap();
        assert!(t6.topic.contains("Volume"));
        assert_eq!(t6.notes, vec!["Area Volume and Surface Area"]);
    }

    #[test]
    fn parses_practice_questions_and_answers() {
        let text = "## Questions\n\n### Section A: Units\n\n1. Convert 4.5 m2 to cm2.\n2. Find the area.\ncontinued line.\n\n## Answers\n\n**1.** 45 000 cm2.\n**2.** 50 cm2.\n\n## See Also\n";
        let set = parse_practice_set("Sample Practice", text);
        assert_eq!(set.questions.len(), 2);
        assert_eq!(set.questions[0].number, 1);
        assert_eq!(set.questions[0].section, "Section A: Units");
        assert_eq!(set.questions[1].text, "Find the area. continued line.");
        assert_eq!(set.answers.len(), 2);
        assert_eq!(set.answers[1].text, "50 cm2.");
    }

    #[test]
    fn registry_covers_test6_seed_rows() {
        let reg = skill_registry();
        assert_eq!(reg.len(), 6);
        let volsa = reg
            .iter()
            .filter(|s| s.vault_note == "Area Volume and Surface Area")
            .count();
        assert_eq!(volsa, 4);
    }
}
