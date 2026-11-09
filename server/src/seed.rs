//! Seed bank import from the vault Practice sets (STUDY-4).
//!
//! Each Practice question becomes an item with its worked answer,
//! linked to a skill, and passed through the [`crate::verify`] gate.
//! The gate's SymPy re-check result is recorded on every item:
//! mismatches land in `review_required`, never silently fixed, and
//! items SymPy cannot check stay flagged for one manual approval.
//! Re-running the seed is idempotent: identical prompts dedupe to the
//! existing rows.

use serde::{Deserialize, Serialize};

use crate::indexer::{find_note, parse_practice_set, skill_registry};
use crate::verify::{NewItem, Origin, StoredItem, find_by_prompt, store_item};

/// A skill the seed needs beyond the STUDY-1 registry.
#[derive(Debug, Clone)]
pub struct SeedSkill {
    pub id: &'static str,
    pub strand: &'static str,
    pub name: &'static str,
    pub vault_note: &'static str,
}

/// Skills for the non-Test-6 Practice content, all resolving to real
/// vault notes.
pub fn seed_skills() -> Vec<SeedSkill> {
    vec![
        SeedSkill { id: "angle-properties", strand: "Geometry", name: "Angle properties", vault_note: "Geometric Reasoning" },
        SeedSkill { id: "similarity-congruence", strand: "Geometry", name: "Congruence and similarity", vault_note: "Similar Triangles" },
        SeedSkill { id: "circle-theorems", strand: "Geometry", name: "Circle theorems", vault_note: "Circle Theorems" },
        SeedSkill { id: "linear-relationships", strand: "Algebra", name: "Linear relationships", vault_note: "Linear Relationships" },
        SeedSkill { id: "inequalities-simultaneous", strand: "Algebra", name: "Inequalities and simultaneous equations", vault_note: "Inequalities and Simultaneous Equations" },
        SeedSkill { id: "quadratics", strand: "Algebra", name: "Quadratics: expanding, factorising, solving", vault_note: "Algebraic Techniques" },
        SeedSkill { id: "nonlinear-graphs", strand: "Algebra", name: "Non-linear graph families", vault_note: "Non-Linear Relationships" },
        SeedSkill { id: "index-laws", strand: "Algebra", name: "Index laws and exponentials", vault_note: "Index Laws and Exponentials" },
        SeedSkill { id: "surds", strand: "Number", name: "Surds and irrational numbers", vault_note: "Real Numbers and Surds" },
        SeedSkill { id: "scientific-notation", strand: "Number", name: "Scientific notation", vault_note: "Real Numbers and Surds" },
        SeedSkill { id: "probability", strand: "Probability", name: "Probability", vault_note: "Conditional Probability" },
        SeedSkill { id: "statistics", strand: "Statistics", name: "Statistics and association", vault_note: "Bivariate Data and Boxplots" },
        SeedSkill { id: "trigonometry", strand: "Geometry", name: "Trigonometry", vault_note: "Trigonometry" },
        SeedSkill { id: "financial-mathematics", strand: "Number", name: "Financial mathematics", vault_note: "Financial Mathematics" },
    ]
}

/// One section-to-skill mapping rule.
struct SectionRule {
    file: &'static str,
    section_prefix: &'static str,
    first_q: u32,
    last_q: u32,
    skill: &'static str,
}

