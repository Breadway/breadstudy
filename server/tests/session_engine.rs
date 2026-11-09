//! STUDY-16 acceptance: server-driven session templates, timer expiry,
//! off-task pause, cached content, skip handling and the readable event
//! log.

use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use sqlx::SqlitePool;

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
        let hints = serde_json::json!([
            format!("first hint for {id}"),
            format!("second hint for {id}")
        ])
        .to_string();
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
    assert_eq!(item.hint_count, 2);
    assert!(item.hints.is_empty(), "locked hints must not leak");

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
    assert_eq!(first.session.item.as_ref().unwrap().hints.len(), 1);

    let second = session::hint(&pool, &started.id, t0())
        .await
        .unwrap()
        .expect("session");
    assert_eq!(second.hints_used, 2);
    assert_eq!(second.session.item.as_ref().unwrap().hints.len(), 2);

    let third = session::hint(&pool, &started.id, t0())
        .await
        .unwrap()
        .expect("session");
    assert!(third.hint.is_none());
    assert_eq!(third.hints_used, 2);

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
