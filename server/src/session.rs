//! Server-driven session state machine (STUDY-16).
//!
//! The server is the authority for phase transitions and timer expiry.
//! A session follows one of two templates:
//!
//! - [`Template::SchoolDay20`]: 20 minutes, mapped onto spec/session.md
//!   (`recall` 0-2, `practice` 2-12, `explain` 12-17, reflect/capture
//!   17-20). `prime` and `break` are present in the phase list but have
//!   zero duration, so the session starts in `recall` and ends after
//!   `reflect`.
//! - [`Template::Weekend40`]: 40 minutes with a real warm-up and a
//!   break at 18-22 minutes: `prime` 0-3, `recall` 3-6, `practice`
//!   6-18, `break` 18-22, `explain` 22-37 (block B, different strand),
//!   `reflect` 37-40. The break placement follows spec/session.md's
//!   weekend template, which places it between block A and block B.
//!
//! Content is resolved once at session start from the item bank and
//! stored as `content_json`. Hints and pre-rendered audio paths come
//! from that cache; this module never calls a model. Live explanations
//! stay outside the session engine.
//!
//! Hint progress is not stored on the run. `hint_state` is the single
//! source of truth, keyed by session id and item, so every run starts at
//! zero and two sessions never share a cursor: [`hint`] advances it through
//! [`fsrs::next_hint_for_session`] and the rendered view mirrors the current
//! level as `hint_unlocked` while always shipping the full ladder. Graded
//! attempts go through [`grade`], which calls [`fsrs::record_attempt`] so
//! mastery, due dates and error patterns update from real sessions.
//!
//! FSRS due-ness is an optional narrow boundary: [`SessionEngine`]
//! accepts a `fn(&str) -> Option<f64>` scorer. When the FSRS module is
//! not present (or returns `None`) items fall back to bank order, so
//! this module compiles and runs without it. Mastery recording is not
//! optional because it is the point of a graded attempt.
//!
//! An append-only event log (`session_events`) records every phase,
//! skip, pause, hint and item change. Other modules read it through
//! [`events`] and [`weekly_skip_count`].
//!
//! Selection keeps FSRS due-ness as the primary key, then prefers items
//! not served in the trailing seven days and falls back to the least
//! recently served item (`served_items`). Multi-part items carry their
//! parts and mark scheme through the same cache: [`grade_parts`] scores
//! each part and totals the marks, while the single-part [`grade`] path
//! is unchanged.

use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::fsrs;

/// A phase in the session state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Prime,
    Recall,
    Practice,
    Explain,
    Reflect,
    Break,
    End,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Prime => "prime",
            Phase::Recall => "recall",
            Phase::Practice => "practice",
            Phase::Explain => "explain",
            Phase::Reflect => "reflect",
            Phase::Break => "break",
            Phase::End => "end",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "prime" => Some(Phase::Prime),
            "recall" => Some(Phase::Recall),
            "practice" => Some(Phase::Practice),
            "explain" => Some(Phase::Explain),
            "reflect" => Some(Phase::Reflect),
            "break" => Some(Phase::Break),
            "end" => Some(Phase::End),
            _ => None,
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One phase duration in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseSpec {
    pub phase: Phase,
    pub seconds: u32,
}

/// The 20 minute school-day template from spec/session.md.
pub const SCHOOL_DAY_PLAN: [PhaseSpec; 7] = [
    PhaseSpec {
        phase: Phase::Prime,
        seconds: 0,
    },
    PhaseSpec {
        phase: Phase::Recall,
        seconds: 2 * 60,
    },
    PhaseSpec {
        phase: Phase::Practice,
        seconds: 10 * 60,
    },
    PhaseSpec {
        phase: Phase::Explain,
        seconds: 5 * 60,
    },
    PhaseSpec {
        phase: Phase::Reflect,
        seconds: 3 * 60,
    },
    PhaseSpec {
        phase: Phase::Break,
        seconds: 0,
    },
    PhaseSpec {
        phase: Phase::End,
        seconds: 0,
    },
];

/// The 40 minute weekend template from spec/session.md.
pub const WEEKEND_PLAN: [PhaseSpec; 7] = [
    PhaseSpec {
        phase: Phase::Prime,
        seconds: 3 * 60,
    },
    PhaseSpec {
        phase: Phase::Recall,
        seconds: 3 * 60,
    },
    PhaseSpec {
        phase: Phase::Practice,
        seconds: 12 * 60,
    },
    // Break at 18-22 minutes, between block A and block B.
    PhaseSpec {
        phase: Phase::Break,
        seconds: 4 * 60,
    },
    PhaseSpec {
        phase: Phase::Explain,
        seconds: 15 * 60,
    },
    PhaseSpec {
        phase: Phase::Reflect,
        seconds: 3 * 60,
    },
    PhaseSpec {
        phase: Phase::End,
        seconds: 0,
    },
];

/// A session template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    SchoolDay20,
    Weekend40,
}

impl Template {
    pub fn as_str(self) -> &'static str {
        match self {
            Template::SchoolDay20 => "school_day_20",
            Template::Weekend40 => "weekend_40",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "school_day_20" | "school_day" | "school-day" | "20" => Some(Template::SchoolDay20),
            "weekend_40" | "weekend" | "weekend-40" | "40" => Some(Template::Weekend40),
            _ => None,
        }
    }

    pub fn plan(self) -> &'static [PhaseSpec] {
        match self {
            Template::SchoolDay20 => &SCHOOL_DAY_PLAN,
            Template::Weekend40 => &WEEKEND_PLAN,
        }
    }

    pub fn total_seconds(self) -> u32 {
        self.plan().iter().map(|spec| spec.seconds).sum()
    }
}

/// Optional narrow FSRS boundary: `skill_id -> due-ness score`.
///
/// Higher scores are more due. The engine only needs this function; it
/// never depends on the FSRS implementation or its types.
pub type DueScoreFn = fn(&str) -> Option<f64>;

/// Optional due-ness scorer injected by the caller. A plain function
/// pointer is enough for tests and the planner routes; the live session
/// router uses [`SessionEngine::fsrs`], which loads the scores from the
/// scheduler at the start of every run.
#[derive(Clone, Copy, Default)]
pub struct SessionEngine {
    due_score: Option<DueScoreFn>,
    fsrs: bool,
}

impl SessionEngine {
    pub fn new() -> Self {
        Self {
            due_score: None,
            fsrs: false,
        }
    }

    pub fn with_due_score(due_score: DueScoreFn) -> Self {
        Self {
            due_score: Some(due_score),
            fsrs: false,
        }
    }

    /// Use the FSRS scheduler: `start` loads every skill's due score from
    /// `skill_schedule` before selecting the run's content.
    pub fn fsrs() -> Self {
        Self {
            due_score: None,
            fsrs: true,
        }
    }

    fn uses_fsrs(self) -> bool {
        self.fsrs
    }

    fn score(self, skill_id: &str, due_scores: &std::collections::HashMap<String, f64>) -> f64 {
        self.due_score
            .and_then(|f| f(skill_id))
            .or_else(|| due_scores.get(skill_id).copied())
            .filter(|score| score.is_finite())
            .unwrap_or(0.0)
    }
}

