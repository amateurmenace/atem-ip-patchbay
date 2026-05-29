//! FFmpeg subprocess + telemetry pump.
//!
//! The encoder profile here matches what real Blackmagic encoders emit
//! (H.264/H.265 Main, no B-frames, fixed GOP, 48 kHz AAC stereo,
//! MPEG-TS over SRT) because BMD decoders (Web Presenter HD/4K, ATEM
//! Streaming Bridge) reject streams that deviate. See [streamer.py:1]
//! in v0.1.0 for the original derivation from the BMD pcap analysis.
//!
//! Phase 3 ports the v0.1.0 streamer wholesale minus the drawtext/logo
//! overlay support — the filter chain is just scale + format
//! conversion. Overlay re-add lands with the v0.2.0 UI bundle
//! (Phase 8) when overlay UI gets reworked anyway.

use crate::ffmpeg_path::{ffmpeg_path, hide_console_tokio};
use crate::ndi_capture::{NdiAudioChunk, NdiCapture, NdiVideoFormat};
use crate::ndi_runtime;
use crate::omt_capture::{OmtCapture, OmtVideoFormat};
use crate::omt_runtime;
use crate::preview::Preview;
use crate::sources::Source;
use crate::state::{EncoderState, Snapshot};
use crate::streamid::{build_srt_url, parse_srt_host_port, SrtUrlParams};

use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Mutex};

const LOG_TAIL_CAPACITY: usize = 500;

#[derive(Debug, Clone)]
pub struct StreamPlan {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_bitrate: u64,
    pub audio_bitrate: u64,
    pub keyframe_seconds: u32,
    pub output_url: String,
    pub protocol: String,
    pub source: Source,
    pub video_codec: String,
    /// Optional `-vf` filter expression applied to the mapped video
    /// stream just before the encoder. Set by source-specific builders
    /// when the input dimensions don't match the configured video_mode
    /// (e.g. NDI from a 720p iPhone fed into a 1080p ATEM target).
    /// Left None for native-resolution paths to preserve the v0.1.0
    /// plain-mapping behavior the bug-fix bundle restored.
    pub video_filter: Option<String>,
    /// Optional `-af` filter expression applied to the mapped audio
    /// stream. Set in two cases (and chained when both apply):
    /// - Multi-channel AVF audio device (Dante VSC, CoreAudio
    ///   aggregate) -> `pan=stereo|c0=cN|c1=cM` so the user picks
    ///   which channel pair goes to L/R.
    /// - audio_mode == "silent" -> `volume=0` so the encoded AAC
    ///   stream is muted regardless of source.
    pub audio_filter: Option<String>,
    /// True when the user has set Audio Mixer -> Mono. Drives the
    /// `-ac 1` arg on the AAC encoder so the output is a single
    /// summed channel instead of stereo.
    pub audio_output_mono: bool,
    /// Session 12 DeckLink-output fields. Populated only when
    /// `protocol == "decklink"`; otherwise empty strings.
    /// - `decklink_device_name` matches FFmpeg's `-list_devices` output
    ///   exactly and is passed as `-i <name>` after `-f decklink`.
    /// - `decklink_format_code` is FFmpeg's per-card mode identifier
    ///   (e.g. "Hp59" for 1080p59.94), passed as `-format_code <code>`.
    /// - `decklink_pixel_format` is the requested output pixel format
    ///   (typically "uyvy422").
    pub decklink_device_name: String,
    pub decklink_format_code: String,
    pub decklink_pixel_format: String,
    /// alpha.25: encoder choice — "auto" lets select_encoder pick the
    /// best for platform + GPU + codec; specific names route directly.
    /// Ignored on the DeckLink branch (raw output, no encoder).
    pub video_encoder: String,
    /// alpha.25: raw FFmpeg flags appended after the encoder block but
    /// before the muxer block. Parsed via shlex at use site.
    pub encoder_extra_flags: String,
    /// alpha.25: audio codec ("aac" = AAC-LC, "aac_he", "aac_he_v2").
    /// build_plan rejects HE on ATEM destinations.
    pub audio_codec: String,
    /// alpha.25: explicit audio bitrate in kbps. 0 = inherit from
    /// active_config.audio_bitrate (XML-driven default).
    pub audio_bitrate_kbps: u32,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
}

impl StreamPlan {
    pub fn gop(&self) -> u32 {
        (self.keyframe_seconds * self.fps).max(1)
    }
}

pub struct Streamer {
    state: Arc<EncoderState>,
    /// Pre-stream preview manager. start() tears any active preview
    /// down before claiming the SDK / device handle for the streaming
    /// receiver — see the handoff rationale in [`crate::preview`].
    preview: Arc<Preview>,
    inner: Mutex<Inner>,
}

struct Inner {
    child: Option<Child>,
    last_command: Vec<String>,
    last_log_lines: VecDeque<String>,
    stop_requested: bool,
    /// Held when source_id == "ndi"; dropped on stop() so the
    /// receiver thread exits and FFmpeg's stdin pipe closes cleanly.
    ndi_capture: Option<NdiCapture>,
    /// Same role as ndi_capture but for source_id == "omt". Always
    /// None when the `omt` cargo feature is off (start_and_probe_format
    /// errors out before the field gets populated).
    omt_capture: Option<OmtCapture>,
    /// Active OMT sender when omt_output_enabled is on. alpha.9 only
    /// populates this for source_id == "ndi" (frame-tee at the writer
    /// task); other source types leave it None and the OMT output
    /// silently doesn't fire. Drop sends OMT_SendDestroy so the
    /// network announcement disappears.
    omt_sender: Option<Arc<crate::omt_sender::OmtSender>>,
    /// Secondary FFmpeg subprocess for OMT-out video tee on non-raw
    /// sources (AVF / pipe / RTSP / SRT-listen / RTMP-listen).
    /// alpha.12 spawns this alongside the primary ATEM FFmpeg so the
    /// same source feeds both — the primary encodes to mpegts/SRT,
    /// the tee outputs BGRA rawvideo we read for OmtSender. None
    /// when source is NDI/OMT (which use the in-process frame-tee
    /// path) or when OMT output is disabled. Kill-on-drop so a panic
    /// can't leak an FFmpeg process.
    omt_video_tee_child: Option<Child>,
    /// Cancellation signal for the auto-reconnect supervisor's
    /// backoff sleep (alpha.30). When the operator clicks Stop mid-
    /// backoff, stop() sets stop_requested + notifies this; the
    /// supervisor's `backoff_with_cancel` select! wakes immediately
    /// rather than waiting up to 60s for the current backoff tick to
    /// elapse. The Notify is level-cheap: re-armed each call to
    /// `notified()`, so repeated start/stop cycles don't accumulate
    /// state.
    supervisor_cancel: Arc<tokio::sync::Notify>,
}

/// Outcome of a single FFmpeg-streaming attempt. The supervisor uses
/// this to decide whether to reconnect (UnexpectedExit) or wind down
/// cleanly (UserStopped).
#[derive(Debug)]
enum AttemptOutcome {
    /// stop_requested was set when the attempt ended — either before
    /// the FFmpeg child exited (user clicked Stop) or because
    /// run_one_attempt itself observed the flag flip. Supervisor
    /// returns without reconnecting.
    UserStopped,
    /// FFmpeg exited without a Stop request. Supervisor consults
    /// `auto_reconnect` + attempt counter to decide whether to retry.
    UnexpectedExit { exit_code: Option<i32> },
}

impl Streamer {
    pub fn new(state: Arc<EncoderState>, preview: Arc<Preview>) -> Arc<Self> {
        Arc::new(Self {
            state,
            preview,
            inner: Mutex::new(Inner {
                child: None,
                last_command: Vec::new(),
                last_log_lines: VecDeque::with_capacity(LOG_TAIL_CAPACITY),
                stop_requested: false,
                ndi_capture: None,
                omt_capture: None,
                omt_sender: None,
                omt_video_tee_child: None,
                supervisor_cancel: Arc::new(tokio::sync::Notify::new()),
            }),
        })
    }

    pub async fn is_running(&self) -> bool {
        let inner = self.inner.lock().await;
        match inner.child.as_ref() {
            Some(_) => true, // We track Child until the monitor sees EOF + wait().
            None => false,
        }
    }

    pub async fn last_command(&self) -> String {
        let inner = self.inner.lock().await;
        if inner.last_command.is_empty() {
            String::new()
        } else {
            shlex_join(&inner.last_command)
        }
    }

    /// Latest JPEG preview frame from the active NDI capture, if
    /// any. Returns None when no NDI source is active or when no
    /// frame has been captured yet. Caller should re-poll at ~2 Hz
    /// to drive the preview <img> tag.
    pub async fn current_ndi_preview(&self) -> Option<Vec<u8>> {
        let inner = self.inner.lock().await;
        inner.ndi_capture.as_ref().and_then(|c| c.latest_preview())
    }

    /// Latest JPEG preview from the active OMT capture, if any. Same
    /// contract as `current_ndi_preview`. Returns None when:
    /// - No OMT source active (most cases — OMT is feature-gated)
    /// - The active OMT capture hasn't encoded a preview yet
    /// - The `omt` cargo feature is off (`OmtCapture::latest_preview`
    ///   returns None unconditionally in that case)
    pub async fn current_omt_preview(&self) -> Option<Vec<u8>> {
        let inner = self.inner.lock().await;
        inner.omt_capture.as_ref().and_then(|c| c.latest_preview())
    }

    pub async fn last_log_tail(&self, lines: usize) -> Vec<String> {
        let inner = self.inner.lock().await;
        let n = inner.last_log_lines.len().min(lines);
        inner
            .last_log_lines
            .iter()
            .skip(inner.last_log_lines.len() - n)
            .cloned()
            .collect()
    }

    pub async fn start(self: &Arc<Self>) -> Result<()> {
        {
            let inner = self.inner.lock().await;
            if inner.child.is_some() {
                return Err(anyhow!("Stream already running."));
            }
        }

        // First attempt — its setup errors propagate to the HTTP
        // caller so a missing NDI source / invalid config surfaces
        // immediately. Subsequent reconnect attempts (driven by the
        // supervisor) treat setup errors as failed-attempts that
        // count toward the auto_reconnect_max_attempts ceiling.
        let stderr = self.spawn_attempt().await?;

        // Reset reconnect-supervisor counters for this fresh
        // operator-initiated session. The supervisor task itself
        // ticks reconnect_attempt + reconnect_next_secs during
        // backoff and increments total_reconnects_this_session on
        // each unexpected exit.
        self.state.stats_in_place(|s| {
            s.reconnect_attempt = 0;
            s.reconnect_next_secs = 0;
            s.total_reconnects_this_session = 0;
        });

        // Spawn the supervisor task. It owns the lifecycle from
        // here — running attempts, deciding whether to reconnect
        // after unexpected exits, and surfacing per-attempt status
        // to the UI. Replaces the old run_monitor-only spawn.
        let me = self.clone();
        tokio::spawn(async move {
            me.run_supervisor(stderr).await;
        });

        Ok(())
    }

