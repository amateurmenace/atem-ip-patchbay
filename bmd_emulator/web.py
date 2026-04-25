"""HTTP control panel for the emulator.

Serves a minimal single-page UI on http://localhost:8080 plus a JSON
API for the page to drive. No Flask dep — stdlib only.
"""

from __future__ import annotations

import json
import logging
import mimetypes
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

from .state import EncoderState
from .streamer import Streamer

log = logging.getLogger(__name__)

STATIC_DIR = Path(__file__).parent / "static"


def make_app(state: EncoderState, streamer: Streamer):
    class Handler(BaseHTTPRequestHandler):
        # Quieter access log.
        def log_message(self, fmt, *args):  # noqa: A003
            log.debug("HTTP %s - %s", self.address_string(), fmt % args)

        # ---- routing ---------------------------------------------------

        def do_GET(self):  # noqa: N802
            url = urlparse(self.path)
            path = url.path
            if path in ("/", "/index.html"):
                return self._serve_static("index.html")
            if path.startswith("/static/"):
                return self._serve_static(path[len("/static/"):])
            if path == "/api/state":
                return self._json(200, state.snapshot())
            if path == "/api/log":
                return self._json(200, {
                    "command": streamer.last_command(),
                    "lines": streamer.last_log_tail(50),
                })
            if path == "/api/devices":
                from .device_scanner import list_capture_devices
                qs = parse_qs(url.query)
                force = qs.get("force", ["0"])[0] in ("1", "true")
                devs = list_capture_devices(force=force)
                return self._json(200, devs.to_json())
            if path == "/api/discover":
                from .discover import discover, to_json
                qs = parse_qs(url.query)
                force = qs.get("force", ["0"])[0] in ("1", "true")
                try:
                    found = discover(force=force, timeout=3.0)
                    return self._json(200, {"devices": to_json(found)})
                except Exception as exc:  # noqa: BLE001
                    return self._json(500, {"error": str(exc)})
            if path == "/api/lan-ip":
                from .netinfo import get_lan_ip
                return self._json(200, {"ip": get_lan_ip()})
            if path == "/api/ndi-senders":
                from .discover import discover_ndi
                qs = parse_qs(url.query)
                force = qs.get("force", ["0"])[0] in ("1", "true")
                try:
                    found = discover_ndi(force=force, timeout=2.5)
                    return self._json(200, {"senders": found})
                except Exception as exc:  # noqa: BLE001
                    return self._json(500, {"error": str(exc)})
            return self._json(404, {"error": "not found"})

        def do_POST(self):  # noqa: N802
            url = urlparse(self.path)
            path = url.path
            length = int(self.headers.get("Content-Length", "0") or "0")
            raw = self.rfile.read(length) if length else b""
            try:
                payload = json.loads(raw.decode("utf-8")) if raw else {}
            except json.JSONDecodeError:
                payload = {}

            if path == "/api/start":
                try:
                    streamer.start()
                    return self._json(200, state.snapshot())
                except Exception as exc:  # noqa: BLE001
                    return self._json(400, {"error": str(exc)})

            if path == "/api/stop":
                streamer.stop()
                return self._json(200, state.snapshot())

            if path == "/api/settings":
                return self._update_settings(payload)

            if path == "/api/load_xml":
                xml_path = payload.get("path", "")
                try:
                    svc = state.add_service_from_xml(xml_path)
                    return self._json(200, {
                        "service": svc.name,
                        "snapshot": state.snapshot(),
                    })
                except Exception as exc:  # noqa: BLE001
                    return self._json(400, {"error": str(exc)})

            if path == "/api/load_xml_text":
                xml_text = payload.get("text", "")
                try:
                    svc = state.add_service_from_xml_text(xml_text)
                    return self._json(200, {
                        "service": svc.name,
                        "snapshot": state.snapshot(),
                    })
                except Exception as exc:  # noqa: BLE001
                    return self._json(400, {"error": f"Could not parse XML: {exc}"})

            if path == "/api/destination/paste":
                from . import paste_parser
                text = payload.get("text", "")
                parsed = paste_parser.parse(text)
                if not parsed.is_valid():
                    return self._json(400, {
                        "error": "Couldn't find a URL or stream key in the pasted text.",
                        "parsed": parsed.to_json(),
                    })
                # Apply parsed fields to state — uses custom_url so the
                # paste lives alongside any saved XML services.
                with state.lock:
                    if parsed.url:
                        state.custom_url = parsed.url
                    if parsed.stream_key:
                        state.stream_key = parsed.stream_key
                    if parsed.passphrase:
                        state.passphrase = parsed.passphrase
                    if parsed.name:
                        state.label = parsed.name
                return self._json(200, {
                    "parsed": parsed.to_json(),
                    "snapshot": state.snapshot(),
                })

            return self._json(404, {"error": "not found"})

        # ---- helpers ---------------------------------------------------

        def _update_settings(self, payload: dict):
            with state.lock:
                if "label" in payload:
                    state.label = str(payload["label"])
                if "video_mode" in payload:
                    state.video_mode = str(payload["video_mode"])
                if "quality_level" in payload:
                    state.quality_level = str(payload["quality_level"])
                if "source_id" in payload:
                    state.source_id = str(payload["source_id"])
                if "current_service_name" in payload:
                    name = str(payload["current_service_name"])
                    if name in state.services:
                        state.current_service_name = name
                        state.stream_key = state.services[name].key
                if "current_server_name" in payload:
                    state.current_server_name = str(payload["current_server_name"])
                if "custom_url" in payload:
                    state.custom_url = str(payload["custom_url"]).strip()
                if "stream_key" in payload:
                    state.stream_key = str(payload["stream_key"])
                if "passphrase" in payload:
                    state.passphrase = str(payload["passphrase"])
                if "srt_mode" in payload:
                    mode = str(payload["srt_mode"]).strip().lower()
                    if mode in ("caller", "listener", "rendezvous"):
                        state.srt_mode = mode
                if "srt_latency_us" in payload:
                    try:
                        ms = int(payload["srt_latency_us"])
                        if 20_000 <= ms <= 8_000_000:
                            state.srt_latency_us = ms
                    except (TypeError, ValueError):
                        pass
                if "srt_listen_port" in payload:
                    try:
                        p = int(payload["srt_listen_port"])
                        if 1 <= p <= 65535:
                            state.srt_listen_port = p
                    except (TypeError, ValueError):
                        pass
                if "streamid_override" in payload:
                    state.streamid_override = str(payload["streamid_override"])
                if "streamid_legacy" in payload:
                    state.streamid_legacy = bool(payload["streamid_legacy"])
                if "video_codec" in payload:
                    codec = str(payload["video_codec"]).strip().lower()
                    if codec in ("h264", "h265"):
                        state.video_codec = codec
                if "av_video_index" in payload:
                    try:
                        state.av_video_index = int(payload["av_video_index"])
                    except (TypeError, ValueError):
                        pass
                if "av_audio_index" in payload:
                    try:
                        state.av_audio_index = int(payload["av_audio_index"])
                    except (TypeError, ValueError):
                        pass
                if "pipe_path" in payload:
                    state.pipe_path = str(payload["pipe_path"])
                if "relay" in payload and isinstance(payload["relay"], dict):
                    r = payload["relay"]
                    if "bind_host" in r:
                        state.relay_bind_host = str(r["bind_host"]).strip() or "0.0.0.0"
                    if "srt_port" in r:
                        try:
                            p = int(r["srt_port"])
                            if 1 <= p <= 65535:
                                state.relay_srt_port = p
                        except (TypeError, ValueError):
                            pass
                    if "srt_latency_us" in r:
                        try:
                            us = int(r["srt_latency_us"])
                            if 20_000 <= us <= 8_000_000:
                                state.relay_srt_latency_us = us
                        except (TypeError, ValueError):
                            pass
                    if "srt_passphrase" in r:
                        state.relay_srt_passphrase = str(r["srt_passphrase"])
                    if "rtmp_port" in r:
                        try:
                            p = int(r["rtmp_port"])
                            if 1 <= p <= 65535:
                                state.relay_rtmp_port = p
                        except (TypeError, ValueError):
                            pass
                    if "rtmp_app" in r:
                        state.relay_rtmp_app = str(r["rtmp_app"]).strip() or "live"
                    if "rtmp_key" in r:
                        state.relay_rtmp_key = str(r["rtmp_key"]).strip() or "stream"
                if "overlay" in payload and isinstance(payload["overlay"], dict):
                    o = payload["overlay"]
                    if "title" in o:
                        state.overlay_title = str(o["title"])
                    if "subtitle" in o:
                        state.overlay_subtitle = str(o["subtitle"])
                    if "logo_path" in o:
                        state.overlay_logo_path = str(o["logo_path"])
                    if "clock" in o:
                        state.overlay_clock = bool(o["clock"])
            return self._json(200, state.snapshot())

        def _json(self, code: int, obj):
            body = json.dumps(obj).encode("utf-8")
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def _serve_static(self, relpath: str):
            full = (STATIC_DIR / relpath).resolve()
            if not str(full).startswith(str(STATIC_DIR.resolve())):
                return self._json(403, {"error": "forbidden"})
            if not full.is_file():
                return self._json(404, {"error": "not found"})
            ctype, _ = mimetypes.guess_type(full.name)
            data = full.read_bytes()
            self.send_response(200)
            self.send_header("Content-Type", ctype or "application/octet-stream")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(data)

    return Handler


