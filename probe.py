#!/usr/bin/env python3
"""Probe a streaming destination for SRT + RTMP reachability.

Reads the same Blackmagic streaming XML format the emulator uses.
For each candidate, runs a short FFmpeg push and reports what
happened: connect refused, handshake-then-disconnect, broken pipe
(server hung up after some packets), or success.

Usage:
    python3 probe.py                       # probe ./config/*.xml
    python3 probe.py path/to/Service.xml   # probe a specific service
    python3 probe.py --host h --port p     # ad-hoc target
    python3 probe.py --srt-only / --rtmp-only
    python3 probe.py --rtmp-apps live,stream,publish

Designed to be cautious: 5-second gap between attempts so we don't
trip rate limiters or IP bans. Sends a tiny test pattern (320x240,
500 kbps) for ~3 seconds per probe.
"""

from __future__ import annotations

import argparse
import re
import shlex
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

# Reuse the emulator's parsers.
sys.path.insert(0, str(Path(__file__).parent))
from bmd_emulator.streamid import build_srt_url, parse_srt_host_port  # noqa: E402
from bmd_emulator.xml_loader import load_service  # noqa: E402

DEFAULT_SRT_PORTS = [1935, 4000, 6000, 7001, 8890, 9000, 9710, 10000]
DEFAULT_RTMP_APPS = ["app", "live", "stream", "publish", "rtmp", ""]
PROBE_DURATION = 3
GAP_BETWEEN_PROBES = 5


@dataclass
class ProbeResult:
    label: str
    url: str
    rc: int
    duration_s: float
    diagnosis: str
    last_lines: list[str]


def classify(stderr: str) -> str:
    s = stderr.lower()
    if "connection refused" in s:
        return "REFUSED — nothing listening or fail2ban temp-ban"
    if "operation timed out" in s or "no route to host" in s:
        return "TIMEOUT — host unreachable / firewall drop"
    if "broken pipe" in s and ("error muxing" in s or "error submitting" in s):
        return "HANDSHAKE OK, PUBLISH REJECTED — server closed mid-stream (auth/bitrate/key mismatch)"
    if "input/output error" in s and "srt" in s:
        return "SRT HANDSHAKE FAILED — no listener on UDP, or streamid rejected"
    if "rtmp_servernsg" in s or "onstatus" in s and "failed" in s:
        return "RTMP REJECTED — server explicitly returned NetStream.Publish.Failed"
    if "successfully connected" in s and "broken pipe" not in s and "error" not in s:
        return "OK — appears to publish without error"
    if "could not find" in s and "format" in s:
        return "PROTOCOL UNKNOWN — ffmpeg lacks the needed support"
    return "UNCLEAR — see last lines"


def run_probe(label: str, url: str, *, dur: int) -> ProbeResult:
    cmd = [
        "ffmpeg", "-hide_banner", "-loglevel", "error",
        "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=10",
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
        "-map", "0:v:0", "-map", "1:a:0",
        "-c:v", "libx264", "-profile:v", "main", "-preset", "ultrafast",
        "-pix_fmt", "yuv420p", "-tune", "zerolatency",
        "-x264-params", "bframes=0:scenecut=0:keyint=20:min-keyint=20",
        "-b:v", "500k", "-maxrate", "500k", "-bufsize", "500k",
        "-c:a", "aac", "-b:a", "64k", "-ar", "48000", "-ac", "2",
        "-t", str(dur),
    ]
    if url.startswith("srt://"):
        cmd += ["-f", "mpegts", "-mpegts_flags", "+resend_headers"]
    elif url.startswith(("rtmp://", "rtmps://")):
        cmd += ["-flvflags", "no_duration_filesize", "-f", "flv"]
    cmd.append(url)

    t0 = time.time()
    proc = subprocess.run(
        cmd,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        timeout=dur + 8,
    )
    elapsed = time.time() - t0
    err = proc.stderr or ""
    diagnosis = classify(err)
    last = [ln for ln in err.splitlines() if ln.strip()][-5:]
    return ProbeResult(
        label=label,
        url=url,
        rc=proc.returncode,
        duration_s=elapsed,
        diagnosis=diagnosis,
        last_lines=last,
    )


def banner(s: str) -> None:
    print()
    print("=" * 72)
    print(s)
    print("=" * 72)


def fmt_result(r: ProbeResult, idx: int, total: int) -> None:
    marker = "✓" if "OK" in r.diagnosis and "REJECTED" not in r.diagnosis else "✗"
    print(f"\n[{idx}/{total}] {marker} {r.label}")
    print(f"  url: {r.url}")
    print(f"  rc={r.rc}  elapsed={r.duration_s:.2f}s")
    print(f"  diagnosis: {r.diagnosis}")
    if r.last_lines:
        print("  last lines:")
        for ln in r.last_lines:
            print(f"    {ln[:140]}")


def gather_targets_from_xml(xml_path: Path) -> list[tuple[str, str, str]]:
    """Return list of (label, base_url, stream_key) for each server in
    the XML. Both RTMP and SRT entries are included.
    """
    svc = load_service(xml_path)
    targets: list[tuple[str, str, str]] = []
    for s in svc.servers:
        targets.append((f"{svc.name} / {s.name} ({s.protocol.upper()})", s.url, svc.key))
    return targets