    /// Spawn a single FFmpeg attempt. Handles all the setup work
    /// (preview teardown, plan build, NDI/OMT capture, OMT sender +
    /// video tee, FFmpeg child + watchdog, stdin wiring, stats
    /// reset, Inner storage). Returns the stderr handle so the
    /// caller (start on first invocation, the supervisor's reconnect
    /// loop on subsequent ones) can pipe it into run_one_attempt.
    ///
    /// Errors here are setup-time failures: missing NDI source,
    /// FFmpeg binary not found, etc. They propagate to the operator
    /// from start(); from the supervisor, they're counted as failed
    /// reconnect attempts (the supervisor catches the Err and goes
    /// to backoff).
    async fn spawn_attempt(self: &Arc<Self>) -> Result<tokio::process::ChildStderr> {
        // Release any active pre-stream preview before claiming the
        // SDK/device handle for the streaming receiver. NDI: avoids
        // holding two Receivers per source from the same process
        // (wasteful + would race the latest_jpeg slot). The
        // _for_streamer variant keeps the last JPEG visible so the
        // UI doesn't flicker during the preview->stream handoff —
        // the streaming path's sampler will overwrite the slot
        // within a frame or two.
        self.preview.stop_for_streamer().await;

        let plan = self.build_plan()?;
        if !plan.source.available {
            return Err(anyhow!(
                "Source '{}' is not available. {}",
                plan.source.label,
                plan.source.notes
            ));
        }

        // NDI / OMT sources need a probe + receiver-thread spin-up
        // before we know the FFmpeg input args. Other source types
        // build the command directly from EncoderState.
        //
        // raw_format carries the dimensions + pix_fmt back so we can
        // start an OmtSender that exactly matches the source feed
        // (alpha.9 OMT output is a frame-tee, so the sender's format
        // is whatever the source produces — no transcode, no resize).
        //
        // alpha.42: when source is NDI and the operator picked Auto
        // (not Custom-AVF/dshow and not Silent), allocate a TCP audio
        // bridge that FFmpeg will connect to as its audio input. The
        // NDI capture loop drains audio chunks and the writer task
        // (spawned below) converts NDI's planar f32 → s16le
        // interleaved and writes to the bridge socket. This replaces
        // the lavfi-anullsrc-as-fallback path that made NDI → DeckLink
        // (and NDI → ATEM with no Custom device) silently lose all
        // audio. See audio_bridge.rs for why TCP loopback is the
        // chosen cross-platform plumbing.
        let snap_for_audio = self.state.snapshot();
        let want_ndi_audio_to_ffmpeg = plan.source.id == "ndi"
            && snap_for_audio.audio_mode != "silent"
            && snap_for_audio.audio_mode != "custom";
        let audio_bridge: Option<crate::audio_bridge::AudioBridge> = if want_ndi_audio_to_ffmpeg {
            match crate::audio_bridge::AudioBridge::start().await {
                Ok(b) => {
                    log::info!(
                        "audio_bridge :{} ready — FFmpeg will receive NDI audio via TCP",
                        b.port()
                    );
                    Some(b)
                }
                Err(e) => {
                    log::warn!(
                        "audio_bridge start failed: {e} — falling back to silent audio"
                    );
                    None
                }
            }
        } else {
            None
        };

        // ndi_audio_rx is the audio channel from NDI capture. Populated
        // when ANY audio consumer is interested — OMT-out (alpha.13) or
        // the TCP audio bridge to FFmpeg (alpha.42).
        let omt_output_enabled = snap_for_audio.omt_output_enabled;
        let audio_wanted = omt_output_enabled || audio_bridge.is_some();
        let (cmd, ndi_capture, omt_capture, frame_rx, raw_format, ndi_audio_rx) =
            match plan.source.id.as_str() {
                "ndi" => {
                    let source_name = plan.source.label.clone();
                    let ndi_source =
                        ndi_runtime::find_source_by_name(&source_name).ok_or_else(|| {
                            anyhow!(
                                "NDI source not found: {source_name:?}. Refresh the discovery list."
                            )
                        })?;
                    let (format, capture, rx, audio_rx) = NdiCapture::start_and_probe_format(
                        ndi_source,
                        Duration::from_secs(5),
                        audio_wanted,
                    )?;
                    let cmd =
                        self.build_ffmpeg_cmd_for_ndi(&plan, &format, audio_bridge.as_ref());
                    let rf = Some((format.width, format.height, format.ffmpeg_pix_fmt));
                    (cmd, Some(capture), None, Some(rx), rf, audio_rx)
                }
                "omt" => {
                    let source_name = plan.source.label.clone();
                    let address =
                        omt_runtime::find_address_by_name(&source_name).ok_or_else(|| {
                            anyhow!(
                                "OMT source not found: {source_name:?}. Refresh the discovery list, \
                                 or rebuild with --features omt if support isn't compiled in."
                            )
                        })?;
                    let (format, capture, rx) =
                        OmtCapture::start_and_probe_format(address, Duration::from_secs(5))?;
                    let cmd = self.build_ffmpeg_cmd_for_omt(&plan, &format);
                    let rf = Some((format.width, format.height, format.ffmpeg_pix_fmt));
                    (cmd, None, Some(capture), Some(rx), rf, None)
                }
                _ => (self.build_ffmpeg_cmd(&plan), None, None, None, None, None),
            };

        // OMT output (frame-tee). When the user has enabled OMT
        // output:
        //   - NDI / OMT source → in-process frame-tee. We already
        //     have raw BGRA bytes flowing through the writer task,
        //     so the OmtSender just consumes them as a free fan-out.
        //   - Other sources (AVF / pipe / RTSP / SRT-listen / RTMP-
        //     listen) → spawn a SECOND FFmpeg subprocess that opens
        //     the same source and outputs BGRA rawvideo on stdout.
        //     We read that and feed OmtSender. alpha.12 is video-
        //     only on this path (audio remains lavfi anullsrc); the
        //     audio tee comes in alpha.13 once cross-platform pipe-
        //     FD plumbing is worked out.
        let snap_for_omt = self.state.snapshot();
        let (omt_sender, omt_video_tee) = if snap_for_omt.omt_output_enabled {
            if let Some((w, h, pix_fmt)) = raw_format {
                match crate::omt_sender::OmtSender::start_for_format(
                    &snap_for_omt.omt_output_name,
                    w,
                    h,
                    pix_fmt,
                ) {
                    Ok(s) => {
                        log::info!(
                            "OMT output enabled (raw frame-tee): publishing as {:?} at {}x{} ({})",
                            snap_for_omt.omt_output_name,
                            w,
                            h,
                            pix_fmt
                        );
                        (Some(Arc::new(s)), None)
                    }
                    Err(err) => {
                        log::warn!("OMT output requested but sender start failed: {err}");
                        (None, None)
                    }
                }
            } else {
                // Non-raw source — spawn the FFmpeg video tee.
                // OmtSender is always BGRA at the configured output
                // dimensions; the tee FFmpeg scales/converts to match.
                match crate::omt_sender::OmtSender::start_for_format(
                    &snap_for_omt.omt_output_name,
                    plan.width,
                    plan.height,
                    "bgra",
                ) {
                    Ok(s) => {
                        let sender = Arc::new(s);
                        match spawn_omt_video_tee(
                            &plan.source.ffmpeg_input_args,
                            plan.width,
                            plan.height,
                            plan.fps,
                            sender.clone(),
                        ) {
                            Ok(child) => {
                                log::info!(
                                    "OMT output enabled (FFmpeg tee): publishing as {:?} at {}x{} (bgra) — \
                                     audio is silent on this path until alpha.13",
                                    snap_for_omt.omt_output_name,
                                    plan.width,
                                    plan.height,
                                );
                                (Some(sender), Some(child))
                            }
                            Err(err) => {
                                log::warn!(
                                    "OMT output requested but tee FFmpeg failed to spawn: {err}"
                                );
                                (None, None)
                            }
                        }
                    }
                    Err(err) => {
                        log::warn!("OMT output requested but sender start failed: {err}");
                        (None, None)
                    }
                }
            }
        } else {
            (None, None)
        };

        log::info!("Launching FFmpeg: {}", shlex_join(&cmd));

        let needs_stdin = ndi_capture.is_some() || omt_capture.is_some();
        let mut command = Command::new(&cmd[0]);
        command
            .args(&cmd[1..])
            .stdin(if needs_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_console_tokio(&mut command);

        // Put FFmpeg in its own process group so it doesn't inherit
        // the Tauri parent's signals AND so we can SIGTERM the whole
        // group from the exit handler if the normal Drop path didn't
        // fire. Otherwise an abrupt parent crash leaves FFmpeg
        // reparented to launchd, streaming to the destination forever
        // — exactly the orphan bug we're fixing.
        #[cfg(unix)]
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                // setpgid(0, 0) makes the child the leader of a new
                // process group with pgid = its own pid.
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = command
            .spawn()
            .map_err(|e| anyhow!("failed to spawn ffmpeg: {e}"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("ffmpeg stderr was not piped"))?;

        // Spawn a tiny bash watchdog that polls our PID every second
        // and SIGTERMs the FFmpeg process group when it sees us go
        // away. Catches the cargo-tauri-dev rebuild SIGKILL path and
        // any other "parent died ungracefully" scenario the in-
        // process exit handlers can't cover. Without this, FFmpeg
        // gets reparented to launchd and keeps streaming forever
        // (the orphan-stream bug). The watchdog also exits cleanly
        // if FFmpeg dies first (normal Stop) so we don't leak one
        // bash process per stream.
        #[cfg(unix)]
        if let Some(ff_pid) = child.id() {
            let parent_pid = std::process::id();
            let cmd = format!(
                "while kill -0 {parent_pid} 2>/dev/null && kill -0 {ff_pid} 2>/dev/null; do sleep 1; done; \
                 if kill -0 {parent_pid} 2>/dev/null; then exit 0; fi; \
                 kill -TERM -{ff_pid} 2>/dev/null; sleep 1; \
                 kill -KILL -{ff_pid} 2>/dev/null"
            );
            let mut watchdog = std::process::Command::new("/bin/bash");
            watchdog
                .arg("-c")
                .arg(&cmd)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            // setsid makes the watchdog a session leader in its own
            // process group, so a group-targeted SIGKILL on us
            // doesn't take it down. Without this, if cargo-tauri-dev
            // ever decides to kill our process group instead of our
            // PID alone, the watchdog dies before it can do its job
            // and we're back to orphan-FFmpeg territory.
            unsafe {
                use std::os::unix::process::CommandExt;
                watchdog.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            match watchdog.spawn() {
                Ok(_) => log::info!(
                    "FFmpeg watchdog armed: parent={parent_pid} -> ffmpeg pgid={ff_pid}"
                ),
                Err(e) => log::warn!(
                    "FFmpeg watchdog spawn failed (parent crash will leave orphans): {e}"
                ),
            }
        }

        // Windows equivalent of the unix watchdog above. Spawns a tiny
        // PowerShell that polls both PIDs every second and force-kills
        // FFmpeg if the parent disappears first. Critical because the
        // multiview rework (alpha.39+) bumps EncoderFleet::TILE_COUNT
        // from 1 to 8 — at 1 channel a parent crash leaks 1 orphan
        // FFmpeg, but at 8 channels it'd leak 8, each holding a
        // DeckLink output the driver won't release until taskkill or
        // device replug. Without this, Windows multiview multiplies
        // the Windows-only orphan-FFmpeg footgun by 8 right when the
        // operator's recovery path gets noisier (8x stale outputs to
        // identify and kill).
        //
        // Why PowerShell instead of cmd.exe taskkill polling: PowerShell
        // has Get-Process with structured -ErrorAction SilentlyContinue
        // suppression. taskkill's equivalent would be parsing the exit
        // code of `tasklist /FI "PID eq N"` which is brittle against
        // localization. PowerShell ships on every supported Windows.
        //
        // Why DETACHED_PROCESS + CREATE_NEW_PROCESS_GROUP:
        //   - DETACHED_PROCESS: the watchdog gets no console, doesn't
        //     show up next to the Tauri window. Implies CREATE_NO_WINDOW
        //     semantics for console processes (per MSDN, CREATE_NO_WINDOW
        //     is ignored when DETACHED_PROCESS is set).
        //   - CREATE_NEW_PROCESS_GROUP: a Ctrl+C / Ctrl+Break sent to
        //     the parent's console group doesn't propagate to the
        //     watchdog. Without this, the watchdog dies before it can
        //     do its job in the most-likely "user-closes-console"
        //     scenario.
        //
        // Why force-kill (no soft-SIGTERM equivalent): Windows console
        // processes don't process WM_CLOSE in any useful way; FFmpeg's
        // graceful-shutdown trigger is `q` on stdin, which the
        // watchdog can't reach. Force-kill is the only reliable option
        // and matches the parent-crashed semantics (graceful shutdown
        // already happened or never will).
        #[cfg(windows)]
        if let Some(ff_pid) = child.id() {
            let parent_pid = std::process::id();
            // PowerShell -Command takes the script as one argument. Curly
            // braces inside format! must be doubled. The script:
            //   1. Spin while BOTH processes are alive.
            //   2. If the parent's gone, force-stop the ffmpeg PID.
            //      (If ffmpeg died first, parent is still alive, so the
            //       second `if` is false and we exit 0.)
            let ps_script = format!(
                "while ((Get-Process -Id {parent_pid} -EA SilentlyContinue) -and \
                       (Get-Process -Id {ff_pid} -EA SilentlyContinue)) {{ \
                    Start-Sleep -Seconds 1 \
                }}; \
                if (-not (Get-Process -Id {parent_pid} -EA SilentlyContinue)) {{ \
                    Stop-Process -Id {ff_pid} -Force -EA SilentlyContinue \
                }}"
            );
            let mut watchdog = std::process::Command::new("powershell");
            watchdog
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-WindowStyle",
                    "Hidden",
                    "-Command",
                    &ps_script,
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());

            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            watchdog.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);

            match watchdog.spawn() {
                Ok(_) => log::info!(
                    "FFmpeg watchdog armed (windows): parent={parent_pid} -> ffmpeg pid={ff_pid}"
                ),
                Err(e) => log::warn!(
                    "FFmpeg watchdog spawn failed (parent crash will leave orphans): {e}"
                ),
            }
        }

        // Wire NDI/OMT -> FFmpeg stdin via a small drainer task. The
        // task body just consumes Vec<u8>s from the channel and
        // writes them to stdin — same shape regardless of which SDK
        // produced the frames, so a single helper handles both. When
        // OMT output is also active, the same task fans each frame
        // out to the OmtSender before writing to stdin (no separate
        // tee thread; the OMT publish is so cheap relative to BGRA
        // memcpy that inline serialization is fine).
        if let Some(rx) = frame_rx {
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("ffmpeg stdin was not piped (raw video source)"))?;
            tokio::spawn(ndi_writer_task(stdin, rx, omt_sender.clone()));
        }

        // OMT audio writer — only spawns when both NDI audio is being
        // captured AND we have an OmtSender to forward to. The NDI
        // capture thread already gates audio drain on the same boolean
        // (omt_output_enabled was passed in at start_and_probe_format)
        // so the channel only exists in the case we want to consume.
        if let Some(audio_rx) = ndi_audio_rx {
            let task_sender = omt_sender.clone();
            let task_bridge = audio_bridge.clone();
            if task_sender.is_some() || task_bridge.is_some() {
                tokio::spawn(ndi_audio_writer_task(audio_rx, task_sender, task_bridge));
            }
        }

        // Reset stats for the new session.
        self.state.stats_in_place(|s| {
            s.status = "Connecting".into();
            s.error = None;
            s.started_at = Some(std::time::Instant::now());
            s.bitrate = 0;
            s.fps = 0.0;
            s.speed = 0.0;
            s.frames_sent = 0;
            s.frames_dropped = 0;
            s.quality = 0.0;
        });

        {
            let mut inner = self.inner.lock().await;
            inner.last_command = cmd;
            inner.last_log_lines.clear();
            inner.stop_requested = false;
            inner.child = Some(child);
            inner.ndi_capture = ndi_capture;
            inner.omt_capture = omt_capture;
            inner.omt_sender = omt_sender;
            inner.omt_video_tee_child = omt_video_tee;
        }

        // Hand stderr back to the caller. start() pipes it directly
        // into the supervisor's first run_one_attempt; subsequent
        // reconnect attempts have the supervisor pipe it in too.
        Ok(stderr)
    }

