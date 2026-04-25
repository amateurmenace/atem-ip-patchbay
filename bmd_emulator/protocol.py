"""Blackmagic Streaming Encoder Ethernet Protocol server.

Implements enough of the v1.2 protocol to look like a real Streaming
Encoder to clients (the BMD Streaming Setup utility, ATEM software,
or any custom integration). Listens on TCP 9977.

Protocol shape:
  - Line-oriented, ASCII.
  - Server lines end with LF; client lines may end with LF or CRLF.
  - Messages are blocks: a header line ending in `:`, then key:value
    lines, then a blank line.
  - On connect, the server dumps a preamble + every status block, then
    `END PRELUDE:` + blank line.
  - Client sends a block to mutate state; server replies `ACK` or
    `NACK`, then a fresh status block reflecting the change.
"""

from __future__ import annotations

import logging
import socket
import threading
from typing import Callable

from .state import (
    AVAILABLE_VIDEO_MODES,
    EncoderState,
    QUALITY_LEVELS,
)
from .streamer import Streamer

log = logging.getLogger(__name__)

PROTOCOL_VERSION = "1.2"
PORT = 9977


class ProtocolServer:
    def __init__(self, state: EncoderState, streamer: Streamer, port: int = PORT):
        self.state = state
        self.streamer = streamer
        self.port = port
        self._sock: socket.socket | None = None
        self._thread: threading.Thread | None = None
        self._clients: list[socket.socket] = []
        self._clients_lock = threading.Lock()
        self._stop = threading.Event()
        self._pending_block: dict[socket.socket, tuple[str, list[tuple[str, str]]]] = {}

    # ---- lifecycle ---------------------------------------------------------

    def start(self) -> None:
        if self._thread is not None:
            return
        # Port-walk fallback: try the requested port first (9977 is the
        # documented BMD port), then up to nine spots forward. A real
        # BMD encoder on the LAN doesn't conflict (we bind 0.0.0.0
        # locally; the LAN device has its own bind), but a stale
        # instance on the same machine would silently brick a fresh
        # launch otherwise.
        last_err: OSError | None = None
        for offset in range(10):
            try_port = self.port + offset
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                sock.bind(("0.0.0.0", try_port))
                self._sock = sock
                self.port = try_port
                last_err = None
                break
            except OSError as exc:
                sock.close()
                last_err = exc
        if self._sock is None:
            raise RuntimeError(
                f"Could not bind BMD protocol on TCP {self.port}-{self.port + 9}: {last_err}"
            )
        self._sock.listen(8)
        self._sock.settimeout(0.5)
        self._stop.clear()
        self._thread = threading.Thread(
            target=self._accept_loop, name="bmd-protocol", daemon=True
        )
        self._thread.start()
        log.info("BMD control protocol listening on TCP %d", self.port)

    def stop(self) -> None:
        self._stop.set()
        if self._sock is not None:
            try:
                self._sock.close()
            except OSError:
                pass
        with self._clients_lock:
            for c in self._clients:
                try:
                    c.close()
                except OSError:
                    pass
            self._clients.clear()

    # ---- accept loop -------------------------------------------------------

    def _accept_loop(self) -> None:
        assert self._sock is not None
        while not self._stop.is_set():
            try:
                client, addr = self._sock.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            log.info("control client connected: %s", addr)
            with self._clients_lock:
                self._clients.append(client)
            t = threading.Thread(
                target=self._client_loop,
                args=(client, addr),
                daemon=True,
                name=f"bmd-client-{addr[1]}",
            )
            t.start()

    def _client_loop(self, client: socket.socket, addr) -> None:
        try:
            self._send_preamble(client)
            buf = b""
            while not self._stop.is_set():
                try:
                    chunk = client.recv(4096)
                except OSError:
                    break
                if not chunk:
                    break
                buf += chunk
                while b"\n" in buf:
                    line, buf = buf.split(b"\n", 1)
                    self._feed_line(client, line.decode("utf-8", "replace"))
        finally:
            with self._clients_lock:
                if client in self._clients:
                    self._clients.remove(client)
            try:
                client.close()
            except OSError:
                pass
            log.info("control client disconnected: %s", addr)

    # ---- block I/O ---------------------------------------------------------

    def _send(self, sock: socket.socket, lines: list[str]) -> None:
        payload = ("\n".join(lines) + "\n").encode("utf-8")
        try:
            sock.sendall(payload)
        except OSError:
            pass

    def _send_block(self, sock: socket.socket, header: str, kvs: list[tuple[str, str]]) -> None:
        lines = [f"{header}:"]
        for k, v in kvs:
            lines.append(f"{k}: {v}")
        lines.append("")
        self._send(sock, lines)

    def _broadcast(self, header: str, kvs: list[tuple[str, str]]) -> None:
        with self._clients_lock:
            clients = list(self._clients)
        for c in clients:
            self._send_block(c, header, kvs)

    # ---- preamble + dump ---------------------------------------------------

    def _send_preamble(self, sock: socket.socket) -> None:
        self._send_block(sock, "PROTOCOL PREAMBLE", [("Version", PROTOCOL_VERSION)])
        self._send_block(sock, "IDENTITY", self._identity_kvs())
        self._send_block(sock, "VERSION", self._version_kvs())
        for line_group in self._network_lines():
            self._send(sock, line_group)
        self._send_block(sock, "UI SETTINGS", self._ui_kvs())
        self._send_block(sock, "STREAM SETTINGS", self._stream_settings_kvs())
        self._send_block(sock, "STREAM XML", [("Files", ", ".join(self.state.services.keys()))])
        self._send_block(sock, "STREAM STATE", self._stream_state_kvs())
        self._send_block(sock, "AUDIO SETTINGS", self._audio_kvs())
        self._send_block(sock, "END PRELUDE", [])

    # ---- block builders ----------------------------------------------------

    def _identity_kvs(self) -> list[tuple[str, str]]:
        s = self.state
        return [
            ("Model", s.model),
            ("Label", s.label),
            ("Unique ID", s.unique_id),
        ]

    def _version_kvs(self) -> list[tuple[str, str]]:
        return [
            ("Product ID", "BE73"),
            ("Hardware Version", "0100"),
            ("Software Version", "01000000"),
            ("Software Release", "0.1"),
        ]

    def _network_lines(self) -> list[list[str]]:
        block: list[str] = ["NETWORK:", "Interface Count: 1", "Default Interface: 0", ""]
        iface: list[str] = [
            "NETWORK INTERFACE 0:",
            "Name: Ethernet",
            "Priority: 1",
            "MAC Address: 00:11:22:33:44:55",
            "Dynamic IP: true",
            "Current Addresses: 0.0.0.0/255.255.255.0",
            "Current Gateway: 0.0.0.0",
            "Current DNS Servers: ",
            "Static Addresses: 0.0.0.0/255.255.255.0",
            "Static Gateway: 0.0.0.0",
            "Static DNS Servers: 8.8.8.8, 8.8.4.4",
            "",
        ]
        return [block, iface]

    def _ui_kvs(self) -> list[tuple[str, str]]:
        return [
            ("Available Locales", "en_US.UTF-8"),
            ("Current Locale", "en_US.UTF-8"),
            ("Available Audio Meters", "PPM -18dB, PPM -20dB, VU -18dB, VU -20dB"),
            ("Current Audio Meter", "PPM -20dB"),
        ]

    def _stream_settings_kvs(self) -> list[tuple[str, str]]:
        s = self.state
        snap = s.snapshot()
        url = snap.get("current_url", "") or ""
        servers = snap.get("available_servers") or []
        server_names = ", ".join(sv["name"] for sv in servers) or "SRT"
        return [
            ("Available Video Modes", ", ".join(AVAILABLE_VIDEO_MODES)),
            ("Video Mode", s.video_mode),
            ("Current Platform", s.current_service_name or "My Platform"),
            ("Current Server", s.current_server_name),
            ("Current Quality Level", s.quality_level),
            ("Stream Key", s.stream_key),
            ("Password", s.passphrase),
            ("Current URL", url),
            ("Customizable URL", "true"),
            ("Available Default Platforms", ""),
            ("Available Custom Platforms", ", ".join(self.state.services.keys())),
            ("Available Servers", server_names),
            ("Available Quality Levels", ", ".join(QUALITY_LEVELS)),
        ]

    def _stream_state_kvs(self) -> list[tuple[str, str]]:
        s = self.state.stats
        return [
            ("Status", s.status),
            ("Bitrate", str(s.bitrate)),
            ("Duration", s.duration_string()),
            ("Cache Used", str(s.cache_used_pct)),
        ]

    def _audio_kvs(self) -> list[tuple[str, str]]:
        return [
            ("Current Monitor Out Audio Source", "Auto"),
            ("Available Monitor Out Audio Sources", "Auto, SDI In, Remote Source"),
        ]

    # ---- line accumulator → block dispatcher -------------------------------

    def _feed_line(self, client: socket.socket, line: str) -> None:
        line = line.rstrip("\r")
        pending = self._pending_block.get(client)
        if pending is None:
            if not line.strip():
                return
            if line.endswith(":"):
                header = line[:-1].strip()
                self._pending_block[client] = (header, [])
            return

        if line.strip() == "":
            header, kvs = pending
            del self._pending_block[client]
            self._dispatch(client, header, kvs)
            return

        if ":" in line:
            k, v = line.split(":", 1)
            pending[1].append((k.strip(), v.strip()))

    # ---- dispatch ----------------------------------------------------------

    def _dispatch(self, client: socket.socket, header: str, kvs: list[tuple[str, str]]) -> None:
        log.info("RX block %r %r", header, kvs)
        handler: Callable[[socket.socket, list[tuple[str, str]]], bool] | None = {
            "IDENTITY": self._handle_identity,
            "STREAM SETTINGS": self._handle_stream_settings,
            "STREAM STATE": self._handle_stream_state,
            "STREAM XML": self._handle_stream_xml,
            "NETWORK INTERFACE 0": self._handle_network_iface,
            "UI SETTINGS": self._handle_ui_settings,
            "AUDIO SETTINGS": self._handle_audio,
            "SHUTDOWN": self._handle_shutdown,
        }.get(header)

        if handler is None:
            self._send_block(client, "NACK", [])
            return

        try:
            ok = handler(client, kvs)
        except Exception as exc:  # noqa: BLE001
            log.exception("handler failed")
            self._send_block(client, "NACK", [])
            self._send_block(client, "ERROR", [("Message", str(exc))])
            return

        if ok:
            self._send_block(client, "ACK", [])
            # Re-dump the affected block to all clients so they stay in sync.
            self._broadcast_after(header)
        else:
            self._send_block(client, "NACK", [])

    def _broadcast_after(self, header: str) -> None:
        if header == "STREAM SETTINGS" or not header:
            self._broadcast("STREAM SETTINGS", self._stream_settings_kvs())
        elif header == "STREAM STATE":
            self._broadcast("STREAM STATE", self._stream_state_kvs())
        elif header == "IDENTITY":
            self._broadcast("IDENTITY", self._identity_kvs())
        elif header == "AUDIO SETTINGS":
            self._broadcast("AUDIO SETTINGS", self._audio_kvs())

    # ---- handlers ----------------------------------------------------------

    def _handle_identity(self, client, kvs) -> bool:
        for k, v in kvs:
            if k == "Label":
                self.state.label = v
        return True

    def _handle_stream_settings(self, client, kvs) -> bool:
        s = self.state
        with s.lock:
            for k, v in kvs:
                if k == "Video Mode" and v in AVAILABLE_VIDEO_MODES:
                    s.video_mode = v
                elif k == "Current Platform":
                    if v in s.services:
                        s.current_service_name = v
                        s.stream_key = s.services[v].key
                elif k == "Current Quality Level":
                    s.quality_level = v
                elif k == "Stream Key":
                    s.stream_key = v
                elif k == "Password":
                    s.passphrase = v
                elif k == "Current Server":
                    s.current_server_name = v
        return True

    def _handle_stream_state(self, client, kvs) -> bool:
        for k, v in kvs:
            if k == "Action":
                action = v.strip().lower()
                if action == "start":
                    try:
                        self.streamer.start()
                    except Exception as exc:  # noqa: BLE001
                        self.state.stats.status = "Interrupted"
                        self.state.stats.error = str(exc)
                        return False
                elif action == "stop":
                    self.streamer.stop()
        return True

    def _handle_stream_xml(self, client, kvs) -> bool:
        # We accept STREAM XML <name>: blocks for adding a service, but
        # the body is the full XML document — we don't inline-parse it
        # in this emulator. Adding files happens via the HTTP UI instead.
        return True

    def _handle_network_iface(self, client, kvs) -> bool:
        # Read-only here; pretend we accepted it for compatibility.
        return True

    def _handle_ui_settings(self, client, kvs) -> bool:
        return True

    def _handle_audio(self, client, kvs) -> bool:
        return True

    def _handle_shutdown(self, client, kvs) -> bool:
        for k, v in kvs:
            if k == "Action" and v.strip().lower() == "factory reset":
                self.streamer.stop()
                self.state.stats.error = None
                return True
        return True