/// Which skill each Practice question links to. Geometry Section B and
/// E split by question because one section spans several skills.
fn section_rules() -> Vec<SectionRule> {
    vec![
        // Geometry Practice
        SectionRule { file: "Geometry Practice", section_prefix: "Section A", first_q: 1, last_q: 3, skill: "unit-conversion-scaling" },
        SectionRule { file: "Geometry Practice", section_prefix: "Section B", first_q: 4, last_q: 4, skill: "volume-prism-cylinder" },
        SectionRule { file: "Geometry Practice", section_prefix: "Section B", first_q: 5, last_q: 6, skill: "volume-cone-pyramid-sphere" },
        SectionRule { file: "Geometry Practice", section_prefix: "Section C", first_q: 7, last_q: 8, skill: "pythagoras-3d" },
        SectionRule { file: "Geometry Practice", section_prefix: "Section D", first_q: 9, last_q: 11, skill: "angle-properties" },
        SectionRule { file: "Geometry Practice", section_prefix: "Section E", first_q: 12, last_q: 13, skill: "similarity-congruence" },
        SectionRule { file: "Geometry Practice", section_prefix: "Section E", first_q: 14, last_q: 14, skill: "unit-conversion-scaling" },
        SectionRule { file: "Geometry Practice", section_prefix: "Section F", first_q: 15, last_q: 17, skill: "circle-theorems" },
        // Algebra Practice
        SectionRule { file: "Algebra Practice", section_prefix: "Section A", first_q: 1, last_q: 4, skill: "linear-relationships" },
        SectionRule { file: "Algebra Practice", section_prefix: "Section B", first_q: 5, last_q: 9, skill: "inequalities-simultaneous" },
        SectionRule { file: "Algebra Practice", section_prefix: "Section C", first_q: 10, last_q: 17, skill: "quadratics" },
        SectionRule { file: "Algebra Practice", section_prefix: "Section D", first_q: 18, last_q: 21, skill: "nonlinear-graphs" },
        // Q22 sits under the Section D header but is exponential content.
        SectionRule { file: "Algebra Practice", section_prefix: "Section D", first_q: 22, last_q: 22, skill: "index-laws" },
        SectionRule { file: "Algebra Practice", section_prefix: "Section E", first_q: 22, last_q: 26, skill: "index-laws" },
        // Number Practice
        SectionRule { file: "Number Practice", section_prefix: "Section A", first_q: 1, last_q: 1, skill: "surds" },
        SectionRule { file: "Number Practice", section_prefix: "Section B", first_q: 2, last_q: 4, skill: "surds" },
        SectionRule { file: "Number Practice", section_prefix: "Section C", first_q: 5, last_q: 8, skill: "surds" },
        SectionRule { file: "Number Practice", section_prefix: "Section D", first_q: 9, last_q: 10, skill: "scientific-notation" },
        SectionRule { file: "Number Practice", section_prefix: "Section E", first_q: 11, last_q: 12, skill: "measurement-error" },
        // Probability, Statistics, Trigonometry, Financial: whole-file skills
        SectionRule { file: "Probability Practice", section_prefix: "Section", first_q: 1, last_q: 99, skill: "probability" },
        SectionRule { file: "Statistics Practice", section_prefix: "Section", first_q: 1, last_q: 99, skill: "statistics" },
        SectionRule { file: "Trigonometry Practice", section_prefix: "Section", first_q: 1, last_q: 99, skill: "trigonometry" },
        SectionRule { file: "Financial Practice", section_prefix: "Section", first_q: 1, last_q: 99, skill: "financial-mathematics" },
    ]
}

/// Practice files backing the seed bank.
pub fn seed_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Geometry/Geometry Practice.md", "Geometry Practice"),
        ("Algebra/Algebra Practice.md", "Algebra Practice"),
        ("Number/Number Practice.md", "Number Practice"),
        ("Probability/Probability Practice.md", "Probability Practice"),
        ("Statistics/Statistics Practice.md", "Statistics Practice"),
        ("Trigonometry/Trigonometry Practice.md", "Trigonometry Practice"),
        ("Financial/Financial Practice.md", "Financial Practice"),
    ]
}

fn skill_for(set_title: &str, section: &str, number: u32) -> Option<&'static str> {
    section_rules().into_iter().find_map(|r| {
        (r.file == set_title
            && section.starts_with(r.section_prefix)
            && number >= r.first_q
            && number <= r.last_q)
        .then_some(r.skill)
    })
}

