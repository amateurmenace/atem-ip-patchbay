# ATEM IP Patchbay — CLAUDE.md

> Internal-only doc that primes Claude Code with everything needed to
> continue the build. The README is the user-facing version; this file
> is the working state — what's broken, what's been tried, what's next.

## v0.2.0 direction (current focus)

The v0.1.0 alpha shipped on Mac arm64 + Windows x64 with one major
known limitation: **NDI Virtual Camera streaming via FFmpeg AVF
doesn't work** (extensively diagnosed; see "Currently-open issues
#1"). v0.2.0 reframes the project around four headline features:

1. **Direct NDI ingest via the NewTek NDI SDK.** Skip NDI Virtual
   Camera entirely. Receive NDICAM (and any other NDI sender on the
   network) into our process via the `grafton-ndi` Rust crate (it
   wraps the SDK's C library — `libndi.dylib` on Mac,
   `Processing.NDI.Lib.x64.dll` on Windows). The receiver runs in
   our process, frames stream into FFmpeg via stdin / a UNIX
   socket, no AVCaptureSession involved.
2. **Better camera previews.** The current
   `getUserMedia`/AVFoundation preview conflicts with FFmpeg
   capture for some virtual cameras and is constrained by browser
   sandboxing. Native preview via Tauri's WebView with frame
   injection (Phase 7) works against any source we can read.
3. **Multi-instance.** A single user wants to push *several*
   different sources to *several* different ATEM inputs (or
   destination devices) simultaneously. Each instance owns its own
   port pair (HTTP + BMD protocol), its own state directory, its
   own FFmpeg subprocess. macOS's `LSMultipleInstancesProhibited`
   defaults to NO so the .app already supports relaunching; we
   just need per-instance state isolation and a tiny launcher that
   discovers a free port pair.
4. **Cross-platform parity.** Mac arm64 already works; Windows
   x64 is plumbed through CI but lightly tested. v0.2.0 should
   include real Windows + Linux test rigs.

### Architectural decision (made 2026-04-26): Tauri

v0.2.0 is a port from Python to **Tauri (Rust shell + existing JS
UI)**. v0.1.0 (Python) is frozen on `main` and tagged
`v0.1.0-alpha.1`; all v0.2.0 work is on the `tauri-rewrite`
branch.

Why Tauri over staying-with-Python+ndi-python:
- Native NDI receive via `grafton-ndi` crate (spike-verified —
  discovered NDICAM on first run).
- Native preview frame injection unlocks the cleanest UX for
  virtual cameras (the current pain point).
- ~3 MB Tauri shell DMG vs ~150 MB PyInstaller bundle (FFmpeg
  sidecar pushes final size to ~85 MB — still half of v0.1.0).
- Single Rust+TS stack; type-safe protocol layer; less ambient
  Python interpreter overhead.

Cost: ~2-3 weeks to port the ~3,000 lines of Python across nine
phases. Phase 0 (this commit) is just the Tauri shell scaffold;
Phases 1-9 progressively port `bmd_emulator/*.py` into Rust
modules under `src-tauri/src/`. The existing JS UI in
`bmd_emulator/static/` is reused unchanged once Phase 1 wires up
the embedded Axum HTTP server.

Phase plan:
- **Phase 0** (✓ scaffold) — `src-tauri/`, signing config
  (Developer ID `6M536MV7GT`), Mac DMG + Windows NSIS targets,
  placeholder webui.
- **Phase 1** — Port `state.py` + `xml_loader.py` + start an
  embedded Axum HTTP server. Tauri webview navigates to
  `http://localhost:N`, existing JS UI runs unchanged.
- **Phase 2** — Port `sources.py` + `device_scanner.py`.
- **Phase 3** — Port `streamer.py` (FFmpeg subprocess + telemetry).
- **Phase 4** — NDI direct ingest via `grafton-ndi` (headline).
- **Phase 5** — Port `protocol.py` (BMD TCP on 9977).
- **Phase 6** — Multi-instance support.
- **Phase 7** — Native preview frame injection.
- **Phase 8** — UI/UX bundle (six user-requested tweaks listed
  below).
