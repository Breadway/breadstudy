//! Item bank verification gate (STUDY-2).
//!
//! Every item passes through this gate before it may enter the bank in
//! `verified` state:
//!
//! 1. Dedupe: a normalized hash of the prompt rejects repeats.
//! 2. Source rule: generated items must record their searxng source
//!    (URL, snippet, fetch date). Gold vault items are exempt.
//! 3. Kind rule: proofs and geometric reasoning without numeric claims
//!    are unverifiable by SymPy and default to `review_required` until
//!    a human approves them once via [`approve_item`].
//! 4. SymPy chain check: every numeric equality link inside the worked
//!    steps must hold (exactly or within rounding tolerance), and the
//!    final answer must match the last worked step. A wrong worked
//!    answer therefore cannot enter `verified` state.
//!
//! SymPy runs through the system `python3`. If it is unavailable the
//! item fails closed to `review_required`, never to `verified`.

use serde::{Deserialize, Serialize};

/// Where an item came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// Transcribed from the vault Practice sets.
    Gold,
    /// Produced by a model; sources are mandatory.
    Generated,
}

/// Web source record required for every generated item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRecord {
    pub url: String,
    pub snippet: String,
    pub fetched_at: String,
}

impl SourceRecord {
    pub fn complete(&self) -> bool {
        !self.url.trim().is_empty()
            && !self.snippet.trim().is_empty()
            && !self.fetched_at.trim().is_empty()
    }
}

/// A candidate item for the bank.
#[derive(Debug, Clone)]
pub struct NewItem {
    pub skill_id: String,
    pub prompt: String,
    /// Concise final answer, e.g. "502.7 cm3" or "10sqrt2 cm (approx 14.1 cm)".
    pub answer: String,
    /// Worked solution lines, each expected to chain equalities.
    pub worked: Vec<String>,
    pub origin: Origin,
    pub source: Option<SourceRecord>,
}

/// The gate decision for one item.
#[derive(Debug, Clone)]
pub enum GateDecision {
    Verified { method: String, detail: serde_json::Value },
    ReviewRequired { reason: String, detail: serde_json::Value },
    Duplicate { of: String },
}

impl GateDecision {
    pub fn status(&self) -> &'static str {
        match self {
            GateDecision::Verified { .. } => "verified",
            GateDecision::ReviewRequired { .. } => "review_required",
            GateDecision::Duplicate { .. } => "duplicate",
        }
    }
}

/// One stored item row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredItem {
    pub id: String,
    pub skill_id: String,
    pub prompt: String,
    pub answer: String,
    pub status: String,
    pub verify_method: String,
    pub verify_detail: String,
    pub source_url: Option<String>,
}

