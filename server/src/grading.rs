//! Photo, typed-LaTeX and spoken-transcript grading (STUDY-17).
//!
//! An attempt is graded against the item's rubric: every method criterion
//! is marked independently and the final answer is checked separately.
//! Error tags come from a fixed set so mastery updates can aggregate them:
//! `sign-slip`, `dropped-unit`, `wrong-formula`, `algebra`, `conceptual`.
//!
//! # Models
//!
//! Grading goes through the BMNS-001 name mapper on Hestia port 7406,
//! which fronts CLIProxyAPI and exposes the canonical `Grok-4.7` id.
//! When that proxy is unavailable the local vision models
//! `Ornith-1.5-9B-Q4_K_M` and `LFM3-Omni-2B-Q4_K_M` are tried on the
//! llama.cpp router (port 7301) at reduced quality. Canonical ids come
//! from `model-registry.json`; nothing here invents a model name.
//!
//! Environment overrides:
//!
//! - `BREADSTUDY_GRADING_URL` (default `http://127.0.0.1:7406/v1`)
//! - `BREADSTUDY_GRADING_MODEL` (default `Grok-4.7`)
//! - `BREADSTUDY_GRADING_API_KEY` (optional bearer token)
//! - `BREADSTUDY_GRADING_FALLBACK_URL` (default `http://127.0.0.1:7301/v1`)
//! - `BREADSTUDY_GRADING_FALLBACK_MODELS`
//!   (default `Ornith-1.5-9B-Q4_K_M,LFM3-Omni-2B-Q4_K_M`)
//! - `BREADSTUDY_GRADING_MIN_CONFIDENCE` (default `0.75`)
//!
//! # Transcription confidence
//!
//! A photo (or a speech transcript carrying a confidence value) is only
//! graded when transcription confidence is at or above the threshold.
//! Below it the call returns [`GradeOutcome::NeedsConfirmation`] with the
//! transcription and records no attempt. The client shows the text, lets
//! the student correct it, then retries with `confirmed: true` and
//! `confirmed_text`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The fixed error-tag set understood by mastery updates.
pub const ERROR_TAGS: [&str; 5] = [
    "sign-slip",
    "dropped-unit",
    "wrong-formula",
    "algebra",
    "conceptual",
];

/// One error tag from the fixed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorTag {
    SignSlip,
    DroppedUnit,
    WrongFormula,
    Algebra,
    Conceptual,
}

impl ErrorTag {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorTag::SignSlip => "sign-slip",
            ErrorTag::DroppedUnit => "dropped-unit",
            ErrorTag::WrongFormula => "wrong-formula",
            ErrorTag::Algebra => "algebra",
            ErrorTag::Conceptual => "conceptual",
        }
    }

    /// Parse a model tag, accepting only the fixed set. Unknown tags are
    /// dropped rather than stored.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().replace('_', "-").as_str() {
            "sign-slip" => Some(ErrorTag::SignSlip),
            "dropped-unit" => Some(ErrorTag::DroppedUnit),
            "wrong-formula" => Some(ErrorTag::WrongFormula),
            "algebra" => Some(ErrorTag::Algebra),
            "conceptual" => Some(ErrorTag::Conceptual),
            _ => None,
        }
    }
}

impl std::fmt::Display for ErrorTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How the student supplied the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputKind {
    Photo,
    Latex,
    Transcript,
}

impl InputKind {
    pub fn as_str(self) -> &'static str {
        match self {
            InputKind::Photo => "photo",
            InputKind::Latex => "latex",
            InputKind::Transcript => "transcript",
        }
    }
}

/// Base64 image bytes and their media type.
#[derive(Clone)]
pub struct PhotoInput {
    pub media_type: String,
    pub base64: String,
}

/// One method criterion from the item rubric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodCriterion {
    pub id: String,
    pub description: String,
    pub marks: f32,
}

/// A parsed item rubric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rubric {
    pub method: Vec<MethodCriterion>,
    pub final_marks: f32,
    pub final_answer: String,
}

