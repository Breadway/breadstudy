//! STUDY-16 acceptance: server-driven session templates, timer expiry,
//! off-task pause, cached content, skip handling and the readable event
//! log.

use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use sqlx::SqlitePool;

use breadstudy_server::fsrs::{self, AttemptInput};
use breadstudy_server::session::{self, Phase, SessionEngine, StartOptions, Template};

fn t0() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 9, 8, 0, 0).unwrap()
}

fn at(seconds: i64) -> chrono::DateTime<Utc> {
    t0() + ChronoDuration::seconds(seconds)
}

async fn test_pool(name: &str) -> (SqlitePool, String) {
    let path = std::env::temp_dir().join(format!(
        "breadstudy-session-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let db = path.to_string_lossy().to_string();
    let pool = breadstudy_server::db::connect(&db).await.unwrap();
    (pool, db)
}

async fn seed_bank(pool: &SqlitePool) {
    let skills = [
        ("volume-prism-cylinder", "Geometry"),
        ("surface-area", "Geometry"),
        ("unit-conversion-scaling", "Number"),
    ];
    for (id, strand) in skills {
        sqlx::query("INSERT OR IGNORE INTO skills(id, strand, name, vault_note) VALUES(?,?,?,?)")
            .bind(id)
            .bind(strand)
            .bind(id)
            .bind("Area Volume and Surface Area")
            .execute(pool)
            .await
            .unwrap();
    }
    for index in 0..12 {
        let (skill, _) = skills[index % skills.len()];
        let id = format!("item-{index:02}");
        let prompt = format!("Prompt {id}: find the value.");
        // Three escalating rungs so the authored ladder from STUDY-18
        // validates and hint_state can own the cursor.
        let first = format!("first hint for {id}");
        let second = format!("{first}. Name the rule before substituting.");
        let third = format!("{second} Carry the working to the final line and check units.");
        let hints = serde_json::json!([first, second, third]).to_string();
        sqlx::query(
            "INSERT INTO items(id, skill_id, prompt, answer, hints, status) \
             VALUES(?,?,?,?,?,'verified')",
        )
        .bind(&id)
        .bind(skill)
        .bind(&prompt)
        .bind("42")
        .bind(&hints)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO item_audio(item_id, path, voice) VALUES('item-00', 'cache/audio/item-00.wav', 'patient-coach')",
    )
    .execute(pool)
    .await
    .unwrap();
}

fn cleanup(db: &str) {
    let _ = std::fs::remove_file(db);
}

#[tokio::test]
async fn school_day_runs_end_to_end_from_cache() {
    let (pool, db) = test_pool("school-day").await;
    seed_bank(&pool).await;

    let started = session::start(&pool, StartOptions::new(Template::SchoolDay20), t0())
        .await
        .unwrap();
    assert_eq!(started.template, "school_day_20");
    assert_eq!(started.phase, Phase::Recall);
    assert_eq!(started.phase_remaining_seconds, 2 * 60);
    assert_eq!(started.total_remaining_seconds, 20 * 60);

    let item = started.item.as_ref().expect("cached recall item");
    assert!(item.prompt.starts_with("Prompt item-"));
    assert_eq!(item.audio_path.as_deref(), Some("cache/audio/item-00.wav"));
    assert_eq!(item.hint_count, 3);
    assert_eq!(
        item.hints.len(),
        3,
        "the full ladder ships even before anything is unlocked"
    );
    assert_eq!(item.hint_unlocked, 0);
    assert_eq!(started.hint_unlocked, 0);

    let practice = session::tick(&pool, &started.id, at(120))
        .await
        .unwrap()
        .expect("session");
    assert_eq!(practice.phase, Phase::Practice);
    assert_eq!(practice.phase_remaining_seconds, 10 * 60);

    let explain = session::tick(&pool, &started.id, at(720))
        .await
        .unwrap()
        .expect("session");
    assert_eq!(explain.phase, Phase::Explain);
    assert_eq!(explain.phase_remaining_seconds, 5 * 60);

    let reflect = session::tick(&pool, &started.id, at(1020))
        .await
        .unwrap()
        .expect("session");
    assert_eq!(reflect.phase, Phase::Reflect);
    assert_eq!(reflect.phase_remaining_seconds, 3 * 60);

    let ended = session::tick(&pool, &started.id, at(1200))
        .await
        .unwrap()
        .expect("session");
    assert_eq!(ended.phase, Phase::End);
    assert_eq!(ended.total_remaining_seconds, 0);
    assert_eq!(ended.outcome.as_deref(), Some("completed"));

    let events = session::events(&pool, &started.id).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "start"));
    assert_eq!(
        events.iter().filter(|event| event.kind == "phase").count(),
        4
    );
    assert!(events.iter().any(|event| event.kind == "end"));
    cleanup(&db);
}

