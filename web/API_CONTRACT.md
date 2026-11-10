# breadstudy API

Base URL: the deployed origin. All bodies are JSON unless stated.

## Health and catalog

- `GET /healthz` -> `ok`
- `GET /api/skills` -> `[{ id, strand, name, vault_note }]`
- `GET /api/skills/{id}` -> skill detail: `name`, `strand`, `vault_note`, `vault_note_path`, `summary`, `item_count`, `note_excerpts`, `sample_items` (answers and hints omitted).

## Planning

- `GET /api/plan` -> plan rows (read-only; no writes).
- `POST /api/plan` -> creates and logs a plan.
- `GET /api/plan/due` -> `[{ skill_id, name, mastery, due_at, due_score, reviews, lapses }]`.
- `GET /api/weekly/snapshot` -> weekly aggregate.
- `GET /api/progress?limit=N` -> `recent_attempts`, `skills` (mastery, accuracy, due), `error_patterns`, `attempts_total`. Limit capped at 200.

## Sessions

- `POST /api/session` `{ template?: "school_day_20" | "weekend_40", skill_id?, grace_tokens? }` -> `SessionView` (201).
- `GET /api/session/{id}` -> `SessionView`, or 404.
- `POST /api/session/{id}/tick`
- `POST /api/session/{id}/advance`
- `POST /api/session/{id}/next-item`
- `POST /api/session/{id}/hint` -> `{ hint, hints_used, session }`
- `POST /api/session/{id}/skip` `{ reason }`
- `POST /api/session/{id}/off-task` `{ seconds }`
- `POST /api/session/{id}/resume`
- `POST /api/session/{id}/grade-parts` -> per-part marks and total.
- `GET /api/skips/weekly` -> `{ weekly_skip_count }`

`SessionView` carries `phase`, `phase_index`, `phase_remaining_seconds`, `total_remaining_seconds`, `item` (id, skill, strand, prompt, hints, `hint_unlocked`, `hint_count`), `plan`, `cached_items_total`, `cached_items_remaining`, `hints_used`, `skip_count`, `grace_tokens`.

## Attempts and grading

- `POST /api/attempts`
  `{ item_id, input_kind: "latex" | "photo" | "transcript", latex?, transcript?, photo_base64?, photo_media_type?, session_id?, confirmed?, confirmed_text? }`
  -> `{ status: "graded", attempt }` or `{ status: "needs_confirmation", transcription, confidence }`.
- Request bodies are capped at 8 MB; larger returns 413 with a JSON message.

## Teaching

- `POST /api/teach` `{ question, item_id?, history?: [{ role, content }], skill_id?, transcript? }` ->
  `{ blocks: [{ kind: "text" | "latex" | "steps" | "svg" | "animation", ... }], skill? }`.
  SVG is sanitized server-side; blocks are length-capped.
- An animation block is `{ kind: "animation", frames: [{ svg, caption?, duration_ms? }] }`,
  capped at 8 frames with each SVG held to the same sanitizer and size cap as a
  static diagram. Durations clamp to 200-3000 ms; the client defaults to 900 ms.
  If any frame fails sanitization the block is downgraded to a single static
  `svg` block built from the first usable frame.

## Notes for clients

- Static files are served from the web directory. Extensionless unknown paths fall back to the app shell; missing assets return 404.
- Errors are JSON: `{ "error": "..." }` with a 4xx or 5xx status.
