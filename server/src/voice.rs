//! breeze-server broker: response cache, upstream proxy, null-TTS fallback.
//!
//! Sessions must never block on the GPU. Every speak request first checks the
//! file cache keyed on (text, instruction, cfg_scale). On a miss the request
//! goes to the upstream Breeze TTS server; if that service is down the broker
//! answers with silent PCM plus subtitles so the session continues text-only.

use std::path::{Path, PathBuf};

pub const SAMPLE_RATE: u32 = 24_000;
pub const CHANNELS: u16 = 1;
pub const DEFAULT_INSTRUCTION: &str = "Speak clearly and naturally.";
pub const DEFAULT_CFG_SCALE: f32 = 1.0;

/// Minimum and maximum null-TTS clip length in seconds.
pub const MIN_CLIP_SECS: f32 = 0.5;
pub const MAX_CLIP_SECS: f32 = 12.0;
/// Seconds of silence per character when the GPU service is down.
pub const SECS_PER_CHAR: f32 = 0.06;

#[derive(Debug, Clone)]
pub struct SpeakRequest {
    pub text: String,
    pub instruction: Option<String>,
    pub cfg_scale: Option<f32>,
}

impl SpeakRequest {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            instruction: None,
            cfg_scale: None,
        }
    }

    pub fn instruction_or_default(&self) -> &str {
        self.instruction.as_deref().unwrap_or(DEFAULT_INSTRUCTION)
    }

    pub fn cfg_or_default(&self) -> f32 {
        self.cfg_scale.unwrap_or(DEFAULT_CFG_SCALE)
    }
}

#[derive(Debug, Clone)]
pub struct SpeakOutcome {
    /// WAV document, PCM16 mono.
    pub wav: Vec<u8>,
    pub sample_rate: u32,
    pub cached: bool,
    pub fallback: bool,
    /// Plain text the session shows while audio plays (or instead of audio).
    pub subtitle: String,
}

