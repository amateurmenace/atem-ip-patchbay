"""Scan the OS for available video + audio capture devices.

Two backends, dispatched by ``sys.platform``:

  - macOS — FFmpeg's avfoundation indev exposes every camera (FaceTime,
    USB webcams, Continuity Camera iPhones, screen capture, OBS Virtual
    Camera, NDI Virtual Camera, etc.) and every audio interface
    (microphones, NDI Audio, ZoomAudioDevice, etc.). Each device has a
    real integer index that we pass straight to FFmpeg.

  - Windows — FFmpeg's dshow indev enumerates DirectShow capture
    devices by name. We assign synthetic positional indices so the
    rest of the app can keep treating ``av_video_index`` /
    ``av_audio_index`` as opaque selectors. The resolver in
    ``sources.py`` looks up the name from the index when building the
    FFmpeg command. We also prepend a synthetic ``Capture screen 0``
    entry so the UI's "Screen" tile has something to bind to — that
    entry is recognised by category and dispatched through gdigrab
    instead of dshow.

Both backends return the same ``DeviceList`` shape so the UI's
``/api/devices`` response is identical across platforms.
"""

from __future__ import annotations

import logging
import re
import subprocess
import sys
import threading
import time
from dataclasses import dataclass

log = logging.getLogger(__name__)

# Matches an avfoundation device line: `[avfoundation @ 0x...] [N] Name`.
_AVF_DEVICE_LINE = re.compile(r"\[[^\]]+\]\s*\[(\d+)\]\s*(.+)$")

# Matches a dshow device line. FFmpeg's prefix changed somewhere
# around the 7.x → 8.x line: older builds used ``[dshow @ ADDR]``,
# newer builds (BtbN n8.1, used by the Windows installer) use the
# generic ``[in#0 @ ADDR]`` for input-devicelisting. We accept both.
#
# The trailing kind marker is also more diverse on modern FFmpeg:
#   (video)         — pure video device
#   (audio)         — pure audio device
#   (audio, video)  — combined-input capture (Blackmagic WDM, etc.)
#   (video, audio)  — same, alternate ordering
#   (none)          — FFmpeg can't determine the type (OBS Virtual
#                     Camera shows up this way)
# We capture the whole parenthesised marker as group(2) and decode
# it in _resolve_dshow_kind below.
_DSHOW_DEVICE_LINE = re.compile(
    r'\[(?:dshow|in#\d+) @ [^\]]+\]\s+"([^"]+)"(?:\s+\(([^)]*)\))?\s*$'
)


@dataclass
class Device:
    index: int
    name: str
    kind: str  # "video" or "audio"

    @property
    def category(self) -> str:
        """Bucket the device for the source-tile UI.

        Returns one of: screen, capture_card, ndi, virtual, iphone, camera
        (for video devices) or ndi, microphone, virtual (for audio).
        """
        return categorize_device(self.name, self.kind)

    def to_json(self) -> dict:
        return {"index": self.index, "name": self.name, "category": self.category}


@dataclass
class DeviceList:
    video: list[Device]
    audio: list[Device]
    scanned_at: float

    def to_json(self) -> dict:
        return {
            "video": [d.to_json() for d in self.video],
            "audio": [d.to_json() for d in self.audio],
        }


# Patterns are tested in order; first match wins. Keep specific before generic.
_VIDEO_CATEGORY_PATTERNS: list[tuple[str, re.Pattern]] = [
    ("screen", re.compile(r"capture screen|desk view|screen capture|desktop", re.I)),
    # "blackmagic" + "wdm capture" cover the Windows-side dshow names for
    # DeckLink / UltraStudio / Web Presenter capture (e.g.
    # "Blackmagic WDM Capture (12)"). NDI is matched in its own row so
    # NDI Webcam pseudo-cameras don't get bucketed as capture cards.
    ("capture_card", re.compile(r"ultrastudio|decklink|intensity|aja|magewell|elgato|epiphan|wdm capture|blackmagic", re.I)),
    ("ndi", re.compile(r"\bndi\b", re.I)),
    ("iphone", re.compile(r"iphone|ipad", re.I)),
    ("virtual", re.compile(r"virtual|obs|sysram|loopback|syphon|vmix", re.I)),
]

