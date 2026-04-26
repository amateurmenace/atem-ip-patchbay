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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
}

impl StreamPlan {
    pub fn gop(&self) -> u32 {
        (self.keyframe_seconds * self.fps).max(1)
    }
}

pub struct Streamer {
    state: Arc<EncoderState>,
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
    pub fn new(state: Arc<EncoderState>) -> Arc<Self> {
        Arc::new(Self {
            state,
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

        let mut child = command
            .spawn()
            .map_err(|e| anyhow!("failed to spawn ffmpeg: {e}"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("ffmpeg stderr was not piped"))?;

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
            // tokio's Child::kill sends SIGKILL on Unix; FFmpeg will
            // tear down quickly and the SRT receiver detects the drop.
            // If we ever need a graceful close (the receiver wants a
            // clean teardown), add a `nix`-based SIGINT-then-SIGKILL
            // path here.
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
        })
    }

    /// Variant of build_ffmpeg_cmd for NDI sources — input is rawvideo
    /// on pipe:0 (frame format determined by the upstream probe), with
    /// silent audio from lavfi.
    fn build_ffmpeg_cmd_for_ndi(&self, plan: &StreamPlan, fmt: &NdiVideoFormat) -> Vec<String> {
        let size = format!("{}x{}", fmt.width, fmt.height);
        let fps = fmt.fps().to_string();
        let mut input_args: Vec<String> = vec![
            "-f".into(), "rawvideo".into(),
            "-pix_fmt".into(), fmt.ffmpeg_pix_fmt.into(),
            "-s".into(), size,
            "-r".into(), fps,
            "-i".into(), "pipe:0".into(),
            "-f".into(), "lavfi".into(),
            "-i".into(), "anullsrc=channel_layout=stereo:sample_rate=48000".into(),
        ];

        // Build a temp Source so the existing build_ffmpeg_cmd
        // pathway works — overwrite the input args with the NDI ones.
        let mut adjusted = plan.clone();
        adjusted.source.ffmpeg_input_args = input_args.split_off(0);
        adjusted.source.combined_av = false; // separate inputs (pipe + lavfi)
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

        // Filter chain: scale to target dimensions + format yuv420p so
        // the encoder gets clean input even if the source delivered
        // something else. (Overlays — drawtext/logo — were in v0.1.0;
        // they come back in Phase 8 once the overlay UI is reworked.)
        let filter = format!(
            "[{v_in}]scale={w}:{h}:force_original_aspect_ratio=decrease,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,format=yuv420p[v0]",
            w = plan.width,
            h = plan.height,
        );
        cmd.push("-filter_complex".into());
        cmd.push(filter);
        cmd.extend([
            "-map".into(),
            "[v0]".into(),
            "-map".into(),
            a_in.into(),
        ]);

        // Video encoder — Main profile, no B-frames, fixed GOP. H.264
        // for broad compatibility, H.265 for Streaming Bridge native
        // mode (matches what real BMD WPs send to ATEM Mini built-in).
        let bitrate_kbps = (plan.video_bitrate / 1000).to_string();
        let bitrate_str = plan.video_bitrate.to_string();
        let fps_str = plan.fps.to_string();
        if plan.video_codec == "h265" {
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
                "-b:v".into(), bitrate_str.clone(),
                "-g".into(), gop.clone(),
                "-keyint_min".into(), gop.clone(),
                "-sc_threshold".into(), "0".into(),
                "-r".into(), fps_str.clone(),
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

        // Audio — AAC-LC 48k stereo.
        cmd.extend([
            "-c:a".into(), "aac".into(),
            "-b:a".into(), plan.audio_bitrate.to_string(),
            "-ar".into(), "48000".into(),
            "-ac".into(), "2".into(),
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
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
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

        // Connection-state heuristics — flip to Streaming once we see
        // FFmpeg's stream mapping or an SRT connection-established log.
        if lower.contains("connection established") || lower.contains("stream mapping") {
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

const ERROR_TAGS: &[&str] = &[
    "connection refused",
    "connection setup failure",
    "operation timed out",
    "no route to host",
    "srt error",
    "protocol not found",
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
