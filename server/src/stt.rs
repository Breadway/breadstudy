//! Explain-back STT: local whisper.cpp transcription plus rubric scoring.
//!
//! The explain step has the student talk through the method, then the agent
//! checks the explanation and fills gaps. Transcription runs locally via the
//! whisper.cpp CLI on an explicit audio file. There is no background
//! listener: the mic is only live during a session while the student records
//! an explain-back answer, and every transcription call names its file.
//!
//! Configuration via environment:
//! - `WHISPER_BIN` (default `whisper-cli` on PATH)
//! - `WHISPER_MODEL` (default `models/ggml-base.en.bin` under the whisper
//!   checkout; integration tests skip when the model file is absent)

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct SttConfig {
    pub bin: PathBuf,
    pub model: PathBuf,
}

impl SttConfig {
    pub fn from_env() -> Self {
        Self {
            bin: std::env::var("WHISPER_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("whisper-cli")),
            model: std::env::var("WHISPER_MODEL")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("models/ggml-base.en.bin")),
        }
    }

    pub fn model_available(&self) -> bool {
        self.model.is_file()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
}

/// Transcribe one 16 kHz WAV file locally. The call blocks on the CLI and
/// returns the plain-text transcription with timestamps stripped.
pub fn transcribe(config: &SttConfig, wav: &Path) -> anyhow::Result<Transcript> {
    let output = Command::new(&config.bin)
        .arg("-m")
        .arg(&config.model)
        .arg("-f")
        .arg(wav)
        .arg("-nt")
        .arg("--no-timestamps")
        .output()
        .map_err(|e| anyhow::anyhow!("whisper binary failed to start: {e}"))?;
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "whisper exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(Transcript {
        text: String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
    })
}

/// One checkable step of a worked-answer rubric.
#[derive(Debug, Clone)]
pub struct RubricStep {
    /// Stable id, e.g. "cone-volume-formula".
    pub id: String,
    /// Short label shown in gap reports.
    pub label: String,
    /// Any of these phrases (case-insensitive, punctuation ignored) counts
    /// as covering the step.
    pub keywords: Vec<String>,
}

impl RubricStep {
    pub fn new(id: &str, label: &str, keywords: &[&str]) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExplanationScore {
    /// Step ids found in the transcript.
    pub covered: Vec<String>,
    /// (id, label) pairs missing from the transcript.
    pub gaps: Vec<(String, String)>,
}

impl ExplanationScore {
    pub fn score(&self, total: usize) -> f32 {
        if total == 0 {
            return 1.0;
        }
        self.covered.len() as f32 / total as f32
    }
}

fn normalize_text(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Score a transcription against rubric steps. A step is covered when any of
/// its keyword phrases appears in the normalized transcript (leading word
/// boundary; a trailing partial word still counts, so "cubic centim"
/// matches "cubic centimetres").
pub fn score_explanation(transcript: &str, steps: &[RubricStep]) -> ExplanationScore {
    let haystack = format!(" {} ", normalize_text(transcript));
    let mut covered = Vec::new();
    let mut gaps = Vec::new();
    for step in steps {
        let hit = step
            .keywords
            .iter()
            .any(|k| haystack.contains(&format!(" {}", normalize_text(k))));
        if hit {
            covered.push(step.id.clone());
        } else {
            gaps.push((step.id.clone(), step.label.clone()));
        }
    }
    ExplanationScore { covered, gaps }
}

/// Rubric for the Volume/SA cone question used in the explain-back check:
/// "Find the volume of a cone with radius 6 cm and height 8 cm."
pub fn cone_volume_rubric() -> Vec<RubricStep> {
    vec![
        RubricStep::new(
            "formula",
            "states the cone volume formula (one third pi r squared h)",
            &["one third", "third pi", "volume of a cone", "pi r squared"],
        ),
        RubricStep::new(
            "substitute",
            "substitutes radius 6 and height 8",
            &[
                "radius 6",
                "radius is 6",
                "r equals 6",
                "r is 6",
                "height 8",
                "height is 8",
                "h equals 8",
                "h is 8",
            ],
        ),
        RubricStep::new(
            "slant-note",
            "notes the slant height is not needed for volume",
            &["slant", "not needed", "no need"],
        ),
        RubricStep::new(
            "evaluate",
            "evaluates to 96 pi (about 301.6 cubic centimetres)",
            &["96 pi", "96pie", "301", "ninety six"],
        ),
        RubricStep::new(
            "units",
            "gives cubic centimetre units",
            &[
                "cubic centim",
                "cm cubed",
                "centimeters cubed",
                "centimetres cubed",
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn student_transcript() -> &'static str {
        "The volume of a cone is one third pi r squared h. \
         The radius is 6 and the height is 8, so it is one third pi times 36 times 8. \
         That gives 96 pi, which is about 301 point 6 cubic centimetres."
    }

    #[test]
    fn full_explanation_covers_all_but_slant_note() {
        let score = score_explanation(student_transcript(), &cone_volume_rubric());
        assert!(score.covered.contains(&"formula".to_string()));
        assert!(score.covered.contains(&"substitute".to_string()));
        assert!(score.covered.contains(&"evaluate".to_string()));
        assert!(score.covered.contains(&"units".to_string()));
        assert_eq!(
            score.gaps,
            vec![(
                "slant-note".to_string(),
                "notes the slant height is not needed for volume".to_string()
            )]
        );
        assert!((score.score(5) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn thin_explanation_lists_gaps() {
        let score = score_explanation("The answer is 96 pi.", &cone_volume_rubric());
        assert_eq!(score.covered, vec!["evaluate".to_string()]);
        assert_eq!(score.gaps.len(), 4);
    }

    #[test]
    fn empty_transcript_covers_nothing() {
        let score = score_explanation("", &cone_volume_rubric());
        assert!(score.covered.is_empty());
        assert_eq!(score.gaps.len(), 5);
        assert_eq!(score.score(5), 0.0);
    }

    #[test]
    fn matching_ignores_case_and_punctuation() {
        let steps = vec![RubricStep::new("s", "S", &["one-third"])];
        let score = score_explanation("ONE third!", &steps);
        assert_eq!(score.covered, vec!["s".to_string()]);
    }

    /// End to end through the real whisper.cpp CLI on its bundled sample.
    /// Skips with a notice when WHISPER_MODEL is absent (CI has no model).
    #[test]
    fn transcribes_spoken_sample_locally() {
        let config = SttConfig::from_env();
        if !config.model_available() {
            eprintln!("skipping whisper e2e: model file absent");
            return;
        }
        let sample = std::env::var("WHISPER_SAMPLE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("samples/jfk.wav"));
        if !sample.is_file() {
            eprintln!("skipping whisper e2e: sample absent");
            return;
        }
        let transcript = transcribe(&config, &sample).expect("local transcription works");
        assert!(
            !transcript.text.is_empty(),
            "transcription must not be empty"
        );
        let score = score_explanation(
            &transcript.text,
            &[RubricStep::new(
                "ask-not",
                "JFK asks what you can do for your country",
                &["country", "fellow americans"],
            )],
        );
        assert!(
            !score.covered.is_empty(),
            "sample should cover its rubric step, got: {}",
            transcript.text
        );
    }
}