/// Options for starting a session.
#[derive(Clone, Copy)]
pub struct StartOptions {
    pub template: Template,
    pub grace_tokens: u32,
    pub engine: SessionEngine,
}

impl StartOptions {
    pub fn new(template: Template) -> Self {
        Self {
            template,
            grace_tokens: 1,
            engine: SessionEngine::new(),
        }
    }
}

impl Default for StartOptions {
    fn default() -> Self {
        Self::new(Template::SchoolDay20)
    }
}

/// One part of a multi-part exam question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemPart {
    pub label: String,
    pub prompt: String,
    pub answer: String,
    pub marks: f32,
    #[serde(default)]
    pub rubric: serde_json::Value,
}

/// A part as exposed to the client. The expected answer and mark scheme
/// stay server-side until the parts are graded.
#[derive(Debug, Clone, Serialize)]
pub struct ItemPartView {
    pub label: String,
    pub prompt: String,
    pub marks: f32,
    pub rubric: serde_json::Value,
}

/// One item cached into the session at start time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedItem {
    pub phase: Phase,
    pub id: String,
    pub skill_id: String,
    pub strand: String,
    pub prompt: String,
    pub hints: Vec<String>,
    pub audio_path: Option<String>,
    /// Empty for a single-part item. Older `content_json` rows have no
    /// field at all, so the default keeps a resumed run readable.
    #[serde(default)]
    pub parts: Vec<ItemPart>,
    #[serde(default)]
    pub mark_scheme: Option<String>,
}

/// The cached item as served to the client. The full authored ladder ships
/// with the item; `hint_unlocked` and `hint_count` tell the client how many
/// rungs the student may currently see.
#[derive(Debug, Clone, Serialize)]
pub struct ItemView {
    pub id: String,
    pub skill_id: String,
    pub strand: String,
    pub prompt: String,
    pub audio_path: Option<String>,
    pub hints: Vec<String>,
    pub hint_count: usize,
    pub hint_unlocked: usize,
    /// Empty for the existing single-part flow.
    pub parts: Vec<ItemPartView>,
    /// Whole-question mark scheme, when this is a multi-part item.
    pub mark_scheme: Option<String>,
    /// The last answer recorded in this run for the explain question.
    pub teach_answer: Option<String>,
}

/// Per-phase cached item totals. `remaining` includes the current item
/// and counts only items cached for that phase.
#[derive(Debug, Clone, Serialize)]
pub struct PhaseItemCount {
    pub phase: Phase,
    pub total: usize,
    pub remaining: usize,
}

/// One phase in the rendered plan.
#[derive(Debug, Clone, Serialize)]
pub struct PhaseView {
    pub phase: Phase,
    pub planned_seconds: u32,
    pub remaining_seconds: u32,
    pub done: bool,
}

/// The state the client renders. The server always computes this from
/// the persisted run and the current wall clock.
#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    pub id: String,
    pub template: String,
    pub phase: Phase,
    pub phase_index: usize,
    pub phase_remaining_seconds: u32,
    pub total_remaining_seconds: u32,
    pub paused: bool,
    pub paused_seconds: u32,
    pub off_task_pauses: u32,
    pub skip_count: u32,
    pub grace_tokens: u32,
    pub hints_used: u32,
    /// Rungs of the current item's ladder unlocked in this session run.
    pub hint_unlocked: usize,
    /// Number of items cached into this run, and how many are still
    /// available in the current and future phases.
    pub cached_items_total: usize,
    pub cached_items_remaining: usize,
    /// Per-phase total/remaining counts so the client can tell a phase is
    /// exhausted without calling `/next-item`.
    pub phase_items: Vec<PhaseItemCount>,
    pub item: Option<ItemView>,
    pub plan: Vec<PhaseView>,
    pub started_at: String,
    pub updated_at: String,
    pub ended_at: Option<String>,
    pub outcome: Option<String>,
}

/// One append-only session event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub id: i64,
    pub session_id: String,
    pub at: String,
    pub kind: String,
    pub phase: Phase,
    pub detail: serde_json::Value,
}

/// Result of an off-task report.
#[derive(Debug, Clone, Serialize)]
pub struct OffTaskOutcome {
    pub paused: bool,
    pub nudge: Option<String>,
    pub session: SessionView,
}

/// Result of a hint request.
#[derive(Debug, Clone, Serialize)]
pub struct HintOutcome {
    pub hint: Option<String>,
    pub hints_used: u32,
    pub session: SessionView,
}

/// Result of a skip.
#[derive(Debug, Clone, Serialize)]
pub struct SkipOutcome {
    pub session: SessionView,
    pub weekly_skip_count: u32,
    pub grace_used: bool,
}

/// Result of grading one attempt. The FSRS outcome carries the mastery,
/// rating and error-pattern evidence the session view reflects.
#[derive(Debug, Clone, Serialize)]
pub struct GradeOutcome {
    pub attempt: fsrs::AttemptOutcome,
    pub session: SessionView,
}

/// One submitted part answer.
#[derive(Debug, Clone, Deserialize)]
pub struct PartAnswer {
    pub label: String,
    pub given: String,
}

/// Payload for grading a multi-part question inside a session.
#[derive(Debug, Clone, Deserialize)]
pub struct MultiPartRequest {
    pub item_id: String,
    #[serde(default)]
    pub parts: Vec<PartAnswer>,
}

/// One marked part.
#[derive(Debug, Clone, Serialize)]
pub struct PartGrade {
    pub label: String,
    pub prompt: String,
    pub given: Option<String>,
    pub expected: String,
    pub correct: bool,
    pub awarded: f32,
    pub marks: f32,
    pub rubric: serde_json::Value,
}

/// Result of grading every part of a multi-part question. `awarded_marks`
/// and `total_marks` are the part totals; the single FSRS attempt still
/// records the skill outcome.
#[derive(Debug, Clone, Serialize)]
pub struct MultiPartGradeOutcome {
    pub item_id: String,
    pub parts: Vec<PartGrade>,
    pub awarded_marks: f32,
    pub total_marks: f32,
    pub correct: bool,
    pub attempt: fsrs::AttemptOutcome,
    pub session: SessionView,
}

#[derive(Debug, Clone)]
struct BankItem {
    id: String,
    skill_id: String,
    strand: String,
    prompt: String,
    hints: Vec<String>,
    audio_path: Option<String>,
    parts: Vec<ItemPart>,
    mark_scheme: Option<String>,
    last_served: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
struct Run {
    id: String,
    template: Template,
    phase_index: usize,
    phase_remaining_seconds: u32,
    paused: bool,
    paused_at: Option<DateTime<Utc>>,
    total_paused_seconds: u32,
    off_task_pauses: u32,
    skip_count: u32,
    grace_tokens: u32,
    item_cursor: i64,
    /// The practise item carried into the explain phase (STUDY-108).
    practice_item_id: Option<String>,
    /// Latest answer recorded for the explain question, read from the
    /// session event log. Not persisted on the run.
    teach_answer: Option<String>,
    /// Mirror of `hint_state.level` for the current item, refreshed from
    /// the database on every load. Never persisted on the run.
    hint_level: i64,
    content: Vec<CachedItem>,
    started_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    outcome: Option<String>,
}

impl Run {
    fn phase(&self) -> Phase {
        self.template.plan()[self.phase_index].phase
    }

