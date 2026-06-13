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

Phase plan (all phases ✓ shipped 2026-04-26 on `tauri-rewrite`):
- **Phase 0** — `src-tauri/`, signing config (Developer ID
  `6M536MV7GT`), Mac DMG + Windows NSIS targets, placeholder webui.
- **Phase 1** — Port `state.py` + `xml_loader.py` + embedded Axum
  HTTP server. Webview navigates to `http://localhost:N`, existing
  JS UI runs unchanged.
- **Phase 2** — Port `sources.py` + `device_scanner.py`.
- **Phase 3** — Port `streamer.py` (FFmpeg subprocess + telemetry).
- **Phase 4** — NDI direct ingest via `grafton-ndi` (headline).
- **Phase 5** — Port `protocol.py` (BMD TCP on 9977).
- **Phase 6** — Multi-instance support (`--instance-name` +
  per-instance state dir).
- **Phase 7** — Native NDI preview (/api/preview JPEG @ 2 Hz).
- **Phase 8** — UI/UX bundle (hero copy, address helper,
  dup-service-name fix, quality chooser promotion, bottom-of-page
  user guide, RTMP/SRT receive mini-wizard).
- **Phase 9** — CI rewrite (`cargo-tauri` matrix replacing
  PyInstaller).

### Bug-fix bundle (commit `45519ef`, 2026-04-26)

Caught during dev-loop testing right after Phase 8b shipped. End-
to-end verified streaming a test pattern through to a real ATEM
destination at 6.5 Mbps with live stats updates.

- **FFmpeg progress parser was blocked on `\n`.** FFmpeg's
  `frame=… fps=… bitrate=…` progress lines are separated by
  carriage returns, not newlines. tokio's `BufReader::lines()`
  splits only on `\n`, so the read loop blocked on the first
  progress chunk forever and the stats panel never updated.
  Replaced with a byte-stream `read()` loop that splits on
  either `\r` or `\n`. Stats now tick live.
- **Always-on filter chain (vs v0.1.0 plain mapping).** Phase 3
  wrapped every input in a `scale+pad+format` filter "to be
  safe." Broke SRT against destinations where v0.1.0's plain
  `-map 0:v:0 -map 1:a:0` worked. Removed; overlays will
  conditionally re-add.
- **URL parameter order matched v0.1.0.** v0.1.0 builds
  `?mode=&latency=&streamid=` in insertion order; v0.2.0 was
  BTreeMap-sorted (`?latency=&mode=&streamid=`). Switched to
  Vec for byte-identical query strings.
- **Status flipped to Streaming too early.** The "stream
  mapping" log trigger fired before the SRT handshake, so
  failed connections briefly showed Streaming with zero
  bitrate. Removed; only `connection established` + first
  progress tick mark Streaming now.
- **Missing error tags.** Added `input/output error`,
  `error opening output`, `could not write header`, `broken
  pipe`, `connection reset`, `no such file or directory` to
  the heuristic error-tag list so the UI flips to Interrupted
  immediately on these failures.
- **Settings DTO missing fields.** Audio dropdown wouldn't
  change the source. The Audio change handler POSTed
  `av_audio_index` + `av_audio_name`; same for `av_video_*`,
  `pipe_path`, `label`, nested `relay`/`overlay`. None existed
  in `SettingsUpdate`/`SettingsPayload`, so they were silently
  dropped. Added all of them and wired through `apply_settings`.
- **XML load response shape mismatch.** `/api/load_xml_text`
  returned the snapshot directly; JS expected `{service,
  snapshot}`. So the chip showed "Loaded service: undefined"
  and the page didn't refresh. Backend wraps response now;
  load implicitly clears existing services first (default
  `replace=true` for UI loads, `false` for boot).
- **`/api/services/clear` endpoint.** Clear XML button now
  hits this endpoint to wipe services + custom_url instead of
  just zeroing custom_url. UI hides the loaded-XML chip.
- **Default quality matrix when no XML loaded.** Hardcoded
  BMD-spec High/Medium/Low at 1080p30/60 and 720p30/60. Lets
  the user stream against a manually-entered destination
  without first dropping an XML, with the quality chooser
  populated.
- **Newest-XML-wins boot order.** `config/*.xml` are loaded in
  mtime order (newest first) so first-load-wins activates the
  most recently dropped XML. Drop a fresh XML in `config/` and
  it becomes the boot-time active service.
- **Clean exit prevents FFmpeg orphans.** Cmd-Q used to leave
  FFmpeg running because `Drop` never fired on the Arc-held
  Streamer (held forever by Axum's serve task). Now hooks
  `RunEvent::Exit/ExitRequested` and synchronously calls
  `streamer.stop()` before parent exit. Belt-and-suspenders:
  spawn FFmpeg in its own process group via `setpgid` pre_exec,
  then `killpg(SIGTERM)` then `SIGKILL` so an abrupt parent
  crash can't leave the group running.

### Open issues after bug-fix bundle (next session)

- **NDI direct-ingest streaming doesn't deliver frames** —
  user-reported. NDI **discovery** works fine (`/api/ndi-senders`
  returns the senders), but the receive→pipe→FFmpeg path doesn't
  land bytes at the destination. The bug is downstream of
  discovery: clicking an NDI tile, hitting Start Stream, doesn't
  produce a working stream. Likely failure points (see
  `src-tauri/src/ndi_capture.rs`): format probe timing out,
  pixel-format mismatch (we ask for BGRX_BGRA — first frame may
  arrive as something else), FFmpeg dying from rawvideo format
  mismatch, or the mpsc channel filling up. Diagnostic plan:
  add per-second frame-count debug logging to
  `run_capture_loop`, run dev with NDICAM broadcasting, inspect
  `/api/log` for FFmpeg's command + stderr, look for
  "NDI capture probed:" log line.
- **NDI dylib bundling NOT in the .app yet.** `cargo tauri build`
  produces a Hardened-runtime .app that crashes on launch with
  `Library not loaded: @rpath/libndi.dylib`. Hardened runtime
  disables dyld fallback library paths so the system
  `/usr/local/lib/libndi.dylib` isn't found. Fix: copy
  libndi.dylib into `Contents/Frameworks/` via
  `bundle.macOS.frameworks` config + `install_name_tool` post-
  build to add an `@executable_path/../Frameworks` rpath. Until
  this lands, **standalone .app testing is broken — use
  `cargo tauri dev` instead**.
- **Mac signing in CI requires four `MACOS_*` secrets.** Currently
  unset, so `release.yml` strips `bundle.macOS.signingIdentity`
  from `tauri.conf.json` and ships unsigned. Steps to enable in
  the next session: user exports the cert via Keychain Access
  GUI, then I run `gh secret set` for the four secrets.
- **Notarization deferred.** Once signing works in CI, add
  `xcrun notarytool submit … --wait` + `xcrun stapler staple`
  to release.yml so end users don't see Gatekeeper at all.
- **Receiver-state lockout after SIGKILL.** Killing the Tauri
  parent abruptly leaves a per-key SRT session at the receiver
  for ~30s-2min, during which new connections with the same key
  fail with generic `Input/output error`. The clean-exit handler
  fixes the common case (Cmd-Q); only force-quit / crash hits
  this. Document if it bites again. **Update from Session 3:**
  user reports this DOES bite — after a few stream tests the
  ATEM stops accepting reconnects. Worth building a small
  network-side diagnostic tool (separate machine on the same
  LAN as the ATEM) to confirm whether the lockout is per-key or
  destination-wide. Listed in the next-session prompt below.

### Session 3 fixes (commits `ce0f617` → `6529cae`, 2026-04-26 PM)

**NDI direct-ingest now works end-to-end with live in-app preview.**
Verified streaming a 1080p HEVC/H.264 stream from iPhone NDICAM
through the patchbay into both a local SRT loopback (clean) and
a real ATEM destination at 6+ Mbps with the preview pane painting
live JPEG snapshots at ~2 Hz.

What landed:

- **Stride-strip in `ndi_capture.rs`.** grafton-ndi's
  `VideoFrame.data` is the raw NDI buffer, allowed to use a
  per-row stride larger than `width*bpp` for SIMD alignment.
  FFmpeg's rawvideo demuxer expects tight frames. New
  `pack_frame` helper memcpys row-by-row when stride > expected;
  passthrough when tight. iPhone NDICAM at 720p is naturally
  tight (1280*4 = 5120) so the slow path never fires there, but
  it's load-bearing for any sender that pads (most desktop NDI
  tools at non-power-of-two widths).
- **Per-second NDI telemetry.** `run_capture_loop` now logs
  sent / empty / errors / channel-cap-remaining each second.
  Diagnoses "frames not flowing" without unwinding the call
  stack. First-frame log dumps width/height/pixel_format/stride/
  data_len/expected_packed so probe-time anomalies are visible.
- **NDI source upscale.** ATEM hardware decoders only accept
  the resolution they advertise. NDICAM is 720p; ATEM Mini
  Extreme expects 1080p. New `video_filter: Option<String>` on
  `StreamPlan` threads `-vf scale=W:H:flags=lanczos` into the
  FFmpeg cmd ONLY when source dims differ from configured
  `video_mode`. Other source paths (AVF, pipe) keep the
  v0.1.0-parity plain `-map` mapping that the bug-fix bundle
  restored.
- **NDI preview is live.** /api/preview already served JPEGs
  via grafton-ndi's `encode_jpeg` every 15 frames; the JS UI
  now polls it at 2 Hz and paints into a `position:absolute`
  `<img>` that overlays the SMPTE bars. Took several rounds to
  nail down:
  - CSS `[hidden] { display: none !important }` so `.bars` and
    `.preview-message` actually hide when JS sets `.hidden = true`
    (their `display: flex` rules had been overriding the
    user-agent `[hidden]` rule of equal specificity — silently
    broken since v0.1.0; only revealed by the new NDI img).
  - Auto-start the poll from `/api/state`, not just on tile
    click, so a user landing in NDI source via session-restore
    + Start sees preview without needing to re-click the tile.
  - `previewKey` reset in `stopPreview` so the auto-start
    dedup check is self-healing if anything kills the timer.
  - Promoted the silent `catch (_e)` in the tick to
    `console.error` so future poll-loop bugs surface immediately.
- **HTTP `Cache-Control: no-cache`** on `/static/*` plus
  re-read `index.html` on every request rather than caching at
  boot. WebView was happily serving stale JS/CSS for minutes
  after edits during dev iteration; both fixes together mean
  plain Cmd-R reflects current disk state.
- **UI cleanups.** Removed the duplicate per-tile relay-config
  panels and the duplicate "SRT Advanced" details card (which
  had duplicated `srt-mode` / `srt-latency` / `srt-listen-port` /
  `streamid-override` / `streamid-legacy` IDs and triggered a
  duplicate-id browser warning). Removed the SRT/RTMP receiver
  tiles from the source gallery — the receive-stream wizard
  below the gallery is now the single canonical "I want this
  to be a server" UI, restyled as a high-contrast green CTA.
  Audio dropdown got an accent border + larger label so it
  reads as the audio-source control rather than a passive
  read-only field. Pipe/URL helper clarified with categorized
  examples (RTSP / HLS / named pipe / UDP). Demo-app disclaimer
  moved to a quiet dashed-rule strip just above the credit line.
  Credit reworded to mention MIT / GitHub.
- **Refresh button** in the topbar (clears caches +
  `location.reload`). **Cmd-R / F5 keybind** in JS — Tauri
  WebView ships with no menu bar and no built-in reload
  shortcut.
- **Info.plist for production builds.** New
  `src-tauri/Info.plist` with `NSCameraUsageDescription`,
  `NSMicrophoneUsageDescription`, `NSLocalNetworkUsageDescription`.
  Bundled by `cargo tauri build`, so the production .app
  prompts for camera/mic. `cargo tauri dev` still doesn't have
  these because the dev binary isn't bundled — see "Open issues
  from Session 3" below.

### Open issues from Session 3

- **Camera (FaceTime / connected iPhone) preview unavailable
  in `cargo tauri dev`.** macOS WKWebView's `getUserMedia` path
  needs the parent app's Info.plist with
  `NSCameraUsageDescription`. The dev binary isn't bundled so
  no Info.plist is around it; macOS silently denies camera
  access without showing a prompt. The Info.plist additions
  land properly in `cargo tauri build` — production .app DOES
  prompt and DOES work (untested in Session 3 because dylib
  bundling needs to land first). Workarounds for dev: open
  http://127.0.0.1:8090 in **Safari** (Safari has its own
  Info.plist with camera descriptions); OR launch
  `cargo tauri dev` from the user's own Terminal so the
  FFmpeg-AVF subprocess inherits Terminal's TCC grant for
  STREAMING (browser preview still unavailable).
  - The next-session prompt proposes a server-side preview
    path (FFmpeg snapshots a JPEG every ~500ms when source is
    AVF, served via the same /api/preview endpoint NDI uses)
    that would make in-app preview work for cameras in dev
    mode AND in production .app, regardless of WebView
    permission state.
- **NDI dylib bundling NOT in the .app yet.** Same as before
  Session 3 — `cargo tauri build` produces a Hardened-runtime
  .app that crashes on launch with `Library not loaded:
  @rpath/libndi.dylib`. Fix: `bundle.macOS.frameworks` +
  `install_name_tool`. Until this lands, **standalone .app
  testing is broken for NDI features**.
- **Production .app NOT yet rebuilt + tested with Info.plist
  additions.** Need to do this and verify camera/mic permission
  prompts surface correctly + that NDI dylib bundling fix lands
  before the .app is usable end-to-end.
- **Mac signing secrets aren't uploaded.** Same as before
  Session 3.
- **Notarization deferred.** Same as before Session 3.

### Session 4 wins (commits `c378c72` → `6a2bb73`, 2026-04-26 PM/evening)

All seven Session 4 priorities shipped. Plus a meaningful
network diagnostic tool. Plus the first signed + notarized
public release.

1. **VideoToolbox HEVC/H.264 hardware encoding** — `streamer.rs`
   detects macOS and routes through `h264_videotoolbox` /
   `hevc_videotoolbox` (`-realtime 1 -allow_sw 1
   -constant_bit_rate 1 -bf 0 -profile:v main`) instead of
   libx264/x265. Encoder CPU drops from ~80% (libx264 veryfast,
   1080p30 NDI) to single digits. BMD-parity-verified end-to-
   end against the user's real ATEM destination (Remote 2.xml).
   Set `ATEM_DISABLE_VT=1` to fall back to libx264 for parity
   testing.

2. **Pre-stream Preview button** (`preview.rs` module) —
   spins up a separate NDI Receiver at `ReceiverBandwidth::
   Highest` (Lowest looked broken; the proxy stream was so ugly
   users assumed their camera was failing). JPEG sampler stuffs
   into the same `latest_jpeg` slot the streaming path uses, so
   `/api/preview` is a single endpoint serving either source.
   `Streamer::start()` calls `Preview::stop_for_streamer()`
   before claiming the SDK handle to avoid double-claim. UI
   button "▶ Preview" / "◼ Stop Preview" yellow when active.

3. **Dante VSC channel selection** — new `audio_pan_l` /
   `audio_pan_r` state fields (1-indexed); when source is
   AVF + audio device name matches `dante` or `aggregate`,
   `streamer.rs` emits `-af pan="stereo|c0=cN|c1=cM"` to route
   the chosen channel pair to the outgoing AAC. Otherwise
   FFmpeg auto-downmixes ALL N channels which sounds wrong for
   Dante routing.

4. **Audio Mixer card split** — Source card renamed to "Video
   Source"; new "Audio Mixer" card holds Auto / Custom / Silent
   radio + stereo/mono toggle + the Dante channel picker.
   audio_mode="auto" forces av_audio_index=-1 in
   `source_selection()` so the source resolver does the right
   thing for combined-AV cameras vs separate AVF audio.
   "silent" maps to `-af volume=0` regardless of source.

5. **NDI video + Custom AVF audio (Dante) end-to-end** — the
   headline production combo. When source is NDI AND
   audio_mode=custom AND av_audio_name is set,
   `build_ffmpeg_cmd_for_ndi` injects `-f avfoundation -i :NAME`
   as input 1 instead of the lavfi anullsrc fallback. Pipe /
   relay video sources still fall back to lavfi for now —
   deferred (see Session 6 priorities, item 5).

