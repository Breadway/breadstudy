# Breeze-TTS-2 q4_k benchmark on the A380 (STUDY-6)

Date: 2026-11-09. Card: 6 GB Intel Arc A380, Mesa 25.2.8, Vulkan backend.
Model: `breeze-tts-2-q4_k.gguf` (2,538,909,376 bytes).

## Result

Sentence: "The volume of a cone is one third pi r squared h."
Direction: steady male mentor, Australian accent, slow and precise.
Seed 42, default sampling, cfg_scale 1.0 (guidance off).

| Run | Backbone | Depth | Vocoder | Total/frame | RTF | TTFA | Audio |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 (warmup) | 23.71 ms | 111.91 ms | 301.65 ms | - | - | 22.7 s | 5.60 s |
| 2 | 20.81 ms | 81.12 ms | 27.47 ms | 129.40 ms | 0.62x | 1.29 s | 5.60 s |
| 3 | 19.93 ms | 80.91 ms | 29.15 ms | 129.99 ms | 0.62x | 1.37 s | 5.60 s |

RTF is seconds of audio per second of wall clock (80 ms frames), so 0.62x is
slower than realtime on this card. The depth decoder dominates at about
81 ms per frame. Per the upstream docs the first run builds GPU pipelines
and does not count; runs 2 and 3 agree.

Output clip: `~/.cache/voice-builds/study6-q4k-cone-volume.wav`
(24 kHz mono 16-bit, 5.60 s, RMS 1838 with energy varying across halves,
so non-silent modulated audio).

Ear check: programmatic checks pass. Human listen pending; play the clip
above to confirm the Patient coach delivery before closing STUDY-6.

## VRAM

Peak VRAM could not be read with unprivileged tooling on this stack:
Anv `VK_EXT_memory_budget` heapUsage reads 0.0 even with the router model
resident, `intel_gpu_top` aborts on startup, and i915 exposes no
per-process VRAM counter here. Lower bound is the 2.4 GiB weight file.
The run completed with no OOM on the 6 GB card with the router unloaded.

q8_0 was not fetched or benchmarked: q4_k is already sub-realtime and
depth-decoder bound, so q8_0 would be slower still. Revisit only if a
larger card or the `--te-cpu` tradeoff changes the picture.

## Procedure (GPU broker steps)

The router holds the card in normal operation, so benchmarking needs a
short unload window authorized by the operator:

1. `systemctl --user stop llamacpp.service`, confirm `inactive` and that
   port 7301 is free.
2. Run the CLI from the build checkout:
   `breeze-cli models/breeze-tts-2-q4_k.gguf --text "..." --instruction "..."
   --output out.wav --timings` (twice; the second run counts).
3. Immediately `systemctl --user start llamacpp.service`, confirm `active`
   and that `curl http://127.0.0.1:7301/v1/models` lists models.

GPU engagement evidence: ggml logs `Found 1 Vulkan devices ... A380` and
`backend: Vulkan0` for each run.
