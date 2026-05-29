//! NDI receiver thread that pipes raw video frames into FFmpeg's stdin.
//!
//! The thread runs blocking NDI capture in a `std::thread` (the NDI
//! C SDK is synchronous), then sends each frame through a Tokio mpsc
//! channel. A separate Tokio task spawned by the streamer reads from
//! that channel and async-writes the bytes to FFmpeg's stdin. The
//! mpsc is bounded (64 frames) so a stalled FFmpeg back-pressures
//! the receiver thread instead of growing memory unbounded.
//!
//! Hand-off:
//! 1. [`NdiCapture::start_and_probe_format`] creates a Receiver,
//!    blocks for the first VideoFrame (so the streamer can build a
//!    matching FFmpeg command), and returns the format + an `mpsc::
//!    Receiver` channel.
//! 2. The streamer launches FFmpeg with stdin piped, spawns a writer
//!    task that drains the channel into stdin.
//! 3. On stop, [`NdiCapture::stop`] flips an atomic; the thread
//!    exits and drops the channel sender, closing the writer task,
//!    closing FFmpeg's stdin, and triggering FFmpeg's clean shutdown.
//!
//! Audio is captured on the same thread as video — alpha.12 adds an
//! audio drain (`capture_audio_timeout(0)` after each video poll) and
//! ships the resulting planar f32 PCM through a parallel mpsc channel
//! to the streamer's OMT audio writer task. The FFmpeg-bound audio
//! path is still separate (lavfi anullsrc / AVF custom) — this audio
//! channel exists only to feed OMT-out. When the streamer doesn't
//! enable OMT output, `start_and_probe_format` is called with
//! `audio_wanted = false` and the capture loop skips audio drain
//! entirely.

use anyhow::{anyhow, Result};
use grafton_ndi::{
    AudioFrame, LineStrideOrSize, PixelFormat, Receiver, ReceiverBandwidth,
    ReceiverColorFormat, ReceiverOptions, Source, VideoFrame, NDI,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Channel buffer size in frames. At 1080p60 BGRA each frame is
/// ~8.3 MB; 64 frames = ~530 MB worst-case if FFmpeg fully stalls,
/// which we'd never want to actually hit. In practice FFmpeg drains
/// the channel within a frame or two, so the buffer just absorbs the
/// occasional encoder spike.
const FRAME_CHANNEL_CAPACITY: usize = 64;

/// Audio chunk buffer size. NDI delivers audio in batches of ~10ms
/// at 48kHz stereo (~960 samples per channel = ~7.5 KB per chunk).
/// 128 chunks ≈ 1.3 seconds of headroom — plenty to absorb OMT
/// consumer hiccups without growing memory unbounded.
const AUDIO_CHANNEL_CAPACITY: usize = 128;

/// One JPEG preview snapshot per N captured frames. At 30 FPS source
/// rate a stride of 15 -> ~2 FPS preview, which is plenty for the
/// UI's "what's the camera seeing" thumbnail and keeps the encode
/// cost negligible (BGRA -> JPEG ~3-5ms per frame on M-series).
const PREVIEW_FRAME_STRIDE: u64 = 15;
/// JPEG quality (0-100). 60 looks fine at the small preview size and
/// keeps payload around 50-150 KB per frame at 1080p.
const PREVIEW_JPEG_QUALITY: u8 = 60;

/// What the streamer needs to build a matching FFmpeg input args.
#[derive(Debug, Clone, Copy)]
pub struct NdiVideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    /// FFmpeg `-pix_fmt` token corresponding to the NDI pixel format
    /// the receiver requested (typically `bgra`).
    pub ffmpeg_pix_fmt: &'static str,
}

impl NdiVideoFormat {
    pub fn fps(&self) -> u32 {
        if self.fps_den == 0 {
            30
        } else {
            ((self.fps_num as f64) / (self.fps_den as f64)).round() as u32
        }
    }
}

