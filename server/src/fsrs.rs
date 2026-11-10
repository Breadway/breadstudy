//! FSRS-style scheduling, the hint ladder and the review logs (STUDY-18).
//!
//! This module owns the memory model behind the planner and the session
//! engine. It deliberately exposes a narrow surface:
//!
//! - [`due_score`] / [`due_skills`] rank skills for the next plan.
//! - [`record_attempt`] stores an attempt and updates stability, difficulty,
//!   mastery, the due date, error patterns and both logs in one step.
//! - [`next_hint`] advances one item's hint ladder and records the use.
//! - [`error_patterns`] and [`weekly_snapshot`] hand the weekly review its
//!   session log, intervention log, error aggregates and mastery export.
//!
//! Scheduling follows the FSRS-4.5 recurrence with the published default
//! weights. Mastery is a separate, bounded projection of accuracy and
//! stability so the UI and the planner can compare skills on one scale.
//! The ladder text lives in `items.hints`; only the cursor lives in the
//! `hint_state` table.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

/// FSRS-4.5 default weights.
const FSRS_W: [f64; 17] = [
    0.4872, 1.4003, 3.7145, 13.8206, 5.1618, 1.2298, 0.8975, 0.031, 1.6474, 0.1367, 1.0461, 2.1072,
    0.0793, 0.3246, 1.587, 0.2272, 2.8755,
];
/// FSRS decay and factor for the forgetting curve.
const DECAY: f64 = -0.5;
const FACTOR: f64 = 19.0 / 81.0;
/// Target retention used to turn stability into a due date (0.9 gives
/// an interval of about one stability unit).
const DESIRED_RETENTION: f64 = 0.9;
/// Learning step after a lapse: ten minutes.
const LAPSE_MINUTES: f64 = 10.0;
/// Mastery assumed for a skill with no recorded review.
pub const UNSEEN_MASTERY: f64 = 0.4;
/// Planner score assumed for a skill with no schedule row.
pub const UNSEEN_DUE_SCORE: f64 = 1.2;
/// Every authored ladder has at least this many levels.
pub const MIN_HINT_LEVELS: usize = 3;

/// FSRS grade for one review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rating {
    Again = 1,
    Hard = 2,
    Good = 3,
    Easy = 4,
}

impl Rating {
    pub fn level(self) -> u8 {
        self as u8
    }

    fn as_f64(self) -> f64 {
        f64::from(self.level())
    }
}

/// Grade one attempt. A correct answer with no hints is Easy, one hint
/// makes it Good and two or more hints make it Hard; a wrong answer is
/// always Again.
pub fn rating_for(correct: bool, hints_used: u8) -> Rating {
    if !correct {
        Rating::Again
    } else {
        match hints_used {
            0 => Rating::Easy,
            1 => Rating::Good,
            _ => Rating::Hard,
        }
    }
}

fn clamp_difficulty(d: f64) -> f64 {
    d.clamp(1.0, 10.0)
}

/// Stability after the first review with this grade, in days.
pub fn initial_stability(rating: Rating) -> f64 {
    FSRS_W[(rating.level() - 1) as usize]
}

/// Difficulty after the first review with this grade.
pub fn initial_difficulty(rating: Rating) -> f64 {
    clamp_difficulty(FSRS_W[4] - (rating.as_f64() - 3.0) * FSRS_W[5])
}

/// FSRS-4.5 difficulty update.
pub fn next_difficulty(difficulty: f64, rating: Rating) -> f64 {
    let delta = -FSRS_W[6] * (rating.as_f64() - 3.0);
    let target = FSRS_W[7] * initial_difficulty(Rating::Easy);
    clamp_difficulty(target + (1.0 - FSRS_W[7]) * (difficulty + delta))
}

/// Probability of recall after `elapsed_days` for a given stability.
pub fn retrievability(elapsed_days: f64, stability: f64) -> f64 {
    if stability <= 0.0 {
        return 0.0;
    }
    (1.0 + FACTOR * elapsed_days.max(0.0) / stability).powf(DECAY)
}

/// FSRS-4.5 stability update. Reviews that are not lapses grow stability;
/// a lapse collapses it so the skill comes back quickly.
pub fn next_stability(
    difficulty: f64,
    stability: f64,
    retrievability_now: f64,
    rating: Rating,
) -> f64 {
    if stability <= 0.0 {
        return initial_stability(rating);
    }
    let next = match rating {
        Rating::Again => {
            FSRS_W[11]
                * difficulty.powf(-FSRS_W[12])
                * ((stability + 1.0).powf(FSRS_W[13]) - 1.0)
                * (FSRS_W[14] * (1.0 - retrievability_now)).exp()
        }
        Rating::Hard | Rating::Good | Rating::Easy => {
            let hard_penalty = if rating == Rating::Hard {
                FSRS_W[15]
            } else {
                1.0
            };
            let easy_bonus = if rating == Rating::Easy {
                FSRS_W[16]
            } else {
                1.0
            };
            stability
                * (1.0
                    + FSRS_W[8].exp()
                        * (11.0 - difficulty)
                        * stability.powf(-FSRS_W[9])
                        * ((FSRS_W[10] * (1.0 - retrievability_now)).exp() - 1.0)
                        * hard_penalty
                        * easy_bonus)
        }
    };
    next.max(0.01)
}

