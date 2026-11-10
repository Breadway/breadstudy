//! STUDY-18 acceptance: FSRS mastery moves with attempts, the hint ladder
//! escalates and is logged, hint use feeds back into mastery, error tags
//! aggregate per skill, and the weekly review can read the logs.

use breadstudy_server::fsrs::{
    due_score, due_skills, error_patterns_for_skill, log_session_event, next_hint,
    next_hint_for_session, record_attempt, weekly_snapshot, AttemptInput, UNSEEN_DUE_SCORE,
};

fn temp_db(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "breadstudy-fsrs-test-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path.to_string_lossy().to_string()
}

async fn test_pool(db: &str) -> sqlx::SqlitePool {
    breadstudy_server::db::connect(db).await.unwrap()
}

async fn seed_skill(pool: &sqlx::SqlitePool, id: &str, name: &str) {
    sqlx::query("INSERT OR IGNORE INTO skills(id, strand, name, vault_note) VALUES(?,?,?,?)")
        .bind(id)
        .bind("Geometry")
        .bind(name)
        .bind("Area Volume and Surface Area")
        .execute(pool)
        .await
        .unwrap();
}

async fn seed_item(
    pool: &sqlx::SqlitePool,
    item_id: &str,
    skill_id: &str,
    answer: &str,
    worked: &[&str],
) {
    let rubric = serde_json::to_string(worked).unwrap();
    sqlx::query(
        "INSERT INTO items(id, skill_id, prompt, answer, rubric, hints, status)
         VALUES(?,?,?,?,?,'[]','verified')",
    )
    .bind(item_id)
    .bind(skill_id)
    .bind(format!("Find the value for {item_id}."))
    .bind(answer)
    .bind(rubric)
    .execute(pool)
    .await
    .unwrap();
}

fn attempt(item_id: &str, correct: bool, tags: &[&str]) -> AttemptInput {
    AttemptInput {
        item_id: item_id.to_string(),
        input_kind: "typed".into(),
        given: if correct { "502.7 cm3" } else { "500 cm3" }.into(),
        correct,
        session_id: Some("sess-test".into()),
        error_tags: tags.iter().map(|tag| tag.to_string()).collect(),
        attempt_id: None,
    }
}

