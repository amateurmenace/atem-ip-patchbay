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

# Matches a dshow device line: `[dshow @ 0x...]  "Name"` with an optional
# trailing `(video)` / `(audio)` marker on older FFmpeg builds.
_DSHOW_DEVICE_LINE = re.compile(
    r'\[dshow @ [^\]]+\]\s+"([^"]+)"(?:\s+\((video|audio)\))?\s*$'
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
    ("capture_card", re.compile(r"ultrastudio|decklink|intensity|aja|magewell|elgato|epiphan", re.I)),
    ("ndi", re.compile(r"\bndi\b", re.I)),
    ("iphone", re.compile(r"iphone|ipad", re.I)),
    ("virtual", re.compile(r"virtual|obs|sysram|loopback|syphon", re.I)),
]

_AUDIO_CATEGORY_PATTERNS: list[tuple[str, re.Pattern]] = [
    ("ndi", re.compile(r"\bndi\b", re.I)),
    ("virtual", re.compile(r"virtual|loopback|aggregate|blackhole|soundflower|background music|stereo mix", re.I)),
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
        # Skip "Alternative name" lines — they reference the same device
        # via its `@device_pnp_...` GUID and we don't need that.
        if "alternative name" in low:
            continue
        m = _DSHOW_DEVICE_LINE.search(line)
        if not m:
            continue
        name = m.group(1).strip()
        kind_marker = m.group(2)  # "video" / "audio" / None
        kind = kind_marker or section
        if kind == "video":
            video.append(Device(index=v_idx, name=name, kind="video"))
            v_idx += 1
        elif kind == "audio":
            audio.append(Device(index=a_idx, name=name, kind="audio"))
            a_idx += 1
    return video, audio


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
