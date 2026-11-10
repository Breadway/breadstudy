use axum::body::Body;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header::CONTENT_TYPE, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use serde::Deserialize;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Duration;

/// One indexed skill, as exposed to the PWA dashboard.
#[derive(serde::Serialize)]
pub struct SkillSummary {
    pub id: String,
    pub strand: String,
    pub name: String,
    pub vault_note: String,
}

/// `GET /api/skills`: every skill the vault indexer imported.
async fn list_skills(
    State(pool): State<SqlitePool>,
) -> Result<Json<Vec<SkillSummary>>, (StatusCode, String)> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT id, strand, name, vault_note FROM skills ORDER BY strand, name",
    )
    .fetch_all(&pool)
    .await
    .map_err(internal_error_message)?;

    Ok(Json(
        rows.into_iter()
            .map(|(id, strand, name, vault_note)| SkillSummary {
                id,
                strand,
                name,
                vault_note,
            })
            .collect(),
    ))
}

/// Merge into the application router with the pool attached.
pub fn skills_router(pool: SqlitePool) -> Router {
    let service = Arc::new(TtsService::from_env());
    Router::new()
        .route("/api/skills", get(list_skills))
        .route(
            "/api/skills/{skill_id}/teach-context",
            get(api_teach_context),
        )
        .route("/api/tts", post(tts))
        .layer(Extension(service))
        .with_state(pool)
}

// ---------------------------------------------------------------------------
// Teaching mode: topic context and speech proxy
// ---------------------------------------------------------------------------

const MAX_TTS_INPUT_CHARS: usize = 1200;
const DEFAULT_KOKORO_URL: &str = "http://127.0.0.1:7314";
const DEFAULT_BREEZE_URL: &str = "http://127.0.0.1:7312";
const DEFAULT_GEMINI_MODEL: &str = "gemini-2.5-flash-preview-tts";
const DEFAULT_GEMINI_KEY_FILE: &str = "/etc/breadstudy/gemini.key";
const BREEZE_INSTRUCTION: &str = "Patient coach: a calm, encouraging male mentor with an Australian accent, speaking slowly and precisely to a Year 10 student.";

const KOKORO_VOICES: [&str; 7] = [
    "bm_george",
    "af_heart",
    "bf_alice",
    "af_bella",
    "am_michael",
    "bf_emma",
    "bm_lewis",
];

const GEMINI_VOICES: [&str; 8] = [
    "Kore", "Puck", "Charon", "Fenrir", "Aoede", "Leda", "Orus", "Zephyr",
];

/// Header name the PWA reads to show which speech backend served a clip.
pub const TTS_BACKEND_HEADER: &str = "x-breadstudy-tts-backend";
pub const TTS_VOICE_HEADER: &str = "x-breadstudy-tts-voice";

/// What the teaching view needs for a topic header. The vault note name comes
/// from the index because the container does not mount the vault checkout.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TeachContext {
    pub skill_id: String,
    pub name: String,
    pub strand: String,
    pub vault_note: String,
    pub vault_note_path: String,
    pub sample_prompts: Vec<String>,
}

#[derive(Debug)]
pub enum TeachContextError {
    NotFound(String),
    Db(sqlx::Error),
}

impl std::fmt::Display for TeachContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeachContextError::NotFound(id) => write!(f, "skill {id} not found"),
            TeachContextError::Db(error) => write!(f, "{error}"),
        }
    }
}

impl IntoResponse for TeachContextError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            TeachContextError::NotFound(id) => (
                StatusCode::NOT_FOUND,
                format!("skill {id} is not in the index"),
            ),
            TeachContextError::Db(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        };
        (
            status,
            Json(json!({ "status": "error", "error": message })),
        )
            .into_response()
    }
}

