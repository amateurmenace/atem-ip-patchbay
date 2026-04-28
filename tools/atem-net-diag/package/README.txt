atem-net-diag — ATEM network diagnostic tool
v0.2.1  ·  arm64 macOS

WHAT'S NEW IN v0.2.1 (Session 6 — monitor-first rework)
-------------------------------------------------------
* WAN BANDWIDTH AT THE GATEWAY — polled from the UDM's stat/health
  endpoint and graphed as upload + download sparklines. Set your
  upload cap once and get a real-time headroom indicator
  ("14/20 Mbps used · 70%") that turns yellow at 70%, red at 90%.

* PER-SOURCE STICKY LABELS — click "name this source…" on any flow
  card to tag the source IP ("Jamie's basement", "Tightrope carousel").
  Persists across the binary's lifetime.

* PRE-SHOW HEALTH BANNER — single green/yellow/red banner at the top
  aggregating UDM, ATEM, WAN, and capture state. One glance to know
  if you're ready to go live.

* QUALITY ALERTS — per-flow alert badges for bitrate dropouts (>50% /
  >75% drop), idle stalls, RTT spikes (>200ms / >500ms), stalling.

* SWITCH PORT UTILIZATION — every UniFi switch with per-port real-time
  tx/rx, errors, link-flaps, SFP optical metrics. The ATEM's port is
  highlighted with an orange dot. Switches sorted with the ATEM's
  host switch first.

* ACTIVE ALARMS BANNER — surfaces UDM-reported alarms when present.

* GATEWAY SYSSTATS — UDM CPU / mem / uptime in the Process health card.

* REVERSE-DNS for stream sources — public-IP flow sources auto-resolve
  to their PTR records ("96.234.1.2 → cust-3-12.verizon.com"). LAN
  IPs skipped.

* EDITABLE ATEM TARGET / CAPTURE INTERFACE — both as collapsible cards
  at the top, two-up. Click to expand.

* THREE BANDWIDTH BUGS FIXED — UDM byte counters for wired clients now
  read correctly, tx/rx labels reflect device perspective (not the
  switch's), and kbps numbers are stable (use UDM-reported rate fields
  rather than self-derived 2s deltas).

WHAT'S NEW IN v0.2.0 (Session 5)
--------------------------------
* DEFAULT MODE = LIVE — pure passive monitoring, zero outbound traffic to
  the ATEM. Safe to run alongside an in-progress production. Switch to
  STANDBY (toggle in dashboard) only when you want to actively test
  reachability against a destination not currently in production.

* UDM (UniFi Dream Machine) integration — polls the controller's per-
  client bandwidth stats via the local API. Shows what's ACTUALLY
  flowing to the ATEM in real time, regardless of which machine the
  diagnostic tool is running on. Solves the switched-LAN visibility
  problem (a peer Mac can't see unicast traffic between two other
  devices on a managed switch — but the UDM sees everything).

* Per-key flow correlation — parses the SRT HSv5 conclusion handshake's
  SID extension to extract each flow's stream key. Per-flow cards now
  show the BMD `u=` key value, so you can match a flow to a specific
  stream identity at a glance during multi-source production.

* Default ATEM target pre-filled (192.168.20.189:1935 / 7c:2e:0d:21:ab:fe).
  Override in the dashboard's ATEM target section if your hardware moves.

WHAT IT DOES
------------
Three data sources, all running concurrently when configured:

  1. UDM POLLING — passive, no outbound to ATEM.
     Polls the UniFi controller's stat/sta endpoint every 2 seconds for
     per-client bandwidth. Surfaces "what's the ATEM receiving right now"
     and "which clients are talking to it" without touching the
     production stream. Works from any machine that can reach the UDM.

  2. CAPTURE (tshark) — passive, requires local capture permission.
     Runs tshark on the configured interface (default en0) with a port
     filter (1935, 9710, 9977, 1936). Per-flow visibility: src:port →
     dst:port, SRT control-packet stats (RTT, bandwidth estimate,
     receiver buffer), live bitrate sparkline, and (when handshake is
     captured) the stream key. ONLY sees flows that physically traverse
     this machine. UDM polling above complements this with a global view.

  3. ACTIVE PROBE — STANDBY MODE ONLY. Off by default.
     Periodically attempts an SRT/RTMP handshake against a destination.
     Useful for testing reachability when no production is running.
     CONSUMES A RECEIVER SLOT; will be REJECTED by the ATEM if a real
     stream is using the same key. Toggle to Standby in the dashboard
     to enable.