/// Days until the next review. Lapses use a ten minute learning step,
/// everything else follows the target-retention interval.
pub fn next_interval_days(stability: f64, rating: Rating) -> f64 {
    if rating == Rating::Again {
        return LAPSE_MINUTES / 1440.0;
    }
    let factor = DESIRED_RETENTION.powf(1.0 / DECAY) - 1.0;
    (stability / FACTOR * factor).max(1.0)
}

/// Bounded mastery projection used by the UI and the planner.
pub fn mastery_from(accuracy: f64, stability: f64) -> f64 {
    let retention_share = stability / (stability + 7.0);
    (0.6 * accuracy + 0.4 * retention_share).clamp(0.05, 1.0)
}

/// Planner score: higher means the skill should come sooner. It combines
/// the mastery gap with how overdue the review is, and discounts skills
/// whose next review is still in the future.
pub fn due_score_from(mastery: f64, due_at: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    let overdue_days = (now - due_at).num_seconds() as f64 / 86_400.0;
    let urgency = if overdue_days > 0.0 {
        (overdue_days / 7.0).min(1.0) * 3.0
    } else {
        0.0
    };
    let future_discount = if overdue_days < 0.0 {
        (-overdue_days / 7.0).min(1.0) * 0.75
    } else {
        0.0
    };
    (1.0 - mastery).max(0.0) * 2.0 + urgency - future_discount
}

fn iso(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_time(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|ts| ts.with_timezone(&Utc))
}

/// One skill as the planner sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DueSkill {
    pub skill_id: String,
    pub name: String,
    pub mastery: f64,
    pub due_at: Option<String>,
    pub due_score: f64,
    pub reviews: i64,
    pub lapses: i64,
}

/// One attempt as submitted by the session engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptInput {
    pub item_id: String,
    pub input_kind: String,
    pub given: String,
    pub correct: bool,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub error_tags: Vec<String>,
    /// When the caller already persisted a rich attempt row (the grading
    /// service does), update that row instead of inserting a second one.
    #[serde(default)]
    pub attempt_id: Option<String>,
}

/// Everything the caller needs after grading one attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptOutcome {
    pub attempt_id: String,
    pub skill_id: String,
    pub correct: bool,
    pub rating: u8,
    pub hints_used: u8,
    pub mastery_before: f64,
    pub mastery_after: f64,
    pub stability: f64,
    pub difficulty: f64,
    pub due_at: String,
    pub due_score: f64,
    pub error_tags: Vec<String>,
    /// Tags that have now been recorded at least twice for this skill.
    pub repeated_errors: Vec<String>,
}

/// One item's authored hint ladder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HintLadder {
    pub item_id: String,
    pub skill_id: String,
    pub levels: Vec<String>,
}

/// One revealed hint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HintReveal {
    pub item_id: String,
    pub level: i64,
    pub total: i64,
    pub text: String,
    /// True when this was the last rung, the only one allowed to reveal
    /// the final answer.
    pub final_level: bool,
}

/// Aggregated error tag for one skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPattern {
    pub skill_id: String,
    pub tag: String,
    pub count: i64,
    pub first_seen: String,
    pub last_seen: String,
}

/// One session or intervention log row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    pub id: i64,
    pub ts: String,
    pub kind: String,
    pub session_id: Option<String>,
    pub item_id: Option<String>,
    pub skill_id: Option<String>,
    pub hint_level: Option<i64>,
    pub detail: serde_json::Value,
}

/// Per-skill mastery export for the weekly review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasteryRow {
    pub skill_id: String,
    pub name: String,
    pub mastery: f64,
    pub stability: f64,
    pub difficulty: f64,
    pub reviews: i64,
    pub lapses: i64,
    pub due_at: Option<String>,
}

/// Everything the weekly review reads in one call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklySnapshot {
    pub since: Option<String>,
    pub sessions: Vec<LogEvent>,
    pub interventions: Vec<LogEvent>,
    pub errors: Vec<ErrorPattern>,
    pub mastery: Vec<MasteryRow>,
}

// ---------------------------------------------------------------- hints

