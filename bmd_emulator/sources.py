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
    video_name: str = "",
    audio_name: str = "",
    label: str = "Camera",
    description: str = "",
) -> Source:
    """Build an AVFoundation capture source (macOS).

    AVFoundation's ``-i`` token addresses devices either by integer
    index (``"2:3"``) or by string name (``"NDI Virtual Camera:NDI
    Audio"``). Names are stable across rescans; indices reshuffle
    silently when devices come and go (FaceTime can be index 0 on
    one scan and index 2 on the next, depending on what else is
    plugged in). We pass names whenever they're set in state, falling
    back to indices only for legacy callers.

    Probes the device for supported (width, height, fps) modes before
    building the command — locked-mode virtual cameras (NDI Virtual
    Camera in particular, only outputs 1080p60) reject any framerate
    they don't natively support. The probe lets us request a real
    mode, then the encoder's ``-r {fps}`` plus the scale filter
    converts to the destination format on the way out.
    """
    from .device_scanner import probe_avf_modes, pick_best_avf_mode

    # Resolve to a current index for the probe (probe needs a positional
    # index; the actual capture command can use the name).
    probe_index = _resolve_avf_index_for_probe(video_name, video_index)
    modes = probe_avf_modes(probe_index)
    actual_w, actual_h, actual_fps = pick_best_avf_mode(modes, width, height, fps)
    actual_size = f"{actual_w}x{actual_h}"
    fps_str = f"{int(actual_fps)}" if actual_fps == int(actual_fps) else f"{actual_fps:.3f}"

    note = ""
    if modes and (actual_w, actual_h) != (width, height):
        note = (
            f"Capturing at {actual_size}@{fps_str}fps (device's native mode); "
            f"encoder will scale to {width}x{height}@{fps}fps for the destination."
        )
    elif modes and actual_fps != fps:
        note = (
            f"Capturing at {fps_str}fps (device-supported); "
            f"encoder will retime to {fps}fps for the destination."
        )

    # AVFoundation token: prefer "name:name" over "index:index".
    if video_name:
        token = f"{video_name}:{audio_name}" if audio_name else video_name
    else:
        token = f"{video_index}:{audio_index}"

    return Source(
        id="avfoundation",
        label=label or "AVFoundation",
        description=description or f"AVFoundation {actual_size}@{fps_str}fps → {width}x{height}@{fps}fps",
        ffmpeg_input_args=[
            "-f", "avfoundation",
            "-framerate", fps_str,
            "-video_size", actual_size,
            "-pixel_format", "uyvy422",
            "-capture_cursor", "1",
            "-i", token,
        ],
        combined_av=True,
        notes=note,
    )


