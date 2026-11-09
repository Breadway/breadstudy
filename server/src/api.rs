/// One indexed skill, as exposed to the PWA dashboard.
#[derive(serde::Serialize)]
pub struct SkillSummary {
    pub id: String,
    pub strand: String,
    pub name: String,
    pub vault_note: String,
}

/// `GET /api/skills`: every skill the vault indexer imported.
async fn list_skills(
    State(pool): State<SqlitePool>,
) -> Result<Json<Vec<SkillSummary>>, (StatusCode, String)> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT id, strand, name, vault_note FROM skills ORDER BY strand, name",
    )
    .fetch_all(&pool)
    .await
    .map_err(internal_error_message)?;

    Ok(Json(
        rows.into_iter()
            .map(|(id, strand, name, vault_note)| SkillSummary {
                id,
                strand,
                name,
                vault_note,
            })
            .collect(),
    ))
}

/// Merge into the application router with the pool attached.
pub fn skills_router(pool: SqlitePool) -> Router {
    Router::new()
        .route("/api/skills", get(list_skills))
        .with_state(pool)
}

use std::sync::Arc;

use serde_json::json;

use crate::grading::{AttemptRequest, GradeError, GradingService};

/// State for the grading routes. The rest of the server can merge this
/// router once `main.rs` imports it.
#[derive(Clone)]
pub struct GradingState {
    pub pool: SqlitePool,
    pub grading: Arc<GradingService>,
}

/// Router fragment for STUDY-17. Merge it with the base router:
///
/// ```ignore
/// let app = Router::new().route("/healthz", get(|| async { "ok" }))
///     .merge(breadstudy_server::api::grading_router(state));
/// ```
pub fn grading_router(state: GradingState) -> Router {
    Router::new()
        .route("/api/attempts", post(grade_attempt))
        .with_state(state)
}

/// `POST /api/attempts`: grade a photo, typed LaTeX or spoken transcript
/// against the item rubric and persist the attempt.
pub async fn grade_attempt(
    State(state): State<GradingState>,
    Json(request): Json<AttemptRequest>,
) -> Response {
    match state.grading.grade(&state.pool, request).await {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(error) => {
            let status = match &error {
                GradeError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
                GradeError::ItemNotFound(_) => StatusCode::NOT_FOUND,
                GradeError::AllGradersFailed(_) => StatusCode::BAD_GATEWAY,
                GradeError::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,
                GradeError::Db(_) | GradeError::Data(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            if status.is_server_error() {
                tracing::error!(error = %error, "grading request failed");
            } else {
                tracing::warn!(error = %error, "grading request rejected");
            }
            (
                status,
                Json(json!({
                    "status": "error",
                    "error": error.to_string(),
                })),
            )
                .into_response()
        }
    }
}
// Route blocks appended by STUDY-18 (FSRS planner, hints, weekly review)
// and STUDY-16 (server-driven session engine). Both are merged into the
// application router by main.rs.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::fsrs::{self, DueSkill, HintReveal, WeeklySnapshot};
use crate::session::{
    self, HintOutcome, OffTaskOutcome, SessionEngine, SessionView, SkipOutcome, StartOptions,
    Template,
};

// ---------------------------------------------------------------------------
// FSRS planner, hint ladder and weekly review (STUDY-18)
// ---------------------------------------------------------------------------

/// Mount with `.merge(api::fsrs_routes()).with_state(pool)` once the
/// server owns a pool.
pub fn fsrs_routes() -> Router<SqlitePool> {
    Router::new()
        .route("/api/plan/due", get(api_due_skills))
        .route("/api/items/{item_id}/hint", post(api_next_hint))
        .route("/api/weekly/snapshot", get(api_weekly_snapshot))
}

fn internal_error_message(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

async fn api_due_skills(
    State(pool): State<SqlitePool>,
) -> Result<Json<Vec<DueSkill>>, (StatusCode, String)> {
    fsrs::due_skills(&pool)
        .await
        .map(Json)
        .map_err(internal_error_message)
}

async fn api_next_hint(
    State(pool): State<SqlitePool>,
    Path(item_id): Path<String>,
) -> Result<Json<Option<HintReveal>>, (StatusCode, String)> {
    fsrs::next_hint(&pool, &item_id)
        .await
        .map(Json)
        .map_err(internal_error_message)
}

#[derive(Debug, Deserialize)]
struct SnapshotQuery {
    since: Option<String>,
}

async fn api_weekly_snapshot(
    State(pool): State<SqlitePool>,
    Query(query): Query<SnapshotQuery>,
) -> Result<Json<WeeklySnapshot>, (StatusCode, String)> {
    fsrs::weekly_snapshot(&pool, query.since.as_deref())
        .await
        .map(Json)
        .map_err(internal_error_message)
}

// ---------------------------------------------------------------------------
// Server-driven session routes (STUDY-16)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct SessionState {
    pool: SqlitePool,
    engine: SessionEngine,
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(id: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("session {id} not found"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

fn internal_error(error: anyhow::Error) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: error.to_string(),
    }
}

#[derive(serde::Deserialize)]
struct StartSessionRequest {
    template: Option<String>,
    grace_tokens: Option<u32>,
}

#[derive(serde::Deserialize)]
struct OffTaskRequest {
    seconds: u32,
}

#[derive(serde::Deserialize)]
struct SkipRequest {
    reason: String,
}

async fn start_session(
    State(state): State<SessionState>,
    Json(request): Json<StartSessionRequest>,
) -> Result<(StatusCode, Json<SessionView>), ApiError> {
    let template = match request.template.as_deref() {
        None => Template::SchoolDay20,
        Some(value) => Template::parse(value).ok_or_else(|| {
            ApiError::bad_request(format!(
                "unknown template {value}; use school_day_20 or weekend_40"
            ))
        })?,
    };
    let mut options = StartOptions::new(template);
    if let Some(tokens) = request.grace_tokens {
        options.grace_tokens = tokens;
    }
    options.engine = state.engine;
    let view = session::start(&state.pool, options, Utc::now())
        .await
        .map_err(internal_error)?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn get_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
) -> Result<Json<SessionView>, ApiError> {
    let view = session::get(&state.pool, &id, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(view))
}

async fn tick_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
) -> Result<Json<SessionView>, ApiError> {
    let view = session::tick(&state.pool, &id, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(view))
}

async fn advance_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
) -> Result<Json<SessionView>, ApiError> {
    let view = session::advance(&state.pool, &id, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(view))
}

async fn off_task_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
    Json(request): Json<OffTaskRequest>,
) -> Result<Json<OffTaskOutcome>, ApiError> {
    let outcome = session::off_task(&state.pool, &id, request.seconds, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(outcome))
}

