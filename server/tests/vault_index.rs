//! STUDY-1 acceptance: the indexer runs on the real vault checkout,
//! all Test 6 skills resolve, and a second run produces no dupes/changes.

use std::path::PathBuf;

fn vault_root() -> PathBuf {
    PathBuf::from(
        std::env::var("BREADSTUDY_VAULT")
            .unwrap_or_else(|_| "/home/breadway/Projects/school-notes-year-10/Maths".to_string()),
    )
}

fn temp_db(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "breadstudy-index-test-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path.to_string_lossy().to_string()
}

#[tokio::test]
async fn test_map_covers_tests_1_to_7_on_real_overview() {
    let text = std::fs::read_to_string(vault_root().join("Methods Pathway Overview.md"))
        .expect("real vault checkout must be present");
    let entries = breadstudy_server::indexer::parse_test_map(&text);
    for n in 1..=7 {
        assert!(
            entries.iter().any(|e| e.id == format!("test-{n}")),
            "test-{n} missing from parsed test map"
        );
    }
    let t6 = entries.iter().find(|e| e.id == "test-6").unwrap();
    assert!(t6.topic.contains("Volume"));
    assert!(t6.notes.iter().any(|n| n == "Area Volume and Surface Area"));
}

#[tokio::test]
async fn real_vault_index_is_idempotent_and_resolves_test6() {
    let vault = vault_root();
    assert!(vault.is_dir(), "vault checkout missing at {}", vault.display());
    let db = temp_db("idempotent");
    let pool = breadstudy_server::db::connect(&db)
        .await
        .expect("connect temp db");
    let first = breadstudy_server::indexer::index_vault(&pool, &vault)
        .await
        .expect("first index run");
    assert!(first.missing.is_empty(), "missing vault notes: {:?}", first.missing);
    assert_eq!(first.skills, 6);
    assert!(!first.test6_skills.is_empty(), "no skills linked to Test 6");
    assert!(
        first.test6_skills.iter().all(|s| first.resolved.contains(s)),
        "not all Test 6 skills resolve: {:?}",
        first.test6_skills
    );

    let count: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM skills), (SELECT COUNT(*) FROM tests), (SELECT COUNT(*) FROM skill_tests)",
    )
    .fetch_one(&pool)
    .await
    .expect("count rows");
    let second = breadstudy_server::indexer::index_vault(&pool, &vault)
        .await
        .expect("second index run");
    let count2: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM skills), (SELECT COUNT(*) FROM tests), (SELECT COUNT(*) FROM skill_tests)",
    )
    .fetch_one(&pool)
    .await
    .expect("count rows again");
    assert_eq!(count, count2, "second run changed row counts");
    assert_eq!(
        first.content_hash, second.content_hash,
        "second run changed stored content"
    );
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn geometry_practice_set_parses_expected_shape() {
    let text = std::fs::read_to_string(vault_root().join("Geometry").join("Geometry Practice.md"))
        .expect("geometry practice must exist");
    let set = breadstudy_server::indexer::parse_practice_set("Geometry Practice", &text);
    assert_eq!(set.questions.len(), 17, "expected 17 practice questions");
    assert_eq!(set.answers.len(), 17, "expected 17 worked answers");
    for n in 1..=17 {
        assert!(set.questions.iter().any(|q| q.number == n as u32));
        assert!(set.answers.iter().any(|a| a.number == n as u32));
    }
}
