//! STUDY-4 acceptance: all importable Practice questions land with
//! skill links, mismatches are flagged review_required (never silently
//! fixed), and SymPy re-check results are recorded per item.

use std::path::PathBuf;

use breadstudy_server::seed::{seed_all, unresolvable_seed_skills};
use breadstudy_server::verify::{NewItem, Origin, store_item};

fn vault_root() -> PathBuf {
    PathBuf::from(
        std::env::var("BREADSTUDY_VAULT")
            .unwrap_or_else(|_| "/home/breadway/Projects/school-notes-year-10/Maths".to_string()),
    )
}

fn temp_db(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "breadstudy-seed-test-{}-{}.sqlite",
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
    breadstudy_server::seed::ensure_seed_skills(&pool).await.unwrap();
    pool
}

#[tokio::test]
async fn seed_imports_every_practice_question_with_skill_links() {
    let vault = vault_root();
    assert!(vault.is_dir(), "vault checkout missing at {}", vault.display());
    assert!(
        unresolvable_seed_skills(&vault).is_empty(),
        "seed skills must resolve: {:?}",
        unresolvable_seed_skills(&vault)
    );
    let db = temp_db("all");
    let pool = test_pool(&db).await;
    let reports = seed_all(&pool, &vault).await.expect("seed run");
    assert_eq!(reports.len(), 7, "seven practice sets must import");
    let total_q: usize = reports.iter().map(|r| r.questions).sum();
    assert!(total_q > 90, "expected 90+ questions, got {total_q}");
    for r in &reports {
        assert!(r.unmapped.is_empty(), "{} unmapped: {:?}", r.file, r.unmapped);
        assert_eq!(
            r.questions,
            r.imported + r.already_present,
            "{} must account for every question",
            r.file
        );
        assert_eq!(r.already_present, 0, "first run imports everything");
        assert_eq!(
            r.verified + r.review_required,
            r.imported,
            "{} every import lands verified or review_required",
            r.file
        );
        for f in &r.flagged {
            assert!(
                f.reason.starts_with("gate:"),
                "flagged items carry the gate reason, got {}",
                f.reason
            );
        }
    }
    // Every item links a real skill and records a SymPy re-check.
    let (n_items, n_skills, n_detail): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(DISTINCT skill_id),
            SUM(CASE WHEN verify_detail != '{}' AND verify_detail != '' THEN 1 ELSE 0 END)
         FROM items",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n_items, total_q as i64);
    assert!(n_skills >= 15, "questions span many skills, got {n_skills}");
    assert_eq!(n_detail, n_items, "every item records its re-check");
    let (n_bad,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM items WHERE status NOT IN ('verified','review_required')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(n_bad, 0);
    println!("{}", serde_json::to_string_pretty(&reports).unwrap());

    // Second run: no dupes, no changes.
    let reports2 = seed_all(&pool, &vault).await.expect("second seed run");
    for r in &reports2 {
        assert_eq!(r.imported, 0, "{} re-imported on second run", r.file);
        assert_eq!(r.already_present, r.questions);
    }
    let (n_items2,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM items")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n_items, n_items2);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn corrupted_worked_answer_is_flagged_not_silently_fixed() {
    let db = temp_db("corrupt");
    let pool = test_pool(&db).await;
    // Real cone worked steps with one transcribed number altered.
    let bad = NewItem {
        skill_id: "volume-cone-pyramid-sphere".into(),
        prompt: "[Section B: Volume and surface area] CORRUPT probe: slant height, volume and surface area of a cone r=5 h=12.".into(),
        answer: "282.7 cm2".into(),
        worked: vec![
            "Slant height l = sqrt(25 + 144) = sqrt(169) = 13".into(),
            "V = (1/3)*pi*(25)*(12) = 100*pi ≈ 314.2".into(),
            "SA = pi*(5)*(18) = 90*pi ≈ 282.9".into(),
        ],
        origin: Origin::Gold,
        source: None,
    };
    let stored = store_item(&pool, &bad).await.unwrap();
    assert_eq!(stored.status, "review_required", "detail: {}", stored.verify_detail);
    assert!(stored.verify_method.contains("sympy-mismatch"));

    // The true transcription verifies, proving the flag is about the math.
    let good = NewItem {
        prompt: "[Section B: Volume and surface area] TRUE probe: slant height, volume and surface area of a cone r=5 h=12.".into(),
        worked: vec![
            "Slant height l = sqrt(25 + 144) = sqrt(169) = 13".into(),
            "V = (1/3)*pi*(25)*(12) = 100*pi ≈ 314.2".into(),
            "SA = pi*(5)*(18) = 90*pi ≈ 282.7".into(),
        ],
        ..bad_clone(&bad)
    };
    let stored = store_item(&pool, &good).await.unwrap();
    assert_eq!(stored.status, "verified", "detail: {}", stored.verify_detail);
    let _ = std::fs::remove_file(&db);
}

fn bad_clone(bad: &NewItem) -> NewItem {
    NewItem {
        skill_id: bad.skill_id.clone(),
        prompt: String::new(),
        answer: "282.7 cm2".into(),
        worked: Vec::new(),
        origin: Origin::Gold,
        source: None,
    }
}
