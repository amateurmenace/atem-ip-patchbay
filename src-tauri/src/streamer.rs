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
        // ndi_audio_rx is the audio channel from NDI capture; only
        // populated when OMT output is enabled (so we don't waste
        // SDK queue space draining audio nobody's listening for).
        let omt_output_enabled = self.state.snapshot().omt_output_enabled;
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
                        omt_output_enabled,
                    )?;
                    let cmd = self.build_ffmpeg_cmd_for_ndi(&plan, &format);
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
        if let (Some(audio_rx), Some(sender)) = (ndi_audio_rx, omt_sender.clone()) {
            tokio::spawn(ndi_audio_writer_task(audio_rx, sender));
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

        // Spawn the monitor task. It owns the stderr handle, parses
        // telemetry, and on EOF awaits the child to get the exit
        // status and clear the slot in inner.
        let me = self.clone();
        tokio::spawn(async move {
            me.run_monitor(stderr).await;
        });

        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        inner.stop_requested = true;
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
        Ok(StreamPlan {
            width,
            height,
            fps,
            video_bitrate: cfg.bitrate,
            audio_bitrate: cfg.audio_bitrate,
            keyframe_seconds: cfg.keyframe_interval,
            output_url,
            protocol,
            source,
            video_codec: snap.video_codec.to_lowercase(),
            video_filter,
            audio_filter,
            audio_output_mono: snap.audio_output_mono,
            decklink_device_name: String::new(),
            decklink_format_code: String::new(),
            decklink_pixel_format: String::new(),
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
            audio_output_mono: snap.audio_output_mono,
            decklink_device_name: snap.decklink_device_name.clone(),
            decklink_format_code: snap.decklink_format_code.clone(),
            decklink_pixel_format: if snap.decklink_pixel_format.is_empty() {
                "uyvy422".to_string()
            } else {
                snap.decklink_pixel_format.clone()
            },
        })
    }

    /// Variant of build_ffmpeg_cmd for NDI sources — input is rawvideo
    /// on pipe:0 (frame format determined by the upstream probe).
    /// Audio source depends on Audio Mixer mode: "custom" + a picked
    /// AVF audio device routes through `-f avfoundation -i :name`,
    /// every other case uses lavfi anullsrc (silent — gets muted to
    /// match user intent for the silent path, passes through as
    /// silence-where-audio-would-be for auto).
    fn build_ffmpeg_cmd_for_ndi(&self, plan: &StreamPlan, fmt: &NdiVideoFormat) -> Vec<String> {
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

        if cfg!(target_os = "macos") {
            if let Some(audio_name) = custom_audio_name.as_deref() {
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
            } else {
                input_args.extend([
                    "-f".into(), "lavfi".into(),
                    "-i".into(), "anullsrc=channel_layout=stereo:sample_rate=48000".into(),
                ]);
            }
        } else if cfg!(target_os = "windows") {
            // DirectShow audio-only input. FFmpeg's dshow demuxer
            // accepts an `audio=<DeviceName>` URI for an audio-only
            // device; same role as the AVF `:<name>` form on macOS.
            // Dante Virtual Soundcard for Windows exposes via WDM/
            // Core Audio and shows up in `-f dshow -list_devices` —
            // its name typically includes parens (e.g.
            // "Dante Virtual Soundcard (Dante Virtual Soundcard
            // 64ch x64)"), passed verbatim through the Command
            // argument so FFmpeg's parser sees the whole string.
            //
            // The pan filter in build_audio_filter() is platform-
            // agnostic — it fires off the device NAME, so picking a
            // device whose name contains "dante" or "aggregate" gets
            // the same L/R channel-pair routing as macOS Dante.
            if let Some(audio_name) = custom_audio_name.as_deref() {
                log::info!("NDI + custom dshow audio: routing through {audio_name:?}");
                input_args.extend([
                    "-f".into(), "dshow".into(),
                    "-i".into(), format!("audio={audio_name}"),
                ]);
            } else {
                input_args.extend([
                    "-f".into(), "lavfi".into(),
                    "-i".into(), "anullsrc=channel_layout=stereo:sample_rate=48000".into(),
                ]);
            }
        } else {
            // Linux — PulseAudio / ALSA wiring lands as a follow-up
            // when there's a real Linux production use case to test
            // against. Stay on lavfi for now so the stream still
            // works (silent audio).
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
        if fmt.width != plan.width || fmt.height != plan.height {
            let filter = format!("scale={}:{}:flags=lanczos", plan.width, plan.height);
            log::info!(
                "NDI scale required: {}x{} -> {}x{} via filter {filter:?}",
                fmt.width, fmt.height, plan.width, plan.height,
            );
            adjusted.video_filter = Some(filter);
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
        self.build_ffmpeg_cmd_for_ndi(plan, &as_ndi)
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

        // DeckLink output muxer takes raw video (rawvideo codec — set
        // explicitly so FFmpeg doesn't try to AAC-encode), PCM audio
        // at 48 kHz, and the pixel format the card expects. Modern
        // DeckLink cards default to uyvy422 (8-bit YUV 4:2:2);
        // 10-bit cards can take yuv422p10le but the UI doesn't
        // surface that yet. -format_code is the per-card mode
        // identifier (Hp59 == 1080p59.94, etc.) — passed through
        // from build_plan_decklink which looked it up via
        // probe_decklink_modes against the live card.
        let pix_fmt = if plan.decklink_pixel_format.is_empty() {
            "uyvy422"
        } else {
            plan.decklink_pixel_format.as_str()
        };
        let ac = if plan.audio_output_mono { "1" } else { "2" };
        cmd.extend([
            "-c:v".into(), "rawvideo".into(),
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

        // Video encoder — Main profile, no B-frames, fixed GOP. H.264
        // for broad compatibility, H.265 for Streaming Bridge native
        // mode (matches what real BMD WPs send to ATEM Mini built-in).
        //
        // On macOS, route through VideoToolbox (Apple ANE + GPU) so
        // the encoder runs on dedicated silicon rather than CPU. The
        // libx264 veryfast path costs ~80% of one core at 1080p30 and
        // dominates the NDI ingest profile because the receiver
        // thread is already CPU-bound; VT drops that to single-digit
        // % and frees headroom for everything else. The flags below
        // produce a Main-profile, no-B-frame, CBR-ish stream that the
        // BMD SRT decoder accepts. Set ATEM_DISABLE_VT=1 to fall back
        // to libx264/libx265 for BMD-parity verification.
        let bitrate_str = plan.video_bitrate.to_string();
        let fps_str = plan.fps.to_string();
        let use_vt = cfg!(target_os = "macos") && std::env::var("ATEM_DISABLE_VT").is_err();

        if use_vt {
            let codec = if plan.video_codec == "h265" {
                "hevc_videotoolbox"
            } else {
                "h264_videotoolbox"
            };
            cmd.extend(
                [
                    "-c:v", codec,
                    "-profile:v", "main",
                    "-pix_fmt", "yuv420p",
                    // Hint VT to prioritize encode latency over
                    // quality — drops frames before delaying when
                    // the encoder can't keep up. Right call for
                    // live SRT, wrong call for VOD transcode.
                    "-realtime", "1",
                    // Allow software fallback if hardware encoding
                    // can't initialize (rare; happens if another
                    // process is holding all ANE slots). Slower
                    // than libx264 in that mode but still streams.
                    "-allow_sw", "1",
                    // True CBR — matches what real BMD encoders
                    // emit and what the ATEM SRT decoder expects
                    // for clean pacing. macOS 13+ honors this; on
                    // older macOS, FFmpeg silently ignores the
                    // flag and VT runs in ABR which is also
                    // accepted by ATEM in practice.
                    "-constant_bit_rate", "1",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
            cmd.extend([
                "-b:v".into(), bitrate_str,
                // Explicit no-B-frames. VT's default profile
                // setting may already exclude B-frames at Main
                // profile, but the flag is load-bearing for the
                // BMD parity guarantee — pcap analysis showed
                // real Web Presenters never emit B-frames.
                "-bf".into(), "0".into(),
                "-g".into(), gop.clone(),
                "-keyint_min".into(), gop,
                "-sc_threshold".into(), "0".into(),
                "-r".into(), fps_str,
            ]);
        } else if plan.video_codec == "h265" {
            let bitrate_kbps = (plan.video_bitrate / 1000).to_string();
            cmd.extend(
                [
                    "-c:v",
                    "libx265",
                    "-profile:v",
                    "main",
                    "-preset",
                    "veryfast",
                    "-tune",
                    "zerolatency",
                    "-pix_fmt",
                    "yuv420p",
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
                "-b:v".into(), bitrate_str,
                "-g".into(), gop.clone(),
                "-keyint_min".into(), gop,
                "-sc_threshold".into(), "0".into(),
                "-r".into(), fps_str,
            ]);
        } else {
            cmd.extend(
                [
                    "-c:v",
                    "libx264",
                    "-profile:v",
                    "main",
                    "-preset",
                    "veryfast",
                    "-tune",
                    "zerolatency",
                    "-pix_fmt",
                    "yuv420p",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
            cmd.push("-x264-params".into());
            cmd.push(format!(
                "bframes=0:scenecut=0:keyint={gop}:min-keyint={gop}:nal-hrd=cbr"
            ));
            cmd.extend([
                "-b:v".into(), bitrate_str.clone(),
                "-maxrate".into(), bitrate_str.clone(),
                "-minrate".into(), bitrate_str.clone(),
                "-bufsize".into(), bitrate_str,
                "-g".into(), gop.clone(),
                "-keyint_min".into(), gop,
                "-sc_threshold".into(), "0".into(),
                "-r".into(), fps_str,
            ]);
        }

        // Audio filter — currently only the pan filter for
        // multi-channel devices (Dante VSC, CoreAudio aggregate). Goes
        // before -c:a so the encoder sees the already-downmixed
        // stereo result.
        if let Some(filter) = plan.audio_filter.as_deref() {
            cmd.push("-af".into());
            cmd.push(filter.into());
        }
        // Audio — AAC-LC 48k. Channels: stereo by default, mono
        // (single summed channel) when the user has set Audio Mixer
        // -> Mono. Real BMD devices accept either, so this is purely
        // a content choice for the operator (e.g. radio-style talk
        // streams where mono saves bitrate for the same intelligibility).
        let ac = if plan.audio_output_mono { "1" } else { "2" };
        cmd.extend([
            "-c:a".into(), "aac".into(),
            "-b:a".into(), plan.audio_bitrate.to_string(),
            "-ar".into(), "48000".into(),
            "-ac".into(), ac.into(),
        ]);

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

    async fn run_monitor(self: Arc<Self>, stderr: tokio::process::ChildStderr) {
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

        // Stderr EOF — child is exiting. Wait for the exit code.
        // Also tear down any OMT video tee FFmpeg + sender so the
        // natural-exit path (source ended, FFmpeg crashed, etc.) frees
        // resources without requiring the user to click Stop. Mirrors
        // what stop() does for the same fields.
        let exit_code = {
            let mut inner = self.inner.lock().await;
            let mut child = match inner.child.take() {
                Some(c) => c,
                None => return,
            };
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

        // Snapshot the recent log tail before lock-free mutation of state.
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

        self.state.stats_in_place(|s| {
            if stop_requested {
                s.status = "Idle".into();
            } else if let Some(rc) = exit_code {
                if rc != 0 {
                    s.status = "Interrupted".into();
                    if s.error.is_none() {
                        s.error = Some(format!("FFmpeg exited with code {rc}: {recent_tail}"));
                    }
                } else {
                    s.status = "Idle".into();
                }
            } else {
                s.status = "Idle".into();
            }
            s.started_at = None;
            s.bitrate = 0;
        });
    }

    async fn handle_log_line(&self, line: &str) {
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

    if chain.is_empty() {
        None
    } else {
        Some(chain.join(","))
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

/// Drains NDI audio chunks from the mpsc channel into the OmtSender.
/// One chunk per call to OMT_Send_SendAudio. Exits when the channel
/// closes (NDI capture stopped) or after enough consecutive send
/// errors that something is clearly broken. Unlike the video path,
/// audio drop-on-error is non-fatal — the OMT consumer can recover
/// from a missing audio chunk via its own resync logic.
async fn ndi_audio_writer_task(
    mut rx: mpsc::Receiver<NdiAudioChunk>,
    sender: Arc<crate::omt_sender::OmtSender>,
) {
    let mut chunk_count: u64 = 0;
    let mut error_log_throttle: u64 = 0;
    while let Some(chunk) = rx.recv().await {
        match sender.feed_audio_frame(
            &chunk.samples,
            chunk.num_channels,
            chunk.sample_rate,
        ) {
            Ok(_rc) => {
                chunk_count = chunk_count.wrapping_add(1);
            }
            Err(err) => {
                if error_log_throttle.is_multiple_of(60) {
                    log::warn!("OMT-out feed_audio_frame failed (logged 1/60): {err}");
                }
                error_log_throttle = error_log_throttle.wrapping_add(1);
            }
        }
    }
    log::info!("OMT audio writer task exiting (sent {chunk_count} chunks)");
}

#[allow(dead_code)]
pub fn _suppress_unused(_: Snapshot) {}
