atem-net-diag — ATEM SRT/RTMP network diagnostic tool
v0.1.0  ·  arm64 macOS  ·  signed by "Developer ID Application: Stephen Walter (6M536MV7GT)"

WHAT IT DOES
------------
Three modes (combine freely):

  1. Active probe — periodically attempts an SRT/RTMP handshake against a
     destination and logs whether it's currently accepting connections.
     Catches "ATEM stops accepting after a few stream tests" lockouts.
     With multiple --key flags it tells you whether one specific stream
     key is locked or the whole receiver.

  2. Passive flow monitor (--monitor IFACE) — wraps tshark with a port
     filter and reports a live flow table: who's currently sending bytes
     to/from your ATEM, on which ports, at what rate.

  3. Visual dashboard (--ui [PORT]) — spins up a single-page web UI at
     http://localhost:8092/ (default) and auto-opens it in your browser.
     Per-key probe status, recent probe timeline, live flow table —
     all updating every 1 second. The dashboard also has a CONFIG FORM
     at the top: type Host / Port / Key / Interval, click Apply, and
     the probe loop reconfigures live (no restart). Combine with
     --key, --monitor, etc. for the initial state.

ONE-TIME SETUP
--------------
1. Install Wireshark (provides tshark + the ChmodBPF helper that grants
   normal users packet-capture permission):

       brew install --cask wireshark

   When the installer offers "Install ChmodBPF", say yes — that's what
   makes --monitor work without sudo.

2. Install FFmpeg (for active probes — uses libsrt):

       brew install ffmpeg

3. (Once per machine) clear the macOS quarantine attribute. The binary
   is signed but Gatekeeper still flags freshly-downloaded files unless
   notarized:

       xattr -dr com.apple.quarantine ./atem-net-diag

USAGE
-----
Show the full flag list:

    ./atem-net-diag --help

Visual dashboard — recommended starting point. Launch with --ui and
nothing else, then enter the ATEM IP / port / key in the form at the top:

    ./atem-net-diag --ui

Or pre-populate from the CLI:

    ./atem-net-diag srt://YOUR_ATEM_IP:1935 --key YOUR_KEY --ui

Add --monitor en0 to also see what's flowing on the wire:

    ./atem-net-diag srt://YOUR_ATEM_IP:1935 --key YOUR_KEY --ui --monitor en0

Multi-key rotation in the dashboard — distinguish per-key vs receiver-
wide lockouts at a glance:

    ./atem-net-diag srt://YOUR_ATEM_IP:1935 \
        --key q1ry-... --key j4fh-... --key n1sn-... --ui

CLI-only modes (no browser):

    ./atem-net-diag srt://YOUR_ATEM_IP:1935 --key K
    ./atem-net-diag srt://YOUR_ATEM_IP:1935 --key K --csv probes.csv
    ./atem-net-diag --monitor en0

WHAT TO LOOK FOR
----------------
  All keys CONNECTED ...................... destination is happy
  REJECTED bursts after a stream .......... receiver-state lockout (~30s-2min)
  One key REJECTS, others CONNECT ......... per-key lockout — use a different key
  All TIMEOUT ............................. destination unreachable

REQUIREMENTS
------------
  - macOS 11+ on Apple Silicon (arm64). For Intel Macs, build from source.
  - ffmpeg in PATH (with libsrt) for srt:// probes.
  - tshark in PATH (Wireshark.app) for --monitor.
  - ChmodBPF helper (Wireshark installer) OR sudo, for --monitor.

SOURCE
------
https://github.com/amateurmenace/atem-ip-patchbay (tools/atem-net-diag/)
MIT licensed, same as the parent app.
