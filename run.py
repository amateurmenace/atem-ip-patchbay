#!/usr/bin/env python3
"""Blackmagic Streaming Encoder Emulator — entry point.

Starts:
  - the BMD Streaming Encoder control protocol server on TCP 9977
  - an HTTP control panel on http://127.0.0.1:8080/

The first XML file under ./config/ (or one passed via --xml) is loaded
as the active streaming service. Defaults match the Web Presenter 1.xml
shipped with this repo.
"""

from __future__ import annotations

import argparse
import logging
import signal
import sys
import time
import webbrowser
from pathlib import Path

from bmd_emulator.protocol import ProtocolServer
from bmd_emulator.state import EncoderState
from bmd_emulator.streamer import Streamer
from bmd_emulator.web import WebServer


def parse_args(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "--xml",
        type=Path,
        action="append",
        help="Path to a Blackmagic streaming service XML. May be repeated. "
        "Default: every *.xml under ./config/",
    )
    p.add_argument("--http-host", default="127.0.0.1")
    p.add_argument(
        "--http-port", type=int, default=8090,
        help="HTTP UI port (default: 8090; will walk forward up to 8099 if taken)",
    )
    p.add_argument(
        "--protocol-port",
        type=int,
        default=9977,
        help="TCP port for BMD control protocol (default: 9977 — the real port)",
    )
    p.add_argument("--no-browser", action="store_true", help="Don't auto-open the UI")
    p.add_argument(
        "--label",
        default="Blackmagic Streaming Encoder Emulator",
        help="Device label shown in IDENTITY block and as bmd_name in streamid",
    )
    p.add_argument(
        "-v", "--verbose", action="store_true", help="Enable debug logging"
    )
    return p.parse_args(argv)


def find_xml_files(args) -> list[Path]:
    if args.xml:
        return [p for p in args.xml if p.exists()]
    config_dir = Path(__file__).parent / "config"
    if not config_dir.is_dir():
        return []
    return sorted(config_dir.glob("*.xml"))


def main(argv=None) -> int:
    args = parse_args(argv)

    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)-7s %(name)s: %(message)s",
        datefmt="%H:%M:%S",
    )
    log = logging.getLogger("bmd-emulator")

    state = EncoderState(label=args.label)

    # Pick reasonable defaults for capture-device indices on first run.
    # On macOS this scans AVFoundation; on Windows, DirectShow.
    try:
        from bmd_emulator.device_scanner import (
            find_default_audio_index,
            find_default_video_index,
            list_capture_devices,
        )
        devs = list_capture_devices(force=True)
        state.av_video_index = find_default_video_index(devs)
        state.av_audio_index = find_default_audio_index(devs)
    except Exception:  # noqa: BLE001
        pass

    xml_files = find_xml_files(args)
    if not xml_files:
        log.warning(
            "No service XML loaded. Drop a Blackmagic streaming XML in ./config/ "
            "or pass --xml. The emulator will run but cannot start a stream "
            "until a destination is configured."
        )
    for path in xml_files:
        try:
            svc = state.add_service_from_xml(str(path))
            log.info("Loaded service %r from %s", svc.name, path.name)
        except Exception as exc:  # noqa: BLE001
            log.error("Failed to load %s: %s", path, exc)

    streamer = Streamer(state)

    proto = ProtocolServer(state, streamer, port=args.protocol_port)
    proto.start()

    web = WebServer(state, streamer, host=args.http_host, port=args.http_port)
    web.start()

    # web.port may have walked forward from the requested port if it
    # was taken — use whatever it actually bound to.
    url = f"http://{args.http_host}:{web.port}/"
    log.info("Ready. Open %s", url)
    if not args.no_browser:
        try:
            webbrowser.open(url)
        except Exception:  # noqa: BLE001
            pass

    stop = {"flag": False}

    def _shutdown(signum, frame):  # noqa: ARG001
        log.info("Shutting down…")
        stop["flag"] = True

    signal.signal(signal.SIGINT, _shutdown)
    signal.signal(signal.SIGTERM, _shutdown)

    try:
        while not stop["flag"]:
            time.sleep(0.5)
    finally:
        streamer.stop()
        proto.stop()
        web.stop()

    return 0


if __name__ == "__main__":
    sys.exit(main())