/// Load the topic header and a handful of starter questions. Verified item
/// prompts are included as walk-through prompts, but answers and hints stay
/// out of the response.
pub async fn teach_context(
    pool: &SqlitePool,
    skill_id: &str,
) -> Result<TeachContext, TeachContextError> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT name, strand, vault_note FROM skills WHERE id = ?1",
    )
    .bind(skill_id)
    .fetch_optional(pool)
    .await
    .map_err(TeachContextError::Db)?;
    let (name, strand, vault_note) =
        row.ok_or_else(|| TeachContextError::NotFound(skill_id.to_string()))?;

    let prompts: Vec<(String,)> = sqlx::query_as(
        "SELECT prompt FROM items WHERE skill_id = ?1 AND status = 'verified' ORDER BY id LIMIT 2",
    )
    .bind(skill_id)
    .fetch_all(pool)
    .await
    .map_err(TeachContextError::Db)?;

    let mut sample_prompts = vec![
        format!("Teach me {name} from the start, with a simple example."),
        format!("Show me a worked exam question on {name}."),
        format!("What mistakes should I avoid in {name}?"),
    ];
    for (prompt,) in prompts {
        let prompt = truncate_chars(prompt.trim(), 140);
        if !prompt.is_empty() {
            sample_prompts.push(format!("Walk me through: {prompt}"));
        }
    }
    sample_prompts.truncate(5);

    Ok(TeachContext {
        skill_id: skill_id.to_string(),
        name,
        strand,
        vault_note_path: format!("Maths/{vault_note}.md"),
        vault_note,
        sample_prompts,
    })
}

async fn api_teach_context(
    State(pool): State<SqlitePool>,
    Path(skill_id): Path<String>,
) -> Result<Json<TeachContext>, TeachContextError> {
    teach_context(&pool, &skill_id).await.map(Json)
}

#[derive(Debug, Clone)]
struct GeminiTts {
    key: String,
    model: String,
}

/// Speech proxy state. Auto order is Kokoro, then Gemini when configured, then
/// Breeze when its broker is already running. Browser speech remains the
/// client's final fallback, so a 503 here is a normal, recoverable state.
#[derive(Clone)]
pub struct TtsService {
    http: reqwest::Client,
    kokoro_url: String,
    breeze_url: String,
    gemini: Option<GeminiTts>,
}

#[derive(Debug)]
struct TtsFailure {
    status: StatusCode,
    message: String,
}

impl TtsFailure {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
struct TtsAudio {
    bytes: Vec<u8>,
    content_type: String,
    backend: &'static str,
    voice: String,
}

#[derive(Debug, Deserialize)]
pub struct TtsRequest {
    #[serde(default)]
    pub input: String,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub speed: Option<f32>,
    /// auto, kokoro, gemini or breeze. Omitted means auto.
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

impl TtsService {
    pub fn from_env() -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(1200))
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        let kokoro_url = std::env::var("BREADSTUDY_KOKORO_TTS_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_KOKORO_URL.to_string());
        let breeze_url = std::env::var("BREADSTUDY_BREEZE_TTS_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BREEZE_URL.to_string());
        let gemini = gemini_key().map(|key| GeminiTts {
            key,
            model: std::env::var("BREADSTUDY_GEMINI_TTS_MODEL")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_GEMINI_MODEL.to_string()),
        });
        if gemini.is_some() {
            tracing::info!("Gemini TTS fallback is configured");
        } else {
            tracing::info!("Gemini TTS is not configured; Kokoro, Breeze or browser speech will be used");
        }
        Self {
            http,
            kokoro_url,
            breeze_url,
            gemini,
        }
    }

