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
//!    steps must hold (exactly or within rounding tolerance;
//!    unparseable prose-heavy links are skipped with a note, never
//!    treated as agreement). Each final-answer alternative must appear
//!    among the worked values, and at least one link overall must
//!    agree. A wrong worked answer therefore cannot enter `verified`
//!    state.
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

/// Known units, longest first. Used to suppress the implicit
/// multiplication star when a letter after a value starts a unit
/// (`7cm` stays `7cm` for the unit pass, not `7*cm`).
const UNITS: [&str; 16] = [
    "cm3", "cm2", "mm3", "mm2", "m3", "m2", "kL", "mL", "cm", "mm", "km",
    "degrees", "degree", "m", "L", "s",
];

/// Length of the unit starting at `rest`, requiring a non-alphanumeric
/// boundary after it (`2m+3` keeps its variable; `2m` is meters).
fn unit_len_at(rest: &[char]) -> usize {
    for u in UNITS {
        let uc: Vec<char> = u.chars().collect();
        if rest.len() >= uc.len() && rest[..uc.len()] == uc[..] {
            let after = rest.get(uc.len()).copied();
            if after.is_none() || !after.unwrap().is_ascii_alphanumeric() {
                return uc.len();
            }
        }
    }
    0
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

/// Expand superscript runs into `**` powers, but only after a value.
/// `6²` becomes `6**2` and `10⁻⁵` becomes `10**-5`; on a bare
/// variable (`x²`, `r²`) it is also a power, while a measured unit
/// (`4.5m²`) drops with the unit.
fn expand_superscripts(s: &str) -> String {
    fn sup_map(c: char) -> Option<char> {
        match c {
            '⁰' => Some('0'),
            '¹' => Some('1'),
            '²' => Some('2'),
            '³' => Some('3'),
            '⁴' => Some('4'),
            '⁵' => Some('5'),
            '⁶' => Some('6'),
            '⁷' => Some('7'),
            '⁸' => Some('8'),
            '⁹' => Some('9'),
            '⁻' => Some('-'),
            _ => None,
        }
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if sup_map(chars[i]).is_some() {
            let mut run = String::new();
            while i < chars.len() {
                if let Some(m) = sup_map(chars[i]) {
                    run.push(m);
                    i += 1;
                } else {
                    break;
                }
            }
            let prev_val = out
                .chars()
                .last()
                .map(|p| p.is_ascii_digit() || p == ')' || p == '.')
                .unwrap_or(false);
            if prev_val {
                if !out.ends_with("**") {
                    out.push_str("**");
                }
                out.push_str(&run);
            } else {
                // A superscript on a measured unit (`4.5m²`) drops with
                // the unit; on a variable (`x²`, `r²`) it is a power.
                let unit_after_value = out.len() >= 2
                    && matches!(out.chars().last(), Some('m') | Some('L'))
                    && out[..out.len() - 1]
                        .chars()
                        .last()
                        .is_some_and(|p| p.is_ascii_digit());
                if !unit_after_value {
                    out.push_str("**");
                    out.push_str(&run);
                }
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Rewrite `N%` as `(N/100)` so percentages compare numerically.
fn expand_percent(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() || (c == '.' && out.chars().last().is_some_and(|p| p.is_ascii_digit()))
        {
            let start = out.len();
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                out.push(chars[i]);
                i += 1;
            }
            if chars.get(i) == Some(&'%') {
                let num = out[start..].to_string();
                out.truncate(start);
                out.push_str(&format!("({num}/100)"));
                i += 1;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Convert forward-trig degree angles to radians: `sin 28°` becomes
/// `sin(28*pi/180)`. Inverse trig (`asin`, `atan`, ...) passes through:
/// ratio arguments stay as-is and degree results convert in
/// [`candidates`].
fn convert_trig_degrees(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let mut func: Option<&str> = None;
        for f in ["asin", "acos", "atan", "sin", "cos", "tan"] {
            let fc: Vec<char> = f.chars().collect();
            if chars.len() >= i + fc.len()
                && chars[i..i + fc.len()] == fc[..]
                && !(fc.len() == 3 && i > 0 && chars[i - 1] == 'a')
            {
                func = Some(f);
                break;
            }
        }
        let Some(f) = func else {
            out.push(chars[i]);
            i += 1;
            continue;
        };
        if f.len() == 4 {
            out.push_str(f);
            i += 4;
            continue;
        }
        let mut j = i + 3;
        while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
            j += 1;
        }
        let opened = if chars.get(j) == Some(&'(') {
            j += 1;
            true
        } else {
            false
        };
        while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
            j += 1;
        }
        let num_start = j;
        while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == '.') {
            j += 1;
        }
        if num_start < j && chars.get(j) == Some(&'°') {
            let num: String = chars[num_start..j].iter().collect();
            out.push_str(f);
            out.push('(');
            out.push_str(&num);
            out.push_str("*pi/180)");
            if opened {
                out.push(')');
            }
            i = j + 1;
        } else {
            out.push_str(f);
            i += 3;
        }
    }
    out
}
/// Squared/cubed units are still units: drop the power early so `^`
/// never mints a fake `**` on them (`cm^2` would corrupt).
fn strip_unit_powers(s: &str) -> String {
    let mut out = s.to_string();
    for (pow, unit) in [
        ("cm^2", "cm"),
        ("cm^3", "cm"),
        ("mm^2", "mm"),
        ("mm^3", "mm"),
        ("km^2", "km"),
        ("km^3", "km"),
        ("m^2", "m"),
        ("m^3", "m"),
        ("cm²", "cm"),
        ("cm³", "cm"),
        ("mm²", "mm"),
        ("mm³", "mm"),
        ("km²", "km"),
        ("km³", "km"),
        ("m²", "m"),
        ("m³", "m"),
    ] {
        out = out.replace(pow, unit);
    }
    out
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
    // Inverse trig names first, so the degree scanner below never sees
    // a bare "sin" inside "sin⁻¹".
    let raw = raw
        .replace("sin⁻¹", "asin")
        .replace("cos⁻¹", "acos")
        .replace("tan⁻¹", "atan")
        .replace("sin^-1", "asin")
        .replace("cos^-1", "acos")
        .replace("tan^-1", "atan")
        .replace("arcsin", "asin")
        .replace("arccos", "acos")
        .replace("arctan", "atan");
    // Forward-trig degree angles before `°` is stripped below.
    let raw = convert_trig_degrees(&raw);
    // Unit powers before `^` mints fake `**` on them below.
    let raw = strip_unit_powers(&raw);
    let raw = raw
        // Markdown bold first, before `^` mints new `**` powers below.
        .replace("**", "")
        .replace('π', "pi")
        // `pi(` is the constant applied to a group, never a call.
        .replace("pi(", "pi*(")
        .replace('×', "*")
        .replace('−', "-")
        .replace('÷', "/")
        .replace('^', "**")
        .replace(['°', '✓', '✗'], "")
        // Money/units that never reach SymPy.
        .replace('$', "")
        .replace("p.a.", "");
    // Squared/cubed ASCII units still strip as units below.
    let s = expand_superscripts(&raw);
    // Drop parenthetical remarks without digits, e.g. "(multiply by ...)".
    let s = expand_sqrt(&s);
    let s = expand_percent(&s);
    let mut s = s;
    while let Some(start) = s.find('(') {
        let Some(rel) = s[start..].find(')') else {
            break;
        };
        let end = start + rel;
        let group = &s[start + 1..end];
        if group.chars().any(|c| c.is_ascii_digit()) {
            break;
        }
        // Groups glued to an identifier are function arguments
        // (`P(A∩B)`, `Solution(3,5)`), not remarks: keep them, unless
        // they read as prose (contain whitespace).
        let glued = start > 0
            && s[..start]
                .chars()
                .last()
                .is_some_and(|p| p.is_alphanumeric() || p == ')');
        if glued && !group.chars().any(|c| c.is_whitespace()) {
            break;
        }
        if glued {
            // Prose remark glued to math: drop just the remark group.
            s.replace_range(start..=end, " ");
            continue;
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
            'x' => {
                // Implicit multiplication: `3x` is `3*x`.
                if prev_val {
                    out.push('*');
                }
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
            c if c.is_ascii_alphabetic() => {
                // Implicit multiplication: `3pi` is `3*pi`, `(a)sqrt`
                // is `(a)*sqrt`. Function names never follow a value,
                // and a letter starting a unit (`7cm`) never splits.
                if matches!(prev, Some(p) if p.is_ascii_digit() || p == ')')
                    && unit_len_at(&chars[i..]) == 0
                {
                    out.push('*');
                }
                out.push(c);
            }
            _ => {
                // Implicit multiplication: `2(3+4)`, `(8-2)(9)`.
                if c == '(' && prev_val {
                    out.push('*');
                }
                out.push(c);
            }
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
    // Leftover trailing single-letter units after a value (`30L`, `2m`).
    // Prose words ending in m/L are untouched.
    while (out.ends_with('m') || out.ends_with('L'))
        && out[..out.len() - 1]
            .chars()
            .last()
            .is_some_and(|p| p.is_ascii_digit() || p == ')' || p == '.')
    {
        out.pop();
    }
    let out = strip_leading_identifier(&out);
    let out = out.trim_start_matches(['=', '~', ':']);
    out.trim_end_matches(['=', '~', ',', ';', '.', ':'])
        .to_string()
}

/// Drop a prose identifier glued to a trailing group: `Solution(3,5)`
/// becomes `(3,5)`. Real calls (`sin(`, `sqrt(`, ...) are exempt, and
/// the group must run to the end of the string.
fn strip_leading_identifier(s: &str) -> String {
    let Some(open) = s.find('(') else {
        return s.to_string();
    };
    let pre = s[..open].trim();
    if pre.is_empty() || !pre.chars().all(|c| c.is_ascii_alphabetic() || c == ' ') {
        return s.to_string();
    }
    if ["sin", "cos", "tan", "asin", "acos", "atan", "sqrt"].contains(&pre.to_lowercase().as_str()) {
        return s.to_string();
    }
    let mut depth = 0;
    for (j, c) in s[open..].char_indices() {
        if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth -= 1;
            if depth == 0 && open + j + 1 != s.len() {
                return s.to_string();
            }
        }
    }
    if depth == 0 { s[open..].to_string() } else { s.to_string() }
}

/// Split text on multi-solution markers. `x = -5 or x = 3` gives two
/// alternatives; each is checked on its own instead of chained.
fn split_alternatives(text: &str) -> Vec<String> {
    text.split(" or ")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Raw equality chunks with a flag for degree-marked content.
/// Chunks also split on `:`, so "so k = 2 * 6" stands on its own.
fn raw_chunks(text: &str) -> Vec<(String, bool)> {
    let t = text
        .replace("=>", "=")
        .replace("->", "=")
        .replace(['≈', '→'], "=");
    let mut out = Vec::new();
    for eq in t.split('=') {
        // Split remarks off the math tail ("Half of -8 ...: x^2-8x...").
        for colon in eq.split(':') {
            out.push((colon.to_string(), colon.contains('°')));
        }
    }
    out
}

/// Split worked text into candidate expressions at equality links.
/// Degree-marked plain numbers in an inverse-trig step are results
/// displayed in degrees, so they convert to radians for comparison.
fn candidates(text: &str) -> Vec<String> {
    let chunks = raw_chunks(text);
    let has_inv = chunks.iter().any(|(c, _)| {
        let n = c.to_lowercase();
        n.contains("asin")
            || n.contains("acos")
            || n.contains("atan")
            || n.contains("arcsin")
            || n.contains("arccos")
            || n.contains("arctan")
            || n.contains("⁻¹")
            || n.contains("^-1")
    });
    let mut out = Vec::new();
    for (chunk, was_deg) in chunks {
        let mut n = normalize_expr(&chunk);
        if n.is_empty() {
            continue;
        }
        if has_inv && was_deg && n.parse::<f64>().is_ok() {
            n = format!("({n}*pi/180)");
        }
        out.push(n);
    }
    out
}

fn decimals(s: &str) -> u32 {
    // Least precise number token sets the tolerance.
    let mut nums = Vec::new();
    let mut cur = String::new();
    let mut in_num = false;
    for c in s.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            cur.push(c);
            in_num = true;
        } else if c == '.' && in_num {
            cur.push(c);
        } else if in_num {
            let d = cur.split('.').nth(1).map(|f| f.len() as u32).unwrap_or(0);
            nums.push(d);
            cur.clear();
            in_num = false;
        }
    }
    nums.into_iter().min().unwrap_or(0)
}

/// Rounding tolerance for a pair. When one side is an exact symbolic
/// form (`pi`, `sqrt`), the decimal side's precision governs, so a
/// mistranscribed decimal cannot hide behind the form's zero decimals.
/// Between plain decimals, either rounding may be the coarser one.
fn tolerance(a: &str, b: &str) -> f64 {
    let exact_form =
        a.contains("pi") || a.contains("sqrt") || b.contains("pi") || b.contains("sqrt");
    let d = if exact_form {
        decimals(a).max(decimals(b))
    } else {
        decimals(a).min(decimals(b))
    };
    let mut tol = (0.51 * 10f64.powi(-(d as i32))).max(1e-9);
    // Inverse-trig results round in degrees: a 1dp angle is honest
    // within about a thousandth of a radian.
    if ["asin", "acos", "atan"]
        .iter()
        .any(|f| a.contains(f) || b.contains(f))
    {
        tol = tol.max(1e-3);
    }
    tol
}

const SYMPY_SCRIPT: &str = r#"
import sympy as sp, sys
def parse(s):
    try:
        return sp.sympify(s, rational=True)
    except Exception:
        # Prose prefix before the math, e.g. "so k = 2*6": retry with
        # the text after the last colon.
        if ':' in s:
            return sp.sympify(s.rsplit(':', 1)[1], rational=True)
        raise
try:
    a = parse(sys.argv[1])
    b = parse(sys.argv[2])
except Exception as e:
    print("error")
    print("parse failed: " + str(e))
    raise SystemExit
# Comma-separated groups parse as tuples, not single values.
if isinstance(a, tuple) or isinstance(b, tuple):
    print("uncheckable")
    raise SystemExit
tol = float(sys.argv[3])
# Identities first: same-symbol equalities verify exactly, before the
# free-symbol gate below skips pairs with variables on both sides.
try:
    if sp.simplify(a - b) == 0:
        print("exact")
        raise SystemExit
except SystemExit:
    raise
except Exception:
    pass
allowed = {sp.pi, sp.E}
if (a.free_symbols | b.free_symbols) - allowed:
    print("uncheckable")
    raise SystemExit
try:
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

/// Check one chain of candidates. Unparseable links are skipped with a
/// note (gold answers mix prose with math); only a hard disagreement
/// fails the chain. Returns the agreement count, the log, and the
/// first mismatch if any.
async fn check_chain(cands: &[String]) -> (usize, Vec<serde_json::Value>, Option<String>) {
    let mut log = Vec::new();
    let mut agreed = 0;
    for pair in cands.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        match sympy_compare(a, b).await {
            SympyVerdict::Agree(exact) => {
                agreed += 1;
                log.push(serde_json::json!({"link": [a, b], "result": if exact {"exact"} else {"approx"}}));
            }
            SympyVerdict::Skip => {
                log.push(serde_json::json!({"link": [a, b], "result": "skipped-symbolic"}));
            }
            SympyVerdict::Disagree => {
                log.push(serde_json::json!({"link": [a, b], "result": "MISMATCH"}));
                return (agreed, log, Some(format!("sympy mismatch: {a} != {b}")));
            }
            SympyVerdict::Error(e) => {
                log.push(serde_json::json!({"link": [a, b], "result": "skipped-unparseable", "error": e}));
            }
        }
    }
    (agreed, log, None)
}

/// All normalized candidates across the worked steps, for final-answer
/// matching.
fn all_worked_candidates(worked: &[String]) -> Vec<String> {
    worked
        .iter()
        .flat_map(|s| split_alternatives(s))
        .flat_map(|sub| candidates(&sub))
        .collect()
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
    let mut agreed = 0usize;
    let mut step_logs = Vec::new();
    for step in &item.worked {
        for sub in split_alternatives(step) {
            let cands = candidates(&sub);
            if cands.len() < 2 {
                continue;
            }
            let (a, log, mismatch) = check_chain(&cands).await;
            agreed += a;
            step_logs.push(serde_json::json!({"step": step, "links": log}));
            if let Some(m) = mismatch {
                return Ok(GateDecision::ReviewRequired {
                    reason: "sympy-mismatch".into(),
                    detail: serde_json::json!({"step": step, "error": m, "links": log}),
                });
            }
        }
    }
    let worked_cands = all_worked_candidates(&item.worked);
    let mut final_log = Vec::new();
    for alt in split_alternatives(&item.answer) {
        let answer_cands = candidates(&alt);
        if answer_cands.is_empty() {
            return Ok(GateDecision::ReviewRequired {
                reason: "sympy-error".into(),
                detail: serde_json::json!({"error": "final answer has no parseable math"}),
            });
        }
        if answer_cands.len() >= 2 {
            let (a, log, mismatch) = check_chain(&answer_cands).await;
            agreed += a;
            final_log.push(serde_json::json!({"answer": alt, "links": log}));
            if let Some(m) = mismatch {
                return Ok(GateDecision::ReviewRequired {
                    reason: "sympy-mismatch".into(),
                    detail: serde_json::json!({"error": m, "answer": item.answer}),
                });
            }
        }
        // Every final alternative must appear among the worked values.
        // Exact string matches skip the subprocess; the rest compare
        // numerically so transcription slips cannot pass. Plain-number
        // finals also retry as degrees, matching inverse-trig steps.
        for c in &answer_cands {
            if worked_cands.iter().any(|w| w == c) {
                final_log.push(serde_json::json!({"answer_value": c, "result": "repeats-worked"}));
                continue;
            }
            // A bare variable is structural, not a value: it can neither
            // match nor mismatch.
            if c.chars().all(|ch| ch.is_ascii_alphabetic()) {
                continue;
            }
            let mut forms = vec![c.clone()];
            if c.parse::<f64>().is_ok() {
                forms.push(format!("({c}*pi/180)"));
            }
            let mut matched = None;
            'outer: for t in &forms {
                for w in &worked_cands {
                    if matches!(sympy_compare(w, t).await, SympyVerdict::Agree(_)) {
                        matched = Some(w.clone());
                        final_log.push(
                            serde_json::json!({"answer_value": c, "matched_worked": w, "result": "approx"}),
                        );
                        break 'outer;
                    }
                }
            }
            if matched.is_none() {
                return Ok(GateDecision::ReviewRequired {
                    reason: "sympy-mismatch".into(),
                    detail: serde_json::json!({
                        "error": "final answer does not match any worked value",
                        "answer": item.answer,
                    }),
                });
            }
        }
    }
    if agreed == 0 {
        return Ok(GateDecision::ReviewRequired {
            reason: "no-checkable-math".into(),
            detail: serde_json::json!({
                "error": "no checkable numeric links in worked steps",
                "steps": step_logs,
            }),
        });
    }
    Ok(GateDecision::Verified {
        method: "sympy-chain".into(),
        detail: serde_json::json!({
            "step_links": step_logs,
            "final_links": final_log,
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
    let rubric = rubric_json(&item.worked, &item.answer);
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

/// Build the stored rubric. Worked steps become method criteria; an item
/// with no captured working is stored as answer-only, so grading awards
/// the final mark and never invents method marks.
fn rubric_json(worked: &[String], answer: &str) -> String {
    let steps: Vec<&str> = worked
        .iter()
        .map(|step| step.trim())
        .filter(|step| !step.is_empty())
        .collect();
    if steps.is_empty() {
        return serde_json::json!({
            "method": [],
            "final": {"marks": 1, "answer": answer},
            "answer_only": true,
        })
        .to_string();
    }
    let method: Vec<serde_json::Value> = steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            serde_json::json!({
                "id": format!("step-{}", index + 1),
                "description": step,
                "marks": 1,
            })
        })
        .collect();
    serde_json::json!({
        "method": method,
        "final": {"marks": 1, "answer": answer},
    })
    .to_string()
}

/// Find an item by its prompt without running the gate. Used to keep
/// re-imports cheap: hash lookup only, no SymPy involved.
pub async fn find_by_prompt(
    pool: &sqlx::SqlitePool,
    prompt: &str,
) -> anyhow::Result<Option<StoredItem>> {
    let hash = dedupe_hash(prompt);
    let id: Option<String> = sqlx::query_scalar("SELECT id FROM items WHERE dedupe_hash = ?")
        .bind(&hash)
        .fetch_optional(pool)
        .await?;
    match id {
        Some(id) => Ok(Some(get_item(pool, &id).await?)),
        None => Ok(None),
    }
}

pub async fn get_item(pool: &sqlx::SqlitePool, id: &str) -> anyhow::Result<StoredItem> {    let row: (String, String, String, String, String, String, String, Option<String>) = sqlx::query_as(
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
        assert_eq!(normalize_expr("90π"), "90*pi");
        assert_eq!(normalize_expr("282.7 cm²"), "282.7");
        assert_eq!(normalize_expr("√(25 + 144)"), "sqrt((25+144))");
        assert_eq!(normalize_expr("10√2 cm"), "10*sqrt(2)");
        assert_eq!(normalize_expr("4.5 m² × 10 000"), "4.5*10000");
        // Superscripts on values stay powers; on units they drop.
        assert_eq!(normalize_expr("6²"), "6**2");
        assert_eq!(normalize_expr("1.4m"), "1.4");
        // Variables are preserved, never eaten as units.
        assert_eq!(normalize_expr("2m+3"), "2m+3");
        // Implicit multiplication is made explicit for SymPy.
        assert_eq!(normalize_expr("3x+2"), "3*x+2");
        assert_eq!(normalize_expr("(2x-3)*(x+5)"), "(2*x-3)*(x+5)");
        assert_eq!(normalize_expr("3(x-2)(x+2)"), "3*(x-2)*(x+2)");
        // ...but a letter starting a unit never splits.
        assert_eq!(normalize_expr("7cm"), "7");
        assert_eq!(normalize_expr("2m"), "2");
        assert_eq!(normalize_expr("90pi"), "90*pi");
    }

    #[test]
    fn splits_equality_chains() {
        let c = candidates("V = (1/3)*pi*(25)*(12) = 100*pi ≈ 314.2");
        assert_eq!(c, vec!["V", "(1/3)*pi*(25)*(12)", "100*pi", "314.2"]);
        let c = candidates("5x = 15 → x = 3");
        assert_eq!(c, vec!["5*x", "15", "x", "3"]);
    }

    #[test]
    fn splits_solution_alternatives() {
        assert_eq!(split_alternatives("x = -5 or x = 3").len(), 2);
        assert_eq!(split_alternatives("error bars").len(), 1);
    }

    #[test]
    fn normalizes_trig_degrees_and_percents() {
        assert_eq!(
            normalize_expr("15 × sin 28°"),
            "15*sin(28*pi/180)"
        );
        assert_eq!(normalize_expr("tan⁻¹(0.75)"), "atan(0.75)");
        assert_eq!(normalize_expr("5%"), "(5/100)");
        assert_eq!(normalize_expr("4.7 × 10⁻⁵"), "4.7*10**-5");
        assert_eq!(normalize_expr("**282.7 cm²**"), "282.7");
        // Implicit multiplication and pi applied to a group.
        assert_eq!(normalize_expr("2(3+4)"), "2*(3+4)");
        assert_eq!(normalize_expr("π(12)²"), "pi*(12)**2");
    }

    #[test]
    fn converts_inverse_trig_degree_results() {
        let c = candidates("x = asin(0.6) ≈ 36.9°");
        assert_eq!(c, vec!["x", "asin(0.6)", "(36.9*pi/180)"]);
    }

    #[test]
    fn powers_apply_to_variables_but_not_measured_units() {
        assert_eq!(normalize_expr("x²"), "x**2");
        assert_eq!(normalize_expr("r²"), "r**2");
        assert_eq!(normalize_expr("4.5m²"), "4.5");
        assert_eq!(normalize_expr("200 m²"), "200");
    }

    #[test]
    fn strips_prose_identifiers_from_tuples() {
        assert_eq!(normalize_expr("Solution(3,5)"), "(3,5)");
        // Real calls are exempt.
        assert_eq!(normalize_expr("sin(28*pi/180)"), "sin(28*pi/180)");
        assert_eq!(normalize_expr("sqrt((25+144))"), "sqrt((25+144))");
    }

    #[test]
    fn splits_colon_remarks_off_math() {
        let c = candidates("Half of -8 is -4, squared is 16: x = 3");
        assert!(c.contains(&"x".to_string()));
        assert!(c.contains(&"3".to_string()));
    }

    #[test]
    fn inverse_trig_tolerance_covers_degree_rounding() {
        assert!(tolerance("acos(0.0714)", "(85.9*pi/180)") >= 1e-3);
    }

    #[test]
    fn rounding_tolerance_covers_one_decimal() {
        assert!((tolerance("314.159", "314.2") - 0.051).abs() < 1e-12);
        // Exact form vs decimal: the decimal's precision governs, so a
        // mistranscribed decimal cannot pass.
        assert!((tolerance("90*pi", "282.9") - 0.051).abs() < 1e-12);
        // Plain decimals of different precision: coarser rounding governs.
        assert!((tolerance("293.87", "294") - 0.51).abs() < 1e-12);
    }

    #[test]
    fn rubric_json_marks_answer_only_when_no_working_was_captured() {
        let answer_only = rubric_json(&[], "502.7 cm3");
        let value: serde_json::Value = serde_json::from_str(&answer_only).unwrap();
        assert_eq!(value["answer_only"], true);
        assert!(value["method"].as_array().unwrap().is_empty());
        assert_eq!(value["final"]["answer"], "502.7 cm3");

        let worked = vec!["V = pi*r^2*h".to_string(), "= 502.7".to_string()];
        let rubric = rubric_json(&worked, "502.7 cm3");
        let value: serde_json::Value = serde_json::from_str(&rubric).unwrap();
        assert_eq!(value["method"].as_array().unwrap().len(), 2);
        assert_eq!(value["method"][0]["id"], "step-1");
        assert!(value.get("answer_only").is_none());
    }
}
