"""STUDY-11 live TTS check: one real render against breeze-server.

Runs only when a broker answers at $BREEZE_URL (default 127.0.0.1:7312);
skips otherwise so CI without the voice stack stays green.
"""
import os
import sys
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from pipeline import BREEZE_URL_DEFAULT, TTSClient


def broker_up(url: str) -> bool:
    try:
        with urllib.request.urlopen(url.rstrip("/") + "/healthz", timeout=5) as res:
            return res.read().decode().strip() == "ok"
    except Exception:
        return False


def main() -> int:
    import tempfile
    url = os.environ.get("BREEZE_URL", BREEZE_URL_DEFAULT)
    if not broker_up(url):
        print("SKIP: no breeze-server at " + url)
        return 0
    with tempfile.TemporaryDirectory() as td:
        out = Path(td)
        tts = TTSClient(out_dir=out)
        dest = out / "live.wav"
        status = tts.render("Area equals pi r squared.", dest)
        if status != "rendered" or tts.backend != "breeze-server":
            print("FAIL: status=%s backend=%s" % (status, tts.backend))
            return 1
        raw = dest.read_bytes()
        if len(raw) < 44 or raw[0:4] != b"RIFF":
            print("FAIL: not a WAV document")
            return 1
        import wave
        with wave.open(str(dest)) as w:
            if w.getframerate() != 24000 or w.getnchannels() != 1:
                print("FAIL: expected 24 kHz mono")
                return 1
            dur = w.getnframes() / w.getframerate()
        print("PASS: live rendered %.2fs via %s (fallback=%s)" % (dur, tts.backend, tts.fallback))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