    #[cfg(test)]
    fn for_test(
        kokoro_url: impl Into<String>,
        breeze_url: impl Into<String>,
        gemini: Option<GeminiTts>,
    ) -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_millis(200))
                .timeout(Duration::from_secs(5))
                .build()
                .expect("test http client"),
            kokoro_url: kokoro_url.into(),
            breeze_url: breeze_url.into(),
            gemini,
        }
    }

    async fn synthesize(&self, request: &TtsRequest) -> Result<TtsAudio, TtsFailure> {
        let text = request.input.trim();
        if text.is_empty() {
            return Err(TtsFailure::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "input is required",
            ));
        }
        if text.chars().count() > MAX_TTS_INPUT_CHARS {
            return Err(TtsFailure::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("input is longer than {MAX_TTS_INPUT_CHARS} characters"),
            ));
        }
        let speed = request.speed.unwrap_or(1.0).clamp(0.5, 2.0);
        let requested = request.backend.as_deref().unwrap_or("auto").trim().to_ascii_lowercase();
        match requested.as_str() {
            "kokoro" => {
                self.kokoro(text, request.voice.as_deref(), speed)
                    .await
                    .map_err(|message| {
                        TtsFailure::new(
                            StatusCode::SERVICE_UNAVAILABLE,
                            format!("{message}; falling back to browser speech"),
                        )
                    })
            }
            "gemini" => self.gemini_only(text, request.voice.as_deref(), speed).await,
            "breeze" => self.breeze(text).await,
            "auto" | "" => self.auto(text, request.voice.as_deref(), speed).await,
            other => Err(TtsFailure::new(
                StatusCode::BAD_REQUEST,
                format!("unknown TTS backend {other}; use auto, kokoro, gemini, breeze or browser"),
            )),
        }
    }

    /// Auto order: Kokoro, then Gemini when a key exists, then Breeze when its
    /// broker is already running. Nothing here starts the GPU broker or
    /// touches the llama router.
    async fn auto(
        &self,
        text: &str,
        requested_voice: Option<&str>,
        speed: f32,
    ) -> Result<TtsAudio, TtsFailure> {
        let mut failures: Vec<String> = Vec::new();
        match self.kokoro(text, requested_voice, speed).await {
            Ok(audio) => return Ok(audio),
            Err(error) => failures.push(error),
        }
        if self.gemini.is_some() {
            match self.gemini_only(text, requested_voice, speed).await {
                Ok(audio) => return Ok(audio),
                Err(error) => failures.push(error.message),
            }
        }
        match self.breeze(text).await {
            Ok(audio) => return Ok(audio),
            Err(error) => failures.push(error.message),
        }
        Err(TtsFailure::new(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "{}; falling back to browser speech",
                failures.join("; ")
            ),
        ))
    }

    async fn kokoro(
        &self,
        text: &str,
        requested_voice: Option<&str>,
        speed: f32,
    ) -> Result<TtsAudio, String> {
        let voice = pick_voice(requested_voice, "bm_george", &KOKORO_VOICES);
        let url = format!(
            "{}/v1/audio/speech",
            self.kokoro_url.trim_end_matches('/')
        );
        let body = json!({
            "model": "kokoro",
            "input": text,
            "voice": voice,
            "speed": speed,
        });
        let response = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("Kokoro sidecar is unreachable: {error}"))?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.starts_with("audio/"))
            .unwrap_or("audio/wav")
            .to_string();
        if !status.is_success() {
            let raw = response.text().await.unwrap_or_default();
            return Err(format!(
                "Kokoro returned {status}: {}",
                truncate_chars(&raw, 220)
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("could not read Kokoro audio: {error}"))?
            .to_vec();
        if bytes.is_empty() {
            return Err("Kokoro returned an empty clip".to_string());
        }
        Ok(TtsAudio {
            bytes,
            content_type,
            backend: "kokoro",
            voice,
        })
    }

    /// Use the already-running Breeze broker. If the broker is up but its GPU
    /// upstream is down it answers with silent fallback audio, which is not
    /// useful for teaching, so that case is reported as unavailable and the
    /// client uses browser speech instead.
    async fn breeze(&self, text: &str) -> Result<TtsAudio, TtsFailure> {
        let url = format!("{}/v1/speak", self.breeze_url.trim_end_matches('/'));
        let body = json!({
            "text": text,
            "instruction": BREEZE_INSTRUCTION,
            "cfg_scale": 1.0,
        });
        let response = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                TtsFailure::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("Breeze broker is unreachable: {error}"),
                )
            })?;
        let status = response.status();
        let raw = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(TtsFailure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("Breeze broker returned {status}: {}", truncate_chars(&raw, 220)),
            ));
        }
        let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
            TtsFailure::new(
                StatusCode::BAD_GATEWAY,
                format!("Breeze broker returned invalid JSON: {error}"),
            )
        })?;
        if parsed
            .get("fallback")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            return Err(TtsFailure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "Breeze broker is running but the GPU is unavailable; falling back to browser speech",
            ));
        }
        let data = parsed
            .get("audio_base64")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                TtsFailure::new(
                    StatusCode::BAD_GATEWAY,
                    "Breeze broker returned no audio; falling back to browser speech",
                )
            })?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| {
                TtsFailure::new(
                    StatusCode::BAD_GATEWAY,
                    format!("Breeze audio was not valid base64: {error}"),
                )
            })?;
        Ok(TtsAudio {
            bytes,
            content_type: "audio/wav".to_string(),
            backend: "breeze",
            voice: "patient-coach".to_string(),
        })
    }

    async fn gemini_only(
        &self,
        text: &str,
        requested_voice: Option<&str>,
        speed: f32,
    ) -> Result<TtsAudio, TtsFailure> {
        let Some(gemini) = &self.gemini else {
            return Err(TtsFailure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "Gemini TTS is not configured; falling back to browser speech",
            ));
        };
        // Gemini TTS takes no speed parameter. Keep the client contract and
        // apply speed only on backends that support it.
        let _ = speed;
        let voice = pick_voice(requested_voice, "Kore", &GEMINI_VOICES);
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
            gemini.model
        );
        let body = json!({
            "contents": [{ "parts": [{ "text": text }] }],
            "generationConfig": {
                "responseModalities": ["AUDIO"],
                "speechConfig": {
                    "voiceConfig": {
                        "prebuiltVoiceConfig": { "voiceName": voice }
                    }
                }
            }
        });
        let response = self
            .http
            .post(&url)
            .header("x-goog-api-key", &gemini.key)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                TtsFailure::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("Gemini TTS request failed: {error}; falling back to browser speech"),
                )
            })?;
        let status = response.status();
        let raw = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(TtsFailure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "Gemini TTS returned {status}: {}; falling back to browser speech",
                    truncate_chars(&raw, 220)
                ),
            ));
        }
        let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
            TtsFailure::new(
                StatusCode::BAD_GATEWAY,
                format!("Gemini TTS returned invalid JSON: {error}"),
            )
        })?;
        let (data, mime) = gemini_inline_audio(&parsed).ok_or_else(|| {
            TtsFailure::new(
                StatusCode::BAD_GATEWAY,
                "Gemini TTS returned no audio; falling back to browser speech",
            )
        })?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| {
                TtsFailure::new(
                    StatusCode::BAD_GATEWAY,
                    format!("Gemini TTS audio was not valid base64: {error}"),
                )
            })?;
        let bytes = if decoded.starts_with(b"RIFF") || mime.contains("wav") {
            decoded
        } else {
            pcm16_to_wav(&decoded, sample_rate_from_mime(mime))
        };
        Ok(TtsAudio {
            bytes,
            content_type: "audio/wav".to_string(),
            backend: "gemini",
            voice,
        })
    }
}

