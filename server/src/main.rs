use std::path::PathBuf;
use std::sync::Arc;

use axum::{routing::get, Router};
use breadstudy_server::{api, db, grading::GradingService, planner, static_web};
use sqlx::SqlitePool;
use tracing_subscriber::EnvFilter;

const DEFAULT_LISTEN: &str = "0.0.0.0:7290";
const DEFAULT_DATABASE_PATH: &str = "/var/lib/breadstudy/breadstudy.db";
const DEFAULT_WEB_DIR: &str = "/srv/breadstudy-web";

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
    Ok(Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(api::skills_router(pool.clone()))
        .merge(api::session_router(pool.clone()))
        .merge(planner::router(pool.clone()))
        .merge(api::fsrs_routes().with_state(pool.clone()))
        .merge(api::grading_router(grading))
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
        ))
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
