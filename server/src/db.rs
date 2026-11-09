use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;

pub async fn connect(db_path: &str) -> anyhow::Result<sqlx::SqlitePool> {
    if let Some(parent) = Path::new(db_path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&format!("sqlite:{db_path}?mode=rwc"))
        .await?;
    sqlx::migrate!("../migrations").run(&pool).await?;
    Ok(pool)
}