/// The concise final answer: the last pure-math `**...**` span
/// (digits, no letters) when the worked answer highlights values,
/// else the last digit-bearing span. The leading answer label
/// (`**22.**`) never counts: a bare question number is not an answer.
/// With no usable span, fall back to the text after the last equality
/// link, else the trimmed text. Judgment answers stay prose and the
/// gate flags them for manual approval.
fn final_answer(text: &str) -> String {
    let text = text.trim();
    // Strip a leading "**N.**" answer label.
    let mut rest = text;
    if rest.starts_with("**") {
        if let Some(rel) = rest[2..].find("**") {
            let dot = 2 + rel;
            let label = &rest[2..dot];
            if !label.is_empty()
                && label.trim_end_matches('.').chars().all(|c| c.is_ascii_digit())
            {
                rest = rest[dot + 2..].trim_start();
            }
        }
    }
    let mut spans = Vec::new();
    let mut tail = rest;
    while let Some(start) = tail.find("**") {
        let after = &tail[start + 2..];
        if let Some(end) = after.find("**") {
            spans.push(after[..end].trim().to_string());
            tail = &after[end + 2..];
        } else {
            break;
        }
    }
    let pure = |s: &String| {
        !s.is_empty()
            && s.chars().any(|c| c.is_ascii_digit())
            && !s.chars().any(|c| c.is_ascii_alphabetic())
    };
    if let Some(s) = spans
        .iter()
        .rev()
        .find(|s| pure(s))
        .or_else(|| {
            spans.iter().rev().find(|s| {
                !s.is_empty() && s.chars().any(|c| c.is_ascii_digit())
            })
        })
        .or_else(|| spans.iter().rev().find(|s| !s.is_empty()))
    {
        return s.clone();
    }
    // No highlighted span: the text after the last equality link.
    let tail = rest
        .rsplit(['=', '≈'])
        .next()
        .unwrap_or(rest)
        .trim()
        .trim_end_matches('.')
        .trim();
    if tail.len() <= 300 && tail != rest {
        return tail.to_string();
    }
    rest.trim().to_string()
}