def _resolve_avf_index_for_probe(name: str, fallback_index: int) -> int:
    """Look up the current AVFoundation index for a device by name.

    Scans the live device list (forced refresh) and returns the index
    matching the given name. Falls back to ``fallback_index`` if the
    name isn't found — keeps the probe working when only legacy
    index-based state is set.
    """
    if not name:
        return fallback_index
    from .device_scanner import list_capture_devices
    devs = list_capture_devices(force=True)
    for d in devs.video:
        if d.name == name:
            return d.index
    return fallback_index


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
    # See the avfoundation() factory above for why we don't pin
    # -framerate / -video_size: forcing those breaks locked-mode
    # virtual cameras (the same NDI Virtual Camera bug exists on
    # the dshow side). The encoder retimes/scales to the destination
    # format on the way out.
    if audio_name:
        return Source(
            id="avfoundation",  # see module docstring — same UI ID, different backend
            label=label or "DirectShow",
            description=description or f"DirectShow capture (native rate) → {width}x{height} @ {fps}fps",
            ffmpeg_input_args=[
                "-f", "dshow",
                "-rtbufsize", "256M",
                "-i", f'video={video_name}:audio={audio_name}',
            ],
            combined_av=True,
        )
    return Source(
        id="avfoundation",
        label=label or "DirectShow",
        description=description or f"DirectShow video (native rate, silent) → {width}x{height} @ {fps}fps",
        ffmpeg_input_args=[
            "-f", "dshow",
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


def srt_listen(
    bind_host: str = "0.0.0.0",
    bind_port: int = 9710,
    latency_us: int = 200_000,
    passphrase: str = "",
) -> Source:
    """Bind an SRT listener and ingest the first publisher that connects.

    The publisher (OBS, Larix Broadcaster, an iPhone, another FFmpeg)
    points its caller-mode SRT URL at ``srt://<this-machine>:<port>``
    and pushes an MPEG-TS stream. We re-encode it through the normal
    BMD pipeline before forwarding to the ATEM destination.
    """
    url = f"srt://{bind_host}:{bind_port}?mode=listener&latency={latency_us}"
    if passphrase:
        url += f"&passphrase={passphrase}"
    return Source(
        id="srt_listen",
        label=f"SRT in :{bind_port}",
        description=f"SRT listener on {bind_host}:{bind_port}",
        ffmpeg_input_args=[
            "-f", "mpegts",
            "-i", url,
        ],
        combined_av=True,
        notes=(
            f"Point your encoder at srt://<this-machine-ip>:{bind_port} "
            "in caller mode. MPEG-TS is auto-detected; H.264 / H.265 / "
            "AAC payloads all work — they get re-encoded into BMD's "
            "preferred profile on the way out."
        ),
    )


def rtmp_listen(
    bind_host: str = "0.0.0.0",
    bind_port: int = 1935,
    app_path: str = "live",
    stream_name: str = "stream",
) -> Source:
    """Bind an RTMP server and accept the first publisher.

    FFmpeg's RTMP listener (``-listen 1``) accepts any incoming
    publish on the given port. The app/stream path components in the
    URL are placeholders — most clients (OBS et al.) just need a URL
    they can push to and any matching key.
    """
    url = f"rtmp://{bind_host}:{bind_port}/{app_path}/{stream_name}"
    return Source(
        id="rtmp_listen",
        label=f"RTMP in :{bind_port}",
        description=f"RTMP listener on {bind_host}:{bind_port}/{app_path}",
        ffmpeg_input_args=[
            "-listen", "1",
            "-f", "flv",
            "-i", url,
        ],
        combined_av=True,
        notes=(
            f"In OBS: Settings → Stream → Custom, Server "
            f"rtmp://<this-machine-ip>:{bind_port}/{app_path}, Key {stream_name}. "
            "FLV/H.264/AAC. Re-encoded into BMD's preferred profile downstream."
        ),
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

    if sid == "srt_listen":
        return srt_listen(
            bind_host=state.relay_bind_host,
            bind_port=state.relay_srt_port,
            latency_us=state.relay_srt_latency_us,
            passphrase=state.relay_srt_passphrase,
        )

    if sid == "rtmp_listen":
        return rtmp_listen(
            bind_host=state.relay_bind_host,
            bind_port=state.relay_rtmp_port,
            app_path=state.relay_rtmp_app,
            stream_name=state.relay_rtmp_key,
        )

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
        # Prefer state-stored names (stable across rescans) over indices.
        # If state.av_video_name is set, use it; else use the name we
        # just looked up from the cached device list.
        v_name_final = state.av_video_name or v_name
        a_name_final = state.av_audio_name or (a_name if state.av_audio_index >= 0 else "")
        return avfoundation(
            width=width,
            height=height,
            fps=fps,
            video_index=state.av_video_index,
            audio_index=state.av_audio_index,
            video_name=v_name_final if not v_name_final.startswith("video[") else "",
            audio_name=a_name_final if not a_name_final.startswith("audio[") else "",
            label=v_name_final,
            description=f"{v_name_final} + {a_name_final or 'no audio'} @ {width}x{height}/{fps}",
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
