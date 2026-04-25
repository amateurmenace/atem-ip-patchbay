"""Tiny network introspection helpers.

Used to fill in the publish-URL hint shown in the UI when the user
selects an SRT or RTMP relay source — they need to know what host
their external encoder should target.
"""

from __future__ import annotations

import socket


def get_lan_ip() -> str:
    """Return the machine's primary outbound LAN IP, or 127.0.0.1.

    Uses the standard "connect a UDP socket and read getsockname"
    trick: no packet is sent (UDP connect just primes the kernel
    routing table), but it lets us read whichever local address
    Linux/macOS/Windows would pick for outbound traffic.
    """
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("8.8.8.8", 80))
        return s.getsockname()[0]
    except OSError:
        return "127.0.0.1"
    finally:
        s.close()