/// Split a worked answer into single-quantity steps on sentence and
/// semicolon boundaries. The gate checks each step as its own
/// equality chain, so multi-part answers must not run together.
fn worked_steps(text: &str) -> Vec<String> {
    text.replace("; ", ". ")
        .split(". ")
        .map(|s| s.trim().trim_end_matches('.').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Per-file import outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedReport {
    pub file: String,
    pub questions: usize,
    pub imported: usize,
    pub already_present: usize,
    pub verified: usize,
    pub review_required: usize,
    pub unmapped: Vec<u32>,
    pub flagged: Vec<FlaggedItem>,
}

/// An item the gate refused to verify, with its reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlaggedItem {
    pub number: u32,
    pub prompt: String,
    pub reason: String,
}

/// Ensure all seed skills plus the STUDY-1 registry exist, so a seed
/// run on a fresh database never hits a dangling skill link.
pub async fn ensure_seed_skills(pool: &sqlx::SqlitePool) -> anyhow::Result<()> {
    for s in skill_registry() {
        sqlx::query(
            "INSERT INTO skills(id, strand, name, vault_note) VALUES(?,?,?,?)
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(&s.id)
        .bind(&s.strand)
        .bind(&s.name)
        .bind(&s.vault_note)
        .execute(pool)
        .await?;
    }
    for s in seed_skills() {
        sqlx::query(
            "INSERT INTO skills(id, strand, name, vault_note) VALUES(?,?,?,?)
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(s.id)
        .bind(s.strand)
        .bind(s.name)
        .bind(s.vault_note)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Import one Practice file; returns the per-file report.
pub async fn seed_practice_file(
    pool: &sqlx::SqlitePool,
    vault_root: &std::path::Path,
    file_rel: &str,
    set_title: &str,
) -> anyhow::Result<SeedReport> {
    ensure_seed_skills(pool).await?;
    let text = std::fs::read_to_string(vault_root.join(file_rel))?;
    let set = parse_practice_set(set_title, &text);
    let mut report = SeedReport {
        file: set_title.to_string(),
        questions: set.questions.len(),
        imported: 0,
        already_present: 0,
        verified: 0,
        review_required: 0,
        unmapped: Vec::new(),
        flagged: Vec::new(),
    };
    for q in &set.questions {
        let Some(answer) = set.answers.iter().find(|a| a.number == q.number) else {
            report.unmapped.push(q.number);
            continue;
        };
        let Some(skill) = skill_for(set_title, &q.section, q.number) else {
            report.unmapped.push(q.number);
            continue;
        };
        let item = NewItem {
            skill_id: skill.to_string(),
            prompt: format!("[{}] {}", q.section, q.text),
            answer: final_answer(&answer.text),
            worked: worked_steps(&answer.text),
            origin: Origin::Gold,
            source: None,
        };
        if find_by_prompt(pool, &item.prompt).await?.is_some() {
            report.already_present += 1;
            continue;
        }
        let stored: StoredItem = store_item(pool, &item).await?;
        report.imported += 1;
        if stored.status == "verified" {
            report.verified += 1;
        } else {
            report.review_required += 1;
            report.flagged.push(FlaggedItem {
                number: q.number,
                prompt: q.text.clone(),
                reason: stored.verify_method.clone(),
            });
        }
    }
    Ok(report)
}

/// Import every Practice set; resolves to the same rows on re-run.
pub async fn seed_all(
    pool: &sqlx::SqlitePool,
    vault_root: &std::path::Path,
) -> anyhow::Result<Vec<SeedReport>> {
    let mut out = Vec::new();
    for (rel, title) in seed_files() {
        out.push(seed_practice_file(pool, vault_root, rel, title).await?);
    }
    Ok(out)
}

/// Check every seed skill note resolves to a real vault file.
pub fn unresolvable_seed_skills(vault_root: &std::path::Path) -> Vec<String> {
    seed_skills()
        .into_iter()
        .filter(|s| find_note(vault_root, s.vault_note).is_none())
        .map(|s| format!("{} -> {}", s.id, s.vault_note))
        .collect()
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn final_answer_prefers_last_bold_span() {
        assert_eq!(
            final_answer("V = 100*pi ≈ **314.2 cm³**."),
            "314.2 cm³"
        );
        assert_eq!(final_answer("Midpoint = (1, 1)."), "(1, 1)");
        // Judgment answers keep prose: no digit-bearing span exists.
        assert_eq!(
            final_answer("The **median better represents the data**, since **6.5** is central."),
            "6.5"
        );
    }

    #[test]
    fn worked_steps_split_sentences() {
        let steps = worked_steps(
            "Slant height l = 13 cm. V = 100*pi ≈ 314.2 cm³. SA = 90*pi ≈ 282.7 cm².",
        );
        assert_eq!(steps.len(), 3);
        let steps = worked_steps("ac = 2 × (-6) = -12; need two numbers summing to 1.");
        assert_eq!(steps.len(), 2);
    }

    #[test]
    fn final_answer_prefers_pure_math_spans() {
        assert_eq!(
            final_answer("Coffee: **71.4%** exercise. Non-coffee: **40%** exercise."),
            "40%"
        );
        assert_eq!(
            final_answer("Mean **13.67**. Median **6.5**. The **median wins** here."),
            "6.5"
        );
    }

    #[test]
    fn final_answer_ignores_the_answer_label() {
        // The leading "**22.**" is a label, never the answer.
        assert_eq!(
            final_answer("**22.** 5x³ = 135 → x³ = 27 → **x = 3**."),
            "x = 3"
        );
        // No spans: fall back to the text after the last link.
        assert_eq!(
            final_answer("Sample space: {HHH, TTT}, so P = 3/8."),
            "3/8"
        );
    }

    #[test]
    fn every_practice_section_maps_to_a_skill() {
        // Geometry spot checks incl. per-question splits.
        assert_eq!(skill_for("Geometry Practice", "Section A: Units", 2), Some("unit-conversion-scaling"));
        assert_eq!(skill_for("Geometry Practice", "Section B: Volume", 4), Some("volume-prism-cylinder"));
        assert_eq!(skill_for("Geometry Practice", "Section B: Volume", 5), Some("volume-cone-pyramid-sphere"));
        assert_eq!(skill_for("Geometry Practice", "Section E: Similarity", 14), Some("unit-conversion-scaling"));
        assert_eq!(skill_for("Algebra Practice", "Section C: Quadratics", 12), Some("quadratics"));
        assert_eq!(skill_for("Algebra Practice", "Section D: Non-linear", 22), Some("index-laws"));
        assert_eq!(skill_for("Number Practice", "Section C: Expanding", 5), Some("surds"));
        assert_eq!(skill_for("Number Practice", "Section C: Expanding", 6), Some("surds"));
        assert_eq!(skill_for("Number Practice", "Section E: Error", 11), Some("measurement-error"));
        assert_eq!(skill_for("Financial Practice", "Section A: Simple", 1), Some("financial-mathematics"));
        assert_eq!(skill_for("Unknown Set", "Section A", 1), None);
    }
}
