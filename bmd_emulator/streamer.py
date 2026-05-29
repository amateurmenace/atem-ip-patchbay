"""FFmpeg streamer — orchestrates encoding and SRT delivery.

The encoder profile is chosen carefully to match what real Blackmagic
encoders emit, because Blackmagic decoders (Web Presenter HD/4K, ATEM
Streaming Bridge) reject streams that deviate:

  - H.264 Main profile only (Baseline and High both reported to fail)
  - yuv420p
  - No B-frames
  - Fixed GOP = keyframe_interval × fps
  - AAC-LC, 48 kHz, stereo, 128 kbps
  - MPEG-TS container
  - SRT in caller mode, latency 500 ms
  - streamid in `#!::r=...,m=publish,bmd_uuid=...,bmd_name=...` form
"""

from __future__ import annotations

import logging
import re
import shlex
import subprocess
import threading
import time
from dataclasses import dataclass

from .sources import Source
from .state import EncoderState
from .streamid import build_srt_url, parse_srt_host_port

log = logging.getLogger(__name__)


@dataclass
class StreamPlan:
    """Resolved parameters for one streaming session."""

    width: int
    height: int
    fps: int
    video_bitrate: int
    audio_bitrate: int
    keyframe_seconds: int
    output_url: str
    protocol: str  # "srt" or "rtmp"
    source: Source
    video_codec: str = "h264"  # "h264" or "h265"

    @property
    def gop(self) -> int:
        return max(1, self.keyframe_seconds * self.fps)


# Match FFmpeg's progress/status lines:
#   frame=  120 fps=30 q=35.9 size=  1234KiB time=00:00:04.00 bitrate=6027.4kbits/s dup=0 drop=0 speed=1.02x
_BITRATE_RE = re.compile(r"bitrate=\s*([\d.]+)\s*kbits/s")
_FRAME_RE   = re.compile(r"frame=\s*(\d+)")
_FPS_RE     = re.compile(r"fps=\s*([\d.]+)")
_SPEED_RE   = re.compile(r"speed=\s*([\d.]+)x")
_DROP_RE    = re.compile(r"drop=\s*(\d+)")
_QUAL_RE    = re.compile(r"\bq=\s*([\d.]+)")