_AUDIO_CATEGORY_PATTERNS: list[tuple[str, re.Pattern]] = [
    ("ndi", re.compile(r"\bndi\b", re.I)),
    ("virtual", re.compile(r"virtual|loopback|aggregate|blackhole|soundflower|background music|stereo mix|vmix", re.I)),
]


def categorize_device(name: str, kind: str) -> str:
    patterns = _VIDEO_CATEGORY_PATTERNS if kind == "video" else _AUDIO_CATEGORY_PATTERNS
    for cat, pat in patterns:
        if pat.search(name):
            return cat
    return "camera" if kind == "video" else "microphone"


_cache: DeviceList | None = None
_cache_lock = threading.Lock()
_CACHE_TTL = 60.0


def list_capture_devices(force: bool = False) -> DeviceList:
    """Return cached device list, rescanning if older than 60s or `force`.

    Dispatches to the avfoundation scanner on macOS and the dshow
    scanner on Windows. On other platforms, returns an empty list.
    """
    global _cache
    with _cache_lock:
        now = time.time()
        if not force and _cache is not None and (now - _cache.scanned_at) < _CACHE_TTL:
            return _cache

        if sys.platform == "darwin":
            video, audio = _scan_avfoundation()
        elif sys.platform.startswith("win"):
            video, audio = _scan_dshow()
        else:
            log.debug("No capture-device scanner for platform %s", sys.platform)
            video, audio = [], []

        _cache = DeviceList(video=video, audio=audio, scanned_at=now)
        return _cache


# Backwards-compat alias — older callers (and external scripts) may still
# import this name. Will be removed once nothing references it.
def list_avfoundation_devices(force: bool = False) -> DeviceList:
    return list_capture_devices(force=force)


# ---------------------------------------------------------------------------
# macOS — AVFoundation
# ---------------------------------------------------------------------------


def _scan_avfoundation() -> tuple[list[Device], list[Device]]:
    """Run `ffmpeg -f avfoundation -list_devices true -i ""` and parse.

    Calling FFmpeg with `-list_devices true` is fast (~200ms) but does
    print a TTY camera-permission prompt the first time on a fresh
    install. Once the user grants access, subsequent calls are silent.
    """
    from .ffmpeg_path import ffmpeg_path
    try:
        proc = subprocess.run(
            [ffmpeg_path(), "-hide_banner", "-f", "avfoundation",
             "-list_devices", "true", "-i", ""],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            timeout=8,
        )
        text = proc.stderr or ""
    except (subprocess.TimeoutExpired, FileNotFoundError) as exc:
        log.warning("avfoundation scan failed: %s", exc)
        text = ""

    return _parse_avfoundation(text)


def _parse_avfoundation(text: str) -> tuple[list[Device], list[Device]]:
    video: list[Device] = []
    audio: list[Device] = []
    section: str | None = None
    for raw_line in text.splitlines():
        line = raw_line.rstrip()
        low = line.lower()
        if "avfoundation video devices" in low:
            section = "video"
            continue
        if "avfoundation audio devices" in low:
            section = "audio"
            continue
        if section is None:
            continue
        m = _AVF_DEVICE_LINE.search(line)
        if not m:
            continue
        idx = int(m.group(1))
        name = m.group(2).strip()
        dev = Device(index=idx, name=name, kind=section)
        (video if section == "video" else audio).append(dev)
    return video, audio


# ---------------------------------------------------------------------------
# Windows — DirectShow
# ---------------------------------------------------------------------------