#[tokio::test]
async fn off_task_pause_and_resume_preserves_remaining_time() {
    let (pool, db) = test_pool("off-task").await;
    seed_bank(&pool).await;

    let started = session::start(&pool, StartOptions::new(Template::SchoolDay20), t0())
        .await
        .unwrap();
    let after_30 = session::tick(&pool, &started.id, at(30))
        .await
        .unwrap()
        .expect("session");
    assert_eq!(after_30.phase_remaining_seconds, 90);

    let short = session::off_task(&pool, &started.id, 30, at(30))
        .await
        .unwrap()
        .expect("session");
    assert!(!short.paused);
    assert!(short.nudge.is_none());

    let paused = session::off_task(&pool, &started.id, 60, at(30))
        .await
        .unwrap()
        .expect("session");
    assert!(paused.paused);
    assert!(paused
        .nudge
        .as_deref()
        .unwrap_or_default()
        .contains("60 seconds"));
    assert_eq!(paused.session.phase_remaining_seconds, 90);

    let still_paused = session::tick(&pool, &started.id, at(200))
        .await
        .unwrap()
        .expect("session");
    assert!(still_paused.paused);
    assert_eq!(still_paused.phase_remaining_seconds, 90);
    assert_eq!(still_paused.paused_seconds, 170);

    let resumed = session::resume(&pool, &started.id, at(200))
        .await
        .unwrap()
        .expect("session");
    assert!(!resumed.paused);
    assert_eq!(resumed.paused_seconds, 170);

    let one_second = session::tick(&pool, &started.id, at(201))
        .await
        .unwrap()
        .expect("session");
    assert_eq!(one_second.phase_remaining_seconds, 89);

    let events = session::events(&pool, &started.id).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "off_task_pause"));
    assert!(events.iter().any(|event| event.kind == "resume"));
    cleanup(&db);
}

#[tokio::test]
async fn weekend_template_has_correct_durations_and_break_placement() {
    let (pool, db) = test_pool("weekend").await;
    seed_bank(&pool).await;

    let started = session::start(&pool, StartOptions::new(Template::Weekend40), t0())
        .await
        .unwrap();
    assert_eq!(started.phase, Phase::Prime);
    assert_eq!(started.phase_remaining_seconds, 3 * 60);
    assert_eq!(started.total_remaining_seconds, 40 * 60);

    let recall = session::tick(&pool, &started.id, at(180))
        .await
        .unwrap()
        .expect("session");
    assert_eq!(recall.phase, Phase::Recall);
    assert_eq!(recall.phase_remaining_seconds, 3 * 60);

    let practice = session::tick(&pool, &started.id, at(360))
        .await
        .unwrap()
        .expect("session");
    assert_eq!(practice.phase, Phase::Practice);
    assert_eq!(practice.phase_remaining_seconds, 12 * 60);
    let block_a_strand = practice.item.as_ref().expect("block A item").strand.clone();

    let pause = session::tick(&pool, &started.id, at(1080))
        .await
        .unwrap()
        .expect("session");
    assert_eq!(pause.phase, Phase::Break, "break must start at 18 minutes");
    assert_eq!(pause.phase_remaining_seconds, 4 * 60);

    let block_b = session::tick(&pool, &started.id, at(1320))
        .await
        .unwrap()
        .expect("session");
    assert_eq!(block_b.phase, Phase::Explain);
    assert_eq!(block_b.phase_remaining_seconds, 15 * 60);
    let block_b_strand = block_b.item.as_ref().expect("block B item").strand.clone();
    assert_ne!(
        block_a_strand, block_b_strand,
        "block B runs in a different strand"
    );

    let reflect = session::tick(&pool, &started.id, at(2220))
        .await
        .unwrap()
        .expect("session");
    assert_eq!(reflect.phase, Phase::Reflect);
    assert_eq!(reflect.phase_remaining_seconds, 3 * 60);

    let ended = session::tick(&pool, &started.id, at(2400))
        .await
        .unwrap()
        .expect("session");
    assert_eq!(ended.phase, Phase::End);
    assert_eq!(ended.outcome.as_deref(), Some("completed"));

    let mut elapsed = 0;
    for phase in &ended.plan {
        if phase.phase == Phase::Break {
            assert_eq!(elapsed, 18 * 60);
            assert_eq!(phase.planned_seconds, 4 * 60);
        }
        elapsed += phase.planned_seconds;
    }
    cleanup(&db);
}

