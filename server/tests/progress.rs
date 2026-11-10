//! STUDY-42 acceptance: the progress surface reads real attempt history,
//! per-skill mastery with due dates, and recurring error patterns, all from
//! the server tables rather than client storage.

use breadstudy_server::progress::{progress_snapshot, recent_attempts, MAX_ATTEMPT_LIMIT};

fn temp_db(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "breadstudy-progress-test-{}-{}.sqlite",
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
    for (id, name) in [
        ("volume-prism-cylinder", "Volume of prisms and cylinders"),
        ("trigonometry", "Trigonometry"),
    ] {
        sqlx::query("INSERT INTO skills(id, strand, name, vault_note) VALUES(?,?,?,?)")
            .bind(id)
            .bind("Geometry")
            .bind(name)
            .bind("Area Volume and Surface Area")
            .execute(pool)
            .await
            .unwrap();
    }
    for (item_id, skill_id, prompt) in [
        (
            "vol-1",
            "volume-prism-cylinder",
            "Find the volume of the cylinder.",
        ),
        (
            "vol-2",
            "volume-prism-cylinder",
            "Find the volume of the prism.",
        ),
        ("trig-1", "trigonometry", "Find the missing side."),
    ] {
        sqlx::query(
            "INSERT INTO items(id, skill_id, prompt, answer, rubric, hints, status) \
             VALUES(?,?,?,'42','[]','[]','verified')",
        )
        .bind(item_id)
        .bind(skill_id)
        .bind(prompt)
        .execute(pool)
        .await
        .unwrap();
    }
    // Newest first: an incorrect attempt with tags, then a partial one, then
    // a fully correct one outside the default window for the limit check.
    sqlx::query(
        "INSERT INTO attempts(id, item_id, input_kind, given, correct, error_tags, \
         method_marks, method_marks_total, final_marks, final_marks_total, hints_used, created_at) \
         VALUES('a3','vol-1','latex','wrong',0,'[\"sign-slip\",\"algebra\"]',1.0,3.0,0.0,1.0,2,'2026-11-10T09:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO attempts(id, item_id, input_kind, given, correct, error_tags, \
         method_marks, method_marks_total, final_marks, final_marks_total, hints_used, created_at) \
         VALUES('a2','trig-1','photo','42',1,'[]',1.0,3.0,1.0,1.0,0,'2026-11-10T08:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO attempts(id, item_id, input_kind, given, correct, error_tags, \
         method_marks, method_marks_total, final_marks, final_marks_total, hints_used, created_at) \
         VALUES('a1','vol-2','latex','42',1,'[]',3.0,3.0,1.0,1.0,0,'2026-11-09T08:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    // Trigonometry is weaker, so it should sort above volume despite the id
    // order.
    for (skill_id, mastery, due_at) in [
        ("volume-prism-cylinder", 0.8_f64, "2026-11-20T09:00:00Z"),
        ("trigonometry", 0.2_f64, "2026-11-10T09:00:00Z"),
    ] {
        sqlx::query(
            "INSERT INTO skill_schedule(skill_id, mastery, accuracy, reviews, lapses, last_review, due_at) \
             VALUES(?,?,0.5,2,1,'2026-11-09T09:00:00Z',?)",
        )
        .bind(skill_id)
        .bind(mastery)
        .bind(due_at)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO error_patterns(skill_id, tag, count, first_seen, last_seen) \
         VALUES('volume-prism-cylinder','sign-slip',4,'2026-11-01T09:00:00Z','2026-11-10T09:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO error_patterns(skill_id, tag, count, first_seen, last_seen) \
         VALUES('trigonometry','algebra',2,'2026-11-02T09:00:00Z','2026-11-09T09:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn progress_reads_attempts_mastery_and_error_patterns() {
    let db = temp_db("snapshot");
    let pool = test_pool(&db).await;
    seed(&pool).await;

    let snapshot = progress_snapshot(&pool, 25).await.unwrap();
    assert_eq!(snapshot.attempts_total, 3);
    assert_eq!(snapshot.recent_attempts.len(), 3);

    let newest = &snapshot.recent_attempts[0];
    assert_eq!(newest.attempt_id, "a3");
    assert_eq!(newest.prompt, "Find the volume of the cylinder.");
    assert_eq!(newest.skill_name, "Volume of prisms and cylinders");
    assert_eq!(newest.verdict, "incorrect");
    assert!(!newest.correct);
    assert_eq!(newest.error_tags, vec!["sign-slip", "algebra"]);
    assert_eq!(newest.method_marks, 1.0);
    assert_eq!(newest.method_marks_total, 3.0);
    assert_eq!(newest.final_marks, 0.0);
    assert_eq!(newest.final_marks_total, 1.0);
    assert_eq!(newest.hints_used, 2);
    assert_eq!(newest.created_at, "2026-11-10T09:00:00Z");

    // Right final answer with thin working is a partial verdict.
    let partial = snapshot
        .recent_attempts
        .iter()
        .find(|attempt| attempt.attempt_id == "a2")
        .expect("partial attempt present");
    assert_eq!(partial.verdict, "partial");

    let correct = snapshot
        .recent_attempts
        .iter()
        .find(|attempt| attempt.attempt_id == "a1")
        .expect("correct attempt present");
    assert_eq!(correct.verdict, "correct");
    assert!(correct.error_tags.is_empty());

    // Weakest skill first, with its scheduled due date.
    assert_eq!(snapshot.skills[0].skill_id, "trigonometry");
    assert_eq!(snapshot.skills[0].mastery, 0.2);
    assert_eq!(
        snapshot.skills[0].due_at.as_deref(),
        Some("2026-11-10T09:00:00Z")
    );
    assert_eq!(snapshot.skills[0].reviews, 2);
    assert_eq!(snapshot.skills[1].mastery, 0.8);

    // Top recurring pattern carries the skill name for the UI.
    let top = &snapshot.error_patterns[0];
    assert_eq!(top.tag, "sign-slip");
    assert_eq!(top.count, 4);
    assert_eq!(top.skill_name, "Volume of prisms and cylinders");
    assert_eq!(snapshot.error_patterns[1].tag, "algebra");
}

#[tokio::test]
async fn recent_attempts_honours_and_clamps_the_limit() {
    let db = temp_db("limit");
    let pool = test_pool(&db).await;
    seed(&pool).await;

    let newest = recent_attempts(&pool, 2).await.unwrap();
    assert_eq!(newest.len(), 2);
    assert_eq!(newest[0].attempt_id, "a3");
    assert_eq!(newest[1].attempt_id, "a2");

    // A zero or negative limit still returns a usable page rather than an
    // empty list, because the page always shows the latest attempt.
    let floored = recent_attempts(&pool, 0).await.unwrap();
    assert_eq!(floored.len(), 1);

    let capped = progress_snapshot(&pool, MAX_ATTEMPT_LIMIT + 500)
        .await
        .unwrap();
    assert_eq!(capped.attempt_limit, MAX_ATTEMPT_LIMIT);
    assert_eq!(capped.recent_attempts.len(), 3);
}
