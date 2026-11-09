# Nightly generation pipeline (STUDY-11)

Unattended 02:00 run: picks due skills, generates 2-3 items per skill,
verifies with SymPy, dedupes, prefetches SearXNG sources, pre-renders TTS.

## Run

```sh
python3 pipelines/nightly/run_0200.py
```

Reads `state/due-skills.json` (defaults to the Test 6 Volume/SA set),
appends to `state/item-bank.json`, writes `state/nightly-<date>.json`,
audio under `state/audio/`.

Schedule with a user timer for 02:00. The generator is deterministic per
(date, skill), so a re-run adds nothing (dedupe by prompt hash).

## Sources

One SearXNG result per skill (127.0.0.1:7102) with URL, snippet, and fetch
date stored on every item. Verified live against SearXNG on 2026-11-09.

## TTS

`TTSClient` renders prompt plus hint audio per item. Backend order:
injected synth (tests), breeze-server when `BREEZE_URL` is set, otherwise
subtitle markers so sessions continue text-only. Breeze-server has no
`/render` endpoint yet (voice track owns it), so live runs currently record
`pending` with subtitle files until that API lands. Tests use a fake synth
and assert every prompt and hint gets a rendered file.

## Checks

```sh
python3 pipelines/nightly/tests/check_pipeline.py
```
