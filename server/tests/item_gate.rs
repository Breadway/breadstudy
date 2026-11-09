//! STUDY-2 acceptance: wrong worked answers cannot enter verified
//! state, generated items require searxng sources, unverifiable kinds
//! default to review_required with a manual approval path.

use breadstudy_server::verify::{
    GateDecision, NewItem, Origin, SourceRecord, approve_item, evaluate, get_item,
    store_item,
};

fn temp_db(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "breadstudy-gate-test-{}-{}.sqlite",
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
    pool
}

fn cylinder_item(answer: &str) -> NewItem {
    NewItem {
        skill_id: "volume-prism-cylinder".into(),
        prompt: "Find the volume of a cylinder with radius 4 cm and height 10 cm, to 1 decimal place.".into(),
        answer: answer.into(),
        worked: vec![
            "V = pi*r^2*h = pi*(4^2)*(10) = 160*pi ≈ 502.7".into(),
        ],
        origin: Origin::Gold,
        source: None,
    }
}

#[tokio::test]
async fn correct_worked_answer_verifies() {
    let db = temp_db("correct");
    let pool = test_pool(&db).await;
    let stored = store_item(&pool, &cylinder_item("502.7 cm3")).await.unwrap();
    assert_eq!(stored.status, "verified");
    assert_eq!(stored.verify_method, "sympy-chain");
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn wrong_worked_answer_cannot_verify() {
    let db = temp_db("wrong");
    let pool = test_pool(&db).await;
    let stored = store_item(&pool, &cylinder_item("500.0 cm3")).await.unwrap();
    assert_eq!(stored.status, "review_required");
    assert!(stored.verify_method.starts_with("gate:"));
    assert!(stored.verify_detail.contains("500"));
    // And it stays there: the stored row is not verified.
    let again = get_item(&pool, &stored.id).await.unwrap();
    assert_eq!(again.status, "review_required");
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn surd_chain_with_unicode_verifies() {
    let db = temp_db("surd");
    let pool = test_pool(&db).await;
    let item = NewItem {
        skill_id: "pythagoras-3d".into(),
        prompt: "Find the space diagonal of a box 3 cm by 4 cm by 5 cm, in exact surd form.".into(),
        answer: "5√2 cm".into(),
        worked: vec!["d = √(9 + 16 + 25) = √50 = 5√2".into()],
        origin: Origin::Gold,
        source: None,
    };
    let stored = store_item(&pool, &item).await.unwrap();
    assert_eq!(stored.status, "verified", "detail: {}", stored.verify_detail);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn proof_defaults_to_review_and_approves_once() {
    let db = temp_db("proof");
    let pool = test_pool(&db).await;
    let item = NewItem {
        skill_id: "surface-area".into(),
        prompt: "Prove that the volume of a cone is one third of the cylinder with the same base and height.".into(),
        answer: "By Cavalieri's principle applied to cross sections.".into(),
        worked: vec![],
        origin: Origin::Gold,
        source: None,
    };
    match evaluate(&pool, &item).await.unwrap() {
        GateDecision::ReviewRequired { reason, .. } => {
            assert_eq!(reason, "unverifiable-kind");
        }
        other => panic!("proof must need review, got {:?}", other.status()),
    }
    let stored = store_item(&pool, &item).await.unwrap();
    assert_eq!(stored.status, "review_required");
    let approved = approve_item(&pool, &stored.id, "gold-practice-import").await.unwrap();
    assert_eq!(approved.status, "verified");
    assert_eq!(approved.verify_method, "manual-approval");
    assert!(approve_item(&pool, &stored.id, "second").await.is_err());
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn generated_item_requires_searxng_source() {
    let db = temp_db("source");
    let pool = test_pool(&db).await;
    let mut item = cylinder_item("502.7 cm3");
    item.prompt = "Generated: volume of a cylinder r=4 h=10.".into();
    item.origin = Origin::Generated;
    item.source = None;
    match evaluate(&pool, &item).await.unwrap() {
        GateDecision::ReviewRequired { reason, .. } => assert_eq!(reason, "missing-source"),
        other => panic!("generated item without source must need review, got {:?}", other.status()),
    }
    item.source = Some(SourceRecord {
        url: "https://example.invalid/surface-area".into(),
        snippet: "cylinder volume V = pi r^2 h".into(),
        fetched_at: "2026-11-09".into(),
    });
    let stored = store_item(&pool, &item).await.unwrap();
    assert_eq!(stored.status, "verified");
    assert!(stored.source_url.unwrap_or_default().contains("example.invalid"));
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn duplicate_prompt_is_not_stored_twice() {
    let db = temp_db("dedupe");
    let pool = test_pool(&db).await;
    let first = store_item(&pool, &cylinder_item("502.7 cm3")).await.unwrap();
    let second = store_item(&pool, &cylinder_item("502.7 cm3")).await.unwrap();
    assert_eq!(first.id, second.id);
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM items")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let _ = std::fs::remove_file(&db);
}
