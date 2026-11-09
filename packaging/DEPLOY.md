# Deploying breadstudy on Hestia

```sh
rsync -a --delete --exclude target --exclude .git ./ hestia:~/breadstudy/
ssh hestia 'cd ~/breadstudy && docker compose -f packaging/compose.yaml --project-directory . up -d --build'
curl -s http://127.0.0.1:7290/healthz
```

Run compose as the `breadway` user, not root.

## breeze-server TTS broker (STUDY-10)

Local TTS broker on loopback port 7310 (BIPS id `breadstudy-tts-prod`).
It fronts the upstream Breeze TTS server on 7311 with a file cache keyed on
(text, instruction, cfg_scale) and a null-TTS fallback: when the GPU service
is down it answers silent 24 kHz PCM plus subtitles so sessions continue
text-only.

```sh
cargo build --release
cp packaging/breeze-server.service ~/.config/systemd/user/
systemctl --daemon-reload --user
systemctl --enable --now --user breeze-server
curl -s http://127.0.0.1:7310/healthz
curl -s -X POST http://127.0.0.1:7310/v1/speak \
  -H 'Content-Type: application/json' \
  -d '{"text":"Area equals pi r squared."}'
```

Env overrides: `BREEZE_LISTEN` (default `127.0.0.1:7310`),
`BREEZE_CACHE_DIR` (default `data/breeze-cache`),
`BREEZE_UPSTREAM_URL` (default `http://127.0.0.1:7311`).

Port registry note: propose this PORTS.md row in the infra repo
(registration pending, applied from this repo design first):

`| 7310 | 127.0.0.1 | breadstudy-tts-prod | ai-inference | breeze-server TTS broker, loopback only |`

Known conflict 2026-11-09: loopback ports 7310 and 7311 are currently held by
long-running embedding services (llama-server MiniLM on 7310, embed-shim on
7311, neither row registered in PORTS.md). Do not enable the unit on 7310
until the operator resolves the collision: either move the embedding stack or
assign new TTS ports and update this file plus the unit.

## Edge

Hestia Caddy (LAN) and Aegis (public) reverse-proxy `study.breadway.dev` to host port 7220; blocks live in the infra repo. Set `BREADSTUDY_BIND=0.0.0.0:7290` in `.env` so the Tailscale path can reach it. BIPS id `breadstudy-web-prod`.
