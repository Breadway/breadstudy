# Weekly review, vault write-back, countdown (STUDY-15)

Review generates from mastery, error, and session exports. Nothing is
invented: pass the JSON exports in, get markdown out.

## Run

```sh
python3 pipelines/review/run_weekly.py --mastery state/mastery.json \
  --attempts state/attempts.json --events state/events.jsonl \
  --week 2026-W46 --out state/review-2026-W46.md
```

## Vault write-back

Explanations stage in `state/writeback-queue.json` and apply only on
approval:

- Existing note with markers: only the marked block is replaced.
- Existing note without markers: the block is appended, no line modified.
- Missing note: a new file lands under `_agent/`, nothing else touched.

Check the queue, then approve one entry at a time. The test proves existing
bytes survive byte-for-byte outside the markers.

## Countdown

Tests 1-7 from the Methods Pathway Overview with Test 6
(2026-11-10, Volume, Capacity and Surface Area) first. Other test dates are
not published in the overview, so they list TBA in urgency order until the
planner sets them.

## Checks

```sh
python3 pipelines/review/tests/check_review.py
```