/// Fold unicode math and case/whitespace for stable hashing.
fn fold_text(s: &str) -> String {
    s.replace('π', "pi")
        .replace('×', "*")
        .replace('−', "-")
        .replace('÷', "/")
        .replace('≈', "~=")
        .replace('²', "2")
        .replace('³', "3")
        .replace('√', "sqrt")
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// Normalized hash of the prompt used for dedupe.
pub fn dedupe_hash(prompt: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    fold_text(prompt).hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Prompts that ask for proof or reasoning rather than a value.
fn is_unverifiable_kind(prompt: &str) -> bool {
    let p = prompt.to_lowercase();
    [
        "prove",
        "proof",
        "show that",
        "explain why",
        "justify",
        "discuss ",
        "derive ",
    ]
    .iter()
    .any(|k| p.contains(k))
}

fn has_digit(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit())
}

/// Next non-blank character at or after `i + 1`, plus how many blanks
/// were skipped (so the caller can advance past them).
fn next_nonblank(i: usize, chars: &[char]) -> (Option<char>, usize) {
    let mut skipped = 0;
    while chars.get(i + 1 + skipped) == Some(&' ') || chars.get(i + 1 + skipped) == Some(&'\t') {
        skipped += 1;
    }
    (chars.get(i + 1 + skipped).copied(), skipped)
}

/// Expand `√` markers into `sqrt(...)` calls before the main pass.
/// A group (`√(...)`) keeps its parentheses; a bare number (`√169`,
/// `5√2`) is wrapped, with `*` inserted after a preceding value.
fn expand_sqrt(s: &str) -> String {
    let mut out = s.to_string();
    while let Some(pos) = out.find('√') {
        let prev_val = out[..pos].chars().last().map(|p| {
            p.is_ascii_digit() || p == ')' || p == '.'
        }).unwrap_or(false);
        let after = pos + '√'.len_utf8();
        let rest = &out[after..];
        if rest.starts_with('(') {
            let mut depth = 0;
            let mut end = None;
            for (j, c) in rest.char_indices() {
                if c == '(' {
                    depth += 1;
                } else if c == ')' {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(after + j + 1);
                        break;
                    }
                }
            }
            let Some(end) = end else { break };
            let group = &out[after..end];
            let star = if prev_val { "*" } else { "" };
            out.replace_range(pos..end, &format!("{star}sqrt({group})"));
        } else {
            let mut digits = 0;
            for c in rest.chars() {
                if c.is_ascii_digit() || c == '.' {
                    digits += c.len_utf8();
                } else {
                    break;
                }
            }
            if digits == 0 {
                break;
            }
            let end = after + digits;
            let num = &out[after..end];
            let star = if prev_val { "*" } else { "" };
            out.replace_range(pos..end, &format!("{star}sqrt({num})"));
        }
    }
    out
}
/// Normalize one candidate expression for SymPy.
///
/// Superscripts apply only to values (`6²` becomes `6**2`); a
/// superscript on a unit (`m²`) is dropped with the unit. A bare `m`
/// right after a value is the meter unit (`1.4m`, `4.5m**2`), while
/// `2m+3` keeps its variable. ASCII `x` is multiplication only
/// between values.
fn normalize_expr(raw: &str) -> String {
    let raw = raw
        .replace('π', "pi")
        .replace('×', "*")
        .replace('−', "-")
        .replace('÷', "/")
        .replace('^', "**")
        .replace(['°', '✓', '✗'], "")
        // Squared/cubed units are still units: drop the marker early so
        // "cm²" strips as "cm" while "6²" still becomes "6**2" below.
        .replace("cm²", "cm")
        .replace("cm³", "cm")
        .replace("mm²", "mm")
        .replace("mm³", "mm")
        .replace("km²", "km")
        .replace("m²", "m")
        .replace("m³", "m");
    // Drop parenthetical remarks without digits, e.g. "(multiply by ...)".
    let mut s = expand_sqrt(&raw);
    while let Some(start) = s.find('(') {
        let Some(rel) = s[start..].find(')') else {
            break;
        };
        let end = start + rel;
        let group = &s[start + 1..end];
        if group.chars().any(|c| c.is_ascii_digit()) {
            break;
        }
        s.replace_range(start..=end, " ");
    }
    // Per-character pass: superscripts, square roots, unit letters.
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 8);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let prev = out.chars().last();
        let prev_val =
            matches!(prev, Some(p) if p.is_ascii_digit() || p == ')' || p == '.');
        match c {
            '²' | '³' => {
                let exp = if c == '²' { "**2" } else { "**3" };
                if prev_val {
                    out.push_str(exp);
                }
                // Else: superscript on a unit remnant or variable, drop it.
            }
            'x' => {
                let (next, skip) = next_nonblank(i, &chars);
                i += skip;
                let next_val = matches!(next, Some(n) if n.is_ascii_digit() || n == '(' || n == '.')
                    || matches!(next, Some('p') | Some('s'));
                if prev_val && next_val {
                    out.push('*');
                } else {
                    out.push('x');
                }
            }
            'm' => {
                let (next, skip) = next_nonblank(i, &chars);
                i += skip;
                if prev_val
                    && matches!(next, None | Some('*') | Some('/') | Some(')'))
                {
                    // Meter unit after a value: drop. The blank scan
                    // above already consumed the gap.
                } else {
                    out.push('m');
                }
            }
            c if c.is_whitespace() => {}
            _ => out.push(c),
        }
        i += 1;
    }
    // Multi-letter units, longest first.
    for unit in [
        "cm3", "cm2", "mm3", "mm2", "m3", "m2", "kL", "mL", "cm", "mm", "km",
        "degrees", "degree",
    ] {
        out = out.replace(unit, "");
    }
    // Leftover trailing single-letter units.
    while out.ends_with('m') || out.ends_with('L') {
        out.pop();
    }
    let out = out.trim_start_matches(['=', '~', ':']);
    out.trim_end_matches(['=', '~', ',', ';', '.', ':'])
        .to_string()
}

