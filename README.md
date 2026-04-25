# ATEM IP Patchbay

> Turn any video source into a remote input that streams into your
> **ATEM Extreme G2**, **Television Studio**, or **Streaming Decoder**
> over ethernet via SRT.

Made by [Stephen Walter](https://weirdmachine.org) and Claude. Powered
by Blackmagic Design.

## How it happened

Made possible in part by the latest update to the iOS Blackmagic Camera
App, which for the first time let that excellent free app stream
directly into an input on a qualifying ATEM. Because it's software and
not Blackmagic hardware, they seem to have lessened some of the
handshake requirements between sender and receiver, so it was much
easier to deconstruct what was essential and what was just noise.
Then Claude and I (mostly Claude) built an emulator of a streaming
encoder and eventually stumbled onto the winning combination that got
a signal into the ATEM Mini Extreme ISO G2 that inspired this journey.

## What it does

NDI, SDI, HDMI, non-Blackmagic SRT and RTMP streams can all be
converted into the special Blackmagic flavor of SRT using this app,
for free. This is more of a demo app showing what is now possible, a
proof of concept, and should only be used in real productions at your
own risk. It is free forever, until it either gets stopped by
Blackmagic or they fully embrace opening up their stream decoding
ecosystem.

Under the hood:

| Layer | Behavior |
|---|---|
| Control protocol | Blackmagic Streaming Encoder Ethernet Protocol v1.2 on TCP 9977 — IDENTITY, VERSION, NETWORK, UI SETTINGS, STREAM SETTINGS, STREAM XML, STREAM STATE, AUDIO SETTINGS, SHUTDOWN. Looks like a real encoder to BMD's tooling. |
| Streaming pipeline | FFmpeg with libx265 (default, what real BMD encoders send) or libx264 + libsrt. Main profile, no B-frames, fixed GOP, AAC-LC 48 kHz stereo, MPEG-TS, SRT caller mode by default. |
| SRT streamid | `bmd_uuid=<uuid>,bmd_name=<label>,u=<key>` — the format real BMD encoders actually send, verified against an iPhone Blackmagic Camera pcap. ATEM Mini built-in Streaming Bridge mode accepts standard libsrt + HEVC + MPEG-TS — no BMOS / BTST / TURN extension needed. |
| Service XML | Parses the standard `<streaming><service>` XML format the real encoder loads via STREAM XML. The `<key>` element becomes the `u=` value in the streamid. |
| Sources (macOS) | Test pattern, AVFoundation device (any webcam, USB capture card, **NDI Virtual Camera**, **OBS Virtual Camera**, Continuity Camera, screen capture), or pipe / URL. |
| Sources (Windows) | Test pattern, DirectShow device (any webcam, USB capture, **NDI Virtual Camera**, **OBS Virtual Camera**), gdigrab desktop capture, or pipe / URL. |
| Overlays | Title, subtitle, logo image, and live clock — composed via FFmpeg `filter_complex` with no measurable bitrate hit (CBR holds the target). |
| Direct connect | Type protocol + host + port + path right in the UI — builds a BMD-correct URL and overrides whatever's in the XML. |
| Probe tool | `python3 probe.py` walks the XML's servers and classifies each as REFUSED / TIMEOUT / HANDSHAKE-OK-PUBLISH-REJECTED / OK. |

## Use at your own risk

Proof of concept. Use in real productions at your own risk. There is
no support, no warranty, and no guarantee that any future Blackmagic
firmware update won't break the handshake we found. If something on
your destination decoder breaks, the first place to look is the
**FFmpeg Log** card in the UI, then `python3 probe.py`.

## Install

### macOS

```sh
brew install ffmpeg python@3.11
git clone <this-repo-url>
cd ATEM\ IP\ Patchbay
cp config/example.xml "config/My ATEM.xml"   # then edit the host + key
python3 run.py
```

The UI opens at <http://127.0.0.1:8090/> and the BMD control protocol
listens on TCP 9977.

### Windows

```powershell
# Install ffmpeg (e.g. from https://www.gyan.dev/ffmpeg/builds/)
# Install Python 3.11+ from python.org
git clone <this-repo-url>
cd "ATEM IP Patchbay"
copy config\example.xml "config\My ATEM.xml"   # edit the host + key
python run.py
```

The Windows source layer uses DirectShow for cameras and gdigrab for
desktop capture. Audio devices appear automatically in the Source tile
gallery alongside video devices.

### Linux

Not supported as a one-click experience yet. The control protocol and
streamer work; only the device-scanner backend is missing (v4l2 / X11
grab). Run `python3 run.py` and use the **Pipe / URL** source as a
workaround.

## How to use

1. Drop your service XML into `config/` (use `config/example.xml` as
   a starting point) or paste an SRT URL into the **Destination →
   Paste** tab.
2. Pick a source from the tile gallery. Cameras, screen captures,
   NDI Virtual Camera, OBS Virtual Camera, and capture cards all
   appear automatically.
3. Pick a video mode and quality profile (defaults to 1080p30 /
   Streaming High — 6 Mbps with HEVC).
4. Click **Start Stream**. The status pill goes amber (Connecting) →
   red (Streaming).
5. The **FFmpeg Log** card shows the actual command line and recent
   stderr if something goes wrong.

Use **H.265** for ATEM Mini built-in Streaming Bridge mode — it's the
codec real BMD encoders and the iPhone Blackmagic Camera both send.
Switch to H.264 only when targeting an RTMP receiver (which usually
rejects HEVC) or older standalone Streaming Bridge firmware.

## Screenshots

<!-- TODO: add screenshots once the first release is cut. Suggested:
     - hero / main UI with a live source preview
     - destination tabs (Paste / XML / LAN)
     - FFmpeg log card showing a healthy stream
-->

## Direct Connect

The **Destination → LAN** panel lets you target any SRT or RTMP
receiver without editing XML. Type a host + port (and an RTMP app path
if needed), click **Use this URL**.

The resulting URL appears in the **Custom URL Override** field, takes
precedence over the XML, and respects the same BMD encode constraints:

- For SRT: streamid in `bmd_uuid=…,bmd_name=…,u=<key>` form, caller
  mode by default, 500 ms latency, MPEG-TS muxing.
- For RTMP/RTMPS: stream key appended to the path as `<url>/<key>`,
  FLV muxing with `no_duration_filesize`.

Click **Clear override** to revert to the XML's server.

## Probe tool

When a destination doesn't accept publishes, run:

```sh
python3 probe.py                      # probe everything in ./config/
python3 probe.py --scan-srt-ports     # also try alt UDP ports
python3 probe.py --scan-rtmp-apps     # also try common app names
python3 probe.py --host h --port p    # ad-hoc target
```

Each probe pushes a tiny test pattern (320×240, 500 kbps) for ~3
seconds, with a 5-second gap between attempts to avoid fail2ban / rate
limiters. The summary classifies every probe:

- **REFUSED** — nothing listening (or you're temp-banned)
- **TIMEOUT** — host unreachable / firewall drop
- **HANDSHAKE OK, PUBLISH REJECTED** — server speaks the protocol
  but rejects after the handshake (auth, bitrate cap, key mismatch)
- **OK** — connection completed

## CLI options

```
python3 run.py --help
```

Common ones:
- `--xml path/to/Service.xml` — load a different service (repeatable).
- `--label "My Encoder"` — sets IDENTITY label and the `bmd_name` part
  of the streamid.
- `--http-port 8081` — move the UI off port 8090.
- `--protocol-port 9988` — move the BMD protocol off 9977 (useful if
  you want to test alongside a real encoder on the LAN).
- `--no-browser` — don't auto-open.
- `-v` — debug logging (shows every block sent / received over the
  protocol).

## Talking to the control protocol manually

```sh
nc 127.0.0.1 9977
```

You'll see the full preamble dump. To start the stream:

```
STREAM STATE:
Action: Start

```

(Trailing blank line required.) The server responds `ACK` and then
broadcasts the new STREAM STATE.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `Connection setup failure` in log | Receiver not reachable on UDP, or streamid `u=` doesn't match the key configured on the decoder |
| Stream connects but decoder shows no picture | Decoder rejected the codec — confirm H.265 vs H.264 and that `-profile:v main` is in the FFmpeg command |
| `Operation not permitted` opening AVFoundation (macOS) | Grant Terminal/iTerm camera+microphone access in System Settings → Privacy & Security |
| `Could not enumerate video devices` (Windows) | Install or update FFmpeg — the dshow indev needs to be present (`ffmpeg -devices` should list `dshow`) |
| `Address already in use` on 9977 or 8090 | Another instance is running, or a real encoder is on your LAN broadcasting on 9977 — pass `--protocol-port` / `--http-port` to move |

## File layout

```
.
├── README.md
├── run.py                              # entry point
├── probe.py                            # destination probe tool
├── config/
│   ├── example.xml                     # copy + edit this
│   └── *.xml                           # your real configs (gitignored)
└── bmd_emulator/
    ├── state.py                        # EncoderState data model
    ├── xml_loader.py                   # parse <streaming> XML
    ├── streamid.py                     # build BMD streamid + SRT URL
    ├── sources.py                      # avfoundation / dshow / gdigrab / pipe
    ├── device_scanner.py               # AVFoundation + DirectShow scan
    ├── streamer.py                     # FFmpeg subprocess + monitor
    ├── protocol.py                     # TCP 9977 BMD protocol server
    ├── discover.py                     # mDNS NDI sender discovery
    ├── paste_parser.py                 # parse pasted SRT URLs / keys
    ├── web.py                          # HTTP control panel
    └── static/                         # UI (vanilla HTML/CSS/JS)
```

## License

[MIT](LICENSE). Original work; no Blackmagic firmware or proprietary
code is included or distributed. The protocol and XML format
implemented here are documented publicly in *Blackmagic Streaming
Encoder Ethernet Protocol* and *Blackmagic Streaming XML File Format*
(both Blackmagic Developer Information).
