#!/usr/bin/env python3
"""Kokoro TTS sidecar for the breadstudy teaching mode.

It is loopback-only by default, which is what the host systemd unit uses. The
compose image sets KOKORO_ALLOW_NETWORK=1 and listens on 0.0.0.0:7314 only on
the private compose network; no host port is published.

The public endpoint is intentionally small and OpenAI-compatible:

    POST /v1/audio/speech
    {"model":"kokoro","input":"...","voice":"bm_george","speed":1.0}

It returns audio/wav. The model is loaded lazily on the first request by
default and kept warm. Set KOKORO_PRELOAD=1 to load it in the background at
startup instead; the compose service does this. Model weights are not in the
repository; point KOKORO_MODEL and KOKORO_VOICES at the downloaded files.

Only Python's standard library is used for HTTP and WAV encoding. The runtime
dependencies are kokoro-onnx and numpy.
"""

from __future__ import annotations

import io
import json
import logging
import os
import sys
import threading
import time
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 7314
DEFAULT_MODEL = "/var/lib/breadstudy/tts/kokoro-v1.0.onnx"
DEFAULT_VOICES = "/var/lib/breadstudy/tts/voices-v1.0.bin"
MAX_INPUT_CHARS = 1200
MAX_VOICE_CHARS = 64
MIN_SPEED = 0.5
MAX_SPEED = 2.0

LOG = logging.getLogger("breadstudy-kokoro")


def _env(name: str, default: str) -> str:
    value = os.environ.get(name, "").strip()
    return value or default


def _port() -> int:
    raw = os.environ.get("KOKORO_PORT", "").strip()
    if not raw:
        return DEFAULT_PORT
    try:
        port = int(raw)
    except ValueError as error:
        raise SystemExit(f"KOKORO_PORT must be an integer, got {raw!r}") from error
    if not 1 <= port <= 65535:
        raise SystemExit(f"KOKORO_PORT out of range: {port}")
    return port


class KokoroEngine:
    """Lazy, single-model wrapper with one synthesis lock.

    kokoro-onnx is not documented as thread safe, so requests are serialized.
    That is fine for a single-student teaching mode and keeps memory flat.
    """

    def __init__(self, model_path: str, voices_path: str) -> None:
        self.model_path = model_path
        self.voices_path = voices_path
        self._kokoro: Any | None = None
        self._load_lock = threading.Lock()
        self._synth_lock = threading.Lock()
        self._sample_rate = 24000
        self.load_seconds: float | None = None

    def _ensure_loaded(self) -> Any:
        if self._kokoro is not None:
            return self._kokoro
        with self._load_lock:
            if self._kokoro is not None:
                return self._kokoro
            for path in (self.model_path, self.voices_path):
                if not os.path.exists(path):
                    raise RuntimeError(
                        f"Kokoro model file is missing: {path}. "
                        "Download it with the commands in packaging/DEPLOY.md."
                    )
            started = time.perf_counter()
            try:
                from kokoro_onnx import Kokoro
            except ImportError as error:
                raise RuntimeError(
                    "kokoro-onnx is not installed. Run "
                    "pip install -r packaging/tts/requirements.txt"
                ) from error
            LOG.info("loading Kokoro model from %s", self.model_path)
            self._kokoro = Kokoro(self.model_path, self.voices_path)
            self.load_seconds = time.perf_counter() - started
            LOG.info("Kokoro model loaded in %.2fs", self.load_seconds)
            return self._kokoro

    def synthesize(self, text: str, voice: str, speed: float) -> tuple[bytes, int]:
        kokoro = self._ensure_loaded()
        with self._synth_lock:
            samples, sample_rate = kokoro.create(
                text, voice=voice, speed=speed, lang="en-us"
            )
            self._sample_rate = int(sample_rate)
            wav = _pcm_to_wav(samples, self._sample_rate)
        return wav, self._sample_rate


def _pcm_to_wav(samples: Any, sample_rate: int) -> bytes:
    """Convert kokoro-onnx float samples to a 16-bit mono WAV."""
    import numpy as np

    array = np.asarray(samples, dtype=np.float32)
    clipped = np.clip(array, -1.0, 1.0)
    pcm = (clipped * 32767.0).astype(np.int16)
    buffer = io.BytesIO()
    with wave.open(buffer, "wb") as out:
        out.setnchannels(1)
        out.setsampwidth(2)
        out.setframerate(sample_rate)
        out.writeframes(pcm.tobytes())
    return buffer.getvalue()


