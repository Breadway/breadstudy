# breadstudy DESIGN

## Problem

Year 10 Maths study needs a loop that decides what to do, runs a timed session, grades the work, and remembers. A chatbot alone does not do this.

## Loop

1. **Plan.** The vault indexer reads the Methods Pathway overview, the Test 1-7 map and the syllabus checklist. The planner orders skills by FSRS due score, then mastery gap, then assessment proximity, and rotates items so a skill is not drilled with the same questions twice in a week.
2. **Session.** State machine `prime -> recall -> practice -> explain -> reflect -> break -> end`, driven by the server. 20-minute school-day and 40-minute weekend templates. Skip consumes an item and stays in phase; the grid only advances when the phase is exhausted.
3. **Items.** Seeded from the Practice sets, then generated per skill. Every item must be SymPy-checkable and carry a source to count as `verified`; otherwise it is stored `review_required`. Rubrics are per-step method marks plus a final answer, and a bare final answer earns the final mark only. Multi-part exam items carry parts, per-part marks and a mark scheme.
4. **Grading.** Typed text, LaTeX or a photo, resized client-side. The model grades against the rubric; a deterministic final-answer check overrides it where the answer is numerically comparable. Error tags: sign-slip, dropped-unit, wrong-formula, algebra, conceptual.
5. **Memory.** A graded attempt writes `attempts`, FSRS `skill_schedule`, error patterns and the session log. Mastery is `0.6 * accuracy + 0.4 * stability/(stability+7)`; due score is `(1-mastery)*2 + overdue urgency - future discount`. Skills never graded show the 0.4 baseline.
6. **Teaching board.** The model authors prose, displayed LaTeX, numbered steps and an SVG diagram through `POST /api/teach`. Blocks reveal in sequence and are spoken. A short transcript keeps follow-ups in context. The student can interrupt at any point: speech stops, the request aborts, and the question box takes focus.
7. **Review.** The progress page shows recent attempts with verdicts and tags, per-skill mastery with due dates, and the most frequent error pattern. The weekly snapshot aggregates sessions and attempts.

## Surfaces

- Hestia server (Rust axum + SQLite, litestream, port 7220, study.breadway.dev)
- Web PWA: dashboard, session, progress, skill and teach pages, camera and mic input
- Hermes friction client (workspace switch, hide distractions, DND, timer, skip and grace tokens)
- Phone: PWA plus Gotify; the phone can be the camera while the laptop runs the session

## Voice

Backend order, all with identical barge-in behaviour:

1. **Kokoro** (default): 82M ONNX model on CPU, measured RTF 0.37x on the Ryzen 5 5600G, about 0.7-0.9s to first audio per sentence. No GPU, so it does not contend with the llama router. Voices include `bm_george` (patient British male), `af_heart`, `bf_alice`.
2. **Gemini TTS** when `BREADSTUDY_GEMINI_API_KEY` or its key file is set. Quality option, sentence-chunked because the API returns whole clips.
3. **Breeze** (explicit quality fallback): 3B model, q4_k on the A380, RTF 0.62x, about 1.3s to first audio. Requires the GPU and does not unload the llama router automatically.
4. **Browser speech**: zero-install fallback, always available.

Latency strategy: synthesize sentence 1 as soon as the first text arrives, prefetch the next while the current plays, pre-warm the model when the page opens. Math normalizer converts LaTeX to speakable text; never reads raw TeX.

## Content pipeline

- `scripts/generate_items.py` asks the model for original problems per skill, verifies `answer_expr` with SymPy, attaches one SearXNG source and dedupes by prompt hash. Needs `--network host` because SearXNG is host-loopback.
- Sessions serve from cache: items, hints and audio paths are resolved at start. Only teaching and explanations call a model live.
- Web search happens at fetch time, never mid-session, except an explicit lookup. Every item stores source URL, snippet and fetch date.

## Enforcement (soft friction only)

T-10 Gotify ping, workspace switch, hide distractions, DND and a session timer. Off-task over 60s pauses with a nudge. Nothing is blocked. Skip logs a reason and re-plans, with a weekly count and grace tokens. Escalation ceiling: +5 and +15 minute re-pings plus an incomplete-session note.

## Deployment

Container on Hestia behind Hestia Caddy (LAN) and Aegis (public). Ports: 7220 web, 7290 container, TTS sidecar 7314, Breeze broker 7312/7313. SQLite replicated continuously with litestream to `/mnt/backups/breadstudy`.

## Status

Shipped: indexer, item bank with SymPy gate, seed and generator, planner, session engine with multi-part items, FSRS mastery, photo grading, teaching board, progress and skill pages, UI overhaul, Gotify reminders, friction client, weekly snapshot.

Open: multi-part rendering in the PWA, phone session pairing, weekly review page, vault note text on the skill page, Kokoro/Gemini/Breeze backend wiring.