class Streamer:
    """Manages the FFmpeg subprocess lifecycle for one encoder."""

    def __init__(self, state: EncoderState):
        self.state = state
        self._proc: subprocess.Popen | None = None
        self._thread: threading.Thread | None = None
        self._stop_requested = False
        self._lock = threading.RLock()
        self._last_command: list[str] = []
        self._last_stderr_lines: list[str] = []

    # ---- public api --------------------------------------------------------

    def is_running(self) -> bool:
        with self._lock:
            return self._proc is not None and self._proc.poll() is None

    def last_command(self) -> str:
        return shlex.join(self._last_command) if self._last_command else ""

    def last_log_tail(self, lines: int = 30) -> list[str]:
        with self._lock:
            return list(self._last_stderr_lines[-lines:])

    def build_plan(self) -> StreamPlan:
        from .sources import resolve_source

        snap = self.state.snapshot()
        if snap["active_config"] is None:
            raise RuntimeError(
                "No streaming profile loaded. Add a service XML first."
            )
        base_url = snap.get("current_url", "")
        protocol = (snap.get("current_protocol") or "").lower()
        if not base_url or not protocol:
            raise RuntimeError(
                "No server selected. Pick an RTMP or SRT server in the UI."
            )
        if not snap["stream_key"]:
            raise RuntimeError("Stream key is empty.")

        cfg = snap["active_config"]
        width, height, fps = self.state.video_dimensions()

        if protocol == "srt":
            host, port = parse_srt_host_port(base_url, default_port=1935)
            output_url = build_srt_url(
                host=host,
                port=port,
                stream_key=snap["stream_key"],
                device_name=snap["label"],
                device_uuid=snap["device_uuid"],
                passphrase=snap["passphrase"] or None,
                mode=snap.get("srt_mode") or "caller",
                latency_us=int(snap.get("srt_latency_us") or 500_000),
                streamid_override=snap.get("streamid_override") or "",
                listen_port=int(snap.get("srt_listen_port") or 9710),
                legacy_streamid=bool(snap.get("streamid_legacy", False)),
            )
        elif protocol in ("rtmp", "rtmps"):
            output_url = self._build_rtmp_url(base_url, snap["stream_key"])
        else:
            raise RuntimeError(f"Unsupported protocol: {protocol!r}")

        source = resolve_source(self.state)
        return StreamPlan(
            width=width,
            height=height,
            fps=fps,
            video_bitrate=cfg["bitrate"],
            audio_bitrate=cfg["audio_bitrate"],
            keyframe_seconds=cfg["keyframe_interval"],
            output_url=output_url,
            protocol=protocol,
            source=source,
            video_codec=(snap.get("video_codec") or "h264").lower(),
        )

    @staticmethod
    def _escape_drawtext(text: str) -> str:
        """Escape characters that drawtext treats specially.

        drawtext text= field needs : ' \\ % escaped, and we strip
        commas/quotes that would otherwise break -filter_complex parsing.
        """
        # Single quotes are deadly inside the single-quoted text='...' wrapper.
        # Replace with the curly equivalent so they render visibly.
        text = text.replace("'", "’")
        text = text.replace("\\", r"\\\\")
        text = text.replace(":", r"\:")
        text = text.replace("%", r"\%")
        text = text.replace(",", r"\,")
        return text

    def _build_video_filter(
        self,
        *,
        v_in: str,
        width: int,
        height: int,
        overlay: dict,
        logo_input_index: int | None,
    ) -> tuple[str, str]:
        """Compose -filter_complex for the requested overlays.

        Returns (filter_complex_string, output_label). If no overlays
        are needed, returns ("", "") so the caller can fall back to a
        plain stream copy.
        """
        title = (overlay.get("title") or "").strip()
        subtitle = (overlay.get("subtitle") or "").strip()
        clock = bool(overlay.get("clock"))
        has_logo = logo_input_index is not None

        if not (title or subtitle or clock or has_logo):
            return "", ""

        # We chain filters and number the labels v0 → v1 → v2 …
        steps: list[str] = []
        current = v_in  # starting label e.g. "0:v:0"
        next_label_idx = 0

        def fresh() -> str:
            nonlocal next_label_idx
            label = f"v{next_label_idx}"
            next_label_idx += 1
            return label

        # We need a known frame size for placement math. If the source
        # is AVFoundation it may not match the requested width/height
        # exactly (camera native size + scale). Force a scale + setsar
        # at the start so all overlays compute against `width`x`height`.
        out = fresh()
        steps.append(
            f"[{current}]scale={width}:{height}:force_original_aspect_ratio=decrease,"
            f"pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1,format=yuv420p[{out}]"
        )
        current = out

        # Title — top-center, large bold sans, semi-transparent backdrop.
        if title:
            esc = self._escape_drawtext(title)
            out = fresh()
            steps.append(
                f"[{current}]drawtext=text='{esc}':"
                f"fontcolor=white:fontsize={max(18, height // 22)}:"
                f"x=(w-text_w)/2:y=h*0.05:"
                f"box=1:boxcolor=black@0.55:boxborderw=14[{out}]"
            )
            current = out

        # Subtitle — directly under the title (or top-center if no title).
        if subtitle:
            esc = self._escape_drawtext(subtitle)
            y_expr = f"h*0.05+{max(18, height // 22) + 26}" if title else "h*0.06"
            out = fresh()
            steps.append(
                f"[{current}]drawtext=text='{esc}':"
                f"fontcolor=white:fontsize={max(14, height // 32)}:"
                f"x=(w-text_w)/2:y={y_expr}:"
                f"box=1:boxcolor=black@0.45:boxborderw=10[{out}]"
            )
            current = out

        # Clock — bottom-left, monospace, ticks every second.
        if clock:
            out = fresh()
            steps.append(
                f"[{current}]drawtext=text='%{{localtime\\:%H\\\\\\:%M\\\\\\:%S}}':"
                f"fontcolor=white:fontsize={max(14, height // 32)}:"
                f"x=20:y=h-th-20:"
                f"box=1:boxcolor=black@0.55:boxborderw=10[{out}]"
            )
            current = out

        # Logo — top-right, scaled to ~12% of frame height, with padding.
        if has_logo:
            logo_h = max(48, height // 8)
            logo_label = fresh()
            steps.append(
                f"[{logo_input_index}:v]scale=-1:{logo_h}[{logo_label}]"
            )
            out = fresh()
            steps.append(
                f"[{current}][{logo_label}]overlay=W-w-20:20[{out}]"
            )
            current = out

        return ";".join(steps), current

    @staticmethod
    def _build_rtmp_url(base_url: str, stream_key: str) -> str:
        """Append the stream key as the final path segment unless already
        present. BMD's XML stores `rtmp://host:port/app` and the encoder
        appends `/<key>` at publish time.
        """
        url = base_url.rstrip("/")
        if stream_key and not url.endswith("/" + stream_key):
            url = f"{url}/{stream_key}"
        return url

    def start(self) -> None:
        with self._lock:
            if self.is_running():
                raise RuntimeError("Stream already running.")

            plan = self.build_plan()
            if not plan.source.available:
                raise RuntimeError(
                    f"Source '{plan.source.label}' is not yet implemented. "
                    f"{plan.source.notes}"
                )

            cmd = self._build_ffmpeg_cmd(plan)
            self._last_command = cmd
            self._last_stderr_lines = []
            self._stop_requested = False

            log.info("Launching FFmpeg: %s", shlex.join(cmd))
            self._proc = subprocess.Popen(
                cmd,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
                bufsize=1,
            )

            self.state.stats.status = "Connecting"
            self.state.stats.error = None
            self.state.stats.started_at = time.time()
            self.state.stats.bitrate = 0
            self.state.stats.fps = 0.0
            self.state.stats.speed = 0.0
            self.state.stats.frames_sent = 0
            self.state.stats.frames_dropped = 0
            self.state.stats.quality = 0.0

            self._thread = threading.Thread(
                target=self._monitor, name="ffmpeg-monitor", daemon=True
            )
            self._thread.start()

    def stop(self, timeout: float = 5.0) -> None:
        with self._lock:
            self._stop_requested = True
            proc = self._proc
            if proc is None:
                return
        if proc.poll() is None:
            try:
                proc.terminate()
                proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=2.0)
        with self._lock:
            self.state.stats.status = "Idle"
            self.state.stats.started_at = None
            self.state.stats.bitrate = 0
            self._proc = None

    # ---- internals --------------------------------------------------------

    def _build_ffmpeg_cmd(self, plan: StreamPlan) -> list[str]:
        gop = plan.gop
        from .ffmpeg_path import ffmpeg_path
        cmd: list[str] = [ffmpeg_path(), "-hide_banner", "-loglevel", "info"]
        cmd.extend(plan.source.ffmpeg_input_args)

        # Build optional logo input (overlay #2) and the filter graph.
        snap = self.state.snapshot()
        overlay = snap.get("overlay") or {}
        logo_path = overlay.get("logo_path") or ""
        has_logo = bool(logo_path)
        if has_logo:
            # Loop the logo as a static image source.
            cmd += ["-loop", "1", "-i", logo_path]

        # Decide stream mapping based on whether the source provides
        # combined A/V (avfoundation, pipe) or separate (test_pattern).
        if plan.source.combined_av:
            v_in, a_in = "0:v:0", "0:a:0"
        else:
            v_in, a_in = "0:v:0", "1:a:0"

        filter_chain, video_out_label = self._build_video_filter(
            v_in=v_in,
            width=plan.width,
            height=plan.height,
            overlay=overlay,
            logo_input_index=(2 if (has_logo and not plan.source.combined_av) else (1 if has_logo else None)),
        )
        if filter_chain:
            cmd += ["-filter_complex", filter_chain]
            cmd += ["-map", f"[{video_out_label}]", "-map", a_in]
        else:
            cmd += ["-map", v_in, "-map", a_in]

        # ---- video encoder: Main profile, no B-frames, fixed GOP ----
        # Constraints come from what BMD decoders accept; both RTMP and
        # SRT receivers tolerate or require them.
        # Codec selection — H.264 is widely compatible (standalone Streaming
        # Bridge, mediamtx, RTMP receivers). H.265 matches what real BMD
        # WPs actually send to ATEM Mini built-in Streaming Bridge mode
        # (per pcap analysis).
        if plan.video_codec == "h265":
            cmd += [
                "-c:v", "libx265",
                "-profile:v", "main",
                "-preset", "veryfast",
                "-tune", "zerolatency",
                "-pix_fmt", "yuv420p",
                "-x265-params",
                (
                    f"bframes=0:no-scenecut=1:keyint={gop}:min-keyint={gop}:"
                    f"vbv-maxrate={plan.video_bitrate // 1000}:"
                    f"vbv-bufsize={plan.video_bitrate // 1000}:"
                    f"repeat-headers=1:hrd=1:log-level=warning"
                ),
                "-b:v", str(plan.video_bitrate),
                "-g", str(gop),
                "-keyint_min", str(gop),
                "-sc_threshold", "0",
                "-r", str(plan.fps),
            ]
        else:
            cmd += [
                "-c:v", "libx264",
                "-profile:v", "main",
                "-preset", "veryfast",
                "-tune", "zerolatency",
                "-pix_fmt", "yuv420p",
                "-x264-params",
                f"bframes=0:scenecut=0:keyint={gop}:min-keyint={gop}:nal-hrd=cbr",
                "-b:v", str(plan.video_bitrate),
                "-maxrate", str(plan.video_bitrate),
                "-minrate", str(plan.video_bitrate),
                "-bufsize", str(plan.video_bitrate),
                "-g", str(gop),
                "-keyint_min", str(gop),
                "-sc_threshold", "0",
                "-r", str(plan.fps),
            ]

        # ---- audio encoder: AAC-LC 48k stereo ----
        cmd += [
            "-c:a", "aac",
            "-b:a", str(plan.audio_bitrate),
            "-ar", "48000",
            "-ac", "2",
        ]

        # ---- container + transport ----
        if plan.protocol == "srt":
            cmd += [
                "-f", "mpegts",
                "-mpegts_flags", "+resend_headers",
                "-flush_packets", "1",
            ]
        elif plan.protocol == "rtmp":
            # RTMP wants FLV. flvflags=no_duration_filesize keeps it
            # stream-shaped (no end-of-file metadata). +global_header so
            # SPS/PPS travel out-of-band.
            cmd += [
                "-flvflags", "no_duration_filesize",
                "-f", "flv",
            ]
        else:
            raise RuntimeError(f"Unsupported protocol: {plan.protocol!r}")

        cmd.append(plan.output_url)
        return cmd

    def _monitor(self) -> None:
        """Read FFmpeg stderr, parse status lines, update stats."""
        proc = self._proc
        if proc is None or proc.stderr is None:
            return
        connecting_seen = False
        for raw_line in proc.stderr:
            line = raw_line.rstrip()
            with self._lock:
                self._last_stderr_lines.append(line)
                if len(self._last_stderr_lines) > 500:
                    self._last_stderr_lines = self._last_stderr_lines[-500:]

            lower = line.lower()
            if not connecting_seen and (
                "opening 'srt://" in lower or "srt://" in lower
            ):
                connecting_seen = True
            if "connection established" in lower or "stream mapping" in lower:
                with self._lock:
                    self.state.stats.status = "Streaming"

            # Parse the whole stats line — FFmpeg emits every ~0.5s so we
            # update telemetry roughly twice per second.
            m_br = _BITRATE_RE.search(line)
            m_fr = _FRAME_RE.search(line)
            m_fp = _FPS_RE.search(line)
            m_sp = _SPEED_RE.search(line)
            m_dp = _DROP_RE.search(line)
            m_q  = _QUAL_RE.search(line)
            if m_br or m_fr or m_fp or m_sp:
                with self._lock:
                    if self.state.stats.status != "Streaming":
                        self.state.stats.status = "Streaming"
                    if m_br:
                        try: self.state.stats.bitrate = int(float(m_br.group(1)) * 1000)
                        except ValueError: pass
                    if m_fr:
                        try: self.state.stats.frames_sent = int(m_fr.group(1))
                        except ValueError: pass
                    if m_fp:
                        try: self.state.stats.fps = float(m_fp.group(1))
                        except ValueError: pass
                    if m_sp:
                        try: self.state.stats.speed = float(m_sp.group(1))
                        except ValueError: pass
                    if m_dp:
                        try: self.state.stats.frames_dropped = int(m_dp.group(1))
                        except ValueError: pass
                    if m_q:
                        try: self.state.stats.quality = float(m_q.group(1))
                        except ValueError: pass

            # Heuristic error detection.
            if any(
                tag in lower
                for tag in (
                    "connection refused",
                    "connection setup failure",
                    "operation timed out",
                    "no route to host",
                    "srt error",
                    "protocol not found",
                )
            ):
                with self._lock:
                    self.state.stats.status = "Interrupted"
                    self.state.stats.error = line

        # Process exited.
        rc = proc.wait()
        with self._lock:
            if self._stop_requested:
                self.state.stats.status = "Idle"
            elif rc != 0:
                self.state.stats.status = "Interrupted"
                if not self.state.stats.error:
                    tail = " | ".join(self._last_stderr_lines[-3:])
                    self.state.stats.error = (
                        f"FFmpeg exited with code {rc}: {tail}"
                    )
            else:
                self.state.stats.status = "Idle"
            self.state.stats.started_at = None
            self.state.stats.bitrate = 0
            self._proc = None
