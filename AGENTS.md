# AGENTS.md - breadstudy

Rust axum + SQLite, same deploy pattern as bread-tracker and breadboard.

## Rules

- `feature/*` or `fix/*` branches, never commit straight to main. Push origin only.
- Never add AI attribution lines to commits or PRs.
- Never use em dashes in commits, PR/issue bodies, or files.
- Never commit secrets or model weights. The repository is public.

## Ports and identity

- Web: host 7220, container 7290, BIPS id `breadstudy-web-prod`.
- TTS: compose service `tts` on 7314 inside the compose network, no host port; host-only unit uses loopback 7314. Host-loopback services are not reachable from the app container.
- Breeze broker: 7312, upstream Breeze server 7313, BIPS id `breadstudy-tts-prod`.
- Breeze benchmark numbers: `docs/voice/benchmark-q4k-a380.md`.

## Secrets

- LLM key: `~/.config/breadstudy/opencode-go.key` (mode 600), mounted read-only at `/etc/breadstudy/llm.key`. Never bake it into the image, never echo it.
- Gemini TTS key, when used: `~/.config/breadstudy/gemini.key` or `BREADSTUDY_GEMINI_API_KEY`.

## Data

- Vault source: `~/Projects/school-notes-year-10/Maths`. Generated content never silently rewrites the vault.
- SQLite lives in the `breadstudy_breadstudy-data` volume, replicated by litestream.
- Reseeding and generation commands are in `packaging/DEPLOY.md`.

## Checks

```sh
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
node --check web/app.js
python3 web/tests/check_pwa.py
```

Builds go through the shared cargo semaphore wrapper at `~/.local/bin/cargo`. Hermes (`breadway@100.92.66.62`) is faster when online.

## Do not

- Do not stop, remove or restart production containers you did not create.
- Do not unload the llama router to run Breeze without operator approval.
- Do not mark an item verified unless it passed the SymPy gate and has a source.