    pub async fn stop(&self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        inner.stop_requested = true;
        // Wake the auto-reconnect supervisor's backoff sleep so a
        // mid-backoff Stop click takes effect within ~milliseconds
        // rather than waiting up to 60s for the current tick to
        // elapse. notify_waiters is edge-triggered but the
        // supervisor's stop_requested check on the next tick covers
        // any wake-miss race.
        inner.supervisor_cancel.notify_waiters();
        if let Some(mut capture) = inner.ndi_capture.take() {
            // Drop blocks while the receiver thread joins; do it
            // before killing FFmpeg so the rawvideo input gets a
            // clean EOF.
            capture.stop();
        }
        if let Some(mut capture) = inner.omt_capture.take() {
            // Same contract as the NDI capture above — drop sender
            // first so FFmpeg's stdin closes cleanly before we
            // SIGTERM the process group.
            capture.stop();
        }
        // Kill the OMT video tee FFmpeg (if running). Done BEFORE
        // dropping the OmtSender so the tee child's reader task sees
        // its source go away cleanly (stdout EOF → channel close →
        // task exit) rather than panicking on a dropped sender.
        // kill_on_drop on the Child already covers the catastrophic
        // case; this is the graceful path.
        if let Some(mut tee_child) = inner.omt_video_tee_child.take() {
            log::info!("Stopping OMT video tee FFmpeg");
            let _ = tee_child.start_kill();
            // Don't await — let the reaper task in Drop clean up. The
            // primary FFmpeg stop below uses the same fire-and-forget
            // pattern.
        }
        // Drop the OMT publisher last (its lifetime is shorter than
        // the writer task's; dropping while the writer holds an Arc
        // would just defer the actual destroy until the task exits,
        // which is fine — but logging the announcement going away
        // is cleaner here).
        if let Some(sender) = inner.omt_sender.take() {
            log::info!(
                "OMT output stopping: had {} active connection(s)",
                sender.connection_count()
            );
            // Arc drops when the writer task exits next tick.
        }
        if let Some(child) = inner.child.as_mut() {
            // First send SIGTERM to the whole process group so
            // FFmpeg can flush + close the SRT/RTMP connection
            // cleanly. Falls through to SIGKILL via tokio's
            // start_kill() so a stuck FFmpeg is still guaranteed
            // to die.
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                unsafe {
                    // Negative pid = process group; -pid means group
                    // led by `pid`. Best-effort; if it fails (group
                    // already gone), no-op.
                    libc::killpg(pid as i32, libc::SIGTERM);
                }
            }
            let _ = child.start_kill();
        }
        Ok(())
    }

    // ---- internals --------------------------------------------------------

    fn build_plan(&self) -> Result<StreamPlan> {
        let snap = self.state.snapshot();

        // Session 12: bidirectional patchbay. When destination_type
        // is "decklink" the validation set is entirely different —
        // no XML profile, no stream key, no SRT/RTMP URL. We instead
        // need a picked DeckLink device, an FFmpeg build that
        // supports it, and a matching output mode.
        if snap.is_decklink() {
            return self.build_plan_decklink(snap);
        }

        let cfg = snap
            .active_config
            .as_ref()
            .ok_or_else(|| anyhow!("No streaming profile loaded. Add a service XML first."))?;
        if snap.current_url.is_empty() || snap.current_protocol.is_empty() {
            return Err(anyhow!(
                "No server selected. Pick an RTMP or SRT server in the UI."
            ));
        }
        if snap.stream_key.is_empty() {
            return Err(anyhow!("Stream key is empty."));
        }

        let (width, height, fps) = crate::state::video_dimensions(&snap.video_mode);
        let protocol = snap.current_protocol.to_lowercase();

        let output_url = match protocol.as_str() {
            "srt" => {
                let (host, port) = parse_srt_host_port(&snap.current_url, 1935);
                build_srt_url(&SrtUrlParams {
                    host: &host,
                    port,
                    stream_key: &snap.stream_key,
                    device_name: &snap.label,
                    device_uuid: &snap.device_uuid,
                    latency_us: snap.srt_latency_us,
                    passphrase: if snap.passphrase.is_empty() {
                        None
                    } else {
                        Some(&snap.passphrase)
                    },
                    mode: &snap.srt_mode,
                    streamid_override: &snap.streamid_override,
                    listen_port: snap.srt_listen_port,
                    legacy_streamid: snap.streamid_legacy,
                })
            }
            "rtmp" | "rtmps" => build_rtmp_url(&snap.current_url, &snap.stream_key),
            other => return Err(anyhow!("Unsupported protocol: {other:?}")),
        };

        let source = crate::sources::resolve_source(&self.state)
            .map_err(|e| anyhow!("Source resolve failed: {e}"))?;
        let audio_filter = build_audio_filter(&snap);
        let video_filter = build_video_filter(&snap, width, height, fps);

        // alpha.25: audio bitrate from explicit override OR fall back
        // to the active_config (XML profile / BMD-spec default).
        let audio_bitrate = if snap.audio_bitrate_kbps > 0 {
            (snap.audio_bitrate_kbps as u64) * 1000
        } else {
            cfg.audio_bitrate
        };

        // alpha.25: AAC-HE / AAC-HE-v2 not supported by the ATEM SRT
        // decoder. Reject early so the operator sees a clear error
        // instead of a stream that "works" but the ATEM silently
        // drops audio.
        if (snap.audio_codec == "aac_he" || snap.audio_codec == "aac_he_v2")
            && snap.destination_type == "atem"
        {
            return Err(anyhow!(
                "AAC-{} not supported by the ATEM SRT decoder. Use AAC-LC or switch destination.",
                if snap.audio_codec == "aac_he" { "HE" } else { "HEv2" }
            ));
        }

        Ok(StreamPlan {
            width,
            height,
            fps,
            video_bitrate: cfg.bitrate,
            audio_bitrate,
            keyframe_seconds: cfg.keyframe_interval,
            output_url,
            protocol,
            source,
            video_codec: snap.video_codec.to_lowercase(),
            video_filter,
            audio_filter,
            audio_output_mono: snap.audio_channels == 1,
            decklink_device_name: String::new(),
            decklink_format_code: String::new(),
            decklink_pixel_format: String::new(),
            video_encoder: snap.video_encoder.clone(),
            encoder_extra_flags: snap.encoder_extra_flags.clone(),
            audio_codec: snap.audio_codec.clone(),
            audio_bitrate_kbps: snap.audio_bitrate_kbps,
            audio_sample_rate: snap.audio_sample_rate,
            audio_channels: snap.audio_channels,
        })
    }

    /// DeckLink-destination plan builder — invoked from `build_plan`
    /// when `snap.is_decklink()`. The mental model differs from the
    /// ATEM/SRT path: there's no remote endpoint, no stream key, no
    /// encoder choice (DeckLink output is raw video + PCM audio),
    /// and the framing dimensions come from the picked output mode
    /// rather than the user's video_mode dropdown (which represents
    /// SOURCE expected geometry).
    fn build_plan_decklink(&self, snap: crate::state::Snapshot) -> Result<StreamPlan> {
        if !crate::ffmpeg_path::ffmpeg_has_decklink() {
            return Err(anyhow!(
                "This FFmpeg build does not include DeckLink output \
                 (--enable-decklink missing). Reinstall ATEM IP Patchbay."
            ));
        }
        if snap.decklink_device_name.trim().is_empty() {
            return Err(anyhow!(
                "No DeckLink device selected. Pick one in the Destination card."
            ));
        }
        if snap.decklink_format_code.trim().is_empty() {
            return Err(anyhow!(
                "No DeckLink output mode selected. Pick one for the chosen device."
            ));
        }
        // Verify the picked mode is still supported by the picked
        // device — guards against the operator unplugging a card or
        // switching cards after a settings save without re-picking.
        let modes = crate::device_scanner::probe_decklink_modes(&snap.decklink_device_name);
        let mode = modes
            .into_iter()
            .find(|m| m.format_code == snap.decklink_format_code)
            .ok_or_else(|| {
                anyhow!(
                    "DeckLink mode '{}' not supported by device '{}'. \
                     Re-pick a mode from the dropdown.",
                    snap.decklink_format_code,
                    snap.decklink_device_name
                )
            })?;
        // FFmpeg's rounding-up rule for fractional rates (29.97 -> 30,
        // 59.94 -> 60). The actual exact rate is preserved in the
        // format_code; this fps is just for GOP math + filter strings,
        // which don't matter for DeckLink (no encoder).
        let fps = ((mode.fps_num as f32) / (mode.fps_den as f32)).round() as u32;
        let source = crate::sources::resolve_source(&self.state)
            .map_err(|e| anyhow!("Source resolve failed: {e}"))?;
        let audio_filter = build_audio_filter(&snap);
        // DeckLink requires the source to be normalized to the card's
        // output geometry + pixel format. Always emit a filter when
        // the source dimensions don't match, or when we need to force
        // the pixel format conversion. Source dimensions aren't
        // known until the source-specific FFmpeg cmd builder probes
        // them (NDI/OMT do this at start time), so the safest bet is
        // to always include a `scale=W:H,format=uyvy422` filter on
        // the DeckLink branch. The cost on already-matching sources
        // is a no-op pass through scale.
        let video_filter = Some(format!(
            "scale={w}:{h},format={pf}",
            w = mode.width,
            h = mode.height,
            pf = if snap.decklink_pixel_format.is_empty() {
                "uyvy422".to_string()
            } else {
                snap.decklink_pixel_format.clone()
            },
        ));
        Ok(StreamPlan {
            width: mode.width,
            height: mode.height,
            fps,
            // No encode happens for DeckLink — these fields are zero
            // to make any accidental encoder-side use surface as an
            // obvious "uninitialized" symptom.
            video_bitrate: 0,
            audio_bitrate: 0,
            keyframe_seconds: 0,
            output_url: String::new(),
            protocol: "decklink".to_string(),
            source,
            video_codec: String::new(),
            video_filter,
            audio_filter,
            audio_output_mono: snap.audio_channels == 1,
            decklink_device_name: snap.decklink_device_name.clone(),
            decklink_format_code: snap.decklink_format_code.clone(),
            decklink_pixel_format: if snap.decklink_pixel_format.is_empty() {
                "uyvy422".to_string()
            } else {
                snap.decklink_pixel_format.clone()
            },
            // DeckLink path doesn't encode video, but carry the
            // settings so audio knobs still apply (PCM is overridden
            // in build_decklink_output_cmd; these are inert there).
            video_encoder: snap.video_encoder.clone(),
            encoder_extra_flags: String::new(),
            audio_codec: snap.audio_codec.clone(),
            audio_bitrate_kbps: snap.audio_bitrate_kbps,
            audio_sample_rate: snap.audio_sample_rate,
            audio_channels: snap.audio_channels,
        })
    }

    /// Variant of build_ffmpeg_cmd for NDI sources — input is rawvideo
    /// on pipe:0 (frame format determined by the upstream probe).
    /// Audio source depends on Audio Mixer mode: "custom" + a picked
    /// AVF audio device routes through `-f avfoundation -i :name`,
    /// every other case uses lavfi anullsrc (silent — gets muted to
    /// match user intent for the silent path, passes through as
    /// silence-where-audio-would-be for auto).
    fn build_ffmpeg_cmd_for_ndi(
        &self,
        plan: &StreamPlan,
        fmt: &NdiVideoFormat,
        audio_bridge: Option<&crate::audio_bridge::AudioBridge>,
    ) -> Vec<String> {
        let size = format!("{}x{}", fmt.width, fmt.height);
        let fps = fmt.fps().to_string();
        let mut input_args: Vec<String> = vec![
            "-f".into(), "rawvideo".into(),
            "-pix_fmt".into(), fmt.ffmpeg_pix_fmt.into(),
            "-s".into(), size,
            "-r".into(), fps,
            "-i".into(), "pipe:0".into(),
        ];

        let snap = self.state.snapshot();
        let custom_audio_name = if snap.audio_mode == "custom" && !snap.av_audio_name.is_empty() {
            Some(snap.av_audio_name.clone())
        } else {
            None
        };

        // alpha.42 audio-input priority order:
        //   1. Custom AVF/dshow device (Dante VSC / USB mic / etc.)
        //      — operator explicitly picked an external device, route
        //      its raw audio in.
        //   2. NDI audio via TCP bridge — the operator's Auto mode
        //      gets the NDI source's own audio passed through to
        //      FFmpeg. NEW in alpha.42; before this commit, Auto mode
        //      with NDI source produced silence.
        //   3. lavfi anullsrc — operator picked Silent, OR audio
        //      bridge allocation failed, OR we're on a platform
        //      without proper audio device support yet.
        if let Some(audio_name) = custom_audio_name.as_deref() {
            if cfg!(target_os = "macos") {
                // AVFoundation audio-only input. The leading colon in
                // ":<name>" tells avfoundation there's no video for
                // this input, just the named audio device. This is
                // how Dante VSC / a USB interface / a separate mic
                // gets composited onto an NDI video feed.
                log::info!("NDI + custom AVF audio: routing through {audio_name:?}");
                input_args.extend([
                    "-f".into(), "avfoundation".into(),
                    "-i".into(), format!(":{audio_name}"),
                ]);
            } else if cfg!(target_os = "windows") {
                // DirectShow audio-only input. FFmpeg's dshow demuxer
                // accepts an `audio=<DeviceName>` URI for an audio-only
                // device; same role as the AVF `:<name>` form on macOS.
                // Dante Virtual Soundcard for Windows exposes via WDM/
                // Core Audio and shows up in `-f dshow -list_devices`.
                // The pan filter in build_audio_filter() is platform-
                // agnostic — it fires off the device NAME so picking a
                // device whose name contains "dante" or "aggregate" gets
                // the same L/R channel-pair routing as macOS Dante.
                log::info!("NDI + custom dshow audio: routing through {audio_name:?}");
                input_args.extend([
                    "-f".into(), "dshow".into(),
                    "-i".into(), format!("audio={audio_name}"),
                ]);
            } else {
                // Linux — PulseAudio / ALSA wiring lands as a follow-up
                // when there's a real Linux production use case to test
                // against. Silent fallback so the stream still works.
                input_args.extend([
                    "-f".into(), "lavfi".into(),
                    "-i".into(), "anullsrc=channel_layout=stereo:sample_rate=48000".into(),
                ]);
            }
        } else if let Some(bridge) = audio_bridge {
            log::info!(
                "NDI audio -> FFmpeg via TCP bridge on port {}",
                bridge.port()
            );
            input_args.extend(bridge.ffmpeg_input_args());
        } else {
            input_args.extend([
                "-f".into(), "lavfi".into(),
                "-i".into(), "anullsrc=channel_layout=stereo:sample_rate=48000".into(),
            ]);
        }

        // Build a temp Source so the existing build_ffmpeg_cmd
        // pathway works — overwrite the input args with the NDI ones.
        let mut adjusted = plan.clone();
        adjusted.source.ffmpeg_input_args = input_args.split_off(0);
        adjusted.source.combined_av = false; // separate inputs (pipe + audio)

        // Scale to the configured video_mode when the NDI source's
        // native resolution doesn't match. ATEM hardware decoders only
        // accept the resolutions they advertise (a 720p NDI stream
        // sent unchanged into a 1080p ATEM input slot connects but
        // never displays). Lanczos for the upscale path; FFmpeg's
        // default bicubic is fine for downscale but lanczos is barely
        // costlier and avoids a moiré pattern on text. Frame-rate
        // mismatches are absorbed by the encoder's `-r` setting later
        // — no fps filter needed unless we ever see source/target rate
        // ratios that aren't clean integer multiples in practice.
        //
        // alpha.44 fix: when destination is DeckLink, build_plan_decklink
        // ALREADY set video_filter to "scale=W:H,format=uyvy422" because
        // the decklink_enc muxer only accepts UYVY422 frames via
        // wrapped_avframe. If we blindly overwrite the filter here with
        // just "scale=W:H:flags=lanczos", the format=uyvy422 conversion
        // step is LOST and the output is BGRA (NDI default) → header
        // write fails with "Could not write header (incorrect codec
        // parameters ?): I/O error". Compose the filters instead:
        // append the lanczos scale BEFORE the format=uyvy422 step (the
        // order matters — format must run after scale so the
        // intermediate buffer is the target size, not source size). For
        // the ATEM/SRT path the existing filter would be None here, so
        // the plain replacement still wins.
        if fmt.width != plan.width || fmt.height != plan.height {
            let scale = format!("scale={}:{}:flags=lanczos", plan.width, plan.height);
            let new_filter = match adjusted.video_filter.as_deref() {
                Some(existing) if existing.starts_with("scale=") => {
                    // build_plan_decklink's filter has the form
                    // "scale=W:H,format=uyvy422". Replace the leading
                    // scale clause with our lanczos scale, keeping any
                    // suffix (the format=uyvy422 + anything else).
                    if let Some(comma_idx) = existing.find(',') {
                        format!("{}{}", scale, &existing[comma_idx..])
                    } else {
                        scale
                    }
                }
                Some(existing) => format!("{},{}", scale, existing),
                None => scale,
            };
            log::info!(
                "NDI scale required: {}x{} -> {}x{} via filter {new_filter:?}",
                fmt.width,
                fmt.height,
                plan.width,
                plan.height,
            );
            adjusted.video_filter = Some(new_filter);
        }

        self.build_ffmpeg_cmd(&adjusted)
    }

    /// Same shape as `build_ffmpeg_cmd_for_ndi` but for an OMT source.
    /// The two formats are structurally identical (width/height/fps_num/
    /// fps_den/ffmpeg_pix_fmt) — we keep separate methods for clarity
    /// and to leave room for SDK-specific tweaks (e.g. OMT might need
    /// different audio handling once libomt audio integration lands
    /// in alpha.10). Today this is a thin pass-through that builds an
    /// equivalent NDI format and reuses the NDI code path.
    fn build_ffmpeg_cmd_for_omt(&self, plan: &StreamPlan, fmt: &OmtVideoFormat) -> Vec<String> {
        let as_ndi = NdiVideoFormat {
            width: fmt.width,
            height: fmt.height,
            fps_num: fmt.fps_num,
            fps_den: fmt.fps_den,
            ffmpeg_pix_fmt: fmt.ffmpeg_pix_fmt,
        };
        // Log the bridge so the FFmpeg log card surfaces "this OMT
        // source is being treated as a raw rawvideo pipe" — useful
        // when a sender produces an unexpected format and we want to
        // compare against what a known-good NDI sender would have done.
        log::info!(
            "OMT capture flowing through NDI-equivalent FFmpeg builder: \
             {}x{}@{}/{} pix_fmt={}",
            fmt.width, fmt.height, fmt.fps_num, fmt.fps_den, fmt.ffmpeg_pix_fmt,
        );
        // OMT pass-through path doesn't currently wire its own audio
        // bridge — alpha.13's OMT audio capture was for OmtSender feed,
        // not FFmpeg input. So None here; OMT source falls through to
        // the lavfi anullsrc fallback unless the operator picks Custom
        // audio (AVF/dshow). Wiring OMT-source-audio → FFmpeg-input is
        // a follow-up (audio_bridge.rs is source-agnostic; just needs
        // OMT capture to expose its audio rx like NDI does).
        self.build_ffmpeg_cmd_for_ndi(plan, &as_ndi, None)
    }

    /// Build the FFmpeg command for a DeckLink output destination.
    /// Same input + map + filter shape as the ATEM/SRT path; the
    /// difference is the output tail — no encoder (DeckLink takes
    /// raw video), PCM audio (DeckLink doesn't accept AAC), no
    /// transport container (we hand off to the `decklink` muxer
    /// which writes directly to the card).
    fn build_decklink_output_cmd(&self, plan: &StreamPlan) -> Vec<String> {
        let mut cmd: Vec<String> = vec![
            ffmpeg_path(),
            "-hide_banner".into(),
            "-loglevel".into(),
            "info".into(),
        ];
        cmd.extend(plan.source.ffmpeg_input_args.iter().cloned());

        let (v_in, a_in) = if plan.source.combined_av {
            ("0:v:0", "0:a:0")
        } else {
            ("0:v:0", "1:a:0")
        };
        cmd.extend([
            "-map".into(), v_in.into(),
            "-map".into(), a_in.into(),
        ]);
        if let Some(filter) = plan.video_filter.as_deref() {
            cmd.push("-vf".into());
            cmd.push(filter.into());
        }
        if let Some(filter) = plan.audio_filter.as_deref() {
            cmd.push("-af".into());
            cmd.push(filter.into());
        }

        // DeckLink output muxer is picky about its input codec.
        // alpha.21 shipped `-c:v rawvideo` which sounds right (the
        // muxer wants raw frames) but is actually wrong — FFmpeg's
        // decklink_enc.cpp rejects rawvideo at header-write time:
        //
        //   [decklink @ ...] Unsupported codec type!
        //   Only V210 and wrapped frame with AV_PIX_FMT_UYVY422
        //   are supported.
        //   [out#0/decklink @ ...] Could not write header
        //   (incorrect codec parameters ?): I/O error
        //
        // First surfaced live in alpha.36 testing on the Broadcast
        // Pix rig — the device-list parse fix landed but the actual
        // Start Stream errored out at FFmpeg launch. Fix: switch to
        // `wrapped_avframe`, FFmpeg's pseudo-codec that passes raw
        // AVFrames straight to the muxer without re-encoding. The
        // upstream `format=uyvy422` in the video filter (set by
        // build_plan_decklink) handles pixel-format conversion;
        // `wrapped_avframe` just packages the AVFrame for delivery.
        //
        // For 10-bit DeckLink cards, V210 would be the alternative
        // — would need a -c:v v210 path with yuv422p10le pixel
        // format. UI doesn't surface 10-bit yet; ship 8-bit only.
        //
        // PCM audio at 48 kHz, channel count per the audio mixer
        // setting. -format_code (Hp59 == 1080p59.94 etc.) is the
        // per-card mode identifier, passed through from
        // build_plan_decklink which looked it up via
        // probe_decklink_modes against the live card.
        let pix_fmt = if plan.decklink_pixel_format.is_empty() {
            "uyvy422"
        } else {
            plan.decklink_pixel_format.as_str()
        };
        let ac = if plan.audio_output_mono { "1" } else { "2" };
        cmd.extend([
            "-c:v".into(), "wrapped_avframe".into(),
            "-pix_fmt".into(), pix_fmt.into(),
            "-c:a".into(), "pcm_s16le".into(),
            "-ar".into(), "48000".into(),
            "-ac".into(), ac.into(),
            "-f".into(), "decklink".into(),
            "-format_code".into(), plan.decklink_format_code.clone(),
            plan.decklink_device_name.clone(),
        ]);
        cmd
    }

    fn build_ffmpeg_cmd(&self, plan: &StreamPlan) -> Vec<String> {
        // Session 12: DeckLink output destination — raw video + PCM
        // audio to the local card, no encode, no SRT/RTMP push. The
        // input + map + filter portion is identical to the ATEM path
        // (frames arrive the same way regardless of where they're
        // going); only the encoder/container/output tail changes.
        // Branch early to keep the existing ATEM code path readable.
        if plan.protocol == "decklink" {
            return self.build_decklink_output_cmd(plan);
        }

        let gop = plan.gop().to_string();
        let mut cmd: Vec<String> = vec![
            ffmpeg_path(),
            "-hide_banner".into(),
            "-loglevel".into(),
            "info".into(),
        ];
        cmd.extend(plan.source.ffmpeg_input_args.iter().cloned());

        let (v_in, a_in) = if plan.source.combined_av {
            ("0:v:0", "0:a:0")
        } else {
            ("0:v:0", "1:a:0")
        };

        // Plain stream mapping with no filter chain unless a source
        // builder explicitly populated `plan.video_filter` (NDI does
        // this when its native resolution differs from the configured
        // video_mode). The Phase 3 commit always wrapped the input in
        // a scale+pad+format filter "to be safe", but that turned out
        // to break SRT against destinations where v0.1.0's plain map
        // worked — so the default stays plain. Overlays (drawtext /
        // logo) come back in a later Phase 8 commit when the overlay
        // UI is reworked, gated by an "are there any overlays" check
        // and concatenated into the same filter expression.
        cmd.extend([
            "-map".into(),
            v_in.into(),
            "-map".into(),
            a_in.into(),
        ]);
        if let Some(filter) = plan.video_filter.as_deref() {
            cmd.push("-vf".into());
            cmd.push(filter.into());
        }

        // alpha.25: encoder selection lives in select_encoder() — it
        // resolves "auto" to the best available encoder for platform
        // + GPU + codec, then composes the right per-encoder flag bag
        // (videotoolbox needs `-realtime 1 -allow_sw 1 -constant_bit_rate 1`;
        // nvenc needs `-preset p4 -rc cbr`; etc.). Honors the legacy
        // ATEM_DISABLE_VT env override + falls back gracefully when a
        // user-picked encoder isn't in available_encoders().
        cmd.extend(select_encoder(plan));

        // Audio filter — runs BEFORE the audio codec args so the
        // encoder sees the already-downmixed stereo result. Pan
        // filter (Dante VSC / CoreAudio aggregate) + silent volume=0
        // path live here; alpha.25's astats audio-meter chain
        // appends inside build_audio_filter().
        if let Some(filter) = plan.audio_filter.as_deref() {
            cmd.push("-af".into());
            cmd.push(filter.into());
        }
        // Audio codec + bitrate + sample-rate + channels from the
        // alpha.25 audio-knobs section. AAC-LC default; AAC-HE /
        // AAC-HE-v2 supported for low-bitrate destinations
        // (build_plan rejects them on ATEM).
        cmd.extend(build_audio_section(plan));

        // Container + transport.
        match plan.protocol.as_str() {
            "srt" => {
                cmd.extend([
                    "-f".into(), "mpegts".into(),
                    "-mpegts_flags".into(), "+resend_headers".into(),
                    "-flush_packets".into(), "1".into(),
                ]);
            }
            "rtmp" => {
                cmd.extend([
                    "-flvflags".into(), "no_duration_filesize".into(),
                    "-f".into(), "flv".into(),
                ]);
            }
            other => {
                // Should be unreachable — build_plan rejects this.
                log::error!("ffmpeg cmd builder hit unknown protocol {other:?}");
            }
        }

        cmd.push(plan.output_url.clone());
        cmd
    }

    /// Run one streaming attempt: read stderr until EOF, wait for
    /// the child, tear down per-attempt resources (NDI/OMT capture,
    /// OMT sender, video tee). Returns an outcome the supervisor
    /// uses to decide whether to reconnect.
    ///
    /// Unlike alpha.29's `run_monitor`, this does NOT set the final
    /// status (Idle / Interrupted) — that's the supervisor's call,
    /// which knows whether another attempt is queued. For
    /// UnexpectedExit the supervisor either transitions to
    /// "Reconnecting" (during backoff) or to a sticky "Interrupted"
    /// after max attempts.
    async fn run_one_attempt(
        self: Arc<Self>,
        stderr: tokio::process::ChildStderr,
    ) -> AttemptOutcome {
        // alpha.41 stall detector: FFmpeg can be in a "Streaming but
        // no frames out" state where the process is still alive but
        // the encoder is stuck (input source went away mid-stream,
        // DeckLink driver hung, NDI receiver buffer drained etc.).
        // alpha.30's supervisor catches CLEAN exits via wait()'s
        // return value but a hung FFmpeg never exits — so without
        // explicit liveness checking the tile would show "Streaming"
        // forever with a black SDI output and the operator wouldn't
        // know until somebody downstream notices.
        //
        // Detection: poll the encoder's snapshot every second. After
        // 5 consecutive ticks where status="Streaming" but fps is
        // ~0, force-kill the child. The kill causes FFmpeg's stderr
        // pipe to close, the stderr-read loop below sees EOF,
        // run_one_attempt finishes normally and returns
        // UnexpectedExit — the supervisor then triggers an
        // alpha.30 backoff-and-reconnect like any other unexpected
        // exit. So the operator sees "Reconnecting in 2s · 1/12"
        // instead of a stuck-Streaming-forever tile.
        //
        // We only detect during Streaming because Connecting can
        // legitimately take a few seconds (NDI source discovery,
        // SRT handshake). Frozen-during-Connecting is alpha.30's
        // existing handle_unexpected_exit territory (FFmpeg exits
        // with an error → backoff already kicks in).
        let stall_self = self.clone();
        let stall_task = tokio::spawn(async move {
            let mut consec_stalls: u32 = 0;
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // First tick fires immediately — skip it so we get a
            // ~1s warm-up before evaluating.
            tick.tick().await;
            loop {
                tick.tick().await;
                let snap = stall_self.state.snapshot();
                let status_low = snap.stats.status.to_lowercase();
                if status_low != "streaming" {
                    consec_stalls = 0;
                    continue;
                }
                if snap.stats.fps < 0.5 {
                    consec_stalls += 1;
                    if consec_stalls >= 5 {
                        log::warn!(
                            "Stall detector: FFmpeg in Streaming with fps={:.2} for 5s — \
                             killing the child so the supervisor triggers reconnect",
                            snap.stats.fps
                        );
                        stall_self.state.stats_in_place(|s| {
                            s.error = Some(
                                "Stream froze (no frames for 5s) — restarting".into(),
                            );
                        });
                        let mut inner = stall_self.inner.lock().await;
                        if let Some(child) = inner.child.as_mut() {
                            // start_kill sends SIGKILL/TerminateProcess
                            // without waiting; the stderr-loop below
                            // observes EOF on the next read and
                            // exits naturally.
                            let _ = child.start_kill();
                        }
                        return;
                    }
                } else {
                    consec_stalls = 0;
                }
            }
        });

        // FFmpeg's progress (`frame=… fps=… bitrate=…`) terminates each
        // update with a CARRIAGE RETURN, not a newline — a terminal
        // overwrites the prior line in place. `read_until(b'\n')`
        // therefore blocks indefinitely waiting for the next `\n`,
        // which only arrives when the process exits. We need to
        // process bytes as they arrive and split on either `\r` or
        // `\n` so each progress tick becomes its own logical line.
        use tokio::io::AsyncReadExt;
        let mut stderr = stderr;
        let mut chunk = [0u8; 1024];
        let mut acc: Vec<u8> = Vec::with_capacity(2048);
        loop {
            let n = match stderr.read(&mut chunk).await {
                Ok(0) => break, // EOF
                Ok(n) => n,
                Err(err) => {
                    log::warn!("ffmpeg stderr read failed: {err}");
                    break;
                }
            };
            acc.extend_from_slice(&chunk[..n]);
            // Walk the accumulator emitting each complete sub-line
            // (sep = \r or \n). Anything trailing without a sep stays
            // in the buffer for the next read.
            let mut start = 0usize;
            for (i, &b) in acc.iter().enumerate() {
                if b == b'\r' || b == b'\n' {
                    if i > start {
                        let line = String::from_utf8_lossy(&acc[start..i]).to_string();
                        self.handle_log_line(&line).await;
                    }
                    start = i + 1;
                }
            }
            if start > 0 {
                acc.drain(..start);
            }
        }
        // Flush any final partial line (FFmpeg's last progress tick
        // before the process exited may not have a trailing sep).
        if !acc.is_empty() {
            let line = String::from_utf8_lossy(&acc).to_string();
            self.handle_log_line(&line).await;
        }

        // alpha.41: stall detector outlived its purpose now that
        // FFmpeg has exited (either naturally or via the detector
        // itself force-killing the child). Abort the task so it
        // doesn't tick uselessly while the supervisor decides
        // whether to reconnect.
        stall_task.abort();

        // Stderr EOF — child is exiting. Wait for the exit code and
        // clean up per-attempt resources (NDI/OMT capture so the
        // next attempt can re-claim the SDK handle; OMT video tee
        // child; OMT sender). Mirrors stop() but doesn't notify the
        // supervisor's cancel — that's a separate signal.
        let exit_code = {
            let mut inner = self.inner.lock().await;
            let mut child = match inner.child.take() {
                Some(c) => c,
                // Slot already empty — stop() raced us. Treat as user-stop.
                None => return AttemptOutcome::UserStopped,
            };
            // Tear down NDI/OMT capture so the next reconnect attempt
            // can re-probe + re-claim the SDK handle. Per the
            // alpha.30 plan we always rebuild captures rather than
            // try to keep them alive across an FFmpeg exit (the
            // stdin EOF would race the next attempt's stdin-take).
            if let Some(mut capture) = inner.ndi_capture.take() {
                capture.stop();
            }
            if let Some(mut capture) = inner.omt_capture.take() {
                capture.stop();
            }
            if let Some(mut tee) = inner.omt_video_tee_child.take() {
                let _ = tee.start_kill();
            }
            if let Some(sender) = inner.omt_sender.take() {
                log::info!(
                    "OMT output ending with primary FFmpeg ({} connections at exit)",
                    sender.connection_count()
                );
            }
            drop(inner); // don't hold lock across .await
            child.wait().await.ok().and_then(|s| s.code())
        };

        let stop_requested = {
            let inner = self.inner.lock().await;
            inner.stop_requested
        };

        // Snapshot the recent log tail for error context.
        let recent_tail = {
            let inner = self.inner.lock().await;
            inner
                .last_log_lines
                .iter()
                .rev()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ")
        };

        if stop_requested {
            self.state.stats_in_place(|s| {
                s.status = "Idle".into();
                s.started_at = None;
                s.bitrate = 0;
                s.reconnect_attempt = 0;
                s.reconnect_next_secs = 0;
            });
            AttemptOutcome::UserStopped
        } else {
            // Don't set Idle/Interrupted — supervisor decides. But
            // record the error context for the UI to show during
            // reconnect / on the eventual sticky Interrupted state.
            self.state.stats_in_place(|s| {
                s.started_at = None;
                s.bitrate = 0;
                if s.error.is_none() {
                    if let Some(rc) = exit_code {
                        s.error = Some(format!("FFmpeg exited with code {rc}: {recent_tail}"));
                    } else {
                        s.error = Some(format!("FFmpeg exited unexpectedly: {recent_tail}"));
                    }
                }
            });
            AttemptOutcome::UnexpectedExit { exit_code }
        }
    }

    /// Auto-reconnect supervisor. Drives attempts + backoff over
    /// the lifetime of one operator-initiated start session.
    /// Returns when the operator clicks Stop OR auto_reconnect is
    /// disabled and an attempt fails OR
    /// auto_reconnect_max_attempts is exceeded.
    async fn run_supervisor(
        self: Arc<Self>,
        initial_stderr: tokio::process::ChildStderr,
    ) {
        let mut attempt: u32 = 1;
        let mut pending_stderr: Option<tokio::process::ChildStderr> = Some(initial_stderr);

        loop {
            // First iteration: use the stderr handed in by start().
            // Subsequent: spawn a fresh attempt. spawn_attempt setup
            // failures count as failed attempts and go to backoff.
            let stderr = match pending_stderr.take() {
                Some(s) => s,
                None => match self.spawn_attempt().await {
                    Ok(s) => s,
                    Err(e) => {
                        // Setup failure during a reconnect attempt
                        // (NDI source went away, FFmpeg sidecar
                        // gone, etc.). Count it as a failed attempt
                        // and go to backoff. handle_unexpected_exit
                        // returns false on max-exceeded or operator-
                        // cancel, in which case we exit the loop.
                        log::warn!("Reconnect spawn_attempt failed: {e}");
                        self.state.stats_in_place(|s| {
                            s.error = Some(format!("Reconnect setup failed: {e}"));
                        });
                        if !self.handle_unexpected_exit(&mut attempt).await {
                            return;
                        }
                        continue;
                    }
                },
            };

            let started_at = std::time::Instant::now();
            let outcome = self.clone().run_one_attempt(stderr).await;
            let ran = started_at.elapsed();

            match outcome {
                AttemptOutcome::UserStopped => {
                    // run_one_attempt already set status = Idle.
                    return;
                }
                AttemptOutcome::UnexpectedExit { .. } => {
                    // Reset attempt counter if the failing attempt
                    // had a stable run (>60s). Otherwise rapid-fire
                    // failures count toward the max.
                    if ran > std::time::Duration::from_secs(60) {
                        attempt = 1;
                    }
                    if !self.handle_unexpected_exit(&mut attempt).await {
                        return;
                    }
                    // Loop back — pending_stderr is None, so the
                    // next iteration spawns a fresh attempt.
                }
            }
        }
    }

    /// Helper for the supervisor's unexpected-exit branch. Increments
    /// the attempt counter, checks against max_attempts, runs the
    /// backoff. Returns true if the supervisor should loop to spawn
    /// the next attempt; false if it should exit (max exceeded, user
    /// cancelled, or auto_reconnect disabled).
    async fn handle_unexpected_exit(self: &Arc<Self>, attempt: &mut u32) -> bool {
        let snap = self.state.snapshot();
        if !snap.auto_reconnect {
            // Auto-reconnect off — surface the error as sticky
            // Interrupted and exit.
            self.state.stats_in_place(|s| {
                s.status = "Interrupted".into();
                s.reconnect_attempt = 0;
                s.reconnect_next_secs = 0;
            });
            return false;
        }
        *attempt += 1;
        if *attempt > snap.auto_reconnect_max_attempts {
            self.state.stats_in_place(|s| {
                s.status = "Interrupted".into();
                s.error = Some(format!(
                    "Stopped after {} reconnect attempts.",
                    snap.auto_reconnect_max_attempts
                ));
                s.reconnect_attempt = 0;
                s.reconnect_next_secs = 0;
            });
            return false;
        }
        self.state.stats_in_place(|s| {
            s.total_reconnects_this_session = s.total_reconnects_this_session.saturating_add(1);
        });
        let delay_secs = backoff_secs(*attempt);
        if !self
            .backoff_with_cancel(delay_secs, *attempt, snap.auto_reconnect_max_attempts)
            .await
        {
            // Cancelled — user clicked Stop mid-backoff. Clean to Idle.
            self.state.stats_in_place(|s| {
                s.status = "Idle".into();
                s.reconnect_attempt = 0;
                s.reconnect_next_secs = 0;
            });
            return false;
        }
        true
    }

    /// 1-Hz tick that updates the UI's reconnect countdown while
    /// waiting for backoff. Returns true if the full delay elapsed;
    /// false if the operator clicked Stop (Stop's `notify_waiters`
    /// wakes the select arm immediately so we don't wait up to 60s
    /// for a tick to elapse).
    async fn backoff_with_cancel(
        &self,
        delay_secs: u32,
        attempt_num: u32,
        max_attempts: u32,
    ) -> bool {
        let cancel = {
            let inner = self.inner.lock().await;
            inner.supervisor_cancel.clone()
        };
        for remaining in (1..=delay_secs).rev() {
            self.state.stats_in_place(|s| {
                s.status = "Reconnecting".into();
                s.reconnect_attempt = attempt_num;
                s.reconnect_next_secs = remaining;
            });
            // Early bail if stop_requested flipped between iters.
            if self.inner.lock().await.stop_requested {
                return false;
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                _ = cancel.notified() => return false,
            }
        }
        true
    }

    async fn handle_log_line(&self, line: &str) {
        // alpha.31: astats metadata lines are high-volume (40+/sec
        // when meters_enabled). Skip storing them in the LOG_TAIL_-
        // bounded buffer or running them through the rest of the
        // parsing pipeline — they're not interesting for log tail,
        // status detection, or error heuristics. Parse + update the
        // audio level fields, then bail.
        if line.contains("lavfi.astats.") {
            if let Some((channel, metric, value)) = parse_astats_line(line) {
                let now = unix_epoch_secs_f64();
                self.state.stats_in_place(|s| {
                    let updated = match (channel, metric) {
                        (1, AstatsMetric::Rms) => {
                            s.audio_db_rms_l = value;
                            true
                        }
                        (1, AstatsMetric::Peak) => {
                            s.audio_db_peak_l = value;
                            true
                        }
                        (2, AstatsMetric::Rms) => {
                            s.audio_db_rms_r = value;
                            true
                        }
                        (2, AstatsMetric::Peak) => {
                            s.audio_db_peak_r = value;
                            true
                        }
                        _ => false, // ignore Overall + channels 3+
                    };
                    if updated {
                        s.audio_levels_at = now;
                    }
                });
            }
            // Whether or not we matched, the line is astats spam —
            // don't fall through to the log-tail buffer or the
            // error heuristic (which would false-positive on the
            // word "error" in some metric names).
            return;
        }

        {
            let mut inner = self.inner.lock().await;
            if inner.last_log_lines.len() == LOG_TAIL_CAPACITY {
                inner.last_log_lines.pop_front();
            }
            inner.last_log_lines.push_back(line.to_string());
        }

        let lower = line.to_lowercase();

        // Connection-state heuristic — flip to Streaming only on a
        // libsrt "connection established" log. The earlier "stream
        // mapping" trigger was too eager: FFmpeg prints that line
        // BEFORE attempting the SRT handshake, so failed-to-connect
        // outputs would briefly show "Streaming" with zero bitrate
        // until the exit code caught up. Progress-line parsing below
        // also bumps status to Streaming on the first frame=N tick.
        if lower.contains("connection established") {
            self.state.stats_in_place(|s| {
                if s.status != "Streaming" {
                    s.status = "Streaming".into();
                }
            });
        }

        // Per-frame stats line.
        if let Some(parsed) = parse_progress(line) {
            self.state.stats_in_place(|s| {
                if s.status != "Streaming" {
                    s.status = "Streaming".into();
                }
                if let Some(b) = parsed.bitrate {
                    s.bitrate = b;
                }
                if let Some(f) = parsed.frame {
                    s.frames_sent = f;
                }
                if let Some(fps) = parsed.fps {
                    s.fps = fps;
                }
                if let Some(sp) = parsed.speed {
                    s.speed = sp;
                }
                if let Some(d) = parsed.drop {
                    s.frames_dropped = d;
                }
                if let Some(q) = parsed.quality {
                    s.quality = q;
                }
            });
        }

        // Heuristic error detection.
        if ERROR_TAGS.iter().any(|t| lower.contains(t)) {
            self.state.stats_in_place(|s| {
                s.status = "Interrupted".into();
                s.error = Some(line.to_string());
            });
        }
    }
}

