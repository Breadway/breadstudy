//! Progress surface (STUDY-42): recent graded attempts, per-skill mastery
//! with due dates, and recurring error patterns.
//!
//! This is a read-only projection over tables other modules already own:
//! `attempts` (STUDY-17 grading and STUDY-18 FSRS), `items`/`skills`
//! (indexer and seeder), `skill_schedule` (STUDY-18) and `error_patterns`
//! (STUDY-18). Nothing here writes, and the shapes are additive: the
//! existing `/api/session` and `/api/attempts` contracts are untouched.
//!
//! The PWA renders `GET /api/progress` rather than rebuilding this view
//! from localStorage, so the numbers reflect attempts the server actually
//! recorded.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

/// Default number of attempts returned when the caller does not ask.
pub const DEFAULT_ATTEMPT_LIMIT: i64 = 25;
/// Upper bound so a query cannot pull the whole attempt history.
pub const MAX_ATTEMPT_LIMIT: i64 = 200;

/// One graded attempt, flattened for the progress list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProgressAttempt {
    pub attempt_id: String,
    pub item_id: String,
    pub prompt: String,
    pub skill_id: String,
    pub skill_name: String,
    /// `correct`, `partial` (right answer with thin working) or `incorrect`.
    pub verdict: String,
    pub correct: bool,
    pub method_marks: f64,
    pub method_marks_total: f64,
    pub final_marks: f64,
    pub final_marks_total: f64,
    /// Error tags as stored, e.g. `["sign-slip","algebra"]`.
    pub error_tags: Vec<String>,
    pub hints_used: i64,
    pub input_kind: String,
    pub created_at: String,
}

/// Per-skill mastery from `skill_schedule`, with the planner's due date.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProgressSkill {
    pub skill_id: String,
    pub name: String,
    pub strand: String,
    pub mastery: f64,
    pub accuracy: Option<f64>,
    pub due_at: Option<String>,
    pub last_review: Option<String>,
    pub reviews: i64,
    pub lapses: i64,
}

/// A recurring error tag aggregated per skill.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProgressErrorPattern {
    pub skill_id: String,
    pub skill_name: String,
    pub tag: String,
    pub count: i64,
    pub first_seen: String,
    pub last_seen: String,
}

/// Everything the progress page renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressSnapshot {
    pub generated_at: String,
    pub attempt_limit: i64,
    pub attempts_total: i64,
    pub recent_attempts: Vec<ProgressAttempt>,
    pub skills: Vec<ProgressSkill>,
    pub error_patterns: Vec<ProgressErrorPattern>,
}

#[derive(Debug, Deserialize)]
struct ProgressQuery {
    limit: Option<i64>,
}

fn clamp_limit(requested: Option<i64>) -> i64 {
    requested
        .unwrap_or(DEFAULT_ATTEMPT_LIMIT)
        .clamp(1, MAX_ATTEMPT_LIMIT)
}

fn verdict(
    correct: bool,
    method_marks: f64,
    method_total: f64,
    final_marks: f64,
    final_total: f64,
) -> &'static str {
    if !correct {
        return "incorrect";
    }
    let final_full = final_total <= 0.0 || final_marks >= final_total;
    let method_full = method_total <= 0.0 || method_marks >= method_total;
    if final_full && method_full {
        "correct"
    } else {
        // Right answer, but the method marks say the working was thin.
        "partial"
    }
}

