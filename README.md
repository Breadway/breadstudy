# breadstudy

Study companion for Year 10 Maths (Nagle Accelerate / Methods pathway). Deterministic scheduler plus tutor model, not a free-roaming agent.

Loop: plan from the vault, run the session, grade the work, write mastery back, save explanations to the vault.

- Server: Rust axum + SQLite (same pattern as bread-tracker), host port 7220, `study.breadway.dev`
- Vault source: `~/Projects/school-notes-year-10/Maths` (test map, syllabus checklist, practice sets)
- Voice: Breeze-TTS-2.cpp on Vulkan (A380), whisper.cpp for explain-back
- Phone: PWA plus Gotify. No FCM dependency for v1.

See `DESIGN.md` for the full loop and `spec/` for the session state machine.

## Status

Live at https://study.breadway.dev (container on Hestia, Aegis edge). The vault indexer, item bank with SymPy gate, seed bank, planner, session engine, FSRS mastery, photo grading, Breeze TTS broker, whisper STT, PWA, Gotify reminders, friction client and weekly review are merged.

DeepSeek V4.1 Flash grades through the OpenCode Go API (`BREADSTUDY_LLM_*`), with local llama.cpp fallbacks marked degraded. Regenerate the item bank with `breadstudy-seed` after changing the vault.

## Layout

- `server/` - axum API, planner, item bank, grading, mastery store
- `migrations/` - SQLite schema
- `packaging/` - Containerfile, compose, litestream, DEPLOY
- `spec/` - session state machine, FSRS params, skill map
- `web/` - PWA session view and scratchpad (scaffold)
