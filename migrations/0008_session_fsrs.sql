-- breadstudy schema v6: hint_state is the single source of truth for hint
-- progress (STUDY-21).
--
-- session_runs.hints_used duplicated hint_state.level while the session
-- engine kept its own counter. The session now reads the ladder cursor from
-- hint_state (and mirrors it into the rendered view only), so the duplicate
-- per-run counter is dropped instead of drifting.
ALTER TABLE session_runs DROP COLUMN hints_used;
