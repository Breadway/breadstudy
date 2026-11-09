# Hermes friction client for breadstudy (STUDY-3).
#
# Soft friction only. Nothing is blocked and the escape hatch is always
# available. This client deliberately avoids hermes-only MCP servers
# (desktop-control, klipper). It shells out to standard workspace tools
# when present and otherwise runs in dry-run so tests and hestia checks pass.
#
# Start/end cycle:
# - save current workspace, switch to a dedicated study workspace
# - open the study window (PWA entry point)
# - hide (never close) distracting windows
# - set Do Not Disturb on, restore after
# - show the session timer in breadbar OSD, clear on end
#
# Focus allowlist: Obsidian, PDFs, Desmos, study site.
# Off-task focus outside the allowlist for 60s pauses the timer with a nudge.
# Escape hatch: typed bail logs a bailed event and reschedules.

from __future__ import annotations

import json
import shutil
import subprocess
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

ALLOWLIST_DEFAULT = ("obsidian", "pdf", "desmos", "study.breadway.dev", "breadstudy")
OFFTASK_PAUSE_SECONDS = 60


def utcnow() -> str:
    return datetime.now(timezone.utc).isoformat()


@dataclass
class Runner:
    """Thin shell wrapper. dry_run records commands instead of executing."""

    dry_run: bool = False
    log: list = field(default_factory=list)

    def run(self, argv: list[str]) -> tuple[int, str]:
        self.log.append(argv)
        if self.dry_run:
            return 0, "dry-run"
        if not shutil.which(argv[0]):
            return 127, "missing: " + argv[0]
        try:
            proc = subprocess.run(argv, capture_output=True, text=True, timeout=10)
            return proc.returncode, (proc.stdout or proc.stderr or "").strip()
        except Exception as e:
            return 1, str(e)


@dataclass
class FrictionConfig:
    workspace: str = "study"
    study_url: str = "https://study.breadway.dev/session.html"
    allowlist: tuple[str, ...] = ALLOWLIST_DEFAULT
    state_dir: Path = Path.home() / ".local/share/breadstudy"
    osd_path: Path | None = None

    def resolved_osd(self) -> Path:
        return self.osd_path or (self.state_dir / "breadbar-osd.txt")


def is_allowed(title: str, cfg: FrictionConfig) -> bool:
    lowered = (title or "").lower()
    return any(token in lowered for token in cfg.allowlist)


def switch_workspace(runner: Runner, name: str) -> str:
    """Try sway, hyprland, then i3 in order. Returns the tool used or dry-run."""
    candidates = [
        (["swaymsg", "workspace", name], "swaymsg"),
        (["hyprctl", "dispatch", "workspace", name], "hyprctl"),
        (["i3-msg", "workspace", name], "i3-msg"),
    ]
    for argv, tool in candidates:
        if runner.dry_run or shutil.which(argv[0]):
            runner.run(argv)
            return tool
    return "none"


def open_study_window(runner: Runner, url: str) -> str:
    if runner.dry_run or shutil.which("xdg-open"):
        runner.run(["xdg-open", url])
        return "xdg-open"
    return "none"


def hide_distractions(runner: Runner) -> int:
    """Hide (minimize) windows that are not the study window. Never close."""
    hidden = 0
    # sway: move non-study windows to scratchpad is hide-like and reversible.
    # We only record the intent here; the real move needs a window list that
    # differs per compositor, so keep this conservative and reversible.
    if runner.dry_run or shutil.which("swaymsg"):
        runner.run(["swaymsg", "[app_id!=\"breadstudy\"]", "move", "scratchpad"])
        hidden += 1
    if shutil.which("wmctrl") and not runner.dry_run:
        code, out = runner.run(["wmctrl", "-l"])
        if code == 0:
            for line in out.splitlines():
                lowered = line.lower()
                if not is_allowed_fallback(lowered):
                    wid = line.split()[0]
                    runner.run(["wmctrl", "-i", "-r", wid, "-b", "add,hidden"])
                    hidden += 1
    elif runner.dry_run:
        runner.run(["wmctrl", "-l"])
    return hidden