/// Split worked text into candidate expressions at equality links.
fn candidates(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for eq in text.replace('≈', "=").split('=') {
        let n = normalize_expr(eq);
        if n.is_empty() {
            continue;
        }
        out.push(n);
    }
    out
}

fn decimals(s: &str) -> u32 {
    s.split('.').nth(1).map(|f| f.len() as u32).unwrap_or(0)
}

/// Rounding tolerance for a pair: half a unit in the last place of the
/// less precise side, so 1-decimal worked answers verify.
fn tolerance(a: &str, b: &str) -> f64 {
    let d = decimals(a).min(decimals(b));
    (0.51 * 10f64.powi(-(d as i32))).max(1e-9)
}

const SYMPY_SCRIPT: &str = r#"
import sympy as sp, sys
def parse(s):
    return sp.sympify(s, rational=True)
try:
    a = parse(sys.argv[1])
    b = parse(sys.argv[2])
except Exception as e:
    print("error")
    print("parse failed: " + str(e))
    raise SystemExit
tol = float(sys.argv[3])
allowed = {sp.pi, sp.E}
if (a.free_symbols | b.free_symbols) - allowed:
    print("uncheckable")
    raise SystemExit
try:
    if sp.simplify(a - b) == 0:
        print("exact")
        raise SystemExit
    da = float(sp.N(a))
    db = float(sp.N(b))
except Exception as e:
    print("error")
    print("eval failed: " + str(e))
    raise SystemExit
print("approx" if abs(da - db) <= tol else "different")
"#;

/// Compare two candidate expressions with SymPy.
async fn sympy_compare(a: &str, b: &str) -> SympyVerdict {
    let tol = tolerance(a, b).to_string();
    let run = tokio::process::Command::new("python3")
        .args(["-c", SYMPY_SCRIPT, a, b, &tol])
        .output();
    let out = match tokio::time::timeout(std::time::Duration::from_secs(60), run).await {
        Ok(Ok(o)) => o,
        _ => return SympyVerdict::Error("sympy-unavailable: python3 failed".into()),
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("No module named sympy") {
            return SympyVerdict::Error("sympy-unavailable: no sympy module".into());
        }
        return SympyVerdict::Error(format!("sympy failed: {}", err.trim()));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    match lines.next().unwrap_or("").trim() {
        "exact" => SympyVerdict::Agree(true),
        "approx" => SympyVerdict::Agree(false),
        "different" => SympyVerdict::Disagree,
        "uncheckable" => SympyVerdict::Skip,
        "error" => SympyVerdict::Error(
            lines.next().unwrap_or("unknown sympy error").to_string(),
        ),
        other => SympyVerdict::Error(format!("unexpected sympy output: {other}")),
    }
}

enum SympyVerdict {
    Agree(bool),
    Disagree,
    Skip,
    Error(String),
}

/// Check one chain of candidates; every checkable link must hold.
async fn check_chain(cands: &[String]) -> Result<Vec<serde_json::Value>, String> {
    let mut log = Vec::new();
    let mut checked = 0;
    for pair in cands.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        match sympy_compare(a, b).await {
            SympyVerdict::Agree(exact) => {
                checked += 1;
                log.push(serde_json::json!({"link": [a, b], "result": if exact {"exact"} else {"approx"}}));
            }
            SympyVerdict::Skip => {
                log.push(serde_json::json!({"link": [a, b], "result": "skipped-symbolic"}));
            }
            SympyVerdict::Disagree => {
                log.push(serde_json::json!({"link": [a, b], "result": "MISMATCH"}));
                return Err(format!("sympy mismatch: {a} != {b}"));
            }
            SympyVerdict::Error(e) => {
                return Err(format!("sympy error on {a} vs {b}: {e}"));
            }
        }
    }
    if checked == 0 {
        return Err("no checkable numeric links".into());
    }
    Ok(log)
}

