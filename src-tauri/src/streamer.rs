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

use crate::ffmpeg_path::ffmpeg_path;
use crate::ndi_capture::{NdiCapture, NdiVideoFormat};
use crate::ndi_runtime;
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

        // NDI sources need a probe + receiver-thread spin-up before we
        // know the FFmpeg input args. Other source types build the
        // command directly from EncoderState.
        let (cmd, ndi_capture, ndi_frame_rx) = if plan.source.id == "ndi" {
            let source_name = plan.source.label.clone();
            let ndi_source = ndi_runtime::find_source_by_name(&source_name)
                .ok_or_else(|| anyhow!("NDI source not found: {source_name:?}. Refresh the discovery list."))?;
            let (format, capture, rx) = NdiCapture::start_and_probe_format(
                ndi_source,
                Duration::from_secs(5),
            )?;
            let cmd = self.build_ffmpeg_cmd_for_ndi(&plan, &format);
            (cmd, Some(capture), Some(rx))
        } else {
            (self.build_ffmpeg_cmd(&plan), None, None)
        };

        log::info!("Launching FFmpeg: {}", shlex_join(&cmd));

        let mut command = Command::new(&cmd[0]);
        command
            .args(&cmd[1..])
            .stdin(if ndi_capture.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

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
            match std::process::Command::new("/bin/bash")
                .arg("-c")
                .arg(&cmd)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(_) => log::info!(
                    "FFmpeg watchdog armed: parent={parent_pid} -> ffmpeg pgid={ff_pid}"
                ),
                Err(e) => log::warn!(
                    "FFmpeg watchdog spawn failed (parent crash will leave orphans): {e}"
                ),
            }
        }

        // Wire the NDI -> FFmpeg stdin pipe via a small drainer task.
        if let (Some(_), Some(rx)) = (ndi_capture.as_ref(), ndi_frame_rx) {
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("ffmpeg stdin was not piped (NDI source)"))?;
            tokio::spawn(ndi_writer_task(stdin, rx));
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
            video_filter: None,
            audio_filter,
            audio_output_mono: snap.audio_output_mono,
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
        } else {
            // Non-Mac path stays on lavfi — DirectShow audio injection
            // for an NDI video source is a separate piece of work.
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

    fn build_ffmpeg_cmd(&self, plan: &StreamPlan) -> Vec<String> {
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
        let exit_code = {
            let mut inner = self.inner.lock().await;
            let mut child = match inner.child.take() {
                Some(c) => c,
                None => return,
            };
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
async fn ndi_writer_task(mut stdin: tokio::process::ChildStdin, mut rx: mpsc::Receiver<Vec<u8>>) {
    while let Some(buf) = rx.recv().await {
        if let Err(err) = stdin.write_all(&buf).await {
            log::warn!("NDI->ffmpeg stdin write failed: {err}");
            break;
        }
    }
    let _ = stdin.shutdown().await;
    log::info!("NDI->ffmpeg writer task exiting");
}

#[allow(dead_code)]
pub fn _suppress_unused(_: Snapshot) {}
