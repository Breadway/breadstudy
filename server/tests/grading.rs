//! STUDY-17 acceptance: rubric method marks, final-answer correctness,
//! fixed error tags, the low-confidence confirmation gate and the
//! reduced-quality local fallback, all through a deterministic fake
//! grader. No live model is required.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use breadstudy_server::api::{grade_attempt, GradingState};
use breadstudy_server::grading::{
    AttemptRequest, CriterionVerdict, Endpoint, ErrorTag, GradeError, GradeFuture, GradeOutcome,
    GradeRequest, Grader, GraderResponse, GradingService, HttpGrader, InputKind,
};
use sqlx::SqlitePool;

const ITEM_ID: &str = "item-vol-1";

const RUBRIC: &str = r#"{
  "method": [
    {"id": "formula", "description": "Uses the cylinder volume formula V = pi r^2 h", "marks": 1},
    {"id": "substitution", "description": "Substitutes r = 3 and h = 7", "marks": 1}
  ],
  "final": {"marks": 1, "answer": "197.9 cm^3"}
}"#;

#[derive(Debug, Clone)]
struct RecordedRequest {
    input_kind: InputKind,
    given: Option<String>,
    has_photo: bool,
    rubric_ids: Vec<String>,
}

struct FakeGrader {
    responses: Mutex<VecDeque<anyhow::Result<GraderResponse>>>,
    requests: Mutex<Vec<RecordedRequest>>,
}