def _validate(payload: dict[str, Any]) -> tuple[str, str, float]:
    text = payload.get("input")
    if not isinstance(text, str) or not text.strip():
        raise ValueError("input is required")
    text = text.strip()
    if len(text) > MAX_INPUT_CHARS:
        raise ValueError(f"input is longer than {MAX_INPUT_CHARS} characters")
    voice = payload.get("voice") or "bm_george"
    if not isinstance(voice, str) or not voice.strip():
        raise ValueError("voice must be a non-empty string")
    voice = voice.strip()
    if len(voice) > MAX_VOICE_CHARS:
        raise ValueError("voice name is too long")
    if not all(ch.isalnum() or ch in "_-" for ch in voice):
        raise ValueError("voice name may only contain letters, digits, _ and -")
    raw_speed = payload.get("speed", 1.0)
    try:
        speed = float(raw_speed)
    except (TypeError, ValueError) as error:
        raise ValueError("speed must be a number") from error
    if not MIN_SPEED <= speed <= MAX_SPEED:
        raise ValueError(f"speed must be between {MIN_SPEED} and {MAX_SPEED}")
    return text, voice, speed


class Handler(BaseHTTPRequestHandler):
    server_version = "breadstudy-kokoro/1"
    protocol_version = "HTTP/1.1"

    engine: KokoroEngine

    def log_message(self, fmt: str, *args: Any) -> None:
        LOG.info("%s - %s", self.address_string(), fmt % args)

    def _json(self, status: int, payload: dict[str, Any]) -> None:
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if self.path == "/healthz":
            self._json(
                200,
                {
                    "status": "ok",
                    "model": self.engine.model_path,
                    "voices": self.engine.voices_path,
                    "loaded": self.engine._kokoro is not None,
                },
            )
            return
        self._json(404, {"error": "not found"})

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if self.path != "/v1/audio/speech":
            self._json(404, {"error": "not found"})
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            self._json(400, {"error": "invalid content length"})
            return
        if length <= 0 or length > 64 * 1024:
            self._json(400, {"error": "request body is empty or too large"})
            return
        try:
            payload = json.loads(self.rfile.read(length))
        except (UnicodeDecodeError, json.JSONDecodeError):
            self._json(400, {"error": "body must be JSON"})
            return
        if not isinstance(payload, dict):
            self._json(400, {"error": "body must be a JSON object"})
            return
        try:
            text, voice, speed = _validate(payload)
        except ValueError as error:
            self._json(422, {"error": str(error)})
            return

        started = time.perf_counter()
        try:
            wav, sample_rate = self.engine.synthesize(text, voice, speed)
        except Exception as error:  # noqa: BLE001 - surface a clear local error
            LOG.exception("synthesis failed")
            self._json(503, {"error": str(error)})
            return
        elapsed = time.perf_counter() - started
        LOG.info(
            "synthesized %d chars voice=%s speed=%.2f in %.2fs",
            len(text),
            voice,
            speed,
            elapsed,
        )
        self.send_response(200)
        self.send_header("Content-Type", "audio/wav")
        self.send_header("Content-Length", str(len(wav)))
        self.send_header("X-Kokoro-Sample-Rate", str(sample_rate))
        self.send_header("X-Kokoro-Synth-Seconds", f"{elapsed:.3f}")
        self.end_headers()
        self.wfile.write(wav)


def _preload(engine: KokoroEngine) -> None:
    try:
        engine._ensure_loaded()
    except Exception:  # noqa: BLE001 - preload must not kill the server
        LOG.exception("Kokoro preload failed")


def main() -> int:
    logging.basicConfig(
        level=os.environ.get("KOKORO_LOG_LEVEL", "INFO").upper(),
        format="%(asctime)s %(levelname)s %(message)s",
    )
    host = _env("KOKORO_HOST", DEFAULT_HOST)
    allow_network = _env("KOKORO_ALLOW_NETWORK", "").lower() in ("1", "true", "yes")
    loopback = host in ("127.0.0.1", "localhost", "::1")
    if not loopback and not (allow_network and host == "0.0.0.0"):
        LOG.error(
            "refusing to bind %s; set KOKORO_ALLOW_NETWORK=1 for the private "
            "compose network or use a loopback host",
            host,
        )
        return 2
    port = _port()
    model = _env("KOKORO_MODEL", DEFAULT_MODEL)
    voices = _env("KOKORO_VOICES", DEFAULT_VOICES)
    engine = KokoroEngine(model, voices)
    Handler.engine = engine
    server = ThreadingHTTPServer((host, port), Handler)
    server.daemon_threads = True
    if _env("KOKORO_PRELOAD", "").lower() in ("1", "true", "yes"):
        threading.Thread(
            target=_preload, args=(engine,), name="kokoro-preload", daemon=True
        ).start()
    LOG.info("breadstudy Kokoro TTS listening on http://%s:%d", host, port)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        LOG.info("stopping")
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