impl Rubric {
    /// Parse the JSON stored in `items.rubric`.
    ///
    /// Two shapes are accepted:
    ///
    /// - `{"method":[{"id","description","marks"}],"final":{"marks","answer"}}`
    /// - a plain array of worked-solution strings or criterion objects,
    ///   which is what the STUDY-2 gate stores (`["V = pi r^2 h = ..."]`).
    ///
    /// A missing or empty rubric still yields one method mark plus the
    /// final answer mark, so grading never silently awards nothing.
    pub fn parse(raw: &str, item_answer: &str) -> Self {
        let fallback = Self::fallback(item_answer);
        let Ok(value) = serde_json::from_str::<Value>(raw) else {
            return fallback;
        };
        match value {
            Value::Array(entries) => {
                let mut rubric = Self::fallback(item_answer);
                rubric.method.clear();
                for (index, entry) in entries.iter().enumerate() {
                    match entry {
                        Value::String(text) if !text.trim().is_empty() => {
                            rubric.method.push(MethodCriterion {
                                id: format!("step-{}", index + 1),
                                description: text.trim().to_string(),
                                marks: 1.0,
                            });
                        }
                        Value::Object(object) => {
                            apply_criterion_object(&mut rubric, object, index);
                        }
                        _ => {}
                    }
                }
                if rubric.method.is_empty() {
                    rubric.method = fallback.method;
                }
                rubric
            }
            Value::Object(object) => {
                let mut rubric = Self::fallback(item_answer);
                rubric.method.clear();
                if let Some(methods) = object.get("method").and_then(Value::as_array) {
                    for (index, entry) in methods.iter().enumerate() {
                        match entry {
                            Value::String(text) if !text.trim().is_empty() => {
                                rubric.method.push(MethodCriterion {
                                    id: format!("method-{}", index + 1),
                                    description: text.trim().to_string(),
                                    marks: 1.0,
                                });
                            }
                            Value::Object(criterion) => {
                                apply_criterion_object(&mut rubric, criterion, index);
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(final_value) = object.get("final") {
                    match final_value {
                        Value::Number(number) => {
                            if let Some(marks) = number.as_f64() {
                                rubric.final_marks = clamp_marks(marks as f32);
                            }
                        }
                        Value::Object(final_object) => {
                            if let Some(marks) = final_object.get("marks").and_then(Value::as_f64) {
                                rubric.final_marks = clamp_marks(marks as f32);
                            }
                            if let Some(answer) = final_object.get("answer").and_then(Value::as_str)
                            {
                                if !answer.trim().is_empty() {
                                    rubric.final_answer = answer.trim().to_string();
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(answer) = object.get("final_answer").and_then(Value::as_str) {
                    if !answer.trim().is_empty() {
                        rubric.final_answer = answer.trim().to_string();
                    }
                }
                if rubric.method.is_empty() {
                    rubric.method = fallback.method;
                }
                rubric
            }
            _ => fallback,
        }
    }

    fn fallback(item_answer: &str) -> Self {
        Self {
            method: vec![MethodCriterion {
                id: "method".into(),
                description: "Shows a correct method with working".into(),
                marks: 1.0,
            }],
            final_marks: 1.0,
            final_answer: item_answer.trim().to_string(),
        }
    }

    pub fn method_total(&self) -> f32 {
        self.method.iter().map(|criterion| criterion.marks).sum()
    }
}

fn clamp_marks(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 10.0)
    } else {
        0.0
    }
}

fn apply_criterion_object(
    rubric: &mut Rubric,
    object: &serde_json::Map<String, Value>,
    index: usize,
) {
    let mark_value = object
        .get("marks")
        .and_then(Value::as_f64)
        .map(|marks| clamp_marks(marks as f32))
        .unwrap_or(1.0);
    let description = object
        .get("description")
        .or_else(|| object.get("criterion"))
        .or_else(|| object.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("method");
    if kind == "final" {
        rubric.final_marks = if mark_value > 0.0 { mark_value } else { 1.0 };
        if let Some(answer) = object.get("answer").and_then(Value::as_str) {
            if !answer.trim().is_empty() {
                rubric.final_answer = answer.trim().to_string();
            }
        } else if !description.is_empty() {
            rubric.final_answer = description;
        }
        return;
    }
    if description.is_empty() {
        return;
    }
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| id.trim().to_string())
        .unwrap_or_else(|| format!("method-{}", index + 1));
    rubric.method.push(MethodCriterion {
        id,
        description,
        marks: mark_value.max(0.0),
    });
}

/// The request handed to a grader.
#[derive(Clone)]
pub struct GradeRequest {
    pub input_kind: InputKind,
    pub prompt: String,
    pub rubric: Rubric,
    /// Text answer, or the confirmed transcription of a photo.
    pub given: Option<String>,
    /// Present only when the grader must read the answer from an image.
    pub photo: Option<PhotoInput>,
}

/// One criterion verdict returned by a grader.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CriterionVerdict {
    pub id: String,
    pub awarded: bool,
    pub reason: String,
}

/// Raw grader output, before rubric validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GraderResponse {
    pub transcription: Option<String>,
    pub confidence: f32,
    pub method: Vec<CriterionVerdict>,
    pub final_correct: bool,
    pub final_answer: Option<String>,
    pub error_tags: Vec<String>,
    pub feedback: String,
    pub model: String,
    pub degraded: bool,
}

impl Default for GraderResponse {
    fn default() -> Self {
        Self {
            transcription: None,
            confidence: 0.0,
            method: Vec::new(),
            final_correct: false,
            final_answer: None,
            error_tags: Vec::new(),
            feedback: String::new(),
            model: String::new(),
            degraded: false,
        }
    }
}

pub type GradeFuture<'a> =
    Pin<Box<dyn Future<Output = anyhow::Result<GraderResponse>> + Send + 'a>>;

/// One grading backend. Tests use a deterministic fake behind this trait;
/// production uses [`HttpGrader`].
pub trait Grader: Send + Sync {
    fn grade<'a>(&'a self, request: &'a GradeRequest) -> GradeFuture<'a>;
}

/// One OpenAI-compatible inference endpoint with a canonical model id.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

/// Primary BMNS proxy grader with local reduced-quality fallbacks.
pub struct HttpGrader {
    primary: Endpoint,
    fallbacks: Vec<Endpoint>,
    client: reqwest::Client,
}

impl HttpGrader {
    pub fn new(primary: Endpoint, fallbacks: Vec<Endpoint>) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            primary,
            fallbacks,
            client,
        })
    }

    /// Read endpoint configuration from the environment.
    pub fn from_env() -> anyhow::Result<Self> {
        let base_url = std::env::var("BREADSTUDY_GRADING_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:7406/v1".into());
        let model = std::env::var("BREADSTUDY_GRADING_MODEL").unwrap_or_else(|_| "Grok-4.7".into());
        let api_key = std::env::var("BREADSTUDY_GRADING_API_KEY")
            .ok()
            .filter(|key| !key.trim().is_empty());
        let fallback_url = std::env::var("BREADSTUDY_GRADING_FALLBACK_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:7301/v1".into());
        let fallback_models = std::env::var("BREADSTUDY_GRADING_FALLBACK_MODELS")
            .unwrap_or_else(|_| "Ornith-1.5-9B-Q4_K_M,LFM3-Omni-2B-Q4_K_M".into());
        let fallbacks = fallback_models
            .split(',')
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(|model| Endpoint {
                base_url: fallback_url.clone(),
                model: model.to_string(),
                api_key: None,
            })
            .collect();
        Self::new(
            Endpoint {
                base_url,
                model,
                api_key,
            },
            fallbacks,
        )
    }

    async fn call(
        &self,
        endpoint: &Endpoint,
        request: &GradeRequest,
        reduced: bool,
    ) -> anyhow::Result<GraderResponse> {
        let prompt = build_prompt(request, reduced);
        let user_content = if let Some(photo) = &request.photo {
            json!([
                {"type": "text", "text": prompt},
                {
                    "type": "image_url",
                    "image_url": {
                        "url": format!(
                            "data:{};base64,{}",
                            photo.media_type, photo.base64
                        )
                    }
                }
            ])
        } else {
            json!(prompt)
        };
        let body = json!({
            "model": endpoint.model,
            "temperature": 0.0,
            "max_tokens": 1200,
            "messages": [
                {"role": "system", "content": "You are a precise maths marker. Return only JSON."},
                {"role": "user", "content": user_content}
            ]
        });
        let url = format!(
            "{}/chat/completions",
            endpoint.base_url.trim_end_matches('/')
        );
        let mut builder = self.client.post(&url).json(&body);
        if let Some(key) = &endpoint.api_key {
            builder = builder.bearer_auth(key);
        }
        let response = builder.send().await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            anyhow::bail!(
                "model {} returned {status}: {}",
                endpoint.model,
                truncate(&text, 240)
            );
        }
        let envelope: Value = serde_json::from_str(&text)?;
        let content = match &envelope["choices"][0]["message"]["content"] {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
            _ => anyhow::bail!("model {} returned no message content", endpoint.model),
        };
        let json_text = extract_json(&content).ok_or_else(|| {
            anyhow::anyhow!(
                "model {} did not return a JSON object: {}",
                endpoint.model,
                truncate(&content, 240)
            )
        })?;
        let mut parsed: GraderResponse = serde_json::from_str(json_text)?;
        parsed.model = endpoint.model.clone();
        parsed.degraded = reduced;
        if request.input_kind == InputKind::Photo {
            let transcription_empty = parsed
                .transcription
                .as_deref()
                .map(str::trim)
                .map(str::is_empty)
                .unwrap_or(true);
            if transcription_empty {
                parsed.transcription = Some(String::new());
                parsed.confidence = 0.0;
            } else if parsed.confidence <= 0.0 {
                // A vision model that omits confidence is not confident
                // enough to grade silently.
                parsed.confidence = 0.5;
            }
        }
        parsed.confidence = parsed.confidence.clamp(0.0, 1.0);
        Ok(parsed)
    }
}

impl Grader for HttpGrader {
    fn grade<'a>(&'a self, request: &'a GradeRequest) -> GradeFuture<'a> {
        Box::pin(async move {
            match self.call(&self.primary, request, false).await {
                Ok(response) => Ok(response),
                Err(error) => {
                    tracing::warn!(
                        model = %self.primary.model,
                        error = %error,
                        "primary grading model unavailable, trying local fallback"
                    );
                    let mut errors = vec![format!("{}: {error}", self.primary.model)];
                    for endpoint in &self.fallbacks {
                        match self.call(endpoint, request, true).await {
                            Ok(response) => return Ok(response),
                            Err(fallback_error) => {
                                errors.push(format!("{}: {fallback_error}", endpoint.model))
                            }
                        }
                    }
                    Err(anyhow::anyhow!(
                        "all grading models failed: {}",
                        errors.join("; ")
                    ))
                }
            }
        })
    }
}

fn truncate(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(limit).collect();
    out.push_str("...");
    out
}

fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end > start {
        Some(&text[start..=end])
    } else {
        None
    }
}

