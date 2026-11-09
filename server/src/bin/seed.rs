//! `breadstudy-seed`: import the vault Practice sets as gold items.
//!
//! Every question becomes a skill-linked item gated by SymPy; the
//! per-file JSON report records verified vs review_required counts so
//! mismatches stay visible instead of being silently fixed.

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
    let vault_root = PathBuf::from(&vault);
    let bad = breadstudy_server::seed::unresolvable_seed_skills(&vault_root);
    if !bad.is_empty() {
        anyhow::bail!("seed skill notes missing from vault: {}", bad.join(", "));
    }
    let reports = breadstudy_server::seed::seed_all(&pool, &vault_root).await?;
    println!("{}", serde_json::to_string_pretty(&reports)?);
    Ok(())
}
