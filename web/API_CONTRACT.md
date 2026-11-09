# STUDY-19 API contract notes

The PWA builds against the stub already on main plus the shapes in
`spec/session.md` and `spec/skills.md`. It does not edit `server/`.

## What main provides now

- `GET /healthz` returns `ok`.
- `GET /api/skills` returns the string `[]` with a text body (axum `&str`
  handler). The PWA fetches it as text and parses JSON, so `[]` still works,
  but the content type is not JSON.

## What the PWA expects (for server-core reconciliation)

- `GET /api/skills` returns JSON array:
  `[{ id, strand, name, vault_note }]`
  with `content-type: application/json`. Example ids come from
  `spec/skills.md`: `volume-prism-cylinder`, `volume-cone-pyramid-sphere`,
  `surface-area`, `unit-conversion-scaling`, `measurement-error`,
  `pythagoras-3d`.
- Future dashboard endpoints (not required for STUDY-19, PWA degrades to
  local sample data when they 404):
  - `GET /api/dashboard` with today, streak, weak spots, upcoming
    assessments, history.
  - `GET /api/session/next` with cached item, hints, and audio URLs.
  - `POST /api/attempts` with method marks, final mark, and error tags.
- Session phases follow `spec/session.md`:
  `prime -> recall -> practice -> explain -> reflect -> break -> end`.
  The client owns the countdown display; the server remains the authority
  for phase transitions and timer expiry once the session engine lands.

## Mismatches to reconcile (server-core owns `server/`)

1. `/api/skills` should return `Json<Vec<Skill>>` instead of `&str`.
2. Dashboard, session, and attempt endpoints do not exist yet. PWA uses
   `localStorage` samples and must be re-pointed once they land.
3. No static file serving for `web/` yet. Deploy must serve `web/` at the
   PWA root or the service worker scope breaks.