/// Normalize text for leak checks: letters and digits only, lowercased.
fn normalize_for_match(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// True when `text` already gives away a distinctive final answer.
fn leaks_answer(text: &str, answer: &str) -> bool {
    let needle = normalize_for_match(answer);
    needle.chars().count() >= 3 && normalize_for_match(text).contains(&needle)
}

/// Validate an authored ladder. Every rung except the last must withhold
/// the final answer, and each rung must be more specific than the one
/// before it (longer, or an extension of it).
pub fn validate_ladder(levels: &[String], answer: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        levels.len() >= MIN_HINT_LEVELS,
        "hint ladder needs at least {MIN_HINT_LEVELS} levels"
    );
    for (index, level) in levels.iter().enumerate() {
        anyhow::ensure!(
            !level.trim().is_empty(),
            "hint level {} is empty",
            index + 1
        );
    }
    for pair in levels.windows(2) {
        let longer = pair[1].len() > pair[0].len();
        let extends = pair[1].contains(pair[0].as_str());
        anyhow::ensure!(
            longer || extends,
            "hint levels must escalate in specificity"
        );
    }
    let last = levels.len() - 1;
    for (index, level) in levels.iter().enumerate().take(last) {
        anyhow::ensure!(
            !leaks_answer(level, answer),
            "hint level {} reveals the final answer early",
            index + 1
        );
    }
    Ok(())
}

/// Author three escalating rungs from the skill name and worked steps.
/// The first rung names the rule, the second sets up the working, and the
/// third carries it to the final value (the last rung may hold the answer).
pub fn author_ladder(skill_name: &str, answer: &str, worked: &[String]) -> Vec<String> {
    let base = format!(
        "Which {skill_name} rule or formula connects the values in this question? Name it before substituting."
    );
    let steps: Vec<&str> = worked
        .iter()
        .map(|step| step.trim())
        .filter(|step| !step.is_empty())
        .collect();
    let setup = steps
        .first()
        .copied()
        .filter(|step| !leaks_answer(step, answer))
        .map(|step| format!("Set up the working: {step}"))
        .unwrap_or_else(|| {
            "Write the formula, list the given values, and decide which quantity you are finding."
                .to_string()
        });
    let finish = steps
        .last()
        .copied()
        .map(|step| format!("Carry the calculation to the final line: {step}"))
        .unwrap_or_else(|| {
            "Carry the arithmetic through, check units and rounding, then state the final value."
                .to_string()
        });
    let second = format!("{base} {setup}");
    let third = format!("{second} {finish}");
    vec![base, second, third]
}

/// The `hint_state` key for a session. `None` or an empty id keeps the
/// legacy item-level cursor used outside a session.
fn hint_session_key(session_id: Option<&str>) -> &str {
    session_id.map(str::trim).filter(|id| !id.is_empty()).unwrap_or("")
}