fn build_prompt(request: &GradeRequest, reduced: bool) -> String {
    let mut prompt = String::new();
    prompt.push_str("Mark this Year 10 Mathematics worked solution against the rubric only.\n");
    if reduced {
        prompt.push_str(
            "You are the reduced-quality local fallback. Be conservative: award a method mark only when the working clearly shows it, and never invent marks.\n",
        );
    }
    prompt.push_str(&format!("Item: {}\n", request.prompt));
    prompt.push_str("Method criteria:\n");
    for criterion in &request.rubric.method {
        prompt.push_str(&format!(
            "- id={} ({} mark): {}\n",
            criterion.id, criterion.marks, criterion.description
        ));
    }
    prompt.push_str(&format!(
        "Final answer expected: {}\n",
        request.rubric.final_answer
    ));
    if let Some(given) = &request.given {
        prompt.push_str(&format!("Student answer text:\n{given}\n"));
    }
    if request.photo.is_some() {
        prompt.push_str(
            "The student answer is the attached photo. Transcribe the handwriting faithfully before marking. If any part is illegible, set confidence below 0.75 and return your best transcription.\n",
        );
    }
    prompt.push_str(
        "Return ONLY one JSON object with keys: transcription (string), confidence (0 to 1), method (array of {id, awarded, reason} using the ids above), final_correct (boolean), final_answer (string), error_tags (array), feedback (string).\n",
    );
    prompt.push_str(&format!(
        "Allowed error_tags: {}. Use only tags you can justify from the working.\n",
        ERROR_TAGS.join(", ")
    ));
    prompt
}