#[tokio::test]
async fn two_failures_lower_mastery_and_change_the_next_plan() {
    let db = temp_db("plan");
    let pool = test_pool(&db).await;
    seed_skill(&pool, "surface-area", "Surface area of solids").await;
    seed_skill(
        &pool,
        "volume-prism-cylinder",
        "Volume of prisms and cylinders",
    )
    .await;
    seed_item(
        &pool,
        "sa-1",
        "surface-area",
        "502.7 cm2",
        &["A = 2*pi*r*h + 2*pi*r^2", "= 160*pi ≈ 502.7"],
    )
    .await;
    seed_item(
        &pool,
        "vol-1",
        "volume-prism-cylinder",
        "502.7 cm3",
        &["V = pi*r^2*h = pi*(4^2)*(10)", "= 160*pi ≈ 502.7"],
    )
    .await;

    // Both skills are unseen, so the tie breaks by id: surface-area first.
    let before = due_skills(&pool).await.unwrap();
    assert_eq!(before.len(), 2);
    assert_eq!(before[0].skill_id, "surface-area");
    assert!((before[0].due_score - UNSEEN_DUE_SCORE).abs() < 1e-9);

    let good = record_attempt(&pool, &attempt("sa-1", true, &[]))
        .await
        .unwrap();
    assert!(good.mastery_after > 0.4, "a clean success lifts mastery");

    let first = record_attempt(&pool, &attempt("vol-1", false, &["sign-error"]))
        .await
        .unwrap();
    let second = record_attempt(
        &pool,
        &attempt("vol-1", false, &["sign-error", "unit-error"]),
    )
    .await
    .unwrap();
    assert!(first.mastery_after < 0.4);
    assert!(
        second.mastery_after < first.mastery_after,
        "second failure must lower mastery again: {} vs {}",
        second.mastery_after,
        first.mastery_after
    );

    let after = due_skills(&pool).await.unwrap();
    assert_eq!(
        after[0].skill_id, "volume-prism-cylinder",
        "the failing skill is planned first"
    );
    assert!(after[0].due_score > after[1].due_score);
    assert!(
        due_score(&pool, "volume-prism-cylinder").await.unwrap()
            > due_score(&pool, "surface-area").await.unwrap()
    );
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn hint_ladder_has_three_escalating_levels() {
    let db = temp_db("ladder");
    let pool = test_pool(&db).await;
    seed_skill(
        &pool,
        "volume-prism-cylinder",
        "Volume of prisms and cylinders",
    )
    .await;
    seed_item(
        &pool,
        "vol-1",
        "volume-prism-cylinder",
        "502.7 cm3",
        &["V = pi*r^2*h = pi*(4^2)*(10)", "= 160*pi ≈ 502.7"],
    )
    .await;

    let first = next_hint(&pool, "vol-1")
        .await
        .unwrap()
        .expect("first rung");
    let second = next_hint(&pool, "vol-1")
        .await
        .unwrap()
        .expect("second rung");
    let third = next_hint(&pool, "vol-1")
        .await
        .unwrap()
        .expect("third rung");
    assert_eq!((first.level, first.total), (1, 3));
    assert_eq!(second.level, 2);
    assert_eq!(third.level, 3);
    assert!(!first.final_level && !second.final_level && third.final_level);
    assert!(second.text.len() > first.text.len());
    assert!(third.text.len() > second.text.len());
    assert!(
        !first.text.contains("502.7"),
        "rung 1 must not reveal the answer"
    );
    assert!(
        !second.text.contains("502.7"),
        "rung 2 must not reveal the answer"
    );
    assert!(
        third.text.contains("502.7"),
        "the last rung may show the final value"
    );
    assert!(next_hint(&pool, "vol-1").await.unwrap().is_none());

    let stored: String = sqlx::query_scalar("SELECT hints FROM items WHERE id = 'vol-1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let levels: Vec<String> = serde_json::from_str(&stored).unwrap();
    assert_eq!(levels.len(), 3);
    let uses: i64 = sqlx::query_scalar("SELECT uses FROM hint_state WHERE item_id = 'vol-1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(uses, 3);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn hint_use_is_logged_and_lowers_the_mastery_gain() {
    let db = temp_db("hint-mastery");
    let pool = test_pool(&db).await;
    seed_skill(&pool, "surface-area", "Surface area of solids").await;
    seed_skill(
        &pool,
        "volume-prism-cylinder",
        "Volume of prisms and cylinders",
    )
    .await;
    seed_item(
        &pool,
        "sa-1",
        "surface-area",
        "502.7 cm2",
        &["A = 2*pi*r*h + 2*pi*r^2", "= 160*pi ≈ 502.7"],
    )
    .await;
    seed_item(
        &pool,
        "vol-1",
        "volume-prism-cylinder",
        "502.7 cm3",
        &["V = pi*r^2*h = pi*(4^2)*(10)", "= 160*pi ≈ 502.7"],
    )
    .await;

    next_hint_for_session(&pool, "vol-1", Some("sess-test"))
        .await
        .unwrap();
    let revealed = next_hint_for_session(&pool, "vol-1", Some("sess-test"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(revealed.level, 2);

    let hinted = record_attempt(&pool, &attempt("vol-1", true, &[]))
        .await
        .unwrap();
    let plain = record_attempt(&pool, &attempt("sa-1", true, &[]))
        .await
        .unwrap();
    assert_eq!(hinted.hints_used, 2);
    assert_eq!(hinted.rating, 2, "two hints downgrade Easy to Hard");
    assert_eq!(plain.rating, 4);
    assert!(
        hinted.mastery_after < plain.mastery_after,
        "hint-assisted success must lift mastery less"
    );
    assert!(hinted.stability < plain.stability);

    let hint_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM session_log WHERE kind = 'hint' AND item_id = 'vol-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(hint_events, 2);
    let assisted: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM intervention_log WHERE kind = 'hint-assisted-attempt'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(assisted, 1);
    let stored_hints: i64 =
        sqlx::query_scalar("SELECT hints_used FROM attempts WHERE item_id = 'vol-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_hints, 2);
    // The ladder resets so the next rep starts from rung one.
    let level: i64 = sqlx::query_scalar(
        "SELECT level FROM hint_state WHERE session_id = 'sess-test' AND item_id = 'vol-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(level, 0);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn error_patterns_aggregate_by_tag_per_skill() {
    let db = temp_db("errors");
    let pool = test_pool(&db).await;
    seed_skill(
        &pool,
        "volume-prism-cylinder",
        "Volume of prisms and cylinders",
    )
    .await;
    seed_item(
        &pool,
        "vol-1",
        "volume-prism-cylinder",
        "502.7 cm3",
        &["V = pi*r^2*h = pi*(4^2)*(10)", "= 160*pi ≈ 502.7"],
    )
    .await;

    let first = record_attempt(&pool, &attempt("vol-1", false, &["sign-error"]))
        .await
        .unwrap();
    let second = record_attempt(
        &pool,
        &attempt("vol-1", false, &["sign-error", "unit-error"]),
    )
    .await
    .unwrap();
    let third = record_attempt(&pool, &attempt("vol-1", false, &["sign-error"]))
        .await
        .unwrap();
    assert!(first.repeated_errors.is_empty());
    assert_eq!(second.repeated_errors, vec!["sign-error".to_string()]);
    assert_eq!(third.repeated_errors, vec!["sign-error".to_string()]);

    let patterns = error_patterns_for_skill(&pool, "volume-prism-cylinder")
        .await
        .unwrap();
    let sign = patterns.iter().find(|p| p.tag == "sign-error").unwrap();
    let unit = patterns.iter().find(|p| p.tag == "unit-error").unwrap();
    assert_eq!(sign.count, 3);
    assert_eq!(unit.count, 1);

    let snapshot = weekly_snapshot(&pool, None).await.unwrap();
    let weekly_sign = snapshot
        .errors
        .iter()
        .find(|p| p.skill_id == "volume-prism-cylinder" && p.tag == "sign-error")
        .unwrap();
    assert_eq!(weekly_sign.count, 3);
    let repeats: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM intervention_log WHERE kind = 'repeat-error'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        repeats, 2,
        "the second and third sign-error are repeat interventions"
    );
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn weekly_review_reads_session_and_intervention_logs() {
    let db = temp_db("weekly");
    let pool = test_pool(&db).await;
    seed_skill(
        &pool,
        "volume-prism-cylinder",
        "Volume of prisms and cylinders",
    )
    .await;
    seed_item(
        &pool,
        "vol-1",
        "volume-prism-cylinder",
        "502.7 cm3",
        &["V = pi*r^2*h = pi*(4^2)*(10)", "= 160*pi ≈ 502.7"],
    )
    .await;

    log_session_event(
        &pool,
        Some("sess-test"),
        "session-start",
        None,
        None,
        serde_json::json!({"template": "focus-25"}),
    )
    .await
    .unwrap();
    next_hint_for_session(&pool, "vol-1", Some("sess-test"))
        .await
        .unwrap();
    let outcome = record_attempt(&pool, &attempt("vol-1", false, &["arithmetic-slip"]))
        .await
        .unwrap();
    assert!(outcome.mastery_after < 0.4);

    let since = "2000-01-01T00:00:00.000Z";
    let snapshot = weekly_snapshot(&pool, Some(since)).await.unwrap();
    assert_eq!(snapshot.since.as_deref(), Some(since));

    let kinds: Vec<&str> = snapshot
        .sessions
        .iter()
        .map(|event| event.kind.as_str())
        .collect();
    assert!(kinds.contains(&"session-start"));
    assert!(kinds.contains(&"hint"));
    assert!(kinds.contains(&"attempt"));
    let attempt_event = snapshot
        .sessions
        .iter()
        .find(|event| event.kind == "attempt")
        .unwrap();
    assert_eq!(attempt_event.session_id.as_deref(), Some("sess-test"));
    assert_eq!(
        attempt_event.skill_id.as_deref(),
        Some("volume-prism-cylinder")
    );
    assert_eq!(attempt_event.detail["correct"], false);

    assert!(snapshot
        .interventions
        .iter()
        .any(|event| event.kind == "hint" && event.hint_level == Some(1)));
    assert!(snapshot
        .interventions
        .iter()
        .any(|event| event.kind == "hint-assisted-attempt"));

    let mastery = snapshot
        .mastery
        .iter()
        .find(|row| row.skill_id == "volume-prism-cylinder")
        .unwrap();
    assert_eq!(mastery.reviews, 1);
    assert_eq!(mastery.lapses, 1);
    assert!(mastery.mastery < 0.4);
    assert!(mastery.stability > 0.0);
    assert!(mastery.due_at.is_some());
    assert!(snapshot
        .errors
        .iter()
        .any(|pattern| pattern.tag == "arithmetic-slip"));

    // A future window is empty, so the review can scope to one week.
    let empty = weekly_snapshot(&pool, Some("2999-01-01T00:00:00.000Z"))
        .await
        .unwrap();
    assert!(empty.sessions.is_empty());
    assert!(empty.interventions.is_empty());
    let _ = std::fs::remove_file(&db);
}