/// Ensure the item has an authored ladder and return it. Stored ladders
/// win when they validate; otherwise one is authored from the worked
/// steps and written back to `items.hints`.
async fn ensure_hint_ladder_for(
    pool: &sqlx::SqlitePool,
    item_id: &str,
    session_id: Option<&str>,
) -> anyhow::Result<HintLadder> {
    let row: (String, String, String, String, String) = sqlx::query_as(
        "SELECT i.skill_id, i.answer, i.rubric, i.hints, sk.name
         FROM items i JOIN skills sk ON sk.id = i.skill_id WHERE i.id = ?",
    )
    .bind(item_id)
    .fetch_one(pool)
    .await?;
    let (skill_id, answer, rubric, hints, skill_name) = row;
    let stored: Vec<String> = serde_json::from_str(&hints).unwrap_or_default();
    let levels = if validate_ladder(&stored, &answer).is_ok() {
        stored
    } else {
        let worked: Vec<String> = serde_json::from_str(&rubric).unwrap_or_default();
        let generated = author_ladder(&skill_name, &answer, &worked);
        validate_ladder(&generated, &answer)?;
        sqlx::query("UPDATE items SET hints = ? WHERE id = ?")
            .bind(serde_json::to_string(&generated)?)
            .bind(item_id)
            .execute(pool)
            .await?;
        generated
    };
    let key = hint_session_key(session_id);
    sqlx::query(
        "INSERT INTO hint_state(session_id, item_id, total, updated_at) VALUES(?,?,?,?)
         ON CONFLICT(session_id, item_id) DO UPDATE SET total = excluded.total, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(item_id)
    .bind(levels.len() as i64)
    .bind(iso(Utc::now()))
    .execute(pool)
    .await?;
    Ok(HintLadder {
        item_id: item_id.to_string(),
        skill_id,
        levels,
    })
}

/// Ensure the item has an authored ladder and return it. Stored ladders
/// win when they validate; otherwise one is authored from the worked
/// steps and written back to `items.hints`.
pub async fn ensure_hint_ladder(
    pool: &sqlx::SqlitePool,
    item_id: &str,
) -> anyhow::Result<HintLadder> {
    ensure_hint_ladder_for(pool, item_id, None).await
}

/// Read (and if needed author) the ladder without advancing it.
pub async fn hint_ladder(pool: &sqlx::SqlitePool, item_id: &str) -> anyhow::Result<HintLadder> {
    ensure_hint_ladder(pool, item_id).await
}

/// Read (and if needed author) the ladder for one session without
/// advancing it.
pub async fn hint_ladder_for_session(
    pool: &sqlx::SqlitePool,
    item_id: &str,
    session_id: &str,
) -> anyhow::Result<HintLadder> {
    ensure_hint_ladder_for(pool, item_id, Some(session_id)).await
}

/// Reveal the next rung of an item's ladder and log the use. Returns
/// `None` once every rung has been seen.
pub async fn next_hint(
    pool: &sqlx::SqlitePool,
    item_id: &str,
) -> anyhow::Result<Option<HintReveal>> {
    next_hint_for_session(pool, item_id, None).await
}

/// Reveal the next rung for one session run. Each session has its own
/// cursor, so revealing a hint in one run cannot leak into another.
pub async fn next_hint_for_session(
    pool: &sqlx::SqlitePool,
    item_id: &str,
    session_id: Option<&str>,
) -> anyhow::Result<Option<HintReveal>> {
    let ladder = ensure_hint_ladder_for(pool, item_id, session_id).await?;
    let total = ladder.levels.len() as i64;
    let key = hint_session_key(session_id);
    let now = Utc::now();
    let mut tx = pool.begin().await?;
    let current: i64 =
        sqlx::query_scalar("SELECT level FROM hint_state WHERE session_id = ? AND item_id = ?")
            .bind(key)
            .bind(item_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or(0);
    if current >= total {
        return Ok(None);
    }
    let level = current + 1;
    sqlx::query(
        "UPDATE hint_state
         SET level = ?, uses = uses + 1, last_used_at = ?, updated_at = ?
         WHERE session_id = ? AND item_id = ?",
    )
    .bind(level)
    .bind(iso(now))
    .bind(iso(now))
    .bind(key)
    .bind(item_id)
    .execute(&mut *tx)
    .await?;
    let detail = serde_json::json!({"level": level, "total": total, "session_id": key});
    insert_session_event(
        &mut tx,
        Some(key).filter(|key| !key.is_empty()),
        &iso(now),
        "hint",
        Some(item_id),
        Some(&ladder.skill_id),
        &detail,
    )
    .await?;
    insert_intervention(
        &mut tx,
        &iso(now),
        "hint",
        Some(item_id),
        Some(&ladder.skill_id),
        Some(level),
        &detail,
    )
    .await?;
    tx.commit().await?;
    let text = ladder
        .levels
        .get((level - 1) as usize)
        .cloned()
        .unwrap_or_default();
    Ok(Some(HintReveal {
        item_id: item_id.to_string(),
        level,
        total,
        text,
        final_level: level >= total,
    }))
}

// -------------------------------------------------------------- attempts

type ScheduleRow = (f64, f64, f64, f64, i64, i64, Option<String>);
/// (skill_id, name, mastery, due_at, reviews, lapses)
type DueSkillRow = (
    String,
    String,
    Option<f64>,
    Option<String>,
    Option<i64>,
    Option<i64>,
);
/// (id, ts, kind, session_id, item_id, skill_id, hint_level, detail)
type LogEventRow = (
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    String,
);
/// (skill_id, name, mastery, stability, difficulty, reviews, lapses, due_at)
type MasteryExportRow = (
    String,
    String,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<i64>,
    Option<i64>,
    Option<String>,
);

async fn load_schedule(
    tx: &mut sqlx::SqliteConnection,
    skill_id: &str,
) -> anyhow::Result<Option<ScheduleRow>> {
    let row = sqlx::query_as::<_, ScheduleRow>(
        "SELECT stability, difficulty, mastery, accuracy, reviews, lapses, last_review
         FROM skill_schedule WHERE skill_id = ?",
    )
    .bind(skill_id)
    .fetch_optional(tx)
    .await?;
    Ok(row)
}

/// Grade an attempt at the current time.
pub async fn record_attempt(
    pool: &sqlx::SqlitePool,
    input: &AttemptInput,
) -> anyhow::Result<AttemptOutcome> {
    record_attempt_at(pool, input, Utc::now()).await
}

/// Grade an attempt at an explicit time. Used for backfills and tests; the
/// session engine calls [`record_attempt`].
pub async fn record_attempt_at(
    pool: &sqlx::SqlitePool,
    input: &AttemptInput,
    now: DateTime<Utc>,
) -> anyhow::Result<AttemptOutcome> {
    let mut tx = pool.begin().await?;
    let skill_id: String = sqlx::query_scalar("SELECT skill_id FROM items WHERE id = ?")
        .bind(&input.item_id)
        .fetch_one(&mut *tx)
        .await?;
    let hint_key = hint_session_key(input.session_id.as_deref());
    let hints_used: i64 =
        sqlx::query_scalar("SELECT level FROM hint_state WHERE session_id = ? AND item_id = ?")
            .bind(hint_key)
            .bind(&input.item_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or(0);
    let hints_used = hints_used.clamp(0, 255) as u8;
    let rating = rating_for(input.correct, hints_used);
    let schedule = load_schedule(&mut tx, &skill_id).await?;

    let (mastery_before, reviews, lapses, mut stability, mut difficulty, accuracy_before) =
        match &schedule {
            Some((s, d, mastery, accuracy, reviews, lapses, _)) => {
                (*mastery, *reviews, *lapses, *s, *d, *accuracy)
            }
            None => (UNSEEN_MASTERY, 0, 0, 0.0, 5.0, 0.4),
        };
    let elapsed_days = schedule
        .as_ref()
        .and_then(|row| row.6.as_deref())
        .and_then(parse_time)
        .map(|last| (now - last).num_seconds().max(0) as f64 / 86_400.0)
        .unwrap_or(0.0);
    let recall = retrievability(elapsed_days, stability);
    if schedule.is_some() {
        stability = next_stability(difficulty, stability, recall, rating);
        difficulty = next_difficulty(difficulty, rating);
    } else {
        stability = initial_stability(rating);
        difficulty = initial_difficulty(rating);
    }
    let accuracy = 0.75 * accuracy_before + 0.25 * if input.correct { 1.0 } else { 0.0 };
    let mastery_after = mastery_from(accuracy, stability);
    let interval = next_interval_days(stability, rating);
    let due = now + Duration::milliseconds((interval * 86_400_000.0).round() as i64);
    let due_at = iso(due);
    let reviews = reviews + 1;
    let lapses = lapses + i64::from(!input.correct);

    sqlx::query(
        "INSERT INTO skill_schedule
           (skill_id, stability, difficulty, mastery, accuracy, reviews, lapses,
            last_review, due_at, updated_at)
         VALUES(?,?,?,?,?,?,?,?,?,?)
         ON CONFLICT(skill_id) DO UPDATE SET
           stability = excluded.stability,
           difficulty = excluded.difficulty,
           mastery = excluded.mastery,
           accuracy = excluded.accuracy,
           reviews = excluded.reviews,
           lapses = excluded.lapses,
           last_review = excluded.last_review,
           due_at = excluded.due_at,
           updated_at = excluded.updated_at",
    )
    .bind(&skill_id)
    .bind(stability)
    .bind(difficulty)
    .bind(mastery_after)
    .bind(accuracy)
    .bind(reviews)
    .bind(lapses)
    .bind(iso(now))
    .bind(&due_at)
    .bind(iso(now))
    .execute(&mut *tx)
    .await?;

    let attempt_id = input
        .attempt_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if input.attempt_id.is_some() {
        sqlx::query("UPDATE attempts SET hints_used = ?, rating = ? WHERE id = ?")
            .bind(i64::from(hints_used))
            .bind(i64::from(rating.level()))
            .bind(&attempt_id)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query(
            "INSERT INTO attempts
               (id, item_id, input_kind, given, correct, error_tags, hints_used, rating, created_at)
             VALUES(?,?,?,?,?,?,?,?,?)",
        )
        .bind(&attempt_id)
        .bind(&input.item_id)
        .bind(&input.input_kind)
        .bind(&input.given)
        .bind(input.correct)
        .bind(serde_json::to_string(&input.error_tags)?)
        .bind(i64::from(hints_used))
        .bind(i64::from(rating.level()))
        .bind(iso(now))
        .execute(&mut *tx)
        .await?;
    }

    // The ladder resets for the next rep; this attempt keeps its count.
    sqlx::query(
        "UPDATE hint_state SET level = 0, updated_at = ?
         WHERE session_id = ? AND item_id = ?",
    )
    .bind(iso(now))
    .bind(hint_key)
    .bind(&input.item_id)
    .execute(&mut *tx)
    .await?;

    let mut repeated_errors = Vec::new();
    if !input.correct {
        let mut seen: Vec<&str> = Vec::new();
        for tag in &input.error_tags {
            let tag = tag.trim();
            if tag.is_empty() || seen.contains(&tag) {
                continue;
            }
            seen.push(tag);
            sqlx::query(
                "INSERT INTO error_patterns(skill_id, tag, count, first_seen, last_seen)
                 VALUES(?,?,1,?,?)
                 ON CONFLICT(skill_id, tag) DO UPDATE SET
                   count = error_patterns.count + 1,
                   last_seen = excluded.last_seen",
            )
            .bind(&skill_id)
            .bind(tag)
            .bind(iso(now))
            .bind(iso(now))
            .execute(&mut *tx)
            .await?;
            let count: i64 = sqlx::query_scalar(
                "SELECT count FROM error_patterns WHERE skill_id = ? AND tag = ?",
            )
            .bind(&skill_id)
            .bind(tag)
            .fetch_one(&mut *tx)
            .await?;
            if count >= 2 {
                repeated_errors.push(tag.to_string());
                insert_intervention(
                    &mut tx,
                    &iso(now),
                    "repeat-error",
                    Some(&input.item_id),
                    Some(&skill_id),
                    None,
                    &serde_json::json!({"tag": tag, "count": count}),
                )
                .await?;
            }
        }
    }

    let detail = serde_json::json!({
        "correct": input.correct,
        "rating": rating.level(),
        "hints_used": hints_used,
        "error_tags": input.error_tags,
        "mastery_before": mastery_before,
        "mastery_after": mastery_after,
        "due_at": due_at,
    });
    insert_session_event(
        &mut tx,
        input.session_id.as_deref(),
        &iso(now),
        "attempt",
        Some(&input.item_id),
        Some(&skill_id),
        &detail,
    )
    .await?;
    if hints_used > 0 {
        insert_intervention(
            &mut tx,
            &iso(now),
            "hint-assisted-attempt",
            Some(&input.item_id),
            Some(&skill_id),
            Some(i64::from(hints_used)),
            &detail,
        )
        .await?;
    }
    tx.commit().await?;

    Ok(AttemptOutcome {
        attempt_id,
        skill_id,
        correct: input.correct,
        rating: rating.level(),
        hints_used,
        mastery_before,
        mastery_after,
        stability,
        difficulty,
        due_score: due_score_from(mastery_after, due, now),
        due_at,
        error_tags: input.error_tags.clone(),
        repeated_errors,
    })
}

// --------------------------------------------------------------- planner

/// Planner score for one skill; higher means sooner.
pub async fn due_score(pool: &sqlx::SqlitePool, skill_id: &str) -> anyhow::Result<f64> {
    let row: Option<(f64, String)> =
        sqlx::query_as("SELECT mastery, due_at FROM skill_schedule WHERE skill_id = ?")
            .bind(skill_id)
            .fetch_optional(pool)
            .await?;
    Ok(match row {
        Some((mastery, due_at)) => {
            let due = parse_time(&due_at).unwrap_or_else(Utc::now);
            due_score_from(mastery, due, Utc::now())
        }
        None => UNSEEN_DUE_SCORE,
    })
}

/// Every skill in planner order, overdue and weak skills first.
pub async fn due_skills(pool: &sqlx::SqlitePool) -> anyhow::Result<Vec<DueSkill>> {
    let rows: Vec<DueSkillRow> = sqlx::query_as(
        "SELECT s.id, s.name, sch.mastery, sch.due_at, sch.reviews, sch.lapses
             FROM skills s LEFT JOIN skill_schedule sch ON sch.skill_id = s.id
             ORDER BY s.id",
    )
    .fetch_all(pool)
    .await?;
    let now = Utc::now();
    let mut skills: Vec<DueSkill> = rows
        .into_iter()
        .map(|(skill_id, name, mastery, due_at, reviews, lapses)| {
            let mastery = mastery.unwrap_or(UNSEEN_MASTERY);
            let due_score = match due_at.as_deref() {
                Some(raw) => due_score_from(mastery, parse_time(raw).unwrap_or(now), now),
                None => UNSEEN_DUE_SCORE,
            };
            DueSkill {
                skill_id,
                name,
                mastery,
                due_at,
                due_score,
                reviews: reviews.unwrap_or(0),
                lapses: lapses.unwrap_or(0),
            }
        })
        .collect();
    skills.sort_by(|a, b| {
        b.due_score
            .total_cmp(&a.due_score)
            .then_with(|| a.skill_id.cmp(&b.skill_id))
    });
    Ok(skills)
}

// ------------------------------------------------------------------ logs

/// Error tags aggregated per skill, newest activity first. `since` is an
/// ISO timestamp filtering on `last_seen`.
pub async fn error_patterns(
    pool: &sqlx::SqlitePool,
    since: Option<&str>,
) -> anyhow::Result<Vec<ErrorPattern>> {
    let rows: Vec<(String, String, i64, String, String)> = sqlx::query_as(
        "SELECT skill_id, tag, count, first_seen, last_seen FROM error_patterns
         WHERE (? IS NULL OR last_seen >= ?)
         ORDER BY count DESC, tag",
    )
    .bind(since)
    .bind(since)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(skill_id, tag, count, first_seen, last_seen)| ErrorPattern {
                skill_id,
                tag,
                count,
                first_seen,
                last_seen,
            },
        )
        .collect())
}

/// Error tags for one skill.
pub async fn error_patterns_for_skill(
    pool: &sqlx::SqlitePool,
    skill_id: &str,
) -> anyhow::Result<Vec<ErrorPattern>> {
    let rows: Vec<(String, i64, String, String)> = sqlx::query_as(
        "SELECT tag, count, first_seen, last_seen FROM error_patterns
         WHERE skill_id = ? ORDER BY count DESC, tag",
    )
    .bind(skill_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(tag, count, first_seen, last_seen)| ErrorPattern {
            skill_id: skill_id.to_string(),
            tag,
            count,
            first_seen,
            last_seen,
        })
        .collect())
}

