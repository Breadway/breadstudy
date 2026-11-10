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
`| 7314 | 127.0.0.1 | breadstudy-kokoro-tts | ai-inference | Kokoro ONNX TTS sidecar, loopback only |`

Known conflicts 2026-11-09: the first-choice ports 7310 and 7311 are held by
long-running embedding services (llama-server MiniLM on 7310, embed-shim on
7311), and the 7307 fallback is held by opencode-go-proxy. None of these
rows are registered in PORTS.md; an INFRA issue tracks them. Do not enable
the unit on any port until the registry confirms the assignment.

## Teaching mode speech setup

The teaching page is served at `/teach.html` and uses the backend order
above. Every successful response carries `X-Breadstudy-TTS-Backend` and
`X-Breadstudy-TTS-Voice` so the UI can show what actually served the clip.
The page requests one sentence at a time and prefetches the next sentence
while the current one plays. The Gemini public TTS API returns whole clips
rather than a stream, so per-sentence requests are the latency strategy. The
same sentence pipeline and barge-in path are used for Kokoro, Gemini, Breeze
and browser speech.

### Kokoro sidecar (default, local CPU)

The sidecar is `packaging/tts/kokoro_server.py` and binds loopback only.
Port 7314 is the default because 7310 and 7311 are occupied. Check before
enabling it:

```sh
ss -ltn 'sport = :7314'
```

Download the two model files with the commands above, then install the
Python dependency in a venv:

```sh
python3 -m venv ~/.local/share/breadstudy/kokoro-venv
~/.local/share/breadstudy/kokoro-venv/bin/pip install -r packaging/tts/requirements.txt
```

Install and start the user unit:

```sh
cp packaging/tts/breadstudy-kokoro-tts.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now breadstudy-kokoro-tts
curl -s http://127.0.0.1:7314/healthz
curl -s -o /tmp/kokoro-check.wav -w '%{http_code} %{time_starttransfer}\n' \
  -X POST http://127.0.0.1:7314/v1/audio/speech \
  -H 'Content-Type: application/json' \
  -d '{"input":"Area equals pi r squared.","voice":"bm_george","speed":1.0}'
```

The service lazy-loads the model on the first request and keeps it warm. It
serializes synthesis, caps input at 1200 characters, and refuses a
non-loopback bind address.

Container note: compose sets `BREADSTUDY_KOKORO_TTS_URL` to
`http://host.docker.internal:7314` because the sidecar runs on the host. A
host binary uses the default `http://127.0.0.1:7314`. The sidecar binds
loopback only, so a plain Docker bridge container cannot reach it through
`host.docker.internal` unless you deliberately run a forwarder on the Docker
gateway. If you keep the sidecars loopback-only, run the breadstudy server as
a host binary for teaching mode, or accept that the page falls back to browser
speech. The same caveat applies to the Breeze broker on 7312.

### Gemini quality fallback

Gemini is used only when a key exists. The key is never baked into the image
or logged. The supported path is a host file mounted read-only:

```sh
install -d -m 700 ~/.config/breadstudy
install -m 600 /dev/null ~/.config/breadstudy/gemini.key
$EDITOR ~/.config/breadstudy/gemini.key
```

Then uncomment the Gemini volume line in `packaging/compose.yaml` and
recreate the container. For a host binary, set
`BREADSTUDY_GEMINI_API_KEY_FILE` to that path. The key is read at startup, so
restart after adding it.

| Variable | Default |
|---|---|
| `BREADSTUDY_GEMINI_API_KEY` | unset; direct environment value, avoid in compose |
| `BREADSTUDY_GEMINI_API_KEY_FILE` | `/etc/breadstudy/gemini.key` |
| `BREADSTUDY_GEMINI_TTS_MODEL` | `gemini-2.5-flash-preview-tts` |

The route maps the request voice onto the Gemini voice set and returns WAV.
If Gemini is selected but unconfigured or upstream fails, the route answers
503 and the page falls back to browser speech instead of going silent.

### Breeze quality fallback

Breeze is explicit. The route uses the broker on
`BREADSTUDY_BREEZE_TTS_URL` (default `http://127.0.0.1:7312`) only when it is
already running; it never starts the broker and never unloads or restarts the
llama router. Start it yourself when the GPU is free:

```sh
cargo build --release
cp packaging/breeze-server.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now breeze-server
curl -s http://127.0.0.1:7312/healthz
```

The Breeze request uses the "patient coach" voice design: a calm,
encouraging male mentor with an Australian accent, speaking slowly and
precisely. If the broker is up but its GPU upstream is down, it answers with
silent fallback audio; the route treats that as unavailable and the page uses
browser speech, because silent audio is not teaching.

In the teaching page, choose **Speech: Breeze (GPU)** from the backend
selector to use it. Auto only reaches Breeze after Kokoro and Gemini have both
failed. Expect the GPU broker to pause or contend with the llama router while
it synthesizes; that trade-off is deliberate and never automatic.

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
