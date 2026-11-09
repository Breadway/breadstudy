# Deploying breadstudy on Hestia

```sh
rsync -a --delete --exclude target --exclude .git ./ hestia:~/breadstudy/
ssh hestia 'cd ~/breadstudy && BREADSTUDY_BIND=127.0.0.1:7220 docker compose -f packaging/compose.yaml --project-directory . up -d --build'
curl -s http://127.0.0.1:7220/healthz
```

Run compose as the `breadway` user, not root.

## Grading model configuration (STUDY-17)

The grader talks to an OpenAI-compatible endpoint. Defaults point at the
OpenCode Go API; override them in `.env` or the shell when needed.

| Variable | Default |
|---|---|
| `BREADSTUDY_LLM_BASE_URL` | `https://opencode.ai/zen/go/v1` |
| `BREADSTUDY_LLM_MODEL` | `deepseek-v4.1-flash` |
| `BREADSTUDY_LLM_API_KEY_FILE` | `/etc/breadstudy/llm.key`, then `$XDG_CONFIG_HOME/breadstudy/opencode-go.key` |
| `BREADSTUDY_LLM_FALLBACK_BASE_URL` | `http://127.0.0.1:7301/v1` |
| `BREADSTUDY_LLM_FALLBACK_MODELS` | `Ornith-1.5-9B-Q4_K_M,LFM3-Omni-2B-Q4_K_M` |
| `BREADSTUDY_LLM_MIN_CONFIDENCE` | `0.75` |
| `BREADSTUDY_LLM_KEY_HOST_PATH` | `/home/breadway/.config/breadstudy/opencode-go.key` |

The API key is never committed, logged or baked into the image. It lives on
the host outside the repo (the repo is public) with mode 600, and compose
bind-mounts it read-only at `/etc/breadstudy/llm.key`:

```sh
install -d -m 700 ~/.config/breadstudy
install -m 600 /dev/null ~/.config/breadstudy/opencode-go.key
$EDITOR ~/.config/breadstudy/opencode-go.key   # paste the key, save
stat -c '%a %n' ~/.config/breadstudy/opencode-go.key
```

Do not put `BREADSTUDY_LLM_API_KEY` in `.env` or compose: a direct value is
visible through `docker inspect`. The file mount is the supported path.

`HttpGrader::from_env` validates the key at startup. A missing or empty file
fails with `no LLM API key found` and the paths it tried. A rejected key
(401/403) surfaces as a configuration error and is not hidden by the local
fallback. Only transient failures such as a timeout or 5xx fall through to
the local vision models, marked degraded and graded at reduced quality.

For a host binary run outside compose:

```sh
BREADSTUDY_LLM_API_KEY_FILE=$HOME/.config/breadstudy/opencode-go.key \
  cargo run -p breadstudy-server
```

## breeze-server TTS broker (STUDY-10)

Local TTS broker on loopback port 7312 (BIPS id `breadstudy-tts-prod`).
It fronts the upstream Breeze TTS server on 7313 with a file cache keyed on
(text, instruction, cfg_scale) and a null-TTS fallback: when the GPU service
is down it answers silent 24 kHz PCM plus subtitles so sessions continue
text-only.

```sh
cargo build --release
cp packaging/breeze-server.service ~/.config/systemd/user/
systemctl --daemon-reload --user
systemctl --enable --now --user breeze-server
curl -s http://127.0.0.1:7312/healthz
curl -s -X POST http://127.0.0.1:7312/v1/speak \
  -H 'Content-Type: application/json' \
  -d '{"text":"Area equals pi r squared."}'
```

Env overrides: `BREEZE_LISTEN` (default `127.0.0.1:7312`),
`BREEZE_CACHE_DIR` (default `data/breeze-cache`),
`BREEZE_UPSTREAM_URL` (default `http://127.0.0.1:7313`).

Port registry note: propose these PORTS.md rows in the infra repo
(registration pending, applied from this repo design first):

`| 7312 | 127.0.0.1 | breadstudy-tts-prod | ai-inference | breeze-server TTS broker, loopback only |`
`| 7313 | 127.0.0.1 | breadstudy-tts-upstream-prod | ai-inference | upstream Breeze TTS server, loopback only |`

Known conflicts 2026-11-09: the first-choice ports 7310 and 7311 are held by
long-running embedding services (llama-server MiniLM on 7310, embed-shim on
7311), and the 7307 fallback is held by opencode-go-proxy. None of these
rows are registered in PORTS.md; an INFRA issue tracks them. Do not enable
the unit on any port until the registry confirms the assignment.

## Seed the item bank

The image has no vault. Seed from the host checkout into the live volume with a
one-off container:

```sh
docker run --rm --name studyseed-runner \
  --entrypoint breadstudy-seed \
  -v breadstudy_breadstudy-data:/var/lib/breadstudy \
  -v /home/breadway/Projects/school-notes-year-10:/vault:ro \
  breadstudy:local \
  --db /var/lib/breadstudy/breadstudy.db --vault /vault/Maths
```

It prints a JSON report per Practice set. Re-running is idempotent; answers
that fail the SymPy gate stay `review_required` instead of being silently fixed.

## Edge

Hestia Caddy (LAN) and Aegis (public) reverse-proxy `study.breadway.dev` to host port 7220; blocks live in the infra repo. Set `BREADSTUDY_BIND=0.0.0.0:7220` in `.env` so the Tailscale path can reach it. BIPS id `breadstudy-web-prod`.