#[tokio::test]
async fn skip_logs_reason_uses_grace_and_reports_weekly_count() {
    let (pool, db) = test_pool("skip").await;
    seed_bank(&pool).await;

    let started = session::start(&pool, StartOptions::new(Template::SchoolDay20), t0())
        .await
        .unwrap();
    let first = session::skip(&pool, &started.id, "felt unwell", t0())
        .await
        .unwrap()
        .expect("session");
    assert!(first.grace_used);
    assert_eq!(first.session.grace_tokens, 0);
    assert_eq!(first.weekly_skip_count, 1);
    assert_eq!(first.session.phase, Phase::Practice, "skip re-plans");

    let second = session::skip(&pool, &started.id, "needed a break", t0())
        .await
        .unwrap()
        .expect("session");
    assert!(!second.grace_used);
    assert_eq!(second.weekly_skip_count, 2);

    let events = session::events(&pool, &started.id).await.unwrap();
    let skips: Vec<_> = events.iter().filter(|event| event.kind == "skip").collect();
    assert_eq!(skips.len(), 2);
    assert_eq!(skips[0].detail["reason"], "felt unwell");
    assert_eq!(skips[1].detail["reason"], "needed a break");
    cleanup(&db);
}

#[tokio::test]
async fn hints_are_unlocked_from_cache_and_items_can_advance() {
    let (pool, db) = test_pool("hints").await;
    seed_bank(&pool).await;

    let started = session::start(&pool, StartOptions::new(Template::SchoolDay20), t0())
        .await
        .unwrap();
    let first = session::hint(&pool, &started.id, t0())
        .await
        .unwrap()
        .expect("session");
    assert_eq!(first.hints_used, 1);
    assert!(first
        .hint
        .as_deref()
        .unwrap_or_default()
        .contains("first hint"));
    assert_eq!(first.session.item.as_ref().unwrap().hints.len(), 3);
    assert_eq!(first.session.item.as_ref().unwrap().hint_unlocked, 1);
    assert_eq!(first.session.hint_unlocked, 1);

    let second = session::hint(&pool, &started.id, t0())
        .await
        .unwrap()
        .expect("session");
    assert_eq!(second.hints_used, 2);
    assert_eq!(second.session.item.as_ref().unwrap().hints.len(), 3);
    assert_eq!(second.session.item.as_ref().unwrap().hint_unlocked, 2);

    let third = session::hint(&pool, &started.id, t0())
        .await
        .unwrap()
        .expect("session");
    assert!(third.hint.is_some());
    assert_eq!(third.hints_used, 3);
    assert_eq!(third.session.item.as_ref().unwrap().hints.len(), 3);
    assert_eq!(third.session.item.as_ref().unwrap().hint_unlocked, 3);

    let fourth = session::hint(&pool, &started.id, t0())
        .await
        .unwrap()
        .expect("session");
    assert!(fourth.hint.is_none());
    assert_eq!(fourth.hints_used, 3);

    let practice = session::advance(&pool, &started.id, t0())
        .await
        .unwrap()
        .expect("session");
    assert_eq!(practice.phase, Phase::Practice);
    let first_item = practice.item.as_ref().expect("practice item").id.clone();
    let next = session::next_item(&pool, &started.id, t0())
        .await
        .unwrap()
        .expect("session");
    let second_item = next.item.as_ref().expect("next practice item").id.clone();
    assert_ne!(first_item, second_item);

    let events = session::events(&pool, &started.id).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "hint"));
    assert!(events.iter().any(|event| event.kind == "item"));
    cleanup(&db);
}