/// Input accepted by the grading API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptRequest {
    pub item_id: String,
    pub input_kind: InputKind,
    #[serde(default)]
    pub latex: Option<String>,
    #[serde(default)]
    pub transcript: Option<String>,
    #[serde(default)]
    pub transcription_confidence: Option<f32>,
    #[serde(default)]
    pub photo_base64: Option<String>,
    #[serde(default)]
    pub photo_media_type: Option<String>,
    /// Corrected or confirmed transcription after a needs-confirmation reply.
    #[serde(default)]
    pub confirmed_text: Option<String>,
    /// Set once the student has seen and accepted a low-confidence transcription.
    #[serde(default)]
    pub confirmed: bool,
}

/// One marked rubric criterion, stored on the attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionResult {
    pub id: String,
    pub description: String,
    pub marks: f32,
    pub awarded: f32,
    pub reason: String,
}

/// A graded attempt as returned by the API and stored in `attempts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradedAttempt {
    pub id: String,
    pub item_id: String,
    pub input_kind: InputKind,
    pub given: String,
    pub correct: bool,
    pub method_marks: f32,
    pub method_marks_total: f32,
    pub final_marks: f32,
    pub final_marks_total: f32,
    pub error_tags: Vec<ErrorTag>,
    pub rubric_breakdown: Vec<CriterionResult>,
    pub grader_model: String,
    pub degraded: bool,
    pub transcription_confidence: Option<f32>,
    pub feedback: String,
}

