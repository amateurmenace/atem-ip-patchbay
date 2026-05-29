"""Parse Blackmagic streaming service XML files.

The XML format is documented in Blackmagic's "Streaming XML File Format"
spec. The interesting fields for us:

  <service>
    <name>...</name>
    <key>...</key>           # stream key — used as r= in SRT streamid
    <servers>
      <server group="...">
        <name>RTMP|SRT</name>
        <url>...</url>
      </server>
    </servers>
    <profiles default="...">
      <profile>
        <name>...</name>
        <low-latency/>
        <config resolution="..." fps="..." codec="...">
          <bitrate>...</bitrate>
          <audio-bitrate>...</audio-bitrate>
          <keyframe-interval>...</keyframe-interval>
        </config>
      </profile>
    </profiles>
  </service>
"""

from __future__ import annotations

import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from pathlib import Path


@dataclass
class StreamConfig:
    resolution: str
    fps: int
    codec: str
    bitrate: int
    audio_bitrate: int
    keyframe_interval: int


@dataclass
class StreamProfile:
    name: str
    low_latency: bool
    configs: list[StreamConfig] = field(default_factory=list)

    def find_config(self, resolution: str, fps: int) -> StreamConfig | None:
        for cfg in self.configs:
            if cfg.resolution == resolution and cfg.fps == fps:
                return cfg
        return None


@dataclass
class StreamServer:
    name: str
    group: str
    url: str

    @property
    def protocol(self) -> str:
        return self.url.split("://", 1)[0].lower() if "://" in self.url else ""


@dataclass
class StreamService:
    name: str
    key: str
    servers: list[StreamServer] = field(default_factory=list)
    profiles: list[StreamProfile] = field(default_factory=list)
    default_profile: str = ""

    def srt_servers(self) -> list[StreamServer]:
        return [s for s in self.servers if s.protocol == "srt"]

    def find_profile(self, name: str) -> StreamProfile | None:
        for p in self.profiles:
            if p.name == name:
                return p
        return None

    def get_default_profile(self) -> StreamProfile | None:
        return self.find_profile(self.default_profile) or (
            self.profiles[0] if self.profiles else None
        )


def load_service(xml_path: str | Path) -> StreamService:
    tree = ET.parse(xml_path)
    root = tree.getroot()
    service_el = root.find("service") if root.tag != "service" else root
    if service_el is None:
        raise ValueError(f"No <service> element in {xml_path}")
    return _parse_service(service_el)


def load_service_text(text: str) -> StreamService:
    """Same as load_service() but reads the XML from a string."""
    root = ET.fromstring(text)
    service_el = root.find("service") if root.tag != "service" else root
    if service_el is None:
        raise ValueError("No <service> element in pasted XML")
    return _parse_service(service_el)


def _parse_service(el: ET.Element) -> StreamService:
    name = (el.findtext("name") or "").strip()
    key = (el.findtext("key") or "").strip()

    servers: list[StreamServer] = []
    for server_el in el.findall("./servers/server"):
        servers.append(
            StreamServer(
                name=(server_el.findtext("name") or "").strip(),
                group=server_el.get("group", "Default"),
                url=(server_el.findtext("url") or "").strip(),
            )
        )

    profiles: list[StreamProfile] = []
    profiles_el = el.find("profiles")
    default_profile = profiles_el.get("default", "") if profiles_el is not None else ""
    if profiles_el is not None:
        for profile_el in profiles_el.findall("profile"):
            profiles.append(_parse_profile(profile_el))

    return StreamService(
        name=name,
        key=key,
        servers=servers,
        profiles=profiles,
        default_profile=default_profile,
    )


def _parse_profile(el: ET.Element) -> StreamProfile:
    configs: list[StreamConfig] = []
    for cfg_el in el.findall("config"):
        try:
            configs.append(
                StreamConfig(
                    resolution=cfg_el.get("resolution", ""),
                    fps=int(cfg_el.get("fps", "0")),
                    codec=cfg_el.get("codec", ""),
                    bitrate=int((cfg_el.findtext("bitrate") or "0").strip()),
                    audio_bitrate=int(
                        (cfg_el.findtext("audio-bitrate") or "0").strip()
                    ),
                    keyframe_interval=int(
                        (cfg_el.findtext("keyframe-interval") or "0").strip()
                    ),
                )
            )
        except (TypeError, ValueError):
            continue
    return StreamProfile(
        name=(el.findtext("name") or "").strip(),
        low_latency=el.find("low-latency") is not None,
        configs=configs,
    )