#[tokio::test]
async fn hint_progress_is_isolated_between_session_runs() {
    let (pool, db) = test_pool("hint-isolation").await;
    seed_bank(&pool).await;

    let first = session::start(&pool, StartOptions::new(Template::SchoolDay20), t0())
        .await
        .unwrap();
    let second = session::start(&pool, StartOptions::new(Template::SchoolDay20), t0())
        .await
        .unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(first.item.as_ref().unwrap().id, second.item.as_ref().unwrap().id);

    session::hint(&pool, &first.id, t0()).await.unwrap();
    session::hint(&pool, &first.id, t0()).await.unwrap();

    let first_view = session::get(&pool, &first.id, t0())
        .await
        .unwrap()
        .expect("first session");
    assert_eq!(first_view.hint_unlocked, 2);
    assert_eq!(first_view.hints_used, 2);
    assert_eq!(first_view.item.as_ref().unwrap().hint_unlocked, 2);

    let second_view = session::get(&pool, &second.id, t0())
        .await
        .unwrap()
        .expect("second session");
    assert_eq!(
        second_view.hint_unlocked, 0,
        "a hint revealed in one run must not unlock another run"
    );
    assert_eq!(second_view.hints_used, 0);
    assert_eq!(second_view.item.as_ref().unwrap().hints.len(), 3);
    assert_eq!(second_view.item.as_ref().unwrap().hint_unlocked, 0);

    cleanup(&db);
}

