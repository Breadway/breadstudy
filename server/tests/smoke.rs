//! End-to-end smoke test: boot the real server binary against a temporary
//! SQLite file, then exercise the health check and one merged API route.
//!
//! The binary is spawned as a child process because the router builder lives
//! in `main.rs`. That also proves the deployable artifact (migrations included)
//! actually starts, rather than only testing a library-only construction path.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct TestServer {
    child: Child,
    dir: PathBuf,
    base: String,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    let listener =
        TcpListener::bind("127.0.0.1:0").expect("reserve an ephemeral loopback port");
    listener
        .local_addr()
        .expect("read the reserved port")
        .port()
}

async fn start_server() -> TestServer {
    let dir = std::env::temp_dir().join(format!("breadstudy-smoke-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create the temp database directory");
    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_breadstudy-server"))
        .env("BREADSTUDY_LISTEN", format!("127.0.0.1:{port}"))
        .env("BREADSTUDY_DATABASE_PATH", dir.join("breadstudy.db"))
        // Grading validates a key at startup; any non-empty value satisfies it
        // without a network call.
        .env("BREADSTUDY_LLM_API_KEY", "smoke-test-key")
        .env(
            "BREADSTUDY_WEB_DIR",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../web"),
        )
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn breadstudy-server");

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().expect("poll the server process") {
            panic!("breadstudy-server exited before becoming healthy: {status}");
        }
        if let Ok(response) = client.get(format!("{base}/healthz")).send().await {
            if response.status().is_success() {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "breadstudy-server did not become healthy on {base}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    TestServer { child, dir, base }
}

#[tokio::test]
async fn boots_and_serves_healthz_and_planner() {
    let server = start_server().await;
    let client = reqwest::Client::new();

    let health = client
        .get(format!("{}/healthz", server.base))
        .send()
        .await
        .expect("GET /healthz");
    assert_eq!(health.status(), reqwest::StatusCode::OK);
    assert_eq!(health.text().await.expect("healthz body"), "ok");

    let due = client
        .get(format!("{}/api/plan/due", server.base))
        .send()
        .await
        .expect("GET /api/plan/due");
    assert_eq!(
        due.status(),
        reqwest::StatusCode::OK,
        "a merged planner route should answer"
    );
    let body: serde_json::Value = due.json().await.expect("due skills JSON");
    assert_eq!(
        body.as_array().map(Vec::len),
        Some(0),
        "a fresh database has no due skills"
    );

    let index = client
        .get(format!("{}/", server.base))
        .send()
        .await
        .expect("GET /");
    assert_eq!(index.status(), reqwest::StatusCode::OK);
    let html = index.text().await.expect("index body");
    assert!(
        html.contains("<html"),
        "the PWA shell should be served at /, got: {html:.80}"
    );
}

#[tokio::test]
async fn accepts_phone_sized_bodies_and_explains_oversized_ones() {
    let server = start_server().await;
    let client = reqwest::Client::new();

    // 3 MB of base64-ish text is what a compressed phone photo looks like
    // after JSON encoding. It must reach the handler rather than 413.
    let accepted = serde_json::json!({
        "item_id": "missing-item",
        "input_kind": "latex",
        "latex": "x".repeat(3 * 1024 * 1024),
    });
    let response = client
        .post(format!("{}/api/attempts", server.base))
        .json(&accepted)
        .send()
        .await
        .expect("POST a 3 MB attempt");
    assert_ne!(
        response.status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE,
        "the 8 MB limit must accept a phone-sized photo"
    );
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

    let oversized = serde_json::json!({
        "item_id": "missing-item",
        "input_kind": "latex",
        "latex": "x".repeat(9 * 1024 * 1024),
    });
    let response = client
        .post(format!("{}/api/attempts", server.base))
        .json(&oversized)
        .send()
        .await
        .expect("POST an oversized attempt");
    assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    let body: serde_json::Value = response.json().await.expect("JSON 413 body");
    let message = body["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("8 MB"),
        "the 413 must explain the limit, got: {message}"
    );
}