    fn view(&self, now: DateTime<Utc>) -> SessionView {
        let plan = self.template.plan();
        let mut total_remaining = 0u32;
        let mut phases = Vec::with_capacity(plan.len());
        for (index, spec) in plan.iter().enumerate() {
            let remaining = if index < self.phase_index {
                0
            } else if index == self.phase_index {
                self.phase_remaining_seconds
            } else {
                spec.seconds
            };
            if index >= self.phase_index {
                total_remaining = total_remaining.saturating_add(remaining);
            }
            phases.push(PhaseView {
                phase: spec.phase,
                planned_seconds: spec.seconds,
                remaining_seconds: remaining,
                done: index < self.phase_index || self.ended_at.is_some(),
            });
        }
        let paused_extra = if self.paused {
            self.paused_at
                .map(|paused_at| (now - paused_at).num_seconds().max(0) as u32)
                .unwrap_or(0)
        } else {
            0
        };
        let hint_level = self.hint_level.max(0) as usize;
        let item = current_cached(self).map(|cached| {
            let parts = cached
                .parts
                .iter()
                .map(|part| ItemPartView {
                    label: part.label.clone(),
                    prompt: part.prompt.clone(),
                    marks: part.marks,
                    rubric: part.rubric.clone(),
                })
                .collect();
            ItemView {
                id: cached.id.clone(),
                skill_id: cached.skill_id.clone(),
                strand: cached.strand.clone(),
                prompt: cached.prompt.clone(),
                audio_path: cached.audio_path.clone(),
                hints: cached.hints.clone(),
                hint_count: cached.hints.len(),
                hint_unlocked: hint_level.min(cached.hints.len()),
                parts,
                mark_scheme: cached.mark_scheme.clone(),
                teach_answer: if self.phase() == Phase::Explain {
                    self.teach_answer.clone()
                } else {
                    None
                },
            }
        });
        let phase_items = phase_item_counts(self);
        let cached_items_remaining = phase_items.iter().map(|count| count.remaining).sum();
        SessionView {
            id: self.id.clone(),
            template: self.template.as_str().to_string(),
            phase: self.phase(),
            phase_index: self.phase_index,
            phase_remaining_seconds: self.phase_remaining_seconds,
            total_remaining_seconds: total_remaining,
            paused: self.paused,
            paused_seconds: self.total_paused_seconds.saturating_add(paused_extra),
            off_task_pauses: self.off_task_pauses,
            skip_count: self.skip_count,
            grace_tokens: self.grace_tokens,
            hints_used: hint_level as u32,
            hint_unlocked: hint_level,
            cached_items_total: self.content.len(),
            cached_items_remaining,
            phase_items,
            item,
            plan: phases,
            started_at: encode_ts(self.started_at),
            updated_at: encode_ts(self.updated_at),
            ended_at: self.ended_at.map(encode_ts),
            outcome: self.outcome.clone(),
        }
    }
}

fn encode_ts(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn decode_ts(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

fn first_content_index(content: &[CachedItem], phase: Phase) -> i64 {
    content
        .iter()
        .position(|item| item.phase == phase)
        .map(|index| index as i64)
        .unwrap_or(-1)
}

fn current_cached(run: &Run) -> Option<&CachedItem> {
    // The explain phase continues the practise question (STUDY-108). The
    // cursor points at the practise item, whose cached phase stays
    // `Practice`; find it by id and return it for explain.
    if run.phase() == Phase::Explain {
        if let Some(practice_id) = run.practice_item_id.as_deref() {
            if let Some(item) = run
                .content
                .iter()
                .find(|item| item.id == practice_id && item.phase != Phase::Explain)
            {
                let cursor_item = run
                    .content
                    .get(run.item_cursor.max(0) as usize)
                    .map(|cached| cached.phase == Phase::Explain)
                    .unwrap_or(false);
                if !cursor_item {
                    return Some(item);
                }
            }
        }
    }
    if run.item_cursor >= 0 {
        if let Some(item) = run.content.get(run.item_cursor as usize) {
            if item.phase == run.phase() {
                return Some(item);
            }
        }
    }
    run.content.iter().find(|item| item.phase == run.phase())
}

/// Next cached item in the current phase after the cursor, if any.
fn next_index_in_phase(run: &Run) -> Option<usize> {
    let phase = run.phase();
    let start = (run.item_cursor + 1).max(0) as usize;
    run.content
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, item)| item.phase == phase)
        .map(|(index, _)| index)
}

/// Total and remaining cached items per phase. `remaining` counts the
/// current item plus every later item cached for that phase, and is zero
/// once the phase has passed.
fn phase_item_counts(run: &Run) -> Vec<PhaseItemCount> {
    let cursor = run.item_cursor.max(0) as usize;
    run.template
        .plan()
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let total = run
                .content
                .iter()
                .filter(|item| item.phase == spec.phase)
                .count();
            let remaining = if index < run.phase_index {
                0
            } else if index > run.phase_index {
                total
            } else {
                run.content
                    .iter()
                    .enumerate()
                    .skip(cursor)
                    .filter(|(_, item)| item.phase == spec.phase)
                    .count()
            };
            PhaseItemCount {
                phase: spec.phase,
                total,
                remaining,
            }
        })
        .collect()
}

/// Read the ladder cursor for the run's current item. `hint_state` owns
/// this value, keyed by session id; the run only mirrors it for rendering.
/// Latest answer recorded in this run for `item_id`, read from the
/// append-only attempt events. Explain shows it as the teach field
/// (STUDY-108).
async fn last_answer_for(
    pool: &SqlitePool,
    session_id: &str,
    item_id: &str,
) -> anyhow::Result<Option<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT detail FROM session_events \
         WHERE session_id = ? AND kind = 'attempt' AND detail LIKE '%' || ? || '%' \
         ORDER BY id DESC LIMIT 5",
    )
    .bind(session_id)
    .bind(item_id)
    .fetch_all(pool)
    .await?;
    for raw in rows {
        let Ok(detail) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if detail.get("item_id").and_then(|value| value.as_str()) != Some(item_id) {
            continue;
        }
        if let Some(given) = detail.get("given").and_then(|value| value.as_str()) {
            return Ok(Some(given.to_string()));
        }
    }
    Ok(None)
}

async fn refresh_teach_answer(pool: &SqlitePool, run: &mut Run) -> anyhow::Result<()> {
    run.teach_answer = None;
    if run.phase() != Phase::Explain {
        return Ok(());
    }
    let Some(item_id) = current_cached(run).map(|item| item.id.clone()) else {
        return Ok(());
    };
    run.teach_answer = last_answer_for(pool, &run.id, &item_id).await?;
    Ok(())
}

async fn hint_level_for(pool: &SqlitePool, run: &Run) -> anyhow::Result<i64> {
    let Some(item) = current_cached(run) else {
        return Ok(0);
    };
    let level: Option<i64> =
        sqlx::query_scalar("SELECT level FROM hint_state WHERE session_id = ? AND item_id = ?")
            .bind(&run.id)
            .bind(&item.id)
            .fetch_optional(pool)
            .await?;
    Ok(level.unwrap_or(0).max(0))
}

