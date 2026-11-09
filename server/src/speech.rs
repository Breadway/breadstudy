//! Maths speech normalizer: LaTeX and formula shorthand to speakable text.
//!
//! TTS input must be speakable, not raw LaTeX. This module sits before
//! breeze-server and covers the Volume and Surface Area formula set first
//! (vault note "Area Volume and Surface Area.md").
//!
//! Example: `V = (1/3)πr²h` becomes
//! `V equals open bracket 1 over 3 close bracket pi r squared h`.

/// Marker wrapping inserted English words. The variable-splitting pass skips
/// marked spans so words like "theta" or "centimetres" survive intact.
const MARK: char = '\u{E000}';
/// Superscript markers kept through unit handling so `m²` can become
/// "square metres" while `r²` becomes "r squared".
const SUP2: char = '\u{E002}';
const SUP3: char = '\u{E003}';

/// Convert LaTeX or formula shorthand to speakable plain text.
pub fn speakable(input: &str) -> String {
    let out = convert(input);
    let flat = out
        .replace([MARK, SUP2, SUP3], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    flat.replace(" ,", ",")
}

fn word(out: &mut String, w: &str) {
    out.push(MARK);
    out.push_str(w);
    out.push(MARK);
}

fn convert(input: &str) -> String {
    // Phase 0: unicode symbols. Powers stay as markers for unit handling.
    let mut s = String::with_capacity(input.len() + 32);
    for c in input.chars() {
        match c {
            '²' => s.push(SUP2),
            '³' => s.push(SUP3),
            '½' => word(&mut s, " a half "),
            '⅓' => word(&mut s, " a third "),
            'π' => word(&mut s, " pi "),
            '√' => word(&mut s, " square root of "),
            '×' => word(&mut s, " times "),
            '÷' => word(&mut s, " divided by "),
            '−' => word(&mut s, " minus "),
            'θ' => word(&mut s, " theta "),
            'Δ' => word(&mut s, " delta "),
            '°' => word(&mut s, " degrees "),
            '≈' => word(&mut s, " is approximately "),
            '→' => word(&mut s, " gives "),
            _ => s.push(c),
        }
    }
    let mut s = s.trim().to_string();

    // Phase 1: strip math delimiters and spacing commands.
    for token in [
        "\\(", "\\)", "\\[", "\\]", "\\left", "\\right", "\\,", "\\;", "\\!", "\\ ",
    ] {
        s = s.replace(token, " ");
    }
    s = s.replace('$', " ");
    s = s.replace("\\\\", " ");

    // Phase 2: \frac{A}{B} innermost first, recursing into A and B.
    s = expand_frac(&s);

    // Phase 3: \sqrt[n]{X} and \sqrt{X}.
    s = expand_sqrt(&s);

    // Phase 4: \text{X} and \mathrm{X} keep inner text.
    s = expand_text_cmd(&s, "\\text");
    s = expand_text_cmd(&s, "\\mathrm");

    // Phase 5: named LaTeX commands to words.
    let cmds: &[(&str, &str)] = &[
        ("\\times", " times "),
        ("\\div", " divided by "),
        ("\\pm", " plus or minus "),
        ("\\cdot", " times "),
        ("\\leq", " is less than or equal to "),
        ("\\geq", " is greater than or equal to "),
        ("\\neq", " is not equal to "),
        ("\\approx", " is approximately "),
        ("\\pi", " pi "),
        ("\\theta", " theta "),
        ("\\delta", " delta "),
        ("\\Delta", " delta "),
        ("\\alpha", " alpha "),
        ("\\beta", " beta "),
        ("\\gamma", " gamma "),
        ("\\circ", " degrees "),
    ];
    for (cmd, rep) in cmds {
        s = s.replace(cmd, &marked(rep));
    }

    // Phase 6: units with optional powers before generic superscripts.
    s = expand_units(&s);

    // Phase 7: superscripts and subscripts.
    s = expand_scripts(&s);

    // Phase 8: standalone SA abbreviation.
    s = expand_sa(&s);

    // Phase 9: ASCII operators and punctuation.
    s = s.replace('=', " = ");
    s = s.replace('+', " + ");
    s = s.replace('-', " - ");
    s = s.replace('*', " times ");
    s = s.replace('/', " over ");
    s = s.replace(':', &marked(" to "));
    s = s.replace('%', " percent ");
    s = s.replace('&', " and ");
    s = s.replace('(', " ( ");
    s = s.replace(')', " ) ");

    // Phase 10: collapse digit group spaces ("10 000" -> "10000").
    s = collapse_digit_spaces(&s);

    // Phase 11: word-level replacements. Marked spans (odd segments after
    // splitting on MARK) pass through verbatim; the rest is tokenized.
    let mut out = String::with_capacity(s.len() + 32);
    for (idx, part) in s.split(MARK).enumerate() {
        if idx % 2 == 1 {
            out.push_str(part);
            out.push(' ');
        } else {
            for tok in part.split_whitespace() {
                let t = tok.trim_matches([SUP2, SUP3]);
                match t {
                    "=" => out.push_str(" equals "),
                    "+" => out.push_str(" plus "),
                    "-" => out.push_str(" minus "),
                    "(" => out.push_str(" open bracket "),
                    ")" => out.push_str(" close bracket "),
                    t if is_decimal(t) => {
                        out.push_str(&format!(" {} ", t.replace('.', " point ")));
                    }
                    t if is_int(t) => {
                        out.push_str(tok);
                        out.push(' ');
                    }
                    _ if is_sup_marker_token(tok) => {
                        for c in tok.chars() {
                            out.push_str(if c == SUP2 { " squared " } else { " cubed " });
                        }
                    }
                    _ => {
                        out.push_str(&split_variables(t));
                        out.push(' ');
                    }
                }
            }
        }
    }
    let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
    fix_singular(&flat)
}

fn marked(rep: &str) -> String {
    format!("{MARK}{rep}{MARK}")
}

fn is_sup_marker_token(tok: &str) -> bool {
    tok.chars().all(|c| c == SUP2 || c == SUP3)
}

/// Expand \frac{A}{B} to "A over B", innermost group first.
fn expand_frac(s: &str) -> String {
    let mut s = s.to_string();
    while let Some(start) = s.find("\\frac") {
        let rest = &s[start + 5..];
        let Some((a, after_a)) = brace_group(rest) else {
            break;
        };
        let Some((b, after_b)) = brace_group(after_a) else {
            break;
        };
        let end = start + 5 + (rest.len() - after_b.len());
        let replacement = format!("{} {} {}", convert(&a), marked(" over "), convert(&b));
        s.replace_range(start..end, &replacement);
    }
    s
}

/// Expand \sqrt[n]{X} and \sqrt{X}.
fn expand_sqrt(s: &str) -> String {
    let mut s = s.to_string();
    while let Some(start) = s.find("\\sqrt") {
        let mut rest = &s[start + 5..];
        let mut root: Option<String> = None;
        if rest.starts_with('[') {
            if let Some(close) = rest.find(']') {
                root = Some(rest[1..close].to_string());
                rest = &rest[close + 1..];
            }
        }
        let Some((inner, after)) = brace_group(rest) else {
            break;
        };
        let end = start + 5 + (s[start + 5..].len() - after.len());
        let phrase = match root.as_deref() {
            Some("2") | None => marked(" square root of "),
            Some("3") => marked(" cube root of "),
            Some(n) => format!("{MARK} {n} root of {MARK}"),
        };
        let replacement = format!("{phrase} {}", convert(&inner));
        s.replace_range(start..end, &replacement);
    }
    s
}

/// Take the first `{...}` group with nesting; return (inner, remainder after `}`).
fn brace_group(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start();
    if !s.starts_with('{') {
        return None;
    }
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                return Some((s[1..i].to_string(), &s[i + 1..]));
            }
        }
    }
    None
}

