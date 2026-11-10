//! Teacher scratchpad: the model authors the explanation board.
//!
//! A request carries a question, optionally the current item, and a short
//! transcript. The reply is an ordered list of blocks the PWA renders with
//! KaTeX and SVG. Model output is untrusted, so every block is length-capped
//! and SVG is restricted to structural and shape elements.

use std::path::PathBuf;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

const DEFAULT_BASE_URL: &str = "https://opencode.ai/zen/go/v1";
const DEFAULT_MODEL: &str = "deepseek-v4.1-flash";
const DEFAULT_KEY_FILE: &str = "/etc/breadstudy/llm.key";

const MAX_QUESTION: usize = 2000;
const MAX_TRANSCRIPT: usize = 2400;
const MAX_BLOCKS: usize = 24;
const MAX_TEXT: usize = 4000;
const MAX_SVG: usize = 20_000;
const MAX_LATEX: usize = 2000;
const MAX_ANIMATION_FRAMES: usize = 8;
const MIN_FRAME_MS: u32 = 200;
const MAX_FRAME_MS: u32 = 3000;
const MAX_CAPTION: usize = 160;

/// The skill a teaching turn is scoped to. It is resolved from the index so
/// the model is told the student's real topic and the reply can name it.
#[derive(Debug, Clone)]
pub struct SkillContext {
    pub id: String,
    pub name: String,
    pub strand: String,
    pub vault_note: String,
}

/// One rendered piece of the board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub latex: Option<String>,
    #[serde(default)]
    pub items: Option<Vec<String>>,
    #[serde(default)]
    pub svg: Option<String>,
    /// Frame sequence for `kind: "animation"`. Omitted from text, latex,
    /// steps and svg blocks so their shape is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<Frame>>,
}

/// One frame of an animation block. Each frame is a complete inline SVG that
/// continues the previous frame, so the client can swap them in place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub svg: String,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default, deserialize_with = "de_optional_ms")]
    pub duration_ms: Option<u32>,
}

/// Models sometimes send `900.0` or `"900"`. A strict u32 would reject the
/// whole board, so accept anything numeric and let the clamp decide.
fn de_optional_ms<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    let ms = match value {
        Some(serde_json::Value::Number(number)) => number.as_f64(),
        Some(serde_json::Value::String(text)) => text.trim().parse::<f64>().ok(),
        _ => None,
    };
    Ok(ms.and_then(|ms| {
        if ms.is_finite() && ms >= 0.0 {
            Some(ms.round().min(u32::MAX as f64) as u32)
        } else {
            None
        }
    }))
}

