"""Build SRT streamid in the Blackmagic-compatible format.

The "secret sauce" for getting Blackmagic decoders (Web Presenter HD/4K,
ATEM Streaming Bridge) to accept a stream is the SRT Access-Control
streamid format with `bmd_uuid` and `bmd_name` extensions, plus the
stream key passed as a *username* (`u=`):

    #!::bmd_uuid=<uuid4>,bmd_name=<device_label>,u=<stream_key>

The whole streamid string must then be URL-encoded when used in the
SRT URL query parameter (`&streamid=...`).

Format derivation: pcap of a real Blackmagic Web Presenter pushing to
an ATEM Mini Extreme ISO G2 in built-in Streaming Bridge mode shows the
stream key carried as `u=`, with no `m=publish` tag, and no `r=` field.
Earlier third-party docs (OvenMediaEngine, mediamtx, datarhei) used
`r=KEY,m=publish` based on the SRT Access Control RFC, but the real
BMD encoders deviate from that convention.

Set `legacy_format=True` to produce the old `r=KEY,m=publish` form for
non-BMD receivers that learned that variant from third-party docs.
"""

from __future__ import annotations

import urllib.parse
import uuid


def build_bmd_streamid(
    stream_key: str,
    device_name: str = "Streaming Encoder",
    device_uuid: str | None = None,
    mode: str = "publish",
    legacy_format: bool = False,
) -> str:
    """Build the unencoded streamid string in Blackmagic format.

    The returned string starts with the `#!::` marker. URL-encode it
    before appending to an SRT URL.
    """
    if device_uuid is None:
        device_uuid = str(uuid.uuid4())

    # Stream keys with slashes are rejected by Blackmagic devices —
    # strip them defensively.
    safe_key = stream_key.replace("/", "_")
    safe_name = device_name.replace(",", " ").replace("=", " ")

    if legacy_format:
        parts = [
            f"r={safe_key}",
            f"m={mode}",
            f"bmd_uuid={device_uuid}",
            f"bmd_name={safe_name}",
        ]
    else:
        parts = [
            f"bmd_uuid={device_uuid}",
            f"bmd_name={safe_name}",
            f"u={safe_key}",
        ]
    return "#!::" + ",".join(parts)


def build_srt_url(
    host: str,
    port: int,
    stream_key: str,
    device_name: str = "Streaming Encoder",
    device_uuid: str | None = None,
    latency_us: int = 500_000,
    passphrase: str | None = None,
    pbkeylen: int | None = None,
    mode: str = "caller",
    streamid_override: str = "",
    listen_port: int | None = None,
    extra_params: dict[str, str] | None = None,
    legacy_streamid: bool = False,
) -> str:
    """Build a fully-formed SRT URL.

    `mode`:
      - "caller" — we initiate (default; what real BMD encoders do)
      - "listener" — we bind and wait for the receiver to call us;
        when listener, the URL's host portion becomes empty and we
        bind on `listen_port` (the `host`/`port` args are then unused)
      - "rendezvous" — both sides initiate simultaneously

    `streamid_override`: if non-empty, used verbatim instead of the
    BMD-format streamid. Useful for receivers that expect a different
    streamid scheme, or no streamid at all (pass " " to omit).
    """
    streamid = (
        streamid_override
        if streamid_override
        else build_bmd_streamid(
            stream_key, device_name, device_uuid, legacy_format=legacy_streamid
        )
    )

    params: dict[str, str] = {
        "mode": mode,
        "latency": str(latency_us),
    }
    # Listener mode doesn't carry a streamid (the caller provides it
    # at handshake time). Suppress the streamid param when listening.
    if mode != "listener" and streamid.strip():
        params["streamid"] = streamid
    if passphrase:
        params["passphrase"] = passphrase
        if pbkeylen is not None:
            params["pbkeylen"] = str(pbkeylen)
    if extra_params:
        params.update(extra_params)

    query = urllib.parse.urlencode(params, quote_via=urllib.parse.quote)

    if mode == "listener":
        bind_port = listen_port if listen_port is not None else port
        # libsrt accepts srt://:port form for listener; FFmpeg accepts
        # srt://0.0.0.0:port too. Use the explicit form for clarity.
        return f"srt://0.0.0.0:{bind_port}?{query}"

    return f"srt://{host}:{port}?{query}"


def parse_srt_host_port(url: str, default_port: int = 1935) -> tuple[str, int]:
    """Pull host + port out of a `srt://host[:port][/...]` URL."""
    if "://" in url:
        url = url.split("://", 1)[1]
    # Strip any path/query.
    for sep in ("/", "?"):
        if sep in url:
            url = url.split(sep, 1)[0]
    if ":" in url:
        host, port_str = url.rsplit(":", 1)
        try:
            return host, int(port_str)
        except ValueError:
            return host, default_port
    return url, default_port
