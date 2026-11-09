# Deploying breadstudy on Hestia

```sh
rsync -a --delete --exclude target --exclude .git ./ hestia:~/breadstudy/
ssh hestia 'cd ~/breadstudy && BREADSTUDY_BIND=127.0.0.1:7220 docker compose -f packaging/compose.yaml --project-directory . up -d --build'
curl -s http://127.0.0.1:7220/healthz
```

Run compose as the `breadway` user, not root.

## Edge

Hestia Caddy (LAN) and Aegis (public) reverse-proxy `study.breadway.dev` to host port 7220; blocks live in the infra repo. Set `BREADSTUDY_BIND=0.0.0.0:7220` in `.env` so the Tailscale path can reach it. BIPS id `breadstudy-web-prod`.