ONE-TIME SETUP
--------------
1. Create a Local Controller API key in your UDM web UI:
   Settings → Control Plane → Integrations → Create API Key
   (Read access is sufficient. Save the key — you won't see it again.)

2. Install Wireshark (provides tshark + the ChmodBPF helper that grants
   normal users packet-capture permission):

       brew install --cask wireshark

   When the installer offers "Install ChmodBPF", say yes.

3. Install FFmpeg (only needed for Standby mode active probes):

       brew install ffmpeg

4. (Once per machine) clear the macOS quarantine attribute. The binary
   is signed but Gatekeeper still flags freshly-downloaded files until
   notarized:

       xattr -dr com.apple.quarantine ./atem-net-diag

USAGE
-----
Recommended: double-click start.command. It will prompt you to set the
UDM_API_KEY if not already exported in your environment, then launch
the dashboard at http://localhost:8092/.

CLI usage:

    # Live mode (default) with UDM polling enabled.
    UDM_API_KEY=YOUR_KEY ./atem-net-diag --ui

    # Same, but pre-configure the active probe target (still off until
    # you flip the dashboard mode toggle to Standby).
    UDM_API_KEY=YOUR_KEY ./atem-net-diag srt://192.168.20.189:1935 --ui

    # CLI-only modes (no browser):
    ./atem-net-diag srt://YOUR_ATEM_IP:1935 --key K
    ./atem-net-diag --monitor en0

ENVIRONMENT VARIABLES
---------------------
  UDM_API_KEY       Local Controller API key. Preferred auth.
  UDM_USERNAME      Local-account fallback for UniFi OS < 9.0.
  UDM_PASSWORD      Local-account fallback (paired with UDM_USERNAME).
  UDM_HOST          Override the UDM URL. Default: https://192.168.20.1

NETWORK TOPOLOGY NOTES
----------------------
A peer machine on a switched LAN typically CAN'T see unicast traffic
between two other devices — modern Ethernet switches don't broadcast
unicast. For the CAPTURE data source to populate, this tool must run
on:

  (a) the streamer's machine (sees egress), or
  (b) the ATEM's local machine if you have shell access, or
  (c) a port-mirrored / SPAN switch port.

The UDM POLLING data source is independent and works from any machine
that can reach the controller — that's why it's the default headline
view in v0.2.0.

WHAT TO LOOK FOR
----------------
LIVE MODE (default):
  ATEM "ONLINE via wired" + non-zero rx_kbps in clients table → flow OK
  ATEM "NOT FOUND" in client list → MAC mismatch or ATEM offline
  Stream key visible on flow card → handshake captured + parsed OK

STANDBY MODE (toggled):
  All keys CONNECTED ............... destination is happy
  REJECTED bursts after a stream ... receiver-state lockout (~30s-2min)
  One key REJECTS, others CONNECT .. per-key lockout
  All TIMEOUT ...................... destination unreachable

REQUIREMENTS
------------
  - macOS 11+ on Apple Silicon (arm64). For Intel Macs, build from source.
  - tshark in PATH for CAPTURE data source.
  - ChmodBPF helper (Wireshark installer) OR sudo, for CAPTURE.
  - ffmpeg in PATH for STANDBY active probes.

SOURCE
------
https://github.com/amateurmenace/atem-ip-patchbay (tools/atem-net-diag/)
MIT licensed, same as the parent app.