async fn tts(
    Extension(service): Extension<Arc<TtsService>>,
    Json(request): Json<TtsRequest>,
) -> Response {
    match service.synthesize(&request).await {
        Ok(audio) => Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, audio.content_type)
            .header(TTS_BACKEND_HEADER, audio.backend)
            .header(TTS_VOICE_HEADER, audio.voice)
            .header("cache-control", "no-store")
            .body(Body::from(audio.bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(failure) => (
            failure.status,
            Json(json!({
                "status": "error",
                "error": failure.message,
                "backend": null,
                "fallback": "browser",
            })),
        )
            .into_response(),
    }
}

fn pick_voice(requested: Option<&str>, default: &str, allowed: &[&str]) -> String {
    let requested = requested.unwrap_or("").trim();
    if allowed.contains(&requested) {
        requested.to_string()
    } else {
        default.to_string()
    }
}

fn gemini_inline_audio(value: &serde_json::Value) -> Option<(&str, &str)> {
    let parts = value
        .get("candidates")?
        .get(0)?
        .get("content")?
        .get("parts")?
        .as_array()?;
    for part in parts {
        let inline = part.get("inlineData").or_else(|| part.get("inline_data"));
        if let Some(inline) = inline {
            let data = inline.get("data")?.as_str()?;
            let mime = inline
                .get("mimeType")
                .or_else(|| inline.get("mime_type"))
                .and_then(|value| value.as_str())
                .unwrap_or("audio/L16;rate=24000");
            return Some((data, mime));
        }
    }
    None
}

fn sample_rate_from_mime(mime: &str) -> u32 {
    mime.split(';')
        .find_map(|part| part.trim().strip_prefix("rate="))
        .and_then(|rate| rate.parse::<u32>().ok())
        .filter(|rate| (8_000..=96_000).contains(rate))
        .unwrap_or(24_000)
}

fn pcm16_to_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let byte_rate = sample_rate * 2;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max).collect();
    out.push_str("...");
    out
}