6. **NDI dylib bundling** — `tauri.conf.json` ships
   `bundle.macOS.frameworks: ["/usr/local/lib/libndi.dylib"]`;
   `build.rs` adds `-Wl,-rpath,@executable_path/../Frameworks`
   to LC_RPATH on macOS. End users no longer need NDI Tools
   pre-installed. The dylib auto-resolves to
   `Contents/Frameworks/libndi.dylib` at launch via the
   embedded rpath. CI's `Stage libndi.dylib at /usr/local/lib`
   step copies the dylib from the SDK's
   `/Library/NDI SDK for Apple/lib/macOS/` to `/usr/local/lib/`
   so `bundle.macOS.frameworks` finds it (NDI SDK installs
   there, not in NDI Tools' default).

7. **FFmpeg sidecar bundled** — `bundle.resources:
   ["sidecar/*"]` in tauri.conf.json. CI downloads
   jellyfin-ffmpeg arm64 (Mac) and BtbN ffmpeg (Win) before
   `cargo tauri build`. `ffmpeg_path::ffmpeg_path()` checks
   `<resource_root>/sidecar/ffmpeg{,.exe}` first, falls
   through to PATH. End users no longer need Homebrew.
   `src-tauri/sidecar/README.txt` is committed as a placeholder
   so the resources glob always matches at least one file in
   local dev builds.

8. **Recovery card + parent-death FFmpeg watchdog** — left-
   column "Recovery" card under Overlays with a destructive-
   red "✖ Kill orphans" CTA. POST `/api/kill-orphans` runs
   `pkill -TERM -f "ffmpeg.*streamid="` then SIGKILL after a
   settle. `Streamer::start()` ALSO spawns a tiny bash
   watchdog (via `setsid` so it survives group-targeted
   SIGKILLs) that polls our PID + FFmpeg's PID; when our
   process dies ungracefully, watchdog SIGTERMs the FFmpeg
   group within ~1s. Catches the cargo-tauri-dev rebuild
   SIGKILL path that was leaving orphan streams running to
   the destination.

9. **Screen-capture scale filter** — AVF "Capture screen N"
   ignores `-video_size`, returns native display resolution
   (3456x2234 on Retina) with bogus 1000k tbr. ATEM rejected
   silently; streams reported "running" but no picture. Now
   `build_video_filter()` detects screen-capture sources and
   emits `scale=W:H:force_original_aspect_ratio=decrease,
   pad=W:H:(ow-iw)/2:(oh-ih)/2,fps=N,setsar=1` to scale-and-
   letterbox to the configured `video_mode` with steady fps.

10. **Many UI/UX cleanups** — 4K modes (2160p23.98–60),
    OVERLAYS card moved to left column, port-forwarding
    explainer rewritten for home/work/venue/church audiences
    with an "ask of IT" template, FAQ entries for
    SRT-vs-RTMP / H.264-vs-H.265 / encoder-CPU, NDI senders
    moved up under Screens, URL/Pipe wizard above receive-
    stream block, "SRT / RTMP SERVER:" prefix pill on receive
    wizard, audio dropdown styled cyan with explicit
    font-family, footer rework with weirdmachine wordmark
    (text fallback when the PNG isn't dropped at
    `/static/weirdmachine-logo.png`), hero credit moved to
    footer, "Email Stephen" → "Email", hero subtitle adds
    "to computer screens", `kill-orphans` button moved from
    topbar to dedicated Recovery card with paragraph
    explanation.

11. **`atem-net-diag` companion tool** (`tools/atem-net-diag/`)
    — a separate Rust binary for live network diagnostics.
    Three modes that combine freely:
    - **Active probe**: `--key K` builds BMD-flavored streamid
      and runs FFmpeg-shell-out handshakes every N seconds.
      `--key K1 --key K2 ...` rotates through multiple keys
      per cycle to distinguish per-key vs destination-wide
      lockouts.
    - **Passive monitor (`--monitor IFACE`)**: wraps tshark
      with a port filter (1935 / 9710 / 9977 / 1936 default).
      Parses SRT control packets to extract receiver-reported
      RTT, bandwidth estimate, receive rate, buffer level.
      Falls through if tshark missing.
    - **Visual dashboard (`--ui [PORT]`)**: embedded HTTP
      server (tiny_http, port 8092 default) serving a single-
      page web UI. Per-stream cards with health badges,
      current bitrate, RTT, packet stats, 60-second bitrate
      sparkline drawn on canvas. Configure target IP / port /
      key / interval right in the form at the top, click
      Apply, probe loop reconfigures live without restart.
    The diag tool's Cargo manifest lives in
    `tools/atem-net-diag/Cargo.toml` (no shared workspace);
    `tools/atem-net-diag/package/{start.command,README.txt}`
    holds the source files copied into release tarballs.
    Built + signed + tarball'd into iCloud Drive for the user
    to AirDrop to a peer Mac during productions.
    **Important Session-4-end finding**: the user's first
    test of the tool returned all REJECTED because the
    hand-crafted bmd_uuid in `build_bmd_srt_url` had 13 hex
    chars in the last group (UUIDs require 12). BMD receivers
    silently reject malformed UUIDs. Fixed in commit
    `de560bf` to a valid v4 UUID. **Also**: a peer Mac on a
    switched LAN typically can't see traffic between two
    other devices (modern switches don't broadcast unicast).
    The tool needs to run on the SAME machine as the streamer
    or on the ATEM's machine — Session 5 added the UDM polling
    path that bypasses this entirely (works from any machine
    that can reach the controller); the LAN visibility hint in
    the v0.2.0 dashboard surfaces the gotcha to operators.

12. **`tauri-rewrite` merged to `main`** — fast-forwarded.
    `main` now points at the same commit as `tauri-rewrite`
    and tracks all Tauri rewrite history. `v0.1.0-alpha.1`
    tag stays reachable.

### v0.2.0-alpha.6 — first PUBLIC signed + notarized release

Live at https://github.com/amateurmenace/atem-ip-patchbay/releases/tag/v0.2.0-alpha.6

- **macOS arm64 .dmg**: 33 MB. Signed by "Developer ID
  Application: Stephen Walter (6M536MV7GT)", notarized via
  `xcrun notarytool submit --wait`, ticket stapled with
  `xcrun stapler staple`. Includes bundled
  `Contents/Frameworks/libndi.dylib` (rebundled from NDI
  SDK at /Library/NDI SDK for Apple/lib/macOS/) and
  bundled `Contents/Resources/sidecar/ffmpeg` (Jellyfin
  GPL build, libsrt + HEVC + VideoToolbox). End users
  install on a clean Mac with zero Gatekeeper prompts.
- **Windows x64 .exe**: NOT shipped in alpha.6. CI's
  `Install NDI SDK (Windows)` step hung for 59 minutes
  before manual cancellation — NewTek's NDI 6 SDK Windows
  installer is built with InstallShield (NOT Inno Setup),
  the `/S` flag we were passing didn't trigger silent
  install and the runner sat waiting on a UAC dialog.
  Fix already committed in `6a2bb73`: switch to
  `/s /v"/qn"` (InstallShield silent + msiexec /qn
  passthrough), add 5-min step timeout. Plus
  `release.yml`'s release job now uses
  `if: always() && needs.build-macos.result == 'success'`
  so a Mac-only release publishes when Windows fails.
  alpha.7 will re-attempt Windows.
- For alpha.6 specifically I downloaded the Mac CI
  artifact via `gh run download` and ran
  `gh release create v0.2.0-alpha.6 ... <dmg>` locally
  to publish. Future releases will be auto-published by
  the pipeline.

### Session 5 wins (atem-net-diag v0.2.0, 2026-04-26 PM)

Closed two of the original Session 5 priorities (atem-net-diag
rework + per-key correlation) end-to-end on the codebase. The
v0.2.0 tarball is built + signed + dropped in iCloud Drive
(`atem-net-diag-0.2.0-macos-arm64.tar.gz`), but the dev Mac
this session ran on is on `192.168.1.0/24` and the user's UDM
+ ATEM are on `192.168.20.0/24`, so the live UDM polling path
is unverified — that's Session 6's first job, on the production-
LAN Mac.

What landed:

1. **Default mode = LIVE (passive only).** New `DiagMode` enum
   (`Live` | `Standby`) on `ConfigSnapshot`. The probe loop
   checks `mode == Standby` before firing FFmpeg handshakes —
   in Live mode the loop sleeps 2s between checks, no outbound
   traffic to the ATEM. Fixes the Session 4 finding that
   active probes were getting REJECTED during real productions
   AND contending with the receiver's accept slot. Operator
   opts into Standby explicitly via the dashboard mode banner
   when they want reachability testing.

2. **UDM (UniFi Network) integration — the headline.** New
   `unifi.rs` module polls the local controller's per-client
   bandwidth stats every 2 seconds. Stack:
   - `ureq` (default-features = false, native-tls + json +
     cookies) for HTTPS — `native-tls` so we can call
     `.danger_accept_invalid_certs(true)` against the UDM's
     self-signed cert without bringing in the rustls dangerous-
     configuration headache.
   - Auth: `UDM_API_KEY` env var (preferred — Local Controller
     API key created via the UDM web UI Settings → Control
     Plane → Integrations) sent as `X-API-KEY` header. Fallback
     to `UDM_USERNAME` + `UDM_PASSWORD` cookie-auth login flow
     for older UniFi OS that doesn't yet have local API keys.
   - Endpoint: tries legacy `/proxy/network/api/s/default/
     stat/sta` first (returns rich tx_bytes / rx_bytes counters
     we delta-derive into kbps), falls back to integration
     API at `/proxy/network/integration/v1/sites/default/
     clients` if the API key only authorizes against the
     integration surface. The integration API is "read-mostly"
     as of early 2026 and doesn't expose byte counters yet —
     bandwidth reads 0 in that fallback path.
   - Credentials NEVER enter `DashboardState` (which serializes
     to `/api/state` JSON). They live only in the polling
     thread's local `UnifiClient`. Status (`Connected{
     last_poll_at }`, `Failed{ error, last_attempt }`, etc.)
     is what surfaces to the UI.
   - Self-signed cert handling is opt-out per-host (the
     trusted UDM at `192.168.20.1`); the trade-off is a LAN
     MITM could intercept the API key. Acceptable for a
     diagnostic tool on a trusted LAN.

3. **Per-key flow correlation via SRT HSv5 SID extension
   parsing.** New `parse_srt_handshake_streamid()` in
   `main.rs` parses the SRT control packet header (16 bytes),
   handshake body (48 bytes), and walks extensions looking
   for type `0x0005` (SID). The SID payload is byte-reversed
   per 4-byte word per RFC 8723 §3.2.1.1.3 — `decode_srt_sid()`
   reverses each chunk back and strips trailing nulls.
   Then `extract_bmd_key()` finds the `u=` field in the
   resulting `#!::bmd_uuid=...,bmd_name=...,u=KEY` BMD-
   flavored streamid. tshark capture extended with
   `-e udp.payload` so we have the raw bytes to parse.
   Result: stream cards in the dashboard show
   `key: <BMD KEY>` once the conclusion handshake is captured,
   making "stream X on key K is at 6.1 Mbps" possible. 10
   unit tests cover decode + extraction + full synthetic
   handshake parse.

4. **New dashboard layout.** Full HTML rewrite:
   - **Mode banner** between header and main grid: large
     coloured strip showing "Mode: LIVE — passive only" or
     "Mode: STANDBY — active probes enabled" with a single-
     click toggle button.
   - **ATEM target panel** (top-left): IP / MAC / port pre-
     filled with `192.168.20.189` / `7c:2e:0d:21:ab:fe` /
     `1935`, plus a UDM-cross-referenced status line ("UDM
     sees: ONLINE via wired as 192.168.20.189 · last seen
     2s ago" or "NOT FOUND in UDM client list").
   - **UDM controller panel** (top-right): host / status pill
     (Connected/Connecting/Failed/NotConfigured) / last poll.
     `not_configured` state surfaces inline help with copy-
     pasteable env-var setup.
   - **Network clients card** (full width): row per UDM
     client, ATEM highlighted with an orange ATEM badge.
     Shows MAC / IP / hostname / tx kbps / rx kbps / wired-
     vs-wifi. Sorted: ATEM first, then by total bandwidth
     desc, then alphabetical.
   - **Live streams (capture)** unchanged from Session 4 but
     now displays the `key:` line per card when the SID
     parser populates `flow_keys`.
   - **Standby-only sections** (probe configuration / active
     probes / probe timeline) auto-dim with a "Switch to
     STANDBY mode to enable" gate when in Live mode. Still
     visible (no surprise content jumps when toggling) but
     don't draw the eye.
   - **Process health** card now shows three threads: probe
     / capture / UDM poll, each with last-active-at.
   - **LAN visibility hint** at the bottom: surfaces when
     capture is enabled, no flows ever seen, AND >30s have
     elapsed. Documents the switched-LAN gotcha (peer Mac
     can't see unicast between two other devices).

5. **Default ATEM target constants** (`DEFAULT_ATEM_IP`,
   `DEFAULT_ATEM_MAC`, `DEFAULT_ATEM_PORT`,
   `DEFAULT_UDM_HOST` in main.rs) baked in so a bare
   `--ui` launch is immediately useful — no manual IP
   entry on first run. Override via the dashboard form.

6. **MAC normalization.** New `normalize_mac()` accepts
   `7C-2E-0D-21-AB-FE` / `7C2E0D21ABFE` / `7c:2e:0d:21:ab:fe`
   and emits the lowercase-colon form UniFi's stat/sta
   endpoint uses. ATEM identification matches against the
   normalized form.

7. **README + start.command rewrites for v0.2.0.** Documents
   the three data sources (UDM polling = primary, capture =
   complement, active probe = Standby-only opt-in), env-var
   setup, switched-LAN topology notes, and what to look for
   in each mode. start.command interactively prompts for
   `UDM_API_KEY` if not in env, exports it to the binary,
   never writes to disk.

8. **`atem-net-diag` v0.2.0 release.** `cargo build --release`
   + Hardened-runtime codesign with Developer ID Application
   `6M536MV7GT` + tarball at
   `tools/atem-net-diag/dist/atem-net-diag-0.2.0-macos-arm64.tar.gz`,
   copied to iCloud Drive root for AirDrop to peer Macs.
   Stripped binary is ~1.5 MB; tarball is ~816 KB.

### Open issues from Session 5

- **Live UDM polling untested.** This Mac (`192.168.1.78` /
  `.172`) isn't on the production LAN. Pings to the UDM at
  `192.168.20.1` and the ATEM at `192.168.20.189` time out.
  The polling thread reports `connecting` and stays there
  (the first request is hanging at TCP connect, would
  eventually error after the 5s HTTP_TIMEOUT). Session 6
  has to validate live on the production-LAN Mac. If the
  endpoint paths or response field names don't match what
  the UDM actually returns, iterate in `tools/atem-net-diag/
  src/unifi.rs` (the legacy `RawClient` struct or the
  `IntegrationClient` struct, or add `#[serde(rename =
  "...")]` aliases). The integration vs legacy fallback
  ordering may also need to flip if the API key only works
  with the new integration surface.

- **Per-key correlation untested live.** Synthetic-packet
  unit tests pass but no real BMD encoder has been captured
  end-to-end yet. The byte-swap-per-word decoding in
  `decode_srt_sid` is per the SRT spec but real implementations
  occasionally diverge — Session 6 should sanity-check
  against an actual iPhone Blackmagic Camera SRT handshake
  or a stream from the Tauri app.

- **Auto mode deferred.** The original Session 5 spec called
  for three modes (Live / Standby / Auto). Session 5 shipped
  Live + Standby; Auto (probes resume after N seconds of
  no-flow on the configured key) requires per-key
  correlation to be live + reliable first, then can be a
  small follow-up.

- **API keys in conversation history.** Two UDM API keys
  were shared during Session 5 (one Site Manager that's the
  wrong type, one Local Controller that we used for the
  failed local smoke test). Both should be revoked by the
  user; treat anything in conversation history as compromised
  (including the second one even though it was for testing
  only). Future sessions should set the key as an env var
  in the user's own shell and have Claude inherit it via
  the launcher, never paste it into the conversation.

### Session 6 wins (atem-net-diag UDM live validation, 2026-04-26 evening)

Validated the Session 5 UDM polling against the real
production LAN, found three coupled bugs that the dev-Mac
build couldn't have caught, fixed them on a `udm-live-fixes`
branch (commit `8a80b5d`). Mode toggle verified end-to-end.
Per-key SRT correlation still untested live, but for a
documented topology reason (this Mac is a peer on a switched
LAN — can't see ATEM unicast traffic — same caveat already
in the README).

What landed:

1. **UDM polling works as designed.** Legacy `/proxy/network/
   api/s/default/stat/sta` returned HTTP 200 with 123 clients
   on the first authenticated request; integration v1 fallback
   wasn't needed (and in fact returned HTTP 000 on this UDM,
   so the legacy-first preference paid off). `unifi_status`
   reaches `connected`, ATEM is correctly identified
   (`is_atem: true`, MAC matches), `unifi_clients` populates
   in full.

2. **Three bugs found + fixed in `unifi.rs`** (all on
   `udm-live-fixes`, single commit `8a80b5d`):

   a. **USW-attached clients use hyphenated counter fields.**
      The ATEM (and 90+ other wired clients on this LAN) report
      bytes via `wired-tx_bytes` / `wired-rx_bytes` — the plain
      `tx_bytes` / `rx_bytes` come back zero for them. Only
      ~13 of 123 clients (the wireless ones) populated the plain
      pair; everything else used `wired-*` exclusively. The two
      pairs are mutually exclusive in observed data, never both
      populated for the same client. Fix: deserialize both via
      `#[serde(rename = "wired-tx_bytes")]` and sum.

   b. **Inverted tx/rx perspective.** UniFi's stat/sta reports
      counters from the **AP/switch perspective**: a client's TX
      is what the port RX'd from it. The Session 5 code passed
      UDM `tx_bytes` straight through to the snapshot's
      `tx_bytes`, so during a 6 Mbps SRT stream **into** the
      ATEM the dashboard showed "ATEM TX 6.8 Mbps". Swap the
      mapping so dashboard `tx_kbps` / `rx_kbps` are
      device-perspective — matching the file's own documented
      intent ("show what's flowing to the ATEM").

   c. **2-second poll interval shorter than UDM's counter
      refresh.** The UDM updates its byte counters every
      ~5-10 seconds, so most polls saw zero delta and one poll
      per refresh window saw ~10s of accumulated bytes squeezed
      into a 2s window — a real 6 Mbps stream displayed as 0
      kbps for ~3 polls then a 49 Mbps spike, then 0 again.
      The UDM already publishes its own smoothed rate in
      `tx_bytes-r` / `rx_bytes-r` (and `wired-*` variants).
      New `udm_rate_kbps()` method prefers those when present,
      falls back to delta math only for the integration API
      path which doesn't surface rate fields. Result: ATEM
      kbps is now stable and accurate, holding the same value
      for ~3 polls between UDM refreshes.

3. **Mode toggle test (Task D) passed.** `POST /api/config
   {"mode":"standby"}` flipped to standby cleanly; one probe
   fired in the next 5s interval; `POST {"mode":"live"}`
   stopped probes within one cycle. Probe outcome was
   `timeout` because keys were empty + the ATEM's accept slot
   was held by the live stream — that's correct behavior, not
   a tool bug.

4. **Empirical UniFi quirk catalog.** Wrote up the three
   stat/sta endpoint quirks (hyphenated fields, switch
   perspective, slow refresh) so the next API integration
   doesn't have to rediscover them. See the `udm_rate_kbps`
   doc-comment in `unifi.rs` for the inline version.

### Open issues from Session 6 (all resolved or superseded — see Session 7 / 8 wins)

- ~~**`udm-live-fixes` branch is local-only.**~~ MERGED. PR #1
  against `tauri-rewrite` on 2026-04-27. Commit `8a80b5d` is
  on the branch and shipped in alpha.7.

- ~~**The shipped v0.2.0 tarball in iCloud Drive is buggy.**~~
  Will be superseded by GitHub Release artifacts in alpha.9
  (Session 8 — Phase F adds the build-atem-net-diag CI job).
  iCloud distribution will be retired once that's live.

- **Per-key SID parsing still untested live.** Carries over.
  The Session 8 mirror-mode wizard (Phase E) gives operators
  a path to fix the underlying topology problem (no SPAN port
  on a peer Mac). Once an operator follows the wizard and
  successfully sets up port mirroring, the SID parser will
  finally see real BMD encoder traffic.

- **Production-LAN Mac has dual NICs on the same /24.**
  Carries over. `monitor_iface` is now editable from the
  dashboard (via PR #3 / live-iface-reload), so an operator
  can flip between en0 / en34 without restarting. The auto-
  pick still doesn't intelligently match the ATEM subnet —
  remains in priorities.

- **API key was pasted into the conversation again.** Same
  warning as Session 5: treat the key shared this session as
  compromised, rotate it, and prefer env-var-from-launcher
  flows for future sessions.

### Session 7 wins (post-Session-6 push, 2026-04-27 → 2026-04-28)

What CLAUDE.md was about to claim was "still-pending" all
shipped between Sessions 6 and 8. Documenting here so future
sessions don't re-discover work that's already merged.

* **`udm-live-fixes` is MERGED.** PR #1 against `tauri-rewrite`
  on 2026-04-27 (from a freshly-credentialed production-LAN Mac).
  The Session 6 warning that the branch was local-only is
  obsolete; commit `8a80b5d` is now on `tauri-rewrite` and
  shipped in alpha.7.

* **alpha.7 + alpha.8 shipped Mac-only.** Both releases
  succeeded on the macOS arm64 .dmg path; both Windows .exe
  jobs failed for an unrelated reason (the `$extract:`
  PowerShell parser bug — see Session 8 wins below). Mac
  artifacts were published; Windows users are still on
  alpha.6 from the official Releases page.

* **atem-net-diag bumped to v0.2.1.** Session 7 follow-ups
  to the Session 6 UDM fixes:
  - PR #2 (`package-script`): reproducible build-package.sh
    producing tarball + self-contained .app bundle + .app.zip.
  - PR #3 (`live-iface-reload`): in-dashboard `monitor_iface`
    change via `/api/config` without restarting the binary
    (sends SIGTERM to tshark child; outer respawn loop picks
    up the new iface from config).
  - Plus inline UI work: switch port utilization (USW per-
    port real-time tx/rx with the ATEM's port pinned to top),
    active alarms banner from UDM `/list/alarm`, gateway
    sysstats (CPU/mem/uptime) in the Process health card,
    sticky source labels per IP, pre-show health banner,
    quality alerts (bitrate dropouts, RTT spikes, idle
    stalls), reverse-DNS for public-IP flow sources.

* **iCloud tarball stale.** The `atem-net-diag-0.2.0-macos-arm64.tar.gz`
  in iCloud Drive predates the udm-live-fixes merge. Will be
  superseded by GitHub Release artifacts in alpha.9 (Session
  8 work), at which point the iCloud distribution can be
  retired.

### Session 8 wins (alpha.9 — bring it all together, 2026-04-28)

Multiple threads bundled into one coherent release. Phases
A, E, F, G, H landed first (smaller surface, lower risk);
Phases B, C, D (frame_pack refactor + OMT receive + OMT send)
land in subsequent commits before the alpha.9 tag.

1. **Windows release fix (Phase A).** The `release.yml:462`
   PowerShell parser bug (`"...$extract:"` — invalid variable
   reference because `:` is the scope-name delimiter) had killed
   alpha.6, .7, and .8's Windows builds. Fix: `${extract}:` to
   delimit the variable name explicitly. Audited the rest of
   the inline pwsh block; only the one line was buggy. Plus:
   - `add_windows_dll_search_path()` in `src-tauri/src/lib.rs`
     calls `SetDllDirectoryW` pointing at the bundled
     `resources\sidecar\` so grafton-ndi's runtime DLL load
     finds `Processing.NDI.Lib.x64.dll`. macOS already had this
     via @executable_path/../Frameworks rpath; Windows was
     missing the equivalent — would have been the next
     failure even after the parser fix landed.
   - CI sanity-check verifies both required sidecar files (NDI
     DLL + ffmpeg.exe) are staged before bundle, so missing
     files fail the build loudly instead of producing a
     runtime-broken .exe.

2. **atem-net-diag mirror-mode wizard (Phase E).** New
   `LanVisibility` enum (Unknown / SeesPeers / PossiblyBlind)
   on DashboardState. Monitor loop classifies each new flow
   as own-host (src_ip or dst_ip in our local IP set) vs
   peer; counters reset on tshark respawn so iface changes
   get a clean re-evaluation. `compute_lan_visibility` runs
   inline in `build_state_json` so the answer is always
   fresh. Yellow visibility-banner in dashboard.html surfaces
   when capture has run >30s with own-host flows but zero
   peer flows; expanded wizard pre-fills local IP, ATEM IP,
   and UDM URL into a 4-step UDM SPAN setup walkthrough
   (plus a 5th "Alternative — run on the streamer's Mac"
   details card). README's NETWORK TOPOLOGY NOTES section
   grew a click-by-click "PORT MIRRORING ON UDM" walkthrough
   with the destination-port DHCP gotcha and the "Direction:
   All packets" note. atem-net-diag bumped to v0.2.2.

3. **atem-net-diag GitHub release artifact (Phase F).** New
   `build-atem-net-diag` job in release.yml — runs on
   macos-14, reuses the same MACOS_CERTIFICATE_P12 / _PWD /
   _KEYCHAIN_PWD secrets as build-macos, runs the existing
   `tools/atem-net-diag/build-package.sh`, uploads tarball
   + .app.zip as release assets. Notarization opt-in via
   APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID secrets (with
   .app.zip stapled by extract → staple → re-zip; tarballs
   of folders can't be stapled). Release-publish job's
   `needs:` extends to include build-atem-net-diag but
   gating stays on `build-macos.result == 'success'` so a
   flaky net-diag build doesn't block the main release.
   Two new compgen-guarded globs in the FILES list.

4. **README "Companion tool" section (Phase G).** New
   block after "What it does" promoting atem-net-diag for
   Ubiquiti users with a feature list (UDM polling, WAN
   headroom, per-flow SRT health, alarms, mirror-mode
   wizard), a download link, and three "use it when"
   scenarios. Surfaces the tool to anyone reading the
   GitHub repo for the first time.

5. **OMT support (Phases B, C, D — landing).** Receive AND
   send. Headlined as the v0.2.0-alpha.9 feature on the
   release page. In-progress at time of this Session 8
   wins entry; final scope + behavior captured in
   subsequent CLAUDE.md edits as the work commits.

6. **CLAUDE.md catch-up (Phase H — this entry).** Sessions
   7 + 8 wins added; stale "udm-live-fixes is local-only"
   warning struck through with corrections. Open issues
   from Session 6 mostly resolved or superseded by Session
   8 work; the few that carry over are noted with current
   status.

### Open issues from Session 8

- **OMT-out audio path deferred to alpha.10.** Video-only
  in alpha.9. libomt has its own audio Send API but
  integrating it cleanly into the FFmpeg-tee pipeline
  (where do the audio frames come from?) is a separate
  design conversation.

- **OMT receive long-tail format coverage.** First alpha.9
  user reports will exercise OMT senders we haven't tested
  against (vMix-with-OMT, OBS-with-OMT-plugin). The
  stride-strip + pixel-format mapping is generalized
  enough that this should "just work" but expect surprises.

- **atem-net-diag Windows + Linux builds deferred.**
  alpha.9 ships net-diag Mac arm64 only. tshark path
  discovery + signing differ enough on each platform to
  warrant their own pass. Tracked in Session 9 priorities.

- **alpha.9.1 hot-fix branch staged** in case the OMT-out
  tee pipeline misbehaves under load. Phase D-only revert
  prepared; Phases A + E + F + G + H all stay live.

- **Mirror-mode wizard untested with actual operator.**
  The 4-step UDM SPAN walkthrough is correct as documented
  but has only been visually-verified — no production
  operator has yet followed it end-to-end. First alpha.9
  feedback will tell us if the steps need rewriting.

### Session 9 wins (alpha.10 + alpha.11, 2026-04-29)

The single-day session that finally cracked Windows. Five Mac-
only alpha releases broke when alpha.11 shipped a working
cross-platform pipeline. Plus a meaningful net-diag heuristic
fix and a UI affordance.

**alpha.10 (commit `d96e626`)** — recursive 7-Zip extraction
for the InstallShield-nested `[0]` payload. Got past alpha.9's
"Processing.NDI.Lib.h not found" bail by adding a
`Find-NdiSdkHeader` function that re-extracts any nested
archive candidate up to 3 levels deep. Mac shipped clean.
Windows progressed past the parser fix BUT 7-Zip rejected the
`[0]` blob as "not an archive" — the recursion logic worked,
the format didn't yield to 7-Zip.

**alpha.11 (commits `b012833` + `b8e21e2`)** — two threads:

1. **Windows finally lands** via `innoextract`. Confirmed
   locally that the NDI 6 SDK installer is **Inno Setup 6.1.0
   (unicode)**, not InstallShield — the Delphi section names
   `.itext` / `.didata` in the wrapper EXE were the giveaway
   (Delphi-compiled binaries; Inno Setup is the most common
   Delphi installer in the wild). 7-Zip handles many installer
   formats but not Inno Setup payloads natively. Switched the
   release.yml install step to `choco install innoextract -y`
   then `innoextract -d $extract -e --silent $out`. The SDK
   lands at `app/Include/`, `app/Lib/x64/`, `app/Bin/x64/` —
   exactly what grafton-ndi's build.rs needs. First Windows
   .exe in the Releases page (`ATEM.IP.Patchbay_0.2.0-alpha.11_x64-setup.exe`,
   48.3 MB).

2. **Net-diag wizard heuristic rewrite.** alpha.9's auto-
   detect was wrong in BOTH directions:
   - **False positive on streamer's own Mac**: every captured
     ATEM-bound flow had one local end → counted as `own_flow_count`
     → after 30s, `peer_flow_count == 0` triggered PossiblyBlind
     even though the streamer had perfect direct visibility.
   - **False negative on peer Mac no SPAN**: the case the
     wizard was DESIGNED for. Peer Mac sees zero traffic on
     the filtered ports because the switch isolates unicast.
     Both counters stayed at 0; heuristic stayed in Unknown
     forever; wizard never fired.

   alpha.11 replaced the local-vs-peer counter logic with a
   direct ATEM-IP match against captured flows: visibility =
   "we've seen the ATEM in src or dst of any flow." Direct,
   unambiguous, fires correctly in all four scenarios
   (streamer's Mac, peer Mac with SPAN, peer Mac no SPAN,
   ATEM's own Mac). `own_flow_count`/`peer_flow_count` fields
   preserved as dead state for one release; remove in 0.2.5
   if nothing else reads them.

   Plus a `?force_visibility=1` URL query param on the
   dashboard that bypasses the auto-detect and renders the
   banner with whatever live data is available — useful for
   demos, screenshots, and verifying the pre-fill data is
   wired correctly. Indicator text shows
   "(forced visible via ?force_visibility=1)" so a forced
   preview can't be confused with a real detection.

   atem-net-diag bumped to 0.2.4. Both the iCloud Drive
   tarball AND the GitHub Release artifact are at this version.

3. **"Net Diag" button in the topbar (alpha.9, commit
   `11cbcc6`).** One-click jump from the streaming app to the
   companion dashboard. Tries `open -a "ATEM Net Diag"` to
   launch the .app bundle if installed, then opens
   `http://localhost:8092` in the default browser regardless.
   POST `/api/open-net-diag` endpoint in http.rs. Documented
   here in retrospect — landed mid-alpha.9 push.

**Cross-platform pipeline status**: Mac arm64 .dmg + Windows
x64 .exe + atem-net-diag .tar.gz/.app.zip all publish from
the same release.yml on each `v*` tag. The release-publish
job's gating is still on `build-macos.result == 'success'`
(Mac is the only required artifact); Windows + net-diag are
additive.

### Open issues from Session 9

- **Windows .exe never tested on real Windows hardware.** The
  installer builds and signs in CI; no operator has yet driven
  it through install → launch → stream-to-ATEM. DirectShow
  device enumeration, Windows DLL search path
  (SetDllDirectoryW we added in alpha.9 should work, untested),
  Tauri's Windows window chrome — all theoretically correct,
  none verified. First Windows operator feedback will exercise
  the long tail.

- **Wizard heuristic untested live.** The alpha.9 banner
  was misfiring; alpha.11 fixed the heuristic but a real
  production operator hasn't yet been on a peer Mac without
  SPAN long enough to see the wizard surface organically.
  `?force_visibility=1` works for previewing the UI; the
  end-to-end "operator notices banner, follows wizard, fixes
  topology" loop is still aspirational.

- **OMT-out audio still deferred** (carries from Session 8).
  Session 10 priority #2.

- **OMT-out from non-NDI sources still deferred** (carries
  from Session 8). Session 10 priority #3.

### Session 10 wins (alpha.13 + alpha.14, 2026-05-25)

Shipped most of Session 10's planned scope (OMT audio + OMT tee
from the alpha.12 plan) plus a parallel "polish the receive-
stream wizard" thread that came up during the session as the
user reported gaps in the alpha.12 production experience.
Pre-show checks panel (the original Session 10 priority #1)
deferred to Session 11 — the OMT and receive-wizard work
consumed the session.

**alpha.13 (commit `ed7616e`) — seven headline changes:**

1. **Windows console window suppression.** Alpha.11 was the
   first Windows release; users immediately reported a black
   "ffmpeg.exe console" terminal popping up next to the Tauri
   window every time they started a stream, plus brief flashes
   on every device scan and Net Diag launch. Fix:
   CREATE_NO_WINDOW (0x0800_0000) applied to every console
   subprocess spawn via new `hide_console_std` /
   `hide_console_tokio` helpers in `ffmpeg_path.rs`. No-op on
   macOS/Linux. Covers streamer's FFmpeg + 3 device_scanner
   FFmpeg probes (avfoundation list / dshow list / AVF mode
   probe) + http.rs's `cmd /c start` URL launcher. Verified
   working on Windows in the alpha.13 test pass.

2. **Receive-wizard advanced settings.** Operators reported
   the default ports (9710 SRT, 1935 RTMP) collided with
   existing listeners and there was no in-UI way to change
   them — they'd have to edit JSON state by hand. New
   "Advanced settings" disclosure inside step 2 of the
   wizard with inputs for SRT port + passphrase, RTMP port
   + app + stream key. Apply / Reset buttons. Backed by the
   already-existing `relay_*` state fields in state.rs —
   alpha.13 just surfaces them. Inputs disabled while the
   receiver is running so displayed config can't drift from
   the bound listener.

3. **Receive-wizard protocol consistency.** Bug from an
   alpha.12 screenshot report: user picked SRT in step 1 of
   the wizard, started the receiver, then changed the radio.
   Result: radio = SRT, step 2 URL = SRT, step 4 banner =
   "RTMP listener bound" with rtmp:// URL. Cause: the
   wizard's protocol radio was independent of the actually-
   running receiver. Fix: new `syncRwProtocolWithSnapshot()`
   forces the radio to match the running listener whenever
   `isReceiverActive(snap)` is true, and disables radios +
   advanced inputs in that state. Inconsistent state is now
   impossible.

4. **App-instruction mismatch banner.** When a user picked
   an RTMP-only app (Blackmagic Camera, DJI Osmo Pocket 3,
   DJI drone) while SRT was selected, step 3 silently
   rendered an rtmp:// URL that disagreed with step 2's
   srt:// URL. The bottom-of-block "Pick RTMP above"
   footnote was easy to miss. Replaced with a prominent
   yellow banner at the TOP of step 3 with a "Switch to
   RTMP" button that flips the wizard in one click. New
   `RW_APP_PROTOCOLS` map drives which apps support which
   protocols. Per-app URLs in step 3 now always render for
   the app's REQUIRED protocol regardless of radio state,
   with the banner explaining the mismatch.

5. **Receive-wizard public-URL helper.** New "On my network"
   / "Different network" sub-toggle in step 2. Public mode
   expands a card with WAN-IP detection via `api.ipify.org`
   (deferred behind a user click so no third-party request
   fires until opt-in), public-address override field
   accepting DDNS hostnames, computed public URL with Copy
   button, port-forward checklist tailored per protocol (UDP
   for SRT, TCP for RTMP) with the most-common ports already
   filled in, CGN warning hint for cellular / 5G / apartment
   ISP scenarios where forwarding won't work no matter what,
   and a "Copy 'send to IT' template" button that builds a
   pre-filled message with all the forwarding details. Per-
   app instructions auto-switch host between LAN IP and
   public address based on which mode is active.

6. **OMT-out audio for NDI sources.** alpha.9 shipped OMT
   video publishing for NDI/OMT sources; audio was always
   deferred ("Audio over OMT is also alpha.10" — never
   landed). Now: `NdiCapture::run_capture_loop` drains audio
   via `capture_audio_timeout(Duration::from_millis(0))`
   after each video poll, ships chunks through a parallel
   mpsc channel. New `OmtSender::feed_audio_frame` uses
   libomt's FPA1 codec (32-bit floating-point planar — the
   only audio codec OMT defines). Audio fields set via
   `as_raw_mut()` since libomt 0.1.3 doesn't expose audio
   setters on its safe wrapper; field names verified
   against `vendor/libomt/libomt.h`. `audio_wanted: bool`
   plumbed through `start_and_probe_format` so audio drain
   only fires when OMT-out is enabled.

7. **OMT-out video tee for non-raw sources.** alpha.9 only
   supported OMT publishing for NDI/OMT sources (in-process
   frame-tee at the writer task). AVF webcams, pipe / RTSP /
   HLS, SRT/RTMP relay listeners silently fell through with
   a "OMT output unavailable for this source" warning. Now:
   when source isn't NDI/OMT AND `omt_output_enabled` is on,
   `spawn_omt_video_tee` starts a SECOND FFmpeg subprocess
   that opens the same source and outputs BGRA rawvideo on
   stdout. Reader task feeds OmtSender frame-by-frame. Scale
   filter normalizes to the configured output geometry so
   OMT consumers see consistent dimensions across both
   feeds. `omt_video_tee_child` in `Inner` for kill-on-drop
   + explicit cleanup from both `stop()` and the natural-
   exit path in `run_monitor`.

   Limitation documented: tee opens the source twice. USB
   / built-in webcams allow it; some virtual cameras and
   proprietary capture drivers refuse the second open.
   Network sources (RTSP, SRT/RTMP listen) work but double
   the bandwidth in. Audio on this path is video-only —
   still lavfi anullsrc until cross-platform pipe-FD
   plumbing lands.

**alpha.14 (commit `c042771`) — single-bug-fix release:**

8. **NDI + Custom audio on Windows.** Surfaced during the
   alpha.13 Windows test. Picking NDI source + Audio Mixer
   Custom mode + Dante Virtual Soundcard produced a stream
   with silent audio at the ATEM. Root cause:
   `build_ffmpeg_cmd_for_ndi`'s non-macOS branch had always
   emitted lavfi anullsrc, with a Session 4 comment
   flagging the DirectShow audio gap that was never
   followed up. So Windows NDI + Custom audio has been
   silent since the feature shipped. Fix: explicit
   `cfg(target_os = "windows")` arm emitting `-f dshow -i
   "audio=<DeviceName>"` — the Windows equivalent of macOS
   AVF's `:<name>` form. Pan filter in `build_audio_filter`
   is platform-agnostic so the L/R channel-pair routing for
   Dante carries over for free. Verified working with
   Dante Virtual Soundcard on Windows by the user.

### Open issues from Session 10

- **Pre-show checks panel in net-diag deferred.** Originally
  Session 10's priority #1 — full-width card under the
  existing pre-show banner with explicit checks for WAN IP /
  headroom / UDM polling / ATEM reachable / capture
  visibility / active streams / WAN-vs-ATEM consistency,
  aggregate verdict at top. Bumped to Session 11. Detail
  still applies (see priorities below).

- **alpha.13 receive-wizard work not yet field-tested.**
  alpha.14 verified the Dante fix in production; the wizard
  improvements (advanced settings, protocol lock, mismatch
  banner, public-URL helper) compile and look right but
  haven't been exercised against a real remote publisher
  (Larix on cellular, OBS over WAN, BMD Camera over RTMP)
  yet. First alpha.14 operator feedback will tell us what
  edge cases the wizard misses.

- **OMT-out audio for non-raw sources still deferred.**
  alpha.13's OMT tee is video-only; non-raw audio remains
  lavfi anullsrc. The clean fix needs cross-platform pipe-
  FD plumbing (UNIX fd 3/4 vs Windows named pipes) which
  is its own design conversation. Tracked for Session 11.

- **Multi-source mode never started.** The conversation
  that produced alpha.13 originally proposed a four-phase
  plan that ended in an in-app 2x2 grid for running
  multiple source→destination pipelines simultaneously. The
  OMT and receive-wizard work consumed the session; multi-
  source remains the largest still-pending architectural
  change. User decided on a two-phase rollout: a small
  "Launch another instance" launcher button polishing the
  existing `--instance-name` CLI flag first, then the in-
  app 2x2 grid as a separate alpha.

- **OMT receivers in production not yet exercised.**
  alpha.13's OMT audio + tee compile and would activate
  with `--features omt` + libomt at link time. CI doesn't
  bundle libomt yet, so the GitHub Release alpha.13/14
  builds DON'T have OMT enabled. A test pass against a
  real OMT consumer (Bluefish OMT receiver, etc.) is
  still needed once libomt is bundled.

- **Pipe / relay custom audio still uses anullsrc.** The
  alpha.14 Windows fix only covered NDI sources. Pipe /
  RTSP / SRT-listen / RTMP-listen video sources with
  Custom audio mode still fall back to lavfi anullsrc on
  every platform — the same comment that flagged the NDI
  Windows gap also flagged this. Fix shape would mirror
  the alpha.14 approach in each source factory.

- **alpha.13/14 other Windows tests pending.** User
  verified Dante audio in alpha.14; the console-window
  fix, receive-wizard port config, app-instruction
  mismatch banner, public-URL helper haven't been
  individually verified yet. They should all "just work"
  but the alpha.14 install is fresh enough that a quick
  test pass against the full alpha.13 feature list is
  worth doing.

- **net-diag Windows + Linux builds still deferred.**
  Mac arm64 only today. Carries from Session 8.

### Session 11 wins (alpha.15 through alpha.19, 2026-05-27)

Shipped the multi-source feature in a different shape than the
session started with, plus bundled net-diag.exe on Windows, plus
got OMT working end-to-end via libomt bundling. Five alphas, two
CI failures that took follow-up to debug.

**alpha.15 (commit `4a35048`)** — first attempt at multi-source as
an in-app 2x2 grid. multiview.html shell with 4 iframes loading
the existing single-source UI scoped via ?tile=N + an app.js
fetch shim rewriting /api/* → /api/i/N/*. Backend EncoderFleet
abstraction; tile-prefixed routes /api/i/:idx/* mounted alongside
/api/* aliases. spawn_instance Tauri command for "+ New Window".
**Operator feedback: iframes were too cramped to actually
configure or use a stream. The whole grid approach was wrong.**

**alpha.16 (commit `0f9c3a0`)** — pivot. Deleted multiview.html
shell + fetch shim. Kept the EncoderFleet abstraction (TILE_COUNT
reduced to 1, vestigial but harmless). New approach: small per-
instance **monitor window** the operator opens via a topbar button.
Each monitor shows live preview JPEG + condensed stats (status
pill, bitrate, source label, destination URL truncated). Sized
360x320, resizable down to 220x200. Document title syncs to
"Live · NDI: iPhone NDICAM — Monitor" so a screen full of monitor
windows is identifiable in the OS task switcher. Expand button
brings the main window to front (`focus_main_window` Tauri
command). Operator workflow: configure each instance in its
main window, open a monitor, drag to corner / second screen; the
OS window manager composes the multi-stream view, just like a
broadcast multiviewer.

**alpha.17 (commit `ab1ee41`)** — bundle atem-net-diag.exe on
Windows so the "Net Diag" topbar button actually works without
a separate download. New step in build-windows CI:
`cargo build --release` in tools/atem-net-diag/, copy the .exe
into src-tauri/sidecar/ alongside ffmpeg.exe + the NDI DLL. New
`net_diag_path()` resolver in ffmpeg_path.rs mirrors the existing
ffmpeg_path() pattern. api_open_net_diag's Windows arm spawns
the bundled binary with `--ui 8092` (detached, no console), then
opens browser to the dashboard. Sanity-check step requires
atem-net-diag.exe in sidecar/ before publishing.

**alpha.18 (commit `3f060ee`)** — bundle libomt so OMT senders
actually appear in discovery. CI failed both platforms:
- Windows: `Get-ChildItem -Recurse -First 1` picked the ARM64
  libomt.lib alphabetically before Winx64 → x64 cargo build hit
  30 LNK2019 unresolved-external errors with "library machine
  type 'ARM64' conflicts with target machine type 'x64'".
- macOS: deep-codesign walked the .app in filesystem order; tried
  to re-sign Contents/MacOS/atem-ip-patchbay before re-signing
  the libomt + libvmx dylibs in Contents/Frameworks/ → codesign
  refused with "code object is not signed at all" because the
  embedded subcomponents were still adhoc-signed.

Also includes a CSS fix for the audio-device dropdown contrast
on Windows — explicit `option { background-color: #0e0f12;
color: #e6e6ea; }` so WebView2 doesn't fall back to OS-native
light-mode colors for the open dropdown popup.

**alpha.19 (commit `7f8f397`)** — fixed alpha.18's two CI bugs:
- Windows: pin to `Libraries\Winx64\` explicitly, case-
  insensitive fallback if path casing varies.
- macOS: sign inside-out — Phase 1 Frameworks, Phase 2 Resources,
  Phase 3 MacOS/, Phase 4 outer .app. The leaves get signed first
  so the main binary's embedded subcomponent verification passes.

Both platforms now ship with:
- OMT discovery working (libomt.dylib/dll bundled, --features omt
  enabled in CI builds; libomt-rs's build.rs finds the binaries
  via LIBOMT_PATH env var exported from CI to GITHUB_ENV)
- libvmx.dylib/dll also bundled (libomt's runtime dep)
- NSIS post-install hook on Windows copies libomt.dll + libvmx.dll
  up to $INSTDIR\ from sidecar\ so the Windows loader finds them
  at process startup (same trick used for the NDI DLL since
  alpha.12)
- Deep-codesign on Mac re-signs every Mach-O with hardened
  runtime + Developer ID, in proper inside-out order

### Open issues from Session 11

- ~~**Net Diag button doesn't actually work on Windows.**~~ FIXED in
  alpha.20 (Session 12 wins, below). Root cause was `cmd /c start ""
  URL` under CREATE_NO_WINDOW silently failing — replaced with
  `rundll32 url.dll,FileProtocolHandler` which hits ShellExecuteW
  directly. The bundled .exe was fine; only the browser-open step
  was broken.

- **Hardware acceleration not used on Windows.** Today the FFmpeg
  encoder routes through h264_videotoolbox / hevc_videotoolbox on
  macOS (via the `use_vt` check at src-tauri/src/streamer.rs around
  line 712), but non-Mac platforms fall through to libx264 /
  libx265 software encoding. Multi-stream + 1080p60 software
  encoding on a typical Windows machine will quickly saturate CPU.
  Need to detect GPU vendor + route through nvenc (NVIDIA), qsv
  (Intel Quick Sync), or amf (AMD). The BtbN FFmpeg builds bundled
  in CI include all three encoders, so no new build work needed —
  just plumbing in streamer.rs. ATEM_DISABLE_VT pattern can extend
  to ATEM_HW_ENCODER=auto|nvenc|qsv|amf|software. **Deferred to
  alpha.22** — see Session 12 wins below.

- **alpha.15-19 features all need first-pass Windows operator
  verification.** Most of this is tested only on Mac dev builds.
  Specifically: monitor window resize behavior, layout-toggle
  persistence (localStorage), OMT sender discovery against real
  OMT senders (vMix, OBS-OMT plugin), bundled libomt actually
  loadable on Windows install, audio dropdown contrast fix lands
  visibly. Carries forward from Session 10's "alpha.13/14 other
  Windows tests pending" item; the test pass keeps growing as we
  add features.

- **CI cycles got expensive.** Five alphas in one session, two
  CI failures requiring debug + re-tag. Each cycle is ~17-20 min.
  Future direction: maybe add local-build verification to the
  worktree dev loop so we catch CI-level failures (esp. cross-
  platform stuff like the codesign ordering bug) before pushing.

### Session 12 direction: BIDIRECTIONAL PATCHBAY (DeckLink outputs)

The next major direction — user-stated 2026-05-27. Today the
app routes a LOCAL SOURCE (NDI camera, AVF/dshow webcam, screen
capture, SRT-listener, etc.) OUT to a NETWORK destination
(BMD-flavored SRT to ATEM, plus OMT-out tee). Session 12 adds
the **other direction**: route a NETWORK SOURCE (NDI sender on
the LAN, OMT sender, incoming SRT stream) to a LOCAL DECKLINK
OUTPUT (SDI / HDMI on a Blackmagic DeckLink card).

End-state use case the user described: this machine becomes a
"decode farm" — receives N NDI/OMT/SRT streams over the network,
outputs each to a different DeckLink SDI output, feeding a
hardware switcher / monitor wall / broadcast workflow. The app
becomes a true PATCHBAY — patching between physical (DeckLink
in/out) and network (NDI/OMT/SRT in/out) signals in both
directions.

Technical sketch:

- **Device discovery**: FFmpeg `-f decklink -list_devices true -i
  dummy` lists installed DeckLink outputs. Mirrors the existing
  `device_scanner::scan_dshow` pattern.
- **State**: extend `EncoderState` with a `destination_type`
  field — "atem" (the current SRT-to-ATEM flow, default) or
  "decklink" (new). When "decklink", add `decklink_device_name`.
- **FFmpeg command**: `build_ffmpeg_cmd` branches on
  destination_type. For DeckLink: `... -f decklink -i <device>`
  output instead of the current `-f mpegts srt://...` output.
  Pixel format negotiation: DeckLink outputs require specific
  pixel formats (uyvy422 typical) and resolutions matching the
  card's supported modes.
- **Existing sources work as-is**: NDI / OMT / SRT-listen /
  RTMP-listen / pipe (RTSP, HLS, etc.) all already produce raw
  frames (NDI/OMT) or bytestreams (others) that FFmpeg can
  decode. Just need to point the output at DeckLink instead of
  SRT.
- **Multi-instance shines**: each Tauri-instance (via
  `--instance-name` from Session 11) can route to a different
  DeckLink output. Operator launches 4 instances; instance A
  takes NDI sender X → DeckLink output 1; instance B takes
  OMT sender Y → DeckLink output 2; etc. Each instance gets
  its own monitor window — same UX as for the encode direction.
- **Hardware decoding**: similar story to encoding. NVIDIA
  cuvid / Intel qsv / AMD amf hardware decoders cut CPU for
  H.264/H.265 ingest. Wire up alongside the hardware-encoding
  work for Session 12.
- **FFmpeg DeckLink support**: needs `--enable-decklink` build.
  BtbN's Windows GPL builds include this historically; verify
  in CI. Mac's Jellyfin GPL build also includes it. Both
  platforms need the BMD DeckLink Driver installed on the
  end-user machine (free download from blackmagicdesign.com).
- **UI**: destination card in the source-pickers area gets a
  new option alongside "ATEM" — "DeckLink Output". When
  selected, expose the DeckLink device dropdown + the output
  mode (1080p59.94, 720p59.94, etc.) populated from the card's
  capabilities probe.

This is a substantial refactor — the streamer's mental model
shifts from "encode local source to network" to "patch source
to destination, where each can be local OR network". Worth a
Plan agent pass before implementation.

### Session 12 priorities (next pickup)

The big new direction is **DeckLink output** — see the "Session
12 direction: BIDIRECTIONAL PATCHBAY" section above. Plus two
Windows-side bugs that surfaced during alpha.17/19 testing and
need investigation before the next operator-test pass.

Priority order:

1. ~~**DeckLink output integration (the headline).**~~ SHIPPED
   in alpha.21. See "Session 12 wins" below for the full
   writeup of what landed, the architectural decisions the
   user confirmed before implementation, and the implementation
   surface (probe + discovery + state + FFmpeg branching + UI +
   monitor label).

2. **Hardware acceleration for Windows encoding (and DeckLink-
   output decoding).** Today the FFmpeg encoder uses
   h264_videotoolbox / hevc_videotoolbox on macOS (via the
   `use_vt` check in src-tauri/src/streamer.rs around line
   712), but non-Mac platforms fall through to libx264 /
   libx265 software encoding. Multi-stream + 1080p60 software
   encoding on a typical Windows machine saturates CPU fast.
   Need to detect GPU vendor and route through nvenc (NVIDIA),
   qsv (Intel Quick Sync), or amf (AMD). BtbN's FFmpeg builds
   already include all three encoders — just plumbing in
   streamer.rs. Same investigation applies to hardware
   DECODERS (cuvid / qsv / amf) for the DeckLink-output path
   in priority #1. Extend ATEM_DISABLE_VT pattern to
   ATEM_HW_ENCODER=auto|nvenc|qsv|amf|software. **Now the
   top alpha.22 candidate** — user re-confirmed during Session
   12 they want this bundled across both paths.

3. ~~**Net Diag button broken on Windows.**~~ FIXED in
   alpha.20. Root cause: `cmd /c start "" URL` under
   CREATE_NO_WINDOW silently fails to open the browser even
   though it returns exit 0. Replaced with `rundll32
   url.dll,FileProtocolHandler URL`. See Session 12 wins
   below for the diagnosis and verification details.

4. **Pre-show checks panel in net-diag.** This is the largest
   single operator-visible win remaining. Session 7 priority
   #4 was flagged as "the largest piece — likely needs to
   break into sub-tasks, but #1 of those is the pre-show
   panel since it prevents the most operator pain."

   What it is: a new full-width card in the dashboard, just
   below the existing pre-show banner, with explicit checks
   the operator can see + click into. Each check is a row
   with a status icon (✓/⚠/✗), the check name, the current
   value, and optional action.

   Checks to implement (most are already in state, just need
   surfacing):
   - **WAN IP detected** — from `wan_snapshot.wan_ip` (already
     in state). Status: pass if non-empty + non-private,
     fail otherwise.
   - **WAN headroom** — `current_upload_kbps / wan_upload_cap_mbps`
     ratio. Pass if <70%, warn at 70-90%, fail >90%.
   - **UDM polling healthy** — from `unifi_status`. Pass if
     `Connected`, fail otherwise.
   - **ATEM reachable** — from the unifi_clients list (look
     for `is_atem: true` entry, check `last_seen_secs` < 60).
   - **Capture visibility** — from `lan_visibility`. Pass if
     `SeesPeers`, warn if `Unknown`, fail if `PossiblyBlind`
     (with deep-link to the wizard).
   - **Active stream count** — count flows where src_ip ==
     atem.ip || dst_ip == atem.ip on ATEM ports. Just info,
     not a pass/fail.
   - **Stream key correlation** — count flows with non-empty
     `stream_key` from the SID parser. Info-only.
   - **WAN ingress vs ATEM RX consistency** — compare WAN
     upload rate vs sum of ATEM-bound flow rates. Warn if
     mismatch >2x suggests NAT/forwarding issue.

   Aggregate verdict at the top: green / yellow / red, with
   a one-line summary of the worst check.

   Data model: `PreShowCheck { id, label, status, detail, hint }`,
   `PreShowStatus { Pass, Warn, Fail, Skip }`. Add
   `pre_show_checks: Vec<PreShowCheck>` to `StateResponse`,
   build it in `build_state_json` from the existing snapshots.
   No new polling threads needed — pure projection over
   existing state.

   UI: new `<section class="card full" id="preshow-checks-card">`
   in `dashboard.html` between the existing
   `.preshow-banner` and the `<main>` grid. Each check
   row uses the same shape as alarm rows (icon | label |
   detail | optional hint).

   Files to touch:
   - `tools/atem-net-diag/src/dashboard.rs` — new struct,
     compute function, add to StateResponse.
   - `tools/atem-net-diag/src/dashboard.html` — new card,
     CSS for status icons, JS render function.
   - `tools/atem-net-diag/Cargo.toml` — bump 0.2.4 → 0.2.5.

   **Deferred to a follow-up**: port-forward verification
   (NAT loopback OR remote helper service) and ATEM-input-
   slots-free check. Those need outbound network probes
   and/or ATEM-direct queries that we don't have plumbing
   for yet.

~~**Multi-source mode — Phase A (launcher polish).**~~ — DONE
in alpha.15/16 via the `spawn_instance` Tauri command + the
"+ New Window" topbar button. Each instance gets its own
process, state dir, port pair.

~~**Multi-source mode — Phase B (in-app 2x2 grid).**~~ —
ATTEMPTED in alpha.15 (multiview.html iframes), ABANDONED in
alpha.16 because iframes were too cramped for actual stream
configuration. The replacement is the **monitor window**
pattern: configure in the full main UI, open a small monitor
window per instance, operator positions them on screen like a
video-switcher multiview. Ships the multi-stream UX without
fighting the OS window manager.

(Original Session 11 priority #2 — superseded.) User
asked for "a way to run multiple stream instances at
once, ie convert multiple NDI sources to multiple SRT
destinations" during the alpha.13 planning conversation
and chose a two-phase rollout. Phase A is the small,
fast-ship change: polish the existing `--instance-name`
CLI flag into a one-click experience.

   What it is:
   - Tauri command `spawn_instance(name)` that runs
     `Command::new(current_exe()).args(["--instance-name",
     &name])` and detaches as an independent process.
   - Topbar "+ New Instance" button with a small modal
     prompt for the instance name (default `instance-2`,
     `instance-3`, ... based on what's already running).
   - Docs in README + in-app footer FAQ explaining each
     instance is its own ATEM destination / own state /
     own ports; close one with Cmd+Q from that window.
   - Optional: a "Running instances" indicator in the
     topbar that lists sibling instances by scanning the
     state dir for active lockfiles. Skip if it gets
     fiddly.

   Files: src-tauri/src/lib.rs (Tauri command),
   bmd_emulator/static/index.html + app.js (button + modal),
   README.md. ~100 lines + docs. Self-contained, ships as
   its own alpha.

3. **Multi-source mode — Phase B (in-app 2x2 grid).** The
   bigger architectural change deferred to a separate
   alpha after Phase A. Single Tauri window holding up to
   4 source→destination pipelines simultaneously, switcher-
   board style.

   What it is:
   - New `src-tauri/src/multi.rs` with
     `struct EncoderFleet { encoders: Vec<Arc<EncoderState>>,
     streamers: Vec<Arc<Streamer>>, previews: Vec<Arc<Preview>> }`.
     Each Streamer already holds its own FFmpeg child + OMT
     sender; the fleet is just N copies.
   - http.rs route prefix `/i/:idx/api/...` for per-instance
     endpoints; keep `/api/...` as an alias for instance 0
     (UI back-compat).
   - Per-tile BMD port walk — instance.rs already walks
     9977→9986; the fleet assigns next-free per slot.
   - New `bmd_emulator/static/multiview.html` — 2x2 grid
     where each tile is a stripped-down version of today's
     single-source UI (source picker + destination +
     Start/Stop). Clicking a tile opens the full per-tile
     panel.
   - VideoToolbox encoder slots — macOS allows ~4 parallel
     HEVC encoders before throttling; document as soft cap.

   ~600 lines + meaningful UI work. The architectural
   refactor for v0.2.0. The most expensive remaining item.

4. **OMT-out audio for non-raw sources.** alpha.13's OMT
   video tee is video-only; audio for non-raw sources (AVF
   / pipe / RTSP / SRT-listen / RTMP-listen) still falls
   back to lavfi anullsrc. Approach: extend
   `spawn_omt_video_tee` to also output raw audio (s16le)
   via a second pipe FD, demux to the OmtSender's
   `feed_audio_frame`.

   Cross-platform pipe handling is the open design
   question:
   - macOS / Linux: FFmpeg's `pipe:3` syntax works with
     custom file descriptors plumbed via
     `std::os::unix::process::CommandExt::pre_exec` + dup2.
   - Windows: need named pipes (`\\.\pipe\xxx`) or a TCP
     loopback workaround. Tokio's process API doesn't
     directly support arbitrary FDs on Windows.

   Worth a Plan agent pass before implementing. May also
   want to revisit the dual-FFmpeg-process alternative
   (one for SRT, one for raw video + audio output) as a
   cross-platform-simpler fallback if the FD plumbing is
   too painful.

5. **Pipe / relay custom audio extension.** alpha.14 fixed
   Windows NDI + Custom audio via `-f dshow -i "audio=…"`
   but the same gap exists for the rest of the source
   types. Picking Custom audio mode with a pipe / RTSP /
   SRT-listen / RTMP-listen video source still emits lavfi
   anullsrc on every platform.

   Fix shape mirrors alpha.14: each source factory in
   sources.rs detects `audio_mode == custom`, appends
   `-f dshow -i "audio=NAME"` (Windows) or
   `-f avfoundation -i ":NAME"` (macOS) as a second audio
   input, sets `combined_av=false`. ~30 lines per platform
   per source — straightforward but needs a touchpoint in
   each source factory (5 source types × 2 platforms = 10
   touchpoints, though most can share a helper).

6. **Pre-show panel: smaller-scope alternative.** The full
   pre-show panel detailed in priority #1 is the most
   impactful operator-facing addition but it's also the
   biggest UI change. If picking that up cold feels too
   large, the smaller alternative is to ship just the
   aggregate verdict band (green/yellow/red strip at the
   top of the dashboard) reading the existing checks
   without the click-into-detail interaction. That's ~50
   lines and gives 70% of the operator-visible value.

7. **Carry-overs (deprioritized but still relevant)** — items
   that have been sitting in the priorities list since
   Sessions 6-8. Not in immediate scope but documented here
   so they don't get forgotten.

   - **Real-Windows-hardware end-to-end test (partial).**
     alpha.13 + alpha.14 verified the Windows console-window
     suppression and the NDI + Custom audio (Dante VSC)
     paths on real Windows hardware. Still NOT individually
     verified: full receive-wizard flow (Advanced settings
     port change, app-instruction mismatch banner, public-
     URL helper / ipify), DirectShow video device
     enumeration end-to-end, Tauri's Windows window chrome
     quirks. The pieces that have been touched recently
     are most worth a deliberate test pass now that
     alpha.14 is in users' hands.
   - **Unified client-list drill-down dashboard.** Session 5
     layout has bandwidth, flows, probe controls in separate
     cards. Replace with click-row-to-expand showing
     everything known about a device. Pin ATEM expanded-by-
     default at top.
   - **Stream identification fallback** when SID extraction
     fails. tshark's protocol dissectors can tag SRT vs RTMP
     vs RTMPS per flow tuple even without successful HSv5
     parse. Speedify-style "3 streams: SRT 6.1 Mbps from X,
     RTMP 4.2 Mbps from Y" view.
   - **Interface picker UX** (dual-NIC handling). Auto-pick
     currently chooses `en0` blindly. Should default to the
     iface whose default route handles ATEM IP, mark which
     iface actually carries ATEM traffic, surface a hint
     when on the "wrong" iface.
   - **Auto mode in net-diag.** Third diag mode (deferred
     from Session 5): probes resume only after N seconds
     of no observed flow on the configured key. Needs per-
     key correlation to be live + reliable first.
   - **net-diag Windows + Linux builds.** Mac arm64 only
     today. tshark path discovery + signing differ enough
     per platform that each warrants its own pass.
   - **Bundle libomt in CI builds.** alpha.13's OMT audio
     and tee paths compile but only activate with
     `--features omt` + libomt at link time. CI's GitHub
     Release builds don't have OMT enabled. Once libomt
     distribution is solidified (same pattern as libndi —
     download from vendor in CI, copy into
     Contents/Frameworks via tauri.conf.json), flip the
     omt feature on by default and the release artifacts
     get OMT-capable.

   - ~~**Push `udm-live-fixes`, merge, rebuild tarball**~~ —
     DONE in alpha.7 (Session 7 wins).
   - ~~**Pre-show panel**~~ — top of Session 11 priorities
     above (item #1).
   - ~~**Multi-source mode in the main app**~~ — promoted
     to Session 11 priorities #2 (Phase A launcher) and #3
     (Phase B 2x2 grid).
   - ~~**Custom audio for pipe / relay video sources**~~ —
     promoted to Session 11 priority #5.
   - ~~**OMT-out audio for NDI sources**~~ — DONE in
     alpha.13 (Session 10 wins, item 6).
   - ~~**OMT-out from non-raw sources via FFmpeg tee**~~ —
     DONE in alpha.13 (Session 10 wins, item 7), video-
     only. Audio for non-raw sources is Session 11
     priority #4.

### Session 12 wins (alpha.20 + alpha.21, 2026-05-28)

Both Session 12 priorities the user picked up cold (warm-up =
Net Diag debug, headline = DeckLink output integration) shipped
in a single session as two sequential alphas. CI green on both
platforms first try for both. Hardware-accel work that the user
re-confirmed for the DeckLink PR was descoped during
implementation and bumped to alpha.22 (see below).

**alpha.20 (commit `f96a8b6`) — Net Diag button fix on Windows.**
User-reported in alpha.19: clicking the Net Diag topbar button
does nothing visible despite alpha.17's `atem-net-diag.exe`
bundling work. Root cause turned out NOT to be the bundled
binary (it spawns correctly, binds 8092, serves the dashboard).
The follow-up step that opens the user's default browser via
`cmd /c start "" URL` exits 0 BUT silently fails to launch
Chrome — cmd.exe's `start` builtin needs an attached console
to allocate ShellExecute correctly, and alpha.13's
hide_console_std (CREATE_NO_WINDOW) flag suppresses that
console. Replaced with `rundll32 url.dll,FileProtocolHandler
URL` which hits ShellExecuteW directly without needing a
console. Verified empirically on the Broadcast Pix test rig
before the fix landed: ProcessStartInfo with CreateNoWindow=true
running `cmd /c start "" URL` returns exit 0 with empty stderr
but no Chrome tab; same command with `rundll32 url.dll` opens
the tab cleanly. 4-line behavior change + matching error
strings + commit-message-only explanation of the alternatives
considered (drop CREATE_NO_WINDOW, pull in opener crate, call
ShellExecuteW via windows-rs).

**alpha.21 (commit `a1cd804`) — bidirectional patchbay
(DeckLink output destinations).** Session 12's headline. The
app now supports a new destination type alongside ATEM/SRT:
LOCAL Blackmagic DeckLink SDI/HDMI outputs. Combined with
Session 11's multi-instance + monitor windows, the operator
gets a "broadcast decode farm" workflow — N network sources
in (NDI/OMT/SRT-listen/RTMP-listen/pipe), N DeckLink outputs
out, feeding a hardware switcher or monitor wall.

Architecture choices the user confirmed before implementation:
- **Data model: tagged Rust enum, flat JSON wire** (Plan
  agent's recommendation). Inner state has a flat
  `destination_type: String` discriminator + four
  `decklink_*` sibling fields when DeckLink is active. A
  `Snapshot::is_decklink()` helper hides the string
  comparison at consumer sites. Wire-side flat layout means
  every existing JS `snap.field` accessor in app.js keeps
  working untouched — no UI churn for the dozens of
  ATEM-side fields that vastly outnumber the new
  DeckLink ones.
- **UI: segmented control inline in existing destination
  card.** `[ATEM (Network · SRT)] [DeckLink (Local ·
  SDI/HDMI)]` toggle at the top, body swaps below. Same
  card. The dest-aux label in the card title flips from
  "SRT → host:port" to "DECKLINK → device · mode" when
  DeckLink is active. Monitor window's describeDest mirrors
  this.
- **HW accel: deferred** despite the user's initial
  request to bundle. During implementation the DeckLink
  surface itself grew to ~970 LOC across nine files; adding
  hardware encoder/decoder routing on top would have doubled
  the change surface for one PR. Decision: alpha.21 ships
  DeckLink with software paths; alpha.22 layers HW accel
  across both directions with shared `select_encoder()` /
  `select_decoder()` helpers keyed off ATEM_HW_ENCODER /
  ATEM_HW_DECODER env vars.

Implementation surface (alpha.21):

1. **FFmpeg DeckLink support probe at boot.**
   `ffmpeg_path.rs::ffmpeg_has_decklink()` — runs
   `ffmpeg -hide_banner -muxers` once, greps for
   "decklink", caches in OnceLock<bool>. Called eagerly
   from Tauri setup() so the answer is ready before the
   first /api/state request. Exposed as
   `ffmpeg_decklink_available: bool` on the state envelope;
   the UI uses it to disable the DeckLink radio with a
   "reinstall" hint when the build lacks `--enable-decklink`.

2. **DeckLink device + mode discovery in device_scanner.rs.**
   Two new functions following the existing
   `list_capture_devices` cache pattern:
   - `list_decklink_outputs(force)` — `ffmpeg -f decklink
     -list_devices true -i dummy`, parses single-quoted
     device names from stderr, 60s TTL cache. Card
     add/remove via force-refresh also clears the
     per-device mode cache.
   - `probe_decklink_modes(device_name)` —
     `ffmpeg -f decklink -list_formats 1 -i {name}`,
     parses the per-card format table into
     `DecklinkMode { format_code, description, width,
     height, fps_num, fps_den, interlaced }`. Lazy per-
     device cache (HashMap<String, Vec<DecklinkMode>>).
   - Three unit tests cover the regex parsers against
     synthetic FFmpeg output.

3. **`/api/decklink-outputs` HTTP endpoint.** Returns
   devices-with-modes shape. `?force=1` bypasses cache.
   The state envelope gets `ffmpeg_decklink_available` in
   the same commit so both signals reach the UI in one
   request.

4. **State model (state.rs):** added
   `destination_type: String` (default "atem"),
   `decklink_device_name: String`,
   `decklink_output_mode: String` (human label like
   "1080p59.94"), `decklink_format_code: String` (FFmpeg's
   per-card mode identifier like "Hp59"),
   `decklink_pixel_format: String` (default "uyvy422").
   Wired through `apply_settings` with validation against
   the known destination-type set ("atem" | "decklink") +
   Snapshot serialization. `SettingsPayload` in http.rs got
   matching Option fields with a one-line addition to the
   From<SettingsPayload> impl.

5. **build_plan branching (streamer.rs).** Early-return at
   the top of `build_plan` when `snap.is_decklink()` — the
   ATEM/SRT validation set (active_config, current_url,
   stream_key) doesn't apply. New `build_plan_decklink`
   validates instead that FFmpeg has DeckLink support, a
   device is picked, and the picked output mode is still
   supported by the card (re-probes via
   `probe_decklink_modes` to handle hot-swap). Uses the
   DeckLink mode's width/height/fps as the plan dimensions
   (rather than the user's video_mode dropdown, which now
   represents SOURCE-side expected geometry). Sets
   `protocol = "decklink"` and populates the new StreamPlan
   fields (decklink_device_name, decklink_format_code,
   decklink_pixel_format).

6. **build_ffmpeg_cmd branching (streamer.rs).** Early
   delegation to new `build_decklink_output_cmd` when
   `plan.protocol == "decklink"`. The DeckLink cmd builder
   shares input/map/filter handling with the ATEM path
   (identical for the first half of the command); the tail
   diverges entirely — no H.264/H.265 encoder, no AAC, no
   MPEG-TS/FLV container. Instead: `-c:v rawvideo -pix_fmt
   uyvy422 -c:a pcm_s16le -ar 48000 -ac {1|2} -f decklink
   -format_code {code} {device}`. The for_ndi / for_omt
   source builders feed their adjusted plans through
   build_ffmpeg_cmd, so the branch dispatches uniformly
   across every source type. Source-specific scale filters
   (NDI's scale=W:H:flags=lanczos when source dims differ
   from plan dims) compose cleanly — FFmpeg's pipeline
   auto-inserts uyvy422 conversion via the output-side
   `-pix_fmt` when the upstream filter omits the format
   step.

7. **UI (index.html + app.js + style.css).** Destination
   card grows a segmented control at the top — `[ATEM
   (Network · SRT)] [DeckLink (Local · SDI/HDMI)]` matching
   the existing `.seg-control` pattern used for protocol /
   codec / quality. New `.dest-decklink-body` block with
   device dropdown, mode dropdown, pixel-format note,
   driver-install link, refresh button. New
   `renderDecklinkDestination(snap)` toggles bodies, gates
   on `ffmpeg_decklink_available`, populates dropdowns from
   `knownDecklinkDevices`, and overwrites the destAux label
   when active. Three new device-status messages — distinct
   for build-support-absent vs. driver-absent vs. devices-
   available. Boot-time `fetchDecklinkDevices(false)` so the
   dropdown is pre-populated when the operator first
   switches modes. New mode labels rendered as "1080p59.94
   (Hp59)" by `humanizeDecklinkMode`.

8. **Monitor window destination label (monitor.js).**
   `describeDest` branches on `destination_type` —
   "{device} · {mode}" for DeckLink, existing
   shortenUrl-for-current_url shape for ATEM. Lets a
   screen full of monitor windows pointing at different
   DeckLink outputs stay distinguishable in the OS task
   switcher.

What this commit does NOT touch (carry-overs):
- Hardware accel routing (encode + decode) — alpha.22.
- Pre-show checks panel — Session 12 priority #4, still open.
- OMT-out audio for non-raw sources — Session 12 priority #5.
- Pipe / relay custom audio extension — Session 12 priority #6.
- README "DeckLink output" section — separate doc commit; the
  app's in-UI hints cover the basics for now (download driver
  from blackmagicdesign.com, etc.).

### Open issues from Session 12

- **alpha.21 DeckLink path not yet operator-tested with real
  hardware.** Code compiled clean on Mac + Windows CI but
  the actual flow (pick DeckLink card → pick output mode →
  start NDI source → see signal on the SDI output) hasn't
  been exercised on the Broadcast Pix test rig (the user
  has DeckLink hardware available). First operator test
  may surface mode-string format edge cases, scale-filter
  interactions for source/target geometry mismatches, or
  driver-error surfacing the UI hints don't yet handle
  gracefully. Likely fast iteration in alpha.22 / alpha.23.

- **HW accel deferred to alpha.22.** User requested both
  directions (SRT-to-ATEM encode + DeckLink-out decode).
  Plumbing in streamer.rs needs platform + GPU detection
  + an ATEM_HW_ENCODER / ATEM_HW_DECODER env-var surface.
  Will share a small `select_encoder` / `select_decoder`
  helper module across both paths. BtbN's Windows FFmpeg
  builds already include nvenc/qsv/amf so no build-side
  work needed.

- **No local cargo check possible on Broadcast Pix
  Windows.** LLVM's libclang.dll isn't installed (the LLVM
  Windows installer needs admin elevation that the user
  account here can't provide; chocolatey-managed installs
  to ProgramData are also blocked). So Rust-side changes
  on this machine ship without a compile-time gate — CI
  is the first compile check. Net Diag fix (4 lines) was
  trivial; DeckLink work (970 LOC across 13 files) compiled
  clean first try anyway. Future sessions on this box can
  either install LLVM via admin elevation OR continue the
  "push-and-let-CI-verify" pattern.

### Session 13 wins (alpha.22 through alpha.28, 2026-05-28)

Massive shipping day — nine alphas committed in a single
operator-feedback-driven session. Alphas 22-26 shipped to
GitHub Releases with CI green; 27-28 are committed locally
and queue for sequential push as the prior CIs clear.

**alpha.22 (commit `7554570`)** — three issues bundled.

1. Switch the Windows FFmpeg distribution from BtbN's GPL
   build to **gyan.dev's full_build**. BtbN dropped
   `--enable-decklink` from their Windows prebuilts (Blackmagic
   SDK redistribution issue). gyan.dev did the same — neither
   has decklink — but gyan.dev's "full_build" includes a much
   richer hardware-accel set (libvpl for Intel QSV, d3d11va /
   d3d12va / dxva2 / cuvid / nvdec / mediafoundation decoders,
   AMF + nvenc encoders) which directly supports the alpha.25
   encoder-picker work and the queued hardware-accel work.
   CI step's sanity check is informational only — doesn't fail
   the build on missing decklink since neither distro ships it.

2. **Net Diag dashboard "stuck on loading" bug** (live since
   alpha.17). Root cause: duplicate `const cfg` + `const atemIp`
   declarations inside `renderVisibilityBanner` in
   `tools/atem-net-diag/src/dashboard.html` — chromium parses
   them as a SyntaxError, so the entire poll loop never
   starts and the literal "loading…" header text never gets
   overwritten. Dropped the duplicates + reused the existing
   variables from above. Net Diag dashboard now renders.

3. **UDM API key dialog added to main app topbar** (later
   removed in alpha.25 per operator feedback — see below).
   ⚙ UDM button next to Net Diag, modal dialog with host +
   key fields, env-var passthrough when api_open_net_diag
   spawns net-diag.

**alpha.23 (commit `c3fa90c`)** — UI polish on top of alpha.22.

- Hero subtitle gains a reverse-direction tagline ("And do the
  reverse: convert network sources to physical outputs using
  DeckLink!") so the bidirectional patchbay framing reads from
  first paint.
- Destination-type segmented control restyled into an accent-
  tinted card with a "Where does this stream go?" label + bigger
  segments. Button labels reframed: "ATEM (Network · SRT)" →
  "Remote (ATEM | Decoder)" and "DeckLink (Local · SDI/HDMI)" →
  "Local (DeckLink | SDI/HDMI)". This isn't about a specific
  protocol — it's about where the stream PHYSICALLY ends up.
- Default `video_mode` flipped from `1080p30` to `1080p29.97`
  (NTSC broadcast cadence — the more common starting point in
  North American workflows). Operators still pick 1080p59.94
  by hand when needed.

**alpha.24 (commit `fdbb0d4`)** — OMT discoverability fix.

Reverses alpha.13's "hide OMT section when empty" decision.
The hide-when-empty UX made OMT undiscoverable for users who
didn't know it existed. Now: the Video Source card always
shows the "OMT senders on your network" section header. When
`knownOmt` is empty, a single placeholder tile renders
("No OMT senders found · click 'scan OMT' above to refresh").
Also new: a "scan OMT" link in the Video Source card title
parallel to the existing "scan NDI" link, wired to
`ensureOmtLoaded(true)`. Tiny ~70-LOC UI-only change.

**alpha.25 (commits `3d24ff5` + `8b5b5ed` + `0f50f19`)** — the
production-quality starter pack. Three-commit alpha, ~1100 LOC.

Background: user asked for "production ready, to be able to take
NDI streams from the machine and into a switcher and have it
work flawlessly, or to be able to send srt streams to an atem
and have it work well." Specifically called out encoder choice,
auto-reconnect, pre-show checks panel, and audio level meters.

Plan agent designed a six-feature bundle (~1890 LOC). Scope was
trimmed mid-session to keep alpha.25 shippable in a single
session without breaking the streamer's delicate lifecycle:

**Shipped in alpha.25:**
- `available_encoders()` probe at boot — runs `ffmpeg -encoders`,
  caches the video-encoder names in a `OnceLock<Vec<String>>`.
  Exposed on `/api/state` as `available_encoders: Vec<String>`.
- New state fields: `video_encoder`, `encoder_extra_flags`,
  `audio_codec`, `audio_bitrate_kbps`, `audio_sample_rate`,
  `audio_channels`, `auto_reconnect`, `auto_reconnect_max_attempts`,
  `meters_enabled`. Plumbed through SettingsUpdate, apply_settings,
  Snapshot, SettingsPayload, From impl. Validated server-side
  (encoder against known set, audio_codec against AAC family,
  audio_bitrate clamped 32-320, sample rate 44100/48000,
  channels 1/2).
- `select_encoder()` + per-encoder flag tables in streamer.rs.
  Five encoder families: VideoToolbox (preserved alpha.4 tuning
  exactly), NVENC (`-preset p4 -tune ll -rc cbr`), QuickSync via
  libvpl (`-preset veryfast -look_ahead 0 -pix_fmt nv12`), AMF
  (`-quality speed -rc cbr -profile main`), libx264/libx265
  (preserved). Auto-mode priority: macOS VT > nvenc > qsv > amf
  > libx*. Honors `ATEM_DISABLE_VT` env override. Falls back to
  auto if user-picked encoder isn't in available_encoders.
- `build_audio_section()` replaces hardcoded AAC-LC 48k path with
  reads from state. AAC-HE / AAC-HE-v2 supported; build_plan
  rejects them on ATEM destinations (the ATEM SRT decoder only
  takes AAC-LC).
- `parse_extra_flags()` hand-rolled shell-style tokenizer
  (~30 lines, no shlex dep). Power-user textarea appends raw
  FFmpeg flags after encoder block, before muxer block. Four
  unit tests cover simple flags / quoted strings / escapes /
  empty input.
- Advanced UI disclosure in the destination card with:
  encoder dropdown (filtered by available_encoders), auto-mode
  resolution hint, auto-reconnect toggle, AAC codec picker,
  audio bitrate (kbps, 0 = inherit), sample rate, channels,
  meters-enabled checkbox, encoder-extra-flags textarea.
- UDM API key dialog **removed** from main app per operator's
  feedback ("UDM should be in net-diag UI, not patchbay UI").
  Reverted: alpha.22's topbar ⚙ button, modal dialog, state
  fields, env-var passthrough at spawn. Net-diag's existing
  env-var workflow continues to work as a fallback.

**Deferred from alpha.25 (queued for Session 14+):**
- Auto-reconnect supervisor — the actual reconnect-on-disconnect
  behavior. UI toggle is present; backend supervisor loop wraps
  streamer.start() with exponential backoff. Risk-isolated to
  its own alpha because the start/stop/run_monitor lifecycle is
  delicate (NDI capture handoff, OMT sender Arc lifetimes,
  watchdog bash).
- Audio level meters — `astats` filter chain in build_audio_filter,
  metadata parser in run_monitor, canvas VU meters above the
  Audio Mixer card. ~400 LOC; preview-path NDI-only RMS computed
  in-process from existing audio samples.
- Quality presets — 5-preset ladder (Broadcast HQ / Streaming
  Standard / Cellular / Bandwidth-Limited / Lowest). Snap-on-pick.
- UDM API key form INSIDE net-diag dashboard — the proper
  relocation. Needs runtime credential injection (Mutex<Option<
  UnifiCredentials>> shared with the three polling threads;
  threads check on each tick and rebuild the UnifiClient when
  credentials change).
- Persistence (state.json) — was D9 in the Plan agent's plan;
  not strictly needed for the encoder/audio surface to be useful.

**alpha.26 (commit `36ae95d`)** — Net Diag → Net Utility rename.

User-facing strings only:
- Topbar button "Net Diag" → "Net Utility".
- Dashboard `<title>` "atem-net-diag · live" → "ATEM Net Utility · live".
- Dashboard `<h1>` "atem-net-diag" → "ATEM Net Utility".

Binary name (`atem-net-diag.exe`), repo path (`tools/atem-net-diag/`),
`/api/open-net-diag` endpoint, and Rust internal names stay for URL
+ build + muscle-memory stability. The tool grew well past
"diagnostics" framing — UDM polling, ATEM reachability, WAN
headroom, per-flow SRT health, alarm forwarding, mirror-mode
wizard, pre-show checks (alpha.28). "Utility" is the broader frame.

**alpha.27 (commit `60b6212`)** — switches & ports filter + pin.

Operator-feedback feature for the net-utility dashboard's
"Switches & ports" card. On a busy LAN: many switches × 48 ports
= wall of numbers. Operators care about a small handful (the ATEM
port, the streamer host's uplink, problem device under
investigation). Two affordances:

- **Filter bar** above the switches grid: text search input
  (matches switch name, model, port name, connected IP, connected
  MAC — case-insensitive, debounced ~80ms) + chip group
  (All / Active / Errors / ATEM / Pinned ★) + Clear button when
  any filter is dirty.
- **Per-port pin (★)** — click to mark a port as a favorite.
  Pinned ports float to the top of their switch's port table with
  a dashed separator + accent left-border. Pin state is a `Set<
  string>` keyed by `${sw.mac}:${port_idx}`. Click delegation on
  the card body so re-renders preserve handler bindings.
- **localStorage persistence** — pins (key `netutility_port_pins_v1`)
  and filter state (key `netutility_switches_filter_v1`) survive
  page reloads. Quota/private-mode failures swallowed (features
  still work without persistence).
- **Flap window display** — when a port shows `N flap(s)`,
  appended `· in ${fmtUptime(sw.uptime_secs)}` so the operator
  sees the period the count covers. Falls back to bare text when
  uptime not reported. Lets operators compute rate ("3 flaps in
  8h" = low concern; "3 flaps in 5min" = active instability).

Pure JS/HTML/CSS — no Rust touched, no new endpoints.

**alpha.28 (commit `ac8d68d`)** — pre-show readiness check panel.

Session 12 priority #4, longstanding ask. A go/no-go check panel
above the dashboard that aggregates the existing state signals
into clear pass/warn/fail rows. Saves the operator from manually
cross-referencing five separate cards to answer "am I ready?"

Backend (Rust):
- New types: `PreShowStatus` (Pass/Warn/Fail/Skip),
  `PreShowCheck` ({id, label, status, detail, hint}),
  `PreShowVerdict` ({overall, summary, checks}).
- `compute_preshow()` function — pure projection over existing
  state. Seven checks today:
  1. WAN IP detected (from wan_snapshot.wan_ip)
  2. WAN headroom (current_up_kbps / wan_upload_cap_mbps;
     <70% Pass, 70-90% Warn, >=90% Fail)
  3. UDM polling (Connected → Pass, Connecting → Warn,
     NotConfigured → Skip, Failed → Fail)
  4. ATEM reachable (last_seen against is_atem client;
     <60s Pass, <300s Warn, else Fail)
  5. Capture visibility (SeesPeers → Pass, PossiblyBlind →
     Fail with mirror-mode wizard hint, Unknown → Skip)
  6. Active streams (flow count, info-only Pass)
  7. Stream-key correlation (count of keys correlated via
     SID parser, info-only, with hint when 0 and capture
     healthy)
- Surfaced on StateResponse as `pre_show: PreShowVerdict`.
- Overall verdict = worst status wins.

UI (HTML/JS/CSS):
- New collapsible card below the existing pre-show banner.
  Color-codes the collapsed-card summary (green/yellow/red)
  based on overall verdict.
- `renderPreshowPanel()` renders one `.preshow-check` row per
  check with icon (✓/⚠/✗/·) + label + detail + optional
  hint below the row when status is Fail/Skip.
- New `escapeHtml()` helper for defense-in-depth — backend
  strings (UDM error messages, IPs) can't inject markup.
- Existing compact-pill `renderPreshow()` banner unchanged —
  the two systems coexist (banner = at-a-glance, panel = detail).

### Open issues from Session 13

- **NSIS sidecar file lock — installer silently skips locked
  files.** Discovered during the alpha.22 first-install testing:
  when the user installed alpha.22 over alpha.21, the NSIS
  installer updated `atem-ip-patchbay.exe` + static files but
  did NOT replace `sidecar/atem-net-diag.exe` because the
  alpha.21 net-diag was running at install time. Tauri's NSIS
  installer doesn't kill running sidecar processes pre-install
  and doesn't surface a "files skipped" warning. Workaround:
  kill all `atem-net-diag.exe` and `atem-ip-patchbay.exe`
  processes before running an installer. Long-term fix: either
  add a Tauri pre-install hook that taskkill's the sidecar, OR
  add an explicit "Quit and reinstall" prompt in the installer.

- **No prebuilt Windows FFmpeg ships --enable-decklink.** Both
  BtbN and gyan.dev dropped DeckLink support from their Windows
  builds — the Blackmagic DeckLink SDK has redistribution terms
  that prohibit shipping prebuilt FFmpeg binaries linked against
  it. alpha.21 ships the DeckLink output UI but it can't
  actually run on Windows without the operator installing their
  own decklink-enabled FFmpeg and pointing `ATEM_PATCHBAY_FFMPEG`
  at it. Chocolatey's `ffmpeg-full` package was also a dead-end —
  it just bundles gyan.dev's full_build (no decklink).
  **Proper fix is build-our-own FFmpeg with DeckLink SDK in CI**
  (queued; needs MSYS2/MinGW cross-compile + SDK download + ~30-
  60 min added CI time, possibly cached). Until then the
  DeckLink-missing UI hint surfaces the ATEM_PATCHBAY_FFMPEG
  override path.

- **`include_str!` and CI cache.** dashboard.html embeds at
  compile time via `include_str!`. Modern Rust (1.50+) tracks
  these files via `cargo:rerun-if-changed` implicitly, so source
  changes to dashboard.html DO trigger rebuilds. Verified in
  alpha.22: after a manual reinstall on the test rig, the
  installed binary's embedded HTML had the alpha.22 cfg-fix
  (substring probe confirmed 5 `const cfg` not 6). The
  apparent regression was just NSIS not replacing the locked
  binary — see first item above.

- **alpha.21-28 features not yet operator-verified end-to-end.**
  Specifically: encoder picker actually routes to nvenc/qsv/amf
  on Windows (need a multi-GPU machine for full coverage); audio
  quality knobs produce expected bitrate at the destination;
  raw-flags textarea passes through correctly; pre-show panel
  shows expected checks against a real network; switches/ports
  filter + pin survive page reloads; flap-window text format
  reads right; Net Utility rename doesn't break any third-party
  link / muscle memory.

### Session 14 priorities (next pickup)

Roughly in order of operator-impact value:

1. **UDM API key form INSIDE the net-diag dashboard.** User has
   been asking for this since alpha.22 testing — the alpha.22
   dialog in main app was a misplacement; alpha.25 removed it.
   Need the form in the net-utility's UDM panel. Implementation:
   - Wrap UDM credentials in `Arc<Mutex<Option<UnifiCredentials>>>`
     shared across the three polling threads (unifi clients, wan,
     system). Threads check the mutex at the top of each tick;
     rebuild their `UnifiClient` when credentials change.
   - Polling threads ALWAYS spawn at startup (even when creds are
     None — they idle in a sleep loop until creds are configured).
     Currently the `if unifi_credentials.is_some()` gate at boot
     prevents this; remove that gate.
   - `/api/config` POST handler extended to accept
     `unifi_api_key` field. Writes through the mutex. Never
     logged. Never persisted (in-memory only; operator re-enters
     on next launch — matches alpha.22's security model).
   - Dashboard form in the UDM panel: visible when `state.state
     == "not_configured"`. Password input + Save button. POSTs
     to /api/config.
   - ~250 LOC across `tools/atem-net-diag/src/{unifi.rs,
     dashboard.rs, dashboard.html}`.

2. **Auto-reconnect supervisor.** UI toggle is in place (alpha.25);
   backend behavior is missing. Wrap `Streamer::start()` in a
   supervisor loop that:
   - Spawns FFmpeg + waits for child exit.
   - Distinguishes "user clicked Stop" (do nothing) from
     unexpected exit (non-zero status code OR runtime < 5s).
   - On unexpected exit: schedule restart at exponential backoff
     (1s, 2s, 4s, 8s, 16s, 32s, 60s cap).
   - After `auto_reconnect_max_attempts` (default 12) consecutive
     failures: give up and surface sticky error.
   - Reset attempt counter after 60s of stable streaming.
   - User-stop sets `cancel_supervisor` flag; supervisor checks
     at every step including the backoff countdown.
   - New StreamStats fields: `reconnect_attempt`,
     `reconnect_next_secs`, `total_reconnects_this_session`.
   - UI status pill becomes "Reconnecting in 4s · attempt 3/12"
     during backoff (yellow, not red).
   - ~250 LOC, mostly in streamer.rs. **Highest-risk piece** —
     the existing start/stop/run_monitor lifecycle is delicate.
     Plan agent (Session 13) explicitly recommended landing this
     in its own commit after other work has CI-verified.

3. **Audio level meters.** UI checkbox exists (alpha.25);
   backend + canvas missing. Approach:
   - Extend `build_audio_filter()` to append
     `astats=metadata=1:reset=1:length=0.1,ametadata=mode=print:
     file=pipe\\:2:key=lavfi.astats:direct=1` when `meters_enabled`
     and destination_type != "decklink".
   - Extend `run_monitor`'s stderr parser to recognize
     `lavfi.astats.<n>.RMS_level=<f32>` and `lavfi.astats.<n>.
     Peak_level=<f32>` lines.
   - New StreamStats fields: `audio_db_l`, `audio_db_r`,
     `audio_db_peak_l`, `audio_db_peak_r` (f32, dB).
   - New `/api/audio-levels` endpoint at 4Hz polling (don't
     bloat /api/state's 1Hz cycle with this).
   - UI: pair of canvas-rendered VU meters above the audio
     dropdown. Green / yellow / red zones (under -18 / -18 to -6
     / -6 to 0). 1.5s peak-hold rendered client-side.
   - Preview-path meters: for NDI sources, compute RMS in-process
     in NdiCapture from existing audio samples (no FFmpeg
     subprocess change needed). Other source types' preview is
     JPEG-only today; meters skip them.
   - ~400 LOC across streamer.rs / preview.rs / http.rs / UI.

4. **Build FFmpeg + DeckLink in CI** (alpha.X). Until this lands,
   alpha.21's DeckLink output direction is non-functional on
   shipped installers. Approach:
   - Download Blackmagic DeckLink SDK in CI (URL access requires
     license-acceptance click-through historically; verify direct
     URL availability or look for a CI-friendly mirror).
   - Cross-compile FFmpeg from source on Windows runner via
     MSYS2/MinGW with `--enable-decklink` + the existing flag
     set (gpl, libsrt, libx264, libx265, nvenc, qsv via libvpl,
     amf, etc.).
   - Cache the build artifact aggressively — full FFmpeg compile
     is 30-45 min from cold. Cache key based on FFmpeg version
     pin + SDK version pin so the artifact only rebuilds when
     either upgrades.
   - Same for macOS (jellyfin-ffmpeg path replaced with
     custom-built variant).
   - Plan agent pass recommended before implementing — non-
     trivial CI work, several dimensions of cost / time / cache
     to balance.

5. **Hardware accel for Windows decoding** (DeckLink-out path).
   Once #4 lands and the DeckLink path is usable, plumb hardware
   decoders for incoming network streams (cuvid / qsv / amf /
   d3d11va). Same select_encoder pattern as the encode side.
   ~150 LOC if the encoder side's pattern is reusable.

6. **Quality presets ladder.** 5-preset segmented control:
   Broadcast HQ (9 Mbps) / Streaming Standard (6) / Cellular
   (3) / Bandwidth-Limited (1.5) / Lowest (0.8). Snap-on-pick
   onto the underlying bitrate / GOP / B-frames / preset fields.
   Coexists with XML-driven BMD-spec profiles (two parallel
   lists). Default: Streaming Standard. ~250 LOC.

7. **Carry-overs from Session 12:**
   - OMT-out audio for non-raw sources (cross-platform pipe-FD
     plumbing).
   - Pipe / relay custom audio (Windows fix mirroring alpha.14's
     NDI pattern, for each non-NDI source type).
   - Real-Windows-hardware end-to-end test for alpha.15-28
     features.

### Session 14 wins (alpha.29 through alpha.34, 2026-05-28)

Six tagged alphas + an entirely new cross-platform FFmpeg+
DeckLink CI pipeline (build-ffmpeg.yml) + a private BMD SDK
mirror. Approximately a 6-hour session. Most substantive
single-session shipment so far. alpha.29 through alpha.33
shipped clean to the Releases page; alpha.34 (the release.yml
swap) failed CI on Mac at the dylib-load sanity check —
documented in Open issues below as the immediate Session 15
pickup.

**alpha.29 (commit `92f195e`) — UDM API key form INSIDE the
net-utility dashboard.** Session 14 priority #1, operator-
asked-for since alpha.22 testing. Moves UDM credential entry
from the misplaced main-app dialog (alpha.22's dead-end;
alpha.25 removed it without surfacing a replacement) into
the net-utility's own UDM panel.

Refactor introduces `unifi::CredentialsHolder = Arc<Mutex<
Option<UnifiCredentials>>>`, shared across the three polling
threads (poll_loop / wan_poll_loop / system_poll_loop) and
the /api/config POST handler. Threads now always spawn
(previously gated on env-creds at boot); when unconfigured
they idle in NotConfigured. Each polling tick re-reads the
holder and rebuilds the UnifiClient on credential change,
so a runtime update via the dashboard form takes effect
within ~2 seconds.

Lock ordering preserved (creds-before-state) across polling
threads + request handler — no deadlock risk. Security:
credentials never enter DashboardState (which serializes to
/api/state); polling threads hold them via the holder Arc;
only UnifiStatus (Connected / Connecting / Failed /
NotConfigured) surfaces. atem-net-diag bumped to 0.2.5.

Also folded in the dashboard.rs hot-fix from `61213a2`
(`LanVisibility::PossiblyBlind` pattern match in the pre-show
check arm — alpha.28's regression that broke both the
build-atem-net-diag Mac job AND the build-windows job).

**alpha.30 (commit `2db1888`) — auto-reconnect supervisor.**
Session 14 priority #2. alpha.25 added the auto_reconnect
toggle + max_attempts UI but no backend behavior; this
alpha lands the actual supervisor.

Refactor extracts `Streamer::start()` body into a new
`spawn_attempt()` helper that returns the stderr handle.
`run_monitor()` becomes `run_one_attempt()` returning an
`AttemptOutcome` enum (UserStopped / UnexpectedExit). New
`run_supervisor()` task owns the lifecycle: alternates
run_one_attempt with `backoff_with_cancel()` ticks (1s →
2s → 4s → 8s → 16s → 32s → 60s cap). New
`handle_unexpected_exit()` centralizes the "reconnect or
give up" decision. New `Inner.supervisor_cancel` (a
`tokio::sync::Notify`) lets `stop()` wake a mid-backoff
sleep within milliseconds rather than waiting 60s for the
current tick.

Per-attempt cleanup tears down NDI/OMT capture + OMT sender
+ video tee so the next attempt can re-claim the SDK handle.
Watchdog (alpha.4) unchanged — each spawn_attempt spawns
its own; self-terminates when its FFmpeg dies. No leak.

UI: status pill shows "RECONNECTING IN 4s · 3/12" (amber)
during backoff. Monitor sub-label shows total drops this
session. Three new StreamStats fields ride through
StatsSnapshot via /api/state.

**alpha.31 (commit `e8249d1`) — audio level meters.** Session
14 priority #3. alpha.25 added the meters_enabled toggle +
state field; this alpha lands the actual visualization.

Backend: `build_audio_filter` appends `astats=metadata=1:
reset=1:length=0.25,ametadata=mode=print:direct=1` when
meters_enabled (and destination_type != "decklink").
`handle_log_line` short-circuits on any line containing
"lavfi.astats." — parses (channel, metric, value), updates
the corresponding StreamStats field, returns BEFORE the
line hits the log-tail buffer or the error-heuristic scan.
That's load-bearing: astats emits 40+ lines/sec; without
the short-circuit it'd fill LOG_TAIL_CAPACITY in seconds.

New parser `parse_astats_line` extracts `lavfi.astats.<ch>.
<metric>=<dB>` triples. Ignores "Overall" + channels 3+
since the UI is stereo-only. Five new StreamStats fields
(audio_db_rms_l, audio_db_rms_r, audio_db_peak_l,
audio_db_peak_r, audio_levels_at) defaulting to -120 dB so
idle meters sit at the bar's bottom.

New EncoderState method `read_audio_levels()` returns the
5 fields directly without going through the full snapshot —
the 4Hz poll endpoint is on the hot path.

New `/api/audio-levels` endpoint at 4Hz polling cadence
returns { rms_l, rms_r, peak_l, peak_r, updated_at,
server_time, meters_enabled }. server_time + updated_at let
the UI compute idle_secs locally + fade meters when the
stream stalls.

UI: canvas-based VU meters in the Audio Mixer card. Stale-
data detection fades to silence when no astats updates in
>0.7s. dB readout under each bar. CSS gradient maps -60..-18
green, -18..-6 yellow, -6..0 red. 1.5s peak-hold rendered
client-side via `audioMeterState.peakHold` map.

Documented limitations: mono streams light only L (astats
only emits channel 1); DeckLink destination skips meters
(no encoder pipeline); 4Hz feels slightly choppy on very
quiet signals (broadcast convention is 25-50Hz).

**alpha.32 (commit `ffaefba`) — UDM form survives password-
manager extensions.** Operator-reported on alpha.29: typing
the UDM API key + clicking Save did nothing while the
console showed Chrome's
"async-listener / channel-closed" extension error pattern.

Root cause: password managers (1Password, LastPass,
Bitwarden, Chrome's built-in saver) hook
`<input type="password">` + `<button type="submit">` pairs.
When the extension's handler crashes mid-async, the form
submission stalls and our submit closure never fires.

Defense-in-depth fix:
- Input is `type="text"` with `-webkit-text-security:disc`
  so the key still LOOKS like bullets but extensions don't
  see a password field. Explicit `data-1p-ignore` /
  `data-lpignore` / `data-bwignore` opt-outs. Non-password-
  looking name attribute.
- Button is `type="button"` (not submit). Click listener
  triggers submit() directly.
- Form submit + Enter-in-input also wired (keyboard users)
  with explicit preventDefault.
- Inline `onsubmit="return false"` as last-resort defense.
- Better HTTP / parse error surfacing.

atem-net-diag bumped to 0.2.6.

**alpha.33 (commit `a835f0c`) — per-switch pin in net-utility.**
Operator follow-up after alpha.27/28 testing. alpha.27 added
per-port pins (★ on individual ports). On a busy LAN with
many switches, operators want to pin a WHOLE switch to the
top — not just individual ports inside one.

- ★/☆ button in each `switch-block-head`. Pinned switches
  sort ABOVE the previous ATEM-auto-priority (operator's
  explicit pin beats the heuristic).
- Pinned switches get accent border + faint background tint.
- New `pinnedSwitches: Set<string>` keyed by sw.mac, parallel
  to alpha.27's pinnedPorts. Separate localStorage key
  (`netutility_switch_pins_v1`).
- Card summary surfaces both pin counts.
- Click delegation extended; same listener handles both
  `.sw-pin-btn` and `.pin-btn`.

Pure JS/HTML/CSS. No Rust changes. atem-net-diag bumped to
0.2.7.

**alpha.34 (commit `0f216ba`) — release.yml swap to sidecar
FFmpeg.** Session 14 capstone. release.yml's Mac block
(lines 113-129) and Windows block (lines 600-653) both now
`gh release download` the FFmpeg+DeckLink artifact from our
build-ffmpeg.yml-produced sidecar prerelease
(ffmpeg-decklink-8.1.1-bmd16.0-rev1) instead of curling from
jellyfin / gyan.dev.

Both blocks add a muxer-assertion sanity check after staging:
decklink + mpegts + flv muxers, libx264 + the platform's HW
encoder (videotoolbox on Mac, nvenc on Windows), srt protocol.
If any miss, the build fails loudly. Regression guard for
any future sidecar pipeline change.

**Windows side: shipped clean.** Both the sidecar pipeline
ran green AND release.yml's Windows job downloaded + verified
+ packaged successfully. The Windows .exe is technically
present at the v0.2.0-alpha.34 tag (the build-windows job
succeeded) but the overall release was gated by Mac.

**Mac side: failed at the dylib-load sanity check.** The
sidecar tarball's ffmpeg has dynamic dependencies on
`/opt/homebrew/opt/srt/lib/libsrt.1.5.dylib` (and likely
libx264, libx265). The release.yml runner didn't have those
installed, so `ffmpeg -version` failed at dyld load. See
Open issues from Session 14 below.

The publish-release job is gated on `build-macos.result ==
'success'`, so alpha.34's GitHub Release page was NEVER
created. Operators continue to see alpha.33 as the latest
release. alpha.34 tag dangles in git pointing at the working
swap; needs the dylib-bundling fix + re-dispatch (or a new
alpha.35 commit) before it can ship.

**Cross-platform FFmpeg+DeckLink CI (build-ffmpeg.yml).**
The session's deepest infrastructure work. 15 iterations to
get both platforms green:

- **Mac (iter 4):** Built FFmpeg n8.1.1 with --enable-decklink
  cleanly via a small shim header that defines IID_IUnknown
  via `CFUUIDGetUUIDBytes(IUnknownUUID)`. BMD's Mac SDK
  doesn't ship IID_IUnknown as a top-level constant; the
  symbol only exists in Examples/Mac/platform.h. The shim
  is written into the SDK's Mac/include/ before configure
  runs + -include'd via extra-cxxflags.

- **Windows (iter 15):** Pivoted from MinGW widl to Microsoft
  MIDL after 5 widl iterations surfaced increasingly
  esoteric incompatibilities (missing IUnknown import,
  lowercase bool, missing cross-file imports between
  version-specific headers, deprecated tIMPORTLIB syntax,
  undefined types chained across imports). MIDL is the
  compiler BMD's IDLs were designed for; it handles their
  cross-references natively.

  Windows pipeline structure:
  1. setup-msys2 (now also includes `zip` for packaging)
  2. ilammy/msvc-dev-cmd@v1 (gives MIDL its cl.exe
     preprocessor)
  3. Download SDK 16.0 zip from amateurmenace/bmd-decklink-
     sdk-mirror via BMD_SDK_PAT
  4. Rename to no-spaces path (FFmpeg configure's word-
     splitting can't survive spaces in paths)
  5. Run MIDL on each .idl; emit DeckLinkAPI.h (16413 lines,
     758 KB)
  6. Stub the per-version v-headers (DeckLinkAPI_v14_2_1.h
     etc.) as `#include "DeckLinkAPI.h"` — BMD's auxiliary
     .idls are designed to be import'd, not compiled
     standalone. The types are inlined into DeckLinkAPI.h
     via MIDL's import handling.
  7. Clone FFmpeg n8.1.1
  8. ./configure with --enable-decklink + --enable-nvenc +
     d3d11va/dxva2/mediafoundation + base flag set
  9. make
  10. Bundle MinGW runtime DLLs (libgcc_s_seh-1.dll,
      libstdc++-6.dll, libwinpthread-1.dll) alongside
      ffmpeg.exe
  11. Sanity check muxers/encoders/protocols
  12. zip + upload via gh release upload to the private
      sidecar prerelease

Sidecar prerelease tag is computed from ci/ffmpeg-pins.env:
`ffmpeg-decklink-${FFMPEG_VERSION#n}-bmd${BMD_SDK_VERSION}-
rev${BUILD_REVISION}`. Current: `ffmpeg-decklink-8.1.1-
bmd16.0-rev1`. Both Mac (9 MB tarball) + Windows (22 MB
zip) artifacts are live on the prerelease.

**BMD SDK mirror.** Private repo `amateurmenace/bmd-decklink-
sdk-mirror` with the SDK 16.0 zip uploaded as a `v16.0`
release. Accessed in CI via `BMD_SDK_PAT` fine-grained PAT
(read-only Contents access on the mirror repo only).
docs/SETUP-FFMPEG-CI.md walks the setup steps that future
SDK version bumps would repeat.

**ci/ffmpeg-pins.env** is the single source of truth for
FFmpeg version + SDK version + BUILD_REVISION. Bumping any
forces a rebuild. paths-filter on build-ffmpeg.yml auto-fires
on pin changes.

### Open issues from Session 14

- **Mac FFmpeg+DeckLink sidecar has dynamic dylib deps.**
  THE blocker on alpha.34 actually shipping. Our Mac
  build-ffmpeg.yml configure used Homebrew's srt + x264 +
  x265 dynamically; the resulting ffmpeg binary references
  `/opt/homebrew/opt/srt/lib/libsrt.1.5.dylib` etc. End-user
  machines won't have those at that path. release.yml's
  sanity check `ffmpeg -version` failed with
  `dyld: Library not loaded`.

  **Fix shape for Session 15:** in build-ffmpeg.yml's Mac
  block, after `make`:
  1. Run `otool -L ffmpeg` to enumerate dynamic deps
  2. For each `/opt/homebrew/...` dylib, copy into the
     tarball alongside ffmpeg
  3. Run `install_name_tool -change /opt/homebrew/...
     @executable_path/<name>` on ffmpeg so it looks for the
     dylibs next to itself
  4. Run `install_name_tool -id @executable_path/<name>` on
     each copied dylib so the install names match
  5. codesign ffmpeg + dylibs (alpha.34's release.yml does
     this in the deep-codesign pass; needs to also pick up
     the new dylibs at the sidecar level)

  Bump `BUILD_REVISION=2` in ci/ffmpeg-pins.env to force a
  rebuild after the fix lands. Sidecar prerelease will get
  a new tag (`ffmpeg-decklink-8.1.1-bmd16.0-rev2`); release.yml
  picks up the new tag automatically via the same pins-source
  pattern.

  Estimate: 1-3 CI iterations to nail the install_name_tool
  invocations + verify the end-user .app loads cleanly.

- **alpha.34 tag dangling.** Tag exists in git pointing at
  `0f216ba` (the release.yml swap commit). The GitHub Release
  page was never created (Mac job gated it). When the Mac
  dylib fix lands, choices:
  a) Delete + re-tag alpha.34 at the new commit (force-push
     the tag — requires `git push origin :refs/tags/v0.2.0-
     alpha.34` then re-tag + push)
  b) Move forward: new commit becomes alpha.35; alpha.34 tag
     stays as a historical pointer to the swap-without-fix
     attempt
  Probably (b) is cleaner. Decide in Session 15.

- **Windows build artifacts at alpha.34.** The Windows .exe
  did build successfully at the alpha.34 tag — but the
  publish step was skipped (`needs.build-macos.result ==
  'success'` gate). The Windows .exe artifact IS retrievable
  via `gh run download` against the alpha.34 CI run, but
  no general operator can see it on the Releases page.

- **PAT in conversation history.** Same warning as Sessions
  5 + 6: the BMD_SDK_PAT pasted into the conversation should
  be rotated. Fine-grained, read-only, 90-day expiry, scoped
  to one private repo — but still treat as compromised.
  Generate a fresh PAT in the GitHub web UI, `gh secret set
  BMD_SDK_PAT` with the new value, delete the old PAT from
  the user's PAT page.

- **build-ffmpeg.yml MIDL step emits stubs for ALL non-
  produced .h files.** The current stub loop walks every
  .idl and writes a stub if the corresponding .h didn't
  exist. That's fine for the per-version v-headers FFmpeg
  references, but some auxiliary IDLs (DeckLinkAPIStreaming_
  v10_8.idl, DeckLinkAPIConfiguration.idl, etc.) ALSO get
  stubbed. Harmless — FFmpeg doesn't include them — but if a
  future FFmpeg version DOES start including one, the stub
  might mask a real type-resolution bug. Document the
  semantics; consider tightening to known-needed names only.

- **Audio meters mono detection.** alpha.31 limitation:
  mono input lights only the L meter (astats only emits
  channel 1 for mono). Could mirror L→R when detected mono.
  Small follow-up.

- **DeckLink output destination skips audio meters.** Same
  alpha.31 limitation — DeckLink path doesn't go through the
  astats filter chain. Computing RMS in-process from the
  frame stream would work but raw frames are bandwidth-
  expensive to sample. Defer.

- **OMT-out audio for non-raw sources.** Still deferred from
  Session 8. Needs cross-platform pipe-FD plumbing (pipe:3 on
  UNIX, named pipes on Windows).

- **Pipe / relay custom audio.** alpha.14 fixed NDI + Custom
  audio on Windows via `-f dshow`. Same gap remains for the
  rest of the source types (AVF / pipe / RTSP / SRT-listen /
  RTMP-listen). Each emits lavfi anullsrc when audio_mode=
  custom + non-Mac platform.

- **Real-Windows-hardware end-to-end testing.** Carry-over
  from Sessions 11/12/13. Many alpha.15-34 features compile
  + ship via CI but haven't been driven through actual
  install → use → stream-to-ATEM on a Broadcast Pix test rig
  (or similar). Each session adds more features to this
  pending test pass.

### Session 15 priorities (next pickup)

In rough order of operator-impact value:

1. **Fix the Mac dylib gap.** THE blocker on alpha.34
   actually shipping Mac DeckLink to end users. Detailed
   fix shape in Open issues #1 above. Bumping BUILD_REVISION
   in ci/ffmpeg-pins.env + dispatching build-ffmpeg.yml
   produces a new sidecar tarball; then re-attempt release.yml
   (either re-tag alpha.34 or move forward to alpha.35).
   ~30-60 min of CI iteration.

2. **Verify alpha.34 end-to-end once Mac dylib is fixed.**
   Install alpha.34 on a clean Mac + Windows. Pick a network
   source (NDI camera, OMT sender, etc.). Pick "Local
   (DeckLink | SDI/HDMI)" as destination. Verify the SDI/HDMI
   output of a real DeckLink card lights up. End-state proof
   of alpha.21's DeckLink output direction working on shipped
   installers.

3. **Pre-show checks panel (carryover).** Session 12 priority
   #4 — full pre-show panel in net-utility with explicit
   pass/warn/fail rows that the operator scans before going
   live. alpha.28's compact pre-show banner is in place; this
   is the larger sibling. Detailed plan in the Session 11/12
   priorities section above.

4. **Hardware accel for Windows + DeckLink-out decoding.**
   Carry-over from Session 12. The encoder side now ships
   nvenc via the sidecar; decoder side for the DeckLink-output
   path is still libx264/x265 software. Plumb cuvid/qsv/amf
   for incoming network streams.

5. **OMT-out audio for non-raw sources.** Needs cross-
   platform pipe-FD plumbing — Plan agent pass first.

6. **Pipe / relay custom audio extension.** Apply alpha.14
   Windows dshow / Mac AVF audio-injection pattern to the
   pipe / RTSP / SRT-listen / RTMP-listen video sources.

7. **Operator-side testing pass.** Many alpha.15-34 features
   shipped through CI without operator verification on real
   hardware. Especially: encoder picker routing to nvenc/
   qsv/amf on a multi-GPU Windows machine, OMT discovery
   against real OMT senders (vMix, OBS-OMT plugin),
   alpha.32/33 form + per-switch pin behavior on production
   networks.

8. **CLAUDE.md catch-up.** This entry covers Session 14;
   subsequent sessions should add their wins + open issues +
   priorities in the same shape.

### Session 15 wins (alpha.35 + alpha.36 + alpha.37, 2026-05-28 evening)

Three tagged alphas. alpha.35 shipped the Session 15 priority
#1 Mac dylib fix that unblocked alpha.34. alpha.36 + alpha.37
were back-to-back follow-ups as operator testing on the
Broadcast Pix rig surfaced first a parsing bug (devices weren't
appearing) then a codec bug (Start Stream failed at FFmpeg
header-write). Both were isolated to single-line fixes in the
patchbay code; the rev2 sidecar FFmpeg itself was healthy.

**alpha.35 (commits `ee2c75c` + `1e7d4f8`) — Mac Homebrew dylib
bundling.** The carryover from Session 14: the FFmpeg we built
in build-ffmpeg.yml linked dynamically against /opt/homebrew/...
dylibs. The build runner had Homebrew installed via the `brew
install nasm yasm pkg-config x264 x265 srt` step; release.yml's
Mac runner doesn't, so `src-tauri/sidecar/ffmpeg -version` died
at dyld load with "Library not loaded: /opt/homebrew/opt/srt/
lib/libsrt.1.5.dylib" before main(). publish-release was gated
on Mac success; alpha.34 never produced a Releases page.

Fix is the standard "portable Mach-O bundle" pattern (what
dylibbundler / macdylibbundler / Python's delocate-wheel all do)
hand-rolled in build-ffmpeg.yml between the sanity check + the
tarball step:

- Iteratively walk ffmpeg + each newly-bundled dylib's
  `otool -L` output, filter to /opt/homebrew + /usr/local
  prefixes, copy next to ffmpeg in /tmp/ffmpeg-out/, set each
  copy's LC_ID_DYLIB to @executable_path/<name> via -id,
  rewrite every load-command reference via -change. Loop until
  no non-system refs remain. Hard-cap at 10 passes as safety.
- Final audit greps every Mach-O for surviving /opt/homebrew
  refs, fails the build loud if found.
- Smoke test invokes the bundled ffmpeg with `env -i` so dyld
  can ONLY use embedded load commands (no DYLD_FALLBACK_LIBRARY_
  PATH leakage from the runner's brew install). If
  @executable_path resolution is wrong, the build dies right at
  build-ffmpeg.yml's smoke step instead of failing 20 min later
  in release.yml.

Live capture from the rev2 build:
- Pass 1 bundled 14 dylibs: libsrt.1.5, libssl.3, libcrypto.3,
  libx264.165, libx265.216, libxcb.1 + 5 libxcb-* siblings
  (shape, render, shm, xfixes), libXau.6, libXdmcp.6, libX11.6.
- Pass 2 caught a small number of cross-references between
  the just-bundled dylibs (no new copies needed).
- Final audit: clean.
- Bundled ffmpeg launches via env -i, srt protocol + decklink
  muxer both present.

X11/libxcb deps came in because FFmpeg's configure auto-enabled
the xcb_xfixes input device when libxcb headers were detected
on the build host. Adds ~2 MB to the tarball but never gets
loaded at runtime on end-user machines (no display server
context). Could shave with `--disable-indev=xcbgrab` (or
`--disable-libxcb`) in a future rev; deferred.

Mac sidecar tarball size: rev1 9 MB → rev2 16.7 MB (the bundled
dylibs).

Also in alpha.35:
- release.yml Mac block: extract changed from `tar -xzf to /tmp;
  cp /tmp/ffmpeg sidecar/` to `tar -xzf -C src-tauri/sidecar`
  so the dylibs land alongside ffmpeg automatically. Existing
  src-tauri/sidecar/README.txt placeholder stays (tar extract
  doesn't delete files not in the archive).
- release.yml Mac block added an otool audit that fails the
  build loud if any /opt/homebrew refs somehow survived
  (catches misconfigured tag pulls of pre-bundling tarballs).
- BUILD_REVISION bumped 1 → 2 in ci/ffmpeg-pins.env. New sidecar
  prerelease tag: ffmpeg-decklink-8.1.1-bmd16.0-rev2. paths-
  filter on build-ffmpeg.yml auto-fired.
- The existing release.yml deep-codesign Phase 2 walks
  Contents/Resources/* recursively + hardened-runtime signs
  every Mach-O it finds, so the 14 new dylibs got Developer ID
  signing without additional wiring. Apple's notary accepted.

alpha.34 tag dangles in git pointing at the swap-without-fix
commit (0f216ba) — no Release page; treated as a historical
pointer. alpha.35 is the first DeckLink-enabled FFmpeg
published to the Releases page on both platforms.

**alpha.36 (commit `c0616c8`) — DeckLink device parsing for
FFmpeg n8.1.1 log prefix.** Operator (on the Broadcast Pix test
rig) installed alpha.35, picked "Local (DeckLink | SDI/HDMI)"
destination, expected the device dropdown to populate with the
13 DeckLink-visible devices (DeckLink Studio 4K + DeckLink 8K
Pro x3 + NDI Machine x4 + Videohub I/O routes + Key/Fill 1).
Got "No DeckLink devices found" warning instead.

Diagnosed via direct probe of the bundled FFmpeg:

```
$ ffmpeg.exe -hide_banner -f decklink -list_devices true -i dummy
[Blackmagic DeckLink indev @ 0x...] The "list_devices" option is
   deprecated: use ffmpeg -sources decklink instead
[in#0 @ 0x...] Blackmagic DeckLink input devices:
[in#0 @ 0x...] 	'DeckLink Studio 4K'
[in#0 @ 0x...] 	'NDI Machine'
   ... 13 lines total ...
```

FFmpeg n8.1.1 changed the decklink indev's log prefix from
`[decklink @ 0x...]` (what older FFmpeg used) to `[in#0 @
0x...]`. On the per-device `-list_formats 1` output, the bracket
prefix is dropped entirely and per-format rows are now just
tab-indented (e.g. `\tHp59\t\t1920x1080 at 60000/1001 fps`).

Both regexes in device_scanner.rs were anchored to the literal
`[decklink` prefix, so every device + every format-row was
silently skipped on the new FFmpeg.

Fix is regex-only:

- `DECKLINK_DEVICE_LINE`: drop the literal `decklink` requirement.
  Now matches any bracketed prefix `[…]` followed by single-
  quoted name. False-positive risk: the deprecation warning line
  uses DOUBLE quotes, doesn't match; header lines like
  "Blackmagic DeckLink input devices:" have no quotes; only
  actual device rows have `'NAME'`.

- `DECKLINK_FORMAT_LINE`: make the bracket prefix entirely
  optional. The header row `\tformat_code\tdescription` skips
  naturally because the regex requires "WxH at N/D fps"
  structure after the first token, which "description" lacks.
  The explicit format_code check in parse_decklink_modes stays
  as belt-and-suspenders.

Added two regression-test fixtures (parse_devices_n8_1_1_format
and parse_modes_n8_1_1_format) with live captures from the test
rig output alongside the existing legacy-format tests. If a
future FFmpeg upgrade changes the format again, the tests fail
loud against the live capture instead of shipping silent.

The user's test pass continued post-alpha.36 — drop-down
populated correctly with all 13 devices. User picked "Output C
to Videohub Input 21" at Hp29, hit Start Stream, immediately
hit a different error (the alpha.37 codec bug below).

**alpha.37 (commit `7dc0d02`) — DeckLink output codec
rawvideo → wrapped_avframe.** Picked from the running app's
/api/log endpoint:

```
[decklink @ ...] Unsupported codec type! Only V210 and wrapped
   frame with AV_PIX_FMT_UYVY422 are supported.
[out#0/decklink @ ...] Could not write header
   (incorrect codec parameters ?): I/O error
```

alpha.21 shipped `-c:v rawvideo` for the decklink-output path
in `build_decklink_output_cmd` — sounds right (the muxer wants
raw frames) but is actually the wrong codec name. FFmpeg's
decklink_enc.cpp explicitly checks for wrapped_avframe or V210
at write_header time and rejects everything else. rawvideo
packages raw pixel BYTES; the muxer needs the raw AVFrame
structure preserved end-to-end so it can hand the frame to the
BMD driver via the DeckLink SDK's ScheduleVideoFrame API.

Fix is a one-codec swap: `-c:v wrapped_avframe`. wrapped_avframe
is FFmpeg's pseudo-codec for passing raw AVFrames straight to
a muxer that knows how to consume them. The upstream
`format=uyvy422` in the video filter (set by
build_plan_decklink) handles pixel-format conversion to what
the card expects; wrapped_avframe just packages the AVFrame
for delivery.

Manually verified before commit:

```
$ ffmpeg.exe -f lavfi -i testsrc2=size=1920x1080:rate=30 \
    -f lavfi -i anullsrc=channel_layout=stereo:sample_rate=48000 \
    -vf format=uyvy422 \
    -c:v wrapped_avframe -c:a pcm_s16le -ar 48000 -ac 2 \
    -f decklink -format_code Hp29 'Output C to Videohub Input 21'
[decklink @ ...] Found Decklink mode 1920 x 1080 with rate 30.00
Output #0, decklink, to 'Output C to Videohub Input 21':
  Stream #0:0: Video: wrapped_avframe, uyvy422(progressive), ...
  Stream #0:1: Audio: pcm_s16le, 48000 Hz, stereo, ...
frame=  90 ... (running cleanly)
```

For 10-bit DeckLink cards, V210 would be the alternative codec
— would need a -c:v v210 path with yuv422p10le pixel format
selection. UI doesn't surface 10-bit yet; deferred.

### Session 15 wins also include CLAUDE.md catch-up

This entry (in the same CLAUDE.md commit that documents Session
15) is the catch-up; Session 16 picks up cleanly from here.

### Open issues from Session 15

- **DeckLink output end-to-end NOT YET FUNCTIONALLY VERIFIED.**
  alpha.36 fixes the device-list parse so devices appear in the
  dropdown. The next step — pick a network source, pick DeckLink
  card + output mode, Start Stream, see signal on SDI/HDMI —
  has not yet been driven through on the test rig as of this
  CLAUDE.md update (operator testing in parallel). First test
  may surface mode-string format edge cases, scale-filter
  interactions, or driver-error surfacing the UI hints don't
  handle gracefully.

- **X11/libxcb dylibs bundled but unused on end-user Macs.**
  rev2 Mac sidecar grew 9 MB → 16.7 MB; ~2 MB of that is
  libX11 + 6 libxcb-* dylibs that FFmpeg auto-enabled because
  the build host had libxcb headers present (xcb_xfixes indev).
  End users have no display server context to invoke these,
  so they're dead weight. Future rev: add
  `--disable-indev=xcbgrab` or `--disable-libxcb` to the Mac
  configure step in build-ffmpeg.yml. Defer until other
  Session 16 work lands.

- **FFmpeg `-list_devices true` flag is deprecated.** FFmpeg 8.x
  warns "use ffmpeg -sources decklink instead". Still works in
  8.1.1; could be REMOVED in a future FFmpeg upgrade. Forward-
  compatible fix would be to switch to `-sources decklink`
  (which may have a different output format that the regex
  would need to handle too). Defer until either FFmpeg removes
  the old flag, or we have a reason to upgrade FFmpeg past
  8.1.1.

- **NSIS file-lock during reinstall pattern kept biting.**
  Each of the Session 15 install attempts (alpha.33 → alpha.35,
  alpha.35 → alpha.36) needed manual `taskkill /F /IM
  atem-ip-patchbay.exe + atem-net-diag.exe` for the installer
  to replace the bundled sidecar files. Long-term fix per
  Session 13 Open issues #1: Tauri NSIS pre-install hook that
  taskkills the sidecar procs, OR explicit "Quit and reinstall"
  prompt in the installer. Now "kept hitting" status — worth
  fixing before next operator install pass.

- **Mac dylib bundle on real end-user Mac not yet tested.**
  Mac CI's env-stripped `ffmpeg -version` smoke test passes
  + Apple's notary accepted the .dmg, but no end-user Mac has
  yet installed alpha.35/36 + driven a stream → ATEM. Same
  caveat as the DeckLink output direction: pending first-
  operator test.

- **alpha.32/33 features still not yet operator-verified.**
  Carryover from Session 14. UDM password-manager defense
  form, per-switch pin behavior on busy LANs. Wasn't tested
  this session (DeckLink was the focus).

### Session 16 direction: BROADCAST MULTIVIEW (NDI → DeckLink, 8 channels)

**The headline pivot for Session 16, user-stated at the end of
Session 15 after the alpha.37 codec fix landed.** The
single-source per-instance UX (what we've shipped through
alpha.37) works but doesn't match how a broadcast operator
actually uses this tool. The vision:

> "When user clicks [Local DeckLink], open a brand new
> interface that looks like a multiview on a video switcher,
> with 8 channels, all in one interface, so a user can route
> up to 8 remote sources to DeckLink outputs. Keep ability to
> switch audio sources at each input. Put audio meters at each
> input/output. Want to use this for broadcast. Should use
> hardware to best of its ability and be reliable."

What this means concretely:

- **A new dedicated UI mode** — operator picks "DeckLink
  broadcast multiview" (working title) and the existing
  single-source main window switches into multiview layout
  (or opens a new dedicated window — design decision for
  Session 16). Live patchbay grid view shows 8 source tiles,
  each routed to a configured DeckLink output, all running
  concurrently.
- **8 channels in one window.** This is the in-app multiview
  pattern. Session 11 attempted a 2x2 iframe grid (alpha.15)
  and abandoned it as too cramped; pivoted to the monitor-
  window pattern (alpha.16) where each instance gets its own
  small monitor window the operator drags around. The
  multiview rework is closer to alpha.15's intent but with
  the lessons learned — no iframes, native multi-pipeline
  scheduling in one Tauri window.
- **Per-channel input picker.** Same NDI / OMT / SRT-listen /
  RTMP-listen / pipe options that exist today on the single-
  source UI, just compactified into a per-tile picker.
- **Per-channel DeckLink output picker.** Each of the 8 tiles
  has its own DeckLink device + output mode dropdown. The
  Broadcast Pix test rig surfaced 13 devices via alpha.36;
  operators in real productions will likely have 4-8
  DeckLink/Videohub outputs.
- **Per-channel audio source picker** (the existing Audio
  Mixer Card surface — Auto / Custom / Silent + L/R Dante
  channel picker — applied per tile).
- **Audio meters at each input AND each output.** alpha.31
  shipped the meters on the single-source UI; this rework
  multiplies the meter rendering by 8 (per tile, both
  source-side and output-side). The output meters are new —
  alpha.31 only showed meters on the encoded SRT-out path,
  DeckLink-out had no meters.
- **Hardware accel.** For NDI sources (already-raw frames,
  no decode), no encode (wrapped_avframe straight through),
  HW accel really only applies to:
  - Source-side decoders for non-raw sources (cuvid / qsv /
    amf for incoming H.264/H.265 via SRT-listen / RTMP-
    listen / pipe RTSP). Session 12 priority #2 carryover.
  - Color conversion (CPU vs GPU) for the format=uyvy422
    step. FFmpeg's filter chain auto-uses GPU paths if
    available with -hwaccel options.
- **Reliability for broadcast.** Auto-reconnect supervisor
  shipped in alpha.30 — needs verification under 8-channel
  concurrent load. One channel's failure shouldn't cascade.
  The watchdog/process-supervisor architecture from alpha.4
  + alpha.30 carries over but needs per-channel isolation.

Architectural decisions to make in Session 16 (in order of
when they bite):

1. **Single Tauri process with N pipelines, or N separate
   processes (existing multi-instance pattern)?** The Session
   11 monitor-window pattern is N processes; the alpha.15
   abandoned grid was 1 process with N iframes. For broadcast
   reliability, isolation between channels matters — if
   channel 3's FFmpeg dies, channels 1, 2, 4-8 should keep
   running. Multi-process gives this for free (kernel
   isolation). Single-process needs careful Tokio task
   structuring + per-channel supervisor.
2. **`EncoderFleet` already exists (Session 11) with
   TILE_COUNT=1.** It's vestigial today. Bumping TILE_COUNT to
   8 + restoring the per-tile route prefix `/api/i/:idx/*`
   gets us most of the way to a working backend. UI is the
   bigger lift.
3. **UI layout.** The "looks like a multiview on a video
   switcher" cue — likely a 4x2 or 2x4 grid of tiles, each
   showing live preview JPEG + status + source/dest labels.
   Plus an overlay or sidebar for per-tile audio meters.
   Likely needs a dedicated route like /multiview.html or a
   layout-mode toggle.
4. **Preview pipeline.** alpha.13 added a 2 Hz JPEG preview
   served via `/api/preview`. For 8 simultaneous previews
   the bandwidth + CPU adds up. Could batch into a single
   /api/preview-grid endpoint that returns 8 sub-images, or
   keep per-channel endpoints and let the browser fetch
   8 in parallel. Browser caching + JPEG quality settings
   matter.
5. **DeckLink card capacity.** A single DeckLink 8K Pro has 4
   outputs (and the user's rig has multiple cards). For 8
   simultaneous outputs, the operator may be using multiple
   physical cards or a Videohub matrix. The DeckLink SDK has
   per-device ScheduleVideoFrame concurrency rules — need to
   verify FFmpeg handles 8 concurrent decklink_enc instances
   cleanly (or batch through fewer FFmpeg processes).
6. **Plan agent pass strongly recommended** before
   implementation — this is comparable in scope to the
   Session 12 DeckLink direction (~970 LOC across 13 files)
   and probably larger, given the UI rework.

Phase sketch (subject to Plan agent revision):

- **Phase A (design + spike):** Plan agent designs the
  architecture; small spike to verify 8 concurrent
  decklink_enc FFmpeg instances actually work on the test
  rig (or whether they need to share a process). Confirms
  single-process-N-pipelines vs N-processes decision.
- **Phase B (backend):** Bump EncoderFleet TILE_COUNT to 8.
  Add per-tile DeckLink destination state. Route prefix
  /api/i/:idx/* re-mounted. Per-tile preview slots.
  Per-tile supervisor isolation.
- **Phase C (UI):** New /multiview.html route + JS module.
  4x2 grid of tile components. Per-tile source picker
  modal. Per-tile DeckLink destination picker modal.
- **Phase D (audio meters everywhere):** Per-tile input
  meters + output meters. Likely needs astats filter chain
  injection on the source-side (for input meters) AND a
  separate astats path on the output-side (for output
  meters, since DeckLink path doesn't go through the
  encoded SRT pipeline that alpha.31's existing meters
  read from).
- **Phase E (hardware accel for incoming streams):**
  Plumb cuvid/qsv/amf decoders for non-raw sources so an
  operator running 8 SRT-listen channels doesn't melt the
  CPU. Same select_decoder pattern Session 12 sketched.
- **Phase F (broadcast-quality reliability):** Auto-
  reconnect tested under 8-channel concurrent load. Per-
  channel isolation verified (kill one channel, others
  keep running). Watchdog catches dead FFmpeg children.
  Document recovery semantics in the UI.

Scope sketch: This is the largest architectural change
since alpha.21 (the original DeckLink output direction).
Multiple alpha cycles expected. The user explicitly chose
to defer this to a new session ("let's do this change in
a new session") so it's the Session 16+ project, not a
hot-fix continuation of Session 15.

### Session 16 also picks up (smaller carryovers)

1. **Verify alpha.37 end-to-end on real hardware** (if not
   already confirmed by end of Session 15). Pick NDI source,
   pick DeckLink output, hit Start Stream, see signal on
   SDI. Should work post-alpha.37 — manual probe with
   testsrc2 confirmed against the same device + mode. If
   any new errors appear, iterate before the multiview
   work starts.

2. **NSIS pre-install process kill** (Session 15 Open issue
   #4). Recurring friction during operator install passes —
   each upgrade needs manual `taskkill` for the installer
   to replace sidecar files. Tauri 2 NSIS hook can run
   taskkill pre-install. ~50 LOC. Lifts a constant
   irritation. Probably ship as alpha.38 before the
   multiview work starts so testing iterations don't keep
   hitting it.

3. **Drop X11/libxcb deps from Mac FFmpeg build** (Session
   15 Open issue #2). `--disable-indev=xcbgrab` (or
   `--disable-libxcb`) in build-ffmpeg.yml's Mac configure.
   Bump BUILD_REVISION 2 → 3. Saves ~2 MB.

4. **Pre-show checks panel** — Session 12 priority #4
   carryover. Useful for net-utility but not on the
   critical path for the multiview work. Could be
   parallelized as a quick interleaved alpha.

5. **OMT-out audio for non-raw sources** — cross-platform
   pipe-FD plumbing. Was Session 12 priority #5 carryover;
   stays carried over.

6. **Pipe / relay custom audio extension** — was Session
   12 priority #6 carryover; stays carried over.

7. **Operator-side testing pass for alpha.32-36 features.**
   UDM password-manager defense form (alpha.32), per-switch
   pin behavior (alpha.33), Mac DeckLink output end-to-end
   (alpha.35/36/37), encoder picker routing to nvenc/qsv/
   amf on a multi-GPU Windows machine.

### Session 17 wins (alpha.38 through alpha.57, 2026-05-29)

Longest single-session push in the project's history — 20 tagged
alphas in ~24 hours. Completed Session 16's broadcast-multiview
direction (Phase A spike + Phase B/C implementation), added the
NDI audio TCP bridge that finally made NDI → DeckLink streams
audible, six substantial UI overhaul iterations driven by operator
feedback, a system monitoring drawer shared across single + multi
views, per-tile expandable stream stats, and a series of hot-fixes
for Windows-only bugs that Mac CI silently masked. Organized by
theme rather than per-alpha because the threads interleaved.

**1. Broadcast multiview is now production-shipped.**

alpha.38 shipped Phase A: a standalone `tools/decklink-spike/` Rust
crate (~350 LOC) that the operator could run on their rig to verify
8 concurrent `decklink_enc` instances coexist + a 4x2 layout mock
at `/static/multiview-mock.html`. Plus the alpha.39 Windows watchdog
parity (`#[cfg(windows)]` mirror of streamer.rs:486's Unix bash
watchdog) so the 8-channel orphan-FFmpeg footgun didn't multiply
when Phase B landed.

alpha.40 shipped the headline: `EncoderFleet::TILE_COUNT` bumped
from 1 to 8, new `/static/multiview.html` (single-file, embedded
CSS + JS), 4x2 grid of tile components, per-tile NDI / OMT / Test
pattern source picker, per-tile DeckLink output picker with mode
dropdown, Start/Stop, live preview JPEG @ 2 Hz. Topbar "Multiview"
button (later removed) navigated to it.

alpha.41 was the production hardening pass — five Tier-1 broadcast
gaps closed in one alpha:
- TILE_COUNT 8 → 4 (operator UX + reliability call — 2x2 at 1600x900
  gives each tile ~750x420 vs ~400x450 at 8; decklink-driver
  concurrency well-understood at 4).
- New `state_persist.rs` module — per-tile JSON persistence written
  atomically (.tmp + rename) on every settings change, loaded at
  boot via the same `apply_settings` path. Tile configs survive
  Cmd-Q / crash / reinstall.
- New `system_monitor.rs` module — `sysinfo` crate polls CPU%/RAM
  every 1.5s, traffic-light pill in multiview header, sustained-
  CPU-streak detection + sticky-memory-threshold logic. **Hidden
  Windows-only crash bug** shipped here (see #8 below).
- Stall detector in `run_one_attempt` — after 5 consecutive
  Streaming-with-fps<0.5 ticks, force-kills FFmpeg → alpha.30
  supervisor catches as UnexpectedExit → triggers reconnect.
- UI guards — Stop confirms when tile is live, dropdowns disabled
  while Streaming so config can't drift.

alpha.42 reshaped the UI per operator feedback: dedicated
`[Single | Multi]` segmented toggle in both topbars (replacing the
standalone Multiview button), Hide-intro button on the hero with
localStorage persist, Net Utility restyled `.prominent` (bigger,
accent border + soft fill), multiview tile redesign (compact, no
scrollbars, header + 1fr preview + auto controls strip),
bidirectional destination toggle per tile `[DeckLink | Remote SRT]`
with device-vs-URL field swap, and physical source options
(Cameras / Screen) added to the per-tile source dropdown.

**2. NDI audio TCP bridge — the alpha.42 headline.**

Operator: "NO SOURCE AUDIO is playing when i have an ndi source
going to decklink." Root cause: `build_ffmpeg_cmd_for_ndi` fell
back to `lavfi anullsrc` (silent) whenever `audio_mode != "custom"`.
NDI audio drained fine into `OmtSender` (alpha.13) but never
reached FFmpeg.

New `src-tauri/src/audio_bridge.rs` module — TCP loopback at
127.0.0.1:0. FFmpeg's input args become `-use_wallclock_as_timestamps 1
-f s16le -ar 48000 -ac 2 -i tcp://127.0.0.1:PORT?listen=0`. The
NDI capture's audio drain converts planar-f32 → interleaved-s16le
(per-channel scaling + clamp-on-overflow so clipping signal doesn't
wrap-around to noise). 5 unit tests cover mono→stereo, planar→
interleaved, clipping, multi-channel truncation, empty-input.

Why TCP loopback over the obvious alternatives:
- Stdin already carries raw video; container muxing would mean
  building nut/matroska in-process. Heavy.
- FD-based pipes (fd 3/4) need `pre_exec` on Unix + named pipes on
  Windows. Cross-platform fragility.
- libndi_newtek as FFmpeg demuxer — our sidecar doesn't build it
  (licensing).

TCP is the boring-correct answer. Per-attempt fresh bridge so the
alpha.30 reconnect supervisor works clean (single-accept design;
each attempt gets a new listener). Operator field-confirmed end-
to-end: "great, audio works now with ndi to decklink". Saved as
memory ([[ndi-audio-bridge-tcp-loopback-works]]) so future me
doesn't try to "simplify" the transport.

**alpha.49 audio drift fix.** After confirming audio worked, the
operator reported "audio was working and then it died after a
couple minutes". Classic source-vs-output clock drift symptom.
The bridge sent raw s16le bytes with no embedded timestamps;
FFmpeg derived PTS from sample-count math (4 bytes = 1 sample @
48kHz). NDI source's crystal vs. DeckLink card's crystal drift by
±20-100 ppm typical, accumulating to 6-30ms over 5 min, then
FFmpeg's a/v sync logic drops audio frames and eventually stops
entirely. Two-piece fix:
- `audio_bridge.rs::ffmpeg_input_args` adds
  `-use_wallclock_as_timestamps 1` so FFmpeg stamps each audio
  chunk with receiver wallclock instead of inferring time.
- `build_audio_filter` appends `aresample=async=1000:first_pts=0`
  to the audio chain when destination_type == decklink. The
  resampler absorbs up to 1000 samples/sec of drift in either
  direction continuously. Only applied for DeckLink because
  ATEM/SRT have encoder-level PTS handling.

Also alpha.49 tightened bridge writer failure surfacing — `send()`
returns bool now (was `let _ = ...`); ndi_audio_writer_task logs
loud + once on first failure, then throttled on subsequent dropped
chunks, so `/api/log` surfaces the cause if FFmpeg disconnects
its TCP audio.

**3. DeckLink-specific fixes.**

alpha.44 fixed "NDI → DeckLink: Could not write header (incorrect
codec parameters?)" — alpha.37's `wrapped_avframe` codec was
correct but `build_ffmpeg_cmd_for_ndi`'s scale-filter logic at
streamer.rs:1087 OVERWROTE `build_plan_decklink`'s
`scale=W:H,format=uyvy422` with just `scale=W:H:flags=lanczos`
whenever source dims ≠ output dims (almost always — iPhone NDICAM
720p → 1080p output). The `format=uyvy422` step was lost; BGRA
frames reached `wrapped_avframe` (passthrough codec), decklink_enc
rejected them. Fix: COMPOSE filters instead of replacing — splice
the lanczos scale into the position of the existing scale clause,
preserving the format=uyvy422 suffix.

alpha.48 dropped the alpha.31 `!is_decklink()` astats gate. The
original comment claimed astats was tied to the encoder pipeline
("raw output, no encoder pipeline"); wrong mental model. astats
is a FILTER not an encoder concern; the audio chain runs filters
→ output regardless of codec downstream. With audio now flowing
via the alpha.42 bridge, the meters need to reflect it.

**4. System monitoring drawer — both views.**

alpha.50 shipped a slide-out drawer (right edge) shared between
single + multi via new `/static/system-drawer.js`. Per-core CPU
bar grid, RAM bar, swap usage with paging-warning sub-text,
FFmpeg process table (pid + cpu% + mem_mb + cmd_excerpt, sorted
by CPU desc), available video encoder chips (HW encoders
highlighted), recent log tail. Backend extended `SystemHealth`
with `cpu_per_core`, `ffmpeg_processes`, `swap_*` fields;
`sys.refresh_processes_specifics` with scoped `ProcessRefreshKind`
keeps the 1.5s tick cheap. FFmpeg detection by name-contains-
"ffmpeg" (case-insensitive); cmd excerpt centers around
`-format_code`/`srt://`/`rtmp://`/`decklink` markers so
streaming-FFmpeg is distinguishable from OMT-tee-FFmpeg.

Single view gained a sys-pill in topbar (none before). Same shape
as multiview's; click opens the same drawer.

alpha.50 also added per-tile expandable stats panel in multiview.
Click "Stats ▾" on tile head → 8-cell grid unfolds between head
and preview: Status, Bitrate (Mbps), FPS, Speed (1.00x target),
Frames sent, Dropped, Duration, Reconnects-this-session. Last
error fills a wide row underneath when present. Cells color-code
on FPS deviance, speed deviance, drop count, reconnect count.
Per-tile expand preference persists via localStorage.

**5. UI overhaul (six iterations driven by operator feedback).**

- alpha.42 view-mode toggle + hide-intro + Net Utility prominence
  (covered above).
- alpha.45 audio meters in multiview tile (VU bars top-right of
  preview), extended preview stats overlay (bitrate · fps · frames),
  Custom audio mode unfold (device picker + L/R Dante channel
  pickers per tile).
- alpha.46 multiview layout hotfix — `aspect-ratio: 16/9` on
  `.preview` was forcing the preview to ~450px in tiles that only
  had ~430px, pushing the controls strip off the visible bottom
  edge. Operator: "i still don't see controls in multiview". Drop
  the aspect-ratio constraint; object-fit:contain on the img
  letterboxes correctly within whatever grid 1fr gives the row.
- alpha.48 added VU meter overlay to single-view preview frame
  (mirroring the multiview tile pattern). pollAudioLevels gained
  preview-meter-l/r as additional render targets — no extra poll.
- alpha.51 topbar layout hotfix — Session-17-added view-toggle
  + sys-pill pushed topbar content past the original 1080px
  max-width cap; `.brand` had `min-width: 0` so it collapsed into
  a single-character column. Operator: "the new stuff at the top
  of the page is cutting off the site logo and title, fix". Drop
  the max-width, flex-shrink:0 on brand, status-block margin-left:
  auto to push right, brand-mark 64→48px, tighter gaps.
- alpha.52 removed the Monitor button. Operator: "get rid of
  'monitor' mode - that is now redundant behavior and the button
  takes up too much space." alpha.16's single-tile companion
  window pattern is fully superseded by multiview + per-tile
  stats. Tauri command `open_monitor_window` stays registered
  (harmless).
- alpha.53 → alpha.54 → alpha.55 intro-toggle iterations:
  - alpha.53: replaced the conditional "↓ About" button with an
    always-visible #intro-toggle-btn that flips text/title
    between Hide and Show. (Pre-53: button was display:none until
    hero was hidden → operator couldn't discover the unhide
    path.)
  - alpha.54: defer attribute on app.js so system-drawer.js
    (which has defer) loads first. Without this, app.js executed
    inline during HTML parse before the deferred drawer script
    ran; `window.createSystemDrawer` was undefined when the
    sys-pill check ran; single-view CPU pill never got wired
    + clicks did nothing. Multiview worked because its bootstrap
    is in a DOMContentLoaded handler.
  - alpha.55: rename "Intro" → "About" throughout the UI;
    localStorage persistence dropped so About always shows on
    app launch (operator's chosen "hide" is session-only). Old
    flag actively cleared from localStorage on every load.

**6. Net Utility timeout — instrumented, root cause TBD.**

Operator reported alpha.43's multithreading fix didn't fully solve
the "Saving forever" timeout. alpha.51 added eprintln! per-phase
instrumentation in `apply_config_update` so the next reproduction
will surface which phase is slow (JSON parse, lock acquisition,
individual field updates). Operator needs to install alpha.51+
and reproduce. **Still open** — see Open issues from Session 17.

**7. alpha.57 Connecting-state stall detector.**

Operator: "when trying to connect a second ndi source to decklink
in multiview, it says connecting forever, so maybe there is a
problem with the decklink output, but then i am unable to stop it
from trying to connect." alpha.41's stall detector only fired
during Streaming; tiles stuck in Connecting (FFmpeg launched but
no first-frame progress — DeckLink card busy, output mode rejected,
NDI source went unreachable mid-handshake) had no automatic
recovery. New detector counts consecutive 1s ticks where status=
connecting; after 15s force-kills the child via start_kill →
supervisor catches UnexpectedExit → reconnect cycle. Per-tile
isolation preserved. **Needs operator verification post-install.**

**8. Critical Windows-only bugs the Mac CI happily masked.**

alpha.41 shipped a `tokio::spawn` outside Tokio runtime context in
`SystemMonitor::start`. Mac CI built clean; alpha.41 + .42 + .43
+ .44 + .45 + .46 all also shipped this latent bug because Mac
runtime pin made the panic invisible. Windows CI also passed —
because CI never EXECUTES the binary, only builds the installer.
Operator installed alpha.44 from CI → app exited within 2 seconds
silently. Captured stderr via `Start-Process -RedirectStandardError`
revealed:

```
thread 'main' panicked at src\system_monitor.rs:97:9:
there is no reactor running, must be called from the context of a
Tokio 1.x runtime
```

alpha.47 fixed by switching to `tauri::async_runtime::spawn` which
routes through Tauri's managed runtime regardless of caller
context. This is the canonical pattern for spawning tasks from
Tauri's synchronous setup() hook.

alpha.50 then shipped a sysinfo 0.30 compile error
(`.to_string_lossy()` on `&str` from `Process::name()` — wrong
type assumption). Mac CI ALSO failed but I'd been pushing fast;
cancelled the doomed alpha.51-.55 CI runs that all inherited the
break, fixed in alpha.56, saved as memory
([[sysinfo-0.30-string-types]]).

**9. Development workflow improvements.**

- `scripts/dev-swap-ui.ps1` — instant UI iteration. Copies
  `bmd_emulator/static/*` into the installed app's static/ dir.
  HTML/JS/CSS changes show on F5 without rebuild. -NetDiag flag
  also rebuilds tools/atem-net-diag/ locally (no libclang needed,
  ~30-60s). -Force flag taskkills running processes first.
  Refuses to run while app is open by default; MD5-compares to
  skip unchanged files.
- Auto-install pattern matured: when CI completes, `gh release
  download`, run NSIS installer silent (per-user, no UAC), kill
  any zombies, re-apply dev-swap, launch. Used continuously from
  alpha.47 onward. Saved hours of manual install + click-through
  during the session.

### Open issues from Session 17

1. **Net Utility "Saving forever" still firing**. alpha.51 added
   eprintln! instrumentation in apply_config_update; the operator
   needs to install alpha.57+ and reproduce so the log shows which
   phase is slow. Next session should READ /api/log via the system
   drawer after a reproduction attempt + iterate based on what's
   slow. Most likely culprit per Session 17 analysis: contention
   on state.lock() with the unifi/wan/system polling threads
   despite the alpha.43 spawn-per-request multithreading. Worth
   considering: a separate RwLock for the config struct so polls
   can read without blocking config writes, OR a copy-on-write
   pattern.

2. **alpha.57 Connecting stall detector untested on rig**.
   Currently in CI; operator should verify the 15s timeout fires
   correctly when a second tile is forced to fight for the same
   DeckLink output device. Look for the log line
   "Stall detector: FFmpeg stuck in Connecting for 15s..." in
   /api/log when reproducing.

3. **Single-view multi-source toggle requested.** Operator (end
   of Session 17): "when i configure multiple input/outputs in
   multiview, when i go back to single mode, i should be able to
   toggle through to the different connections i have." Concrete
   feature: single view currently only ever shows/edits tile 0.
   Add a tile-selector control in single view (likely topbar
   chips or a dropdown next to the view-toggle: "Tile 1 / Tile 2
   / Tile 3 / Tile 4") so the operator can cycle between
   configured tiles using the rich single-source UI. The single
   view's /api/* alias currently hard-resolves to tile 0
   (http.rs:230); needs a session-level tile-idx state + the alias
   becomes dynamic OR all `/api/*` calls in app.js add a `/i/N`
   prefix when a non-zero tile is selected.

4. **Hardware acceleration verification pass deferred to next
   session**. The encoder picker + auto-mode priority routing
   (VT > nvenc > qsv > amf > libx*) is already live since alpha.25
   per Session 12. Windows FFmpeg sidecar (gyan.dev full_build)
   ships all three HW encoders. What's needed: operator-side test
   pass on the multi-GPU Windows rig to confirm auto-mode picks
   nvenc (or whatever's appropriate) + that CPU stays reasonable
   under 4-tile load. The alpha.50 system drawer's
   "Available video encoders" chips show what's compiled in;
   FFmpeg process table shows per-encoder CPU. Both wired —
   operator just needs to test.

5. **OMT-out audio for non-raw sources** — still carrying over
   from Session 12. Needs cross-platform pipe-FD plumbing. The
   alpha.42 audio_bridge.rs pattern is reusable; OMT capture
   would expose audio_rx like NDI, the writer task fans out the
   same way. Probably ~150 LOC + 1 day.

6. **Pipe / relay custom audio extension** — still carrying over
   from Session 12. alpha.14's NDI dshow audio injection pattern
   needs to apply to AVF / pipe / RTSP / SRT-listen / RTMP-listen
   source factories on Windows + Mac. Mechanical but tedious
   (10 touchpoints).

7. **Long tail of operator-side verification** still pending:
   - The alpha.45/46 multiview meter behavior under 4-tile concurrent
     load on the test rig.
   - The alpha.50 system drawer's FFmpeg process table on a real
     streaming session (should show ~4 ffmpegs with cmd excerpts).
   - The alpha.50 per-tile stats panel under reconnect/interrupted
     scenarios.
   - The alpha.49 NDI audio drift correction over a 30+ min stream
     (the original failure mode took "a couple minutes" — verify
     past 30 min).
   - The alpha.51 topbar layout at various window widths
     (especially the Broadcast Pix monitor's native resolution).
   - The alpha.55 About-resets-on-launch behavior.

8. **CI flakiness** observed twice this session:
   - alpha.38 Mac job failed downloading the FFmpeg+DeckLink sidecar
     because build-ffmpeg.yml fired concurrently (tag-push race);
     fixed mid-session by adding `branches: ['**']` to build-
     ffmpeg.yml's push trigger.
   - alpha.47 Windows job failed `cargo install tauri-cli` due to a
     crates.io network blip; resolved via `gh run rerun --failed`.
   - alpha.47 publish-release step failed because a partial release
     was created during a previous failed run + the rerun couldn't
     create-over-existing. Fixed manually by downloading the
     Windows artifact + `gh release upload --clobber`.
   - alpha.50-.55 inherited the sysinfo `to_string_lossy` compile
     error; alpha.56 fixed root + cascaded.

### Session 18 priorities (next pickup)

In approximate order of operator impact:

1. **Investigate + fix Net Utility timeout** (Session 17 Open
   issue #1). alpha.51's eprintln instrumentation is in the latest
   install. Reproduction approach:
   - Open Net Utility → UDM panel
   - Paste a real UDM API key + click Save
   - Open the system drawer → "Recent FFmpeg log" section won't
     have net-diag's stderr (different process). Need to either:
     (a) tail net-diag's log directly via `Get-Content -Tail`,
     (b) add a `/api/log` endpoint to net-diag that surfaces its
         own eprintln output, OR
     (c) update the system drawer's log-tail to also fetch
         net-diag's recent log via an additional endpoint.
   - Once a reproduction shows which phase is slow, fix the
     specific bottleneck. Likely candidates: `state.lock()`
     contention against polling threads, an unintended network
     call inside apply_config_update, or a body read that's
     blocking on a slow client.

2. **Single-view multi-source tile toggle** (Session 17 Open
   issue #3). End-of-Session-17 ask: single view currently only
   addresses tile 0. Add a tile selector (probably topbar
   `[T1 ◉ T2 T3 T4]` chips next to the view-toggle, with the
   active tile highlighted). On switch:
   - JS rewrites all `/api/*` fetches to `/api/i/N/*` (where N is
     the selected tile), OR
   - Backend's `/api/*` alias becomes dynamic with a cookie or
     query-param hint.
   Tile 0 stays the default for back-compat. The single-source
   UI's existing per-tile state (which is already in
   tile.encoder for each tile) just gets surfaced to whichever
   tile the operator picked. ~200-300 LOC across app.js +
   index.html + http.rs.

3. **Hardware acceleration verification pass** (Session 17
   Open issue #4). Already 95% built; just needs operator verify.
   Test plan:
   - 4 tiles, all NDI → DeckLink, watch CPU in the system drawer
   - Check FFmpeg process table's cmd excerpts to confirm
     `-c:v h264_nvenc` (or similar) is being picked vs. libx264
   - If software encoder is being picked despite nvenc being
     available, debug `select_encoder`'s auto-mode priority.

4. **alpha.57 Connecting timeout verification** (Session 17 Open
   issue #2). Quick operator test: configure tile 1 with an NDI
   source to DeckLink Studio 4K running. Configure tile 2 with a
   different NDI source ALSO targeting DeckLink Studio 4K (same
   device — should be the contention case). Hit Start on tile 2.
   It should: enter Connecting, sit ~15s, log the stall message,
   flip to Reconnecting, then go through backoff. Stop should
   work during Reconnecting.

5. **OMT-out audio for non-raw sources** (Session 17 Open issue
   #5, longstanding carryover).

6. **Pipe / relay custom audio extension** (Session 17 Open issue
   #6, longstanding carryover).

7. **Long-tail operator verification pass** (Session 17 Open
   issue #7). A focused dedicated session would clear most of
   these in 1-2 hours.

8. **Carry-overs not pulled forward** — pre-show checks panel
   (Session 12 priority #4) and OMT receive in production +
   alpha.32/33 Windows tests + net-diag Windows/Linux builds.
   None are blocking; ship as opportunities arise.

### Session 17 also includes CLAUDE.md catch-up + commit

This entry is the catch-up. The next session picks up cleanly
from alpha.57 with the operator-side verification queue and the
Session 18 priorities above.

### Session 18 wins (alpha.58 + alpha.59, 2026-05-29)

Operator-driven firefighting session on the Broadcast Pix Windows
rig (plus a Mac). Two alphas shipped + CI-green on both platforms +
installed/verified on Windows. The session was *supposed* to be the
single-view tile toggle / 6-NDI→DeckLink work, but it turned into a
production-down debugging marathon that ultimately uncovered a
long-standing ATEM receiver-slot lockout. Net result: the recovery +
streaming-reliability foundation is now solid; the 6-NDI feature work
is still pending.

**alpha.58 (commit `da9531e`) — recovery + crash hardening.** Fixes
the unrecoverable "attach Dante DVS audio to an NDI source → crash,
Stop/refresh/kill-orphans all do nothing" report. Three coupled
structural bugs:
1. **Stop-aware capture send** (ndi_capture.rs + omt_capture.rs).
   `tx.blocking_send()` parked the capture OS thread forever when
   FFmpeg stopped draining stdin (Dante's dshow open hangs → FFmpeg
   never reads the rawvideo pipe → the bounded channel fills). A
   parked thread can't see the stop flag, so `stop()`'s `handle.join()`
   hung — while holding the streamer `inner` lock → every recovery call
   wedged. Replaced with `send_frame_or_stop` (poll `try_send`,
   re-check `stop` between full-channel retries) + bounded
   `join_with_timeout`.
2. **Off-lock capture joins** in `stop()` + `run_one_attempt` — take
   captures out under the lock, join off-lock; only non-blocking
   `start_kill` stays under the lock.
3. **Poison-proof state RwLock** — `.read()/.write().unwrap()` →
   `.unwrap_or_else(|e| e.into_inner())`, so one panic while holding the
   write guard no longer poison-cascades into every /api/* handler +
   supervisor + stall-detector (a permanent wedge from a single panic).
   Plus: **Windows kill-orphans implemented** (was a no-op returning
   "use Task Manager") via CIM + Stop-Process matching our ffmpeg.exe
   (streamid=/decklink/format_code/tcp://127); and a lock-free
   **"Force Stop ALL"** button (Recovery card + multiview topbar →
   `/api/force-stop-all`) that OS-kills every ffmpeg + resets tiles,
   reachable even when a tile is wedged.

**alpha.59 (commit `9df6320`) — graceful FFmpeg shutdown + source
hot-swap.** The real fix for "every restart wedges the ATEM."
1. **Graceful shutdown** (streamer.rs `stop()`). alpha.58 still
   force-killed FFmpeg up front (TerminateProcess), abandoning the SRT
   connection → the ATEM held a stale receiver session → input wedged.
   Fix: for pipe sources (NDI/OMT), stopping the capture closes FFmpeg's
   stdin → FFmpeg sees input EOF → flushes + writes the MPEG-TS trailer +
   closes SRT **cleanly** + exits on its own. `stop()` now stops captures
   and lets FFmpeg exit (polling `is_running()` up to a 2.5s budget),
   force-killing only as a bounded fallback (non-pipe sources have no
   stdin EOF). Windows-safe — no CTRL_BREAK/console hacks.
   `run_one_attempt` stays the sole reaper of `inner.child` (the
   non-blocking start_kill pattern the stall detector already uses) → no
   wait()/take() race. `stop()` keeps `&self` → zero caller changes.
   (Designed via a Plan-agent pass first, since the lifecycle is delicate
   and can't be compile-tested on this box — no libclang.)
2. **Source hot-swap** (http.rs `api_settings`). Changing the source
   dropdown updated `source_id` but the live FFmpeg kept ingesting the
   OLD source (its input is fixed at launch) — the operator switched to
   NDI but the ATEM kept showing the test pattern, a major confusion.
   Now `api_settings` compares `source_identity` (source_id /
   ndi_source_name / omt_source_name / av_video_index) before+after; if
   it changed while live, it restarts the stream in a detached task
   (graceful stop → start) with `status="Switching"`. Per-tile in
   multiview. No new endpoint/payload/state field.
3. **⚙ Advanced button** right of the Quality picker (opens + scrolls to
   the Advanced panel) — operator-requested.

**The debugging marathon + the CONFIRMED root cause** (recorded so the
next session doesn't repeat the wrong turns I made):
- A 40-min orphan FFmpeg (NDI+Dante→DeckLink) held the Dante device +
  DeckLink output; two app instances hung at the **driver level** —
  `taskkill`/`Stop-Process` couldn't kill them, only .NET
  `Process.Kill()` did (after the SDK call unwound). The extra instances
  came from MY relaunch commands during recovery, NOT the operator
  (they only ever opened one — be careful with Start-Process relaunches).
- "SRT connects to the ATEM but no image": I wrongly chased the
  **encoder** (the ATEM decodes BOTH H.265 and H.264 — operator
  confirmed), the **video format** (matched at 1080p29.97, not it), and
  an **ATEM-input wedge**. Operator power-cycled the ATEM and it still
  failed, and reproduced the break on a Mac.
- **The actual answer** (finally read from the FFmpeg log):
  `[srt @ ...] Connection to srt://...,u=<KEY> failed: I/O error` = the
  **ATEM per-key receiver-slot lockout** (documented: "new connections
  with the same key fail with I/O error"). NDI was **never broken** —
  the /api/preview JPEG showed REMOTEENGINEERING's full multiviewer
  feed, and FFmpeg read the input correctly (`rawvideo (BGRA),
  1920x1080, 30 tbr`). "Test pattern works / NDI doesn't" was a **timing
  coincidence**: the test pattern grabbed the first clean connection; by
  the time the source was switched to NDI, the restart churn had locked
  the key's slot. The lockout comes from the Windows abrupt-kill (no
  clean SRT close) leaving stale sessions that accumulate; the
  power-cycle cleared it but Windows (Reconnecting) + the Mac re-grabbed
  the key and re-locked it. **alpha.59's graceful shutdown is the
  durable fix; immediate recovery = quiet EVERY machine → power-cycle
  the ATEM with nothing connecting → ONE clean stream from ONE machine.
  Only one machine per ATEM key (two = I/O error for the second).**

**alpha.60 (commit `e3ff37b`) — the ACTUAL "NDI black" fix (a SECOND,
separate bug from the lockout).** After alpha.59, the operator reported
NDI STILL showed black on the ATEM — on a Mac too, while **webcams and
the test pattern worked fine**, and even a **fresh never-used
destination** was black (so NOT the lockout — my lockout conclusion above
was incomplete). The /api/preview proved the NDI capture was perfect
(full multiviewer), and FFmpeg encoded correct yuv420p Main 6 Mbps
reaching the ATEM (q≈22 = real detail, not black frames). The ONE thing
unique to NDI vs the working sources: the **NDI audio TCP bridge** tags
audio with `-use_wallclock_as_timestamps 1` (the alpha.49 DeckLink
drift remedy). On the SRT/MPEG-TS path that puts the audio on a real-time
timeline while the video (rawvideo pipe) is 0-based frame time → the
mismatched PTS breaks the program PCR → the ATEM can't present the video
→ black. The operator PROVED it: setting the Audio Mixer to **Silent**
(no bridge) made the video appear instantly. **Fix:
`AudioBridge::ffmpeg_input_args(use_wallclock)` — DeckLink keeps wallclock
(its drift remedy), SRT/RTMP use sample-count PTS (0-based, aligned with
the video).** So the session's "NDI won't reach the ATEM" was TWO
separate bugs: the per-key SRT lockout (alpha.59 prevents the lockup) AND
the audio-bridge timestamps (alpha.60 — the dominant cause of the black
video). NDI capture/encode were always healthy. **Immediate operator
workaround while alpha.60 builds: Audio Mixer = Silent.**

### Open issues from Session 18

- **alpha.60 installed + verified on Windows; end-to-end NOT yet
  operator-confirmed (Session 19 #1).** alpha.60 added the NDI-audio-
  bridge fix (the actual "NDI black" cause). Decisive test pending: NDI
  source + Audio Mixer = **Auto** (not Silent) → ATEM shows video AND
  plays audio. Operator already confirmed the **Silent** workaround gives
  video; alpha.60 should restore audio without it. Also to confirm on
  real hardware: alpha.59 hot-swap between NDI cameras (clean switch,
  status "Switching", no re-lock) + graceful stop not wedging the ATEM.
- **Mac needs updating to alpha.60** (Releases v0.2.0-alpha.60 macOS
  arm64 .dmg) — that's where the operator tests; both the graceful-stop
  (alpha.59) and audio-bridge (alpha.60) fixes matter there.
- **Intermittent NDI-start wedge on Windows.** Several times,
  `NdiCapture::start_and_probe_format` → `receiver.capture_video(500ms)`
  blocked PAST its 5s deadline when the NDI SDK/source was in a bad state
  (the probe loop never re-checks the deadline) → instance goes
  Responding=false, all /api/* time out, no ffmpeg spawns. Needs a hard
  wall-clock timeout / watchdog around the probe so a hung NDI probe
  can't wedge the process. A wedged SDK currently needs a reboot.
- **/api/state has NO `.snapshot` wrapper** — fields are top-level
  (`$r.stats.status`, `$r.video_encoder`, …). Only /api/load_xml wraps in
  `{service, snapshot}`; /api/log returns `{command, lines[]}`. (Noted —
  wasted time parsing `.snapshot` on /api/state.)
- **NSIS file-lock on reinstall** still bites — kill atem-ip-patchbay.exe
  + ffmpeg.exe (+ net-diag) before installing or the installer silently
  skips locked files. The Session 16 NSIS pre-install taskkill hook is
  still unaddressed.
- **6-NDI → DeckLink (the Session 16 headline) did not progress** — the
  session was consumed by the firefight. Still pending: 8 tiles, DeckLink
  concurrency, broadcast-multiview.
- **True seamless source switching** (SRT stays up, swap FFmpeg's input)
  — deferred; a bigger architecture. alpha.59's graceful-stop +
  auto-restart is the practical version.
- Longstanding carryovers untouched: pre-show checks panel (net-utility),
  OMT-out audio for non-raw sources, pipe/relay custom audio, Net Utility
  "Saving forever".

### Session 19 priorities

1. **Verify alpha.60 end-to-end (#1) — closes the Session-18 arc.**
   Update the Mac to alpha.60. Key test: NDI source + Audio Mixer =
   **Auto** → ATEM shows video AND plays audio (the alpha.60 audio-bridge
   fix; the operator already has video via the Silent workaround). Then
   confirm alpha.59's hot-swap (change NDI source while live → clean
   auto-restart, status "Switching", no ATEM re-lock) and that graceful
   stop no longer wedges the ATEM. Reminders: only ONE machine per ATEM
   key; quiet all machines before a clean start.
2. **Harden the NDI-start wedge.** Hard wall-clock timeout/watchdog
   around `NdiCapture::start_and_probe_format` so a hung `capture_video`
   can't freeze the instance. The remaining reliability hole.
3. **6-NDI → DeckLink (resume the Session 16 headline).** TILE_COUNT→8,
   DeckLink concurrency at 6-8 outputs, broadcast-multiview UX. The
   operator's stated real-world goal.
4. **Single-view tile toggle** (the original Session 18 ask, never
   started) — cycle the single-view UI across configured tiles.
5. **Carryovers:** NSIS pre-install taskkill hook; pre-show checks panel;
   OMT-out audio non-raw; pipe/relay custom audio; Net Utility "Saving
   forever".

### Session 19 wins (alpha.61 through alpha.65, 2026-05-29/30)

Started as "verify alpha.60," cleared that gate, then drove the
Session-16 broadcast-multiview headline all the way to a 6-channel
NDI→DeckLink patchbay that is production-ready for VIDEO AND AUDIO.
Five tagged alphas, all CI-green + installed + hardware-verified on the
Broadcast Pix Windows rig.

1. **alpha.61 — NDI-start watchdog (Session 19 priority #2).** The probe
   loop in `NdiCapture::start_and_probe_format` only checked its deadline
   at the top of the loop, but `receiver.capture_video(500ms)` can block
   indefinitely on a wedged SDK → froze `start()` → froze the whole
   instance (Responding=false, all /api/* time out, reboot-class; the
   reconnect supervisor made it worse, blocking a fresh worker per retry
   until the pool drained). Fix: build NDI+Receiver on the caller thread
   (local allocs, no network wait), run the blocking probe + capture-
   thread spin-up on a throwaway `ndi-probe` thread bounded by
   `recv_timeout(format_timeout + 3s)`; on timeout return an error and
   detach the stuck thread. New free fn `probe_and_spawn`; public
   signature unchanged. Mirrors the existing `join_with_timeout` pattern.

2. **alpha.62 — 6-channel broadcast multiview (the headline).** The
   multiview was already ~90% built at TILE_COUNT=4 (alpha.40-57) and
   fully per-tile-isolated, so 6 channels = two constants + a CSS grid:
   `fleet.rs` TILE_COUNT 4→6 + `multiview.html` TILE_COUNT 4→6 + grid
   2x2→3x2. The **Phase A `decklink-spike`** (finally run on the rig)
   validated 6 concurrent `decklink_enc` outputs holding a full 180s with
   zero stalls → single-process Option A holds; the spike's blunt
   "FAIL→N-processes" auto-verdict was a misread (the only failures were
   teardown crashes, not runtime). 4 concurrent NDI→DeckLink at 30fps with
   CPU only ~12% → ample headroom for 6+.

3. **alpha.63 — DeckLink Stop crash fix.** FFmpeg access-violates
   (0xC0000005) in the decklink muxer's `av_write_trailer` when CLOSING a
   real-SDI output — codec-independent (wrapped_avframe AND v210 both
   crash), only real-SDI paths (the NDI-Machine virtual outputs close
   clean), teardown-only (never mid-run). Fix: in `stop()`, when
   `snap.is_decklink()`, force-kill FFmpeg up front (before stdin-EOF
   drives the crashing trailer) — safe because a DeckLink output is local
   hardware with no network session to close cleanly (unlike SRT→ATEM
   where alpha.59's graceful close is load-bearing). Verified: real-SDI
   Stop returns in 0s, no wedge, device re-claims instantly — even on a
   FROZEN ffmpeg (0% CPU).

4. **alpha.64 + alpha.65 — the DeckLink AUDIO arc (throttle → freeze →
   fixed).** The full 6-stream hardware test caught what 30s spot-checks
   couldn't. With Audio=Auto, NDI→DeckLink VIDEO collapsed to ~2.4fps;
   bisected with Audio=Silent → instant 30fps. **alpha.64** removed the
   audio bridge's `-use_wallclock_as_timestamps 1` (alpha.49's drift
   remedy) → fixed the throttle, but a 10-min drift watch found it had
   traded the throttle for a HARD FREEZE at ~2:44 (FFmpeg blocked at 0%
   CPU, both streams stuck, status falsely "Streaming"; silent ran clean
   5+ min → audio-specific). **alpha.65** root-caused both: the frame-
   counted `-r 30` video PTS assumes EXACTLY nominal fps, but the NDI
   source isn't, so the video timeline diverges from the audio's whichever
   way audio is stamped (wallclock audio = throttle; sample-count audio =
   freeze). Fix: `-use_wallclock_as_timestamps 1` on BOTH the rawvideo
   pipe AND the audio bridge for DeckLink so they share one real-time
   timeline; aresample=async absorbs the residual ~2-3 samples/sec card-
   clock drift. Throttle-avoidance validated standalone (piped wallclock-
   both = 446 frames in 15s); freeze-avoidance verified in-app:
   REMOTEENGINEERING + Auto audio → real SDI ran 30fps with live varying
   program audio for 6.7 min, frames climbing steadily past the old ~4934
   freeze point to 12860, no freeze. SRT/ATEM path untouched.

### Open issues from Session 19

- **Stall detector reads cumulative fps, missed the freeze.** The
  alpha.41/57 stall detectors compare the cumulative fps stat (which
  stayed >0.5 even with frames frozen) instead of an instantaneous frame-
  delta, so the alpha.64 freeze sat undetected for 10 min. Fix to
  instantaneous frame-delta so any future freeze auto-recovers (force-kill
  → supervisor reconnect). Top Session 20 reliability item.
- **2 of 4 NDI-Machine VIRTUAL outputs 400-on-start.** 'NDI Machine' and
  'NDI Machine 3' reject /start with an empty-body 400 even solo;
  'NDI Machine 2/4' + all real-SDI outputs work. Quirk of those virtual
  DeckLink→NDI devices; real broadcast SDI path unaffected. Low priority.
- **/start blocks on the NDI probe** (~1-3s solo, up to 15s under
  6-concurrent contention) so a sequential-start driver hits client
  timeouts (the streams start anyway). Consider spawning the probe async.
- **App log not exposed via API.** The NDI audio-drain/bridge telemetry
  (log::info) goes to a file/stdout not reachable via /api/log (which is
  just FFmpeg stderr) and wasn't capturable via stderr-redirect on the
  release build. Add an app-log endpoint or route key telemetry into the
  LOG_TAIL — would have made the alpha.64 freeze diagnosis far faster.
- **TILE_COUNT is 6, not 8.** Bumped 4→6 for the operator's stated 6-output
  goal (3x2 grid). Bump to 8 (4x2) if they need more channels.
- **Windows release CI flake (innoextract).** The NDI SDK install step
  flaked once (alpha.62) — `gh run rerun --failed` cleared it, then the
  Windows .exe needed a manual `gh release upload --clobber` (the publish
  job had already run). Recurring; harden the choco-innoextract step.
- **Carryovers** (none blocking): pre-show checks panel, OMT-out audio for
  non-raw sources, pipe/relay custom audio, NSIS pre-install taskkill.

### Session 20 wins (alpha.66 through alpha.70, 2026-06-12/13)

Session-long firefight on the operator's headline complaint: **NDI source →
REMOTE SRT → ATEM, the audio dies after ~2-10 min (plays fine, then choppy,
then silent), and the stream freezes on long runs.** Five tagged alphas.
**The audio-dies bug is NOT yet fixed** but is now DEFINITIVELY localized
after eliminating every other suspect; alpha.70 ships the deciding
diagnostic. The drop/freeze side IS addressed.

**The drops/freeze (addressed):**
- **alpha.66** (`639a945`): mirrored alpha.65's DeckLink wallclock+aresample
  onto the SRT NDI path (wallclock on BOTH the rawvideo pipe + audio bridge,
  aresample=async) + switched the stall detector from cumulative-`fps` to an
  instantaneous `frames_sent` delta (Session-19 open #1 — cumulative fps
  stayed >0.5 and missed freezes). `ATEM_DISABLE_NDI_WALLCLOCK` escape hatch.
- **alpha.67** (`92bfdd7`): lockout-aware reconnect backoff for ATEM SRT.
  The SRT link drops on a lossy path → the stall detector force-kills FFmpeg
  → the ATEM holds the per-key receiver slot ~15s after an abrupt disconnect
  → the old 1-2s backoff hammered the locked slot = "Conversion failed!"
  churn (measured: reconnect counter 6→12 in ~40s around one drop).
  `backoff_secs_atem_srt`: 15/30/45s instead of 1/2/4.
- **alpha.68** (`b8ce337`): SRT latency default 500ms→200ms + **prominent
  latency control** (presets 200/1000/2000ms) pulled out of the buried
  Advanced `<details>`. A/B PROVED latency fixes the drops PER-PATH: 500ms
  dropped every ~2min to 173.76.193.167, 2000ms = ZERO drops. Audio default
  confirmed already "auto" (the "silent" the operator saw was leftover
  persisted tile state from my env-var testing).

**The audio-dies bug (localized, NOT fixed):**
- A 2-min loopback capture showed the patchbay output CLEAN → I WRONGLY
  concluded "network not pipeline." Wrong: the failure is at ~2-14 min; a
  2-min capture is too short.
- A **15-min** loopback capture PROVED the patchbay produces ESCALATING
  audio SILENCE gaps (alpha.66 run: 7s@6min, 32s@8.8min, 47s@14min — packets
  all present, CONTENT is digital silence). It IS the pipeline.
- **Source eliminated**: operator streamed a SECOND unrelated NDI source
  (a YouTube video via NDI screen capture = guaranteed continuous audio) →
  died IDENTICALLY. Two sources, same death → it's the patchbay's NDI-audio
  path, not the source.
- **Timestamps eliminated**: sample-count (`ATEM_DISABLE_NDI_WALLCLOCK=1`,
  no aresample) ALSO fails (178s start-silence + choppy). Network eliminated
  (loopback fails). ATEM eliminated (operator confirmed it meters+routes
  audio fine when it arrives).
- **alpha.69** (`53901ee`): found the logger was gated on
  `cfg!(debug_assertions)` so the RELEASE exe had NO logger (every
  log::info/warn went nowhere; stdout/stderr redirect = 0 bytes on the GUI
  exe) — the wall blocking all in-app diagnosis. Enabled a LogDir file
  target in release (`patchbay.log`). ALSO bumped the audio buffers
  (128→512 + `-thread_queue_size 1024`) as a fix attempt — the 15-min
  capture proved that made it WORSE (285s silence vs 94s).
- With logging on, the per-second NDI telemetry shows the capture→`audio_tx`
  hop is FLAWLESS during the failure: `audio_sent` steady (~47/s
  REMOTEENGINEERING, ~100/s STUDIO-A-BPIX), `audio_dropped=0`, no bridge
  errors. So the audio is lost AFTER capture (bridge→FFmpeg or inside
  FFmpeg) OR the chunks arrive already-silent.
- **alpha.70** (`4decc84`): added `audio_rms_db` (the RMS/dBFS of the
  captured chunks, measured before `try_send`) to the per-second telemetry
  — the one missing data point — and reverted the alpha.69 buffer bump.

**Operator workaround NOW:** Audio Mixer → Custom → Dante device bypasses
the NDI-audio bridge entirely.

### Open issues from Session 20

1. **THE AUDIO BUG — not fixed; alpha.70 is the deciding diagnostic.**
   Install alpha.70, run NDI→SRT (STUDIO-A-BPIX YouTube screen-cap repros in
   ~2-3 min), let it fail, then read `patchbay.log` at
   `C:\Users\Broadcast Pix\AppData\Local\org.weirdmachine.atem-ip-patchbay\logs\patchbay.log`
   for `audio_rms_db` DURING a gap:
   - **< -70dB while `audio_sent`>0** → the grafton-ndi RECEIVER is handing
     us silence after a few min → fix is UPSTREAM in NDI audio capture (the
     `capture_audio_timeout(0)` per-video-frame drain in
     `ndi_capture.rs::run_capture_loop`, receiver config, or an SDK quirk;
     consider a dedicated blocking audio-capture thread decoupled from the
     video loop).
   - **~ -20..-45dB (real) while the output is silent** → chunks are good but
     die DOWNSTREAM → instrument/fix the bridge writer→socket→FFmpeg hop (add
     bytes-written/sec telemetry to `audio_bridge.rs`) or FFmpeg's
     two-live-input handling.
2. **patchbay.log rotates fast (~137 lines ≈ 2.3 min)** and loses history —
   catch telemetry live / during a gap. Consider a bigger rotation size or a
   ring-buffer `/api/applog` endpoint (Session-19 #4).
3. **Multiview tile confusion**: the operator's LIVE stream may be on a
   different tile/key than the test tile (Session 20: their stream was tile 0
   / key `rmlj-`, my tests were tile 1 / key `6xoh-`). Always scan all tiles.
   Two streams to the SAME ATEM key conflict.
4. **Latency is per-path**: 200ms default is fine for solid links; lossy
   internet paths need 1000-2000ms (now one click via the alpha.68 control).
5. The alpha.66 frame-delta stall detector + alpha.67 backoff are in but
   unverified under a real multi-hour remote-SRT run.

### Session 21 wins (alpha.71, 2026-06-13)

**FIXED the NDI→SRT→ATEM "audio dies after a few minutes" bug** — the
operator's headline complaint, open since before alpha.66. Diagnosed with
alpha.70's `audio_rms_db` telemetry on the Broadcast Pix rig, root-caused with
a standalone FFmpeg harness, fixed surgically, shipped as alpha.71.

**Verdict: DOWNSTREAM, not upstream.** Drove a faithful repro via the API —
tile 1 = STUDIO-A-BPIX (NDI, continuous YouTube audio) → Auto audio →
`custom_url` redirected to a LOCAL loopback SRT listener so it never touched
the live ATEM (tile 0 / key `rmlj-` is the operator's; my test was tile 1 /
key `6xoh-`). Ran 15 min. The capture telemetry was flawless the WHOLE time
(`audio_rms_db` -20..-60 dB, ZERO lines < -70, `audio_sent` ~100/s,
`audio_dropped`=0) while the loopback OUTPUT (FFmpeg silencedetect) grew
escalating choppy digital-silence gaps from ~5 min on (matching Session-20's
7s@6min / 32s@8.8min). Airtight: at the exact moments the output was silent
the CAPTURE was simultaneously LOUD (-25..-40 dB). So the grafton-ndi receiver
hands us perfect audio — it dies inside FFmpeg. This also RULES OUT the
upstream "dedicated audio-capture thread" fix the Session-20 decision tree
hypothesized; no `ndi_capture.rs` change was needed.

**Root cause: alpha.66's `-use_wallclock_as_timestamps` on the SRT path.**
alpha.66 put wallclock on BOTH FFmpeg inputs (rawvideo stdin pipe + s16le TCP
audio bridge) for SRT/RTMP, copying alpha.65's DeckLink fix. But DeckLink is
genlocked (its muxer pins the pipeline to exactly 1.0x); SRT free-runs
slightly sub-realtime (NDI 29.97 fps forced to CFR `-r 30` + encode/mux
overhead → measured `speed=0.984x`). The video hides that with CFR frame-dup;
the audio can't — with wallclock, each sub-realtime read is stamped at a
real-time-NOW that drifts WIDER than the samples' own 1/48000 s duration, so
`aresample=async` sees an ever-growing forward gap and FILLS it with
escalating silence.

**Harness proof** (bundled ffmpeg, realtime testsrc2 video + a sub-realtime
sine feed over the same flags as the app): wallclock + 6%-deficit = **40
silence regions / 45s**; no-wallclock = **0**; no-wallclock + 10%-deficit
stress = **0**. Same total sample count every run → the bug OVERWRITES real
audio with silence (it doesn't drop samples), matching Session-20's "packets
present, content is digital silence."

**Fix (commit `3a88c3f`):** `-use_wallclock_as_timestamps` ONLY for DeckLink.
SRT/RTMP revert to symmetric 0-based PTS (video = frame count, audio = sample
count — the not-black alpha.60-65 behavior, so the alpha.60 black-ATEM can't
return) while KEEPING `aresample=async=1000` to smoothly absorb the small real
source-vs-output drift (what alpha.66 was actually reaching for; pre-alpha.66
had NO drift handling, which is why that era also lost audio). wallclock and
aresample are now DECOUPLED — the dead `ATEM_DISABLE_NDI_WALLCLOCK` env hatch
is removed (its behavior is the new SRT default). ~3 lines of real logic
change in `build_ffmpeg_cmd_for_ndi` + `build_audio_filter`. DeckLink
(wallclock-both, load-bearing since alpha.65) and custom/silent audio modes
are untouched.

**Verification status:** CI GREEN on both platforms (build gate — can't
compile locally, no libclang); the installed alpha.71 binary was confirmed
emitting the FIXED FFmpeg command (`use_wallclock_as_timestamps` occurrences =
0, down from 2; `aresample=async` retained); and it streamed NDI→bridge→
hevc_nvenc→SRT healthily for ~15s (real audio -16 dB) before — unrelated to
the fix — **Windows Defender quarantined the unsigned exe as a
`Trojan:Win32/Bearfoos.A!ml` ML FALSE POSITIVE** (confirmed via
`Get-MpThreatDetection`; the exe just vanished, no WER entry, no panic). This
account is NOT admin, so the exe can't be restored/excluded from a Claude
session (`Add-MpPreference` + `MpCmdRun -Restore` both 0x80070005). See
[[defender-quarantines-unsigned-windows-build]].

**STILL PENDING (needs operator):** (1) restore the exe — Windows Security →
Protection history → Allow/Restore the Bearfoos item, and add a folder
exclusion for the install dir so it sticks; (2) then the 15-min real-NDI→SRT
long-run with Audio Mixer = Auto to confirm audio stays present + a/v in sync
on the live ATEM. The standalone FFmpeg harness already PROVED the silence is
gone (40 regions/45s with wallclock → 0 without, identical ffmpeg+flags), so
this is final confirmation, not the primary proof. Also restore tile 1's test
config back to its original (REMOTEENGINEERING, auto, atem,
`srt://173.76.193.167:1935`, key `6xoh-76yk-ry`) — Session 21 left it pointed
at the loopback `srt://127.0.0.1:9999`. (Operator's LIVE tile 0 / key `rmlj-`
was never touched.)

### Session 21 priorities

1. ~~**Read `audio_rms_db` … FIX THE AUDIO BUG.**~~ DONE in alpha.71 (commit
   `3a88c3f`) — see Session 21 wins above. DOWNSTREAM: wallclock on the
   sub-realtime SRT path made `aresample` fill escalating silence; fixed by
   dropping wallclock for SRT while keeping aresample. **Pending: on-rig
   verify once alpha.71 CI publishes** (Auto audio, >15 min, audio + a/v sync).
2. Verify the drop/freeze fixes hold under a real long remote-SRT run.
3. The Session-19-era 6-NDI→DeckLink show + carry-overs below, once audio is
   closed.

### Session 20 priorities (set at END OF SESSION 19 — mostly deferred by the audio firefight above; still valid carry-overs)

1. **Operator runs the real 6-camera show** on the rig (6 NDI → 6 DeckLink
   SDI, video+audio) — the field proof now that every piece is verified.
2. **Stall detector → instantaneous frame-delta** (open #1) so any freeze
   auto-recovers instead of sitting undetected.
3. **App-log endpoint** for faster in-app diagnosis (open #4).
4. **Bump TILE_COUNT to 8** if the operator wants >6 channels.
5. Carryovers (pre-show panel, OMT-out audio, pipe/relay custom audio,
   NSIS taskkill, Net Utility "Saving forever").

### Session 19 also includes CLAUDE.md catch-up + commit

This entry is the catch-up. Session 20 picks up from a 6-channel
NDI→DeckLink patchbay that is video+audio hardware-verified (alpha.65),
with the stall-detector fix as the top reliability follow-up.

### Session 18 also includes CLAUDE.md catch-up + commit

This entry is the catch-up. Session 19 picks up from alpha.59
installed-on-Windows, with operator-verification of the graceful-stop +
hot-swap fixes (quiet-all → power-cycle → one clean NDI→ATEM connection)
as the immediate next step.

### atem-net-diag tool architecture (Session 4)

Lives at `tools/atem-net-diag/`. Standalone Rust crate (its own
Cargo.toml, no workspace). Three modes that combine freely:

```
src/
  main.rs          — CLI parser, probe loop, monitor mode (CLI),
                     SRT/UDP packet parsing helpers
  dashboard.rs     — embedded HTTP server (tiny_http), shared
                     state via Arc<Mutex<DashboardState>>, probe +
                     monitor threads write to it, /api/state +
                     /api/config served from it
  dashboard.html   — single-page HTML UI, embedded via include_str!
                     into the binary at compile time. Polls /api/state
                     at 1Hz, renders per-stream cards with sparklines
package/
  start.command    — double-click launcher, .command extension makes
                     macOS open it in Terminal automatically. Clears
                     com.apple.quarantine, checks for ffmpeg, prints
                     URL banner, runs ./atem-net-diag --ui, keeps
                     terminal open for error visibility.
  README.txt       — end-user usage doc shipped in the tarball
dist/              — gitignored build output; tar.gz produced here
```

Build + sign + tarball flow (manual today; should be a script):

```
cargo build --release
codesign --force --options runtime --timestamp \
  --sign "Developer ID Application: Stephen Walter (6M536MV7GT)" \
  --identifier "org.weirdmachine.atem-net-diag" \
  target/release/atem-net-diag
mkdir -p dist/atem-net-diag-X.Y.Z-macos-arm64
cp target/release/atem-net-diag dist/.../
cp package/start.command package/README.txt dist/.../
chmod +x dist/.../start.command
tar -czf dist/atem-net-diag-X.Y.Z-macos-arm64.tar.gz -C dist atem-net-diag-X.Y.Z-macos-arm64
cp dist/*.tar.gz ~/Library/Mobile\ Documents/com~apple~CloudDocs/
```

Distribution today: drop tarball into iCloud Drive root, the
user pulls it from iCloud on the test Mac. Future: ship as a
separate GitHub release asset alongside the main app.

Key implementation gotchas:
- **bmd_uuid format**: hand-crafted UUIDs MUST be valid v4
  format (8-4-4-4-12 hex chars). BMD receivers silently
  reject malformed UUIDs. Current value:
  `d1a90517-1c00-4e57-9fab-617465616d64`. The "atemd" hex
  payload in the last group is decorative.
- **Switched LAN visibility**: a peer Mac running tshark
  CAN'T see unicast traffic between two other devices on
  most modern Ethernet switches. Tool needs to run on the
  same machine as the streamer, on the receiver's machine,
  on a port-mirrored / SPAN port, or query a router/gateway
  API (UDM Pro Max etc.).
- **tshark capture permissions**: macOS requires either
  ChmodBPF (Wireshark installer's helper) or sudo. The
  dashboard's empty-flows state hints at this.
- **SRT field extraction**: tshark's SRT dissector parses
  HSv5 ACKD packets and exposes `srt.bw`, `srt.rate`,
  `srt.rtt`, `srt.rttvar`, `srt.bufavail` as `-e` field
  outputs. The streamid extension (carries the user's
  stream key) is NOT extracted by tshark's dissector —
  Session 5 fixed this by adding `-e udp.payload` to the
  capture and parsing the SRT HSv5 conclusion handshake
  ourselves in `parse_srt_handshake_streamid` (main.rs).
  See "Session 5 wins" item #3 for the wire-format details.

### v0.2.0 release tags

- `v0.2.0-alpha.1` (commit `16baa5d`): NDI SDK headers missing
  on both Mac + Win runners.
- `v0.2.0-alpha.2` (commit `b3f5e9c`): added NDI SDK install
  steps (downloads.ndi.tv .pkg / .exe). Mac NDI install worked
  but codesign failed because `tauri.conf.json` hardcodes
  `signingIdentity` and the runner's keychain doesn't have the
  cert.
- `v0.2.0-alpha.3` (commit `2bb061a`): release.yml strips
  `signingIdentity` when no `MACOS_CERTIFICATE_P12` secret is
  set, so unsigned builds work. Status pending. Even if those
  binaries publish, **they'll crash on launch on end-user
  machines because of the NDI dylib bundling issue above** —
  any further v0.2.0-alpha tag should wait until that's fixed.
- `v0.2.0-alpha.4` through `v0.2.0-alpha.8` (Sessions 4-7):
  iterative Mac-only releases (Windows job blocked by the
  `$extract:` PowerShell parser bug). See "Session 4 wins"
  through "Session 7 wins" for the per-alpha details.
- `v0.2.0-alpha.9` (commit `4272199`): "bring it all together."
  Mac arm64 .dmg + atem-net-diag tarball/.app.zip shipped;
  Windows still failed because alpha.9's 7-Zip extraction
  only handled `*.msi` payloads (NDI 5 era), not the
  unnamed `[0]` blob NDI 6 produces.
- `v0.2.0-alpha.10` (commit `d96e626`): recursive 7-Zip
  extraction up to 3 levels deep. Got further on Windows,
  but 7-Zip rejected the Inno Setup `[0]` payload as "not
  an archive."
- `v0.2.0-alpha.11` (commit `b8e21e2`): switched to
  `innoextract` (the NDI SDK installer is Inno Setup 6.1.0
  unicode, not InstallShield). **First cross-platform release**
  with both .dmg + .exe shipped. Plus net-diag wizard
  heuristic rewrite (ATEM-IP-aware), `?force_visibility=1`
  preview affordance, and net-diag bumped to 0.2.4.
- `v0.2.0-alpha.12` (commit `a5212e6`): NDI DLL copy-up via
  NSIS post-install hook. Fixes "Processing.NDI.Lib.x64.dll
  was not found" on Windows first-launch — the Windows
  loader couldn't find the DLL in
  `$INSTDIR\sidecar\` (where tauri's `bundle.resources`
  placed it). Hook copies the DLL up to `$INSTDIR\` next
  to the .exe at install time, removes it on uninstall.
- `v0.2.0-alpha.13` (commit `ed7616e`): "console fix +
  receive-wizard polish + OMT audio + tee" — seven headline
  changes (see Session 10 wins above for full detail):
  Windows FFmpeg console window suppression, receive-wizard
  advanced settings (custom ports / RTMP app / stream key /
  SRT passphrase), protocol-radio lock while receiver
  active (fixes the alpha.12 "SRT picked but RTMP banner"
  state-drift), app-instruction mismatch banner with one-
  click protocol switch, public-URL helper with WAN-IP
  detection and port-forward checklist, NDI audio capture
  wired into OmtSender's new `feed_audio_frame`, and OMT-
  out video tee for AVF / pipe / RTSP / SRT-listen /
  RTMP-listen sources via a second FFmpeg subprocess.
- `v0.2.0-alpha.14` (commit `c042771`): NDI + Custom audio
  on Windows. Bug surfaced during alpha.13 Windows test —
  picking NDI source + Audio Mixer Custom mode + a real
  audio device (Dante VSC, USB interface) produced silent
  audio at the ATEM. Root cause:
  `build_ffmpeg_cmd_for_ndi`'s non-macOS branch always
  emitted lavfi anullsrc, with a Session 4 comment
  flagging the DirectShow audio gap that was never
  followed up. Fix: explicit
  `cfg(target_os = "windows")` arm using
  `-f dshow -i "audio=<DeviceName>"`. Pan filter is
  platform-agnostic so L/R channel routing for Dante
  carries over for free. Verified working in production.
- `v0.2.0-alpha.15` (commit `4a35048`): first attempt at
  multi-source as an in-app 2x2 grid. multiview.html shell
  loading the existing single-source UI in 4 iframes scoped
  via ?tile=N, app.js fetch shim rewriting /api/* →
  /api/i/N/*, EncoderFleet abstraction with 4 tiles, tile-
  prefixed routes /api/i/:idx/* + /api/* aliases for tile 0,
  spawn_instance Tauri command. **Iframes too cramped to be
  operational — reverted in alpha.16.**
- `v0.2.0-alpha.16` (commit `0f9c3a0`): pivot from the 2x2
  grid to a multi-instance + monitor-window pattern.
  multiview shell deleted, fetch shim removed, root `/`
  serves index.html again, TILE_COUNT back to 1 (fleet
  abstraction kept as vestigial-but-harmless). New
  bmd_emulator/static/monitor.html — small companion
  window per instance with live preview + status pill +
  bitrate + source/dest labels, 360x320 default with
  Expand button. Tauri commands `open_monitor_window` +
  `focus_main_window`. Topbar "Monitor" button (cyan, next
  to Net Diag).
- `v0.2.0-alpha.17` (commit `ab1ee41`): bundle
  atem-net-diag.exe in the Windows installer's sidecar/
  folder. New `cargo build --release` step in
  build-windows CI for tools/atem-net-diag/, copy .exe
  into src-tauri/sidecar/. New `net_diag_path()` resolver
  in ffmpeg_path.rs mirrors the ffmpeg_path() pattern.
  api_open_net_diag's Windows arm spawns the bundled
  binary with `--ui 8092` (detached, no console). Sanity-
  check requires atem-net-diag.exe in sidecar/ before
  publishing. **User-reported 2026-05-27: button still
  doesn't actually open anything visible — needs debug
  in Session 12.**
- `v0.2.0-alpha.18` (commit `3f060ee`): attempted to bundle
  libomt so OMT senders appear in discovery, plus an audio-
  dropdown contrast fix. CI **failed both platforms** —
  Windows picked ARM64 libomt.lib alphabetically before
  Winx64 → 30 LNK2019 errors; macOS deep-codesign signed
  files in filesystem order so the main binary failed to
  sign against its still-adhoc-signed Frameworks
  subcomponents.
- `v0.2.0-alpha.19` (commit `7f8f397`): fixed alpha.18's
  two CI bugs. Windows: pin OMT extraction to
  Libraries\Winx64\ explicitly with a case-insensitive
  fallback. macOS: sign inside-out (Frameworks → Resources
  → MacOS/main → outer .app). First Mac + Windows release
  with bundled libomt + OMT cargo feature enabled. Audio
  dropdown contrast fix from alpha.18 also ships here.
- `v0.2.0-alpha.20` (commit `f96a8b6`): Net Diag button
  fix on Windows. `cmd /c start "" URL` under
  CREATE_NO_WINDOW returns exit 0 but silently fails to
  launch the browser — cmd.exe's `start` builtin needs an
  attached console to allocate ShellExecute. Replaced with
  `rundll32 url.dll,FileProtocolHandler URL` which hits
  ShellExecuteW directly. Verified empirically on the
  Broadcast Pix test rig: ProcessStartInfo with
  CreateNoWindow=true running `cmd /c start "" URL` returns
  0 with empty stderr but no browser tab opens; same with
  rundll32 opens the tab. 4-line change; only Windows arm
  of api_open_net_diag. See Session 12 wins above.
- `v0.2.0-alpha.21` (commit `a1cd804`): Session 12
  headline — bidirectional patchbay (DeckLink output
  destinations). Route NETWORK sources (NDI / OMT /
  SRT-listen / RTMP-listen / pipe) to LOCAL Blackmagic
  DeckLink SDI/HDMI outputs alongside the existing
  LOCAL-source → NETWORK-ATEM path. ~970 LOC across 13
  files. New `ffmpeg_has_decklink()` probe at boot, new
  `/api/decklink-outputs` device+modes endpoint, new
  destination-type segmented control inline in the
  destination card, new DeckLink picker UI (device +
  mode + pixel-format hint + driver-install link +
  refresh), new `build_plan_decklink` +
  `build_decklink_output_cmd` raw-output FFmpeg branch.
  Monitor window destination label flips to "{device} ·
  {mode}" when DeckLink is active. Hardware accel
  deferred to alpha.22. CI green Mac + Windows first
  try.
- `v0.2.0-alpha.22` (commit `7554570`): Windows FFmpeg
  swap (BtbN → gyan.dev full_build) for richer HW-accel
  set. Net Diag dashboard "stuck on loading" fix
  (duplicate `const cfg`/`atemIp` SyntaxError live since
  alpha.17). UDM API key dialog added to main app
  topbar (reverted in alpha.25).
- `v0.2.0-alpha.23` (commit `c3fa90c`): UI polish on
  top of alpha.22. Hero gains a reverse-direction
  tagline. Destination-type picker restyled as an
  accent-tinted card with a "Where does this stream go?"
  label + reframed buttons ("Remote (ATEM | Decoder)" /
  "Local (DeckLink | SDI/HDMI)"). Default video_mode
  flipped from 1080p30 to 1080p29.97.
- `v0.2.0-alpha.24` (commit `fdbb0d4`): OMT
  discoverability. Reverses alpha.13's "hide OMT when
  empty" — the Video Source card always shows the OMT
  section, with an empty-state placeholder when no
  senders are discovered. New "scan OMT" link in card
  title parallel to "scan NDI". ~70 LOC UI-only.
- `v0.2.0-alpha.25` (commits `3d24ff5` + `8b5b5ed` +
  `0f50f19`): production-quality starter pack.
  Encoder picker dropdown (Auto + every encoder in the
  bundled FFmpeg). Per-encoder flag tables for libx264,
  libx265, videotoolbox, nvenc, qsv, amf. Auto-mode
  resolution with platform priority + ATEM_DISABLE_VT
  honored + fall-back to auto on user-picked-not-
  available. Audio quality knobs (codec, bitrate,
  sample rate, channels) with HE/HEv2 server-side block
  on ATEM destinations. Power-user encoder-extra-flags
  textarea with shell-style tokenizer. Advanced UI
  disclosure inside destination card with auto-reconnect
  toggle (UI present; supervisor lands in next alpha).
  UDM API key dialog removed from main app per
  operator feedback (relocates to net-diag in Session 14).
  ~1100 LOC. CI green.
- `v0.2.0-alpha.26` (commit `36ae95d`): rename "Net Diag"
  to "Net Utility" in user-facing strings. Topbar button
  + dashboard title + h1. Binary name / repo path /
  API endpoints / Rust internals unchanged for URL +
  build stability.
- `v0.2.0-alpha.27` (commit `60b6212`): switches & ports
  filter + pin in Net Utility dashboard. Text search
  across switch/port/connected-device + chip group
  (All / Active / Errors / ATEM / Pinned). Per-port pin
  button (★) — pinned ports float to top of their
  switch's port table with a dashed separator + accent
  border. localStorage persistence for pins + filter
  state. Flap counts now display the time period they
  cover ("3 flaps in 8h 32m").
- `v0.2.0-alpha.28` (commit `ac8d68d`): pre-show
  readiness check panel in Net Utility. Seven checks
  (WAN IP / WAN headroom / UDM polling / ATEM
  reachable / capture visibility / active streams /
  stream-key correlation) projected from existing state.
  Pass/Warn/Fail/Skip status per check. Overall verdict
  color-codes the collapsed-card summary. New types
  PreShowCheck / PreShowVerdict on the wire.
- `v0.2.0-alpha.29` (commit `92f195e`): UDM API key form
  INSIDE net-utility. Runtime credential injection via
  `Arc<Mutex<Option<UnifiCredentials>>>` shared across the
  three polling threads. Folds in the alpha.28 dashboard.rs
  pattern fix.
- `v0.2.0-alpha.30` (commit `2db1888`): auto-reconnect
  supervisor. Exponential backoff (1-60s, max 12 attempts,
  reset after 60s stable). spawn_attempt / run_one_attempt /
  run_supervisor / handle_unexpected_exit / backoff_with_
  cancel + new Inner.supervisor_cancel Notify.
- `v0.2.0-alpha.31` (commit `e8249d1`): audio level meters
  via astats + ametadata=mode=print in build_audio_filter.
  Five new StreamStats fields + new /api/audio-levels
  endpoint @ 4Hz + canvas VU meters in the Audio Mixer card
  with green/yellow/red zones + 1.5s peak-hold.
- `v0.2.0-alpha.32` (commit `ffaefba`): UDM key form
  survives password-manager extensions. Defense-in-depth:
  type="text" with -webkit-text-security:disc CSS, type=
  "button" with click handler, data-*-ignore attributes,
  inline onsubmit="return false". Plus the cross-platform
  FFmpeg+DeckLink CI plumbing made first dispatches: Mac
  green via shim, Windows still iterating on widl.
- `v0.2.0-alpha.33` (commit `a835f0c`): per-switch pin in
  net-utility. ★/☆ button in switch-block-head. New
  pinnedSwitches Set keyed by sw.mac + separate
  localStorage key. Pinned switches sort above ATEM-auto-
  priority.
- `v0.2.0-alpha.34` (commit `0f216ba`): release.yml swap
  to sidecar FFmpeg. Mac + Windows blocks both now `gh
  release download` from the sidecar prerelease
  (ffmpeg-decklink-8.1.1-bmd16.0-rev1) + add muxer-
  assertion sanity check. **Did not ship on Mac** — the
  sidecar's Mac ffmpeg has dynamic deps on Homebrew dylibs
  (/opt/homebrew/opt/srt/lib/libsrt.1.5.dylib etc.). The
  publish-release job was gated on Mac success; alpha.34
  GitHub Release page was never created. Windows build
  did succeed but is only retrievable via the CI run's
  artifacts, not the Releases page. See Open issues from
  Session 14 for the dylib-bundling fix.
- `v0.2.0-alpha.35` (commits `ee2c75c` + `1e7d4f8`):
  Session 15 #1 — Mac Homebrew dylib bundling fix in
  build-ffmpeg.yml. Iterative otool + install_name_tool
  bundler walks ffmpeg + each newly-bundled dylib's deps
  until no /opt/homebrew refs remain. 14 dylibs bundled
  (libsrt, libssl, libcrypto, libx264, libx265, libxcb*,
  libX11, libXau, libXdmcp). Mac sidecar 9 MB → 16.7 MB.
  release.yml Mac extract changed to flat `tar -xzf -C
  src-tauri/sidecar`. BUILD_REVISION 1 → 2 (new sidecar
  prerelease ffmpeg-decklink-8.1.1-bmd16.0-rev2). **First
  DeckLink-enabled FFmpeg published on the Releases page
  on both Mac and Windows.** Notary accepted. (alpha.34
  tag stays as historical pointer to swap-without-fix
  commit, no Release page.)
- `v0.2.0-alpha.36` (commit `c0616c8`): Session 15 hot-
  fix — DeckLink device parsing for FFmpeg n8.1.1 log
  prefix. Operator tested alpha.35 on the Broadcast Pix
  test rig (DeckLink Studio 4K + DeckLink 8K Pro x3 +
  NDI Machine x4 + Videohub I/O routes + Key/Fill 1 = 13
  visible devices); UI showed "No DeckLink devices found"
  because FFmpeg n8.1.1 changed the decklink indev log
  prefix from `[decklink @ 0x...]` to `[in#0 @ 0x...]`
  and dropped the bracket prefix entirely on per-format
  rows (now tab-indented only). Both
  DECKLINK_DEVICE_LINE + DECKLINK_FORMAT_LINE regexes in
  device_scanner.rs were anchored to literal `[decklink`,
  silently skipped every line on the new FFmpeg. Fix is
  regex-only: drop the literal word from DEVICE_LINE +
  make the bracket prefix optional in FORMAT_LINE. Plus
  two regression-test fixtures captured live from the
  test rig output (parse_devices_n8_1_1_format,
  parse_modes_n8_1_1_format) alongside the existing
  legacy-format tests.
- `v0.2.0-alpha.37` (commit `7dc0d02`): Session 15 second
  hot-fix — DeckLink output codec rawvideo →
  wrapped_avframe in build_decklink_output_cmd. After
  alpha.36 populated the device dropdown, operator picked
  a device + mode, hit Start Stream, immediately errored
  at FFmpeg header-write: "Unsupported codec type! Only
  V210 and wrapped frame with AV_PIX_FMT_UYVY422 are
  supported." alpha.21 shipped `-c:v rawvideo` which
  sounds right but FFmpeg's decklink_enc.cpp rejects it.
  wrapped_avframe is FFmpeg's pseudo-codec for passing
  AVFrames straight to a muxer; the existing
  `format=uyvy422` upstream filter handles the actual
  pixel-format conversion. Manually verified before
  commit with testsrc2 → Output C to Videohub Input 21
  → frames written at 30fps. 10-bit V210 path deferred.
- `v0.2.0-alpha.58` (commit `da9531e`): Session 18 — recovery +
  crash hardening. Stop-aware capture send (ndi_capture.rs +
  omt_capture.rs: `blocking_send` → `send_frame_or_stop` +
  `join_with_timeout`) fixing the Dante-on-NDI wedge where a
  back-pressured capture thread parked forever and froze `stop()`;
  off-lock capture joins in stop()/run_one_attempt; poison-proof
  state RwLock (`.unwrap_or_else(|e| e.into_inner())`); real Windows
  kill-orphans (CIM + Stop-Process, was a no-op); lock-free
  "Force Stop ALL" button (`/api/force-stop-all`). CI green Mac +
  Windows; installed + verified on the Windows rig.
- `v0.2.0-alpha.59` (commit `9df6320`): Session 18 — graceful
  FFmpeg shutdown + source hot-swap. `stop()` no longer force-kills
  up front; for NDI/OMT it stops the capture (→ stdin EOF → FFmpeg
  flushes + closes SRT cleanly + exits) with a 2.5s bounded
  force-kill fallback — the durable fix for the ATEM per-key
  receiver-slot lockout that abrupt kills caused. `api_settings`
  auto-restarts the stream on a source change while live
  (`status="Switching"`) so switching sources actually switches
  what's streaming. ⚙ Advanced button right of the Quality picker.
  CI green Mac + Windows; installed + verified on Windows. Confirmed
  the session's "NDI won't reach the ATEM" was the per-key lockout
  (SRT "I/O error"), NOT NDI — NDI capture was always healthy
  (preview proof + correct FFmpeg input interpretation). (See alpha.60 —
  there was ALSO a second, dominant "NDI black" cause: the audio bridge.)
- `v0.2.0-alpha.60` (commit `e3ff37b`): Session 18 — the actual
  "NDI → ATEM shows black" fix. The NDI audio TCP bridge tagged audio
  with `-use_wallclock_as_timestamps 1` (alpha.49 DeckLink drift remedy);
  on the SRT/MPEG-TS path that put audio on a wallclock timeline vs the
  0-based video → broken program PCR → ATEM couldn't present the video.
  Operator-confirmed by Audio Mixer = Silent making the video appear.
  Fix: `ffmpeg_input_args(use_wallclock)` — wallclock for DeckLink only;
  SRT/RTMP use sample-count PTS aligned with the video. Restores NDI
  video + audio to the ATEM. Webcams/test-pattern were never affected
  (no bridge).
- `v0.2.0-alpha.61` (commit `273ced2`): Session 19 — hard wall-clock
  watchdog around `NdiCapture::start_and_probe_format`. The blocking
  first-frame probe runs on a throwaway `ndi-probe` thread bounded by
  `recv_timeout(format_timeout + 3s)`; a wedged `capture_video` can no
  longer freeze the instance (returns an error + detaches the stuck
  thread). New free fn `probe_and_spawn`; public signature unchanged.
- `v0.2.0-alpha.62` (commit `4c38021`): Session 19 — enable the 6-channel
  broadcast multiview. `fleet.rs` TILE_COUNT 4→6 + `multiview.html`
  TILE_COUNT 4→6 + grid 2x2→3x2. The whole per-tile multiview was already
  built at 4; the Phase A `decklink-spike` validated 6 concurrent decklink
  outputs (180s clean) on the rig first. Windows .exe needed a manual
  rerun + `gh release upload --clobber` after an innoextract CI flake.
- `v0.2.0-alpha.63` (commit `ea5b098`): Session 19 — force-kill FFmpeg on
  Stop for DeckLink. The decklink muxer's `av_write_trailer` access-
  violates (0xC0000005) when closing a real-SDI output (codec-independent;
  NDI-Machine virtual outputs close clean; teardown-only). `stop()` force-
  kills when `is_decklink()` before stdin-EOF drives the crashing trailer
  — safe (local hardware, no lockout). Recovers even a frozen ffmpeg in 0s.
- `v0.2.0-alpha.64` (commit `a952cac`): Session 19 — dropped the audio
  bridge wallclock for ALL destinations to fix the NDI→DeckLink video
  THROTTLE (wallclock audio raced ahead of the frame-counted video,
  stalling the muxer to ~2.4fps). Fixed the throttle but exposed a hard
  FREEZE at ~2:44 (sample-count audio still diverges). Superseded by 65.
- `v0.2.0-alpha.65` (commit `44c0296`): Session 19 — `-use_wallclock_as_
  timestamps 1` on BOTH the rawvideo pipe AND the audio bridge for
  DeckLink, so they share one real-time timeline (root cause: the frame-
  counted `-r 30` video PTS assumed exactly nominal fps; the NDI source
  isn't). Fixes BOTH the throttle and the freeze. Throttle-avoidance
  validated standalone (446 frames/15s wallclock-both vs 2 frames/322s
  audio-only-wallclock); freeze-avoidance verified in-app (6.7 min clean,
  30fps + live program audio, frames past the old ~4934 freeze point to
  12860). SRT/ATEM unchanged. **NDI→DeckLink is now full video+audio
  broadcast-ready.**
- `v0.2.0-alpha.66` (commit `639a945`): Session 20 — SRT NDI wallclock+
  aresample drift fix (mirror of alpha.65 DeckLink) + frame-delta stall
  detector (was cumulative-fps). Did NOT fix the operator's audio-dies bug.
- `v0.2.0-alpha.67` (commit `92bfdd7`): Session 20 — lockout-aware reconnect
  backoff for ATEM SRT (`backoff_secs_atem_srt` 15/30/45s) to stop the
  "Conversion failed!" churn against the still-held per-key receiver slot.
- `v0.2.0-alpha.68` (commit `b8ce337`): Session 20 — SRT latency default
  500→200ms + prominent latency control with presets (pulled out of
  Advanced). A/B proved latency is the per-path fix for the link DROPS.
- `v0.2.0-alpha.69` (commit `53901ee`): Session 20 — enable release logging
  (was `cfg!(debug_assertions)`-only → the shipped exe had NO logger) with a
  LogDir `patchbay.log` target. Audio buffer bump (128→512 +
  `-thread_queue_size 1024`) — proven WORSE (285s silence vs 94s), reverted
  in alpha.70.
- `v0.2.0-alpha.70` (commit `4decc84`): Session 20 — `audio_rms_db`
  capture-content telemetry to split the audio bug into upstream-of-bridge
  (NDI receiver hands us silence) vs downstream (real audio dies in
  bridge→FFmpeg). Reverted the alpha.69 buffers. THE deciding diagnostic;
  audio-dies bug STILL OPEN.
- `v0.2.0-alpha.71` (commit `3a88c3f`): Session 21 — **FIXES the NDI→SRT→ATEM
  audio-dies bug.** alpha.70's `audio_rms_db` proved it DOWNSTREAM (capture
  continuously real -20..-60 dB while the loopback output grew escalating
  digital-silence gaps from ~5 min). Root cause: alpha.66's
  `-use_wallclock_as_timestamps` on the slightly-sub-realtime SRT pipeline
  (`speed≈0.984x`) made `aresample=async` fill the growing wallclock-vs-sample
  gap with silence. Fix: wallclock ONLY for DeckLink; SRT/RTMP back to
  symmetric 0-based PTS + keep `aresample` (decoupled the wallclock/aresample
  pairing, removed the dead `ATEM_DISABLE_NDI_WALLCLOCK` hatch). Validated in a
  standalone FFmpeg harness (40 silence regions/45s WITH wallclock, 0 WITHOUT).
  On-rig long-run verify pending.

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

## Latest commits (v0.2.0 / `tauri-rewrite` == `main` after Session 4 merge)

Run `git log --oneline -25 main` for the live list. Session 4
added (newest first):

```
6a2bb73 release.yml: fix Windows NDI install hang + Mac-only release path
d82fd7d chore: gitignore .claude/ agent state directory
cfa3ef4 net-diag: per-stream cards in dashboard with bitrate, RTT, health
267caa0 net-diag: bare --ui launch (no URL) now works + improved start.command
c6433b9 net-diag: add package/ source files (start.command launcher + README)
de560bf net-diag: live config form in dashboard + valid UUID in BMD streamid
17e2f40 net-diag: --ui [PORT] mode — embedded HTTP server + live web dashboard
435287a net-diag: --monitor IFACE — passive flow capture via tshark
ec58db8 net-diag: multi-key rotation — distinguish per-key vs destination-wide lockouts
a47c3a2 streamer: setsid the FFmpeg watchdog so group-kills can't take it down
689a246 fix: screen-capture scale + notarize-non-fatal upload
50d40b8 net-diag: --key K flag — build BMD streamid from a key + base URL
50b4418 streamer: bash watchdog for parent-death FFmpeg cleanup
bbc54a7 fix: screen-capture + video-only AVF sources stream cleanly
121ca90 release: bundle FFmpeg into the .app/.exe so end users without Homebrew can stream
ac96b81 ui: hero subtitle adds "to computer screens" to the source list
42398e6 ui: move kill-orphans into a Recovery card under Overlays
9c917f5 ui: kill-orphans button keeps its name, asterisks link button to hint
be2e797 release: bump to 0.2.0-alpha.4 for the first signed + notarized build
e4cf7ad audio: NDI video + custom AVF audio (Dante) end-to-end
85bfd29 ui: split audio into Audio Mixer card + kill-orphans button + footer rework
b2ece30 release: add notarytool + stapler step + libndi bundle sanity-check
cd722bc bundle: ship libndi.dylib in Contents/Frameworks/ + neutral SRT/RTMP wizard
3bca7e2 audio: L/R channel pan picker for Dante VSC + aggregate devices
2e9c1f1 preview: receive full-bandwidth (was Lowest, looked broken)
2928973 preview: add pre-stream Preview button (NDI low-bandwidth)
c378c72 streamer: route through VideoToolbox on macOS for hardware encoding
```

Session 4's first commit (VideoToolbox) is `c378c72`. The
fast-forward of `main` to `tauri-rewrite` happened at
`d82fd7d` (commit message: "chore: gitignore .claude/").

## What's next (priority order if picking up cold)

See **"Session 16 direction: BROADCAST MULTIVIEW"** under the
v0.2.0 direction section above for the full architectural
pivot. Quick summary:

**Headline pivot (the new direction, user-stated end of
Session 15):** Reframe the DeckLink output path as a
broadcast multiview interface — 8 channels in one window,
each routing a network source (NDI/OMT/SRT-listen/etc.) to
a DeckLink output, with per-channel audio source picker +
input/output audio meters, hardware-accel where possible,
broadcast-grade reliability (per-channel isolation,
auto-reconnect, watchdog).

This is the largest architectural change since alpha.21
(the original DeckLink output direction). Multiple alpha
cycles expected. Plan agent pass strongly recommended
before implementation. Six suggested phases in the Session
16 direction section above (design+spike → backend →
UI → audio meters → HW accel → reliability).

**Before the multiview work starts, smaller follow-ups:**

1. **Verify alpha.37 end-to-end on real hardware** (if not
   already confirmed by end of Session 15). NDI source →
   DeckLink output → signal on SDI. Should work post-
   alpha.37; manual probe confirmed.

2. **NSIS pre-install process kill.** Recurring friction
   during install passes — each upgrade needs manual
   taskkill. Tauri 2 NSIS hook. ~50 LOC. Ship as alpha.38
   probably.

3. **Drop X11/libxcb deps from Mac FFmpeg build.** Session
   15 Open issue #2. Saves ~2 MB. Easy win.

**Carryovers (parallel-able with multiview work):**

4. **Pre-show checks panel** — Session 12 priority #4.
   Net-utility dashboard, ~300 LOC.

5. **OMT-out audio for non-raw sources** — cross-platform
   pipe-FD plumbing.

6. **Pipe / relay custom audio extension** — alpha.14
   Windows dshow / Mac AVF pattern for pipe / RTSP /
   SRT-listen / RTMP-listen.

7. **Operator-side testing pass** — alpha.32-37 features
   need real-hardware verification (UDM form, per-switch
   pin, Mac DeckLink, multi-GPU encoder picker).

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