/// Substrings in FFmpeg stderr that mean the stream isn't going to
/// recover. When any of these are seen, status flips to Interrupted
/// and the line becomes the visible error in the UI's error banner
/// — without waiting for the process to actually exit.
/// alpha.31: audio meters helper. The kind of dB measurement
/// astats reports on a given log line.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum AstatsMetric {
    Rms,
    Peak,
}

/// Parse a single FFmpeg stderr line for an `lavfi.astats.<channel>.
/// <metric>=<dB>` measurement. Returns (channel_number, metric_kind,
/// value_db) when matched. `channel_number` is 1-indexed (matches
/// FFmpeg's astats output convention). Returns None for the "Overall"
/// pseudo-channel + for metrics we don't render (Min_level, Max_level,
/// DC_offset, etc.).
///
/// Example matched lines (the `[Parsed_ametadata_N @ 0x...]` prefix
/// is tolerated but not required):
///   "[Parsed_ametadata_1 @ 0x7f80] lavfi.astats.1.RMS_level=-23.456"
///   "lavfi.astats.2.Peak_level=-12.345"
fn parse_astats_line(line: &str) -> Option<(u32, AstatsMetric, f32)> {
    let idx = line.find("lavfi.astats.")?;
    let rest = &line[idx + "lavfi.astats.".len()..];
    let dot = rest.find('.')?;
    let channel_str = &rest[..dot];
    // "Overall" is the pseudo-channel that aggregates all real
    // channels; ignore it (UI is per-channel L/R).
    let channel: u32 = channel_str.parse().ok()?;
    let rest = &rest[dot + 1..];
    let eq = rest.find('=')?;
    let metric_str = &rest[..eq];
    let metric_kind = match metric_str {
        "RMS_level" => AstatsMetric::Rms,
        "Peak_level" => AstatsMetric::Peak,
        _ => return None,
    };
    let value_str = rest[eq + 1..].trim();
    let value: f32 = value_str.parse().ok()?;
    Some((channel, metric_kind, value))
}

