# Hermes friction client (STUDY-3)

Soft friction only. Nothing is blocked and the escape hatch is always
available. No hermes-only MCP servers (desktop-control, klipper): only
standard workspace tools, window hide, DND, timer, and focus events.

## Run on hermes

```sh
python3 clients/hermes/focus_watch.py --minutes 20
```

Start switches to the `study` workspace, opens the study PWA, hides (never
closes) distracting windows, sets DND on, and shows the timer in the
breadbar OSD. End restores the previous workspace and DND and clears the
OSD. Off-task focus outside the allowlist (Obsidian, PDFs, Desmos, study
site) pauses the timer with a nudge after 60s. Typing `bail` (or writing the
word to `state/bail-requested`) aborts cleanly: it logs a bailed event,
restores everything, and leaves a reschedule marker for the planner.

Options: `--workspace`, `--study-url`, `--state-dir`, `--dry-run`.

## Checks

```sh
python3 clients/hermes/tests/check_friction.py
```

Dry-run by default in tests so hestia CI stays green without a compositor.
