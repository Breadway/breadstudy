//! STUDY-45 acceptance: a skill opens its vault note name plus a summary
//! and worked-example notes from the item bank, and a session can be scoped
//! to that skill without changing the existing session contract.

use breadstudy_server::session::{self, StartOptions, Template};
use breadstudy_server::skills::{skill_detail, SkillViewError};

fn temp_db(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "breadstudy-skill-view-test-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path.to_string_lossy().to_string()
}

async fn test_pool(db: &str) -> sqlx::SqlitePool {
    breadstudy_server::db::connect(db).await.unwrap()
}

async fn seed(pool: &sqlx::SqlitePool) {
    for (id, strand, name, vault) in [
        ("trigonometry", "Geometry", "Trigonometry", "Trigonometry"),
        (
            "volume-prism-cylinder",
            "Geometry",
            "Volume of prisms and cylinders",
            "Area Volume and Surface Area",
        ),
    ] {
        sqlx::query("INSERT INTO skills(id, strand, name, vault_note) VALUES(?,?,?,?)")
            .bind(id)
            .bind(strand)
            .bind(name)
            .bind(vault)
            .execute(pool)
            .await
            .unwrap();
    }
    for (item_id, skill_id, prompt, snippet) in [
        (
            "trig-1",
            "trigonometry",
            "Find the missing side.",
            "Worked example: label the opposite and adjacent, then use tan.",
        ),
        (
            "trig-2",
            "trigonometry",
            "Find the angle to the nearest degree.",
            "Remember SOH CAH TOA and check the mode is degrees.",
        ),
        (
            "trig-3",
            "trigonometry",
            "Find the hypotenuse.",
            "Worked example: label the opposite and adjacent, then use tan.",
        ),
        (
            "vol-1",
            "volume-prism-cylinder",
            "Find the volume of the cylinder.",
            "V = pi r squared h",
        ),
    ] {
        sqlx::query(
            "INSERT INTO items(id, skill_id, prompt, answer, rubric, hints, source_url, source_snippet, status) \
             VALUES(?,?,?,'42','[]','[]','file:///vault/Maths/Trigonometry.md',?,'verified')",
        )
        .bind(item_id)
        .bind(skill_id)
        .bind(prompt)
        .bind(snippet)
        .execute(pool)
        .await
        .unwrap();
    }
    // A draft item must not count towards the preview.
    sqlx::query(
        "INSERT INTO items(id, skill_id, prompt, answer, rubric, hints, status) \
         VALUES('trig-draft','trigonometry','Draft question','1','[]','[]','draft')",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn skill_view_returns_vault_note_and_item_bank_summary() {
    let db = temp_db("detail");
    let pool = test_pool(&db).await;
    seed(&pool).await;

    let detail = skill_detail(&pool, "trigonometry").await.unwrap();
    assert_eq!(detail.name, "Trigonometry");
    assert_eq!(detail.strand, "Geometry");
    assert_eq!(detail.vault_note, "Trigonometry");
    assert_eq!(detail.vault_note_path, "Maths/Trigonometry.md");
    assert_eq!(detail.item_count, 3);
    assert!(detail.summary.contains("Trigonometry"));
    assert!(detail.summary.contains("3 verified practice items"));
    assert!(detail.summary.contains("vault note Trigonometry"));

    // Source excerpts become worked-example notes, deduplicated and without
    // in-bank answers or hints.
    assert_eq!(detail.note_excerpts.len(), 2);
    assert!(detail
        .note_excerpts
        .iter()
        .any(|note| note.contains("SOH CAH TOA")));
    assert_eq!(detail.sample_items.len(), 3);
    assert_eq!(detail.sample_items[0].item_id, "trig-1");
    assert_eq!(detail.sample_items[0].prompt, "Find the missing side.");
    assert_eq!(
        detail.sample_items[0].source_url.as_deref(),
        Some("file:///vault/Maths/Trigonometry.md")
    );
}

#[tokio::test]
async fn unknown_skill_is_a_not_found_error() {
    let db = temp_db("missing");
    let pool = test_pool(&db).await;
    seed(&pool).await;

    match skill_detail(&pool, "not-a-skill").await {
        Err(SkillViewError::NotFound(id)) => assert_eq!(id, "not-a-skill"),
        other => panic!("expected not found, got {other:?}"),
    }
}

async fn cached_skill_ids(pool: &sqlx::SqlitePool, session_id: &str) -> Vec<String> {
    let content: String = sqlx::query_scalar("SELECT content_json FROM session_runs WHERE id = ?")
        .bind(session_id)
        .fetch_one(pool)
        .await
        .unwrap();
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    parsed
        .into_iter()
        .map(|item| item["skill_id"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn scoped_session_only_caches_the_requested_skill() {
    let db = temp_db("scoped");
    let pool = test_pool(&db).await;
    seed(&pool).await;

    let started = session::start(
        &pool,
        StartOptions::new(Template::SchoolDay20).with_skill("trigonometry"),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let skills = cached_skill_ids(&pool, &started.id).await;
    assert!(!skills.is_empty(), "scoped session cached no items");
    assert!(
        skills.iter().all(|skill| skill == "trigonometry"),
        "scoped session leaked other skills: {skills:?}"
    );
    assert_eq!(
        started.item.as_ref().map(|item| item.skill_id.as_str()),
        Some("trigonometry")
    );
}

#[tokio::test]
async fn unscoped_session_still_uses_the_whole_bank() {
    let db = temp_db("unscoped");
    let pool = test_pool(&db).await;
    seed(&pool).await;

    let started = session::start(
        &pool,
        StartOptions::new(Template::SchoolDay20),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let skills = cached_skill_ids(&pool, &started.id).await;
    assert!(skills.iter().any(|skill| skill == "trigonometry"));
    assert!(skills.iter().any(|skill| skill == "volume-prism-cylinder"));
}

#[tokio::test]
async fn scoping_to_an_empty_skill_explains_the_problem() {
    let db = temp_db("empty-skill");
    let pool = test_pool(&db).await;
    sqlx::query("INSERT INTO skills(id, strand, name, vault_note) VALUES('empty','Geometry','Empty','Empty')")
        .execute(&pool)
        .await
        .unwrap();

    let error = session::start(
        &pool,
        StartOptions::new(Template::SchoolDay20).with_skill("empty"),
        chrono::Utc::now(),
    )
    .await
    .expect_err("a skill with no verified items must not start silently");
    assert!(error.to_string().contains("no verified items"));
}