/// Result of a grading request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GradeOutcome {
    Graded {
        attempt: GradedAttempt,
    },
    NeedsConfirmation {
        transcription: String,
        confidence: f32,
        input_kind: InputKind,
        message: String,
    },
}

/// Grading failures, mapped to HTTP statuses by the API layer.
#[derive(Debug, thiserror::Error)]
pub enum GradeError {
    #[error("invalid grading request: {0}")]
    Invalid(String),
    #[error("item not found: {0}")]
    ItemNotFound(String),
    #[error("all grading models failed: {0}")]
    AllGradersFailed(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("grading data error: {0}")]
    Data(#[from] serde_json::Error),
}

struct StoredItem {
    id: String,
    prompt: String,
    answer: String,
    rubric: String,
}

/// Grades attempts against item rubrics and records them.
pub struct GradingService {
    grader: Arc<dyn Grader>,
    confidence_threshold: f32,
}

impl GradingService {
    pub fn new(grader: Arc<dyn Grader>, confidence_threshold: f32) -> Self {
        Self {
            grader,
            confidence_threshold: confidence_threshold.clamp(0.0, 1.0),
        }
    }

    pub fn from_env(grader: Arc<dyn Grader>) -> Self {
        let threshold = std::env::var("BREADSTUDY_GRADING_MIN_CONFIDENCE")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(0.75);
        Self::new(grader, threshold)
    }

    pub async fn grade(
        &self,
        pool: &sqlx::SqlitePool,
        request: AttemptRequest,
    ) -> Result<GradeOutcome, GradeError> {
        let item = load_item(pool, &request.item_id).await?;
        let rubric = Rubric::parse(&item.rubric, &item.answer);
        let input_kind = request.input_kind;

        let mut given: Option<String> = None;
        let mut photo: Option<PhotoInput> = None;
        let mut stated_confidence: Option<f32> = None;

        match input_kind {
            InputKind::Latex => {
                let text = request.latex.as_deref().unwrap_or("").trim().to_string();
                if text.is_empty() {
                    return Err(GradeError::Invalid(
                        "latex input requires a non-empty latex field".into(),
                    ));
                }
                given = Some(text);
            }
            InputKind::Transcript => {
                let text = request
                    .transcript
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if text.is_empty() {
                    return Err(GradeError::Invalid(
                        "transcript input requires a non-empty transcript field".into(),
                    ));
                }
                let confidence = request
                    .transcription_confidence
                    .unwrap_or(1.0)
                    .clamp(0.0, 1.0);
                if confidence < self.confidence_threshold && !request.confirmed {
                    return Ok(GradeOutcome::NeedsConfirmation {
                        transcription: text,
                        confidence,
                        input_kind,
                        message: confirmation_message(confidence, self.confidence_threshold),
                    });
                }
                given = Some(confirmed_or_original(
                    request.confirmed_text.as_deref(),
                    &text,
                ));
            }
            InputKind::Photo => {
                let base64 = request
                    .photo_base64
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let media_type = request
                    .photo_media_type
                    .as_deref()
                    .unwrap_or("image/jpeg")
                    .trim()
                    .to_lowercase();
                if let Some(text) = request
                    .confirmed_text
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    given = Some(text.to_string());
                } else {
                    if base64.is_empty() {
                        return Err(GradeError::Invalid(
                            "photo input requires photo_base64 (or confirmed_text after confirmation)"
                                .into(),
                        ));
                    }
                    if !media_type.starts_with("image/") {
                        return Err(GradeError::Invalid(
                            "photo_media_type must be an image/* media type".into(),
                        ));
                    }
                    photo = Some(PhotoInput { media_type, base64 });
                }
            }
        }