fn gemini_key() -> Option<String> {
    if let Ok(key) = std::env::var("BREADSTUDY_GEMINI_API_KEY") {
        let key = key.trim();
        if !key.is_empty() {
            return Some(key.to_string());
        }
    }
    let path = std::env::var("BREADSTUDY_GEMINI_API_KEY_FILE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_GEMINI_KEY_FILE.to_string());
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|key| !key.is_empty())
}

use serde_json::json;

use crate::grading::{AttemptRequest, GradeError, GradingService};

/// State for the grading routes. The rest of the server can merge this
/// router once `main.rs` imports it.
#[derive(Clone)]
pub struct GradingState {
    pub pool: SqlitePool,
    pub grading: Arc<GradingService>,
}

/// Router fragment for STUDY-17. Merge it with the base router:
///
/// ```ignore
/// let app = Router::new().route("/healthz", get(|| async { "ok" }))
///     .merge(breadstudy_server::api::grading_router(state));
/// ```
pub fn grading_router(state: GradingState) -> Router {
    Router::new()
        .route("/api/attempts", post(grade_attempt))
        .with_state(state)
}

/// `POST /api/attempts`: grade a photo, typed LaTeX or spoken transcript
/// against the item rubric and persist the attempt.
pub async fn grade_attempt(
    State(state): State<GradingState>,
    Json(request): Json<AttemptRequest>,
) -> Response {
    match state.grading.grade(&state.pool, request).await {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(error) => {
            let status = match &error {
                GradeError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
                GradeError::ItemNotFound(_) => StatusCode::NOT_FOUND,
                GradeError::AllGradersFailed(_) => StatusCode::BAD_GATEWAY,
                GradeError::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,
                GradeError::Db(_) | GradeError::Data(_) | GradeError::Schedule(_) => {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            };
            if status.is_server_error() {
                tracing::error!(error = %error, "grading request failed");
            } else {
                tracing::warn!(error = %error, "grading request rejected");
            }
            (
                status,
                Json(json!({
                    "status": "error",
                    "error": error.to_string(),
                })),
            )
                .into_response()
        }
    }
}
// Route blocks appended by STUDY-18 (FSRS planner, hints, weekly review)
// and STUDY-16 (server-driven session engine). Both are merged into the
// application router by main.rs.

use chrono::Utc;

use crate::fsrs::{self, DueSkill, HintReveal, WeeklySnapshot};
use crate::session::{
    self, HintOutcome, OffTaskOutcome, SessionEngine, SessionView, SkipOutcome, StartOptions,
    Template,
};

// ---------------------------------------------------------------------------
// FSRS planner, hint ladder and weekly review (STUDY-18)
// ---------------------------------------------------------------------------

/// Mount with `.merge(api::fsrs_routes()).with_state(pool)` once the
/// server owns a pool.
pub fn fsrs_routes() -> Router<SqlitePool> {
    Router::new()
        .route("/api/plan/due", get(api_due_skills))
        .route("/api/items/{item_id}/hint", post(api_next_hint))
        .route("/api/weekly/snapshot", get(api_weekly_snapshot))
}

fn internal_error_message(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

async fn api_due_skills(
    State(pool): State<SqlitePool>,
) -> Result<Json<Vec<DueSkill>>, (StatusCode, String)> {
    fsrs::due_skills(&pool)
        .await
        .map(Json)
        .map_err(internal_error_message)
}

async fn api_next_hint(
    State(pool): State<SqlitePool>,
    Path(item_id): Path<String>,
) -> Result<Json<Option<HintReveal>>, (StatusCode, String)> {
    fsrs::next_hint(&pool, &item_id)
        .await
        .map(Json)
        .map_err(internal_error_message)
}

#[derive(Debug, Deserialize)]
struct SnapshotQuery {
    since: Option<String>,
}

async fn api_weekly_snapshot(
    State(pool): State<SqlitePool>,
    Query(query): Query<SnapshotQuery>,
) -> Result<Json<WeeklySnapshot>, (StatusCode, String)> {
    fsrs::weekly_snapshot(&pool, query.since.as_deref())
        .await
        .map(Json)
        .map_err(internal_error_message)
}

// ---------------------------------------------------------------------------
// Server-driven session routes (STUDY-16)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct SessionState {
    pool: SqlitePool,
    engine: SessionEngine,
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(id: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("session {id} not found"),
        }
    }

    fn unprocessable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

fn internal_error(error: anyhow::Error) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: error.to_string(),
    }
}

