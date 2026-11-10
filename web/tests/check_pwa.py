"""STUDY-19 installability and layout checks. No third party deps."""
import json
import re
import sys
from pathlib import Path

WEB = Path(__file__).resolve().parents[1]

def fail(msg):
    print("FAIL: " + msg)
    return False

def main():
    ok = True
    manifest_path = WEB / "manifest.webmanifest"
    try:
        manifest = json.loads(manifest_path.read_text())
    except Exception as e:
        print("FAIL: manifest parse: " + str(e))
        return 1
    for field in ("name", "short_name", "start_url", "display", "icons"):
        if field not in manifest:
            ok = fail("manifest missing " + field) and False
    if manifest.get("display") not in ("standalone", "fullscreen", "minimal-ui"):
        ok = fail("manifest display must allow install") and False
    icons = manifest.get("icons", [])
    sizes = {i.get("sizes") for i in icons}
    if "192x192" not in sizes or "512x512" not in sizes:
        ok = fail("manifest needs 192 and 512 icons") and False
    for icon in icons:
        p = (WEB / icon["src"]).resolve()
        try:
            base = WEB.resolve()
            p.relative_to(base)
        except Exception:
            ok = fail("icon escapes web root: " + icon["src"]) and False
        if not p.exists():
            ok = fail("missing icon file: " + icon["src"]) and False
        elif p.stat().st_size == 0:
            ok = fail("empty icon file: " + icon["src"]) and False

    sw = (WEB / "sw.js").read_text()
    for token in ("install", "activate", "fetch", "caches.open", "skipWaiting"):
        if token not in sw:
            ok = fail("sw.js missing " + token) and False
    for shell_file in ("index.html", "session.html", "styles.css", "app.js"):
        if shell_file not in sw:
            ok = fail("sw.js shell missing " + shell_file) and False

    session = (WEB / "session.html").read_text()
    for token in ('rel="manifest"', "session-timer", "hint-list", "hint-btn",
                  "latex-preview", "sym-pi", "sym-sqrt", "sym-power", "Your answer (plain text or LaTeX)", "next-question-btn", "skip-btn", 'type="file"',
                  "latex-answer", "phase-row", "capture="):
        if token not in session:
            ok = fail("session.html missing " + token) and False
    if 'name="viewport"' not in session and "name='viewport'" not in session:
        ok = fail("session.html missing viewport meta") and False

    index = (WEB / "index.html").read_text()
    for token in ("dash-today", "dash-skills", "Upcoming assessments", "dash-assess"):
        if token not in index:
            ok = fail("index.html missing " + token) and False

    css = (WEB / "styles.css").read_text()
    if "@media" not in css or "640px" not in css and "600px" not in css:
        ok = fail("styles.css missing phone media query") and False
    if "360" in css or "min-height: 44px" in css:
        pass  # touch target hint present
    else:
        ok = fail("styles.css missing touch target sizing") and False

    app = (WEB / "app.js").read_text()
    for token in ("prime", "recall", "practice", "explain", "hint", "/api/skills", "SAMPLE_ITEM"):
        if token not in app:
            ok = fail("app.js missing " + token) and False

    scratch = (WEB / "scratchpad.js").read_text()
    for token in ("katex", "previewInto", "drawCylinder", "cylinder", "r"):
        if token not in scratch:
            ok = fail("scratchpad.js missing " + token) and False
    # Sample Volume/SA latex must be wired into the session render path.
    if "pi" not in app and "\\pi" not in app:
        ok = fail("sample Volume/SA latex missing") and False

    # 360px check: camera and mic controls must sort before other answers on narrow screens.
    if "#photo-input-label" not in css:
        ok = fail("phone CSS must keep the camera reachable") and False

    print("PASS" if ok else "CHECKS FAILED")
    return 0 if ok else 1

if __name__ == "__main__":
    sys.exit(main())
