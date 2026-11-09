use axum::{Router, routing::get};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/skills", get(breadstudy_server::api::list_skills));
    let listen = std::env::var("BREADSTUDY_LISTEN").unwrap_or_else(|_| "0.0.0.0:7290".into());
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!("breadstudy listening on {listen}");
    axum::serve(listener, app).await?;
    Ok(())
}