#[derive(serde::Deserialize)]
struct StartSessionRequest {
    template: Option<String>,
    grace_tokens: Option<u32>,
    /// Scope the run to one skill (STUDY-45). Omitted keeps the existing
    /// whole-bank behaviour, so old clients are unaffected.
    skill_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct OffTaskRequest {
    seconds: u32,
}

#[derive(serde::Deserialize)]
struct SkipRequest {
    reason: String,
}

async fn start_session(
    State(state): State<SessionState>,
    Json(request): Json<StartSessionRequest>,
) -> Result<(StatusCode, Json<SessionView>), ApiError> {
    let template = match request.template.as_deref() {
        None => Template::SchoolDay20,
        Some(value) => Template::parse(value).ok_or_else(|| {
            ApiError::bad_request(format!(
                "unknown template {value}; use school_day_20 or weekend_40"
            ))
        })?,
    };
    let mut options = StartOptions::new(template);
    if let Some(tokens) = request.grace_tokens {
        options.grace_tokens = tokens;
    }
    if let Some(skill_id) = request.skill_id.as_deref() {
        let skill_exists: Option<String> = sqlx::query_scalar("SELECT id FROM skills WHERE id = ?")
            .bind(skill_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|error| internal_error(error.into()))?;
        if skill_exists.is_none() {
            return Err(ApiError::unprocessable(format!("unknown skill {skill_id}")));
        }
        options.skill_id = Some(skill_id.to_string());
    }
    options.engine = state.engine;
    let view = session::start(&state.pool, options, Utc::now())
        .await
        .map_err(|error| {
            if error.to_string().contains("has no verified items") {
                ApiError::unprocessable(error.to_string())
            } else {
                internal_error(error)
            }
        })?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn get_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
) -> Result<Json<SessionView>, ApiError> {
    let view = session::get(&state.pool, &id, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(view))
}

async fn tick_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
) -> Result<Json<SessionView>, ApiError> {
    let view = session::tick(&state.pool, &id, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(view))
}