fn unix_epoch_secs_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Auto-reconnect backoff schedule (alpha.30). `next_attempt` is
/// the attempt number we're about to fire (so 2 = first retry after
/// the initial attempt failed). Caps at 60s. Returns 0 for
/// next_attempt == 1, which the supervisor shouldn't actually call
/// — kept defensive so a future caller can't accidentally produce a
/// pathological wait time.
fn backoff_secs(next_attempt: u32) -> u32 {
    match next_attempt {
        0 | 1 => 0,
        2 => 1,
        3 => 2,
        4 => 4,
        5 => 8,
        6 => 16,
        7 => 32,
        _ => 60,
    }
}

const ERROR_TAGS: &[&str] = &[
    "connection refused",
    "connection setup failure",
    "operation timed out",
    "no route to host",
    "srt error",
    "protocol not found",
    "input/output error",
    "error opening output",
    "could not write header",
    "broken pipe",
    "connection reset",
    "no such file or directory",
];

#[derive(Debug, Default)]
struct Progress {
    bitrate: Option<u64>,
    frame: Option<u64>,
    fps: Option<f32>,
    speed: Option<f32>,
    drop: Option<u64>,
    quality: Option<f32>,
}

static BITRATE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"bitrate=\s*([\d.]+)\s*kbits/s").unwrap());
static FRAME_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"frame=\s*(\d+)").unwrap());
static FPS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"fps=\s*([\d.]+)").unwrap());
static SPEED_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"speed=\s*([\d.]+)x").unwrap());
static DROP_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"drop=\s*(\d+)").unwrap());
static QUAL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\bq=\s*([\d.]+)").unwrap());

