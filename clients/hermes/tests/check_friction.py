"""STUDY-3 checks: start/end cycle, 60s off-task pause, escape hatch."""
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from friction import (FrictionConfig, Runner, Session, is_allowed)


def make_session(tmp: str) -> tuple[Session, Runner, FrictionConfig]:
    state = Path(tmp) / "state"
    osd = Path(tmp) / "osd.txt"
    cfg = FrictionConfig(workspace="study", state_dir=state, osd_path=osd)
    runner = Runner(dry_run=True)
    return Session(cfg=cfg, runner=runner), runner, cfg


def main() -> int:
    failures = []
    with tempfile.TemporaryDirectory() as td:
        # 1. Start/end cycle on hermes semantics.
        session, runner, cfg = make_session(td)
        started = session.start()
        if started.get("kind") != "session-start":
            failures.append("start did not log session-start")
        cmds = [" ".join(c) for c in runner.log]
        if not any("workspace" in c and "study" in c for c in cmds):
            failures.append("start did not switch to the study workspace")
        if not any("xdg-open" in c for c in cmds):
            failures.append("start did not open the study window")
        if not any("dnd on" in c for c in cmds):
            failures.append("start did not set DND on")
        if not cfg.resolved_osd().exists():
            failures.append("timer did not show in breadbar OSD on start")
        events = (cfg.state_dir / "friction-events.jsonl").read_text()
        if "session-start" not in events:
            failures.append("session-start missing from the event log")

        ended = session.end()
        if ended.get("outcome") != "done":
            failures.append("end outcome wrong")
        cmds = [" ".join(c) for c in runner.log]
        if not any("dnd off" in c for c in cmds):
            failures.append("end did not restore DND")
        if not any("workspace" in c and " 1" in c for c in cmds):
            failures.append("end did not restore the previous workspace")
        if cfg.resolved_osd().exists():
            failures.append("end did not clear the OSD timer")
        events = (cfg.state_dir / "friction-events.jsonl").read_text()
        if "session-end" not in events:
            failures.append("session-end missing from the event log")
        # Hide path never closes: no kill or close commands issued.
        if any("kill" in c or "close" in c for c in cmds):
            failures.append("client must hide, never close windows")

        # 2. Allowlist keeps focus; off-task for 60s pauses with a nudge.
        session2, runner2, cfg2 = make_session(td + "/w2")
        session2.start()
        for title in ("Obsidian - Area notes", "document.pdf - Zathura",
                      "Desmos Graphing", "study.breadway.dev session"):
            if session2.tick(title, now=1000.0) != "focused" or not is_allowed(title, cfg2):
                failures.append("allowlist rejected: " + title)
        if session2.tick("Chat - memes", now=2000.0) != "off-task":
            failures.append("off-task focus not detected")
        if session2.tick("Chat - memes", now=2059.0) != "off-task":
            failures.append("paused before the 60s mark")
        if session2.tick("Chat - memes", now=2061.0) != "paused":
            failures.append("no 60s pause on off-task focus")
        if not session2.paused or not session2.nudged:
            failures.append("pause state or nudge flag missing")
        if not any("notify-send" in " ".join(c) for c in runner2.log):
            failures.append("pause did not send a nudge")
        log2 = (cfg2.state_dir / "friction-events.jsonl").read_text()
        if "pause-nudge" not in log2:
            failures.append("pause-nudge missing from the event log")
        if "paused" not in cfg2.resolved_osd().read_text():
            failures.append("OSD did not show the paused timer")
        if session2.tick("Obsidian - back to work", now=2070.0) != "focused":
            failures.append("return to allowlist did not resume")

        # 3. Escape hatch aborts cleanly with a logged bailed event.
        session3, runner3, cfg3 = make_session(td + "/w3")
        session3.start()
        bailed = session3.bail("tired, stop for today")
        if bailed.get("kind") != "bailed" or not bailed.get("rescheduled"):
            failures.append("bail did not log a rescheduled bailed event")
        if "tired" not in bailed.get("reason", ""):
            failures.append("bail reason missing from the event")
        if not (cfg3.state_dir / "reschedule-requested").exists():
            failures.append("bail did not leave a reschedule marker")
        log3 = (cfg3.state_dir / "friction-events.jsonl").read_text()
        if "bailed" not in log3 or "session-end" not in log3:
            failures.append("bail left no bailed plus session-end trail")
        if cfg3.resolved_osd().exists():
            failures.append("bail did not clear the OSD timer")
        cmds3 = [" ".join(c) for c in runner3.log]
        if not any("dnd off" in c for c in cmds3):
            failures.append("bail did not restore DND")

    if failures:
        for f in failures:
            print("FAIL: " + f)
        return 1
    print("PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
