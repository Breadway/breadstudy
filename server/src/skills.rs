//! Skill topic view (STUDY-45): the vault note name, a short summary and
//! worked-example notes for one skill, read from the indexed item bank.
//!
//! The container does not mount the student's vault checkout, so this
//! endpoint cannot read the Markdown note itself. It returns the note name
//! (`skills.vault_note`) for the client to show or link, plus a compact
//! summary assembled from the verified items and the source excerpts the
//! indexer stored on them (`items.source_snippet`). Answers and hints are
//! deliberately omitted so opening the topic view does not spoil practice.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use sqlx::{Row, SqlitePool};

/// One verified item as a topic preview.
#[derive(Debug, Clone, Serialize)]
pub struct SkillItemPreview {
    pub item_id: String,
    pub prompt: String,
    pub source_url: Option<String>,
    pub note_excerpt: Option<String>,
}

/// Everything the skill view needs, plus what "practise this skill" scopes.
#[derive(Debug, Clone, Serialize)]
pub struct SkillDetail {
    pub id: String,
    pub strand: String,
    pub name: String,
    /// Name of the Markdown note in the vault checkout, e.g. "Trigonometry".
    pub vault_note: String,
    /// Where that note lives in the vault, for display outside the container.
    pub vault_note_path: String,
    /// One-paragraph summary derived from the item bank.
    pub summary: String,
    pub item_count: i64,
    pub note_excerpts: Vec<String>,
    pub sample_items: Vec<SkillItemPreview>,
}

#[derive(Debug)]
pub enum SkillViewError {
    NotFound(String),
    Db(sqlx::Error),
}

impl std::fmt::Display for SkillViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillViewError::NotFound(id) => write!(f, "skill {id} not found"),
            SkillViewError::Db(error) => write!(f, "{error}"),
        }
    }
}

impl From<sqlx::Error> for SkillViewError {
    fn from(error: sqlx::Error) -> Self {
        SkillViewError::Db(error)
    }
}

impl IntoResponse for SkillViewError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            SkillViewError::NotFound(id) => (
                StatusCode::NOT_FOUND,
                format!("skill {id} is not in the index"),
            ),
            SkillViewError::Db(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        };
        (
            status,
            Json(serde_json::json!({ "status": "error", "error": message })),
        )
            .into_response()
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

fn build_summary(name: &str, strand: &str, vault_note: &str, item_count: i64) -> String {
    if item_count == 0 {
        return format!(
            "{name} ({strand}) has no verified practice items yet. The vault note {vault_note} is the source to read first."
        );
    }
    format!(
        "{name} ({strand}) has {item_count} verified practice item{} in the bank, drawn from the vault note {vault_note}.",
        if item_count == 1 { "" } else { "s" }
    )
}

/// Load one skill with a compact preview of its verified items.
pub async fn skill_detail(
    pool: &SqlitePool,
    skill_id: &str,
) -> Result<SkillDetail, SkillViewError> {
    let skill = sqlx::query("SELECT id, strand, name, vault_note FROM skills WHERE id = ?")
        .bind(skill_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| SkillViewError::NotFound(skill_id.to_string()))?;

    let id: String = skill.get("id");
    let strand: String = skill.get("strand");
    let name: String = skill.get("name");
    let vault_note: String = skill.get("vault_note");

    let rows = sqlx::query(
        "SELECT id, prompt, source_url, source_snippet FROM items \
         WHERE skill_id = ? AND status = 'verified' ORDER BY id LIMIT 40",
    )
    .bind(&id)
    .fetch_all(pool)
    .await?;

    let mut note_excerpts: Vec<String> = Vec::new();
    let mut sample_items: Vec<SkillItemPreview> = Vec::new();
    for row in &rows {
        let source_url: Option<String> = row.get("source_url");
        let snippet: Option<String> = row.get("source_snippet");
        let note_excerpt = snippet
            .as_deref()
            .map(|text| truncate(text, 320))
            .filter(|text| !text.is_empty());
        if let Some(example) = &note_excerpt {
            if note_excerpts.len() < 3 && !note_excerpts.iter().any(|seen| seen == example) {
                note_excerpts.push(example.clone());
            }
        }
        if sample_items.len() < 6 {
            sample_items.push(SkillItemPreview {
                item_id: row.get("id"),
                prompt: row.get("prompt"),
                source_url,
                note_excerpt,
            });
        }
    }

    let item_count = rows.len() as i64;
    Ok(SkillDetail {
        summary: build_summary(&name, &strand, &vault_note, item_count),
        vault_note_path: format!("Maths/{vault_note}.md"),
        id,
        strand,
        name,
        vault_note,
        item_count,
        note_excerpts,
        sample_items,
    })
}

async fn api_skill_detail(
    State(pool): State<SqlitePool>,
    Path(skill_id): Path<String>,
) -> Result<Json<SkillDetail>, SkillViewError> {
    skill_detail(&pool, &skill_id).await.map(Json)
}

/// Routes for the skill topic view. Merge with the application router.
pub fn skill_router(pool: SqlitePool) -> Router {
    Router::new()
        .route("/api/skills/{skill_id}", get(api_skill_detail))
        .with_state(pool)
}
