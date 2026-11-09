pub async fn list_skills() -> &'static str {
    "[]"
}

// STUDY-18: FSRS planner, hint ladder and weekly-review routes. Mount with
// `.merge(api::fsrs_routes()).with_state(pool)` once the server owns a pool.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::fsrs::{self, DueSkill, HintReveal, WeeklySnapshot};

fn internal(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

/// Routes consumed by the session PWA and the weekly review.
pub fn fsrs_routes() -> Router<sqlx::SqlitePool> {
    Router::new()
        .route("/api/plan/due", get(api_due_skills))
        .route("/api/items/{item_id}/hint", post(api_next_hint))
        .route("/api/weekly/snapshot", get(api_weekly_snapshot))
}

async fn api_due_skills(
    State(pool): State<sqlx::SqlitePool>,
) -> Result<Json<Vec<DueSkill>>, (StatusCode, String)> {
    fsrs::due_skills(&pool).await.map(Json).map_err(internal)
}

async fn api_next_hint(
    State(pool): State<sqlx::SqlitePool>,
    Path(item_id): Path<String>,
) -> Result<Json<Option<HintReveal>>, (StatusCode, String)> {
    fsrs::next_hint(&pool, &item_id)
        .await
        .map(Json)
        .map_err(internal)
}

#[derive(Debug, Deserialize)]
struct SnapshotQuery {
    since: Option<String>,
}

async fn api_weekly_snapshot(
    State(pool): State<sqlx::SqlitePool>,
    Query(query): Query<SnapshotQuery>,
) -> Result<Json<WeeklySnapshot>, (StatusCode, String)> {
    fsrs::weekly_snapshot(&pool, query.since.as_deref())
        .await
        .map(Json)
        .map_err(internal)
}