/// Run the gate without writing anything.
pub async fn evaluate(
    pool: &sqlx::SqlitePool,
    item: &NewItem,
) -> anyhow::Result<GateDecision> {
    let hash = dedupe_hash(&item.prompt);
    let existing: Option<String> =
        sqlx::query_scalar("SELECT id FROM items WHERE dedupe_hash = ?")
            .bind(&hash)
            .fetch_optional(pool)
            .await?;
    if let Some(id) = existing {
        return Ok(GateDecision::Duplicate { of: id });
    }
    if item.origin == Origin::Generated {
        let ok = item.source.as_ref().map(|s| s.complete()).unwrap_or(false);
        if !ok {
            return Ok(GateDecision::ReviewRequired {
                reason: "missing-source".into(),
                detail: serde_json::json!({
                    "rule": "every generated item must record searxng URL, snippet and fetch date",
                    "source": item.source,
                }),
            });
        }
    }
    let all_text = format!("{} {}", item.answer, item.worked.join(" "));
    if is_unverifiable_kind(&item.prompt) || !has_digit(&all_text) {
        return Ok(GateDecision::ReviewRequired {
            reason: "unverifiable-kind".into(),
            detail: serde_json::json!({
                "rule": "proofs, reasoning and non-numeric answers need one manual approval",
            }),
        });
    }
    for step in &item.worked {
        let cands = candidates(step);
        if cands.len() < 2 {
            continue;
        }
        if let Err(e) = check_chain(&cands).await {
            return Ok(GateDecision::ReviewRequired {
                reason: "sympy-mismatch".into(),
                detail: serde_json::json!({"step": step, "error": e}),
            });
        }
    }
    let answer_cands = candidates(&item.answer);
    if answer_cands.is_empty() {
        return Ok(GateDecision::ReviewRequired {
            reason: "sympy-error".into(),
            detail: serde_json::json!({"error": "final answer has no parseable math"}),
        });
    }
    let last_step_cands = item
        .worked
        .last()
        .map(|s| candidates(s))
        .unwrap_or_default();
    let mut joined = last_step_cands.clone();
    joined.extend(answer_cands.clone());
    let joint_log = if joined.len() >= 2 {
        match check_chain(&joined).await {
            Ok(log) => log,
            Err(e) => {
                return Ok(GateDecision::ReviewRequired {
                    reason: "sympy-mismatch".into(),
                    detail: serde_json::json!({
                        "error": format!("final answer does not match worked steps: {e}"),
                        "answer": item.answer,
                    }),
                });
            }
        }
    } else if last_step_cands.iter().any(|c| answer_cands.contains(c)) {
        vec![serde_json::json!({"link": "answer repeats worked value"})]
    } else {
        return Ok(GateDecision::ReviewRequired {
            reason: "sympy-mismatch".into(),
            detail: serde_json::json!({
                "error": "final answer does not match any worked value",
                "answer": item.answer,
            }),
        });
    };
    Ok(GateDecision::Verified {
        method: "sympy-chain".into(),
        detail: serde_json::json!({
            "final_links": joint_log,
            "source_recorded": item.source.is_some(),
        }),
    })
}