async fn advance_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
) -> Result<Json<SessionView>, ApiError> {
    let view = session::advance(&state.pool, &id, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(view))
}

async fn off_task_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
    Json(request): Json<OffTaskRequest>,
) -> Result<Json<OffTaskOutcome>, ApiError> {
    let outcome = session::off_task(&state.pool, &id, request.seconds, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(outcome))
}

async fn resume_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
) -> Result<Json<SessionView>, ApiError> {
    let view = session::resume(&state.pool, &id, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(view))
}

async fn skip_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
    Json(request): Json<SkipRequest>,
) -> Result<Json<SkipOutcome>, ApiError> {
    let outcome = session::skip(&state.pool, &id, &request.reason, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(outcome))
}

async fn hint_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
) -> Result<Json<HintOutcome>, ApiError> {
    let outcome = session::hint(&state.pool, &id, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(outcome))
}

async fn next_item_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
) -> Result<Json<SessionView>, ApiError> {
    let view = session::next_item(&state.pool, &id, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(view))
}

async fn grade_parts_session(
    State(state): State<SessionState>,
    Path(id): Path<String>,
    Json(request): Json<session::MultiPartRequest>,
) -> Result<Json<session::MultiPartGradeOutcome>, ApiError> {
    let outcome = session::grade_parts(&state.pool, &id, request, Utc::now())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(&id))?;
    Ok(Json(outcome))
}

async fn weekly_skips(
    State(state): State<SessionState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let count = session::weekly_skip_count(&state.pool, Utc::now())
        .await
        .map_err(internal_error)?;
    Ok(Json(serde_json::json!({ "weekly_skip_count": count })))
}

/// Routes for the server-driven session engine. Merge this into the
/// application router; the returned router already carries its pool.
pub fn session_router(pool: SqlitePool) -> Router {
    session_router_with_engine(pool, SessionEngine::new())
}

/// Same as [`session_router`], with an optional FSRS due-ness scorer
/// injected at the narrow `fn(&str) -> Option<f64>` boundary.
pub fn session_router_with_engine(pool: SqlitePool, engine: SessionEngine) -> Router {
    Router::new()
        .route("/api/session", post(start_session))
        .route("/api/session/{id}", get(get_session))
        .route("/api/session/{id}/tick", post(tick_session))
        .route("/api/session/{id}/advance", post(advance_session))
        .route("/api/session/{id}/off-task", post(off_task_session))
        .route("/api/session/{id}/resume", post(resume_session))
        .route("/api/session/{id}/skip", post(skip_session))
        .route("/api/session/{id}/hint", post(hint_session))
        .route("/api/session/{id}/next-item", post(next_item_session))
        .route("/api/session/{id}/grade-parts", post(grade_parts_session))
        .route("/api/skips/weekly", get(weekly_skips))
        .with_state(SessionState { pool, engine })
}

#[cfg(test)]
mod teaching_tests {
    use super::*;

