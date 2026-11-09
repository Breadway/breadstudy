//! Planner: assigns the next skill per session (STUDY-5).
//!
//! Deterministic and logged. Each skill scores from assessment
//! proximity (Test 6 on 2026-11-10 first), FSRS due-ness, an outstanding
//! miss boost, and a strand-interleave penalty; every pick carries a
//! human-readable reason that answers "why this skill today".
//!
//! FSRS arrives through the narrow [`crate::session::DueScoreFn`]
//! boundary (`skill_id -> Option<due in 0..=1>`). When no scorer is
//! wired yet the planner runs with [`no_fsrs`], which scores 0, so this
//! lands without waiting on the FSRS module.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::{HashMap, HashSet};

use crate::session::DueScoreFn;

/// The Test 6 Volume/SA assessment date, used when the tests table
/// carries no assessment date yet (STUDY-15 backfills real dates).
pub fn test6_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 10).expect("fixed test date")
}

/// No FSRS scorer wired yet: everything scores 0 due-ness.
pub fn no_fsrs(_skill_id: &str) -> Option<f64> {
    None
}

pub const SKIP_BOOST: f64 = 50.0;
pub const INTERLEAVE_PENALTY: f64 = 5.0;
pub const FSRS_WEIGHT: f64 = 10.0;
pub const PROXIMITY_WEIGHT: f64 = 2.0;
pub const PROXIMITY_WINDOW_DAYS: i64 = 14;

/// One ranked skill with its logged reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredSkill {
    pub skill_id: String,
    pub strand: String,
    pub score: f64,
    pub reason: String,
}

/// Inputs to the pure scorer. `missed` holds skill ids with an
/// outstanding miss; `tests` carries `(test_id, date)` links.
pub struct ScoreInputs<'a> {
    pub today: NaiveDate,
    pub skills: &'a [(String, String)],
    pub tests: &'a [(String, String, Option<NaiveDate>)],
    pub missed: &'a HashSet<String>,
    pub last_planned: Option<(String, String)>,
    pub due: DueScoreFn,
}

fn fmt1(v: f64) -> String {
    format!("{v:.1}")
}

/// Deterministic ranking: score descending, skill id ascending.
/// Calling twice on the same inputs yields the same plan.
pub fn score_skills(inputs: &ScoreInputs) -> Vec<ScoredSkill> {
    let mut tests_by_skill: HashMap<&str, Vec<(&str, Option<NaiveDate>)>> = HashMap::new();
    for (skill_id, test_id, date) in inputs.tests {
        tests_by_skill
            .entry(skill_id.as_str())
            .or_default()
            .push((test_id.as_str(), *date));
    }
    let last_strand = inputs.last_planned.as_ref().map(|(_, s)| s.as_str());
    let mut out = Vec::new();
    for (skill_id, strand) in inputs.skills {
        let mut parts = Vec::new();
        let mut score = 1.0;
        // Assessment proximity: nearest dated linked test inside the window.
        let mut prox = 0.0;
        let mut prox_test: Option<(&str, i64)> = None;
        if let Some(links) = tests_by_skill.get(skill_id.as_str()) {
            for (test_id, date) in links {
                let mut date = *date;
                if date.is_none() && *test_id == "test-6" {
                    date = Some(test6_date());
                }
                if let Some(d) = date {
                    let days = (d - inputs.today).num_days();
                    if (0..=PROXIMITY_WINDOW_DAYS).contains(&days) {
                        let p = (PROXIMITY_WINDOW_DAYS - days) as f64 * PROXIMITY_WEIGHT;
                        if p > prox {
                            prox = p;
                            prox_test = Some((test_id, days));
                        }
                    }
                }
            }
        }
        if prox > 0.0 {
            score += prox;
            let (test_id, days) = prox_test.expect("set with prox");
            parts.push(format!("{} T-{}d +{}", test_id, days, fmt1(prox)));
        }
        // FSRS due-ness through the narrow boundary; absent scores 0.
        let due = (inputs.due)(skill_id)
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(0.0, 1.0))
            .unwrap_or(0.0);
        if due > 0.0 {
            let w = due * FSRS_WEIGHT;
            score += w;
            parts.push(format!("fsrs {} +{}", fmt1(due), fmt1(w)));
        }
        // An outstanding miss resurfaces first.
        if inputs.missed.contains(skill_id) {
            score += SKIP_BOOST;
            parts.push(format!("missed session +{}", fmt1(SKIP_BOOST)));
        }
        // Interleave strands: repeating the last strand costs a little.
        if last_strand == Some(strand.as_str()) {
            score -= INTERLEAVE_PENALTY;
            parts.push(format!("strand-repeat -{}", fmt1(INTERLEAVE_PENALTY)));
        }
        parts.push(format!("total {}", fmt1(score)));
        out.push(ScoredSkill {
            skill_id: skill_id.clone(),
            strand: strand.clone(),
            score,
            reason: parts.join("; "),
        });
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.skill_id.cmp(&b.skill_id))
    });
    out
}