fn parse_progress(line: &str) -> Option<Progress> {
    let mut p = Progress::default();
    let mut any = false;
    if let Some(c) = BITRATE_RE.captures(line) {
        if let Ok(kbps) = c[1].parse::<f64>() {
            p.bitrate = Some((kbps * 1000.0) as u64);
            any = true;
        }
    }
    if let Some(c) = FRAME_RE.captures(line) {
        if let Ok(n) = c[1].parse() {
            p.frame = Some(n);
            any = true;
        }
    }
    if let Some(c) = FPS_RE.captures(line) {
        if let Ok(f) = c[1].parse() {
            p.fps = Some(f);
            any = true;
        }
    }
    if let Some(c) = SPEED_RE.captures(line) {
        if let Ok(s) = c[1].parse() {
            p.speed = Some(s);
            any = true;
        }
    }
    if let Some(c) = DROP_RE.captures(line) {
        if let Ok(d) = c[1].parse() {
            p.drop = Some(d);
            any = true;
        }
    }
    if let Some(c) = QUAL_RE.captures(line) {
        if let Ok(q) = c[1].parse() {
            p.quality = Some(q);
            any = true;
        }
    }
    if any {
        Some(p)
    } else {
        None
    }
}

/// Build an FFmpeg `-vf` video-filter expression for sources whose
/// native resolution / frame timing doesn't match the configured
/// output. Today, only the screen-capture path needs it: AVF's
/// "Capture screen N" device ignores `-video_size`, gives the full
/// native display resolution (4K-ish on Retina Macs), and reports
/// 1000k tbr because AVF doesn't propagate a sane frame rate. Both
/// would make the encoded stream unrecognizable to the ATEM —
/// which expects exactly the configured video_mode dimensions.
///
/// The filter scales-and-letterboxes to the target dimensions
/// (preserving aspect ratio with black bars rather than stretching),
/// pins fps to the configured rate so the encoder gets a steady
/// pacing, and sets SAR=1 so the destination interprets the display
/// aspect ratio correctly.
///
/// Returns None for non-screen sources — NDI's resolution match is
/// handled in build_ffmpeg_cmd_for_ndi, AVF cameras already advertise
/// the right modes, pipe / relay deliver whatever the upstream sends
/// and downscaling there is the producer's job.
fn build_video_filter(snap: &Snapshot, width: u32, height: u32, fps: u32) -> Option<String> {
    if snap.source_id != "avfoundation" {
        return None;
    }
    let name = snap.av_video_name.to_lowercase();
    let is_screen = name.contains("capture screen") || name.contains("desk view");
    if !is_screen {
        return None;
    }
    Some(format!(
        "scale={width}:{height}:force_original_aspect_ratio=decrease:flags=lanczos,\
         pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black,\
         fps={fps},setsar=1"
    ))
}

