"""Encoder state model.

A single `EncoderState` instance holds the configuration and runtime
state, mirroring the data model in the BMD Streaming Encoder Ethernet
Protocol (IDENTITY / STREAM SETTINGS / STREAM STATE / etc.).
"""

from __future__ import annotations

import threading
import time
import uuid
from dataclasses import dataclass, field

from .xml_loader import StreamService, load_service, load_service_text

# Resolutions the real Streaming Encoder supports — keep the same
# vocabulary for protocol compatibility.
AVAILABLE_VIDEO_MODES = [
    "Auto",
    "1080p23.98",
    "1080p24",
    "1080p25",
    "1080p29.97",
    "1080p30",
    "1080p50",
    "1080p59.94",
    "1080p60",
    "720p25",
    "720p30",
    "720p50",
    "720p60",
]

QUALITY_LEVELS = [
    "Streaming High",
    "Streaming Medium",
    "Streaming Low",
]


@dataclass
class StreamStats:
    status: str = "Idle"  # Idle | Connecting | Streaming | Interrupted
    bitrate: int = 0
    duration_seconds: float = 0.0
    cache_used_pct: int = 0
    started_at: float | None = None
    error: str | None = None

    # Real-time telemetry parsed from FFmpeg progress lines.
    fps: float = 0.0
    speed: float = 0.0          # encoder speed; 1.0 = realtime
    frames_sent: int = 0
    frames_dropped: int = 0
    quality: float = 0.0        # x264/x265 q (lower = better)

    def duration_string(self) -> str:
        if self.started_at is None:
            return "00:00:00:00"
        elapsed = max(0.0, time.time() - self.started_at)
        days = int(elapsed // 86400)
        hours = int((elapsed % 86400) // 3600)
        mins = int((elapsed % 3600) // 60)
        secs = int(elapsed % 60)
        return f"{days:02d}:{hours:02d}:{mins:02d}:{secs:02d}"


@dataclass
class EncoderState:
    label: str = "Blackmagic Streaming Encoder Emulator"
    model: str = "Blackmagic Streaming Encoder HD"
    unique_id: str = field(default_factory=lambda: uuid.uuid4().hex.upper())
    device_uuid: str = field(default_factory=lambda: str(uuid.uuid4()))

    video_mode: str = "1080p30"
    quality_level: str = "Streaming High"
    source_id: str = "test_pattern"

    # AVFoundation / DirectShow device selection (for source_id="avfoundation").
    # Indices were the original way to address devices, but AVFoundation
    # silently reshuffles them between rescans (FaceTime can be index 0
    # on one scan and index 2 on the next). Names are stable, so we
    # store both: name wins when set, index is the legacy fallback.
    av_video_index: int = 0
    av_audio_index: int = -1  # -1 means "auto-pick a built-in mic"
    av_video_name: str = ""
    av_audio_name: str = ""

    # Path/URL for source_id="pipe".
    pipe_path: str = ""

    # Relay-listener configuration (source_id="srt_listen" / "rtmp_listen").
    # The app binds a local SRT/RTMP server; an external encoder publishes
    # into it; we re-encode to BMD-flavored SRT and forward to the ATEM.
    relay_bind_host: str = "0.0.0.0"
    relay_srt_port: int = 9710
    relay_srt_latency_us: int = 200_000
    relay_srt_passphrase: str = ""
    relay_rtmp_port: int = 1935
    relay_rtmp_app: str = "live"
    relay_rtmp_key: str = "stream"

    # Overlay configuration (applied via filter_complex when streaming).
    overlay_title: str = ""
    overlay_subtitle: str = ""
    overlay_logo_path: str = ""
    overlay_clock: bool = False

    # Destination / service info.
    services: dict[str, StreamService] = field(default_factory=dict)
    current_service_name: str = ""
    current_server_name: str = "SRT"
    stream_key: str = ""
    passphrase: str = ""

    # When set, overrides whichever server is selected. Protocol is
    # auto-detected from the URL scheme. Useful for local testing
    # against an ad-hoc receiver, or for pointing at a relay that
    # isn't in the loaded XML.
    custom_url: str = ""

    # ---- SRT advanced ----
    # caller: we initiate (default — what real BMD encoders do)
    # listener: we bind & wait for the receiver to call into us
    # rendezvous: both sides initiate simultaneously (NAT traversal)
    srt_mode: str = "caller"
    srt_latency_us: int = 500_000
    srt_listen_port: int = 9710  # bind port when srt_mode == listener
    # If non-empty, used verbatim as the streamid (overrides BMD format).
    streamid_override: str = ""
    # When True, build streamid as `r=KEY,m=publish,bmd_uuid=...,bmd_name=...`
    # (the third-party convention). Default False uses the format the real
    # BMD encoders actually send: `bmd_uuid=...,bmd_name=...,u=KEY`.
    streamid_legacy: bool = False

    # ---- video codec ----
    # "h265" — libx265, Main profile (DEFAULT). What real BMD WPs and the
    #          iPhone Blackmagic Camera send to ATEM Mini built-in Streaming
    #          Bridge mode. Better compression at the same quality.
    # "h264" — libx264, Main profile. Use for RTMP receivers (rarely accept
    #          HEVC) or older standalone Streaming Bridge firmware.
    video_codec: str = "h265"

    stats: StreamStats = field(default_factory=StreamStats)
    lock: threading.RLock = field(default_factory=threading.RLock, repr=False)

    # ---- service / XML wiring ------------------------------------------------

    def add_service_from_xml(
        self, xml_path: str, *, make_active: bool | None = None
    ) -> StreamService:
        return self._register_service(load_service(xml_path), make_active=make_active)

    def add_service_from_xml_text(
        self, xml_text: str, *, make_active: bool = True
    ) -> StreamService:
        return self._register_service(load_service_text(xml_text), make_active=make_active)

    def _register_service(
        self, svc: StreamService, *, make_active: bool | None = None
    ) -> StreamService:
        """Add a service to the registry.

        `make_active`:
          - None  (default): only become current if no service is set yet
            — preserves boot-time "first XML wins" behavior.
          - True: always become current, overwriting whatever was selected
            — what UI imports want.
          - False: register but don't switch.
        """
        with self.lock:
            first_load = not self.current_service_name
            should_activate = make_active if make_active is not None else first_load
            self.services[svc.name] = svc
            if should_activate:
                self.current_service_name = svc.name
                self.stream_key = svc.key
                default_profile = svc.get_default_profile()
                if default_profile:
                    self.quality_level = default_profile.name
                srt = svc.srt_servers()
                if srt:
                    self.current_server_name = srt[0].name
                elif svc.servers:
                    self.current_server_name = svc.servers[0].name
                # UI imports clear the custom_url override so the new service
                # actually drives the destination.
                if make_active is True:
                    self.custom_url = ""
        return svc

    def current_service(self) -> StreamService | None:
        return self.services.get(self.current_service_name)

    def current_active_server(self) -> tuple[str, str, str] | None:
        """Return (name, url, protocol) for whichever destination is active.

        If `custom_url` is set, it wins over the XML selection — the
        protocol is detected from the URL scheme. Otherwise we follow
        `current_server_name` and fall back to the first SRT (or any)
        server in the XML.
        """
        if self.custom_url:
            scheme = self.custom_url.split("://", 1)[0].lower() if "://" in self.custom_url else ""
            if scheme in ("srt", "rtmp", "rtmps"):
                return ("Custom", self.custom_url, scheme)

        svc = self.current_service()
        if not svc or not svc.servers:
            return None
        for s in svc.servers:
            if s.name == self.current_server_name:
                return s.name, s.url, s.protocol
        # Selection didn't match — fall back.
        srt = svc.srt_servers()
        if srt:
            return srt[0].name, srt[0].url, srt[0].protocol
        s = svc.servers[0]
        return s.name, s.url, s.protocol

    # ---- video mode parsing --------------------------------------------------

    def video_dimensions(self) -> tuple[int, int, int]:
        """Return (width, height, fps) for the current video mode.

        Auto / unknown modes fall back to 1080p30.
        """
        mode = self.video_mode
        if mode == "Auto" or mode not in AVAILABLE_VIDEO_MODES:
            mode = "1080p30"
        # mode is like "1080p59.94" — height + 'p' + fps (possibly fractional)
        height_str, fps_str = mode.split("p", 1)
        height = int(height_str)
        width = 1920 if height == 1080 else 1280
        # fps: round fractional rates (29.97 → 30, 59.94 → 60) since FFmpeg's
        # rate filter accepts both, but the int form is what x264's GOP math uses.
        fps = round(float(fps_str))
        return width, height, fps

    # ---- profile resolution --------------------------------------------------

    def resolve_active_config(self):
        """Return the (StreamConfig, fps) we should encode with right now,
        or None if nothing is loaded.
        """
        svc = self.current_service()
        if not svc:
            return None
        profile = svc.find_profile(self.quality_level) or svc.get_default_profile()
        if not profile:
            return None
        width, height, fps = self.video_dimensions()
        res = "1080p" if height == 1080 else "720p"
        cfg = profile.find_config(res, fps)
        if cfg is None and profile.configs:
            # fall back to the highest-bitrate config in the profile
            cfg = max(profile.configs, key=lambda c: c.bitrate)
        return cfg

    # ---- snapshot ------------------------------------------------------------

    def snapshot(self) -> dict:
        with self.lock:
            active = self.current_active_server()
            cfg = self.resolve_active_config()
            svc = self.current_service()
            available_servers = (
                [{"name": s.name, "url": s.url, "protocol": s.protocol} for s in svc.servers]
                if svc
                else []
            )
            return {
                "label": self.label,
                "model": self.model,
                "unique_id": self.unique_id,
                "device_uuid": self.device_uuid,
                "video_mode": self.video_mode,
                "quality_level": self.quality_level,
                "source_id": self.source_id,
                "available_video_modes": AVAILABLE_VIDEO_MODES,
                "available_quality_levels": [
                    p.name for p in (svc.profiles if svc else [])
                ],
                "available_services": list(self.services.keys()),
                "available_servers": available_servers,
                "current_service_name": self.current_service_name,
                "current_server_name": self.current_server_name,
                "current_url": active[1] if active else "",
                "current_protocol": active[2] if active else "",
                "custom_url": self.custom_url,
                "stream_key": self.stream_key,
                "passphrase": self.passphrase,
                "srt_mode": self.srt_mode,
                "srt_latency_us": self.srt_latency_us,
                "srt_listen_port": self.srt_listen_port,
                "streamid_override": self.streamid_override,
                "streamid_legacy": self.streamid_legacy,
                "video_codec": self.video_codec,
                "av_video_index": self.av_video_index,
                "av_audio_index": self.av_audio_index,
                "av_video_name": self.av_video_name,
                "av_audio_name": self.av_audio_name,
                "pipe_path": self.pipe_path,
                "relay": {
                    "bind_host": self.relay_bind_host,
                    "srt_port": self.relay_srt_port,
                    "srt_latency_us": self.relay_srt_latency_us,
                    "srt_passphrase": self.relay_srt_passphrase,
                    "rtmp_port": self.relay_rtmp_port,
                    "rtmp_app": self.relay_rtmp_app,
                    "rtmp_key": self.relay_rtmp_key,
                },
                "overlay": {
                    "title": self.overlay_title,
                    "subtitle": self.overlay_subtitle,
                    "logo_path": self.overlay_logo_path,
                    "clock": self.overlay_clock,
                },
                "active_config": (
                    {
                        "resolution": cfg.resolution,
                        "fps": cfg.fps,
                        "codec": cfg.codec,
                        "bitrate": cfg.bitrate,
                        "audio_bitrate": cfg.audio_bitrate,
                        "keyframe_interval": cfg.keyframe_interval,
                    }
                    if cfg
                    else None
                ),
                "stats": {
                    "status": self.stats.status,
                    "bitrate": self.stats.bitrate,
                    "duration": self.stats.duration_string(),
                    "cache_used": self.stats.cache_used_pct,
                    "error": self.stats.error,
                    "fps": round(self.stats.fps, 1),
                    "speed": round(self.stats.speed, 2),
                    "frames_sent": self.stats.frames_sent,
                    "frames_dropped": self.stats.frames_dropped,
                    "quality": round(self.stats.quality, 1),
                },
            }
