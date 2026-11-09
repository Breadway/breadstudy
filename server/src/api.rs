pub async fn list_skills() -> &'static str {
    "[]"
}

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::json;
use sqlx::SqlitePool;

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