def _scan_dshow() -> tuple[list[Device], list[Device]]:
    """Run `ffmpeg -f dshow -list_devices true -i dummy` and parse.

    DirectShow device enumeration writes to stderr and exits non-zero
    (because the dummy input doesn't exist) — both are expected. We
    only care about the stderr text.

    A synthetic ``Capture screen 0`` entry is prepended to the video
    list so the UI's screen-capture tile has something to bind to;
    the source resolver routes that entry through gdigrab instead of
    dshow.
    """
    from .ffmpeg_path import ffmpeg_path
    try:
        proc = subprocess.run(
            [ffmpeg_path(), "-hide_banner", "-f", "dshow",
             "-list_devices", "true", "-i", "dummy"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            timeout=8,
        )
        text = proc.stderr or ""
    except (subprocess.TimeoutExpired, FileNotFoundError) as exc:
        log.warning("dshow scan failed: %s", exc)
        text = ""

    video, audio = _parse_dshow(text)

    # Prepend the desktop-capture entry. Index 0 in the video list keeps
    # the "first video device" default reasonable on machines with no
    # other camera (most Windows servers).
    desktop = Device(index=0, name="Capture screen 0", kind="video")
    for d in video:
        d.index += 1
    video.insert(0, desktop)
    return video, audio


def _parse_dshow(text: str) -> tuple[list[Device], list[Device]]:
    video: list[Device] = []
    audio: list[Device] = []
    # Legacy section-header fallback: older FFmpeg builds emitted
    # "DirectShow video devices" / "DirectShow audio devices" headers
    # and unmarked device lines underneath. Newer builds carry the
    # kind inline per-device, so this state machine just stays at None
    # in the modern-format path.
    section: str | None = None
    v_idx = 0
    a_idx = 0
    for raw_line in text.splitlines():
        line = raw_line.rstrip()
        low = line.lower()
        if "directshow video devices" in low:
            section = "video"
            continue
        if "directshow audio devices" in low:
            section = "audio"
            continue
        # Alternative-name lines reference the same device by its
        # `@device_pnp_...` GUID and we don't need them.
        if "alternative name" in low:
            continue
        m = _DSHOW_DEVICE_LINE.search(line)
        if not m:
            continue
        name = m.group(1).strip()
        kind_marker = m.group(2)
        kinds = _resolve_dshow_kind(kind_marker, section)
        if "video" in kinds:
            video.append(Device(index=v_idx, name=name, kind="video"))
            v_idx += 1
        if "audio" in kinds:
            audio.append(Device(index=a_idx, name=name, kind="audio"))
            a_idx += 1
    return video, audio


def _resolve_dshow_kind(marker: str | None, section: str | None) -> set[str]:
    """Translate a dshow kind marker (or legacy section) to a set of kinds.

    Modern FFmpeg emits one of: ``video``, ``audio``, ``audio, video``
    (the combined-input capture variant — Blackmagic WDM, some Magewell
    cards), ``video, audio`` (same, reversed), or ``none`` (FFmpeg
    couldn't determine the device type — OBS Virtual Camera and some
    NDI inputs report this way). We map combined markers into BOTH
    lists so the device is selectable as either input. ``none`` is
    treated as video on the practical observation that virtually every
    real-world ``(none)`` device in dshow listings is a virtual camera.
    """
    if marker is None:
        return {section} if section in ("video", "audio") else set()
    parts = {p.strip().lower() for p in marker.split(",")}
    kinds: set[str] = set()
    if "video" in parts:
        kinds.add("video")
    if "audio" in parts:
        kinds.add("audio")
    if "none" in parts and not kinds:
        kinds.add("video")
    return kinds


# ---------------------------------------------------------------------------
# AVFoundation mode probe — find what (width, height, fps) the device
# actually supports so the source factory can pick a real mode instead
# of asking for one the device will reject.
# ---------------------------------------------------------------------------


_AVF_MODE_LINE = re.compile(
    r"(\d+)x(\d+)@\[([\d.]+)\s+([\d.]+)\]fps"
)


def probe_avf_modes(device_index: int) -> list[tuple[int, int, float, float]]:
    """Return supported [(width, height, fps_min, fps_max), ...] for an
    AVFoundation device.

    Provokes FFmpeg into listing supported modes by asking for a
    framerate the device will refuse (1 fps); FFmpeg responds by
    enumerating the modes it would accept. Returns ``[]`` on parse
    failure or timeout — caller should fall back to the user's
    requested format and let FFmpeg report the error.

    Locked-mode virtual cameras (NDI Virtual Camera, OBS Virtual
    Camera in some configs) only support a single mode; without this
    probe we'd hard-code the destination format and fail with
    "Selected framerate is not supported by the device."
    """
    from .ffmpeg_path import ffmpeg_path
    try:
        proc = subprocess.run(
            [ffmpeg_path(), "-hide_banner", "-f", "avfoundation",
             "-framerate", "1", "-i", str(device_index),
             "-t", "0", "-f", "null", "-"],
            capture_output=True, text=True, timeout=5,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError) as exc:
        log.warning("AVF mode probe failed for device %d: %s", device_index, exc)
        return []

    modes: list[tuple[int, int, float, float]] = []
    for line in proc.stderr.splitlines():
        m = _AVF_MODE_LINE.search(line)
        if m:
            modes.append((
                int(m.group(1)), int(m.group(2)),
                float(m.group(3)), float(m.group(4)),
            ))
    return modes


def pick_best_avf_mode(
    modes: list[tuple[int, int, float, float]],
    want_w: int, want_h: int, want_fps: float,
) -> tuple[int, int, float]:
    """Pick (width, height, fps) closest to the desired output format.

    Strategy:
      1. Prefer modes matching the requested resolution exactly.
      2. Within those, prefer modes whose fps range covers the request.
      3. Otherwise pick the mode with the closest fps endpoint.
      4. If we have to compromise on fps, use the mode's max fps —
         high-rate input downsampled at the encoder is cleaner than
         low-rate input upsampled.

    Returns the user's request unmodified if ``modes`` is empty so
    the caller falls back gracefully.
    """
    if not modes:
        return (want_w, want_h, float(want_fps))

    matching = [m for m in modes if m[0] == want_w and m[1] == want_h]
    pool = matching if matching else modes

    def score(m: tuple[int, int, float, float]) -> tuple[int, float]:
        w, h, fps_lo, fps_hi = m
        if fps_lo <= want_fps <= fps_hi:
            return (0, 0.0)  # exact-fps fit beats everything
        return (1, min(abs(fps_lo - want_fps), abs(fps_hi - want_fps)))

    best = min(pool, key=score)
    w, h, fps_lo, fps_hi = best
    chosen_fps = float(want_fps) if fps_lo <= want_fps <= fps_hi else fps_hi
    return (w, h, chosen_fps)


# ---------------------------------------------------------------------------
# Default-device heuristics — used at boot to pick something sensible
# ---------------------------------------------------------------------------


def find_default_audio_index(devices: DeviceList) -> int:
    """Pick a sensible default audio device.

    Preference order (avoiding virtual / aggregate devices):
      MacBook Pro Microphone → built-in microphone → first non-virtual → 0
    """
    name_priorities = [
        re.compile(r"macbook.*microphone", re.I),
        re.compile(r"built.*microphone", re.I),
        re.compile(r"^microphone$", re.I),
    ]
    for pat in name_priorities:
        for d in devices.audio:
            if pat.search(d.name):
                return d.index
    # Skip names that look virtual-only.
    skip = re.compile(r"(zoom|ndi audio|aggregate|dante|virtual|stereo mix)", re.I)
    for d in devices.audio:
        if not skip.search(d.name):
            return d.index
    return devices.audio[0].index if devices.audio else -1


def find_default_video_index(devices: DeviceList) -> int:
    """Prefer FaceTime HD / a real webcam over virtual cameras and screens."""
    for d in devices.video:
        if "facetime" in d.name.lower():
            return d.index
    skip = re.compile(r"(virtual|capture screen|desk view|screen capture)", re.I)
    for d in devices.video:
        if not skip.search(d.name):
            return d.index
    return devices.video[0].index if devices.video else 0