#[tokio::test]
async fn graded_session_updates_fsrs_and_hint_state_is_the_only_counter() {
    let (pool, db) = test_pool("graded-fsrs").await;
    seed_bank(&pool).await;

    let started = session::start(&pool, StartOptions::new(Template::SchoolDay20), t0())
        .await
        .unwrap();
    let recall_item = started.item.as_ref().expect("recall item").id.clone();

    session::hint(&pool, &started.id, t0()).await.unwrap();
    let hinted_view = session::hint(&pool, &started.id, t0())
        .await
        .unwrap()
        .expect("session");
    assert_eq!(hinted_view.hints_used, 2);

    let hinted = session::grade(
        &pool,
        &started.id,
        AttemptInput {
            item_id: recall_item.clone(),
            input_kind: "typed".into(),
            given: "42".into(),
            correct: true,
            session_id: None,
            error_tags: vec![],
            attempt_id: None,
        },
        t0(),
    )
    .await
    .unwrap()
    .expect("graded");
    assert_eq!(hinted.attempt.hints_used, 2);
    assert_eq!(hinted.attempt.rating, 2, "two hints are a Hard success");
    assert_eq!(
        hinted.session.hints_used, 0,
        "the ladder resets after grading and the view mirrors hint_state"
    );
    let stored_mastery: f64 =
        sqlx::query_scalar("SELECT mastery FROM skill_schedule WHERE skill_id = ?")
            .bind(&hinted.attempt.skill_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!((stored_mastery - hinted.attempt.mastery_after).abs() < 1e-9);

    // An unaided success on the same template must gain more mastery than
    // the hint-assisted one.
    let practice = session::advance(&pool, &started.id, t0())
        .await
        .unwrap()
        .expect("session");
    let practice_item = practice.item.as_ref().expect("practice item").id.clone();
    let unaided = session::grade(
        &pool,
        &started.id,
        AttemptInput {
            item_id: practice_item,
            input_kind: "typed".into(),
            given: "42".into(),
            correct: true,
            session_id: None,
            error_tags: vec![],
            attempt_id: None,
        },
        t0(),
    )
    .await
    .unwrap()
    .expect("graded");
    assert_eq!(unaided.attempt.rating, 4);
    assert!(
        hinted.attempt.mastery_after < unaided.attempt.mastery_after,
        "hint use must downgrade the mastery gain: {} vs {}",
        hinted.attempt.mastery_after,
        unaided.attempt.mastery_after
    );

    // Grading a wrong attempt updates the per-skill error patterns.
    for _ in 0..2 {
        session::grade(
            &pool,
            &started.id,
            AttemptInput {
                item_id: "item-03".into(),
                input_kind: "typed".into(),
                given: "0".into(),
                correct: false,
                session_id: None,
                error_tags: vec!["sign-error".into()],
                attempt_id: None,
            },
            t0(),
        )
        .await
        .unwrap()
        .expect("graded");
    }

    let ended = session::tick(&pool, &started.id, at(1200))
        .await
        .unwrap()
        .expect("session");
    assert_eq!(ended.outcome.as_deref(), Some("completed"));

    let schedules: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill_schedule")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(schedules >= 2, "graded skills have schedule rows");

    let errors: i64 = sqlx::query_scalar(
        "SELECT count FROM error_patterns WHERE skill_id = ? AND tag = 'sign-error'",
    )
    .bind(&hinted.attempt.skill_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(errors, 2);

    let level: i64 = sqlx::query_scalar(
        "SELECT level FROM hint_state WHERE session_id = ? AND item_id = ?",
    )
    .bind(&started.id)
    .bind(&recall_item)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(level, 0);
    let uses: i64 = sqlx::query_scalar(
        "SELECT uses FROM hint_state WHERE session_id = ? AND item_id = ?",
    )
    .bind(&started.id)
    .bind(&recall_item)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(uses, 2);

    // The duplicate per-run counter is gone; hint_state owns the cursor.
    let run_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('session_runs')")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        !run_columns.iter().any(|name| name == "hints_used"),
        "session_runs must not keep a second hint counter"
    );

    let events = session::events(&pool, &started.id).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "hint"));
    assert!(events.iter().any(|event| event.kind == "attempt"));
    let snapshot = fsrs::weekly_snapshot(&pool, None).await.unwrap();
    let attempts: Vec<_> = snapshot
        .sessions
        .iter()
        .filter(|event| event.kind == "attempt")
        .collect();
    assert_eq!(attempts.len(), 4);
    assert!(attempts
        .iter()
        .all(|event| event.session_id.as_deref() == Some(started.id.as_str())));
    cleanup(&db);
}

#[tokio::test]
async fn fsrs_boundary_is_optional_and_orders_items_when_present() {
    fn due(skill_id: &str) -> Option<f64> {
        match skill_id {
            "unit-conversion-scaling" => Some(9.0),
            "volume-prism-cylinder" => Some(1.0),
            _ => Some(0.0),
        }
    }

    let (pool, db) = test_pool("fsrs").await;
    seed_bank(&pool).await;

    let mut options = StartOptions::new(Template::SchoolDay20);
    options.engine = SessionEngine::with_due_score(due);
    let ordered = session::start(&pool, options, t0()).await.unwrap();
    assert_eq!(
        ordered.item.as_ref().expect("item").skill_id,
        "unit-conversion-scaling"
    );

    // No FSRS module: the engine still starts and serves bank order.
    let fallback = session::start(&pool, StartOptions::new(Template::SchoolDay20), t0())
        .await
        .unwrap();
    assert!(fallback.item.is_some());
    cleanup(&db);
}

#[tokio::test]
async fn empty_bank_still_starts_a_timed_session() {
    let (pool, db) = test_pool("empty").await;
    let started = session::start(&pool, StartOptions::new(Template::SchoolDay20), t0())
        .await
        .unwrap();
    assert_eq!(started.phase, Phase::Recall);
    assert!(started.item.is_none());
    assert_eq!(started.total_remaining_seconds, 20 * 60);
    cleanup(&db);
}