    fn temp_db(name: &str) -> String {
        let path = std::env::temp_dir().join(format!(
            "breadstudy-teach-api-{}-{}-{}.sqlite",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_file(&path);
        path.to_string_lossy().to_string()
    }

    async fn seed_skill(pool: &SqlitePool) {
        sqlx::query(
            "INSERT INTO skills(id, strand, name, vault_note) VALUES('trigonometry','Geometry','Trigonometry','Trigonometry')",
        )
        .execute(pool)
        .await
        .expect("skill");
        for (id, prompt) in [
            ("trig-1", "Find the missing side of the right triangle."),
            ("trig-2", "Find the angle to the nearest degree."),
        ] {
            sqlx::query(
                "INSERT INTO items(id, skill_id, prompt, answer, rubric, hints, status) \
                 VALUES(?, 'trigonometry', ?, '42', '[]', '[]', 'verified')",
            )
            .bind(id)
            .bind(prompt)
            .execute(pool)
            .await
            .expect("item");
        }
    }

    #[tokio::test]
    async fn teach_context_returns_vault_note_and_sample_prompts() {
        let db = temp_db("context");
        let pool = crate::db::connect(&db).await.expect("db");
        seed_skill(&pool).await;
        let context = teach_context(&pool, "trigonometry")
            .await
            .expect("context");
        assert_eq!(context.name, "Trigonometry");
        assert_eq!(context.strand, "Geometry");
        assert_eq!(context.vault_note, "Trigonometry");
        assert_eq!(context.vault_note_path, "Maths/Trigonometry.md");
        assert!(context.sample_prompts.len() >= 3);
        assert!(context
            .sample_prompts
            .iter()
            .any(|prompt| prompt.contains("Teach me Trigonometry")));
        assert!(context
            .sample_prompts
            .iter()
            .any(|prompt| prompt.contains("Find the missing side")));
        let _ = std::fs::remove_file(&db);
    }

    #[tokio::test]
    async fn teach_context_reports_unknown_skill() {
        let db = temp_db("context-missing");
        let pool = crate::db::connect(&db).await.expect("db");
        let error = teach_context(&pool, "nope").await.expect_err("missing");
        assert!(matches!(error, TeachContextError::NotFound(_)));
        let _ = std::fs::remove_file(&db);
    }

    fn request(input: &str) -> TtsRequest {
        TtsRequest {
            input: input.to_string(),
            voice: None,
            speed: None,
            backend: None,
            model: None,
        }
    }

    #[tokio::test]
    async fn tts_rejects_empty_and_overlong_input_before_any_network_call() {
        let service = TtsService::for_test("http://127.0.0.1:1", "http://127.0.0.1:1", None);
        let empty = service.synthesize(&request("   ")).await.expect_err("empty");
        assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
        let long = "x".repeat(MAX_TTS_INPUT_CHARS + 1);
        let error = service.synthesize(&request(&long)).await.expect_err("long");
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn pcm16_is_wrapped_in_a_valid_wav_header() {
        let wav = pcm16_to_wav(&[0, 1, 2, 3], 24_000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + 4);
    }

    #[test]
    fn gemini_inline_audio_is_extracted() {
        let value = json!({
            "candidates": [{
                "content": {"parts": [{"inlineData": {"mimeType": "audio/L16;rate=24000", "data": "AAAA"}}]}
            }]
        });
        let (data, mime) = gemini_inline_audio(&value).expect("audio");
        assert_eq!(data, "AAAA");
        assert_eq!(sample_rate_from_mime(mime), 24_000);
    }

    async fn breeze_mock(fallback: bool) -> String {
        let app = Router::new().route(
            "/v1/speak",
            post(move || {
                let fallback = fallback;
                async move {
                    Json(json!({
                        "fallback": fallback,
                        "audio_base64": "UklGRg==",
                        "sample_rate": 24000,
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock breeze");
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn auto_resolution_falls_through_to_breeze() {
        let breeze = breeze_mock(false).await;
        let service = TtsService::for_test("http://127.0.0.1:1", breeze, None);
        let audio = service
            .synthesize(&request("Area equals pi r squared."))
            .await
            .expect("breeze audio");
        assert_eq!(audio.backend, "breeze");
    }

    #[tokio::test]
    async fn auto_resolution_does_not_use_silent_breeze_fallback() {
        let breeze = breeze_mock(true).await;
        let service = TtsService::for_test("http://127.0.0.1:1", breeze, None);
        let error = service
            .synthesize(&request("Area equals pi r squared."))
            .await
            .expect_err("silent fallback");
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(error.message.contains("browser speech"));
    }
}
