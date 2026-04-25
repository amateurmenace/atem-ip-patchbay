"""Video/audio source definitions for the emulator.

Sources are produced from the live `EncoderState` so the FFmpeg input
arguments reflect the user's current selection (resolution, framerate,
chosen capture-device indices, etc).

Source IDs (the values stored in ``state.source_id``):

  - ``test_pattern`` — SMPTE-style bars + 1 kHz tone via lavfi.
  - ``avfoundation`` — capture from a system-native A/V device. The
    name is historical (the Mac path uses AVFoundation); on Windows
    the same source ID dispatches to DirectShow (dshow) or, for screen
    captures, gdigrab. See ``resolve_source`` for the platform fork.
  - ``pipe`` — a named pipe / file path / arbitrary URL that produces
    an MPEG-TS or other FFmpeg-readable stream.

The UI treats ``source_id`` as an opaque token, so adding Windows
backends did not require a new ID — the dispatch is server-side.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass, field


@dataclass
class Source:
    """A resolved source.

    `ffmpeg_input_args` is a list of CLI tokens inserted before output
    options. It must produce one video stream and one audio stream
    that the encoder maps as `0:v:0` + `1:a:0` (or `0:v:0` + `0:a:0`
    for combined-stream sources like avfoundation / dshow combined).
    """

    id: str
    label: str
    description: str
    ffmpeg_input_args: list[str] = field(default_factory=list)
    available: bool = True
    notes: str = ""
    # When True, video and audio share input #0 and we map 0:v + 0:a.
    combined_av: bool = False


# ---------------------------------------------------------------------------
# Source factories
# ---------------------------------------------------------------------------


def test_pattern(width: int, height: int, fps: int) -> Source:
    """SMPTE-style colour bars with a 1 kHz tone, both via lavfi."""
    size = f"{width}x{height}"
    return Source(
        id="test_pattern",
        label="Test Pattern",
        description=f"SMPTE bars {size} @ {fps}fps + 1 kHz tone",
        ffmpeg_input_args=[
            "-re",
            "-f", "lavfi",
            "-i", f"testsrc2=size={size}:rate={fps},format=yuv420p",
            "-f", "lavfi",
            "-i", "sine=frequency=1000:sample_rate=48000",
        ],
        combined_av=False,
    )


def avfoundation(
    width: int,
    height: int,
    fps: int,
    video_index: int,
    audio_index: int,
    label: str = "Camera",
    description: str = "",
) -> Source:
    """Build an AVFoundation capture source (macOS).

    AVFoundation's capture engine takes a single ``-i "V:A"`` token
    where V and A are the integer device indices. Audio and video
    share input #0.

    NDI Virtual Camera + NDI Audio (from NDI Tools) work transparently
    here — the OS exposes them like any other capture device.
    """
    size = f"{width}x{height}"
    return Source(
        id="avfoundation",
        label=label or "AVFoundation",
        description=description or f"AVFoundation capture {size} @ {fps}fps",
        ffmpeg_input_args=[
            "-f", "avfoundation",
            "-framerate", str(fps),
            "-video_size", size,
            "-pixel_format", "uyvy422",
            "-capture_cursor", "1",
            "-i", f"{video_index}:{audio_index}",
        ],
        combined_av=True,
    )


def dshow(
    width: int,
    height: int,
    fps: int,
    video_name: str,
    audio_name: str = "",
    label: str = "Camera",
    description: str = "",
) -> Source:
    """Build a DirectShow capture source (Windows).

    dshow accepts a combined ``video=<name>:audio=<name>`` token,
    which is the form real BMD encoders' Windows tooling uses. We
    keep `combined_av=True` so the streamer's mapping logic
    (``0:v + 0:a``) applies cleanly.

    If ``audio_name`` is empty (no audio device configured), pair
    with a silent lavfi source as a separate input — that flips the
    mapping back to ``0:v + 1:a`` (combined_av=False). Either way the
    rest of the pipeline is unchanged.
    """
    size = f"{width}x{height}"
    if audio_name:
        return Source(
            id="avfoundation",  # see module docstring — same UI ID, different backend
            label=label or "DirectShow",
            description=description or f"DirectShow capture {size} @ {fps}fps",
            ffmpeg_input_args=[
                "-f", "dshow",
                "-framerate", str(fps),
                "-video_size", size,
                "-rtbufsize", "256M",
                "-i", f'video={video_name}:audio={audio_name}',
            ],
            combined_av=True,
        )
    return Source(
        id="avfoundation",
        label=label or "DirectShow",
        description=description or f"DirectShow video {size} @ {fps}fps (silent)",
        ffmpeg_input_args=[
            "-f", "dshow",
            "-framerate", str(fps),
            "-video_size", size,
            "-rtbufsize", "256M",
            "-i", f'video={video_name}',
            "-f", "lavfi",
            "-i", "anullsrc=channel_layout=stereo:sample_rate=48000",
        ],
        combined_av=False,
    )


def gdigrab_desktop(
    width: int,
    height: int,
    fps: int,
    audio_name: str = "",
    label: str = "Screen capture",
    description: str = "",
) -> Source:
    """Capture the full Windows desktop via gdigrab.

    gdigrab is video-only, so audio comes from a paired dshow input
    when one is selected, or a silent lavfi source otherwise. Either
    way this is a two-input source, so ``combined_av=False``.
    """
    size = f"{width}x{height}"
    cmd = [
        "-f", "gdigrab",
        "-framerate", str(fps),
        "-video_size", size,
        "-i", "desktop",
    ]
    if audio_name:
        cmd.extend([
            "-f", "dshow",
            "-rtbufsize", "256M",
            "-i", f"audio={audio_name}",
        ])
    else:
        cmd.extend([
            "-f", "lavfi",
            "-i", "anullsrc=channel_layout=stereo:sample_rate=48000",
        ])
    return Source(
        id="avfoundation",
        label=label or "Screen capture",
        description=description or f"Desktop capture {size} @ {fps}fps",
        ffmpeg_input_args=cmd,
        combined_av=False,
    )


def gdigrab_title(
    width: int,
    height: int,
    fps: int,
    title: str,
    audio_name: str = "",
) -> Source:
    """Capture a specific Windows window by title (gdigrab).

    Not currently exposed in the UI — added for the source factory to
    feature parity with macOS's per-window selection. Wire to a tile
    in a future iteration.
    """
    size = f"{width}x{height}"
    cmd = [
        "-f", "gdigrab",
        "-framerate", str(fps),
        "-video_size", size,
        "-i", f"title={title}",
    ]
    if audio_name:
        cmd.extend([
            "-f", "dshow",
            "-rtbufsize", "256M",
            "-i", f"audio={audio_name}",
        ])
    else:
        cmd.extend([
            "-f", "lavfi",
            "-i", "anullsrc=channel_layout=stereo:sample_rate=48000",
        ])
    return Source(
        id="avfoundation",
        label=f"Window: {title}",
        description=f"Window capture {size} @ {fps}fps",
        ffmpeg_input_args=cmd,
        combined_av=False,
    )


def pipe(path: str) -> Source:
    """Read from a named pipe / file / arbitrary URL.

    Useful for bridging exotic sources: NDI via `gst-launch ... !
    mpegtsmux ! filesink location=/tmp/ndi.ts`, RTSP cameras, RTMP
    pull, etc. FFmpeg auto-detects the container.
    """
    return Source(
        id="pipe",
        label="Pipe / URL",
        description=f"Read from {path or '<unset>'}",
        ffmpeg_input_args=["-re", "-i", path] if path else [],
        available=bool(path),
        notes=(
            "Set the pipe path in the Source panel. Works with FIFOs, "
            "files, or any URL FFmpeg supports (rtsp://, http://, etc.)."
        ),
        combined_av=True,
    )


# ---------------------------------------------------------------------------
# Resolver — turn an EncoderState into a concrete Source
# ---------------------------------------------------------------------------


def resolve_source(state) -> Source:
    """Look at the live state and produce the right Source.

    Imported lazily inside Streamer to avoid a circular import.
    """
    width, height, fps = state.video_dimensions()
    sid = state.source_id

    if sid == "test_pattern":
        return test_pattern(width, height, fps)

    if sid == "pipe":
        return pipe(state.pipe_path)

    if sid == "avfoundation":
        return _resolve_capture_device(state, width, height, fps)

    raise ValueError(f"Unknown source id: {sid!r}")


def _resolve_capture_device(state, width: int, height: int, fps: int) -> Source:
    """Resolve a system-native capture device, dispatching by platform.

    macOS → ``avfoundation`` factory, indices fed straight to FFmpeg.
    Windows → ``dshow`` (or ``gdigrab`` for screen-category devices),
    with the chosen index resolved to a device name via the cached
    device list.
    """
    from .device_scanner import list_capture_devices

    devs = list_capture_devices()
    v_dev = next((d for d in devs.video if d.index == state.av_video_index), None)
    a_dev = next((d for d in devs.audio if d.index == state.av_audio_index), None)
    v_name = v_dev.name if v_dev else f"video[{state.av_video_index}]"
    a_name = a_dev.name if a_dev else (
        f"audio[{state.av_audio_index}]" if state.av_audio_index >= 0 else ""
    )

    if sys.platform == "darwin":
        return avfoundation(
            width=width,
            height=height,
            fps=fps,
            video_index=state.av_video_index,
            audio_index=state.av_audio_index,
            label=v_name,
            description=f"{v_name} + {a_name or 'no audio'} @ {width}x{height}/{fps}",
        )

    if sys.platform.startswith("win"):
        # Screen-category devices route through gdigrab instead of dshow —
        # dshow can't capture the desktop and Windows has no equivalent
        # of avfoundation's "Capture screen N" entries.
        if v_dev and v_dev.category == "screen":
            return gdigrab_desktop(
                width=width,
                height=height,
                fps=fps,
                audio_name=a_name,
                label=v_name,
                description=f"{v_name} + {a_name or 'no audio'} @ {width}x{height}/{fps}",
            )
        return dshow(
            width=width,
            height=height,
            fps=fps,
            video_name=v_name,
            audio_name=a_name,
            label=v_name,
            description=f"{v_name} + {a_name or 'no audio'} @ {width}x{height}/{fps}",
        )

    # Linux (and anything else) — no device backend yet; fall back to
    # the test pattern so the app stays usable instead of erroring.
    return test_pattern(width, height, fps)


# Backwards-compat shim — older callers may still use `get_source()`.
def get_source(source_id: str, width: int, height: int, fps: int) -> Source:
    if source_id == "test_pattern":
        return test_pattern(width, height, fps)
    raise ValueError(
        f"get_source() can't build {source_id!r} without state context. "
        f"Use resolve_source(state) instead."
    )
