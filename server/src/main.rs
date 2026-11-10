use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Request};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Json, Router};
use breadstudy_server::{api, db, grading::GradingService, planner, static_web};
use sqlx::SqlitePool;
use tracing_subscriber::EnvFilter;

const DEFAULT_LISTEN: &str = "0.0.0.0:7290";
const DEFAULT_DATABASE_PATH: &str = "/var/lib/breadstudy/breadstudy.db";
const DEFAULT_WEB_DIR: &str = "/srv/breadstudy-web";
/// A phone photo arrives base64-encoded inside JSON, so the raw request is
/// larger than the image. 8 MB accepts a resized photo with headroom while
/// keeping an unbounded body off the server.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Replace axum's bare 413 text with an actionable JSON error the PWA can
/// show. The status stays 413 so clients can still branch on it.
async fn clarify_payload_too_large(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "status": "error",
                "error": "Upload is too large. The server accepts request bodies up to 8 MB; resize the photo and retry.",
                "max_body_bytes": MAX_BODY_BYTES,
            })),
        )
            .into_response();
    }
    response
}

/// Build the application router around a live SQLite pool.
///
/// Every module that shipped a router fragment is mounted here: the session
/// engine (STUDY-16), the FSRS planner, hints and weekly review (STUDY-18),
/// the skill planner (STUDY-5) and photo grading (STUDY-17). Grading reads its
/// endpoint and key from the environment at startup, so a missing key fails
/// here with the paths it tried instead of surfacing later as a request error.
fn build_router(pool: SqlitePool, web_dir: PathBuf) -> anyhow::Result<Router> {
    let grading = api::GradingState {
        pool: pool.clone(),
        grading: Arc::new(GradingService::from_env()?),
    };
    let web_root = web_dir.clone();
    let mut app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(api::skills_router(pool.clone()))
        .merge(api::session_router_with_engine(
            pool.clone(),
            breadstudy_server::session::SessionEngine::fsrs(),
        ))
        .merge(planner::router(pool.clone()))
        .merge(api::fsrs_routes().with_state(pool.clone()))
        .merge(api::grading_router(grading));

    // The teacher scratchpad is optional: without a key the rest of the app
    // still works, and the UI shows the failure instead of a blank pane.
    match breadstudy_server::teach::TeachClient::from_env() {
        Ok(client) => {
            app = app.merge(breadstudy_server::teach::router(pool.clone(), client));
        }
        Err(error) => tracing::warn!("teacher scratchpad disabled: {error}"),
    }

    Ok(app
        .route(
            "/",
            get({
                let root = web_root.clone();
                move || {
                    let root = root.clone();
                    async move { static_web::serve(root, None).await }
                }
            }),
        )
        .route(
            "/{*path}",
            get({
                let root = web_root;
                move |path| {
                    let root = root.clone();
                    async move { static_web::serve(root, Some(path)).await }
                }
            }),
        )
        // DefaultBodyLimit wraps the extractors; the clarify layer sits
        // inside it so a rejection produced by Json still gets the JSON
        // message instead of axum's plain-text buffer error.
        .layer(middleware::from_fn(clarify_payload_too_large))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES)))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let database_path = std::env::var("BREADSTUDY_DATABASE_PATH")
        .unwrap_or_else(|_| DEFAULT_DATABASE_PATH.to_string());
    let pool = db::connect(&database_path)
        .await
        .map_err(|error| anyhow::anyhow!("opening {database_path}: {error}"))?;
    let web_dir = std::env::var("BREADSTUDY_WEB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_WEB_DIR));
    let app = build_router(pool, web_dir)?;

    let listen = std::env::var("BREADSTUDY_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_string());
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    let address = listener.local_addr()?;
    tracing::info!(%database_path, "breadstudy listening on {address}");
    axum::serve(listener, app).await?;
    Ok(())
}