/// \text{X} and \mathrm{X} keep inner text as words.
fn expand_text_cmd(s: &str, cmd: &str) -> String {
    let mut out = s.to_string();
    while let Some(start) = out.find(cmd) {
        let rest = &out[start + cmd.len()..];
        let Some((inner, after)) = brace_group(rest) else {
            break;
        };
        let end = start + cmd.len() + (rest.len() - after.len());
        out.replace_range(start..end, &marked(&format!(" {inner} ")));
    }
    out
}

/// Units with optional powers: m^2, cm^3, mm2, kL, mL ...
fn expand_units(s: &str) -> String {
    let mut s = s.to_string();
    // Multi-letter abbreviations first (cm before bare m).
    for (abbr, many) in [
        ("km", "kilometres"),
        ("cm", "centimetres"),
        ("mm", "millimetres"),
        ("mL", "millilitres"),
        ("kL", "kilolitres"),
    ] {
        for pat in [format!("{abbr}^3"), format!("{abbr}^2")] {
            s = replace_unit(&s, &pat, &format!(" {MARK} cubic {many} {MARK} "));
        }
        for pat in [format!("{abbr}{SUP2}"), format!("{abbr}{SUP3}")] {
            let words = if pat.ends_with(SUP3) {
                "cubic"
            } else {
                "square"
            };
            s = s.replace(&pat, &format!(" {MARK} {words} {many} {MARK} "));
        }
        s = replace_unit(&s, abbr, &format!(" {MARK} {many} {MARK} "));
    }
    // Bare m, m^2, m^3 and bare L.
    for (pat, shape) in [("m^3", "cubic"), ("m^2", "square")] {
        s = replace_unit(&s, pat, &format!(" {MARK} {shape} metres {MARK} "));
    }
    for pat in [format!("m{SUP3}"), format!("m{SUP2}")] {
        let shape = if pat.ends_with(SUP3) {
            "cubic"
        } else {
            "square"
        };
        // Avoid matching the m inside already-marked unit words.
        let mut out = String::with_capacity(s.len() + 16);
        let mut rest = s.as_str();
        while let Some(pos) = rest.find(&pat) {
            let before = &rest[..pos];
            let prev_marked = before.ends_with(MARK);
            out.push_str(before);
            if prev_marked {
                out.push_str(&pat);
            } else {
                out.push_str(&format!(" {MARK} {shape} metres {MARK} "));
            }
            rest = &rest[pos + pat.len()..];
        }
        out.push_str(rest);
        s = out;
    }
    s = replace_bare(&s, 'm', "metres");
    s = replace_bare(&s, 'L', "litres");
    s
}

