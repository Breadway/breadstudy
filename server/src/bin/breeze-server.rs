//! breeze-server: local TTS broker for breadstudy sessions.
//!
//! Fronts the upstream Breeze TTS server (GPU, Vulkan on the A380) with a
//! response cache keyed on (text, instruction, cfg_scale) and a null-TTS
//! fallback. When the GPU service is down the broker answers with silent PCM
//! plus subtitles so a session continues text-only.
//!
//! Endpoints:
//! - GET /healthz
//! - POST /v1/speak with JSON {text, instruction?, cfg_scale?} answers JSON
//!   {sample_rate, channels, cached, fallback, subtitle, audio_base64}
//! - WS /v1/stream takes the same JSON per text frame and answers one meta
//!   text frame {subtitle, cached, fallback, sample_rate, bytes} followed by
//!   one binary frame with the WAV document.

use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
};
use breadstudy_server::voice::{Broker, SpeakRequest};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Deserialize)]
struct SpeakBody {
    text: String,
    instruction: Option<String>,
    cfg_scale: Option<f32>,
}

#[derive(Debug, Serialize)]
struct SpeakReply {
    sample_rate: u32,
    channels: u16,
    cached: bool,
    fallback: bool,
    subtitle: String,
    audio_base64: String,
}

#[derive(Debug, Serialize)]
struct StreamMeta {
    subtitle: String,
    cached: bool,
    fallback: bool,
    sample_rate: u32,
    bytes: usize,
}

#[derive(Clone)]
struct AppState {
    broker: Arc<Broker>,
}

fn bad_request(msg: &str) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, msg.to_string())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn speak(
    State(state): State<AppState>,
    Json(body): Json<SpeakBody>,
) -> Result<Json<SpeakReply>, (StatusCode, String)> {
    if body.text.trim().is_empty() {
        return Err(bad_request("text must not be empty"));
    }
    let req = SpeakRequest {
        text: body.text,
        instruction: body.instruction,
        cfg_scale: body.cfg_scale,
    };
    let out = state.broker.speak(&req).await;
    Ok(Json(SpeakReply {
        sample_rate: out.sample_rate,
        channels: 1,
        cached: out.cached,
        fallback: out.fallback,
        subtitle: out.subtitle,
        audio_base64: breadstudy_server::voice::base64_encode(&out.wav),
    }))
}

async fn stream(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |mut socket| async move {
        while let Some(msg) = socket.recv().await {
            let msg = match msg {
                Ok(m) => m,
                Err(_) => break,
            };
            let text = match msg {
                Message::Text(t) => t.to_string(),
                Message::Close(_) => break,
                _ => continue,
            };
            let body: SpeakBody = match serde_json::from_str(&text) {
                Ok(b) => b,
                Err(_) => {
                    let _ = socket
                        .send(Message::Text(r#"{"error":"bad request: JSON with text field expected"}"#.into()))
                        .await;
                    continue;
                }
            };
            if body.text.trim().is_empty() {
                let _ = socket
                    .send(Message::Text(r#"{"error":"bad request: text must not be empty"}"#.into()))
                    .await;
                continue;
            }
            let req = SpeakRequest {
                text: body.text,
                instruction: body.instruction,
                cfg_scale: body.cfg_scale,
            };
            let out = state.broker.speak(&req).await;
            let meta = StreamMeta {
                bytes: out.wav.len(),
                subtitle: out.subtitle,
                cached: out.cached,
                fallback: out.fallback,
                sample_rate: out.sample_rate,
            };
            let meta_text = serde_json::to_string(&meta).unwrap_or_else(|_| "{}".into());
            if socket.send(Message::Text(meta_text.into())).await.is_err() {
                break;
            }
            if socket.send(Message::Binary(out.wav.into())).await.is_err() {
                break;
            }
        }
    })
}

pub fn router(broker: Broker) -> Router {
    let state = AppState {
        broker: Arc::new(broker),
    };
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/speak", post(speak))
        .route("/v1/stream", get(stream))
        .with_state(state)
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let broker = Broker::new(
        env_or("BREEZE_CACHE_DIR", "data/breeze-cache"),
        env_or("BREEZE_UPSTREAM_URL", "http://127.0.0.1:7313"),
    );
    let listen = env_or("BREEZE_LISTEN", "127.0.0.1:7312");
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!("breeze-server listening on {listen}");
    axum::serve(listener, router(broker)).await?;
    Ok(())
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use axum::http::header::CONTENT_TYPE;
    use breadstudy_server::voice::SAMPLE_RATE;

    async fn serve() -> String {
        let dir = std::env::temp_dir().join(format!(
            "breadstudy-breeze-http-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let broker = Broker::new(&dir, "http://127.0.0.1:1");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router(broker)).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn speak_endpoint_returns_fallback_json_when_gpu_down() {
        let base = serve().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{base}/v1/speak"))
            .json(&serde_json::json!({"text": "Area equals pi r squared."}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get(CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            "application/json"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["fallback"], true);
        assert_eq!(body["cached"], false);
        assert_eq!(body["sample_rate"], SAMPLE_RATE);
        assert_eq!(body["subtitle"], "Area equals pi r squared.");
        assert!(!body["audio_base64"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn speak_endpoint_rejects_empty_text() {
        let base = serve().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{base}/v1/speak"))
            .json(&serde_json::json!({"text": "   "}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn healthz_answers_ok() {
        let base = serve().await;
        let body = reqwest::get(format!("{base}/healthz"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "ok");
    }

    #[tokio::test]
    async fn stream_replies_meta_then_audio() {
        use futures_util::{SinkExt, StreamExt};
        let base = serve().await;
        let ws_url = base.replacen("http", "ws", 1) + "/v1/stream";
        let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
        ws.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"text":"Area equals pi r squared."}"#.into(),
        ))
        .await
        .unwrap();
        let meta_msg = ws.next().await.unwrap().unwrap();
        let meta_text = match meta_msg {
            tokio_tungstenite::tungstenite::Message::Text(t) => t.to_string(),
            other => panic!("expected meta text frame, got {other:?}"),
        };
        let meta: serde_json::Value = serde_json::from_str(&meta_text).unwrap();
        assert_eq!(meta["fallback"], true);
        assert_eq!(meta["cached"], false);
        assert_eq!(meta["sample_rate"], SAMPLE_RATE);
        assert_eq!(meta["subtitle"], "Area equals pi r squared.");
        let audio_msg = ws.next().await.unwrap().unwrap();
        let audio = match audio_msg {
            tokio_tungstenite::tungstenite::Message::Binary(b) => b,
            other => panic!("expected binary audio frame, got {other:?}"),
        };
        assert_eq!(meta["bytes"], audio.len());
        let info = breadstudy_server::voice::parse_wav(&audio).expect("valid wav");
        assert_eq!(info.sample_rate, SAMPLE_RATE);
        assert_eq!(info.channels, 1);
    }
}