/// Stable cache key over (text, instruction, cfg_scale). Uses FNV-1a so the
/// key is identical across processes and restarts.
pub fn cache_key(text: &str, instruction: Option<&str>, cfg_scale: Option<f32>) -> String {
    let instruction = instruction.unwrap_or(DEFAULT_INSTRUCTION);
    let cfg = cfg_scale.unwrap_or(DEFAULT_CFG_SCALE);
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text
        .bytes()
        .chain(std::iter::once(0))
        .chain(instruction.bytes())
        .chain(std::iter::once(0))
        .chain(cfg.to_bits().to_le_bytes())
    {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Seconds of null-TTS silence for a text.
pub fn fallback_secs(text: &str) -> f32 {
    let secs = MIN_CLIP_SECS + SECS_PER_CHAR * text.chars().count() as f32;
    secs.clamp(MIN_CLIP_SECS, MAX_CLIP_SECS)
}

/// Silent PCM16 mono WAV at [`SAMPLE_RATE`] sized from the text length.
pub fn fallback_wav(text: &str) -> Vec<u8> {
    let n = (fallback_secs(text) * SAMPLE_RATE as f32).round() as usize;
    wav_bytes(&vec![0i16; n], SAMPLE_RATE)
}

/// Encode PCM16 mono samples as a WAV document.
pub fn wav_bytes(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * u32::from(CHANNELS) * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct WavInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub num_samples: usize,
}

/// Parse the 44-byte header of a WAV document produced by [`wav_bytes`].
pub fn parse_wav(data: &[u8]) -> Option<WavInfo> {
    if data.len() < 44 {
        return None;
    }
    if &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return None;
    }
    let sample_rate = u32::from_le_bytes(data[24..28].try_into().ok()?);
    let channels = u16::from_le_bytes(data[22..24].try_into().ok()?);
    let bits = u16::from_le_bytes(data[34..36].try_into().ok()?);
    let data_len = u32::from_le_bytes(data[40..44].try_into().ok()?) as usize;
    if bits != 16 || data.len() < 44 + data_len {
        return None;
    }
    Some(WavInfo {
        sample_rate,
        channels,
        bits,
        num_samples: data_len / 2,
    })
}

const B64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 encode (needed for the JSON speak response).
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let mut n: u32 = 0;
        for (i, b) in chunk.iter().enumerate() {
            n |= (*b as u32) << (16 - 8 * i);
        }
        let chars = match chunk.len() {
            3 => [
                Some(B64_TABLE[(n >> 18) as usize & 63] as char),
                Some(B64_TABLE[(n >> 12) as usize & 63] as char),
                Some(B64_TABLE[(n >> 6) as usize & 63] as char),
                Some(B64_TABLE[n as usize & 63] as char),
            ],
            2 => [
                Some(B64_TABLE[(n >> 18) as usize & 63] as char),
                Some(B64_TABLE[(n >> 12) as usize & 63] as char),
                Some(B64_TABLE[(n >> 6) as usize & 63] as char),
                None,
            ],
            _ => [
                Some(B64_TABLE[(n >> 18) as usize & 63] as char),
                Some(B64_TABLE[(n >> 12) as usize & 63] as char),
                None,
                None,
            ],
        };
        for c in chars.into_iter().flatten() {
            out.push(c);
        }
        for _ in 0..(3 - chunk.len()) {
            out.push('=');
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct Broker {
    pub cache_dir: PathBuf,
    pub upstream_url: String,
    pub client: reqwest::Client,
}

impl Broker {
    pub fn new(cache_dir: impl Into<PathBuf>, upstream_url: impl Into<String>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            upstream_url: upstream_url.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Speak, consulting cache first and falling back to silent PCM when the
    /// upstream GPU service is unreachable. Never fails for backend reasons;
    /// cache IO problems are treated as a miss.
    pub async fn speak(&self, req: &SpeakRequest) -> SpeakOutcome {
        let key = cache_key(&req.text, req.instruction.as_deref(), req.cfg_scale);
        if let Some(hit) = self.read_cache(&key).await {
            return hit;
        }
        if let Some((wav, sample_rate)) = self.fetch_upstream(req).await {
            let outcome = SpeakOutcome {
                subtitle: req.text.clone(),
                wav: wav.clone(),
                sample_rate,
                cached: false,
                fallback: false,
            };
            let _ = self.write_cache(&key, &outcome).await;
            return outcome;
        }
        SpeakOutcome {
            wav: fallback_wav(&req.text),
            sample_rate: SAMPLE_RATE,
            cached: false,
            fallback: true,
            subtitle: req.text.clone(),
        }
    }

    async fn read_cache(&self, key: &str) -> Option<SpeakOutcome> {
        let wav = tokio::fs::read(self.cache_dir.join(format!("{key}.wav")))
            .await
            .ok()?;
        let meta = tokio::fs::read(self.cache_dir.join(format!("{key}.json")))
            .await
            .ok()?;
        let meta: serde_json::Value = serde_json::from_slice(&meta).ok()?;
        let info = parse_wav(&wav)?;
        Some(SpeakOutcome {
            wav,
            sample_rate: info.sample_rate,
            cached: true,
            fallback: false,
            subtitle: meta.get("subtitle")?.as_str()?.to_string(),
        })
    }

    async fn write_cache(&self, key: &str, outcome: &SpeakOutcome) -> anyhow::Result<()> {
        tokio::fs::create_dir_all(&self.cache_dir).await?;
        tokio::fs::write(
            self.cache_dir.join(format!("{key}.wav")),
            &outcome.wav,
        )
        .await?;
        let meta = serde_json::json!({
            "subtitle": outcome.subtitle,
            "sample_rate": outcome.sample_rate,
        });
        tokio::fs::write(
            self.cache_dir.join(format!("{key}.json")),
            serde_json::to_vec(&meta)?,
        )
        .await?;
        Ok(())
    }

    /// Ask the upstream Breeze server for raw s16le PCM and wrap it as WAV.
    /// Returns None when the GPU service is down or answers badly.
    async fn fetch_upstream(&self, req: &SpeakRequest) -> Option<(Vec<u8>, u32)> {
        let url = format!("{}/v1/audio/speech", self.upstream_url.trim_end_matches('/'));
        let resp = self
            .client
            .post(url)
            .form(&[
                ("text", req.text.as_str()),
                ("instruction", req.instruction_or_default()),
                ("cfg_scale", &req.cfg_or_default().to_string()),
            ])
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let sample_rate = resp
            .headers()
            .get("x-sample-rate")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(SAMPLE_RATE);
        let body = resp.bytes().await.ok()?;
        if body.is_empty() || body.len() % 2 != 0 {
            return None;
        }
        let samples: Vec<i16> = body
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        Some((wav_bytes(&samples, sample_rate), sample_rate))
    }

    pub fn cache_paths(&self, key: &str) -> (PathBuf, PathBuf) {
        (
            self.cache_dir.join(format!("{key}.wav")),
            self.cache_dir.join(format!("{key}.json")),
        )
    }

    pub fn cache_dir_path(&self) -> &Path {
        &self.cache_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_stable_and_covers_all_fields() {
        let a = cache_key("hello", None, None);
        let b = cache_key("hello", None, None);
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert_ne!(a, cache_key("hello!", None, None));
        assert_ne!(a, cache_key("hello", Some("Loud."), None));
        assert_ne!(a, cache_key("hello", None, Some(3.0)));
        assert_eq!(a, cache_key("hello", Some(DEFAULT_INSTRUCTION), Some(1.0)));
    }

    #[test]
    fn base64_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"M"), "TQ==");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"Man"), "TWFu");
    }

    #[test]
    fn fallback_wav_is_valid_24k_mono() {
        let wav = fallback_wav("hello");
        let info = parse_wav(&wav).expect("valid wav");
        assert_eq!(info.sample_rate, SAMPLE_RATE);
        assert_eq!(info.channels, 1);
        assert_eq!(info.bits, 16);
        let body = &wav[44..];
        assert!(body.iter().all(|b| *b == 0));
        let expect = (fallback_secs("hello") * SAMPLE_RATE as f32).round() as usize;
        assert_eq!(info.num_samples, expect);
    }

    #[test]
    fn fallback_secs_scales_and_clamps() {
        assert_eq!(fallback_secs(""), MIN_CLIP_SECS);
        assert!(fallback_secs("short") > MIN_CLIP_SECS);
        assert_eq!(fallback_secs(&"x".repeat(10_000)), MAX_CLIP_SECS);
    }

    #[tokio::test]
    async fn speak_falls_back_when_upstream_is_down() {
        let dir = std::env::temp_dir().join("breadstudy-breeze-test-down");
        let _ = std::fs::remove_dir_all(&dir);
        let broker = Broker::new(&dir, "http://127.0.0.1:1");
        let req = SpeakRequest::new("The volume of a cone is one third pi r squared h.");
        let out = broker.speak(&req).await;
        assert!(out.fallback);
        assert!(!out.cached);
        assert_eq!(out.subtitle, req.text);
        let info = parse_wav(&out.wav).expect("valid wav");
        assert_eq!(info.sample_rate, SAMPLE_RATE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Minimal stub of the upstream Breeze HTTP API: answers one raw-PCM body
    /// with an X-Sample-Rate header and counts hits.
    async fn stub_upstream(pcm: Vec<u8>) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>, tokio::task::JoinHandle<()>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();
        let handle = tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                hits_clone.fetch_add(1, Ordering::SeqCst);
                // Drain the request (headers plus form body) before answering.
                let mut buf = vec![0u8; 8192];
                let mut seen = Vec::new();
                let mut content_len = 0usize;
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    seen.extend_from_slice(&buf[..n]);
                    if let Some(end) = find_headers_end(&seen) {
                        content_len = header_content_length(&seen[..end]);
                        let have = seen.len() - end;
                        let mut need = content_len.saturating_sub(have);
                        while need > 0 {
                            let n = sock.read(&mut buf).await.unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            need = need.saturating_sub(n);
                        }
                        break;
                    }
                }
                let _ = content_len;
                let body_len = pcm.len();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: audio/pcm\r\nX-Sample-Rate: 24000\r\nContent-Length: {body_len}\r\nConnection: close\r\n\r\n"
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&pcm).await;
            }
        });
        (format!("http://{addr}"), hits, handle)
    }

    fn find_headers_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| i + 4)
    }

    fn header_content_length(head: &[u8]) -> usize {
        let text = String::from_utf8_lossy(head).to_lowercase();
        text.lines()
            .filter_map(|l| l.strip_prefix("content-length:"))
            .filter_map(|v| v.trim().parse().ok())
            .next()
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn speak_caches_upstream_hit_and_serves_without_model() {
        use std::sync::atomic::Ordering;
        let dir = std::env::temp_dir().join("breadstudy-breeze-test-cache");
        let _ = std::fs::remove_dir_all(&dir);
        let pcm = vec![0u8; 4800];
        let (url, hits, handle) = stub_upstream(pcm).await;
        let broker = Broker::new(&dir, &url);
        let req = SpeakRequest::new("Area equals pi r squared.");
        let first = broker.speak(&req).await;
        assert!(!first.fallback);
        assert!(!first.cached);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        handle.abort();
        // Stub is gone: the second identical line must come from cache.
        let second = broker.speak(&req).await;
        assert!(!second.fallback);
        assert!(second.cached);
        assert_eq!(second.wav, first.wav);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