impl FakeGrader {
    fn new(responses: Vec<anyhow::Result<GraderResponse>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn recorded(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Grader for FakeGrader {
    fn grade<'a>(&'a self, request: &'a GradeRequest) -> GradeFuture<'a> {
        self.requests.lock().unwrap().push(RecordedRequest {
            input_kind: request.input_kind,
            given: request.given.clone(),
            has_photo: request.photo.is_some(),
            rubric_ids: request
                .rubric
                .method
                .iter()
                .map(|criterion| criterion.id.clone())
                .collect(),
        });
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(anyhow::anyhow!("fake grader ran out of responses")));
        Box::pin(async move { response })
    }
}

fn verdict(id: &str, awarded: bool, reason: &str) -> CriterionVerdict {
    CriterionVerdict {
        id: id.into(),
        awarded,
        reason: reason.into(),
    }
}

fn response(
    transcription: Option<&str>,
    confidence: f32,
    method: Vec<CriterionVerdict>,
    final_correct: bool,
    tags: &[&str],
    model: &str,
    degraded: bool,
) -> GraderResponse {
    GraderResponse {
        transcription: transcription.map(str::to_string),
        confidence,
        method,
        final_correct,
        final_answer: None,
        error_tags: tags.iter().map(|tag| tag.to_string()).collect(),
        feedback: "marker feedback".into(),
        model: model.into(),
        degraded,
    }
}

fn full_marks_response() -> GraderResponse {
    response(
        Some("V = pi r^2 h = pi * 3^2 * 7 = 63pi ≈ 197.9 cm^3"),
        0.95,
        vec![
            verdict("formula", true, "volume formula stated"),
            verdict("substitution", true, "r and h substituted"),
        ],
        true,
        &[],
        "fake-primary",
        false,
    )
}

async fn setup(name: &str) -> (String, SqlitePool) {
    let path = std::env::temp_dir().join(format!(
        "breadstudy-grading-test-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let db = path.to_string_lossy().to_string();
    let pool = breadstudy_server::db::connect(&db).await.unwrap();
    sqlx::query(
        "INSERT INTO skills(id, strand, name, vault_note) VALUES('volume-prism-cylinder','Geometry','Volume of prisms and cylinders','Area Volume and Surface Area')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO items(id, skill_id, prompt, answer, rubric) VALUES(?,?,?,?,?)")
        .bind(ITEM_ID)
        .bind("volume-prism-cylinder")
        .bind("A cylinder has radius 3 cm and height 7 cm. Find its volume.")
        .bind("197.9 cm^3")
        .bind(RUBRIC)
        .execute(&pool)
        .await
        .unwrap();
    (db, pool)
}

fn photo_request() -> AttemptRequest {
    AttemptRequest {
        item_id: ITEM_ID.into(),
        input_kind: InputKind::Photo,
        latex: None,
        transcript: None,
        transcription_confidence: None,
        photo_base64: Some("ZmFrZS1waG90by1ieXRlcw==".into()),
        photo_media_type: Some("image/jpeg".into()),
        confirmed_text: None,
        confirmed: false,
    }
}

async fn attempt_count(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM attempts")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn photo_grades_with_method_and_final_marks() {
    let (db, pool) = setup("photo-marks").await;
    let grader = Arc::new(FakeGrader::new(vec![Ok(full_marks_response())]));
    let service = GradingService::new(grader.clone(), 0.75);

    let outcome = service.grade(&pool, photo_request()).await.unwrap();
    let GradeOutcome::Graded { attempt } = outcome else {
        panic!("expected a graded attempt");
    };

    assert_eq!(attempt.input_kind, InputKind::Photo);
    assert!(attempt.given.contains("197.9"));
    assert_eq!(attempt.method_marks, 2.0);
    assert_eq!(attempt.method_marks_total, 2.0);
    assert_eq!(attempt.final_marks, 1.0);
    assert_eq!(attempt.final_marks_total, 1.0);
    assert!(attempt.correct);
    assert!(attempt.error_tags.is_empty());
    assert_eq!(attempt.rubric_breakdown.len(), 2);
    assert_eq!(attempt.rubric_breakdown[0].id, "formula");
    assert_eq!(attempt.rubric_breakdown[0].awarded, 1.0);

    let (correct, method, tags, degraded): (i64, f64, String, i64) = sqlx::query_as(
        "SELECT correct, method_marks, error_tags, degraded FROM attempts WHERE id = ?",
    )
    .bind(&attempt.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(correct, 1);
    assert_eq!(method, 2.0);
    assert_eq!(tags, "[]");
    assert_eq!(degraded, 0);

    let requests = grader.recorded();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].input_kind, InputKind::Photo);
    assert!(requests[0].has_photo);
    assert_eq!(requests[0].rubric_ids, vec!["formula", "substitution"]);

    drop(pool);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn sign_slip_tag_is_persisted_on_the_attempt() {
    let (db, pool) = setup("sign-slip").await;
    let sign_slip = response(
        Some("V = pi r^2 h = pi * 3^2 * 7 = -197.9 cm^3"),
        0.96,
        vec![
            verdict("formula", true, "volume formula stated"),
            verdict("substitution", true, "r and h substituted"),
        ],
        true,
        &["sign-slip", "not-a-real-tag"],
        "fake-primary",
        false,
    );
    let grader = Arc::new(FakeGrader::new(vec![Ok(sign_slip)]));
    let service = GradingService::new(grader, 0.75);

    let outcome = service.grade(&pool, photo_request()).await.unwrap();
    let GradeOutcome::Graded { attempt } = outcome else {
        panic!("expected a graded attempt");
    };

    assert!(!attempt.correct);
    assert_eq!(attempt.final_marks, 0.0);
    assert_eq!(attempt.method_marks, 2.0);
    assert_eq!(attempt.error_tags, vec![ErrorTag::SignSlip]);

    let tags: String = sqlx::query_scalar("SELECT error_tags FROM attempts WHERE id = ?")
        .bind(&attempt.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(tags.contains("sign-slip"));
    assert!(!tags.contains("not-a-real-tag"));

    drop(pool);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn low_confidence_photo_asks_for_confirmation_and_records_nothing() {
    let (db, pool) = setup("low-confidence").await;
    let unclear = response(
        Some("V = pi r^2 h = 63 pi ≈ 197.9"),
        0.42,
        vec![],
        true,
        &[],
        "fake-primary",
        false,
    );
    let grader = Arc::new(FakeGrader::new(vec![
        Ok(unclear),
        Ok(full_marks_response()),
    ]));
    let service = GradingService::new(grader, 0.75);

    let outcome = service.grade(&pool, photo_request()).await.unwrap();
    match outcome {
        GradeOutcome::NeedsConfirmation {
            transcription,
            confidence,
            input_kind,
            message,
        } => {
            assert!(transcription.contains("63 pi"));
            assert!((confidence - 0.42).abs() < f32::EPSILON);
            assert_eq!(input_kind, InputKind::Photo);
            assert!(message.contains("below"));
        }
        GradeOutcome::Graded { .. } => panic!("low confidence must not be graded silently"),
    }
    assert_eq!(attempt_count(&pool).await, 0);

    let confirmed = AttemptRequest {
        input_kind: InputKind::Photo,
        photo_base64: None,
        confirmed_text: Some("V = pi r^2 h = pi * 3^2 * 7 = 63pi ≈ 197.9 cm^3".into()),
        confirmed: true,
        ..photo_request()
    };
    let outcome = service.grade(&pool, confirmed).await.unwrap();
    let GradeOutcome::Graded { attempt } = outcome else {
        panic!("confirmed transcription should grade");
    };
    assert!(attempt.correct);
    assert_eq!(attempt_count(&pool).await, 1);

    drop(pool);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn degraded_fallback_response_is_recorded() {
    let (db, pool) = setup("fallback").await;
    let fallback = response(
        Some("V = pi r^2 h = 63 pi = 197.9 cm3"),
        0.8,
        vec![verdict("formula", true, "formula recognized")],
        true,
        &["dropped-unit"],
        "Ornith-1.5-9B-Q4_K_M",
        true,
    );
    let grader = Arc::new(FakeGrader::new(vec![Ok(fallback)]));
    let service = GradingService::new(grader, 0.75);

    let outcome = service.grade(&pool, photo_request()).await.unwrap();
    let GradeOutcome::Graded { attempt } = outcome else {
        panic!("fallback should grade");
    };

    assert!(attempt.degraded);
    assert_eq!(attempt.grader_model, "Ornith-1.5-9B-Q4_K_M");
    assert_eq!(attempt.method_marks, 1.0);
    assert_eq!(attempt.method_marks_total, 2.0);
    assert!(attempt.correct);
    assert!(attempt.error_tags.contains(&ErrorTag::DroppedUnit));

    let (degraded, model): (i64, String) =
        sqlx::query_as("SELECT degraded, grader_model FROM attempts WHERE id = ?")
            .bind(&attempt.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(degraded, 1);
    assert_eq!(model, "Ornith-1.5-9B-Q4_K_M");

    drop(pool);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn transcript_low_confidence_asks_then_grades_confirmed_text() {
    let (db, pool) = setup("transcript").await;
    let grader = Arc::new(FakeGrader::new(vec![Ok(full_marks_response())]));
    let service = GradingService::new(grader, 0.75);

    let mut request = AttemptRequest {
        item_id: ITEM_ID.into(),
        input_kind: InputKind::Transcript,
        latex: None,
        transcript: Some("V equals pi r squared h equals 63 pi".into()),
        transcription_confidence: Some(0.5),
        photo_base64: None,
        photo_media_type: None,
        confirmed_text: None,
        confirmed: false,
    };
    match service.grade(&pool, request.clone()).await.unwrap() {
        GradeOutcome::NeedsConfirmation { input_kind, .. } => {
            assert_eq!(input_kind, InputKind::Transcript);
        }
        GradeOutcome::Graded { .. } => panic!("low-confidence transcript must ask"),
    }
    assert_eq!(attempt_count(&pool).await, 0);

    request.confirmed = true;
    request.confirmed_text = Some("V = pi r^2 h = 63pi ≈ 197.9 cm^3".into());
    let outcome = service.grade(&pool, request).await.unwrap();
    let GradeOutcome::Graded { attempt } = outcome else {
        panic!("confirmed transcript should grade");
    };
    assert_eq!(attempt.input_kind, InputKind::Transcript);
    assert!(attempt.correct);

    drop(pool);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn typed_latex_grades_without_a_photo() {
    let (db, pool) = setup("latex").await;
    let grader = Arc::new(FakeGrader::new(vec![Ok(full_marks_response())]));
    let service = GradingService::new(grader.clone(), 0.75);

    let request = AttemptRequest {
        item_id: ITEM_ID.into(),
        input_kind: InputKind::Latex,
        latex: Some("V = \\pi r^2 h = 63\\pi \\approx 197.9\\ \\text{cm}^3".into()),
        transcript: None,
        transcription_confidence: None,
        photo_base64: None,
        photo_media_type: None,
        confirmed_text: None,
        confirmed: false,
    };
    let outcome = service.grade(&pool, request).await.unwrap();
    let GradeOutcome::Graded { attempt } = outcome else {
        panic!("typed latex should grade");
    };
    assert!(attempt.correct);
    let requests = grader.recorded();
    assert_eq!(requests[0].input_kind, InputKind::Latex);
    assert!(!requests[0].has_photo);
    assert!(requests[0].given.as_deref().unwrap().contains("\\pi"));

    drop(pool);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn api_handler_returns_graded_json() {
    let (db, pool) = setup("api").await;
    let grader = Arc::new(FakeGrader::new(vec![Ok(full_marks_response())]));
    let state = GradingState {
        pool: pool.clone(),
        grading: Arc::new(GradingService::new(grader, 0.75)),
    };

    let response = grade_attempt(State(state), Json(photo_request())).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["status"], "graded");
    assert_eq!(value["attempt"]["method_marks"], 2.0);
    assert_eq!(value["attempt"]["final_marks"], 1.0);
    assert_eq!(value["attempt"]["input_kind"], "photo");

    drop(pool);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn missing_item_is_rejected() {
    let (db, pool) = setup("missing-item").await;
    let grader = Arc::new(FakeGrader::new(vec![]));
    let service = GradingService::new(grader, 0.75);
    let request = AttemptRequest {
        item_id: "not-here".into(),
        ..photo_request()
    };
    let error = service.grade(&pool, request).await.unwrap_err();
    assert!(matches!(
        error,
        breadstudy_server::grading::GradeError::ItemNotFound(_)
    ));

    drop(pool);
    let _ = std::fs::remove_file(&db);
}

/// Read one HTTP request and return its body.
async fn read_http_body(socket: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt;
    let mut data = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = socket.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..read]);
        if let Some(head_end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&data[..head_end]).to_ascii_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if data.len() >= head_end + 4 + length {
                return String::from_utf8_lossy(&data[head_end + 4..head_end + 4 + length])
                    .to_string();
            }
        }
    }
    String::new()
}

/// Minimal OpenAI-compatible stub: rejects the configured primary model
/// and answers the local fallback model, so the real failover path is
/// exercised without any live model.
async fn spawn_mock_grader(
    content: String,
    reject_prefix: &'static str,
    reject_status: &'static str,
) -> String {
    use tokio::io::AsyncWriteExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let content = content.clone();
            tokio::spawn(async move {
                let body = read_http_body(&mut socket).await;
                let request: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let model = request["model"].as_str().unwrap_or("");
                let (status, payload) = if model.starts_with(reject_prefix) {
                    (
                        reject_status,
                        serde_json::json!({"error": "stub rejection"}),
                    )
                } else {
                    (
                        "200 OK",
                        serde_json::json!({"choices": [{"message": {"content": content}}]}),
                    )
                };
                let payload = payload.to_string();
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://{address}/v1")
}

#[tokio::test]
async fn proxy_failure_fails_over_to_the_local_vision_model() {
    let (db, pool) = setup("http-fallback").await;
    let fallback = response(
        Some("V = pi r^2 h = 63 pi = 197.9 cm3"),
        0.8,
        vec![verdict("formula", true, "formula recognized")],
        true,
        &["dropped-unit"],
        "",
        true,
    );
    let content = serde_json::to_string(&fallback).unwrap();
    let base_url = spawn_mock_grader(content, "deepseek", "500 Internal Server Error").await;

    let grader = HttpGrader::new(
        Endpoint {
            base_url: base_url.clone(),
            model: "deepseek-v4.1-flash".into(),
            api_key: Some("test-key".into()),
        },
        vec![Endpoint {
            base_url,
            model: "Ornith-1.5-9B-Q4_K_M".into(),
            api_key: None,
        }],
    )
    .unwrap();
    let service = GradingService::new(Arc::new(grader), 0.75);

    let outcome = service.grade(&pool, photo_request()).await.unwrap();
    let GradeOutcome::Graded { attempt } = outcome else {
        panic!("local fallback should grade");
    };
    assert!(attempt.degraded);
    assert_eq!(attempt.grader_model, "Ornith-1.5-9B-Q4_K_M");
    assert!(attempt.correct);
    assert!(attempt.error_tags.contains(&ErrorTag::DroppedUnit));
    assert_eq!(attempt.method_marks, 1.0);
    assert_eq!(attempt_count(&pool).await, 1);

    drop(pool);
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn rejected_key_is_a_config_error_not_a_silent_fallback() {
    let (db, pool) = setup("auth-failure").await;
    let fallback = response(
        Some("V = pi r^2 h = 63 pi = 197.9 cm3"),
        0.8,
        vec![verdict("formula", true, "formula recognized")],
        true,
        &[],
        "",
        true,
    );
    let content = serde_json::to_string(&fallback).unwrap();
    let base_url = spawn_mock_grader(content, "deepseek", "401 Unauthorized").await;

    let grader = HttpGrader::new(
        Endpoint {
            base_url: base_url.clone(),
            model: "deepseek-v4.1-flash".into(),
            api_key: Some("wrong-key".into()),
        },
        vec![Endpoint {
            base_url,
            model: "LFM3-Omni-2B-Q4_K_M".into(),
            api_key: None,
        }],
    )
    .unwrap();
    let service = GradingService::new(Arc::new(grader), 0.75);

    let error = service.grade(&pool, photo_request()).await.unwrap_err();
    match error {
        GradeError::Config(message) => {
            assert!(message.contains("rejected the API key"), "{message}");
        }
        other => panic!("expected a config error, got {other:?}"),
    }
    assert_eq!(attempt_count(&pool).await, 0);

    drop(pool);
    let _ = std::fs::remove_file(&db);
}
