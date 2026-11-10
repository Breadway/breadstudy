# breadstudy

Study companion for Year 10 Maths (Nagle Accelerate / Methods pathway). A deterministic planner and mastery model decide what to study; the model generates, grades and teaches inside that plan.

Live at **https://study.breadway.dev** (Hestia container 7220, Aegis edge).

## What it does

- **Plans:** indexes the vault (`~/Projects/school-notes-year-10/Maths`), maps skills to the Test 1-7 course map, and orders practice by FSRS due-ness, mastery and assessment proximity.
- **Runs sessions:** 20-minute school-day and 40-minute weekend templates with prime, recall, practice, explain, reflect, break and end phases.
- **Serves items:** 300+ verified practice items across 20 skills, seeded from the vault Practice sets and generated with SymPy verification. Items not served in the last 7 days come first after due order.
- **Grades:** typed answers, LaTeX and photos, with method marks plus final answer, error tags and written feedback. Graded attempts update FSRS mastery and due dates.
- **Teaches:** a teaching board where the model writes prose, equations, steps and SVG diagrams, with speech and an interrupt control.
- **Reviews:** progress page with attempt history, per-skill mastery, recurring error patterns; weekly snapshot endpoint.
- **Pushes:** Gotify reminders and a soft-friction client for the laptop (no blocking).

## Stack

- Server: Rust axum + SQLite (litestream replication), container on Hestia.
- Web: dependency-free PWA in `web/` (KaTeX from CDN for maths).
- Models: DeepSeek V4.1 Flash through the OpenCode Go API for generation, grading and teaching; local llama.cpp fallbacks for grading.
- Speech: Kokoro as the `tts` service on the compose network (no host port), Gemini TTS when a key is configured, Breeze as an explicit host-only GPU quality fallback, browser speech as the always-works option. Host-only runs can use the loopback systemd unit instead of the compose service.

## Quick start

```sh
# on hestia
cd ~/Projects/breadstudy
BREADSTUDY_BIND=0.0.0.0:7220 docker compose -p breadstudy \
  -f packaging/compose.yaml --project-directory=. up -d --build
curl -s localhost:7220/healthz
```

Key at `~/.config/breadstudy/opencode-go.key`, mounted read-only into the container. See `packaging/DEPLOY.md` for the full runbook, seeding and item generation.

## Layout

- `server/src` - API, session engine, planner, FSRS, grading, teaching, static serving
- `migrations/` - SQLite schema
- `web/` - PWA: dashboard, session, progress, skill, teach
- `scripts/` - seed and item generation
- `spec/` - session rules and skill map
- `clients/hermes` - soft-friction client
- `pipelines/`, `services/` - nightly generation and reminders
- `docs/voice/` - Breeze benchmark on the A380

## Tests

```sh
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 web/tests/check_pwa.py
```
