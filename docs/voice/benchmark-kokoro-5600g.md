# Kokoro TTS on Hestia (Ryzen 5 5600G, CPU only)

Measured 2026-11-10 with `kokoro-onnx` 0.6.1, `onnxruntime` 1.31.1 and
`kokoro-v1.0.onnx` (voices v1.0). No GPU used.

## Throughput

| Voice | Audio | Wall time | RTF |
|---|---|---|---|
| `bm_george` | 7.7s | 2.90s | 0.38x |
| `af_heart` | 6.9s | 1.94s | 0.28x |
| `bf_alice` | 7.0s | 1.91s | 0.27x |

Three-sentence paragraph, mixed: 8.62s of audio in 3.06s, RTF 0.35x, about
2.9x faster than realtime.

## Latency to first audio

Short sentences, model warm:

| Phrase | Time to audio | Speech length |
|---|---|---|
| "So the base is a circle." | 0.759s | 2.07s |
| "Its area is pi r squared." | 0.875s | 2.35s |
| "Now multiply by the height." | 0.843s | 2.30s |
| "That gives you the volume." | 0.702s | 1.77s |

Cold load is about 1.26s.

## Interpretation

Throughput is comfortably realtime; the floor is about 0.7-0.9s per chunk
because phonemization plus the model pass dominate regardless of sentence
length. Playback never outruns generation, so sentence-level pipelining (start
sentence 1 immediately, prefetch the next) gives continuous speech with one
short pause at the start.

Comparison: Breeze q4_k on the A380 measured RTF 0.62x and about 1.3s to first
audio, and requires unloading the llama router on a 6 GB card. Kokoro is faster
on both axes but sounds smaller; keep Breeze as the explicit quality fallback.