/// Most recent attempts joined to their item prompt and skill name.
pub async fn recent_attempts(
    pool: &SqlitePool,
    limit: i64,
) -> anyhow::Result<Vec<ProgressAttempt>> {
    let limit = limit.clamp(1, MAX_ATTEMPT_LIMIT);
    let rows = sqlx::query(
        "SELECT a.id, a.item_id, i.prompt, i.skill_id, COALESCE(s.name, i.skill_id) AS skill_name, \
         a.correct, a.method_marks, a.method_marks_total, a.final_marks, a.final_marks_total, \
         a.error_tags, a.hints_used, a.input_kind, a.created_at \
         FROM attempts a \
         JOIN items i ON i.id = a.item_id \
         LEFT JOIN skills s ON s.id = i.skill_id \
         ORDER BY a.created_at DESC, a.id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let correct = row.get::<i64, _>("correct") != 0;
            let method_marks = row.get::<f64, _>("method_marks");
            let method_marks_total = row.get::<f64, _>("method_marks_total");
            let final_marks = row.get::<f64, _>("final_marks");
            let final_marks_total = row.get::<f64, _>("final_marks_total");
            let tags: String = row.get("error_tags");
            ProgressAttempt {
                attempt_id: row.get("id"),
                item_id: row.get("item_id"),
                prompt: row.get("prompt"),
                skill_id: row.get("skill_id"),
                skill_name: row.get("skill_name"),
                verdict: verdict(
                    correct,
                    method_marks,
                    method_marks_total,
                    final_marks,
                    final_marks_total,
                )
                .to_string(),
                correct,
                method_marks,
                method_marks_total,
                final_marks,
                final_marks_total,
                error_tags: serde_json::from_str(&tags).unwrap_or_default(),
                hints_used: row.get("hints_used"),
                input_kind: row.get("input_kind"),
                created_at: row.get("created_at"),
            }
        })
        .collect())
}

/// Every skill with its scheduled mastery and due date, weakest first.
pub async fn skill_mastery(pool: &SqlitePool) -> anyhow::Result<Vec<ProgressSkill>> {
    let rows = sqlx::query(
        "SELECT s.id, s.name, s.strand, sch.mastery, sch.accuracy, sch.due_at, \
         sch.last_review, sch.reviews, sch.lapses \
         FROM skills s LEFT JOIN skill_schedule sch ON sch.skill_id = s.id \
         ORDER BY COALESCE(sch.mastery, 0.4) ASC, s.name",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| ProgressSkill {
            skill_id: row.get("id"),
            name: row.get("name"),
            strand: row.get("strand"),
            mastery: row.get::<Option<f64>, _>("mastery").unwrap_or(0.4),
            accuracy: row.get("accuracy"),
            due_at: row.get("due_at"),
            last_review: row.get("last_review"),
            reviews: row.get::<Option<i64>, _>("reviews").unwrap_or(0),
            lapses: row.get::<Option<i64>, _>("lapses").unwrap_or(0),
        })
        .collect())
}

/// Error tags across all skills, most frequent first, with skill names.
pub async fn error_patterns(pool: &SqlitePool) -> anyhow::Result<Vec<ProgressErrorPattern>> {
    let rows = sqlx::query(
        "SELECT ep.skill_id, COALESCE(s.name, ep.skill_id) AS skill_name, ep.tag, \
         ep.count, ep.first_seen, ep.last_seen \
         FROM error_patterns ep LEFT JOIN skills s ON s.id = ep.skill_id \
         ORDER BY ep.count DESC, ep.last_seen DESC, ep.tag",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| ProgressErrorPattern {
            skill_id: row.get("skill_id"),
            skill_name: row.get("skill_name"),
            tag: row.get("tag"),
            count: row.get("count"),
            first_seen: row.get("first_seen"),
            last_seen: row.get("last_seen"),
        })
        .collect())
}

/// Build the full progress snapshot in one read pass.
pub async fn progress_snapshot(pool: &SqlitePool, limit: i64) -> anyhow::Result<ProgressSnapshot> {
    let limit = clamp_limit(Some(limit));
    let attempts_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attempts")
        .fetch_one(pool)
        .await?;
    Ok(ProgressSnapshot {
        generated_at: chrono::Utc::now().to_rfc3339(),
        attempt_limit: limit,
        attempts_total,
        recent_attempts: recent_attempts(pool, limit).await?,
        skills: skill_mastery(pool).await?,
        error_patterns: error_patterns(pool).await?,
    })
}

async fn api_progress(
    State(pool): State<SqlitePool>,
    Query(query): Query<ProgressQuery>,
) -> Result<Json<ProgressSnapshot>, (StatusCode, String)> {
    progress_snapshot(&pool, clamp_limit(query.limit))
        .await
        .map(Json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

/// Routes for the progress page. Merge with the application router.
pub fn progress_router(pool: SqlitePool) -> Router {
    Router::new()
        .route("/api/progress", get(api_progress))
        .with_state(pool)
}