async fn insert_session_event(
    conn: &mut sqlx::SqliteConnection,
    session_id: Option<&str>,
    ts: &str,
    kind: &str,
    item_id: Option<&str>,
    skill_id: Option<&str>,
    detail: &serde_json::Value,
) -> anyhow::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO session_log(session_id, ts, kind, item_id, skill_id, detail)
         VALUES(?,?,?,?,?,?)",
    )
    .bind(session_id)
    .bind(ts)
    .bind(kind)
    .bind(item_id)
    .bind(skill_id)
    .bind(detail.to_string())
    .execute(conn)
    .await?;
    Ok(result.last_insert_rowid())
}

async fn insert_intervention(
    conn: &mut sqlx::SqliteConnection,
    ts: &str,
    kind: &str,
    item_id: Option<&str>,
    skill_id: Option<&str>,
    hint_level: Option<i64>,
    detail: &serde_json::Value,
) -> anyhow::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO intervention_log(ts, kind, item_id, skill_id, hint_level, detail)
         VALUES(?,?,?,?,?,?)",
    )
    .bind(ts)
    .bind(kind)
    .bind(item_id)
    .bind(skill_id)
    .bind(hint_level)
    .bind(detail.to_string())
    .execute(conn)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Append a session lifecycle event (`session-start`, `skip`, `bailed`,
/// `pause-nudge`) for the weekly review.
pub async fn log_session_event(
    pool: &sqlx::SqlitePool,
    session_id: Option<&str>,
    kind: &str,
    item_id: Option<&str>,
    skill_id: Option<&str>,
    detail: serde_json::Value,
) -> anyhow::Result<i64> {
    let mut conn = pool.acquire().await?;
    insert_session_event(
        &mut conn,
        session_id,
        &iso(Utc::now()),
        kind,
        item_id,
        skill_id,
        &detail,
    )
    .await
}

/// Append an intervention event the weekly review should see.
pub async fn log_intervention(
    pool: &sqlx::SqlitePool,
    kind: &str,
    item_id: Option<&str>,
    skill_id: Option<&str>,
    hint_level: Option<i64>,
    detail: serde_json::Value,
) -> anyhow::Result<i64> {
    let mut conn = pool.acquire().await?;
    insert_intervention(
        &mut conn,
        &iso(Utc::now()),
        kind,
        item_id,
        skill_id,
        hint_level,
        &detail,
    )
    .await
}

fn log_event_from_columns(row: LogEventRow) -> LogEvent {
    let (id, ts, kind, session_id, item_id, skill_id, hint_level, detail) = row;
    LogEvent {
        id,
        ts,
        kind,
        session_id,
        item_id,
        skill_id,
        hint_level,
        detail: serde_json::from_str(&detail).unwrap_or(serde_json::Value::Null),
    }
}

/// Session, intervention, error and mastery data for the weekly review.
/// `since` is an ISO timestamp; `None` reads the full history.
pub async fn weekly_snapshot(
    pool: &sqlx::SqlitePool,
    since: Option<&str>,
) -> anyhow::Result<WeeklySnapshot> {
    let session_rows: Vec<LogEventRow> = sqlx::query_as(
        "SELECT id, ts, kind, session_id, item_id, skill_id, NULL, detail
         FROM session_log WHERE (? IS NULL OR ts >= ?) ORDER BY id",
    )
    .bind(since)
    .bind(since)
    .fetch_all(pool)
    .await?;
    let intervention_rows: Vec<LogEventRow> = sqlx::query_as(
        "SELECT id, ts, kind, NULL, item_id, skill_id, hint_level, detail
         FROM intervention_log WHERE (? IS NULL OR ts >= ?) ORDER BY id",
    )
    .bind(since)
    .bind(since)
    .fetch_all(pool)
    .await?;
    let mastery_rows: Vec<MasteryExportRow> = sqlx::query_as(
        "SELECT s.id, s.name, sch.mastery, sch.stability, sch.difficulty,
                sch.reviews, sch.lapses, sch.due_at
         FROM skills s LEFT JOIN skill_schedule sch ON sch.skill_id = s.id
         ORDER BY s.id",
    )
    .fetch_all(pool)
    .await?;
    Ok(WeeklySnapshot {
        since: since.map(str::to_string),
        sessions: session_rows
            .into_iter()
            .map(log_event_from_columns)
            .collect(),
        interventions: intervention_rows
            .into_iter()
            .map(log_event_from_columns)
            .collect(),
        errors: error_patterns(pool, since).await?,
        mastery: mastery_rows
            .into_iter()
            .map(
                |(skill_id, name, mastery, stability, difficulty, reviews, lapses, due_at)| {
                    MasteryRow {
                        skill_id,
                        name,
                        mastery: mastery.unwrap_or(UNSEEN_MASTERY),
                        stability: stability.unwrap_or(0.0),
                        difficulty: difficulty.unwrap_or(5.0),
                        reviews: reviews.unwrap_or(0),
                        lapses: lapses.unwrap_or(0),
                        due_at,
                    }
                },
            )
            .collect(),
    })
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn rating_reflects_hint_use() {
        assert_eq!(rating_for(true, 0), Rating::Easy);
        assert_eq!(rating_for(true, 1), Rating::Good);
        assert_eq!(rating_for(true, 2), Rating::Hard);
        assert_eq!(rating_for(true, 9), Rating::Hard);
        assert_eq!(rating_for(false, 0), Rating::Again);
    }

    #[test]
    fn failing_lowers_stability() {
        let s = initial_stability(Rating::Good);
        let d = initial_difficulty(Rating::Good);
        let recall = retrievability(1.0, s);
        let remembered = next_stability(d, s, recall, Rating::Good);
        let lapsed = next_stability(d, s, recall, Rating::Again);
        assert!(remembered > s);
        assert!(lapsed < s);
        assert!(lapsed > 0.0);
    }

    #[test]
    fn easy_review_waits_longer_than_a_lapse() {
        let s = initial_stability(Rating::Easy);
        let easy = next_interval_days(s, Rating::Easy);
        let lapse = next_interval_days(s, Rating::Again);
        assert!(easy >= 1.0);
        assert!((lapse - 10.0 / 1440.0).abs() < 1e-12);
        assert!(easy > lapse);
    }

    #[test]
    fn retrievability_decays_over_time() {
        let fresh = retrievability(0.0, 10.0);
        let later = retrievability(10.0, 10.0);
        let stale = retrievability(30.0, 10.0);
        assert!((fresh - 1.0).abs() < 1e-12);
        assert!(fresh > later && later > stale);
    }

    #[test]
    fn mastery_moves_with_accuracy_and_stability() {
        let low = mastery_from(0.1, 0.2);
        let high = mastery_from(0.9, 20.0);
        assert!(low < high);
        assert!((0.05..=1.0).contains(&low));
        assert!((0.05..=1.0).contains(&high));
    }

    #[test]
    fn overdue_and_weak_skills_score_higher() {
        let now = Utc::now();
        let due_now = due_score_from(0.5, now, now);
        let overdue = due_score_from(0.5, now - Duration::days(7), now);
        let future = due_score_from(0.5, now + Duration::days(7), now);
        let weak = due_score_from(0.1, now, now);
        assert!(overdue > due_now);
        assert!(due_now > future);
        assert!(weak > due_now);
    }

    #[test]
    fn authored_ladder_escalates_and_withholds_the_answer() {
        let worked = vec![
            "V = pi*r^2*h = pi*(4^2)*(10)".to_string(),
            "= 160*pi ≈ 502.7".to_string(),
        ];
        let ladder = author_ladder("volume of a cylinder", "502.7 cm3", &worked);
        assert_eq!(ladder.len(), MIN_HINT_LEVELS);
        validate_ladder(&ladder, "502.7 cm3").unwrap();
        assert!(ladder[2].contains("502.7"));
        assert!(!ladder[0].contains("502.7"));
        assert!(!ladder[1].contains("502.7"));
    }

    #[test]
    fn validation_rejects_leaks_and_flat_ladders() {
        let leaking = vec![
            "Think about the answer 502.7".to_string(),
            "Use the formula and multiply".to_string(),
            "Finish the calculation".to_string(),
        ];
        assert!(validate_ladder(&leaking, "502.7").is_err());
        let flat = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert!(validate_ladder(&flat, "502.7").is_err());
        let short = vec!["a hint".to_string(), "another hint".to_string()];
        assert!(validate_ladder(&short, "502.7").is_err());
    }
}