- **Phase 9** — CI rewrite (`cargo-tauri` matrix replacing
  PyInstaller).

### v0.2.0 UI / UX scope (queued)

- **New hero subtitle**: "Turn multiple worldwide video sources,
  from iPhones to Drones to NDI, into remote inputs that stream
  directly into your ATEM Switcher or Blackmagic Streaming
  Decoders / Bridges over the single ethernet cable. Even route
  Dante audio onto a video source that maps directly to one of
  your switcher's SDI or HDMI inputs!"
- **New "What it does" paragraph**: "If you have an ATEM Mini
  Extreme ISO G2, Television Studio HD8 ISO, or an upcoming
  qualifying ST2110 ATEM Switcher, you can change a local input
  into a remote input that can be sent over the public Internet
  directly to your switcher from anywhere in the world. Blackmagic
  Streaming Decoder and Streaming Bridges can also receive sources
  from anywhere with a stable enough internet connection
  (~2.5-3.5 Mbps upload), but previously this was limited to just
  other Blackmagic hardware. Now, NDI, SDI, HDMI, non-Blackmagic
  SRT and RTMP streams, etc. can all be converted into the special
  Blackmagic flavor of SRT using this app. This is more of a demo
  app showing what is now possible, a proof of concept, and should
  only be used in real productions at your own risk. It is free
  forever, until it either gets stopped by Blackmagic or they
  fully embrace opening up their powerful stream decoding
  ecosystem."
- **Destination address clarity** — show an explicit example
  format (e.g. `srt://192.168.1.50:1935` or
  `srt://relay.example.com:1935`) and call out that the **port
  matters** (most users miss this).
- **Quality settings chooser in the wizard** — currently buried
  in Advanced. Surface it at top level with **projected
  bitrates** (High = 6 Mbps, Medium = 4.5 Mbps, Low = 2.5 Mbps)
  and brief network-suitability text per option (fiber/cable,
  DSL, cellular).
- **RTMP/SRT relay re-design** — current implementation works but
  the UX is confusing. Goal: user sets a custom RTMP/SRT
  destination on their drone/camera/streaming device, that
  publishes to a server this app runs, the app re-encodes to BMD
  SRT and forwards to ATEM. Promote to a dedicated mini-wizard
  that expands when the user clicks an "I want to receive a
  stream" button — clearer step-by-step ("step 1: copy this URL,
  step 2: paste into your camera, step 3: start receiver, step 4:
  hit Start Stream").
