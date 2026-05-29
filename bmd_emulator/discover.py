"""Discover Blackmagic devices on the local network via mDNS / Bonjour.

ATEM switchers, Streaming Bridges, HyperDecks, and most BMD-LAN devices
advertise on the `_blackmagic._tcp` Bonjour service type. Some newer
HyperDecks moved to `_bmd_blockcfg._tcp` and `_hyperdeck_ctrl._tcp`.

We use macOS's `dns-sd` CLI (always present) — no Python deps.
"""

from __future__ import annotations

import logging
import re
import subprocess
import threading
import time
from dataclasses import dataclass

log = logging.getLogger(__name__)

SERVICE_TYPES = [
    "_blackmagic._tcp",
    "_bmd_blockcfg._tcp",
    "_hyperdeck_ctrl._tcp",
]


@dataclass
class DiscoveredDevice:
    name: str
    service_type: str
    host: str = ""
    port: int = 0
    txt: dict[str, str] | None = None


# `dns-sd -B` output looks like:
#   Browsing for _blackmagic._tcp.local
#   DATE: ---Sat 25 Apr 2026---
#  6:54:31.512  ...STARTING...
# Timestamp     A/R    Flags  if Domain               Service Type         Instance Name
#  6:54:31.612  Add        2   8 local.               _blackmagic._tcp.    ATEM Mini Extreme ISO G2
_BROWSE_RE = re.compile(r"^\s*[\d:.]+\s+(Add|Rmv)\s+\S+\s+\d+\s+\S+\s+(\S+)\s+(.+)$")


def _drain_lines(proc: subprocess.Popen, deadline: float) -> list[str]:
    """Drain `proc.stdout` until `deadline`, then terminate. Non-blocking
    via a background thread + a deadline-bounded join.
    """
    lines: list[str] = []

    def reader():
        try:
            for line in proc.stdout:  # type: ignore[union-attr]
                lines.append(line)
        except Exception:  # noqa: BLE001
            pass

    t = threading.Thread(target=reader, daemon=True)
    t.start()
    remaining = max(0.0, deadline - time.time())
    t.join(timeout=remaining)
    try:
        proc.terminate()
        proc.wait(timeout=1.0)
    except Exception:  # noqa: BLE001
        try:
            proc.kill()
        except Exception:  # noqa: BLE001
            pass
    t.join(timeout=0.5)
    return lines


def _browse(stype: str, timeout: float = 2.5) -> list[str]:
    """Return instance names seen during `timeout` seconds of browsing."""
    proc = subprocess.Popen(
        ["dns-sd", "-B", stype, "local."],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        bufsize=1,
    )
    deadline = time.time() + timeout
    lines = _drain_lines(proc, deadline)
    seen: set[str] = set()
    for line in lines:
        m = _BROWSE_RE.match(line)
        if m and m.group(1) == "Add":
            seen.add(m.group(3).strip())
    return sorted(seen)


# `dns-sd -L` output looks like:
#  6:54:31.812  ATEM Mini Extreme ISO G2._blackmagic._tcp.local. can be reached at
#               atem-extreme.local.:9910 (interface 8)
#               txtvers=1
#               name=ATEM Mini Extreme ISO G2
_RESOLVE_HOSTPORT_RE = re.compile(r"can be reached at\s+(\S+):(\d+)")
_RESOLVE_TXT_RE = re.compile(r"^\s*([A-Za-z0-9_.-]+)=(.*)$")


def _resolve(name: str, stype: str, timeout: float = 1.5) -> tuple[str, int, dict[str, str]]:
    proc = subprocess.Popen(
        ["dns-sd", "-L", name, stype, "local."],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        bufsize=1,
    )
    host, port = "", 0
    txt: dict[str, str] = {}
    deadline = time.time() + timeout
    for line in _drain_lines(proc, deadline):
        m = _RESOLVE_HOSTPORT_RE.search(line)
        if m:
            host = m.group(1).rstrip(".")
            try:
                port = int(m.group(2))
            except ValueError:
                port = 0
            continue
        m = _RESOLVE_TXT_RE.match(line)
        if m:
            txt[m.group(1)] = m.group(2)
    return host, port, txt


_cache: list[DiscoveredDevice] = []
_cache_lock = threading.Lock()
_cache_at: float = 0.0
_CACHE_TTL = 30.0


def discover(force: bool = False, timeout: float = 2.5) -> list[DiscoveredDevice]:
    """Browse all known BMD service types, return resolved device list."""
    global _cache_at, _cache
    with _cache_lock:
        if not force and _cache and (time.time() - _cache_at) < _CACHE_TTL:
            return _cache
    found: list[DiscoveredDevice] = []
    for stype in SERVICE_TYPES:
        for name in _browse(stype, timeout=timeout):
            host, port, txt = _resolve(name, stype, timeout=1.2)
            found.append(
                DiscoveredDevice(name=name, service_type=stype, host=host, port=port, txt=txt)
            )
    with _cache_lock:
        _cache = found
        _cache_at = time.time()
    return found


def to_json(devs: list[DiscoveredDevice]) -> list[dict]:
    return [
        {
            "name": d.name,
            "service_type": d.service_type,
            "host": d.host,
            "port": d.port,
            "txt": d.txt or {},
        }
        for d in devs
    ]


# ---------------------------------------------------------------------------
# NDI discovery (separate API — distinct from BMD device discovery)
# ---------------------------------------------------------------------------
#
# NDI senders advertise on `_ndi._tcp.local.` (NDI v3+). Each instance name
# follows the pattern `MACHINE-NAME (Source Name)`, e.g.:
#   `STUDIO-MAC (Camera 1)`
#   `STUDIO-MAC (NDI Virtual Input)`
# We don't bother resolving each one — the name string + host machine is all
# we surface. Consumption requires NDI Virtual Camera (NDI Tools) routing the
# selected sender into a video device, since FFmpeg here doesn't have
# libndi_newtek compiled in.

_ndi_cache: list[dict] | None = None
_ndi_cache_at: float = 0.0
_NDI_CACHE_TTL = 20.0


def discover_ndi(force: bool = False, timeout: float = 2.5) -> list[dict]:
    """Browse `_ndi._tcp.local.` for NDI senders on the LAN.

    Returns a deduped list of dicts with `machine` and `source` keys parsed
    from each `MACHINE (Source)` instance name.
    """
    global _ndi_cache, _ndi_cache_at
    if not force and _ndi_cache is not None and (time.time() - _ndi_cache_at) < _NDI_CACHE_TTL:
        return _ndi_cache

    names = _browse("_ndi._tcp", timeout=timeout)
    seen: dict[tuple[str, str], dict] = {}
    for name in names:
        machine, source = _split_ndi_name(name)
        key = (machine.lower(), source.lower())
        if key not in seen:
            seen[key] = {
                "name": name,
                "machine": machine,
                "source": source,
            }
    _ndi_cache = list(seen.values())
    _ndi_cache_at = time.time()
    return _ndi_cache


def _split_ndi_name(name: str) -> tuple[str, str]:
    """`MACHINE-NAME (Source Name)` → (`MACHINE-NAME`, `Source Name`)."""
    name = name.strip()
    open_idx = name.rfind("(")
    close_idx = name.rfind(")")
    if open_idx > 0 and close_idx > open_idx:
        return name[:open_idx].strip(), name[open_idx + 1:close_idx].strip()
    return name, ""