        let grade_request = GradeRequest {
            input_kind,
            prompt: item.prompt.clone(),
            rubric: rubric.clone(),
            given: given.clone(),
            photo,
        };
        let response = self
            .grader
            .grade(&grade_request)
            .await
            .map_err(|error| GradeError::AllGradersFailed(error.to_string()))?;

        let answer_text = if let Some(text) = given {
            text
        } else {
            let transcription = response
                .transcription
                .as_deref()
                .unwrap_or("")
                .trim()
                .to_string();
            let confidence = response.confidence.clamp(0.0, 1.0);
            if confidence < self.confidence_threshold && !request.confirmed {
                return Ok(GradeOutcome::NeedsConfirmation {
                    transcription,
                    confidence,
                    input_kind,
                    message: confirmation_message(confidence, self.confidence_threshold),
                });
            }
            if transcription.is_empty() {
                return Err(GradeError::AllGradersFailed(
                    "grader returned no transcription for the photo".into(),
                ));
            }
            stated_confidence = Some(confidence);
            transcription
        };

        let attempt = persist_attempt(
            pool,
            &item,
            &rubric,
            input_kind,
            &answer_text,
            &response,
            stated_confidence,
        )
        .await?;
        Ok(GradeOutcome::Graded { attempt })
    }
}

fn confirmation_message(confidence: f32, threshold: f32) -> String {
    format!(
        "Transcription confidence {:.2} is below {:.2}. Check the text, correct it if needed, then confirm before grading.",
        confidence, threshold
    )
}

fn confirmed_or_original(confirmed: Option<&str>, original: &str) -> String {
    confirmed
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(original)
        .to_string()
}

async fn load_item(pool: &sqlx::SqlitePool, id: &str) -> Result<StoredItem, GradeError> {
    let row: Option<(String, String, String, String)> =
        sqlx::query_as("SELECT id, prompt, answer, rubric FROM items WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    let Some((id, prompt, answer, rubric)) = row else {
        return Err(GradeError::ItemNotFound(id.to_string()));
    };
    Ok(StoredItem {
        id,
        prompt,
        answer,
        rubric,
    })
}