/// Record a missed session so the next plan resurfaces the skill.
pub async fn record_skip(
    pool: &SqlitePool,
    skill_id: &str,
    date: NaiveDate,
    reason: &str,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO skips(id, skill_id, date, reason) VALUES(?,?,?,?)")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(skill_id)
        .bind(date.format("%Y-%m-%d").to_string())
        .bind(reason)
        .execute(pool)
        .await?;
    Ok(())
}

async fn skill_rows(pool: &SqlitePool) -> anyhow::Result<Vec<(String, String)>> {
    Ok(sqlx::query_as("SELECT id, strand FROM skills ORDER BY id")
        .fetch_all(pool)
        .await?)
}

async fn test_links(pool: &SqlitePool) -> anyhow::Result<Vec<(String, String, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT st.skill_id, st.test_id, t.assessment_date
         FROM skill_tests st JOIN tests t ON t.id = st.test_id",
    )
    .fetch_all(pool)
    .await?)
}

/// Skills with a skip in the trailing 14 days that no later plan consumed.
async fn outstanding_misses(
    pool: &SqlitePool,
    today: NaiveDate,
) -> anyhow::Result<HashSet<String>> {
    let cutoff = (today - chrono::Duration::days(PROXIMITY_WINDOW_DAYS))
        .format("%Y-%m-%d")
        .to_string();
    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT s.skill_id, s.date,
            (SELECT MAX(p.date) FROM plans p WHERE p.skill_id = s.skill_id) AS planned_after
         FROM skips s WHERE s.date >= ?",
    )
    .bind(&cutoff)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(_, skip_date, planned)| {
            planned.as_ref().map(|p| p.as_str() < skip_date.as_str()).unwrap_or(true)
        })
        .map(|(skill_id, _, _)| skill_id)
        .collect())
}

async fn last_planned(pool: &SqlitePool) -> anyhow::Result<Option<(String, String)>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT p.skill_id, s.strand FROM plans p JOIN skills s ON s.id = p.skill_id
         ORDER BY p.date DESC, p.created_at DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Plan the next skills for `date`, log the picks, and return them.
/// Reproducible for a fixed database state: the pure scorer decides.
pub async fn plan_day(
    pool: &SqlitePool,
    date: NaiveDate,
    due: DueScoreFn,
    limit: usize,
) -> anyhow::Result<Vec<ScoredSkill>> {
    let skills = skill_rows(pool).await?;
    let links = test_links(pool).await?;
    let links: Vec<(String, String, Option<NaiveDate>)> = links
        .into_iter()
        .map(|(s, t, d)| {
            let d = d.and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok());
            (s, t, d)
        })
        .collect();
    let missed = outstanding_misses(pool, date).await?;
    let last = last_planned(pool).await?;
    let inputs = ScoreInputs {
        today: date,
        skills: &skills,
        tests: &links,
        missed: &missed,
        last_planned: last,
        due,
    };
    let ranked = score_skills(&inputs);
    let date_s = date.format("%Y-%m-%d").to_string();
    for pick in ranked.iter().take(limit) {
        sqlx::query("INSERT INTO plans(id, date, skill_id, score, reason) VALUES(?,?,?,?,?)")
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&date_s)
            .bind(&pick.skill_id)
            .bind(pick.score)
            .bind(&pick.reason)
            .execute(pool)
            .await?;
    }
    Ok(ranked.into_iter().take(limit).collect())
}

#[derive(Deserialize)]
struct PlanQuery {
    date: Option<String>,
    limit: Option<usize>,
}