- **Bottom-of-page user guide** — full-width section below the
  main grid containing:
  - **Visual schematic** of the data flow (Source → This app →
    Network → ATEM). SVG with annotated boxes.
  - **Latency facts** — SRT push 200-500ms typical, encoder
    50-100ms, total ~250-600ms end-to-end.
  - **Expandable FAQ** (likely qs: "Why is the input black on the
    ATEM?", "What's the minimum upload bandwidth?", "Can I use
    this with [non-ATEM device]?", "Is this Blackmagic-approved?").
  - **Mailto button** to <stephen@weirdmachine.org>
  - **Author website link** to <https://weirdmachine.org>
  - **GitHub repo link** to
    <https://github.com/amateurmenace/atem-ip-patchbay>

## What this is

Cross-platform proof-of-concept that pushes any video source into
Blackmagic ATEM gear (Mini Extreme G2, Television Studio, Streaming
Decoder) over the BMD-flavored SRT handshake. macOS arm64 (`.dmg`) +
Windows x64 (`Setup.exe`). MIT licensed.

Repo: <https://github.com/amateurmenace/atem-ip-patchbay>

The pitch: NDI / SDI / HDMI / non-Blackmagic SRT / RTMP all converted
into ATEM-acceptable SRT. Made possible by a differential analysis of
an iPhone Blackmagic Camera pcap vs. a Web Presenter pcap, which proved
the BMOS extension is optional and standard libsrt + HEVC + MPEG-TS +
the right `streamid` format works.

## Run / build commands

### v0.2.0 (Tauri — `tauri-rewrite` branch)

```sh
# Dev — opens the Tauri window with hot-reload on src-tauri/ changes
cargo tauri dev

# Mac build (.app + signed .dmg, ~1-2 min after first warm cache)
cargo tauri build
# Output: src-tauri/target/release/bundle/{macos,dmg}/...

# Windows build (run on Windows)
cargo tauri build
# Output: src-tauri/target/release/bundle/nsis/*.exe

# Compile-check only (fast, no bundle)
cargo check --manifest-path src-tauri/Cargo.toml
```

First `cargo tauri build` from a cold cache takes ~5-10 min
(~200 crate dependencies). Subsequent builds are 30-90 sec.

### v0.1.0 (Python — `main` branch, frozen at `v0.1.0-alpha.1`)

```sh
# Dev server — loads ./config/*.xml, opens browser to localhost:8090
python3 run.py

# Mac build (.app + signed .dmg, ~3-5 min)
python3 build/build.py
# Output: build/dist/ATEM IP Patchbay.app + ATEM-IP-Patchbay-0.1.0-arm64.dmg

# Windows build (run on Windows; macOS will refuse)
python build\build.py

# CI smoke (mirrors the GH Actions ci.yml check)
python3 -m py_compile bmd_emulator/*.py run.py probe.py
```

The dev server's HTTP port is 8090 by default and walks forward to
8091..8099 if taken (commit `607271d`). The BMD control protocol port
is 9977 with the same walk behavior.

**Important**: Dev server should be launched from the user's OWN
Terminal — not via the Bash tool. Camera permission attaches to the
launching process; a Bash-spawned `python3` inherits Claude Code's
permission (often missing), so AVF capture hangs silently.

## Architecture cheat-sheet

```
run.py                          # entry point — loads XMLs, starts protocol + HTTP servers
config/                         # streaming-service XMLs (real ones gitignored)
  example.xml                   #   tracked, placeholder host/key
  1935 Test.xml                 #   gitignored, real ATEM key (n1sn-...)
  Web Presenter 1.xml           #   gitignored, real ATEM key (j4fh-...)
bmd_emulator/
  state.py                      # EncoderState data model + snapshot dict
  web.py                        # HTTP control panel + JSON API
  static/                       # UI (single-page vanilla HTML/CSS/JS)
    index.html                  #   Destination wizard at top of right column
    app.js                      #   Wizard wiring, segmented controls, NDI hint
    style.css                   #   Segmented controls, format-warning, port-fwd help
  sources.py                    # avfoundation / dshow / gdigrab / pipe / srt_listen / rtmp_listen
  device_scanner.py             # AVF + DirectShow scan + AVF mode probe
  streamer.py                   # FFmpeg subprocess + telemetry monitor
  streamid.py                   # BMD streamid: bmd_uuid=...,bmd_name=...,u=KEY
  ffmpeg_path.py                # bundled-sidecar > PATH resolver (sys._MEIPASS)
  protocol.py                   # TCP 9977 BMD control protocol server
  discover.py                   # mDNS for _ndi._tcp.local.
  paste_parser.py               # parses any-shape destination input
  netinfo.py                    # LAN IP detection for relay-publish URL
build/                          # v0.1.0 (Python) PyInstaller pipeline — kept on main
  build.py                      # Make-style orchestrator (Mac OR Windows path)
  macos.spec / windows.spec     # PyInstaller specs
  installer.iss                 # Inno Setup script
  .cache/ .venv/ .work/ dist/   # all gitignored
src-tauri/                      # v0.2.0 Tauri shell — added in Phase 0 on tauri-rewrite
  Cargo.toml                    # name=atem-ip-patchbay, tauri 2, tauri-plugin-log
  tauri.conf.json               # productName, signing identity 6M536MV7GT,
                                # bundle targets [app, dmg, nsis], minimumSystemVersion 11
  build.rs                      # tauri_build::build()
  src/main.rs                   # binary entry — calls atem_ip_patchbay_lib::run()
  src/lib.rs                    # tauri::Builder::default().setup(...).run(...)
  capabilities/                 # ACL — what JS can invoke on the Rust side
  icons/                        # placeholder set from cargo tauri init
webui/                          # frontendDist target for Tauri (Phase 0 placeholder).
  index.html                    # Phase 1 swaps this for a redirect to the Axum HTTP server.
.github/workflows/
  ci.yml                        # PR / push: smoke-python on main (v0.1.0),
                                # cargo check on tauri-rewrite (v0.2.0).
                                # Branch-guarded `if`s pick the right job per ref.
  release.yml                   # tag-driven matrix build + GH release.
                                # Phase 9 rewrote this for cargo-tauri (Mac
                                # arm64 + Win x64 NSIS); Mac signing via the
                                # MACOS_CERTIFICATE_P12 / MACOS_CERTIFICATE_PWD /
                                # MACOS_KEYCHAIN_PWD / MACOS_SIGN_IDENTITY
                                # secret bundle. NDI dylib bundling is NOT in
                                # the v0.2.0 release pipeline yet — end users
                                # need NDI Tools installed for NDI features
                                # (small footnote in release notes).
```

## Conventions

- **Commit messages**: detailed, "why" not "what". User likes the style
  of recent commits (e.g. `b29b9f6`, `390594a`). Co-author tag at
  bottom: `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`.
- **License**: MIT. The packaged FFmpeg sidecar (Jellyfin GPL on Mac,
  BtbN GPL on Windows) is distributed under GPL with attribution in the
  README — separate executables, "aggregate" relationship.
- **No autopush**: confirm with the user before `git push` unless
  explicitly told to.
- **No emoji** in code or text output unless the user asks. ASCII glyphs
  preferred for cross-platform output (Windows cp1252 console can't
  encode many unicode characters).
- **No new files unless asked**. Especially no docs / READMEs.
- **No tests** yet — CI just compile-checks + smoke-tests the HTTP server.
- **Branch strategy**: solo dev, everything goes on `main`.

## Real keys live where (DO NOT COMMIT)

`config/1935 Test.xml` and `config/Web Presenter 1.xml` are gitignored
and contain real ATEM stream keys. NEVER commit them. NEVER include
their contents (especially key strings) in commit messages or anywhere
the LLM might write to a tracked file. The `.gitignore` allowlist is
configured so only `config/example.xml` is tracked.

## Code-signing

Mac builds are signed with `Developer ID Application: Stephen Walter
(6M536MV7GT)` — the build script auto-detects the identity from
keychain via `security find-identity -v -p codesigning`. Override with
`SIGN_IDENTITY=...` env var.

**Notarization is deferred** for the alpha. To enable later: run
`xcrun notarytool store-credentials` once, then add `xcrun notarytool
submit ... --wait` + `xcrun stapler staple` steps to `build.py` after
the create-dmg step.

Windows builds are unsigned (alpha doesn't have an EV cert).
SmartScreen prompts users with "More info → Run anyway" — recoverable.

## Currently-open issues

### 1. NDI Virtual Camera: confirmed-broken via FFmpeg AVF (workaround = v0.2.0 NDI SDK)

**Confirmed not fixable via FFmpeg flags after extensive iteration.**

Symptom: clicking the NDI Virtual Camera tile and hitting Start Stream
→ FFmpeg opens the AVF device cleanly (`Stream #0:0: Video: rawvideo
(UYVY), uyvy422, 1920x1080`) → enters main loop ("Press [q] to stop")
→ no frame callbacks fire → `frame=0` forever, audio bytes accumulate
from any paired audio input. Encoder gets nothing on the video side.

What we ruled out via testing:
- Device-name vs. index addressing (commit `b29b9f6`) — no change
- Mode probe + framerate match (commit `b29b9f6`) — no change
- Dropping `-pixel_format` so FFmpeg auto-negotiates (commit `13e4129`)
  — no change; AVF auto-overrode yuv420p to uyvy422 correctly
- Stop browser preview before starting FFmpeg (commit `80c367a`)
  — no change
- Splitting video + audio into separate AVF sessions
  — no change (tested, reverted in this commit)
- Bumping `-thread_queue_size 1024` — no change

What we know works against the same device:
- **Photo Booth** plays NDICAM video live (proves NDI Virtual Input
  → NDI Virtual Camera AVF bridge is healthy at the OS level)
- The browser's `getUserMedia` path shows live preview in our own UI
  (proves the AVF device delivers frames to high-level
  `AVCaptureSession` consumers)

Diagnosis: FFmpeg's `avfoundation` indev uses a lower-level
`AVCaptureDeviceInput` + `AVCaptureVideoDataOutput` callback path
that some virtual cameras don't service. This is a recurring
complaint in the OBS / FFmpeg / NDI Tools issue trackers and has
no flag-level fix. AVCaptureSession (Photo Booth, getUserMedia)
works; AVCaptureVideoDataOutput callbacks (FFmpeg) doesn't.

**The real fix is direct NDI ingest.** Bypass NDI Virtual Camera /
AVF entirely; receive NDI frames in Python via the NewTek NDI SDK
(`ndi-python` binding loads `libndi.dylib` from `/usr/local/lib/`,
which NDI Tools installs); pipe raw frames to FFmpeg's stdin.
Estimated work: ~1 day. See "v0.2.0 candidate features" → "Direct
NDI ingest".

For the **alpha**, document NDI Virtual Camera as a known limitation:
"Direct NDI ingest is not supported in v0.1.0; use OBS Virtual
Camera (which works because OBS implements both the high-level and
low-level AVF callback paths), the SRT/RTMP relay listener, or wait
for v0.2.0." Source factory keeps the simple combined `name:name`
form that works for hardware webcams.

### 2. .app's bundled XML is only the placeholder

The Mac `.app` PyInstaller bundle includes `config/example.xml` (host
`your-atem-or-streaming-bridge.example.com`), so first-launch users
can't stream until they set the wizard's Address field. The dev server
loads the user's real XMLs from on-disk `config/`, so dev testing has
real destinations.

Fix options:
- Add **placeholder-URL detection** in the wizard render: if
  `current_url` contains `example.com`, show a yellow warning ("set
  your real ATEM address in the Address field above").
- Have the `.app` scan `~/Library/Application Support/ATEM IP Patchbay/config/`
  on launch for user-supplied XMLs.

### 3. AVFoundation index → name fallback isn't airtight

State stores both `av_video_index` and `av_video_name` (commit
`b29b9f6`). UI sends both on tile click. Source factory uses names
when present, indices as fallback. But if state is set without names
— e.g. via `/api/settings` POST without `av_video_name`, or defaults
from `run.py` at boot — it falls back to indices and the original
"clicked the wrong device" bug returns.

Fix: have `find_default_video_index` / `find_default_audio_index` in
`device_scanner.py` ALSO populate the name fields when they pick a
default at boot. Mirror that anywhere index is set without a name.

### 4. Format-probe latency

Every Start Stream re-probes the AVF device's supported modes (~1 sec).
Could cache per-device-name with a 60-sec TTL alongside the existing
device-list cache. Low priority.

## Things DONE this session (≤ commit b29b9f6)

- Mac `.app` builds + signs with Developer ID Application identity
  (6M536MV7GT). create-dmg packaging. Notarization deferred.
- Windows build pipeline (`build/build.py` Windows branch +
  `windows.spec` + `installer.iss`). Verified by user on real Windows
  hardware after fixing 3 PyInstaller bugs (utf-8 stdout, venv path,
  PyInstaller pin for Python 3.14).
- DirectShow scanner handles modern BtbN FFmpeg output format —
  `[in#N @ ...]` prefix, inline `(audio, video)` / `(none)` markers.
  Verified against user's 11-device sample.
- Destination wizard at top of right column. Format selector with
  yellow warning + live `1920 × 1080 @ 30 fps` decode + "how to find
  your switcher format" expandable. Port-forwarding 101 expandable.
- Port-walk fallback (8090→8099, 9977→9986) so a stale instance can't
  silently brick a launch.
- AVF device-NAME-based addressing + mode probe. NDI Virtual Camera's
  locked 1080p60 mode is now correctly identified; FFmpeg command
  built with the matching `-framerate 60 -video_size 1920x1080`.
- GH Actions: `ci.yml` on PR + `release.yml` on tag push (Mac arm64 +
  Windows x64 matrix, attaches `.dmg` / `Setup.exe` to GitHub Release
  with auto-generated notes). CI is green on `main`.
- NDI inline-hint UX: clicking a discovered NDI sender shows an inline
  hint with a one-click "Use NDI Virtual Camera + NDI Audio" bridge
  button.
- SRT/RTMP relay sources (`srt_listen` / `rtmp_listen`) — turn the
  patchbay into a server for OBS / Larix / iPhone to publish into.

## Latest commits

```
b29b9f6 Fix two AVFoundation source bugs surfaced by NDI Virtual Camera
e6eae96 Make NDI sender clicks discoverable + actionable
8e9f52e GH Actions: CI smoke + tag-driven release pipeline
390594a Redesign Destination as a top-of-column wizard
607271d Port-walk fallback so a stale instance can't silently brick a launch
5bbaf59 Fix dshow parser for modern FFmpeg output format
b9f1995 Fix three Windows-build bugs found on first real run
d671b51 Add macOS PyInstaller build pipeline (signed .dmg)
75b9304 Add SRT/RTMP relay sources — turn the patchbay into a server
1f56183 Initial commit: ATEM IP Patchbay v0.1.0
```

## What's next (priority order if picking up cold)

1. **Fix NDI Virtual Camera streaming** — see "Currently-open issues #1".
   Either find the missing FFmpeg flag / pixel-format / permission
   piece, or definitively rule it out as a code bug and improve the
   "no frames received" UX so the user understands they need to bind
   NDI Virtual Input to NDICAM in its menu bar.
2. **Notarize the Mac `.dmg`** so first-launch downloaders don't get
   the Gatekeeper warning. One-time `xcrun notarytool
   store-credentials` setup + a build-script step.
3. **Cut `v0.1.0-alpha.1` tag** (`git tag v0.1.0-alpha.1 && git push
   --tags`) and let the GH Actions release pipeline produce both
   binaries.
4. Polish: format-probe caching; placeholder-URL warning; default-
   device names at boot.

## v0.2.0 candidate features

- **Direct NDI ingest** via custom FFmpeg with `libndi_newtek` (~1-2
  days, would eliminate the NDI Virtual Camera bridge dependency).
  Compile FFmpeg from source with libndi enabled; ship as the sidecar.
  Inherits NDI SDK attribution requirement.
- **NDI Discovery Server** support — query the centralized server's
  HTTP API instead of relying on multicast mDNS. Lots of NDI deploys
  use this. Config file lives at
  `~/Library/Application Support/NewTek/NDI/ndi-config.v1.json` (Mac).
- **Universal2 Mac binaries** — currently arm64-only. Add a `macos-13`
  matrix entry to `release.yml` + a `lipo`-merge step to combine
  arm64 + x86_64 FFmpeg sidecars.
- **First-run wizard** — when no XML is loaded and `custom_url` is
  empty, show a 3-step wizard ("Where's your ATEM?" → "Paste your
  stream key" → "Pick a video source") instead of the current
  always-on wizard.
- **Source thumbnails** on tiles — periodic 1-frame capture from each
  AVF device for the tile background.
- **Persistent state** — save last-used destination + label + codec
  to `~/Library/Application Support/ATEM IP Patchbay/state.json`
  (and `%APPDATA%/...` on Windows).