#[allow(clippy::too_many_arguments)]
async fn persist_attempt(
    pool: &sqlx::SqlitePool,
    item: &StoredItem,
    rubric: &Rubric,
    input_kind: InputKind,
    answer_text: &str,
    response: &GraderResponse,
    transcription_confidence: Option<f32>,
) -> Result<GradedAttempt, GradeError> {
    let (breakdown, method_marks, method_total) = score_method(rubric, answer_text, response);

    let deterministic_final = final_answer_matches(answer_text, &rubric.final_answer);
    let final_correct = deterministic_final.unwrap_or(response.final_correct);
    if let Some(echo) = response.final_answer.as_deref() {
        tracing::debug!(echo, "grader final answer echo");
    }
    let final_marks = if final_correct {
        rubric.final_marks
    } else {
        0.0
    };

    let mut tags = Vec::new();
    for raw in &response.error_tags {
        if let Some(tag) = ErrorTag::parse(raw) {
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }
    if deterministic_final == Some(false)
        && looks_like_sign_slip(answer_text, &rubric.final_answer)
        && !tags.contains(&ErrorTag::SignSlip)
    {
        tags.push(ErrorTag::SignSlip);
    }
    if rubric_has_unit(&rubric.final_answer)
        && !answer_has_unit(answer_text)
        && !tags.contains(&ErrorTag::DroppedUnit)
    {
        tags.push(ErrorTag::DroppedUnit);
    }
    tags.sort();

    let id = uuid::Uuid::new_v4().to_string();
    let error_tags_json = serde_json::to_string(&tags)?;
    let breakdown_json = serde_json::to_string(&breakdown)?;
    sqlx::query(
        "INSERT INTO attempts(
            id, item_id, input_kind, given, correct, error_tags,
            method_marks, method_marks_total, final_marks, final_marks_total,
            rubric_breakdown, grader_model, degraded, transcription_confidence, feedback
         ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&item.id)
    .bind(input_kind.as_str())
    .bind(answer_text)
    .bind(final_correct as i64)
    .bind(&error_tags_json)
    .bind(method_marks as f64)
    .bind(method_total as f64)
    .bind(final_marks as f64)
    .bind(rubric.final_marks as f64)
    .bind(&breakdown_json)
    .bind(&response.model)
    .bind(response.degraded as i64)
    .bind(transcription_confidence.map(|value| value as f64))
    .bind(&response.feedback)
    .execute(pool)
    .await?;

    Ok(GradedAttempt {
        id,
        item_id: item.id.clone(),
        input_kind,
        given: answer_text.to_string(),
        correct: final_correct,
        method_marks,
        method_marks_total: method_total,
        final_marks,
        final_marks_total: rubric.final_marks,
        error_tags: tags,
        rubric_breakdown: breakdown,
        grader_model: response.model.clone(),
        degraded: response.degraded,
        transcription_confidence,
        feedback: response.feedback.clone(),
    })
}

fn score_method(
    rubric: &Rubric,
    answer_text: &str,
    response: &GraderResponse,
) -> (Vec<CriterionResult>, f32, f32) {
    let mut results = Vec::new();
    let mut awarded_total = 0.0;
    let mut marks_total = 0.0;
    for criterion in &rubric.method {
        marks_total += criterion.marks;
        let verdict = response
            .method
            .iter()
            .find(|verdict| verdict.id == criterion.id);
        let (awarded, reason) = match verdict {
            Some(verdict) => {
                let awarded = if verdict.awarded {
                    criterion.marks
                } else {
                    0.0
                };
                (awarded, verdict.reason.clone())
            }
            None if response.degraded => {
                // Local fallbacks often omit per-criterion output. Fall
                // back to deterministic keyword evidence at reduced quality.
                if keyword_match(&criterion.description, answer_text) {
                    (criterion.marks, "reduced-quality keyword match".into())
                } else {
                    (0.0, "reduced-quality: no evidence of this step".into())
                }
            }
            None => (0.0, "grader did not assess this criterion".into()),
        };
        awarded_total += awarded;
        results.push(CriterionResult {
            id: criterion.id.clone(),
            description: criterion.description.clone(),
            marks: criterion.marks,
            awarded,
            reason,
        });
    }
    (results, awarded_total, marks_total)
}

fn keyword_match(description: &str, answer_text: &str) -> bool {
    const STOPWORDS: [&str; 12] = [
        "uses",
        "use",
        "shows",
        "show",
        "applies",
        "apply",
        "correct",
        "correctly",
        "the",
        "and",
        "with",
        "that",
    ];
    let haystack = fold(answer_text);
    let keywords: Vec<String> = description
        .split(|c: char| !c.is_alphanumeric() && c != '^')
        .map(str::to_lowercase)
        .filter(|token| token.len() >= 2 && !STOPWORDS.contains(&token.as_str()))
        .collect();
    if keywords.is_empty() {
        return answer_text.trim().len() > 4;
    }
    let hits = keywords
        .iter()
        .filter(|keyword| haystack.contains(keyword.as_str()))
        .count();
    hits * 2 >= keywords.len()
}

fn fold(text: &str) -> String {
    text.to_lowercase()
        .replace('π', "pi")
        .replace('×', "*")
        .replace('−', "-")
        .replace('÷', "/")
        .replace('≈', "=")
        .replace('√', "sqrt")
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\\')
        .collect()
}

fn strip_units(text: &str) -> String {
    let mut out = text.to_lowercase();
    for unit in [
        "cm^3", "cm3", "cm^2", "cm2", "m^3", "m3", "m^2", "m2", "mm", "km", "kg", "ml",
    ] {
        out = out.replace(unit, " ");
    }
    out
}

fn numbers(text: &str) -> Vec<f64> {
    let text = strip_units(text);
    let chars: Vec<char> = text.chars().collect();
    let mut values = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index].is_ascii_digit() && (index == 0 || chars[index - 1] != '^') {
            let negative = index > 0
                && chars[index - 1] == '-'
                && (index < 2 || !chars[index - 2].is_ascii_digit());
            let mut end = index;
            while end < chars.len() && (chars[end].is_ascii_digit() || chars[end] == '.') {
                end += 1;
            }
            let token: String = chars[index..end].iter().collect();
            if let Ok(mut value) = token.parse::<f64>() {
                if negative {
                    value = -value;
                }
                values.push(value);
            }
            index = end;
        } else {
            index += 1;
        }
    }
    values
}