def expand_srt_ports(host: str, key: str, ports: list[int]) -> list[tuple[str, str, str]]:
    out = []
    for port in ports:
        url = build_srt_url(host, port, key, device_name="probe", device_uuid="00000000-0000-0000-0000-000000000000")
        out.append((f"SRT scan {host}:{port}", url, key))
    return out


def expand_rtmp_apps(host: str, port: int, key: str, apps: list[str]) -> list[tuple[str, str, str]]:
    out = []
    for app in apps:
        if app:
            url = f"rtmp://{host}:{port}/{app}/{key}" if key else f"rtmp://{host}:{port}/{app}"
        else:
            url = f"rtmp://{host}:{port}/{key}" if key else f"rtmp://{host}:{port}/"
        out.append((f"RTMP app='{app or '<none>'}'", url, key))
    return out


def parse_args():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("xml", nargs="?", type=Path, help="XML to probe (default: every ./config/*.xml)")
    p.add_argument("--host", help="Override host for an ad-hoc probe")
    p.add_argument("--port", type=int, default=1935, help="Port for ad-hoc probe (default: 1935)")
    p.add_argument("--key", default="probe-key", help="Stream key for ad-hoc probe")
    p.add_argument("--srt-only", action="store_true")
    p.add_argument("--rtmp-only", action="store_true")
    p.add_argument("--scan-srt-ports", action="store_true",
                   help="Also try SRT on common alt ports if XML primary fails")
    p.add_argument("--scan-rtmp-apps", action="store_true",
                   help="Also try common RTMP app names if XML primary fails")
    p.add_argument("--rtmp-apps", default=",".join(DEFAULT_RTMP_APPS),
                   help="Comma-separated app names for --scan-rtmp-apps")
    p.add_argument("--gap", type=float, default=GAP_BETWEEN_PROBES,
                   help=f"Seconds between probes (default {GAP_BETWEEN_PROBES} — avoids fail2ban)")
    p.add_argument("--duration", type=int, default=PROBE_DURATION,
                   help=f"Per-probe push duration in seconds (default {PROBE_DURATION})")
    return p.parse_args()


def main() -> int:
    args = parse_args()

    targets: list[tuple[str, str, str]] = []

    if args.host:
        # Ad-hoc mode.
        if not args.rtmp_only:
            targets += expand_srt_ports(
                args.host, args.key,
                DEFAULT_SRT_PORTS if args.scan_srt_ports else [args.port],
            )
        if not args.srt_only:
            apps = [a.strip() for a in args.rtmp_apps.split(",")] if args.scan_rtmp_apps else ["app"]
            targets += expand_rtmp_apps(args.host, args.port, args.key, apps)
    else:
        xml_paths: list[Path] = (
            [args.xml] if args.xml else sorted((Path(__file__).parent / "config").glob("*.xml"))
        )
        if not xml_paths:
            print("No XML files found in ./config/. Pass an XML path or --host.")
            return 1
        for xml_path in xml_paths:
            primary = gather_targets_from_xml(xml_path)
            for label, url, key in primary:
                if args.srt_only and not url.startswith("srt://"):
                    continue
                if args.rtmp_only and not url.startswith(("rtmp://", "rtmps://")):
                    continue

                if url.startswith("srt://"):
                    host, port = parse_srt_host_port(url)
                    if args.scan_srt_ports:
                        targets += expand_srt_ports(host, key, DEFAULT_SRT_PORTS)
                    else:
                        full = build_srt_url(host, port, key, device_name="probe",
                                             device_uuid="00000000-0000-0000-0000-000000000000")
                        targets.append((label, full, key))
                elif url.startswith(("rtmp://", "rtmps://")):
                    if args.scan_rtmp_apps:
                        # Strip path, use host+port only.
                        rest = url.split("://", 1)[1]
                        host = rest.split("/", 1)[0]
                        host_only, port_str = (host.split(":", 1) + ["1935"])[:2]
                        apps = [a.strip() for a in args.rtmp_apps.split(",")]
                        targets += expand_rtmp_apps(host_only, int(port_str), key, apps)
                    else:
                        if key and not url.rstrip("/").endswith("/" + key):
                            url = url.rstrip("/") + "/" + key
                        targets.append((label, url, key))

    if not targets:
        print("No targets selected. Pass --scan-srt-ports / --scan-rtmp-apps for broader coverage.")
        return 1

    banner(f"Probing {len(targets)} target(s) — {args.duration}s push, {args.gap}s gap")
    results: list[ProbeResult] = []
    for i, (label, url, _) in enumerate(targets, 1):
        try:
            r = run_probe(label, url, dur=args.duration)
        except subprocess.TimeoutExpired:
            r = ProbeResult(label=label, url=url, rc=-1, duration_s=-1,
                            diagnosis="TIMEOUT (probe hung)", last_lines=[])
        results.append(r)
        fmt_result(r, i, len(targets))
        if i < len(targets):
            time.sleep(args.gap)

    banner("SUMMARY")
    ok = [r for r in results if "OK" in r.diagnosis and "REJECTED" not in r.diagnosis]
    bad = [r for r in results if r not in ok]
    print(f"\n  {len(ok)} apparent successes, {len(bad)} failures")
    if ok:
        print("\n  Working candidates:")
        for r in ok:
            print(f"    • {r.label}  →  {r.url}")
    if bad:
        print("\n  Failure breakdown:")
        for r in bad:
            print(f"    • {r.label}: {r.diagnosis}")
    return 0 if ok else 2


if __name__ == "__main__":
    sys.exit(main())
