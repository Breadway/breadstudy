# breadstudy DESIGN

## Problem

Year 10 Maths study needs a loop that decides what to do, runs a timed session, grades the work, and remembers. A chatbot alone does not do this.

## Loop

1. Plan: vault indexer reads the Methods Pathway overview, Test 1-7 map, and syllabus checklist. Planner assigns the next skill from test proximity plus FSRS due-ness. Interleaves strands.
2. Session: state machine `prime -> recall -> practice -> explain -> reflect -> break -> end`. 20 min school day, 40 min weekend.
3. Items: seeded from the six Practice sets with worked answers (gold, re-checked with SymPy). Generated items carry rubric, hints, expected time, difficulty, source URLs. Symbolic/numeric answers gate on SymPy before entering the bank. Unverifiable items (proofs, geometric reasoning) stay `review_required` until approved once.
4. Grading: photo, typed LaTeX, or spoken. Method marks plus final answer. Error tags: sign-slip, dropped-unit, wrong-formula, algebra, conceptual.
5. Memory: attempts, mastery per skill, error patterns, session and intervention log. Weekly review generated from this.
6. Teaching board: the model authors explanations on the scratchpad (prose, LaTeX, steps and SVG diagrams) through `POST /api/teach`, revealed in sequence. The student asks, the tutor draws. Explanations can be saved to the vault under a marked agent section or `_agent/` folder. Never silently rewrites notes. Small review queue.

## Surfaces

- Hestia server (Rust axum + SQLite, litestream, port 7220, study.breadway.dev)
- Web PWA (session view, teacher board, dashboard, camera/mic input)
- Hermes friction client (workspace switch, hide distractions, DND, timer, focus events, skip + grace tokens)
- Phone (PWA + Gotify, optional ICS feed to Nextcloud)

## Voice

- TTS: breeze-server systemd unit on 7310, Vulkan on A380, q4_k first, PCM streaming. Math normalizer converts LaTeX to speech text. Cache key (text, instruction, cfg_scale). Null-TTS fallback with subtitles.
- Voice: Patient coach (male mentor, Australian, slow, precise). Voice design, not cloning.
- STT: whisper.cpp local for explain-back.

## Content pipeline

- Nightly 02:00: pick due skills, generate 2-3 items per skill for the week, verify with SymPy, dedupe, prefetch web sources, pre-render TTS.
- Sessions serve from cache. Only live explanations hit a model. Web search at fetch time via searxng 7102, never mid-session except explicit lookup. Every item stores source URL, snippet, fetch date.

## Enforcement (soft friction only)

T-10 Gotify ping, start switches workspace and hides distractions, DND on, session timer. Off-task over 60s pauses the timer with a nudge. Nothing blocked. Skip logs a reason, re-plans, shows weekly skip count. Escalation ceiling: +5/+15 min re-ping and an incomplete-session note. Grace tokens for legit skips.

## Models

- Generation and grading via BMNS proxy / CLIProxyAPI. Local fallback Ornith-1.5-9B and LFM3-Omni-2B for offline photo grading at reduced quality.
- Breeze and whisper are explicit additions under BMNS when they get endpoints.

## Phases

- P0: Breeze Vulkan RTF/VRAM with router unloaded, one verified Volume/SA loop, Gotify push
- P1: indexer, planner, bank with SymPy gate, FSRS, session UI, PWA, photo grading, hint ladder, 20/40 templates
- P2: Breeze wiring and cache, math normalizer, scratchpad, vault write-back, whisper explain-back
- P3: hermes client, ladder, ICS, activity signals
- P4: nightly generation, daily plan, weekly review, puzzle rewards, error coaching, assessment countdown
