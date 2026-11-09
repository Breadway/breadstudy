# Deploying breadstudy on Hestia

```sh
rsync -a --delete --exclude target --exclude .git ./ hestia:~/breadstudy/
ssh hestia 'cd ~/breadstudy && docker compose -f packaging/compose.yaml --project-directory . up -d --build'
curl -s http://127.0.0.1:7290/healthz
```

Run compose as the `breadway` user, not root.

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

## Edge

Hestia Caddy (LAN) and Aegis (public) reverse-proxy `study.breadway.dev` to host port 7220; blocks live in the infra repo. Set `BREADSTUDY_BIND=0.0.0.0:7290` in `.env` so the Tailscale path can reach it. BIPS id `breadstudy-web-prod`.