async fn resume_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
) -> Result<Json<SessionView>, ApiError> {
    let view = session::resume(&state.pool, &id, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(view))
}

async fn skip_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
    Json(request): Json<SkipRequest>,
) -> Result<Json<SkipOutcome>, ApiError> {
    let outcome = session::skip(&state.pool, &id, &request.reason, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(outcome))
}

async fn hint_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
) -> Result<Json<HintOutcome>, ApiError> {
    let outcome = session::hint(&state.pool, &id, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(outcome))
}

async fn next_item_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
) -> Result<Json<SessionView>, ApiError> {
    let view = session::next_item(&state.pool, &id, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(view))
}

async fn weekly_skips(
    State(state): State<SessionState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let count = session::weekly_skip_count(&state.pool, Utc::now())
        .await
        .map_err(internal_error)?;
    Ok(Json(serde_json::json!({ "weekly_skip_count": count })))
}

/// Routes for the server-driven session engine. Merge this into the
/// application router; the returned router already carries its pool.
pub fn session_router(pool: SqlitePool) -> Router {
    session_router_with_engine(pool, SessionEngine::new())
}

/// Same as [`session_router`], with an optional FSRS due-ness scorer
/// injected at the narrow `fn(&str) -> Option<f64>` boundary.
pub fn session_router_with_engine(pool: SqlitePool, engine: SessionEngine) -> Router {
    Router::new()
        .route("/api/session", post(start_session))
        .route("/api/session/{id}", get(get_session))
        .route("/api/session/{id}/tick", post(tick_session))
        .route("/api/session/{id}/advance", post(advance_session))
        .route("/api/session/{id}/off-task", post(off_task_session))
        .route("/api/session/{id}/resume", post(resume_session))
        .route("/api/session/{id}/skip", post(skip_session))
        .route("/api/session/{id}/hint", post(hint_session))
        .route("/api/session/{id}/next-item", post(next_item_session))
        .route("/api/skips/weekly", get(weekly_skips))
        .with_state(SessionState { pool, engine })
}