#[derive(Debug, Deserialize)]
pub struct TeachRequest {
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub history: Vec<ChatTurn>,
    /// Optional skill the student picked from the topic list. Older clients
    /// omit it and keep the original whole-board behaviour.
    #[serde(default)]
    pub skill_id: Option<String>,
    /// A short text transcript of the current teaching conversation. The
    /// client sends this instead of (or alongside) `history` so the model can
    /// keep the thread without storing server-side chat state.
    #[serde(default)]
    pub transcript: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct TeachReply {
    pub blocks: Vec<Block>,
    pub skill: Option<String>,
}

#[derive(Clone)]
pub struct TeachState {
    pub pool: SqlitePool,
    pub client: TeachClient,
}

#[derive(Clone)]
pub struct TeachClient {
    base_url: String,
    model: String,
    api_key: String,
    session_id: String,
}

impl TeachClient {
    pub fn from_env() -> anyhow::Result<Self> {
        let base_url =
            env_non_empty("BREADSTUDY_LLM_BASE_URL").unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let model =
            env_non_empty("BREADSTUDY_LLM_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_string());
        let api_key = read_api_key()?;
        let session_id = env_non_empty("BREADSTUDY_LLM_SESSION")
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        Ok(Self {
            base_url,
            model,
            api_key,
            session_id,
        })
    }

    async fn explain(
        &self,
        question: &str,
        item_prompt: Option<&str>,
        history: &[ChatTurn],
        topic: Option<&SkillContext>,
        transcript: &str,
    ) -> anyhow::Result<Vec<Block>> {
        let mut messages = vec![ChatTurn {
            role: "system".into(),
            content: SYSTEM_PROMPT.into(),
        }];
        for turn in history.iter().rev().take(6).rev() {
            let role = match turn.role.as_str() {
                "assistant" => "assistant",
                _ => "user",
            };
            messages.push(ChatTurn {
                role: role.into(),
                content: truncate(&turn.content, MAX_QUESTION),
            });
        }
        let mut user = String::new();
        if let Some(topic) = topic {
            user.push_str("Topic the student picked:\n");
            user.push_str(&topic.name);
            user.push_str(" (");
            user.push_str(&topic.strand);
            user.push_str("). Vault note: ");
            user.push_str(&topic.vault_note);
            user.push_str(".\n\n");
        }
        if let Some(prompt) = item_prompt {
            user.push_str("Current problem:\n");
            user.push_str(prompt);
            user.push_str("\n\n");
        }
        let transcript = transcript.trim();
        if !transcript.is_empty() {
            user.push_str("Recent teaching conversation, kept for continuity:\n");
            user.push_str(&truncate(transcript, MAX_TRANSCRIPT));
            user.push_str("\n\n");
        }
        user.push_str("Question:\n");
        user.push_str(&truncate(question, MAX_QUESTION));
        messages.push(ChatTurn {
            role: "user".into(),
            content: user,
        });

        let body = serde_json::json!({
            "model": self.model,
            "temperature": 0.2,
            "response_format": { "type": "json_object" },
            "messages": messages,
        });
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let response = reqwest::Client::new()
            .post(url)
            .bearer_auth(&self.api_key)
            .header("x-opencode-session", &self.session_id)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let raw = response.text().await?;
        if !status.is_success() {
            anyhow::bail!("llm returned {status}: {}", truncate(&raw, 300));
        }
        let parsed: serde_json::Value = serde_json::from_str(&raw)?;
        let content = parsed["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("llm response had no message content"))?;
        let board: Board = serde_json::from_str(content)
            .map_err(|error| anyhow::anyhow!("board was not valid JSON: {error}"))?;
        Ok(sanitize_blocks(board.blocks))
    }
}

#[derive(Debug, Deserialize)]
struct Board {
    #[serde(default)]
    blocks: Vec<Block>,
}

const SYSTEM_PROMPT: &str = "You are a patient maths tutor writing on a shared scratchpad for a Year 10 student.\n\
Return ONLY JSON: {\"blocks\":[...]} where each block is one of:\n\
{\"kind\":\"text\",\"text\":\"...\"}\n\
{\"kind\":\"latex\",\"latex\":\"...\"}  (one display equation, no surrounding $$)\n\
{\"kind\":\"steps\",\"items\":[\"...\",\"...\"]}\n\
{\"kind\":\"svg\",\"svg\":\"<svg viewBox=...>...</svg>\"}\n\
{\"kind\":\"animation\",\"frames\":[{\"svg\":\"<svg viewBox=...>...</svg>\",\"caption\":\"...\",\"duration_ms\":900}]}\n\
Rules: 4 to 12 blocks, short prose, at most 2 equations, at most 1 SVG or animation. Use an animation when motion explains the idea: unrolling a cylinder into a rectangle, sweeping a radius around a circle, rotating a triangle, or shifting a graph. An animation needs 3 to 6 frames that all use the same fixed viewBox and the same shapes, each frame continuing the previous picture instead of redrawing it from scratch. Give every frame a short caption of a few words and a duration_ms between 400 and 1500. The SVG must be a self-contained diagram using only svg, g, path, line, rect, circle, ellipse, polygon, polyline, text, tspan, marker, defs and marker elements, with viewBox set and no scripts, styles, images, foreignObject, event attributes or external references. Label diagrams with <text>. Prefer showing the shape and the substitution over restating the formula.";

/// The server-side gate. Anything that fails a rule is dropped rather than
/// rendered, and an all-dropped board is an error so the UI can say so.
pub fn sanitize_blocks(blocks: Vec<Block>) -> Vec<Block> {
    blocks
        .into_iter()
        .take(MAX_BLOCKS)
        .filter_map(|block| match block.kind.as_str() {
            "text" => {
                let text = block.text?;
                let text = truncate(&text, MAX_TEXT);
                Some(Block {
                    kind: "text".into(),
                    text: Some(text),
                    latex: None,
                    items: None,
                    svg: None,
                    frames: None,
                })
            }
            "latex" => {
                let latex = block.latex.or(block.text)?;
                Some(Block {
                    kind: "latex".into(),
                    text: None,
                    latex: Some(truncate(&latex, MAX_LATEX)),
                    items: None,
                    svg: None,
                    frames: None,
                })
            }
            "steps" => {
                let items = block.items.unwrap_or_default();
                if items.is_empty() {
                    return None;
                }
                Some(Block {
                    kind: "steps".into(),
                    text: None,
                    latex: None,
                    items: Some(
                        items
                            .into_iter()
                            .take(12)
                            .map(|item| truncate(&item, MAX_TEXT))
                            .collect(),
                    ),
                    svg: None,
                    frames: None,
                })
            }
            "svg" => {
                let svg = block.svg?;
                if !is_safe_svg(&svg) {
                    return None;
                }
                Some(Block {
                    kind: "svg".into(),
                    text: None,
                    latex: None,
                    items: None,
                    svg: Some(truncate(&svg, MAX_SVG)),
                    frames: None,
                })
            }
            "animation" => sanitize_animation(block),
            _ => None,
        })
        .collect()
}

/// An animation is all-or-nothing. Every candidate frame must pass the same
/// gate as a static SVG, otherwise the sequence is dropped and the first
/// usable frame is shown as a plain diagram so the board still renders.
fn sanitize_animation(block: Block) -> Option<Block> {
    let candidates: Vec<Frame> = block
        .frames
        .unwrap_or_default()
        .into_iter()
        .take(MAX_ANIMATION_FRAMES)
        .collect();
    let mut valid: Vec<Frame> = Vec::new();
    let mut all_valid = !candidates.is_empty();
    for frame in candidates {
        match sanitize_frame(frame) {
            Some(frame) => valid.push(frame),
            None => all_valid = false,
        }
    }
    if valid.is_empty() {
        return None;
    }
    if all_valid {
        return Some(Block {
            kind: "animation".into(),
            text: None,
            latex: None,
            items: None,
            svg: None,
            frames: Some(valid),
        });
    }
    let first = valid.remove(0);
    Some(Block {
        kind: "svg".into(),
        text: None,
        latex: None,
        items: None,
        svg: Some(first.svg),
        frames: None,
    })
}

fn sanitize_frame(frame: Frame) -> Option<Frame> {
    if frame.svg.len() > MAX_SVG || !is_safe_svg(&frame.svg) {
        return None;
    }
    let caption = frame
        .caption
        .map(|caption| truncate(caption.trim(), MAX_CAPTION))
        .filter(|caption| !caption.is_empty());
    Some(Frame {
        svg: frame.svg,
        caption,
        duration_ms: frame.duration_ms.map(|ms| ms.clamp(MIN_FRAME_MS, MAX_FRAME_MS)),
    })
}

/// Conservative check: the payload must look like a single inline SVG and
/// must not contain anything executable or externally referenced.
pub fn is_safe_svg(svg: &str) -> bool {
    let lower = svg.to_ascii_lowercase();
    if !lower.trim_start().starts_with("<svg") || !lower.contains("</svg>") {
        return false;
    }
    const BANNED: [&str; 12] = [
        "<script",
        "javascript:",
        "foreignobject",
        "<iframe",
        "<image",
        "<style",
        "<use",
        "onload",
        "onclick",
        "onmouseover",
        "xlink:href",
        "href=",
    ];
    !BANNED.iter().any(|needle| lower.contains(needle))
}

async fn teach(
    State(state): State<TeachState>,
    Json(request): Json<TeachRequest>,
) -> Result<Json<TeachReply>, (StatusCode, String)> {
    let question = request.question.trim().to_string();
    if question.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "question is required".into()));
    }