/// Build an FFmpeg `-af` audio-filter expression. Two reasons a
/// filter gets emitted (chained when both apply):
///
/// 1. The active source is an AVF multi-channel audio device
///    (Dante VSC, CoreAudio aggregate) AND the user is in Custom
///    audio mode — emits `pan=stereo|c0=cN|c1=cM` so the picked
///    channel pair goes to L/R instead of FFmpeg auto-downmixing
///    all N channels. Without this, Dante operators get every
///    routed channel summed together which is rarely useful.
///
/// 2. Audio Mixer mode is "silent" — emits `volume=0` so the
///    encoded AAC track is muted regardless of source. We keep an
///    audio track (rather than not encoding one) because BMD
///    decoders expect to see one in the MPEG-TS PMT.
///
/// Returns None when neither condition fires (normal stereo mics,
/// auto mode, etc.) so FFmpeg's default channel handling applies.
fn build_audio_filter(snap: &Snapshot) -> Option<String> {
    let mut chain: Vec<String> = Vec::new();

    // Pan filter applies in Custom audio mode whenever the picked
    // AVF audio device is multi-channel (Dante VSC, CoreAudio
    // aggregate). Independent of which source provides the video —
    // NDI + Dante is the headline use case (camera over NDI, console
    // over Dante, both arriving at this Mac, both routed into one
    // outgoing stream).
    if snap.audio_mode == "custom" {
        let name = snap.av_audio_name.to_lowercase();
        if name.contains("dante") || name.contains("aggregate") {
            // 1-indexed in state (matches user-facing UI), 0-indexed
            // in FFmpeg. max(1) defends against a UI bug submitting 0.
            let l = snap.audio_pan_l.max(1) - 1;
            let r = snap.audio_pan_r.max(1) - 1;
            chain.push(format!("pan=stereo|c0=c{l}|c1=c{r}"));
        }
    }

    if snap.audio_mode == "silent" {
        chain.push("volume=0".into());
    }

    // alpha.49: clock-drift compensation for the DeckLink output
    // path. NDI's source clock (whatever device generated the audio
    // — iPhone, vMix, etc.) and the DeckLink card's hardware clock
    // are different crystals; even 50 ppm of drift accumulates to
    // visible/audible problems over 5-10 minutes (audio falls behind
    // or runs ahead, FFmpeg's a/v sync logic eventually compensates
    // by dropping audio frames entirely → operator sees audio "work
    // for a couple minutes then die"). aresample=async=1000 inserts
    // up to 1000 samples/sec of correction continuously, absorbing
    // the drift without audible artifacts. Paired with the
    // `-use_wallclock_as_timestamps 1` added to the bridge input
    // args (see audio_bridge.rs::ffmpeg_input_args) which gives the
    // resampler the timing signal it needs.
    //
    // Only applied for DeckLink because ATEM/SRT destinations have
    // an encoder + container that handles a/v sync via the encoder's
    // PTS allocator — the resampler is unnecessary and can introduce
    // tiny offsets on those paths.
    if snap.is_decklink() {
        chain.push("aresample=async=1000:first_pts=0".into());
    }

    // alpha.31: audio level meters. astats computes per-channel RMS
    // + peak every `length` seconds; ametadata=mode=print emits
    // those metrics as log lines that our stderr reader parses in
    // handle_log_line. length=0.25 gives 4Hz updates which matches
    // the UI's polling cadence — bumping faster would generate
    // stderr spam without UI benefit. direct=1 disables ametadata's
    // internal buffering so the lines arrive promptly.
    //
    // alpha.48: dropped the original `!is_decklink()` gate. The old
    // logic assumed astats was tied to the encoder pipeline ("raw
    // output, no encoder pipeline" comment) — but astats is a FILTER,
    // not an encoder concern. The audio chain runs filters → output
    // regardless of whether the output is libx264-encoded or raw
    // PCM to DeckLink. Operator confirmed NDI → DeckLink audio works
    // end-to-end via the alpha.42 TCP bridge; gating off the meters
    // here meant the multiview VU bars stayed muted-gray even when
    // real audio was flowing, which read as "meters are broken"
    // (and triggered the alpha.48 bug report).
    if snap.meters_enabled {
        chain.push("astats=metadata=1:reset=1:length=0.25".into());
        chain.push("ametadata=mode=print:direct=1".into());
    }

    if chain.is_empty() {
        None
    } else {
        Some(chain.join(","))
    }
}

/// alpha.25: encoder selection. Resolves `plan.video_encoder`
/// ("auto" or a specific encoder name) to a concrete encoder, then
/// composes the right per-encoder flag bag. Returns the full encoder
/// command segment that's spliced into build_ffmpeg_cmd between the
/// video filter and the audio block.
///
/// Auto-mode priority (per the Plan agent's D2 decision): macOS
/// VideoToolbox > nvenc > qsv > amf > libx264/libx265. Honors the
/// legacy `ATEM_DISABLE_VT` env override for BMD-parity testing.
///
/// User-picked specific encoders fall back to auto-mode if the
/// bundled FFmpeg doesn't include them (e.g. operator selected
/// h264_nvenc on a build without it) rather than emit a broken cmd.
fn select_encoder(plan: &StreamPlan) -> Vec<String> {
    let codec = plan.video_codec.to_lowercase();
    let chosen = if plan.video_encoder == "auto" || plan.video_encoder.is_empty() {
        auto_select_encoder(&codec)
    } else {
        // Verify the user's pick is actually available; fall back to
        // auto if not (logged so the operator sees what happened).
        let available = crate::ffmpeg_path::available_encoders();
        if available.iter().any(|e| e == &plan.video_encoder) {
            plan.video_encoder.clone()
        } else {
            log::warn!(
                "encoder '{}' not in bundled FFmpeg ({} encoders available); \
                 falling back to auto",
                plan.video_encoder,
                available.len()
            );
            auto_select_encoder(&codec)
        }
    };
    log::info!("encoder: codec={codec} requested={} chosen={chosen}", plan.video_encoder);
    build_encoder_section(plan, &chosen)
}

/// Walk the per-codec encoder priority list, return the first one
/// the bundled FFmpeg has. Always finds something — libx264 / libx265
/// are universally present in the prebuilt distributions.
fn auto_select_encoder(codec: &str) -> String {
    let available = crate::ffmpeg_path::available_encoders();
    let disable_vt = std::env::var("ATEM_DISABLE_VT").is_ok();

    let candidates: Vec<&str> = if codec == "h265" {
        if cfg!(target_os = "macos") {
            vec!["hevc_videotoolbox", "hevc_nvenc", "hevc_qsv", "hevc_amf", "libx265"]
        } else {
            vec!["hevc_nvenc", "hevc_qsv", "hevc_amf", "libx265"]
        }
    } else if cfg!(target_os = "macos") {
        vec!["h264_videotoolbox", "h264_nvenc", "h264_qsv", "h264_amf", "libx264"]
    } else {
        vec!["h264_nvenc", "h264_qsv", "h264_amf", "libx264"]
    };

    for c in &candidates {
        if disable_vt && c.ends_with("_videotoolbox") {
            continue;
        }
        if available.iter().any(|e| e == c) {
            return c.to_string();
        }
    }
    // Pathological fallback — libx264/libx265 are always present, but
    // if even those aren't found, name them anyway so FFmpeg emits a
    // clear "Unknown encoder" error on the operator's terms.
    if codec == "h265" { "libx265".into() } else { "libx264".into() }
}

