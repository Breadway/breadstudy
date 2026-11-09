"""Focus watcher: polls the active window and feeds Session.tick.

Escape: type bail on stdin, or write it to <state-dir>/bail-requested.
Soft friction only: unknown focus never blocks, the timer just keeps going.
"""
import argparse
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from friction import FrictionConfig, Runner, Session


def active_title() -> str:
    try:
        if shutil.which("swaymsg"):
            proc = subprocess.run(["swaymsg", "-t", "get_tree"], capture_output=True,
                                  text=True, timeout=5)
            out = proc.stdout
            marker = '"focused": true'
            idx = out.rfind(marker)
            if idx >= 0:
                window = out[max(0, idx - 2000):idx + 200]
                for key in ('"app_id":', '"name":'):
                    k = window.rfind(key)
                    if k >= 0:
                        frag = window[k + len(key):k + len(key) + 120]
                        return frag.strip(' ":,')
            return "sway:unknown"
        if shutil.which("hyprctl"):
            proc = subprocess.run(["hyprctl", "activewindow"], capture_output=True,
                                  text=True, timeout=5)
            return proc.stdout.strip().splitlines()[0][:160] if proc.stdout.strip() else "hypr:unknown"
        if shutil.which("xdotool"):
            proc = subprocess.run(["xdotool", "getactivewindow", "getwindowname"],
                                  capture_output=True, text=True, timeout=5)
            return proc.stdout.strip()[:160] or "x11:unknown"
    except Exception:
        pass
    return "unknown"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--minutes", type=int, default=20)
    parser.add_argument("--workspace", default="study")
    parser.add_argument("--study-url", default="https://study.breadway.dev/session.html")
    parser.add_argument("--state-dir", default=str(Path.home() / ".local/share/breadstudy"))
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    cfg = FrictionConfig(workspace=args.workspace, study_url=args.study_url,
                         state_dir=Path(args.state_dir))
    session = Session(cfg=cfg, runner=Runner(dry_run=args.dry_run))
    session.start()
    print("study session started. Type bail to stop early.")
    bail_file = cfg.state_dir / "bail-requested"
    deadline = time.monotonic() + args.minutes * 60
    try:
        import select
        haveselect = True
    except Exception:
        haveselect = False
    while time.monotonic() < deadline:
        title = active_title()
        state = session.tick(title)
        print("focus=" + title[:80] + " state=" + state)
        if bail_file.exists():
            session.bail(bail_file.read_text().strip())
            print("bailed via sentinel file.")
            return 0
        if haveselect and select.select([sys.stdin], [], [], 5)[0]:
            line = sys.stdin.readline().strip().lower()
            if line == "bail":
                session.bail(line)
                print("bailed via typed escape.")
                return 0
        else:
            time.sleep(5)
    session.end()
    print("session done.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
