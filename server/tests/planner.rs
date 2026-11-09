//! STUDY-5 acceptance: the planner is reproducible for a fixed date,
//! a missed session resurfaces first, and every pick logs its reason.

use chrono::NaiveDate;

fn temp_db(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "breadstudy-planner-test-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path.to_string_lossy().to_string()
}

async fn test_pool(db: &str) -> sqlx::SqlitePool {
    let pool = breadstudy_server::db::connect(db).await.unwrap();
    for s in breadstudy_server::indexer::skill_registry() {
        sqlx::query("INSERT OR IGNORE INTO skills(id, strand, name, vault_note) VALUES(?,?,?,?)")
            .bind(&s.id)
            .bind(&s.strand)
            .bind(&s.name)
            .bind(&s.vault_note)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT OR IGNORE INTO tests(id, ord, topic, main_notes) VALUES('test-6', 6, 'Volume, Capacity and Surface Area', '[\"Area Volume and Surface Area\"]')")
        .execute(&pool)
        .await
        .unwrap();
    for skill in ["volume-prism-cylinder", "unit-conversion-scaling"] {
        sqlx::query("INSERT OR IGNORE INTO skill_tests(skill_id, test_id) VALUES(?, 'test-6')")
            .bind(skill)
            .execute(&pool)
            .await
            .unwrap();
    }
    pool
}

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 9).unwrap()
}

#[tokio::test]
async fn plan_writes_logged_picks() {
    let db = temp_db("writes");
    let pool = test_pool(&db).await;
    let picks = breadstudy_server::planner::plan_day(
        &pool,
        day(),
        breadstudy_server::planner::no_fsrs,
        2,
    )
    .await
    .unwrap();
    assert_eq!(picks.len(), 2);
    for p in &picks {
        assert!(p.reason.contains("total"), "logged reason: {}", p.reason);
    }
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM plans")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 2);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn skipped_skill_resurfaces_on_the_next_plan() {
    let db = temp_db("skip");
    let pool = test_pool(&db).await;
    breadstudy_server::planner::record_skip(&pool, "measurement-error", day(), "sick")
        .await
        .unwrap();
    let picks = breadstudy_server::planner::plan_day(
        &pool,
        day(),
        breadstudy_server::planner::no_fsrs,
        3,
    )
    .await
    .unwrap();
    assert_eq!(picks[0].skill_id, "measurement-error");
    assert!(picks[0].reason.contains("missed"));
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn plan_is_reproducible_for_a_fixed_database_state() {
    let db = temp_db("repro");
    let pool = test_pool(&db).await;
    let first = breadstudy_server::planner::plan_day(
        &pool,
        day(),
        breadstudy_server::planner::no_fsrs,
        6,
    )
    .await
    .unwrap();
    // A second identical day would see the first plan as history, so
    // reproducibility is asserted on a fresh database with the same seed.
    let db2 = temp_db("repro2");
    let pool2 = test_pool(&db2).await;
    let second = breadstudy_server::planner::plan_day(
        &pool2,
        day(),
        breadstudy_server::planner::no_fsrs,
        6,
    )
    .await
    .unwrap();
    assert_eq!(first, second);
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(&db2);
}

#[tokio::test]
async fn fsrs_scorer_is_optional_but_used_when_given() {
    fn surface_due(skill: &str) -> Option<f64> {
        Some(if skill == "surface-area" { 1.0 } else { 0.0 })
    }
    // Far from any assessment: proximity contributes nothing.
    let far = NaiveDate::from_ymd_opt(2026, 12, 20).unwrap();
    let db = temp_db("fsrs-plain");
    let pool = test_pool(&db).await;
    let plain =
        breadstudy_server::planner::plan_day(&pool, far, breadstudy_server::planner::no_fsrs, 6)
            .await
            .unwrap();
    assert!(plain.iter().all(|p| !p.reason.contains("fsrs")));
    assert_eq!(plain[0].skill_id, "measurement-error");
    let _ = std::fs::remove_file(&db);
    let db = temp_db("fsrs-scored");
    let pool = test_pool(&db).await;
    let scored = breadstudy_server::planner::plan_day(&pool, far, surface_due, 6)
        .await
        .unwrap();
    assert_eq!(scored[0].skill_id, "surface-area");
    assert!(scored[0].reason.contains("fsrs"));
    let _ = std::fs::remove_file(&db);
}