fn last_number(text: &str) -> Option<f64> {
    numbers(text).into_iter().last()
}

fn final_answer_matches(given: &str, expected: &str) -> Option<bool> {
    if let (Some(given_value), Some(expected_value)) = (last_number(given), last_number(expected)) {
        let tolerance = 0.05 + expected_value.abs() * 0.002;
        return Some((given_value - expected_value).abs() <= tolerance);
    }
    let given_folded = fold(given);
    let expected_folded = fold(expected);
    if expected_folded.len() < 3 {
        return None;
    }
    Some(given_folded.contains(&expected_folded) || expected_folded.contains(&given_folded))
}

fn looks_like_sign_slip(given: &str, expected: &str) -> bool {
    match (last_number(given), last_number(expected)) {
        (Some(given_value), Some(expected_value)) => {
            let tolerance = 0.05 + expected_value.abs() * 0.002;
            given_value.abs() > tolerance
                && expected_value.abs() > tolerance
                && (given_value.abs() - expected_value.abs()).abs() <= tolerance
                && given_value.signum() != expected_value.signum()
        }
        _ => false,
    }
}

fn rubric_has_unit(expected: &str) -> bool {
    answer_has_unit(expected)
}

fn answer_has_unit(text: &str) -> bool {
    let folded = text.to_lowercase();
    [
        "cm", "mm", "km", "kg", "ml", "m2", "m3", "m^2", "m^3", "cm2", "cm3", "cm^2", "cm^3",
    ]
    .iter()
    .any(|unit| folded.contains(unit))
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parses_object_rubric() {
        let rubric = Rubric::parse(
            r#"{"method":[{"id":"formula","description":"Uses V = pi r^2 h","marks":1},{"id":"substitute","description":"Substitutes r = 3, h = 7","marks":1}],"final":{"marks":1,"answer":"63pi cm^3"}}"#,
            "197.9 cm3",
        );
        assert_eq!(rubric.method.len(), 2);
        assert_eq!(rubric.method[0].id, "formula");
        assert_eq!(rubric.method_total(), 2.0);
        assert_eq!(rubric.final_answer, "63pi cm^3");
    }

    #[test]
    fn parses_legacy_worked_steps() {
        let rubric = Rubric::parse(r#"["V = pi r^2 h = 63pi","V ≈ 197.9 cm^3"]"#, "197.9 cm3");
        assert_eq!(rubric.method.len(), 2);
        assert_eq!(rubric.method[0].id, "step-1");
        assert_eq!(rubric.final_answer, "197.9 cm3");
    }

    #[test]
    fn empty_rubric_still_has_method_and_final_marks() {
        let rubric = Rubric::parse("[]", "10 cm2");
        assert_eq!(rubric.method.len(), 1);
        assert_eq!(rubric.method_total(), 1.0);
        assert_eq!(rubric.final_marks, 1.0);
    }

    #[test]
    fn extracts_last_numeric_value_without_units() {
        assert_eq!(last_number("V = 63pi ≈ 197.9 cm^3"), Some(197.9));
        assert_eq!(last_number("r^2 = 9, h = 7"), Some(7.0));
        assert_eq!(last_number("-197.9 cm3"), Some(-197.9));
    }

    #[test]
    fn answer_matching_is_numeric_with_tolerance() {
        assert_eq!(
            final_answer_matches("V = 160*pi ≈ 502.7 cm^3", "502.7 cm3"),
            Some(true)
        );
        assert_eq!(
            final_answer_matches("V = -197.9 cm^3", "197.9 cm^3"),
            Some(false)
        );
    }

    #[test]
    fn unknown_tags_are_dropped() {
        assert_eq!(ErrorTag::parse("sign_slip"), Some(ErrorTag::SignSlip));
        assert_eq!(ErrorTag::parse("made-up"), None);
    }
}