    let item_prompt: Option<String> = match request.item_id.as_deref() {
        Some(id) => {
            let row: Option<(String,)> = sqlx::query_as("SELECT prompt FROM items WHERE id = ?1")
                .bind(id)
                .fetch_optional(&state.pool)
                .await
                .map_err(internal_error)?;
            row.map(|(prompt,)| prompt)
        }
        None => None,
    };

    let topic: Option<SkillContext> = match request.skill_id.as_deref() {
        Some(id) => {
            let row: Option<(String, String, String)> = sqlx::query_as(
                "SELECT name, strand, vault_note FROM skills WHERE id = ?1",
            )
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(internal_error)?;
            match row {
                Some((name, strand, vault_note)) => Some(SkillContext {
                    id: id.to_string(),
                    name,
                    strand,
                    vault_note,
                }),
                None => {
                    return Err((
                        StatusCode::UNPROCESSABLE_ENTITY,
                        format!("unknown skill {id}"),
                    ))
                }
            }
        }
        None => None,
    };

    let blocks = state
        .client
        .explain(
            &question,
            item_prompt.as_deref(),
            &request.history,
            topic.as_ref(),
            &request.transcript,
        )
        .await
        .map_err(internal_error)?;

    if blocks.is_empty() {
        return Err((
            StatusCode::BAD_GATEWAY,
            "the model returned no usable blocks; try rephrasing".into(),
        ));
    }