class WebServer:
    def __init__(self, state: EncoderState, streamer: Streamer, host: str = "127.0.0.1", port: int = 8080):
        self.state = state
        self.streamer = streamer
        self.host = host
        self.port = port
        self._httpd: ThreadingHTTPServer | None = None
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        handler = make_app(self.state, self.streamer)
        # Try the preferred port first, then walk forward up to 9 spots
        # (8090 → 8091 → … → 8099). A stale instance holding the
        # preferred port shouldn't silently brick a fresh launch — the
        # alternative was the .app exiting with no UI in windowed mode,
        # leaving the user with a "process in Activity Monitor but no
        # window" mystery (which is exactly what happened on first try).
        last_err: OSError | None = None
        for offset in range(10):
            try_port = self.port + offset
            try:
                self._httpd = ThreadingHTTPServer((self.host, try_port), handler)
                self.port = try_port
                last_err = None
                break
            except OSError as exc:
                last_err = exc
        if self._httpd is None:
            raise RuntimeError(
                f"Could not bind HTTP server on {self.host}:{self.port}-{self.port + 9}: {last_err}"
            )
        self._thread = threading.Thread(
            target=self._httpd.serve_forever,
            name="http-control",
            daemon=True,
        )
        self._thread.start()
        log.info("HTTP control panel: http://%s:%d/", self.host, self.port)

    def stop(self) -> None:
        if self._httpd is not None:
            self._httpd.shutdown()
            self._httpd.server_close()
            self._httpd = None