/// Replace the cached hint text with the authoritative ladder. The cache
/// exists so a session does not re-read items mid-run, but `hint_state`
/// and `items.hints` decide what the rungs actually are.
async fn sync_cached_ladder(pool: &SqlitePool, run: &mut Run, item_id: &str) -> anyhow::Result<()> {
    let ladder = fsrs::hint_ladder_for_session(pool, item_id, &run.id).await?;
    for cached in &mut run.content {
        if cached.id == item_id {
            cached.hints = ladder.levels.clone();
        }
    }
    Ok(())
}

async fn load_bank(pool: &SqlitePool) -> anyhow::Result<Vec<BankItem>> {
    // No created_at bias: selection order comes from FSRS due scores, and
    // id is only a stable tie-break inside one score group.
    let rows = sqlx::query(
        "SELECT i.id, i.skill_id, COALESCE(s.strand, '') AS strand, i.prompt, i.hints, \
         i.mark_scheme, a.path AS audio_path \
         FROM items i \
         LEFT JOIN skills s ON s.id = i.skill_id \
         LEFT JOIN item_audio a ON a.item_id = i.id \
         WHERE i.status = 'verified' \
         ORDER BY i.id",
    )
    .fetch_all(pool)
    .await?;

    let mut parts_by_item: std::collections::HashMap<String, Vec<ItemPart>> =
        std::collections::HashMap::new();
    let part_rows = sqlx::query(
        "SELECT item_id, label, prompt, answer, marks, rubric \
         FROM item_parts ORDER BY item_id, ordinal",
    )
    .fetch_all(pool)
    .await?;
    for row in part_rows {
        let rubric = serde_json::from_str::<serde_json::Value>(&row.get::<String, _>("rubric"))
            .unwrap_or(serde_json::Value::Null);
        parts_by_item
            .entry(row.get("item_id"))
            .or_default()
            .push(ItemPart {
                label: row.get("label"),
                prompt: row.get("prompt"),
                answer: row.get("answer"),
                marks: row.get::<f64, _>("marks") as f32,
                rubric,
            });
    }

    let mut last_served: std::collections::HashMap<String, DateTime<Utc>> =
        std::collections::HashMap::new();
    let served_rows = sqlx::query(
        "SELECT item_id, MAX(served_at) AS last_served FROM served_items GROUP BY item_id",
    )
    .fetch_all(pool)
    .await?;
    for row in served_rows {
        if let Some(at) = decode_ts(&row.get::<String, _>("last_served")) {
            last_served.insert(row.get("item_id"), at);
        }
    }

    Ok(rows
        .into_iter()
        .map(|row| {
            let id: String = row.get("id");
            BankItem {
                parts: parts_by_item.remove(&id).unwrap_or_default(),
                last_served: last_served.get(&id).copied(),
                id,
                skill_id: row.get("skill_id"),
                strand: row.get("strand"),
                prompt: row.get("prompt"),
                hints: serde_json::from_str(&row.get::<String, _>("hints")).unwrap_or_default(),
                audio_path: row.get("audio_path"),
                mark_scheme: row.get("mark_scheme"),
            }
        })
        .collect())
}

/// Order by due score, then by recency of service, then rotate inside each
/// otherwise-equal group so a repeated session over the same bank moves on
/// instead of serving the same items forever.
///
/// Recency is the second key, never the first: FSRS due-ness still decides
/// between skills. Within one due score, an item not served in the trailing
/// seven days comes first, then the least recently served item. Items that
/// have never been served keep the old rotation behaviour, which is why a
/// fresh bank still behaves exactly as before. Without a due scorer every
/// skill scores zero, so recency and rotation alone order the bank instead
/// of replaying bank order forever.
fn sort_due(
    items: &mut [&BankItem],
    engine: SessionEngine,
    due_scores: &std::collections::HashMap<String, f64>,
    rotation: usize,
    now: DateTime<Utc>,
) {
    let recent_cutoff = now - ChronoDuration::days(7);
    let recent = |item: &BankItem| {
        item.last_served
            .map(|served| served >= recent_cutoff)
            .unwrap_or(false)
    };
    items.sort_by(|left, right| {
        engine
            .score(&right.skill_id, due_scores)
            .total_cmp(&engine.score(&left.skill_id, due_scores))
            .then_with(|| recent(left).cmp(&recent(right)))
            .then_with(|| left.last_served.cmp(&right.last_served))
            .then_with(|| left.id.cmp(&right.id))
    });
    let keys: Vec<(f64, bool, Option<DateTime<Utc>>)> = items
        .iter()
        .map(|item| {
            (
                engine.score(&item.skill_id, due_scores),
                recent(item),
                item.last_served,
            )
        })
        .collect();
    let mut start = 0;
    while start < items.len() {
        let mut end = start + 1;
        while end < items.len()
            && keys[end].0.total_cmp(&keys[start].0).is_eq()
            && keys[end].1 == keys[start].1
            && keys[end].2 == keys[start].2
        {
            end += 1;
        }
        let len = end - start;
        if len > 1 {
            items[start..end].rotate_left(rotation % len);
        }
        start = end;
    }
}

fn cached_from(bank: &BankItem, phase: Phase) -> CachedItem {
    CachedItem {
        phase,
        id: bank.id.clone(),
        skill_id: bank.skill_id.clone(),
        strand: bank.strand.clone(),
        prompt: bank.prompt.clone(),
        hints: bank.hints.clone(),
        audio_path: bank.audio_path.clone(),
        parts: bank.parts.clone(),
        mark_scheme: bank.mark_scheme.clone(),
    }
}

fn take_for_phase(
    out: &mut Vec<CachedItem>,
    remaining: &mut Vec<&BankItem>,
    phase: Phase,
    count: usize,
) -> Vec<CachedItem> {
    let mut taken = Vec::new();
    while taken.len() < count && !remaining.is_empty() {
        let bank = remaining.remove(0);
        let cached = cached_from(bank, phase);
        out.push(cached.clone());
        taken.push(cached);
    }
    taken
}

fn take_for_phase_other_strand(
    out: &mut Vec<CachedItem>,
    remaining: &mut Vec<&BankItem>,
    phase: Phase,
    count: usize,
    exclude: Option<&str>,
) -> Vec<CachedItem> {
    let mut taken = Vec::new();
    if let Some(exclude) = exclude {
        let mut index = 0;
        while taken.len() < count && index < remaining.len() {
            if remaining[index].strand == exclude {
                index += 1;
                continue;
            }
            let bank = remaining.remove(index);
            let cached = cached_from(bank, phase);
            out.push(cached.clone());
            taken.push(cached);
        }
    }
    // Degrade gracefully when the bank has only one strand: fill the
    // rest of the phase instead of leaving the learner with no item.
    if taken.len() < count {
        taken.extend(take_for_phase(out, remaining, phase, count - taken.len()));
    }
    taken
}