/// Gate an item and store it with the resulting status. Duplicates are
/// not stored twice; the existing id is returned.
pub async fn store_item(pool: &sqlx::SqlitePool, item: &NewItem) -> anyhow::Result<StoredItem> {
    let decision = evaluate(pool, item).await?;
    if let GateDecision::Duplicate { of } = decision {
        return get_item(pool, &of).await;
    }
    let (status, method, detail) = match &decision {
        GateDecision::Verified { method, detail } => ("verified", method.clone(), detail.clone()),
        GateDecision::ReviewRequired { reason, detail } => (
            "review_required",
            format!("gate:{reason}"),
            detail.clone(),
        ),
        GateDecision::Duplicate { .. } => unreachable!(),
    };
    let id = uuid::Uuid::new_v4().to_string();
    let hash = dedupe_hash(&item.prompt);
    let rubric = serde_json::to_string(&item.worked)?;
    let (source_url, source_snippet, fetched_at) = match &item.source {
        Some(s) => (
            Some(s.url.clone()),
            Some(s.snippet.clone()),
            Some(s.fetched_at.clone()),
        ),
        None => (None, None, None),
    };
    sqlx::query(
        "INSERT INTO items(id, skill_id, prompt, answer, rubric, hints, source_url, source_snippet, fetched_at, status, verify_method, verify_detail, dedupe_hash)
         VALUES(?,?,?,?,?,'[]',?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&item.skill_id)
    .bind(&item.prompt)
    .bind(&item.answer)
    .bind(&rubric)
    .bind(&source_url)
    .bind(&source_snippet)
    .bind(&fetched_at)
    .bind(status)
    .bind(&method)
    .bind(detail.to_string())
    .bind(&hash)
    .execute(pool)
    .await?;
    get_item(pool, &id).await
}

pub async fn get_item(pool: &sqlx::SqlitePool, id: &str) -> anyhow::Result<StoredItem> {
    let row: (String, String, String, String, String, String, String, Option<String>) = sqlx::query_as(
        "SELECT id, skill_id, prompt, answer, status, verify_method, verify_detail, source_url FROM items WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(StoredItem {
        id: row.0,
        skill_id: row.1,
        prompt: row.2,
        answer: row.3,
        status: row.4,
        verify_method: row.5,
        verify_detail: row.6,
        source_url: row.7,
    })
}

/// The "approved once" path for `review_required` items: records a
/// manual approval, never silently flips state.
pub async fn approve_item(
    pool: &sqlx::SqlitePool,
    id: &str,
    note: &str,
) -> anyhow::Result<StoredItem> {
    let item = get_item(pool, id).await?;
    if item.status != "review_required" {
        anyhow::bail!("item {id} is {}: only review_required items can be approved", item.status);
    }
    let detail = serde_json::json!({"approval": note, "was": item.verify_detail});
    sqlx::query(
        "UPDATE items SET status = 'verified', verify_method = 'manual-approval', verify_detail = ? WHERE id = ?",
    )
    .bind(detail.to_string())
    .bind(id)
    .execute(pool)
    .await?;
    get_item(pool, id).await
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn dedupe_ignores_case_and_whitespace() {
        let a = dedupe_hash("Find the volume of a cylinder, r = 4 cm.");
        let b = dedupe_hash("  find THE volume of a cylinder, r=4 cm. ");
        assert_eq!(a, b);
        assert_ne!(a, dedupe_hash("Find the area of a circle."));
    }

    #[test]
    fn classifies_proofs_as_unverifiable() {
        assert!(is_unverifiable_kind("Prove that the sum is 180 degrees."));
        assert!(!is_unverifiable_kind("Find the volume of a cone."));
    }

    #[test]
    fn normalizes_cone_math() {
        assert_eq!(normalize_expr("90π"), "90pi");
        assert_eq!(normalize_expr("282.7 cm²"), "282.7");
        assert_eq!(normalize_expr("√(25 + 144)"), "sqrt((25+144))");
        assert_eq!(normalize_expr("10√2 cm"), "10*sqrt(2)");
        assert_eq!(normalize_expr("4.5 m² × 10 000"), "4.5*10000");
        // Superscripts on values stay powers; on units they drop.
        assert_eq!(normalize_expr("6²"), "6**2");
        assert_eq!(normalize_expr("1.4m"), "1.4");
        // Variables are preserved, never eaten as units.
        assert_eq!(normalize_expr("2m+3"), "2m+3");
    }

    #[test]
    fn splits_equality_chains() {
        let c = candidates("V = (1/3)*pi*(25)*(12) = 100*pi ≈ 314.2");
        assert_eq!(c, vec!["V", "(1/3)*pi*(25)*(12)", "100*pi", "314.2"]);
    }

    #[test]
    fn rounding_tolerance_covers_one_decimal() {
        assert!((tolerance("314.159", "314.2") - 0.051).abs() < 1e-12);
    }
}
