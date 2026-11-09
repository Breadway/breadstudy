//! `breadstudy-index`: run the vault indexer against a SQLite database.
//!
//! Reads the Year 10 Maths vault checkout (Methods Pathway Overview,
//! strand notes, Practice sets) and stores skills, tests and links.
//! The vault is the source of truth; re-runs are idempotent.

use std::path::PathBuf;

fn arg_value(name: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == name {
            return args.next();
        }
        if let Some(v) = a.strip_prefix(&format!("{name}=")) {
            return Some(v.to_string());
        }
    }
    None
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let db = arg_value("--db").unwrap_or_else(|| "data/breadstudy.sqlite".to_string());
    let vault = arg_value("--vault").unwrap_or_else(|| {
        std::env::var("BREADSTUDY_VAULT")
            .unwrap_or_else(|_| "/home/breadway/Projects/school-notes-year-10/Maths".to_string())
    });
    let pool = breadstudy_server::db::connect(&db).await?;
    let report =
        breadstudy_server::indexer::index_vault(&pool, &PathBuf::from(&vault)).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.test6_skills.is_empty() && report.missing.is_empty() {
        Ok(())
    } else if report.test6_skills.is_empty() {
        anyhow::bail!("no skills resolved for Test 6");
    } else {
        anyhow::bail!("vault notes missing for skills: {}", report.missing.join(", "));
    }
}