/// Per-encoder flag composition. Each branch produces the same
/// shape: `-c:v <encoder>` + encoder-specific tuning + bitrate/gop/fps.
/// Common BMD-parity invariants (no B-frames, fixed GOP, Main profile,
/// yuv420p) hold across all branches.
fn build_encoder_section(plan: &StreamPlan, encoder: &str) -> Vec<String> {
    let bitrate = plan.video_bitrate.to_string();
    let bitrate_kbps = (plan.video_bitrate / 1000).to_string();
    let gop = plan.gop().to_string();
    let fps = plan.fps.to_string();
    let mut cmd: Vec<String> = Vec::new();

    match encoder {
        "h264_videotoolbox" | "hevc_videotoolbox" => {
            // VideoToolbox: Apple ANE + GPU. Realtime + allow_sw + CBR
            // matches the alpha.4 BMD-parity tuning.
            cmd.extend(
                [
                    "-c:v", encoder,
                    "-profile:v", "main",
                    "-pix_fmt", "yuv420p",
                    "-realtime", "1",
                    "-allow_sw", "1",
                    "-constant_bit_rate", "1",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
            cmd.extend([
                "-b:v".into(), bitrate,
                "-bf".into(), "0".into(),
                "-g".into(), gop.clone(),
                "-keyint_min".into(), gop,
                "-sc_threshold".into(), "0".into(),
                "-r".into(), fps,
            ]);
        }
        "h264_nvenc" | "hevc_nvenc" => {
            // NVIDIA NVENC: p4 preset = "medium speed", tune ll =
            // low-latency. CBR for stable bitrate matching ATEM's
            // expectations. nvenc accepts -profile:v main directly.
            cmd.extend(
                [
                    "-c:v", encoder,
                    "-preset", "p4",
                    "-tune", "ll",
                    "-rc", "cbr",
                    "-profile:v", "main",
                    "-pix_fmt", "yuv420p",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
            cmd.extend([
                "-b:v".into(), bitrate.clone(),
                "-maxrate".into(), bitrate.clone(),
                "-bufsize".into(), bitrate,
                "-bf".into(), "0".into(),
                "-g".into(), gop.clone(),
                "-keyint_min".into(), gop,
                "-r".into(), fps,
            ]);
        }
        "h264_qsv" | "hevc_qsv" => {
            // Intel QuickSync via libvpl. NV12 pixel format preferred
            // (closer to the hardware's native layout). look_ahead 0
            // for live (avoid the lookahead-induced encode delay).
            cmd.extend(
                [
                    "-c:v", encoder,
                    "-preset", "veryfast",
                    "-look_ahead", "0",
                    "-profile:v", "main",
                    "-pix_fmt", "nv12",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
            cmd.extend([
                "-b:v".into(), bitrate.clone(),
                "-maxrate".into(), bitrate,
                "-bf".into(), "0".into(),
                "-g".into(), gop,
                "-r".into(), fps,
            ]);
        }
        "h264_amf" | "hevc_amf" => {
            // AMD AMF. "speed" quality preset (fastest, lowest latency).
            // CBR rate-control. amf uses `-profile` (no `:v`) for h264.
            cmd.extend(
                [
                    "-c:v", encoder,
                    "-quality", "speed",
                    "-rc", "cbr",
                    "-profile", "main",
                    "-pix_fmt", "yuv420p",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
            cmd.extend([
                "-b:v".into(), bitrate.clone(),
                "-maxrate".into(), bitrate,
                "-bf".into(), "0".into(),
                "-g".into(), gop,
                "-r".into(), fps,
            ]);
        }
        "libx265" => {
            cmd.extend(
                [
                    "-c:v", "libx265",
                    "-profile:v", "main",
                    "-preset", "veryfast",
                    "-tune", "zerolatency",
                    "-pix_fmt", "yuv420p",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
            cmd.push("-x265-params".into());
            cmd.push(format!(
                "bframes=0:no-scenecut=1:keyint={gop}:min-keyint={gop}:\
                 vbv-maxrate={bitrate_kbps}:vbv-bufsize={bitrate_kbps}:\
                 repeat-headers=1:hrd=1:log-level=warning"
            ));
            cmd.extend([
                "-b:v".into(), bitrate,
                "-g".into(), gop.clone(),
                "-keyint_min".into(), gop,
                "-sc_threshold".into(), "0".into(),
                "-r".into(), fps,
            ]);
        }
        _ => {
            // libx264 fallback — also the path for any unrecognized
            // encoder name (defense-in-depth; auto_select_encoder
            // already guarantees a known encoder).
            cmd.extend(
                [
                    "-c:v", "libx264",
                    "-profile:v", "main",
                    "-preset", "veryfast",
                    "-tune", "zerolatency",
                    "-pix_fmt", "yuv420p",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
            cmd.push("-x264-params".into());
            cmd.push(format!(
                "bframes=0:scenecut=0:keyint={gop}:min-keyint={gop}:nal-hrd=cbr"
            ));
            cmd.extend([
                "-b:v".into(), bitrate.clone(),
                "-maxrate".into(), bitrate.clone(),
                "-minrate".into(), bitrate.clone(),
                "-bufsize".into(), bitrate,
                "-g".into(), gop.clone(),
                "-keyint_min".into(), gop,
                "-sc_threshold".into(), "0".into(),
                "-r".into(), fps,
            ]);
        }
    }

    // alpha.25 power-user textarea: append any extra flags the
    // operator pasted. Parsed via parse_extra_flags (handles quoted
    // strings + backslash escapes). Positioned AFTER the encoder
    // block so user can override encoder params (the stated use case);
    // BEFORE the muxer/output block (in build_ffmpeg_cmd) so users
    // can't accidentally rewrite the transport. Bad input surfaces
    // as an FFmpeg startup error in the existing error banner.
    let extras = parse_extra_flags(&plan.encoder_extra_flags);
    if !extras.is_empty() {
        log::info!("encoder_extra_flags appending: {:?}", extras);
        cmd.extend(extras);
    }

    cmd
}

/// alpha.25: audio codec + bitrate + sample-rate + channels block,
/// replacing the previous hardcoded AAC-LC 48k path. AAC-HE / HEv2
/// supported for low-bitrate destinations (build_plan rejects them
/// on ATEM). Channels=1 is mono, 2 is stereo; 5.1 is forward-looking.
fn build_audio_section(plan: &StreamPlan) -> Vec<String> {
    let mut cmd: Vec<String> = Vec::new();
    // The aac encoder handles all three sub-profiles via -profile:a.
    cmd.push("-c:a".into());
    cmd.push("aac".into());
    match plan.audio_codec.as_str() {
        "aac_he" => {
            cmd.push("-profile:a".into());
            cmd.push("aac_he".into());
        }
        "aac_he_v2" => {
            cmd.push("-profile:a".into());
            cmd.push("aac_he_v2".into());
        }
        _ => {} // AAC-LC: no profile flag needed (FFmpeg's default).
    }
    cmd.push("-b:a".into());
    cmd.push(plan.audio_bitrate.to_string());
    cmd.push("-ar".into());
    cmd.push(plan.audio_sample_rate.to_string());
    cmd.push("-ac".into());
    cmd.push(plan.audio_channels.to_string());
    cmd
}

/// alpha.25: minimal shell-style tokenizer for the encoder-extra-flags
/// textarea. Handles double and single quotes, backslash escapes, and
/// whitespace separation. Empty input -> empty vec. Doesn't try to
/// expand env variables or glob — just enough to handle typical
/// FFmpeg flag inputs like `-tune zerolatency -profile:v high` or
/// `-x264-params "rc-lookahead=20:aq-mode=2"`.
fn parse_extra_flags(input: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;
    for ch in input.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        match ch {
            '\\' if !in_single => {
                escape = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod alpha25_tests {
    use super::parse_extra_flags;

    #[test]
    fn parses_simple_flags() {
        assert_eq!(
            parse_extra_flags("-tune zerolatency -profile:v high"),
            vec!["-tune", "zerolatency", "-profile:v", "high"]
        );
    }

    #[test]
    fn parses_quoted_strings() {
        assert_eq!(
            parse_extra_flags(r#"-x264-params "rc-lookahead=20:aq-mode=2""#),
            vec!["-x264-params", "rc-lookahead=20:aq-mode=2"]
        );
    }

    #[test]
    fn parses_escapes() {
        assert_eq!(
            parse_extra_flags(r#"foo bar\ baz qux"#),
            vec!["foo", "bar baz", "qux"]
        );
    }

    #[test]
    fn empty_returns_empty() {
        assert!(parse_extra_flags("").is_empty());
        assert!(parse_extra_flags("   \t\n").is_empty());
    }
}

fn build_rtmp_url(base_url: &str, stream_key: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if !stream_key.is_empty() && !trimmed.ends_with(&format!("/{stream_key}")) {
        format!("{trimmed}/{stream_key}")
    } else {
        trimmed.to_string()
    }
}

/// shlex-style join: quote any token with whitespace or shell
/// metacharacters using single quotes.
fn shlex_join(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|t| {
            if t.is_empty() || t.chars().any(needs_quoting) {
                let escaped = t.replace('\'', "'\\''");
                format!("'{escaped}'")
            } else {
                t.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn needs_quoting(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '|' | '&' | ';' | '<' | '>' | '(' | ')' | '$' | '`' | '"' | '\'' | '\\' | '*' | '?' | '#' | '~' | '!' | '['
    )
}

/// Drains NDI frames from the mpsc channel into FFmpeg's stdin. Exits
/// when the channel closes (NDI capture stopped) or stdin write fails
/// (FFmpeg exited).
async fn ndi_writer_task(
    mut stdin: tokio::process::ChildStdin,
    mut rx: mpsc::Receiver<Vec<u8>>,
    omt_sender: Option<Arc<crate::omt_sender::OmtSender>>,
) {
    let mut sender_log_throttle: u64 = 0;
    while let Some(buf) = rx.recv().await {
        // OMT-out fan-out: feed the same frame to OmtSender before
        // writing to FFmpeg's stdin. Errors here don't break the
        // ATEM-bound FFmpeg path — they just log and skip this
        // frame's OMT publish (a missed OMT frame is recoverable;
        // a missed FFmpeg-stdin frame stalls the encoder).
        if let Some(sender) = omt_sender.as_ref() {
            match sender.feed_frame(&buf) {
                Ok(_rc) => {}
                Err(err) => {
                    if sender_log_throttle.is_multiple_of(60) {
                        log::warn!("OMT-out feed_frame failed (logged 1/60): {err}");
                    }
                    sender_log_throttle = sender_log_throttle.wrapping_add(1);
                }
            }
        }
        if let Err(err) = stdin.write_all(&buf).await {
            log::warn!("raw->ffmpeg stdin write failed: {err}");
            break;
        }
    }
    let _ = stdin.shutdown().await;
    log::info!("raw->ffmpeg writer task exiting");
}

/// Spawn a secondary FFmpeg subprocess that opens the SAME source as
/// the primary streaming FFmpeg and outputs BGRA rawvideo on stdout
/// — used by the OMT-out tee path for non-raw sources (AVF / pipe /
/// RTSP / SRT-listen / RTMP-listen). The primary FFmpeg keeps its
/// mpegts→SRT job; this tee runs in parallel.
///
/// Source-consumption note: this opens the source a SECOND time.
/// USB / built-in cameras typically allow multiple AVF consumers
/// at the OS level; some virtual cameras and proprietary capture
/// drivers may refuse the second open. Network sources (RTSP,
/// SRT/RTMP listen, named pipe acting as a network endpoint) create
/// a second subscriber — works but doubles bandwidth/CPU on that
/// side. Document the limitation if it bites in field reports.
///
/// Returns the spawned Child so the streamer can kill it on stop.
/// Frame bytes are read off stdout in a tokio task spawned here and
/// fed to `sender` directly — no intermediate mpsc channel because
/// the reader is single-purpose and tied to the same lifetime as
/// the sender Arc.
fn spawn_omt_video_tee(
    input_args: &[String],
    width: u32,
    height: u32,
    fps: u32,
    sender: Arc<crate::omt_sender::OmtSender>,
) -> Result<Child> {
    let mut cmd: Vec<String> = vec![
        ffmpeg_path(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
    ];
    // Reuse the same input args the primary FFmpeg uses (including
    // -f avfoundation, -framerate, -video_size, -i "<index>", etc.).
    // No back-pressure flag needed — this FFmpeg is in real-time
    // alignment with the source rate already.
    cmd.extend(input_args.iter().cloned());
    // Single video stream out; ignore any source audio (alpha.13
    // will add an audio tee for non-raw sources). The scale filter
    // matches what the primary FFmpeg emits to the ATEM so the OMT
    // consumer sees consistent dimensions across both feeds.
    cmd.extend([
        "-map".into(),
        "0:v:0".into(),
        "-vf".into(),
        format!(
            "scale={w}:{h}:force_original_aspect_ratio=decrease,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,format=bgra,fps={fps},setsar=1",
            w = width,
            h = height,
            fps = fps,
        ),
        "-pix_fmt".into(),
        "bgra".into(),
        "-f".into(),
        "rawvideo".into(),
        "pipe:1".into(),
    ]);

    log::info!("Launching OMT video tee FFmpeg: {}", shlex_join(&cmd));

    let mut command = Command::new(&cmd[0]);
    command
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    hide_console_tokio(&mut command);
    // Same process-group treatment as the primary FFmpeg so a stop
    // signal can target the tee process group too if needed.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = command
        .spawn()
        .map_err(|e| anyhow!("failed to spawn OMT tee FFmpeg: {e}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("OMT tee FFmpeg stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("OMT tee FFmpeg stderr was not piped"))?;

    // Reader task: pull width*height*4 bytes per frame off stdout and
    // feed to the OmtSender. Exits on EOF or any read error.
    let frame_bytes = (width as usize) * (height as usize) * 4;
    tokio::spawn(omt_video_tee_reader_task(stdout, frame_bytes, sender));
    // Stderr drainer — keeps the pipe from filling and unwinds FFmpeg
    // warnings into the parent log so a failing tee surfaces clearly.
    tokio::spawn(omt_video_tee_stderr_task(stderr));

    Ok(child)
}

async fn omt_video_tee_reader_task(
    mut stdout: tokio::process::ChildStdout,
    frame_bytes: usize,
    sender: Arc<crate::omt_sender::OmtSender>,
) {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; frame_bytes];
    let mut frame_count: u64 = 0;
    let mut error_log_throttle: u64 = 0;
    loop {
        match stdout.read_exact(&mut buf).await {
            Ok(_) => match sender.feed_frame(&buf) {
                Ok(_) => {
                    frame_count = frame_count.wrapping_add(1);
                }
                Err(err) => {
                    if error_log_throttle.is_multiple_of(60) {
                        log::warn!(
                            "OMT tee feed_frame failed (logged 1/60): {err}"
                        );
                    }
                    error_log_throttle = error_log_throttle.wrapping_add(1);
                }
            },
            Err(err) => {
                // EOF is the normal exit path (primary stop, source
                // ended, tee FFmpeg killed). Anything else is logged.
                if err.kind() != std::io::ErrorKind::UnexpectedEof {
                    log::warn!("OMT tee stdout read error: {err}");
                }
                break;
            }
        }
    }
    log::info!("OMT video tee reader task exiting ({frame_count} frames)");
}

async fn omt_video_tee_stderr_task(mut stderr: tokio::process::ChildStderr) {
    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(&mut stderr).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        // Tee FFmpeg's loglevel is set to "warning" so this is
        // signal-to-noise. Prefix the message so the source is
        // unambiguous in the log.
        log::warn!("[omt-tee-ffmpeg] {line}");
    }
}

/// Drains NDI audio chunks from the mpsc channel and fans them out
/// to up to two consumers:
///   - `sender` — alpha.13 OmtSender feed (OMT-out audio publishing).
///   - `bridge` — alpha.42 TCP bridge to FFmpeg (so the encoder gets
///     real source audio instead of lavfi anullsrc silence).
/// Exits when the channel closes (NDI capture stopped). Both
/// consumers are best-effort: send errors are throttle-logged and
/// the chunk is dropped, never propagated, because audio drops are
/// recoverable downstream and we don't want to take the video path
/// down with us.
async fn ndi_audio_writer_task(
    mut rx: mpsc::Receiver<NdiAudioChunk>,
    sender: Option<Arc<crate::omt_sender::OmtSender>>,
    bridge: Option<crate::audio_bridge::AudioBridge>,
) {
    let mut chunk_count: u64 = 0;
    let mut omt_error_log_throttle: u64 = 0;
    let mut bridge_error_log_throttle: u64 = 0;
    let mut bridge_dead_logged_once = false;
    while let Some(chunk) = rx.recv().await {
        if let Some(s) = sender.as_ref() {
            match s.feed_audio_frame(
                &chunk.samples,
                chunk.num_channels,
                chunk.sample_rate,
            ) {
                Ok(_rc) => {}
                Err(err) => {
                    if omt_error_log_throttle.is_multiple_of(60) {
                        log::warn!("OMT-out feed_audio_frame failed (logged 1/60): {err}");
                    }
                    omt_error_log_throttle = omt_error_log_throttle.wrapping_add(1);
                }
            }
        }
        if let Some(b) = bridge.as_ref() {
            let bytes = crate::audio_bridge::ndi_planar_f32_to_s16le_interleaved(
                &chunk.samples,
                chunk.num_channels,
            );
            if !bytes.is_empty() {
                // alpha.49: surface bridge send failures. Pre-49 this
                // was `let _ = b.send(bytes).await` and a dead bridge
                // silently swallowed audio forever — exactly the
                // "audio died after a couple minutes" report. Now we
                // log once-loud when the bridge first dies + throttle
                // subsequent failures so the operator sees the cause
                // in /api/log without filling the buffer.
                if !b.send(bytes).await {
                    if !bridge_dead_logged_once {
                        bridge_dead_logged_once = true;
                        log::error!(
                            "audio_bridge writer task gone — FFmpeg disconnected its TCP \
                             audio input. Audio will stay silent until the next reconnect \
                             attempt brings up a fresh bridge."
                        );
                    } else if bridge_error_log_throttle.is_multiple_of(240) {
                        log::warn!(
                            "audio_bridge still gone after {} dropped chunks",
                            bridge_error_log_throttle
                        );
                    }
                    bridge_error_log_throttle =
                        bridge_error_log_throttle.wrapping_add(1);
                }
            }
        }
        chunk_count = chunk_count.wrapping_add(1);
    }
    log::info!("NDI audio writer task exiting (drained {chunk_count} chunks)");
}

#[allow(dead_code)]
pub fn _suppress_unused(_: Snapshot) {}