fn select_content(
    template: Template,
    bank: &[BankItem],
    engine: SessionEngine,
    due_scores: &std::collections::HashMap<String, f64>,
    rotation: usize,
    now: DateTime<Utc>,
) -> Vec<CachedItem> {
    let mut remaining: Vec<&BankItem> = bank.iter().collect();
    sort_due(&mut remaining, engine, due_scores, rotation, now);
    let mut out = Vec::new();
    match template {
        Template::SchoolDay20 => {
            take_for_phase(&mut out, &mut remaining, Phase::Recall, 2);
            take_for_phase(&mut out, &mut remaining, Phase::Practice, 3);
            take_for_phase(&mut out, &mut remaining, Phase::Explain, 2);
        }
        Template::Weekend40 => {
            take_for_phase(&mut out, &mut remaining, Phase::Recall, 2);
            let block_a = take_for_phase(&mut out, &mut remaining, Phase::Practice, 3);
            let strand = block_a.first().map(|item| item.strand.clone());
            take_for_phase_other_strand(
                &mut out,
                &mut remaining,
                Phase::Explain,
                3,
                strand.as_deref(),
            );
        }
    }
    out
}

fn row_to_run(row: &sqlx::sqlite::SqliteRow) -> Run {
    let template =
        Template::parse(&row.get::<String, _>("template")).unwrap_or(Template::SchoolDay20);
    let content: Vec<CachedItem> =
        serde_json::from_str(&row.get::<String, _>("content_json")).unwrap_or_default();
    Run {
        id: row.get("id"),
        template,
        phase_index: row.get::<i64, _>("phase_index").max(0) as usize,
        phase_remaining_seconds: row.get::<i64, _>("phase_remaining_seconds").max(0) as u32,
        paused: row.get::<i64, _>("paused") != 0,
        paused_at: row
            .get::<Option<String>, _>("paused_at")
            .as_deref()
            .and_then(decode_ts),
        total_paused_seconds: row.get::<i64, _>("total_paused_seconds").max(0) as u32,
        off_task_pauses: row.get::<i64, _>("off_task_pauses").max(0) as u32,
        skip_count: row.get::<i64, _>("skip_count").max(0) as u32,
        grace_tokens: row.get::<i64, _>("grace_tokens").max(0) as u32,
        item_cursor: row.get("item_cursor"),
        practice_item_id: row.get("practice_item_id"),
        teach_answer: None,
        hint_level: 0,
        content,
        started_at: decode_ts(&row.get::<String, _>("started_at")).unwrap_or_else(Utc::now),
        updated_at: decode_ts(&row.get::<String, _>("updated_at")).unwrap_or_else(Utc::now),
        ended_at: row
            .get::<Option<String>, _>("ended_at")
            .as_deref()
            .and_then(decode_ts),
        outcome: row.get("outcome"),
    }
}

async fn load(pool: &SqlitePool, id: &str) -> anyhow::Result<Option<Run>> {
    let row = sqlx::query(
        "SELECT id, template, phase_index, phase_remaining_seconds, paused, paused_at, \
         total_paused_seconds, off_task_pauses, skip_count, grace_tokens, item_cursor, \
         practice_item_id, content_json, started_at, updated_at, ended_at, outcome \
         FROM session_runs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some(mut run) = row.as_ref().map(row_to_run) else {
        return Ok(None);
    };
    run.hint_level = hint_level_for(pool, &run).await?;
    refresh_teach_answer(pool, &mut run).await?;
    Ok(Some(run))
}