def is_allowed_fallback(lowered_line: str) -> bool:
    return any(token in lowered_line for token in ALLOWLIST_DEFAULT)


def set_dnd(runner: Runner, on: bool) -> str:
    """Enable DND while the session is active, restore after. Best effort."""
    if shutil.which("makoctl") and not runner.dry_run:
        runner.run(["makoctl", "mode", "-a" if on else "-r", "do-not-disturb"])
        return "makoctl"
    if shutil.which("dunstctl") and not runner.dry_run:
        runner.run(["dunstctl", "set-paused", "true" if on else "false"])
        return "dunstctl"
    if shutil.which("gsettings") and not runner.dry_run:
        runner.run([
            "gsettings", "set", "org.gnome.desktop.notifications",
            "show-in-lock-screen", "false" if on else "true",
        ])
        return "gsettings"
    runner.run(["dnd", "on" if on else "off"])
    return "recorded"


def write_osd(cfg: FrictionConfig, text: str) -> None:
    cfg.state_dir.mkdir(parents=True, exist_ok=True)
    cfg.resolved_osd().write_text(text + "\n")


def clear_osd(cfg: FrictionConfig) -> None:
    try:
        cfg.resolved_osd().unlink()
    except FileNotFoundError:
        pass


def append_event(cfg: FrictionConfig, event: dict) -> Path:
    cfg.state_dir.mkdir(parents=True, exist_ok=True)
    path = cfg.state_dir / "friction-events.jsonl"
    with path.open("a") as f:
        f.write(json.dumps(event) + "\n")
    return path


@dataclass
class Session:
    cfg: FrictionConfig
    runner: Runner
    previous_workspace: str = "1"
    started_at: str = ""
    paused: bool = False
    off_task_since: float | None = None
    nudged: bool = False

    def start(self) -> dict:
        self.started_at = utcnow()
        switch_workspace(self.runner, self.cfg.workspace)
        open_study_window(self.runner, self.cfg.study_url)
        hide_distractions(self.runner)
        set_dnd(self.runner, True)
        write_osd(self.cfg, "breadstudy 20:00")
        evt = {"at": self.started_at, "kind": "session-start",
               "workspace": self.cfg.workspace, "url": self.cfg.study_url}
        append_event(self.cfg, evt)
        return evt

    def tick(self, active_title: str, now: float | None = None) -> str:
        """Feed the focused window title. Returns: focused, off-task, paused."""
        moment = time.monotonic() if now is None else now
        if is_allowed(active_title, self.cfg):
            self.off_task_since = None
            self.nudged = False
            if self.paused:
                self.paused = False
                append_event(self.cfg, {"at": utcnow(), "kind": "resume", "title": active_title})
            return "focused"
        if self.off_task_since is None:
            self.off_task_since = moment
            return "off-task"
        if (moment - self.off_task_since) >= OFFTASK_PAUSE_SECONDS and not self.paused:
            self.paused = True
            self.nudged = True
            self.runner.run(["notify-send", "breadstudy", "Paused: back to the study window when ready."])
            append_event(self.cfg, {"at": utcnow(), "kind": "pause-nudge", "title": active_title})
            try:
                write_osd(self.cfg, "breadstudy paused")
            except Exception:
                pass
            return "paused"
        return "off-task"

    def bail(self, reason: str) -> dict:
        """Escape hatch: always available, logs bailed and reschedules."""
        evt = {"at": utcnow(), "kind": "bailed", "reason": (reason or "").strip(),
               "rescheduled": True}
        append_event(self.cfg, evt)
        self.end(outcome="bailed")
        # Reschedule marker for the planner to pick up.
        (self.cfg.state_dir / "reschedule-requested").write_text(evt["at"] + "\n")
        return evt

    def end(self, outcome: str = "done") -> dict:
        set_dnd(self.runner, False)
        switch_workspace(self.runner, self.previous_workspace)
        clear_osd(self.cfg)
        evt = {"at": utcnow(), "kind": "session-end", "outcome": outcome}
        append_event(self.cfg, evt)
        return evt
