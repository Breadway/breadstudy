# Session rules

Server-owned state machine. `prime -> recall -> practice -> explain -> reflect -> break -> end`.

## Templates

- **School day, 20 min:** recall 120s, practice 600s, explain 300s, reflect 180s. Prime and break are zero-duration and hidden.
- **Weekend, 40 min:** prime 180s, recall 180s, practice 720s, break 240s (18:00-22:00), explain 900s in a different strand, reflect 180s.

## Content

- Items, hints and audio paths are resolved at session start and served from the cached run. No model call happens mid-session except the teaching board.
- The bank is filtered to `verified` items, ordered by FSRS due score, then not-served-in-7-days, then least recently served.
- The session view reports `cached_items_total`, `cached_items_remaining` and per-phase counts.

## Controls

- **Next question** pulls the next cached item in the phase.
- **Advance** moves to the next phase and is refused while the phase still has cached items.
- **Skip** requires a reason, consumes the current item, stays in phase, and counts against weekly skips and grace tokens.
- **Hint** unlocks the next rung of the item's ladder. Progress is keyed by `(session_id, item_id)` and starts at zero.
- **Off-task** at 60 seconds pauses the timer and returns a nudge; resume continues from the remaining time.

## Grading

- `POST /api/attempts` grades one item and records the attempt; with `session_id` it also writes FSRS state.
- `POST /api/session/{id}/grade-parts` grades a multi-part item and totals the marks.
- Method marks require working text; a bare final answer earns only the final mark.
- A deterministic numeric check overrides the model when both answers are comparable.

## Teaching

- `POST /api/teach` takes a question, optional `item_id` and a short transcript, and returns ordered blocks: prose, LaTeX, steps and one sanitized SVG.
- The client reveals and speaks blocks in order and can interrupt at any time.