    Ok(Json(TeachReply {
        blocks,
        skill: topic.map(|topic| topic.name),
    }))
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

pub fn router(pool: SqlitePool, client: TeachClient) -> Router {
    Router::new()
        .route("/api/teach", post(teach))
        .with_state(TeachState { pool, client })
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn read_api_key() -> anyhow::Result<String> {
    if let Ok(key) = std::env::var("BREADSTUDY_LLM_API_KEY") {
        if !key.trim().is_empty() {
            return Ok(key.trim().to_string());
        }
    }
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("BREADSTUDY_LLM_API_KEY_FILE") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(PathBuf::from(DEFAULT_KEY_FILE));
    if let Ok(config) = std::env::var("XDG_CONFIG_HOME") {
        candidates.push(PathBuf::from(config).join("breadstudy/opencode-go.key"));
    }
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(home).join(".config/breadstudy/opencode-go.key"));
    }
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let key = text.trim();
            if !key.is_empty() {
                return Ok(key.to_string());
            }
        }
    }
    anyhow::bail!("no LLM API key found")
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut cut = max;
    while cut > 0 && !value.is_char_boundary(cut) {
        cut -= 1;
    }
    value[..cut].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(json: &str) -> Block {
        serde_json::from_str(json).expect("block")
    }

    #[test]
    fn keeps_text_latex_and_steps() {
        let blocks = sanitize_blocks(vec![
            block(r#"{"kind":"text","text":"Volume is base area times height."}"#),
            block(r#"{"kind":"latex","latex":"V=\\pi r^2h"}"#),
            block(r#"{"kind":"steps","items":["Find r","Square it","Multiply by h"]}"#),
        ]);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[1].kind, "latex");
        assert_eq!(blocks[2].items.as_ref().map(Vec::len), Some(3));
    }

    #[test]
    fn strips_executable_svg() {
        assert!(!is_safe_svg(r#"<svg onload="alert(1)"></svg>"#));
        assert!(!is_safe_svg(r#"<svg><script>alert(1)</script></svg>"#));
        assert!(!is_safe_svg(r#"<svg><image href="http://x/y.png"/></svg>"#));
        assert!(is_safe_svg(
            r#"<svg viewBox="0 0 10 10"><circle cx="5" cy="5" r="4"/></svg>"#
        ));
    }

    #[test]
    fn drops_unknown_blocks_and_caps_length() {
        let huge = "x".repeat(MAX_TEXT + 500);
        let blocks = sanitize_blocks(vec![
            block(r#"{"kind":"video","text":"nope"}"#),
            block(&format!(r#"{{"kind":"text","text":"{huge}"}}"#)),
        ]);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text.as_ref().map(|t| t.len()), Some(MAX_TEXT));
    }

    #[test]
    fn keeps_animation_frames_and_clamps_durations() {
        let blocks = sanitize_blocks(vec![block(
            r#"{"kind":"animation","frames":[
                {"svg":"<svg viewBox=\"0 0 10 10\"><rect width=\"4\" height=\"10\"/></svg>","caption":"Start","duration_ms":10},
                {"svg":"<svg viewBox=\"0 0 10 10\"><rect width=\"7\" height=\"10\"/></svg>","caption":"Half","duration_ms":900},
                {"svg":"<svg viewBox=\"0 0 10 10\"><rect width=\"10\" height=\"10\"/></svg>","caption":"Unrolled","duration_ms":9000}
            ]}"#,
        )]);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "animation");
        let frames = blocks[0].frames.as_ref().expect("frames");
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].duration_ms, Some(MIN_FRAME_MS));
        assert_eq!(frames[1].duration_ms, Some(900));
        assert_eq!(frames[2].duration_ms, Some(MAX_FRAME_MS));
        assert_eq!(frames[2].caption.as_deref(), Some("Unrolled"));
    }

    #[test]
    fn caps_animation_frames_at_eight() {
        let frame = r#"{"svg":"<svg viewBox=\"0 0 10 10\"><circle cx=\"5\" cy=\"5\" r=\"4\"/></svg>"}"#;
        let frames = [frame; 11].join(",");
        let blocks = sanitize_blocks(vec![block(&format!(
            r#"{{"kind":"animation","frames":[{frames}]}}"#
        ))]);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].frames.as_ref().map(Vec::len), Some(MAX_ANIMATION_FRAMES));
    }

    #[test]
    fn unsafe_frame_falls_back_to_first_valid_static_svg() {
        let blocks = sanitize_blocks(vec![block(
            r#"{"kind":"animation","frames":[
                {"svg":"<svg viewBox=\"0 0 10 10\"><rect width=\"4\" height=\"10\"/></svg>","caption":"Start"},
                {"svg":"<svg onload=\"alert(1)\"></svg>","caption":"Evil"},
                {"svg":"<svg viewBox=\"0 0 10 10\"><rect width=\"10\" height=\"10\"/></svg>","caption":"End"}
            ]}"#,
        )]);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "svg");
        assert!(blocks[0].frames.is_none());
        let svg = blocks[0].svg.as_deref().expect("svg");
        assert!(svg.contains("width=\"4\""));
        assert!(!svg.contains("alert"));
    }

    #[test]
    fn animation_with_no_valid_frame_is_dropped() {
        let blocks = sanitize_blocks(vec![block(
            r#"{"kind":"animation","frames":[
                {"svg":"<svg onload=\"alert(1)\"></svg>"},
                {"svg":"<svg><script>alert(1)</script></svg>"}
            ]}"#,
        )]);
        assert!(blocks.is_empty());
    }

    #[test]
    fn board_parses_animation_and_tolerates_numeric_strings() {
        let board: Board = serde_json::from_str(
            r#"{"blocks":[
                {"kind":"text","text":"Unrolling the label"},
                {"kind":"animation","frames":[
                    {"svg":"<svg viewBox=\"0 0 10 10\"><rect width=\"4\" height=\"10\"/></svg>","caption":"Cylinder","duration_ms":900.0},
                    {"svg":"<svg viewBox=\"0 0 10 10\"><rect width=\"10\" height=\"10\"/></svg>","caption":"Rectangle","duration_ms":"1200"}
                ]}
            ]}"#,
        )
        .expect("board");
        assert_eq!(board.blocks.len(), 2);
        let frames = board.blocks[1].frames.as_ref().expect("frames");
        assert_eq!(frames[0].duration_ms, Some(900));
        assert_eq!(frames[1].duration_ms, Some(1200));
    }

    #[test]
    fn legacy_block_shape_has_no_frames_key() {
        let blocks = sanitize_blocks(vec![
            block(r#"{"kind":"text","text":"hello"}"#),
            block(r#"{"kind":"svg","svg":"<svg viewBox=\"0 0 10 10\"><circle cx=\"5\" cy=\"5\" r=\"4\"/></svg>"}"#),
        ]);
        for block in blocks {
            let json = serde_json::to_value(&block).expect("json");
            assert!(
                json.get("frames").is_none(),
                "legacy block gained a frames key: {json}"
            );
            assert!(json.get("kind").is_some());
            assert!(json.get("text").is_some());
        }
    }

    #[tokio::test]
    #[ignore = "live model call; needs BREADSTUDY_LLM_API_KEY_FILE"]
    async fn live_cylinder_animation() {
        let client = TeachClient::from_env().expect("teach client");
        let blocks = client
            .explain(
                "Show me a cylinder unrolling into a rectangle, as an animation.",
                None,
                &[],
                None,
                "",
            )
            .await
            .expect("live teach reply");
        println!(
            "LIVE_BLOCKS={}",
            serde_json::to_string(&blocks).expect("json")
        );
        assert!(
            blocks.iter().any(|block| block.kind == "animation"),
            "model returned no animation block: {blocks:?}"
        );
    }
}
