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

    /// True when selection should order by due-ness instead of bank order.
    fn is_active(self) -> bool {
        self.fsrs || self.due_score.is_some()
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

#[derive(Debug, Clone)]
struct BankItem {
    id: String,
    skill_id: String,
    strand: String,
    prompt: String,
    hints: Vec<String>,
    audio_path: Option<String>,
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
        let item = current_cached(self).map(|cached| ItemView {
            id: cached.id.clone(),
            skill_id: cached.skill_id.clone(),
            strand: cached.strand.clone(),
            prompt: cached.prompt.clone(),
            audio_path: cached.audio_path.clone(),
            hints: cached.hints.clone(),
            hint_count: cached.hints.len(),
            hint_unlocked: hint_level.min(cached.hints.len()),
        });
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
    if run.item_cursor >= 0 {
        if let Some(item) = run.content.get(run.item_cursor as usize) {
            if item.phase == run.phase() {
                return Some(item);
            }
        }
    }
    run.content.iter().find(|item| item.phase == run.phase())
}

/// Read the ladder cursor for the run's current item. `hint_state` owns
/// this value, keyed by session id; the run only mirrors it for rendering.
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
         a.path AS audio_path \
         FROM items i \
         LEFT JOIN skills s ON s.id = i.skill_id \
         LEFT JOIN item_audio a ON a.item_id = i.id \
         WHERE i.status = 'verified' \
         ORDER BY i.id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| BankItem {
            id: row.get("id"),
            skill_id: row.get("skill_id"),
            strand: row.get("strand"),
            prompt: row.get("prompt"),
            hints: serde_json::from_str(&row.get::<String, _>("hints")).unwrap_or_default(),
            audio_path: row.get("audio_path"),
        })
        .collect())
}

/// Order by due score, then rotate inside each equal-score group so a
/// repeated session over the same bank moves on instead of serving the
/// same items forever. A plain `SessionEngine::new()` keeps bank order.
fn sort_due(
    items: &mut [&BankItem],
    engine: SessionEngine,
    due_scores: &std::collections::HashMap<String, f64>,
    rotation: usize,
) {
    if !engine.is_active() {
        return;
    }
    items.sort_by(|left, right| {
        engine
            .score(&right.skill_id, due_scores)
            .total_cmp(&engine.score(&left.skill_id, due_scores))
            .then_with(|| left.id.cmp(&right.id))
    });
    let scores: Vec<f64> = items
        .iter()
        .map(|item| engine.score(&item.skill_id, due_scores))
        .collect();
    let mut start = 0;
    while start < items.len() {
        let mut end = start + 1;
        while end < items.len() && scores[end].total_cmp(&scores[start]).is_eq() {
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
) -> Vec<CachedItem> {
    let mut remaining: Vec<&BankItem> = bank.iter().collect();
    sort_due(&mut remaining, engine, due_scores, rotation);
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
         content_json, started_at, updated_at, ended_at, outcome \
         FROM session_runs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some(mut run) = row.as_ref().map(row_to_run) else {
        return Ok(None);
    };
    run.hint_level = hint_level_for(pool, &run).await?;
    Ok(Some(run))
}

async fn save(pool: &SqlitePool, run: &Run) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE session_runs SET template = ?, phase = ?, phase_index = ?, \
         phase_remaining_seconds = ?, paused = ?, paused_at = ?, total_paused_seconds = ?, \
         off_task_pauses = ?, skip_count = ?, grace_tokens = ?, item_cursor = ?, \
         content_json = ?, updated_at = ?, ended_at = ?, outcome = ? \
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
    let mut next_index = from_index + 1;
    while next_index + 1 < plan.len() && plan[next_index].seconds == 0 {
        next_index += 1;
    }
    run.phase_index = next_index;
    let next = plan[next_index];
    run.phase_remaining_seconds = next.seconds;
    run.item_cursor = first_content_index(&run.content, next.phase);
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
         content_json, started_at, updated_at, ended_at, outcome \
         ) VALUES(?,?,?,?,?,0,NULL,0,0,0,?,?,?,?,?,NULL,NULL)",
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

/// Log a skip with its reason, spend a grace token when available,
/// re-plan by moving to the next phase, and return the weekly skip
/// count.
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
    log_event(
        pool,
        &run,
        now,
        "skip",
        serde_json::json!({
            "reason": reason,
            "grace_used": grace_used,
            "grace_tokens_left": run.grace_tokens,
            "replanned": true,
        }),
    )
    .await?;
    if run.ended_at.is_none() {
        advance_phase(pool, &mut run, now, "skip").await?;
    }
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
    let start = (run.item_cursor + 1).max(0) as usize;
    let found = run
        .content
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, item)| item.phase == phase)
        .map(|(index, item)| (index, item.id.clone()));
    if let Some((index, item_id)) = found {
        run.item_cursor = index as i64;
        run.hint_level = hint_level_for(pool, &run).await?;
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
    run.updated_at = now;
    save(pool, &run).await?;
    Ok(Some(GradeOutcome {
        attempt,
        session: run.view(now),
    }))
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
