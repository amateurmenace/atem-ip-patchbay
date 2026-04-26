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
//! Audio is intentionally not piped here — Phase 4 routes audio
//! through FFmpeg's `lavfi anullsrc` (silent). NDI audio support
//! lands in a follow-up.

use anyhow::{anyhow, Result};
use grafton_ndi::{
    PixelFormat, Receiver, ReceiverBandwidth, ReceiverColorFormat, ReceiverOptions, Source, NDI,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio::sync::mpsc;

/// Channel buffer size in frames. At 1080p60 BGRA each frame is
/// ~8.3 MB; 64 frames = ~530 MB worst-case if FFmpeg fully stalls,
/// which we'd never want to actually hit. In practice FFmpeg drains
/// the channel within a frame or two, so the buffer just absorbs the
/// occasional encoder spike.
const FRAME_CHANNEL_CAPACITY: usize = 64;

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
    pub fn start_and_probe_format(
        source: Source,
        format_timeout: Duration,
    ) -> Result<(NdiVideoFormat, Self, mpsc::Receiver<Vec<u8>>)> {
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
        log::info!(
            "NDI capture probed: {}x{}@{}/{} pix_fmt={}",
            format.width,
            format.height,
            format.fps_num,
            format.fps_den,
            format.ffmpeg_pix_fmt,
        );

        let (tx, rx) = mpsc::channel::<Vec<u8>>(FRAME_CHANNEL_CAPACITY);
        // Send the buffered first frame so the encoder gets a clean
        // start without a 1-frame stutter.
        let _ = tx.try_send(frame.data.clone());

        let stop = Arc::new(AtomicBool::new(false));
        let preview = Arc::new(std::sync::Mutex::new(None));
        let stop_w = stop.clone();
        let preview_w = preview.clone();
        let handle = thread::Builder::new()
            .name("ndi-capture".into())
            .spawn(move || {
                let _ndi = ndi; // keep handle alive for the receiver's lifetime
                run_capture_loop(receiver, stop_w, tx, preview_w);
            })?;

        Ok((
            format,
            NdiCapture {
                stop,
                handle: Some(handle),
                preview,
            },
            rx,
        ))
    }

    /// Signal the capture thread to exit and wait for it to drain.
    /// Drops the channel sender, which causes the streamer's writer
    /// task to see the channel close and shut down FFmpeg's stdin.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for NdiCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_capture_loop(
    receiver: Receiver,
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Vec<u8>>,
    preview: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
) {
    let mut frame_counter: u64 = 0;
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
                // VideoFrame implements Drop (releases the NDI buffer)
                // so we clone the bytes out before sending.
                if tx.blocking_send(frame.data.clone()).is_err() {
                    break;
                }
            }
            Ok(_) => {} // empty heartbeat, ignore
            Err(err) => {
                log::warn!("NDI capture_video error: {err}");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
    log::info!("NDI capture thread exiting");
}

fn pix_fmt_for_ffmpeg(pf: PixelFormat) -> &'static str {
    use PixelFormat::*;
    match pf {
        BGRA | BGRX => "bgra",
        RGBA | RGBX => "rgba",
        UYVY => "uyvy422",
        UYVA => "uyvy422",
        _ => "bgra",
    }
}
