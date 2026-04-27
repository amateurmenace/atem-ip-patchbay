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

### Session 6 priorities (next pickup)

User-stated priorities, in roughly intended order. The first
one HAS to happen on the production-LAN Mac (anywhere with line
of sight to `192.168.20.1`); the others can happen on any dev
machine.

1. **Live UDM + per-key validation on the production-LAN Mac.**
   The Session 5 v0.2.0 atem-net-diag tarball is at
   `~/Library/Mobile Documents/com~apple~CloudDocs/atem-net-diag-0.2.0-macos-arm64.tar.gz`
   (sync to whichever Mac is on `192.168.20.x`). Run with
   `UDM_API_KEY=...` set, hit `/api/state`, and verify:
   - `unifi_status.state == "connected"` (not stuck at
     `connecting` or returning `failed`).
   - `unifi_clients` has the expected number of devices on
     the LAN, including the ATEM at `is_atem: true`.
   - Per-client `tx_kbps` / `rx_kbps` look reasonable
     (idle clients ≈ 0, active clients non-zero).
   - During a real stream from the parent app, the Live
     streams card shows the flow AND the SID parser pulls
     out the BMD `u=KEY` (rendered on the card as `key:
     <KEY>`).
   If UDM polling fails, the most likely fixes are in
   `tools/atem-net-diag/src/unifi.rs`: endpoint path
   (legacy vs integration), response field names (snake_case
   vs camelCase, alias via `#[serde(rename = "...")]`), or
   integration-vs-legacy fallback ordering. Iterate, push
   to a `udm-live-fixes` branch, drop a fresh tarball.

2. **Auto mode in atem-net-diag** (deferred from Session 5
   spec, blocked on #1 above being reliable). Third diag
   mode where probes run only after N seconds of no observed
   flow on the configured key/port — combines the safety of
   Live with the proactive-testing benefit of Standby.
   Needs per-key correlation to be live + reliable so we
   can correctly detect "is flow X actually flowing right
   now?" before deciding to probe.

3. **Multi-source mode in the main app.** Goal: a single
   user pushes 2-4 different sources to 2-4 different ATEM
   inputs simultaneously. Two paths, both should ship:
   - **Multiple instances** (already supported via
     `--instance-name N` from Phase 6). Document in the FAQ:
     "How do I run multiple streams to multiple destinations?"
     with a step-by-step (open the .app multiple times,
     each gets its own port pair + state directory).
   - **In-app multi-source mode**. New toggle near the top
     of the main window. When enabled:
     - Opens a second window with a 2x2 multi-view (4
       boxes, each rendering one source's preview JPEG)
     - Main window grows a prominent "Input 1 / 2 / 3 / 4"
       picker at the top
     - User selects an input, configures destination /
       source / audio / etc. for that input independently
     - Each input has its own EncoderState, Streamer, port
       pair (HTTP + BMD) — basically four parallel instance
       managers inside one process
     - "Start All" / "Stop All" controls in addition to
       per-input start/stop
   - UI placement for the multi-source toggle: **above the
     Recovery card in the left column**, with title
     "Multi-Source", a paragraph explanation of the two
     paths (in-app multi-source vs multiple .app
     instances), the in-app toggle, and an FAQ-style
     expandable for "Can I run several streams at once?".
   Architecture sketch:
   - New module `multi.rs` with `MultiState` holding 4
     `EncoderState` + 4 `Streamer` instances
   - Window 2: opens via Tauri's `WebviewWindow::new`
     pointed at `/static/multiview.html`
   - HTTP API gets prefixed routes: `/api/i1/state`,
     `/api/i2/state`, ... so the multiview page can poll
     all four cheaply
   - `/api/preview` becomes `/api/i1/preview` ... etc.
   - Single FFmpeg per input → 4 FFmpeg processes total
     (M-series Mac with VideoToolbox can sustain this
     comfortably since each encode is ~5% CPU)
   This is significant work — probably its own session.

4. **Test the Windows .exe build on real Windows hardware.**
   alpha.7 release pipeline (already configured) should
   produce a working Windows installer once the NDI silent-
   install fix lands. After that, drive the install through
   real Windows: DirectShow device enumeration, FFmpeg path
   resolver (sidecar/ffmpeg.exe), NDI dylib path
   (Processing.NDI.Lib.x64.dll bundled? sidecar? PATH?),
   Tauri's Windows window chrome.

5. **Custom audio for pipe / relay video sources.** Today
   only AVF + NDI video sources support Custom audio mode.
   Pipe (URL/RTSP) and relay (SRT/RTMP listener) video
   sources still fall back to lavfi anullsrc when the user
   picks Custom + an AVF audio device. The fix is structurally
   the same as the NDI path: detect `audio_mode == "custom"`
   in the source builder, append `-f avfoundation -i :NAME`
   as the audio input, set `combined_av=false`. Each source
   builder needs its own version of the conditional.

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

See **"Session 6 priorities"** under the v0.2.0 direction
section above for the full list. Quick summary:

1. **Live UDM/per-key validation** on the production-LAN Mac
   — run the Session 5 v0.2.0 atem-net-diag tarball
   (`~/Library/Mobile Documents/com~apple~CloudDocs/atem-net-diag-0.2.0-macos-arm64.tar.gz`),
   verify UDM polling returns connected clients with the
   ATEM highlighted, verify SID parser extracts the BMD
   `u=KEY` from a real handshake. Iterate `unifi.rs` if
   endpoint paths/field names need adjustment.
2. **Auto mode** in atem-net-diag (third diag mode — probes
   only after N seconds of no flow on the configured key).
   Blocked on #1 being reliable.
3. **Multi-source mode** in main app — both
   `--instance-name`-via-multiple-launches AND in-app 2x2
   multi-view + per-input config picker.
4. **Test Windows .exe** on real Windows hardware (alpha.7
   tag once the NDI installer fix is verified by CI).
5. **Custom audio for pipe / relay** video sources.

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