async fn save(pool: &SqlitePool, run: &Run) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE session_runs SET template = ?, phase = ?, phase_index = ?, \
         phase_remaining_seconds = ?, paused = ?, paused_at = ?, total_paused_seconds = ?, \
         off_task_pauses = ?, skip_count = ?, grace_tokens = ?, item_cursor = ?, \
         practice_item_id = ?, content_json = ?, updated_at = ?, ended_at = ?, outcome = ? \
         WHERE id = ?",
    )
    .bind(run.template.as_str())
    .bind(run.phase().as_str())
    .bind(run.phase_index as i64)
    .bind(run.phase_remaining_seconds as i64)
    .bind(i64::from(run.paused))
    .bind(run.paused_at.map(encode_ts))
    .bind(run.total_paused_seconds as i64)
    .bind(run.off_task_pauses as i64)
    .bind(run.skip_count as i64)
    .bind(run.grace_tokens as i64)
    .bind(run.item_cursor)
    .bind(run.practice_item_id.clone())
    .bind(serde_json::to_string(&run.content)?)
    .bind(encode_ts(run.updated_at))
    .bind(run.ended_at.map(encode_ts))
    .bind(run.outcome.clone())
    .bind(&run.id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn log_event(
    pool: &SqlitePool,
    run: &Run,
    at: DateTime<Utc>,
    kind: &str,
    detail: serde_json::Value,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO session_events(session_id, at, kind, phase, detail) VALUES(?,?,?,?,?)",
    )
    .bind(&run.id)
    .bind(encode_ts(at))
    .bind(kind)
    .bind(run.phase().as_str())
    .bind(detail.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

async fn refresh(pool: &SqlitePool, id: &str, now: DateTime<Utc>) -> anyhow::Result<Option<Run>> {
    let Some(mut run) = load(pool, id).await? else {
        return Ok(None);
    };
    let changed = apply_elapsed(pool, &mut run, now).await?;
    run.hint_level = hint_level_for(pool, &run).await?;
    refresh_teach_answer(pool, &mut run).await?;
    if changed {
        save(pool, &run).await?;
    }
    Ok(Some(run))
}

/// Apply wall-clock time to a running, unpaused session. Returns true
/// when anything changed and needs saving.
async fn apply_elapsed(
    pool: &SqlitePool,
    run: &mut Run,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    if run.paused || run.ended_at.is_some() {
        return Ok(false);
    }
    let elapsed = (now - run.updated_at).num_seconds();
    if elapsed <= 0 {
        return Ok(false);
    }
    let mut bag = elapsed.min(i64::from(u32::MAX)) as u32;
    let mut guard = 0;
    while bag > 0 && run.ended_at.is_none() {
        guard += 1;
        if guard > 32 {
            break;
        }
        if run.phase_remaining_seconds == 0 {
            advance_phase(pool, run, now, "timer").await?;
            continue;
        }
        if bag < run.phase_remaining_seconds {
            run.phase_remaining_seconds -= bag;
            bag = 0;
        } else {
            bag -= run.phase_remaining_seconds;
            run.phase_remaining_seconds = 0;
            advance_phase(pool, run, now, "timer").await?;
        }
    }
    run.updated_at = now;
    Ok(true)
}

async fn advance_phase(
    pool: &SqlitePool,
    run: &mut Run,
    now: DateTime<Utc>,
    reason: &str,
) -> anyhow::Result<()> {
    let plan = run.template.plan();
    let from_index = run.phase_index;
    let from = plan[from_index];
    if from_index + 1 >= plan.len() {
        run.ended_at = Some(now);
        run.outcome = Some("completed".to_string());
        log_event(pool, run, now, "end", serde_json::json!({"reason": reason})).await?;
        return Ok(());
    }
    let actual_seconds = from.seconds.saturating_sub(run.phase_remaining_seconds);
    let practised = (from.phase == Phase::Practice)
        .then(|| current_cached(run).map(|item| item.id.clone()))
        .flatten();
    let mut next_index = from_index + 1;
    while next_index + 1 < plan.len() && plan[next_index].seconds == 0 {
        next_index += 1;
    }
    run.phase_index = next_index;
    let next = plan[next_index];
    run.phase_remaining_seconds = next.seconds;
    if from.phase == Phase::Explain {
        run.practice_item_id = None;
        run.teach_answer = None;
    }
    if next.phase == Phase::Explain {
        if let Some(practice_id) = practised {
            run.practice_item_id = Some(practice_id.clone());
            run.item_cursor = run
                .content
                .iter()
                .position(|item| item.id == practice_id && item.phase == Phase::Practice)
                .map(|index| index as i64)
                .unwrap_or_else(|| first_content_index(&run.content, next.phase));
        } else {
            run.item_cursor = first_content_index(&run.content, next.phase);
        }
    } else {
        run.item_cursor = first_content_index(&run.content, next.phase);
    }
    run.hint_level = hint_level_for(pool, run).await?;
    let mut detail = serde_json::json!({
        "from": from.phase.as_str(),
        "to": next.phase.as_str(),
        "planned_seconds": from.seconds,
        "actual_seconds": actual_seconds,
        "reason": reason,
    });
    if next.phase == Phase::End {
        run.ended_at = Some(now);
        run.outcome = Some("completed".to_string());
        detail["outcome"] = serde_json::json!("completed");
    }
    log_event(pool, run, now, "phase", detail).await?;
    if next.phase == Phase::End {
        log_event(pool, run, now, "end", serde_json::json!({"reason": reason})).await?;
    }
    Ok(())
}

/// Start a session and cache its content from the item bank.
pub async fn start(
    pool: &SqlitePool,
    options: StartOptions,
    now: DateTime<Utc>,
) -> anyhow::Result<SessionView> {
    let plan = options.template.plan();
    let mut phase_index = 0usize;
    while phase_index + 1 < plan.len() && plan[phase_index].seconds == 0 {
        phase_index += 1;
    }
    let bank = load_bank(pool).await?;
    // The FSRS engine loads due scores once per run, so newly graded
    // attempts change the very next session's order. Rotation advances
    // with the run count to avoid replaying the same items.
    let due_scores = if options.engine.uses_fsrs() {
        fsrs::due_score_map(pool).await?
    } else {
        std::collections::HashMap::new()
    };
    let rotation: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs")
        .fetch_one(pool)
        .await?;
    let mut content = select_content(
        options.template,
        &bank,
        options.engine,
        &due_scores,
        rotation.max(0) as usize,
        now,
    );
    let id = Uuid::new_v4().to_string();
    // The cached rungs mirror items.hints and this run's hint_state row.
    // Creating the row at start fixes the cursor at zero for every session,
    // so an earlier run's revealed hints cannot leak in.
    for item in &mut content {
        let ladder = fsrs::hint_ladder_for_session(pool, &item.id, &id).await?;
        item.hints = ladder.levels;
    }
    let phase = plan[phase_index].phase;
    let phase_remaining_seconds = plan[phase_index].seconds;
    let item_cursor = first_content_index(&content, phase);
    let mut run = Run {
        id,
        template: options.template,
        phase_index,
        phase_remaining_seconds,
        paused: false,
        paused_at: None,
        total_paused_seconds: 0,
        off_task_pauses: 0,
        skip_count: 0,
        grace_tokens: options.grace_tokens,
        item_cursor,
        practice_item_id: None,
        teach_answer: None,
        hint_level: 0,
        content,
        started_at: now,
        updated_at: now,
        ended_at: None,
        outcome: None,
    };
    run.hint_level = hint_level_for(pool, &run).await?;
    sqlx::query(
        "INSERT INTO session_runs( \
         id, template, phase, phase_index, phase_remaining_seconds, paused, paused_at, \
         total_paused_seconds, off_task_pauses, skip_count, grace_tokens, item_cursor, \
         practice_item_id, content_json, started_at, updated_at, ended_at, outcome \
         ) VALUES(?,?,?,?,?,0,NULL,0,0,0,?,?,NULL,?,?,?,NULL,NULL)",
    )
    .bind(&run.id)
    .bind(run.template.as_str())
    .bind(phase.as_str())
    .bind(run.phase_index as i64)
    .bind(run.phase_remaining_seconds as i64)
    .bind(run.grace_tokens as i64)
    .bind(run.item_cursor)
    .bind(serde_json::to_string(&run.content)?)
    .bind(encode_ts(run.started_at))
    .bind(encode_ts(run.updated_at))
    .execute(pool)
    .await?;
    // Remember every item cached into this run. Selection reads the most
    // recent service time to avoid serving the same item twice in a week.
    for item in &run.content {
        sqlx::query(
            "INSERT INTO served_items(item_id, session_id, phase, served_at) VALUES(?,?,?,?)",
        )
        .bind(&item.id)
        .bind(&run.id)
        .bind(item.phase.as_str())
        .bind(encode_ts(now))
        .execute(pool)
        .await?;
    }
    log_event(
        pool,
        &run,
        now,
        "start",
        serde_json::json!({
            "template": run.template.as_str(),
            "phase": phase.as_str(),
            "cached_items": run.content.len(),
            "content_source": "cache",
        }),
    )
    .await?;
    Ok(run.view(now))
}

/// Read the latest state, applying elapsed wall-clock time first.
pub async fn get(
    pool: &SqlitePool,
    id: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<SessionView>> {
    Ok(refresh(pool, id, now).await?.map(|run| run.view(now)))
}

/// Advance the server timer to `now`.
pub async fn tick(
    pool: &SqlitePool,
    id: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<SessionView>> {
    get(pool, id, now).await
}

/// Manual "next phase" transition. Nothing is blocked; skipping a phase
/// is always allowed.
pub async fn advance(
    pool: &SqlitePool,
    id: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<SessionView>> {
    let Some(mut run) = refresh(pool, id, now).await? else {
        return Ok(None);
    };
    if run.ended_at.is_none() {
        advance_phase(pool, &mut run, now, "manual").await?;
        refresh_teach_answer(pool, &mut run).await?;
        run.updated_at = now;
        save(pool, &run).await?;
    }
    Ok(Some(run.view(now)))
}

/// Report off-task behaviour. Over 60 seconds pauses the timer and
/// returns a nudge; nothing is blocked.
pub async fn off_task(
    pool: &SqlitePool,
    id: &str,
    seconds: u32,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<OffTaskOutcome>> {
    let Some(mut run) = refresh(pool, id, now).await? else {
        return Ok(None);
    };
    let mut nudge = None;
    if seconds >= 60 && !run.paused && run.ended_at.is_none() {
        run.paused = true;
        run.paused_at = Some(now);
        run.off_task_pauses = run.off_task_pauses.saturating_add(1);
        nudge = Some(
            "You have been off task for 60 seconds. The timer is paused. \
             Nothing is blocked; resume when you are ready."
                .to_string(),
        );
        log_event(
            pool,
            &run,
            now,
            "off_task_pause",
            serde_json::json!({"seconds": seconds, "nudge": nudge}),
        )
        .await?;
    } else {
        log_event(
            pool,
            &run,
            now,
            "off_task",
            serde_json::json!({"seconds": seconds, "paused": run.paused}),
        )
        .await?;
    }
    run.updated_at = now;
    save(pool, &run).await?;
    Ok(Some(OffTaskOutcome {
        paused: run.paused,
        nudge,
        session: run.view(now),
    }))
}

/// Resume a paused timer after an off-task nudge.
pub async fn resume(
    pool: &SqlitePool,
    id: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<SessionView>> {
    let Some(mut run) = refresh(pool, id, now).await? else {
        return Ok(None);
    };
    if run.paused {
        let paused_seconds = run
            .paused_at
            .map(|paused_at| (now - paused_at).num_seconds().max(0) as u32)
            .unwrap_or(0);
        run.total_paused_seconds = run.total_paused_seconds.saturating_add(paused_seconds);
        run.paused = false;
        run.paused_at = None;
        run.updated_at = now;
        log_event(
            pool,
            &run,
            now,
            "resume",
            serde_json::json!({"paused_seconds": paused_seconds}),
        )
        .await?;
        save(pool, &run).await?;
    }
    Ok(Some(run.view(now)))
}

/// Log a skip with its reason, spend a grace token when available, and
/// consume the item while staying in the current phase. The phase only
/// advances once every cached item in it has been skipped (STUDY-108).
pub async fn skip(
    pool: &SqlitePool,
    id: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<SkipOutcome>> {
    let Some(mut run) = refresh(pool, id, now).await? else {
        return Ok(None);
    };
    run.skip_count = run.skip_count.saturating_add(1);
    let grace_used = run.grace_tokens > 0;
    if grace_used {
        run.grace_tokens -= 1;
    }
    let skipped_item = current_cached(&run).map(|item| item.id.clone());
    let next_index = if run.ended_at.is_none() {
        next_index_in_phase(&run)
    } else {
        None
    };
    let stayed_in_phase = next_index.is_some();
    log_event(
        pool,
        &run,
        now,
        "skip",
        serde_json::json!({
            "reason": reason,
            "grace_used": grace_used,
            "grace_tokens_left": run.grace_tokens,
            "skipped_item_id": skipped_item,
            "stayed_in_phase": stayed_in_phase,
            "replanned": !stayed_in_phase,
        }),
    )
    .await?;
    if run.ended_at.is_none() {
        if let Some(index) = next_index {
            let item_id = run.content[index].id.clone();
            run.item_cursor = index as i64;
            run.hint_level = hint_level_for(pool, &run).await?;
            log_event(
                pool,
                &run,
                now,
                "item",
                serde_json::json!({"item_id": item_id, "phase": run.phase().as_str()}),
            )
            .await?;
        } else {
            advance_phase(pool, &mut run, now, "skip").await?;
        }
    }
    refresh_teach_answer(pool, &mut run).await?;
    run.updated_at = now;
    save(pool, &run).await?;
    let weekly_skip_count = weekly_skip_count(pool, now).await?;
    Ok(Some(SkipOutcome {
        session: run.view(now),
        weekly_skip_count,
        grace_used,
    }))
}

/// Unlock the next rung of the current item's ladder. The cursor lives in
/// `hint_state`; this function advances it through [`fsrs::next_hint`] and
/// keeps a session event so the review can see hint use in context.
pub async fn hint(
    pool: &SqlitePool,
    id: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<HintOutcome>> {
    let Some(mut run) = refresh(pool, id, now).await? else {
        return Ok(None);
    };
    let Some(item_id) = current_cached(&run).map(|item| item.id.clone()) else {
        return Ok(Some(HintOutcome {
            hint: None,
            hints_used: 0,
            session: run.view(now),
        }));
    };
    let reveal = fsrs::next_hint_for_session(pool, &item_id, Some(&run.id)).await?;
    if let Some(reveal) = reveal.as_ref() {
        // The authoritative ladder may have been authored or repaired on
        // first use; refresh the cached rungs so the view matches it.
        sync_cached_ladder(pool, &mut run, &item_id).await?;
        log_event(
            pool,
            &run,
            now,
            "hint",
            serde_json::json!({
                "item_id": item_id,
                "level": reveal.level,
                "total": reveal.total,
                "final_level": reveal.final_level,
            }),
        )
        .await?;
        run.updated_at = now;
        save(pool, &run).await?;
    }
    run.hint_level = hint_level_for(pool, &run).await?;
    Ok(Some(HintOutcome {
        hint: reveal.map(|reveal| reveal.text),
        hints_used: run.hint_level.max(0) as u32,
        session: run.view(now),
    }))
}

/// Move to the next cached item inside the current phase, if any.
pub async fn next_item(
    pool: &SqlitePool,
    id: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<SessionView>> {
    let Some(mut run) = refresh(pool, id, now).await? else {
        return Ok(None);
    };
    let phase = run.phase();
    let found = next_index_in_phase(&run).map(|index| (index, run.content[index].id.clone()));
    if let Some((index, item_id)) = found {
        run.item_cursor = index as i64;
        run.hint_level = hint_level_for(pool, &run).await?;
        refresh_teach_answer(pool, &mut run).await?;
        log_event(
            pool,
            &run,
            now,
            "item",
            serde_json::json!({"item_id": item_id, "phase": phase.as_str()}),
        )
        .await?;
        run.updated_at = now;
        save(pool, &run).await?;
    }
    Ok(Some(run.view(now)))
}

/// Grade one attempt from this session and record it through FSRS.
///
/// The session id is attached here, not trusted from the caller, so the
/// weekly review can tie the attempt, hint and intervention rows back to
/// the run. Hint use is read from `hint_state` by
/// [`fsrs::record_attempt`]; this module does not keep a parallel count.
/// The session event log still records the attempt for [`events`] and
/// [`weekly_skip_count`] readers.
pub async fn grade(
    pool: &SqlitePool,
    id: &str,
    mut input: fsrs::AttemptInput,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<GradeOutcome>> {
    let Some(mut run) = refresh(pool, id, now).await? else {
        return Ok(None);
    };
    anyhow::ensure!(
        run.content.iter().any(|item| item.id == input.item_id),
        "item {} is not part of session {}",
        input.item_id,
        run.id
    );
    input.session_id = Some(run.id.clone());
    let attempt = fsrs::record_attempt(pool, &input).await?;
    log_event(
        pool,
        &run,
        now,
        "attempt",
        serde_json::json!({
            "item_id": &input.item_id,
            "given": &input.given,
            "skill_id": &attempt.skill_id,
            "correct": attempt.correct,
            "rating": attempt.rating,
            "hints_used": attempt.hints_used,
            "mastery_before": attempt.mastery_before,
            "mastery_after": attempt.mastery_after,
            "error_tags": &input.error_tags,
            "repeated_errors": &attempt.repeated_errors,
        }),
    )
    .await?;
    // record_attempt resets the ladder for the next rep.
    run.hint_level = hint_level_for(pool, &run).await?;
    refresh_teach_answer(pool, &mut run).await?;
    run.updated_at = now;
    save(pool, &run).await?;
    Ok(Some(GradeOutcome {
        attempt,
        session: run.view(now),
    }))
}

/// Grade a multi-part question part by part and total the marks.
///
/// Each part keeps its own answer, marks and rubric; the client submits
/// one answer per part. A part is fully awarded when its submitted answer
/// matches the authored answer after whitespace/unit folding, or within a
/// small numeric tolerance. The FSRS attempt is recorded once for the
/// item, so the skill schedule still sees one review rather than one per
/// part.
pub async fn grade_parts(
    pool: &SqlitePool,
    id: &str,
    request: MultiPartRequest,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<MultiPartGradeOutcome>> {
    let Some(mut run) = refresh(pool, id, now).await? else {
        return Ok(None);
    };
    let cached = run
        .content
        .iter()
        .find(|item| item.id == request.item_id)
        .cloned();
    let Some(cached) = cached else {
        anyhow::bail!(
            "item {} is not part of session {}",
            request.item_id,
            run.id
        );
    };
    anyhow::ensure!(
        !cached.parts.is_empty(),
        "item {} has no multi-part answers",
        cached.id
    );

    let mut grades = Vec::with_capacity(cached.parts.len());
    let mut awarded_marks = 0.0f32;
    let mut total_marks = 0.0f32;
    for part in &cached.parts {
        let submitted = request.parts.iter().find(|answer| {
            answer.label.trim().eq_ignore_ascii_case(part.label.trim())
        });
        let given = submitted.map(|answer| answer.given.trim().to_string());
        let correct = given
            .as_deref()
            .map(|given| answers_match(given, &part.answer))
            .unwrap_or(false);
        let awarded = if correct { part.marks } else { 0.0 };
        awarded_marks += awarded;
        total_marks += part.marks;
        grades.push(PartGrade {
            label: part.label.clone(),
            prompt: part.prompt.clone(),
            given,
            expected: part.answer.clone(),
            correct,
            awarded,
            marks: part.marks,
            rubric: part.rubric.clone(),
        });
    }

    let summary = grades
        .iter()
        .map(|grade| {
            format!(
                "({}) {}",
                grade.label,
                grade.given.as_deref().unwrap_or("no answer")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let all_correct =
        total_marks > 0.0 && (awarded_marks - total_marks).abs() <= f32::EPSILON * 8.0;
    let input = fsrs::AttemptInput {
        item_id: cached.id.clone(),
        input_kind: "typed".into(),
        given: summary.clone(),
        correct: all_correct,
        session_id: Some(run.id.clone()),
        error_tags: Vec::new(),
        attempt_id: None,
    };
    let attempt = fsrs::record_attempt(pool, &input).await?;
    log_event(
        pool,
        &run,
        now,
        "attempt",
        serde_json::json!({
            "item_id": &cached.id,
            "given": &summary,
            "skill_id": &attempt.skill_id,
            "correct": attempt.correct,
            "rating": attempt.rating,
            "hints_used": attempt.hints_used,
            "awarded_marks": awarded_marks,
            "total_marks": total_marks,
            "parts": grades.iter().map(|grade| serde_json::json!({
                "label": &grade.label,
                "correct": grade.correct,
                "awarded": grade.awarded,
                "marks": grade.marks,
            })).collect::<Vec<_>>(),
        }),
    )
    .await?;
    run.hint_level = hint_level_for(pool, &run).await?;
    refresh_teach_answer(pool, &mut run).await?;
    run.updated_at = now;
    save(pool, &run).await?;
    Ok(Some(MultiPartGradeOutcome {
        item_id: cached.id,
        parts: grades,
        awarded_marks,
        total_marks,
        correct: all_correct,
        attempt,
        session: run.view(now),
    }))
}

/// Fold whitespace, unicode maths and common unit suffixes before
/// comparing two short answers.
fn normalized_answer(raw: &str) -> String {
    raw.trim()
        .trim_end_matches('.')
        .to_lowercase()
        .replace('−', "-")
        .replace('×', "*")
        .replace('÷', "/")
        .replace('π', "pi")
        .replace('²', "^2")
        .replace('³', "^3")
        .replace('≈', "")
        .replace("approx", "")
        .replace(['$', ','], "")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

fn numeric_answer(raw: &str) -> Option<f64> {
    let mut text = raw.trim().trim_end_matches('.').to_lowercase();
    for prefix in ["x=", "y=", "n=", "answer:", "="] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim().to_string();
        }
    }
    let text = text.replace(['$', ','], "").replace("cm", "");
    let mut end = text.len();
    while end > 0 {
        let last = text[..end].chars().next_back()?;
        if last.is_ascii_alphabetic() || matches!(last, '%' | '°') {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    let core = text[..end].trim();
    if let Ok(value) = core.parse::<f64>() {
        return Some(value);
    }
    if let Some((numerator, denominator)) = core.split_once('/') {
        let numerator = numerator.trim().parse::<f64>().ok()?;
        let denominator = denominator.trim().parse::<f64>().ok()?;
        if denominator != 0.0 {
            return Some(numerator / denominator);
        }
    }
    None
}

fn answers_match(given: &str, expected: &str) -> bool {
    if normalized_answer(given) == normalized_answer(expected) {
        return true;
    }
    match (numeric_answer(given), numeric_answer(expected)) {
        (Some(given), Some(expected)) => {
            let scale = expected.abs().max(1.0);
            (given - expected).abs() <= 0.001 * scale
        }
        _ => false,
    }
}

/// Read the append-only event log for a session. This is the public
/// boundary other modules (dashboard, weekly review, planner) use.
pub async fn events(pool: &SqlitePool, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
    let rows = sqlx::query(
        "SELECT id, session_id, at, kind, phase, detail FROM session_events \
         WHERE session_id = ? ORDER BY id",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| SessionEvent {
            id: row.get("id"),
            session_id: row.get("session_id"),
            at: row.get("at"),
            kind: row.get("kind"),
            phase: Phase::parse(&row.get::<String, _>("phase")).unwrap_or(Phase::End),
            detail: serde_json::from_str(&row.get::<String, _>("detail"))
                .unwrap_or(serde_json::Value::Null),
        })
        .collect())
}

/// Number of logged skips in the trailing seven days.
pub async fn weekly_skip_count(pool: &SqlitePool, now: DateTime<Utc>) -> anyhow::Result<u32> {
    let cutoff = encode_ts(now - ChronoDuration::days(7));
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM session_events WHERE kind = 'skip' AND at >= ?")
            .bind(cutoff)
            .fetch_one(pool)
            .await?;
    Ok(count.max(0) as u32)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn school_day_covers_twenty_minutes() {
        assert_eq!(Template::SchoolDay20.total_seconds(), 20 * 60);
    }

    #[test]
    fn weekend_covers_forty_minutes_with_break_at_eighteen() {
        assert_eq!(Template::Weekend40.total_seconds(), 40 * 60);
        let mut elapsed = 0u32;
        let mut break_start = None;
        let mut break_end = None;
        for spec in Template::Weekend40.plan() {
            if spec.phase == Phase::Break {
                break_start = Some(elapsed);
                break_end = Some(elapsed + spec.seconds);
            }
            elapsed += spec.seconds;
        }
        assert_eq!(break_start, Some(18 * 60));
        assert_eq!(break_end, Some(22 * 60));
    }

    #[test]
    fn phase_round_trips() {
        for phase in [
            Phase::Prime,
            Phase::Recall,
            Phase::Practice,
            Phase::Explain,
            Phase::Reflect,
            Phase::Break,
            Phase::End,
        ] {
            assert_eq!(Phase::parse(phase.as_str()), Some(phase));
        }
    }
}