/// Planner routes. Merge into the application router; the returned
/// router already carries its pool. Scores without an FSRS scorer, so
/// this lands before that module; use [`router_with`] to inject one.
pub fn router(pool: SqlitePool) -> Router {
    router_with(pool, no_fsrs)
}

/// Same as [`router`], with an FSRS due-ness scorer injected at the
/// narrow `fn(&str) -> Option<f64>` boundary.
pub fn router_with(pool: SqlitePool, due: DueScoreFn) -> Router {
    let due_pool = pool.clone();
    Router::new()
        .route(
            "/api/plan",
            get(
                move |query: Query<PlanQuery>| {
                    let pool = due_pool.clone();
                    async move { plan_limited(State(pool), query, due).await }
                },
            ),
        )
        .with_state(pool)
}

async fn plan_limited(
    State(pool): State<SqlitePool>,
    query: Query<PlanQuery>,
    due: DueScoreFn,
) -> Result<Json<Vec<ScoredSkill>>, (StatusCode, Json<serde_json::Value>)> {
    let err =
        |message: String| (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": message })));
    let date = match &query.date {
        Some(d) => {
            NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .map_err(|_| err(format!("bad date {d}; use YYYY-MM-DD")))?
        }
        None => chrono::Utc::now().date_naive(),
    };
    let picks = plan_day(&pool, date, due, query.limit.unwrap_or(3))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
        })?;
    Ok(Json(picks))
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn skills() -> Vec<(String, String)> {
        vec![
            ("surface-area".into(), "Geometry".into()),
            ("surds".into(), "Number".into()),
            ("volume-prism-cylinder".into(), "Geometry".into()),
        ]
    }

    fn inputs<'a>(
        skills: &'a [(String, String)],
        tests: &'a [(String, String, Option<NaiveDate>)],
        missed: &'a HashSet<String>,
        last: Option<(String, String)>,
    ) -> ScoreInputs<'a> {
        ScoreInputs {
            today: NaiveDate::from_ymd_opt(2026, 11, 9).unwrap(),
            skills,
            tests,
            missed,
            last_planned: last,
            due: no_fsrs,
        }
    }

    #[test]
    fn plan_is_reproducible_for_a_fixed_date() {
        let skills = skills();
        let tests = vec![(
            "volume-prism-cylinder".to_string(),
            "test-6".to_string(),
            None,
        )];
        let missed = HashSet::new();
        let a = score_skills(&inputs(&skills, &tests, &missed, None));
        let b = score_skills(&inputs(&skills, &tests, &missed, None));
        assert_eq!(a, b);
        assert!(!a.is_empty());
        for pick in &a {
            assert!(pick.reason.contains("total"), "every pick logs its reason");
        }
    }

    #[test]
    fn test6_proximity_outranks_unlinked_skills() {
        let skills = skills();
        let tests = vec![(
            "volume-prism-cylinder".to_string(),
            "test-6".to_string(),
            None,
        )];
        let missed = HashSet::new();
        let ranked = score_skills(&inputs(&skills, &tests, &missed, None));
        assert_eq!(ranked[0].skill_id, "volume-prism-cylinder");
        assert!(ranked[0].reason.contains("test-6"));
    }

    #[test]
    fn missed_session_resurfaces_first() {
        let skills = skills();
        let tests = vec![(
            "volume-prism-cylinder".to_string(),
            "test-6".to_string(),
            None,
        )];
        let missed: HashSet<String> = ["surds".to_string()].into_iter().collect();
        let ranked = score_skills(&inputs(&skills, &tests, &missed, None));
        assert_eq!(ranked[0].skill_id, "surds");
        assert!(ranked[0].reason.contains("missed"));
    }

    #[test]
    fn interleave_penalizes_repeating_strand() {
        let skills = skills();
        let tests: Vec<(String, String, Option<NaiveDate>)> = vec![];
        let missed = HashSet::new();
        let last = Some(("volume-prism-cylinder".to_string(), "Geometry".to_string()));
        let ranked = score_skills(&inputs(&skills, &tests, &missed, last));
        assert_eq!(ranked[0].skill_id, "surds");
        assert!(ranked[0].score > ranked[1].score);
    }
}
