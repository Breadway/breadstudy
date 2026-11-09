# AGENTS.md - breadstudy

Rust axum + SQLite, same deploy pattern as bread-tracker and breadboard.

- `feature/*` or `fix/*` branches, never commit straight to main. Push origin only.
- Never add AI attribution lines to commits or PRs.
- Never use em dashes in commits, PR/issue bodies, or files.
- Host port 7220, container 7290, BIPS id `breadstudy-web-prod`.
- TTS broker port 7310 (loopback), BIPS id `breadstudy-tts-prod`. Upstream Breeze server on 7311.
- Vault source is `~/Projects/school-notes-year-10/Maths`. Generated content never silently rewrites the vault.
