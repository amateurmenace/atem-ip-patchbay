"""Parse pasted destination text into (url, key, passphrase, name).

Accepts five common forms users will plausibly paste:

  1. **SRT URL with embedded streamid** —
     `srt://207.180.143.114:1935?streamid=#!::bmd_uuid=…,u=KEY`

  2. **Plain SRT or RTMP URL** —
     `srt://host:1935`  or  `rtmp://server/app/key`

  3. **Multi-line key/value** —
     `Host: 207.180.143.114\\nPort: 1935\\nKey: n1sn-…\\nName: ATEM`

  4. **JSON object** —
     `{"url": "srt://…", "key": "…", "name": "…"}`

  5. **Bare stream key** —
     `n1sn-2kpg-9w37-7f54` (left in place; user must combine with a
     separately-pasted URL)

The parser returns a `Pasted` dataclass whose fields the caller can
overlay onto state. Anything missing is left as the empty string so the
caller can handle "did you fill in enough?" themselves.
"""

from __future__ import annotations

import json
import re
import urllib.parse
from dataclasses import dataclass


@dataclass
class Pasted:
    url: str = ""
    stream_key: str = ""
    passphrase: str = ""
    name: str = ""
    protocol: str = ""  # "srt" / "rtmp" / "rtmps" — detected from url

    def is_valid(self) -> bool:
        return bool(self.url) or bool(self.stream_key)

    def to_json(self) -> dict:
        return {
            "url": self.url,
            "stream_key": self.stream_key,
            "passphrase": self.passphrase,
            "name": self.name,
            "protocol": self.protocol,
        }


_KEY_LINE = re.compile(r"^\s*([a-z][a-z0-9_ \-]*)\s*[:=]\s*(.+?)\s*$", re.I)
_URL_RE = re.compile(r"\b((?:srt|rtmp|rtmps)://[^\s\"']+)", re.I)
_BARE_KEY = re.compile(r"^[a-z0-9]{4}(?:-[a-z0-9]{4}){3}$", re.I)


def parse(text: str) -> Pasted:
    text = (text or "").strip()
    if not text:
        return Pasted()

    # 1. JSON?
    if text.startswith("{") and text.endswith("}"):
        try:
            obj = json.loads(text)
            return _from_dict(obj)
        except json.JSONDecodeError:
            pass

    # 2. Find any URL anywhere in the text. Embedded streamid pulls out
    #    `u=KEY` automatically. Multi-line pastes can have the URL on
    #    one line and `Key: …` on another — this finds the URL first
    #    and the key/value pass below picks up the rest.
    out = Pasted()
    url_match = _URL_RE.search(text)
    if url_match:
        out.url = url_match.group(1)
        out.protocol = out.url.split("://", 1)[0].lower()
        # If the URL has a streamid query param with u=KEY, peel it.
        out.stream_key = _extract_u_from_streamid(out.url)

    # 3. Walk lines for Key: / Passphrase: / Name: / Host+Port etc.
    host = ""
    port = ""
    proto_hint = ""
    for line in text.splitlines():
        m = _KEY_LINE.match(line)
        if not m:
            # Maybe a bare key on its own line?
            stripped = line.strip()
            if not out.stream_key and _BARE_KEY.match(stripped):
                out.stream_key = stripped
            continue
        field = m.group(1).strip().lower().replace(" ", "_").replace("-", "_")
        value = m.group(2).strip().strip("\"'")
        if field in ("key", "stream_key", "streamkey", "u"):
            if value and not out.stream_key:
                out.stream_key = value
        elif field in ("passphrase", "password", "pw"):
            out.passphrase = value
        elif field in ("name", "service_name", "service", "label", "device", "device_name"):
            out.name = value
        elif field in ("host", "ip", "address", "server"):
            host = value
        elif field in ("port",):
            port = value
        elif field in ("protocol", "proto", "scheme"):
            proto_hint = value.lower()
        elif field in ("url",) and not out.url:
            out.url = value
            out.protocol = value.split("://", 1)[0].lower() if "://" in value else ""

    # 4. If we got host+port but no URL, synthesize one.
    if not out.url and host:
        proto = proto_hint or "srt"
        if proto not in ("srt", "rtmp", "rtmps"):
            proto = "srt"
        port_s = port or ("1935" if proto in ("rtmp", "rtmps") else "1935")
        out.url = f"{proto}://{host}:{port_s}"
        out.protocol = proto

    return out


def _extract_u_from_streamid(url: str) -> str:
    """Pull `u=<key>` out of an SRT streamid embedded in the query."""
    if "://" not in url:
        return ""
    try:
        parsed = urllib.parse.urlparse(url)
        qs = urllib.parse.parse_qs(parsed.query)
        sid = ""
        for candidate in ("streamid", "streamId", "STREAMID"):
            if candidate in qs and qs[candidate]:
                sid = qs[candidate][0]
                break
        if not sid:
            return ""
        # streamid format: #!::k1=v1,k2=v2,...
        body = sid[4:] if sid.startswith("#!::") else sid
        for part in body.split(","):
            if part.startswith("u="):
                return part[2:]
            if part.startswith("r="):  # legacy format
                return part[2:]
    except Exception:  # noqa: BLE001
        return ""
    return ""


def _from_dict(obj: dict) -> Pasted:
    out = Pasted()
    out.url = str(obj.get("url") or obj.get("server") or "")
    out.stream_key = str(obj.get("key") or obj.get("stream_key") or obj.get("streamKey") or "")
    out.passphrase = str(obj.get("passphrase") or obj.get("password") or "")
    out.name = str(obj.get("name") or obj.get("service") or "")
    if out.url and "://" in out.url:
        out.protocol = out.url.split("://", 1)[0].lower()
    if not out.stream_key and out.url:
        out.stream_key = _extract_u_from_streamid(out.url)
    return out