/// One chunk of NDI audio in the layout OMT expects (planar f32 PCM —
/// channel 0's samples first, then channel 1's, etc.). grafton-ndi
/// already stores audio planar internally, so we just clone the
/// `data()` slice into an owned Vec for cross-thread transfer.
/// `samples.len() / num_channels` recovers the samples-per-channel
/// count — kept implicit rather than stored to avoid carrying a
/// redundant field through the channel.
#[derive(Debug)]
pub struct NdiAudioChunk {
    pub samples: Vec<f32>,
    pub sample_rate: i32,
    pub num_channels: i32,
}

pub struct NdiCapture {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    /// Latest JPEG snapshot, refreshed by the capture thread once
    /// every PREVIEW_FRAME_STRIDE captured frames. Read by the
    /// /api/preview HTTP handler. None until the first preview frame
    /// has been encoded.
    preview: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
}

impl NdiCapture {
    /// Latest JPEG-encoded preview frame, if one has been captured
    /// yet. Cheap clone (bytes are Arc'd internally for the read).
    pub fn latest_preview(&self) -> Option<Vec<u8>> {
        self.preview.lock().unwrap().clone()
    }
}

impl NdiCapture {
    /// Start capture from the named NDI source. Blocks for up to
    /// `format_timeout` waiting for the first video frame so the
    /// caller can learn the source's actual resolution and frame
    /// rate before launching FFmpeg. Returns the format plus the
    /// receiving end of an mpsc channel — the caller drains that
    /// into FFmpeg's stdin.
    ///
    /// When `audio_wanted` is true, also returns an audio receiver
    /// channel; the capture loop interleaves `capture_audio_timeout(0)`
    /// calls after each video poll and forwards owned `NdiAudioChunk`s.
    /// When false, the loop never polls audio and the returned audio
    /// channel is `None`. Setting it false keeps the SDK's internal
    /// audio queue from growing for streams the user isn't OMT-
    /// publishing.
    pub fn start_and_probe_format(
        source: Source,
        format_timeout: Duration,
        audio_wanted: bool,
    ) -> Result<(
        NdiVideoFormat,
        Self,
        mpsc::Receiver<Vec<u8>>,
        Option<mpsc::Receiver<NdiAudioChunk>>,
    )> {
        // Each capture session gets its own NDI handle. (The
        // discovery Finder lives on its own NDI handle in
        // [`crate::ndi_runtime`]; receivers and finders are
        // independent.)
        let ndi = NDI::new()?;
        let receiver = Receiver::new(
            &ndi,
            &ReceiverOptions::builder(source)
                .color(ReceiverColorFormat::BGRX_BGRA)
                .bandwidth(ReceiverBandwidth::Highest)
                .build(),
        )?;

        // Probe — capture frames until we get one with non-empty data.
        // NDI sometimes sends empty status frames before the real
        // video starts, so we loop within the timeout.
        let deadline = std::time::Instant::now() + format_timeout;
        let frame = loop {
            if std::time::Instant::now() > deadline {
                return Err(anyhow!(
                    "no video frame received within {:?} — is the NDI source actually sending?",
                    format_timeout
                ));
            }
            match receiver.capture_video(Duration::from_millis(500)) {
                Ok(f) if !f.data.is_empty() && f.width > 0 && f.height > 0 => break f,
                Ok(_) => continue,
                Err(_) => continue,
            }
        };

        let format = NdiVideoFormat {
            width: frame.width as u32,
            height: frame.height as u32,
            fps_num: frame.frame_rate_n.max(1) as u32,
            fps_den: frame.frame_rate_d.max(1) as u32,
            ffmpeg_pix_fmt: pix_fmt_for_ffmpeg(frame.pixel_format),
        };
        let bpp = bytes_per_pixel(frame.pixel_format);
        let expected_packed = (format.width as usize) * (format.height as usize) * bpp;
        log::info!(
            "NDI capture probed: {}x{}@{}/{} pix_fmt={:?}->{} bpp={} \
             stride={:?} data_len={} expected_packed={}",
            format.width,
            format.height,
            format.fps_num,
            format.fps_den,
            frame.pixel_format,
            format.ffmpeg_pix_fmt,
            bpp,
            frame.line_stride_or_size,
            frame.data.len(),
            expected_packed,
        );

        let (tx, rx) = mpsc::channel::<Vec<u8>>(FRAME_CHANNEL_CAPACITY);
        // Send the buffered first frame so the encoder gets a clean
        // start without a 1-frame stutter. Pack first to strip any
        // line-stride padding (NDI senders are allowed to align rows
        // to >width*bpp; FFmpeg's rawvideo demuxer expects tightly
        // packed frames).
        let first_packed = pack_frame(&frame, bpp);
        log::info!(
            "NDI first frame packed: {} bytes (expected {})",
            first_packed.len(),
            expected_packed
        );
        let _ = tx.try_send(first_packed);

        // Audio channel — only allocated when the caller plans to
        // consume audio (OMT-out enabled). When `audio_wanted` is
        // false, the capture loop receives `None` for its audio tx
        // and skips audio drain entirely.
        let (audio_tx, audio_rx) = if audio_wanted {
            let (atx, arx) = mpsc::channel::<NdiAudioChunk>(AUDIO_CHANNEL_CAPACITY);
            (Some(atx), Some(arx))
        } else {
            (None, None)
        };

        let stop = Arc::new(AtomicBool::new(false));
        let preview = Arc::new(std::sync::Mutex::new(None));
        let stop_w = stop.clone();
        let preview_w = preview.clone();
        let handle = thread::Builder::new()
            .name("ndi-capture".into())
            .spawn(move || {
                let _ndi = ndi; // keep handle alive for the receiver's lifetime
                run_capture_loop(receiver, stop_w, tx, audio_tx, preview_w);
            })?;

        Ok((
            format,
            NdiCapture {
                stop,
                handle: Some(handle),
                preview,
            },
            rx,
            audio_rx,
        ))
    }