/// Replace a token only at word boundaries (char-based, marker-safe).
fn replace_token(s: &str, pat: &str, words: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let pat_chars: Vec<char> = pat.chars().collect();
    let mut out = String::with_capacity(s.len() + 32);
    let mut i = 0;
    while i < chars.len() {
        if chars[i..].starts_with(&pat_chars)
            && (i == 0 || !is_word_char(chars[i - 1]))
            && (i + pat_chars.len() == chars.len() || !is_word_char(chars[i + pat_chars.len()]))
        {
            out.push_str(words);
            i += pat_chars.len();
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == MARK
}

/// Replace unit abbreviations only at letter boundaries.
fn replace_unit(s: &str, pat: &str, words: &str) -> String {
    replace_token(s, pat, words)
}

/// Standalone single letters m and L become unit words.
fn replace_bare(s: &str, letter: char, many: &str) -> String {
    replace_token(s, &letter.to_string(), &marked(&format!(" {many} ")))
}

/// "1 square metres" -> "1 square metre" and friends (runs on plain text).
fn fix_singular(s: &str) -> String {
    let mut s = s.to_string();
    for (many, one) in [
        ("square metres", "square metre"),
        ("cubic metres", "cubic metre"),
        ("square centimetres", "square centimetre"),
        ("cubic centimetres", "cubic centimetre"),
        ("square millimetres", "square millimetre"),
        ("square kilometres", "square kilometre"),
        ("metres", "metre"),
        ("centimetres", "centimetre"),
        ("millimetres", "millimetre"),
        ("kilometres", "kilometre"),
        ("litres", "litre"),
        ("millilitres", "millilitre"),
        ("kilolitres", "kilolitre"),
    ] {
        s = s.replace(&format!("1 {many}"), &format!("1 {one}"));
    }
    s
}

/// Superscripts `^2 ^3 ^{...} ^x`, marker powers, and subscripts.
fn expand_scripts(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 32);
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == SUP2 {
            out.push_str(&marked(" squared "));
            i += 1;
            continue;
        }
        if c == SUP3 {
            out.push_str(&marked(" cubed "));
            i += 1;
            continue;
        }
        if (c == '^' || c == '_') && i > 0 {
            let is_sup = c == '^';
            if chars.get(i + 1) == Some(&'{') {
                if let Some(end) = find_close(&chars, i + 1) {
                    let inner: String = chars[i + 2..end].iter().collect();
                    out.push_str(&marked(if is_sup { " to the power of " } else { " sub " }));
                    out.push_str(&convert(&inner));
                    out.push(' ');
                    i = end + 1;
                    continue;
                }
            } else if let Some(&next) = chars.get(i + 1) {
                if next != ' ' {
                    if is_sup {
                        match next {
                            '2' => out.push_str(&marked(" squared ")),
                            '3' => out.push_str(&marked(" cubed ")),
                            _ => {
                                out.push_str(&marked(" to the power of "));
                                out.push(next);
                                out.push(' ');
                            }
                        }
                    } else {
                        out.push_str(&marked(" sub "));
                        out.push(next);
                        out.push(' ');
                    }
                    i += 2;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn find_close(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0;
    for (k, c) in chars.iter().enumerate().skip(open) {
        if *c == '{' {
            depth += 1;
        } else if *c == '}' {
            depth -= 1;
            if depth == 0 {
                return Some(k);
            }
        }
    }
    None
}

/// Standalone SA token means surface area.
fn expand_sa(s: &str) -> String {
    replace_token(s, "SA", &marked(" surface area "))
}

fn collapse_digit_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        out.push(chars[i]);
        if chars[i].is_ascii_digit() {
            let mut j = i + 1;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            if j < chars.len() && chars[j].is_ascii_digit() && j > i + 1 {
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// English words that must survive the variable-splitting pass.
const KEEP_WORDS: &[&str] = &[
    "of", "and", "the", "per", "sin", "cos", "tan", "log", "ln", "exp",
];

/// Split short lowercase variable runs: "lwh" -> "l w h". Marked spans,
/// numbers and longer or capitalized English words pass through.
fn split_variables(tok: &str) -> String {
    if tok.contains(MARK) || tok.contains(SUP2) || tok.contains(SUP3) {
        return tok.to_string();
    }
    let lower =
        tok.len() <= 3 && tok.bytes().all(|b| b.is_ascii_lowercase()) && !KEEP_WORDS.contains(&tok);
    if lower {
        return tok
            .chars()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(" ");
    }
    tok.to_string()
}

fn is_int(t: &str) -> bool {
    !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit())
}

fn is_decimal(t: &str) -> bool {
    let parts: Vec<&str> = t.split('.').collect();
    parts.len() == 2
        && !parts[0].is_empty()
        && !parts[1].is_empty()
        && parts[0].bytes().all(|b| b.is_ascii_digit())
        && parts[1].bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(input: &str, expected: &str) {
        assert_eq!(speakable(input), expected, "input: {input}");
    }

    #[test]
    fn issue_latex_examples() {
        check("x^2", "x squared");
        check("\\frac{\\sqrt{3}}{2}", "square root of 3 over 2");
        check("\\delta", "delta");
        check("\\pi r^2", "pi r squared");
    }

    #[test]
    fn unit_conversions() {
        check("1 m = 100 cm", "1 metre equals 100 centimetres");
        check("1 km = 1000 m", "1 kilometre equals 1000 metres");
        check("1 cm = 10 mm", "1 centimetre equals 10 millimetres");
        check(
            "1 m² = 10 000 cm²",
            "1 square metre equals 10000 square centimetres",
        );
        check(
            "1 km² = 1 000 000 m²",
            "1 square kilometre equals 1000000 square metres",
        );
        check(
            "1 cm² = 100 mm²",
            "1 square centimetre equals 100 square millimetres",
        );
        check(
            "1 m³ = 1 000 000 cm³",
            "1 cubic metre equals 1000000 cubic centimetres",
        );
        check(
            "1 L = 1000 mL = 1000 cm³",
            "1 litre equals 1000 millilitres equals 1000 cubic centimetres",
        );
        check("1 m³ = 1000 L", "1 cubic metre equals 1000 litres");
        check(
            "3 m² = 3 × 10 000 = 30 000 cm²",
            "3 square metres equals 3 times 10000 equals 30000 square centimetres",
        );
        check(
            "2 m³ = 2 × 1 000 000 = 2 000 000 cm³",
            "2 cubic metres equals 2 times 1000000 equals 2000000 cubic centimetres",
        );
    }

    #[test]
    fn capacity_rows() {
        check("1 mL = 1 cm³", "1 millilitre equals 1 cubic centimetre");
        check("1 L = 1000 cm³", "1 litre equals 1000 cubic centimetres");
        check(
            "1 kL = 1 m³ = 1000 L",
            "1 kilolitre equals 1 cubic metre equals 1000 litres",
        );
    }

    #[test]
    fn area_formulas() {
        check("A = lw", "A equals l w");
        check("A = ½bh", "A equals a half b h");
        check("A = bh", "A equals b h");
        check(
            "A = ½(a + b)h",
            "A equals a half open bracket a plus b close bracket h",
        );
        check("A = πr²", "A equals pi r squared");
        check(
            "A = (θ/360) × πr²",
            "A equals open bracket theta over 360 close bracket times pi r squared",
        );
    }

    #[test]
    fn composite_area_example() {
        check(
            "Area = (8 × 5) − (2 × 3) = 40 − 6 = 34 m²",
            "Area equals open bracket 8 times 5 close bracket minus open bracket 2 times 3 close bracket equals 40 minus 6 equals 34 square metres",
        );
    }

    #[test]
    fn perimeter_formulas() {
        check(
            "P = 2(l + w)",
            "P equals 2 open bracket l plus w close bracket",
        );
        check("C = 2πr = πd", "C equals 2 pi r equals pi d");
        check(
            "l = (θ/360) × 2πr",
            "l equals open bracket theta over 360 close bracket times 2 pi r",
        );
    }

    #[test]
    fn volume_formulas() {
        check("V = πr²h", "V equals pi r squared h");
        check(
            "V = (1/3)πr²h",
            "V equals open bracket 1 over 3 close bracket pi r squared h",
        );
        check(
            "V = (4/3)πr³",
            "V equals open bracket 4 over 3 close bracket pi r cubed",
        );
        check(
            "V = (1/3) × base area × h",
            "V equals open bracket 1 over 3 close bracket times base area times h",
        );
    }

    #[test]
    fn surface_area_formulas() {
        check(
            "SA = 2(lw + lh + wh)",
            "surface area equals 2 open bracket l w plus l h plus w h close bracket",
        );
        check(
            "SA = 2πr² + 2πrh = 2πr(r + h)",
            "surface area equals 2 pi r squared plus 2 pi r h equals 2 pi r open bracket r plus h close bracket",
        );
        check("SA = 4πr²", "surface area equals 4 pi r squared");
        check(
            "SA = πr² + πrl = πr(r + l)",
            "surface area equals pi r squared plus pi r l equals pi r open bracket r plus l close bracket",
        );
        check(
            "SA = πr² + 2πrh",
            "surface area equals pi r squared plus 2 pi r h",
        );
    }

    #[test]
    fn pythagoras_3d() {
        check(
            "l = √(r² + h²)",
            "l equals square root of open bracket r squared plus h squared close bracket",
        );
        check(
            "d = √(9 + 16 + 25) = √50 = 5√2 cm",
            "d equals square root of open bracket 9 plus 16 plus 25 close bracket equals square root of 50 equals 5 square root of 2 centimetres",
        );
    }

    #[test]
    fn worked_cone_example() {
        check(
            "r = 6 cm, h = 8 cm",
            "r equals 6 centimetres, h equals 8 centimetres",
        );
        check(
            "l = √(36 + 64) = √100 = 10 cm",
            "l equals square root of open bracket 36 plus 64 close bracket equals square root of 100 equals 10 centimetres",
        );
        check(
            "V = (1/3)π(6²)(8) = (1/3)π(36)(8) = 96π ≈ 301.6 cm³",
            "V equals open bracket 1 over 3 close bracket pi open bracket 6 squared close bracket open bracket 8 close bracket equals open bracket 1 over 3 close bracket pi open bracket 36 close bracket open bracket 8 close bracket equals 96 pi is approximately 301 point 6 cubic centimetres",
        );
    }

    #[test]
    fn worked_composite_solid() {
        check(
            "V = πr²h + (2/3)πr³ = π(25)(10) + (2/3)π(125) = 250π + (250/3)π ≈ 1047 cm³",
            "V equals pi r squared h plus open bracket 2 over 3 close bracket pi r cubed equals pi open bracket 25 close bracket open bracket 10 close bracket plus open bracket 2 over 3 close bracket pi open bracket 125 close bracket equals 250 pi plus open bracket 250 over 3 close bracket pi is approximately 1047 cubic centimetres",
        );
    }

    #[test]
    fn error_and_scaling_rows() {
        check(
            "0.5 / 84 ≈ 0.006",
            "0 point 5 over 84 is approximately 0 point 006",
        );
        check(
            "A = 25π ≈ 78.54 cm²",
            "A equals 25 pi is approximately 78 point 54 square centimetres",
        );
        check("k² : 1", "k squared to 1");
        check("k³ : 1", "k cubed to 1");
        check(
            "20 ÷ 400 = 0.05 m²",
            "20 divided by 400 equals 0 point 05 square metres",
        );
    }
}
