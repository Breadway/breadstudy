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
| `BREADSTUDY_LLM_FALLBACK_BASE_URL` | `http://host.docker.internal:7301/v1` |
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

## Speech backends

Order of preference, all with the same interrupt behaviour:

1. **Kokoro** (default, CPU): `POST /v1/audio/speech` on loopback 7314, served by
   a small kokoro-onnx sidecar. Measured on hestia: RTF 0.37x on the Ryzen 5
   5600G, 0.7-0.9s to first audio per sentence. Model files are about 340MB and
   live outside the repo:

   ```sh
   install -d -m 755 /var/lib/breadstudy/tts
   curl -L -o /var/lib/breadstudy/tts/kokoro-v1.0.onnx \
     https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/kokoro-v1.0.onnx
   curl -L -o /var/lib/breadstudy/tts/voices-v1.0.bin \
     https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/voices-v1.0.bin
   ```

2. **Gemini TTS** when `BREADSTUDY_GEMINI_API_KEY` or
   `BREADSTUDY_GEMINI_API_KEY_FILE` (default `/etc/breadstudy/gemini.key`) is
   set. Model defaults to `gemini-2.5-flash-preview-tts`.
3. **Breeze** as an explicit quality fallback (see below). Needs the GPU.
4. **Browser speech** on the client when no server backend is reachable.

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

## Generate more practice items

`scripts/generate_items.py` asks the model for original problems per skill,
verifies `answer_expr` with SymPy, attaches one SearXNG source and inserts them:

```sh
docker run --rm --network host --entrypoint python3 \
  -v breadstudy_breadstudy-data:/var/lib/breadstudy \
  -v /home/breadway/.config/breadstudy/opencode-go.key:/etc/breadstudy/llm.key:ro \
  breadstudy:local /app/scripts/generate_items.py --per-skill 4
```

`--network host` is required because SearXNG binds loopback on the host.
Without a source, items are stored as `review_required` rather than verified.

## Reset practice data

To wipe test attempts and mastery so real study starts clean:

```sh
docker compose -p breadstudy exec breadstudy sh -c 'true' 2>/dev/null || true
sudo sqlite3 /var/lib/docker/volumes/breadstudy_breadstudy-data/_data/breadstudy.db \
  "DELETE FROM attempts; DELETE FROM skill_schedule; DELETE FROM error_patterns; DELETE FROM session_runs; DELETE FROM session_events; DELETE FROM served_items; DELETE FROM hint_state;"
```

Stop the container first if you want a clean replacement, and remember
litestream will replicate the change. Items, skills and rubrics are untouched.

## Preview instance

To show a build without touching production, run a second container on 17220
against a copy of the database:

```sh
install -d -m 775 /tmp/opencode/breadstudy-preview
sudo cp /var/lib/docker/volumes/breadstudy_breadstudy-data/_data/breadstudy.db \
  /tmp/opencode/breadstudy-preview/breadstudy.db
sudo chown breadway:breadway /tmp/opencode/breadstudy-preview/breadstudy.db
docker run -d --name studypreview-web -p 0.0.0.0:17220:7290 \
  -v /tmp/opencode/breadstudy-preview:/var/lib/breadstudy \
  -v ~/.config/breadstudy/opencode-go.key:/etc/breadstudy/llm.key:ro \
  breadstudy:local
```

## Edge

Hestia Caddy (LAN) and Aegis (public) reverse-proxy `study.breadway.dev` to host port 7220; blocks live in the infra repo. Set `BREADSTUDY_BIND=0.0.0.0:7220` in `.env` so the Tailscale path can reach it. BIPS id `breadstudy-web-prod`.