    /// Signal the capture thread to exit and wait for it to drain.
    /// Drops the channel sender, which causes the streamer's writer
    /// task to see the channel close and shut down FFmpeg's stdin.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            // Bounded join. With the stop-aware send loop the capture
            // thread exits within ~one 500ms poll once `stop` is set, so
            // this normally returns immediately. The timeout is the
            // belt-and-suspenders cap: if a grafton-ndi SDK call ever
            // blocks past it, we detach rather than hang — shutdown and
            // recovery must never freeze on a stuck capture thread.
            join_with_timeout(handle, "ndi-capture", Duration::from_secs(3));
        }
    }
}

impl Drop for NdiCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Result of [`send_frame_or_stop`].
enum SendOutcome {
    /// Frame handed off to the channel.
    Sent,
    /// `stop` was set while the channel was full, or the receiver was
    /// dropped — the capture loop should exit.
    Stopped,
}

/// Send one frame to the bounded channel without ever parking the
/// capture thread indefinitely. `tx.blocking_send()` blocks until
/// capacity frees, which never happens if downstream FFmpeg has
/// stopped reading its stdin (hung opening a dshow audio device, a
/// stalled DeckLink output, etc.). A parked capture thread can't
/// observe the `stop` flag, so `stop()`'s `handle.join()` hangs
/// forever — and it holds the streamer lock while it does. That is the
/// unrecoverable Dante-on-NDI wedge. We poll `try_send` and re-check
/// `stop` on every full-channel retry so a back-pressured or wedged
/// FFmpeg can never make this thread unkillable.
fn send_frame_or_stop(
    tx: &mpsc::Sender<Vec<u8>>,
    mut buf: Vec<u8>,
    stop: &AtomicBool,
) -> SendOutcome {
    loop {
        match tx.try_send(buf) {
            Ok(()) => return SendOutcome::Sent,
            Err(mpsc::error::TrySendError::Closed(_)) => return SendOutcome::Stopped,
            Err(mpsc::error::TrySendError::Full(returned)) => {
                if stop.load(Ordering::Acquire) {
                    return SendOutcome::Stopped;
                }
                buf = returned;
                // Brief yield before retry. 5ms is ~a third of a 60fps
                // frame interval — short enough to add no meaningful
                // latency when back-pressure is transient, long enough
                // not to spin the CPU when FFmpeg is genuinely stuck.
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// Join a thread but give up after `timeout`, logging and detaching
/// instead of blocking forever. Used by `stop()` so a wedged receiver
/// thread can never freeze the shutdown/recovery path. A short-lived
/// helper thread owns the actual join; if it doesn't complete in time
/// we return and let the OS reclaim everything at process exit.
fn join_with_timeout(handle: JoinHandle<()>, name: &str, timeout: Duration) {
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    thread::spawn(move || {
        let _ = handle.join();
        let _ = done_tx.send(());
    });
    if done_rx.recv_timeout(timeout).is_err() {
        log::warn!(
            "{name} thread did not exit within {timeout:?}; detaching \
             (reclaimed at process exit)"
        );
    }
}

fn run_capture_loop(
    receiver: Receiver,
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Vec<u8>>,
    audio_tx: Option<mpsc::Sender<NdiAudioChunk>>,
    preview: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
) {
    let mut frame_counter: u64 = 0;
    // Per-second telemetry so we can tell at a glance whether NDI
    // is delivering frames, whether they're being shipped to FFmpeg,
    // and whether the bounded channel is back-pressuring (which would
    // mean FFmpeg is the bottleneck).
    let mut window_frames: u32 = 0;
    let mut window_empty: u32 = 0;
    let mut window_errors: u32 = 0;
    let mut window_audio_sent: u32 = 0;
    let mut window_audio_dropped: u32 = 0;
    let mut window_start = Instant::now();
    while !stop.load(Ordering::Acquire) {
        match receiver.capture_video(Duration::from_millis(500)) {
            Ok(frame) if !frame.data.is_empty() => {
                // Sample one preview JPEG every PREVIEW_FRAME_STRIDE
                // frames. encode_jpeg is in the grafton-ndi crate and
                // uses the underlying NDI buffer in place — no second
                // copy. Failures get logged and skipped; the live
                // pipeline still gets the raw bytes via the channel.
                if frame_counter % PREVIEW_FRAME_STRIDE == 0 {
                    match frame.encode_jpeg(PREVIEW_JPEG_QUALITY) {
                        Ok(jpeg) => {
                            *preview.lock().unwrap() = Some(jpeg);
                        }
                        Err(err) => log::debug!("preview JPEG encode failed: {err}"),
                    }
                }
                frame_counter = frame_counter.wrapping_add(1);
                // Strip line-stride padding so FFmpeg's rawvideo
                // demuxer sees tightly-packed frames. VideoFrame's
                // Drop releases the NDI buffer at end-of-scope.
                let bpp = bytes_per_pixel(frame.pixel_format);
                let packed = pack_frame(&frame, bpp);
                match send_frame_or_stop(&tx, packed, &stop) {
                    SendOutcome::Sent => window_frames += 1,
                    SendOutcome::Stopped => {
                        log::info!(
                            "NDI capture: stop requested or channel closed (sent {frame_counter} frames)"
                        );
                        break;
                    }
                }
            }
            Ok(_) => {
                window_empty += 1;
            }
            Err(err) => {
                window_errors += 1;
                log::warn!("NDI capture_video error: {err}");
                thread::sleep(Duration::from_millis(50));
            }
        }

        // Audio drain — only fires when the streamer wants audio
        // (OMT-out enabled at start). Non-blocking with a 0ms timeout
        // so we never block the video loop on audio. Each video poll
        // pulls every audio frame the SDK has queued since last time.
        // try_send drops chunks quietly if the consumer is slow; OMT
        // audio is best-effort, not a hard requirement.
        if let Some(atx) = audio_tx.as_ref() {
            loop {
                match receiver.capture_audio_timeout(Duration::from_millis(0)) {
                    Ok(Some(af)) => match build_audio_chunk(&af) {
                        Some(chunk) => match atx.try_send(chunk) {
                            Ok(()) => window_audio_sent += 1,
                            Err(_) => window_audio_dropped += 1,
                        },
                        None => window_audio_dropped += 1,
                    },
                    Ok(None) => break, // queue empty
                    Err(err) => {
                        log::debug!("NDI capture_audio error: {err}");
                        break;
                    }
                }
            }
        }

        let now = Instant::now();
        if now.duration_since(window_start) >= Duration::from_secs(1) {
            log::info!(
                "NDI capture 1s: sent={} empty={} errors={} ch_cap_remaining={} \
                 audio_sent={} audio_dropped={}",
                window_frames,
                window_empty,
                window_errors,
                tx.capacity(),
                window_audio_sent,
                window_audio_dropped,
            );
            window_frames = 0;
            window_empty = 0;
            window_errors = 0;
            window_audio_sent = 0;
            window_audio_dropped = 0;
            window_start = now;
        }
    }
    log::info!("NDI capture thread exiting (total frames: {frame_counter})");
}

/// Convert a grafton-ndi `AudioFrame` into a transportable chunk.
/// Returns None if the frame has invalid layout (zero channels /
/// zero samples). The samples Vec is a planar f32 PCM copy — same
/// layout OMT expects (channel-major), so the OMT sender can pass it
/// straight through without re-interleaving.
fn build_audio_chunk(frame: &AudioFrame) -> Option<NdiAudioChunk> {
    if frame.num_channels <= 0 || frame.num_samples <= 0 {
        return None;
    }
    let samples = frame.data().to_vec();
    Some(NdiAudioChunk {
        samples,
        sample_rate: frame.sample_rate,
        num_channels: frame.num_channels,
    })
}

fn pix_fmt_for_ffmpeg(pf: PixelFormat) -> &'static str {
    use PixelFormat::*;
    match pf {
        BGRA | BGRX => "bgra",
        RGBA | RGBX => "rgba",
        UYVY => "uyvy422",
        UYVA => "uyvy422",
        other => {
            // Falling back to bgra here used to be silent. If the SDK
            // ever hands us a format the receiver options didn't ask
            // for (e.g. NV12, P216 from a hardware encoder) the wrong
            // pix_fmt token gets baked into the FFmpeg cmdline and
            // every byte in the rawvideo stream is misinterpreted.
            log::warn!(
                "NDI pix_fmt {other:?} not in mapping table — defaulting FFmpeg \
                 token to bgra; output may be corrupted"
            );
            "bgra"
        }
    }
}

/// Bytes per pixel for the formats we know about. Used to compute
/// the expected tightly-packed frame size.
fn bytes_per_pixel(pf: PixelFormat) -> usize {
    use PixelFormat::*;
    match pf {
        BGRA | BGRX | RGBA | RGBX => 4,
        UYVY | UYVA => 2,
        _ => 4,
    }
}

/// Stride-strip a grafton-ndi VideoFrame for FFmpeg's rawvideo demuxer.
/// Thin wrapper that pulls the right fields off `VideoFrame` and hands
/// them to the shared `frame_pack::pack_frame` (Phase B refactor —
/// same logic backs OMT capture too, see `omt_capture.rs`).
fn pack_frame(frame: &VideoFrame, bpp: usize) -> Vec<u8> {
    let width = frame.width.max(0) as usize;
    let height = frame.height.max(0) as usize;
    let line_stride = match frame.line_stride_or_size {
        LineStrideOrSize::LineStrideBytes(s) if s > 0 => Some(s as usize),
        // DataSizeBytes (compressed) or zero/negative stride — let
        // the shared helper passthrough.
        _ => None,
    };
    crate::frame_pack::pack_frame(&frame.data, width, height, bpp, line_stride)
}
