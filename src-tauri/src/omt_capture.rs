//! OMT receiver thread — parallel to `ndi_capture.rs`.
//!
//! Captures frames from an OMT sender via the libomt crate, strips
//! per-row stride via the shared `frame_pack` module (same as NDI),
//! and pumps them through a Tokio mpsc channel to FFmpeg's stdin.
//!
//! Same hand-off contract as NdiCapture:
//! 1. [`OmtCapture::start_and_probe_format`] creates the OMT receiver,
//!    blocks for the first non-empty video frame, and returns the
//!    format + an `mpsc::Receiver` channel.
//! 2. The streamer launches FFmpeg with stdin piped, spawns a writer
//!    task that drains the channel into stdin.
//! 3. On stop, [`OmtCapture::stop`] flips an atomic; the thread
//!    exits, drops the channel sender, FFmpeg's stdin closes, FFmpeg
//!    shuts down cleanly.
//!
//! The whole module compiles to a no-op when the `omt` cargo feature
//! is off — `start_and_probe_format` returns an error stating that
//! OMT support isn't compiled in. This keeps the streamer's source-
//! resolution branch ergonomic (it doesn't need #[cfg] gates) and
//! gives users a clear path forward (rebuild with --features omt).

use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::mpsc;

#[cfg(feature = "omt")]
const FRAME_CHANNEL_CAPACITY: usize = 64;

/// What the streamer needs to build FFmpeg input args. Same shape as
/// `NdiVideoFormat` (intentionally — the streamer's
/// `build_ffmpeg_cmd_for_raw_pipe` accepts either via a generic
/// internal struct).
#[derive(Debug, Clone, Copy)]
pub struct OmtVideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    /// FFmpeg `-pix_fmt` token. Most OMT senders we'll see use BGRA
    /// or UYVY; libomt's `OmtPreferredVideoFormat::UyvyOrBgra` lets
    /// the SDK pick the cheaper transform.
    pub ffmpeg_pix_fmt: &'static str,
}

impl OmtVideoFormat {
    /// Frame rate as a single integer for FFmpeg's `-r` arg. Currently
    /// only used internally by `build_ffmpeg_cmd_for_omt` when bridging
    /// to the NDI builder; kept public so future callers can read the
    /// rate without recomputing fps_num/fps_den themselves.
    #[allow(dead_code)]
    pub fn fps(&self) -> u32 {
        if self.fps_den == 0 {
            30
        } else {
            ((self.fps_num as f64) / (self.fps_den as f64)).round() as u32
        }
    }
}

pub struct OmtCapture {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    /// Latest preview frame as JPEG bytes — populated by the capture
    /// loop on every Nth frame; read by `/api/preview`.
    preview: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
}

impl OmtCapture {
    pub fn latest_preview(&self) -> Option<Vec<u8>> {
        self.preview.lock().unwrap().clone()
    }

    /// Start capture from the named OMT source address (libomt's
    /// `"HOSTNAME (Source Name)"` discovery string). Blocks up to
    /// `format_timeout` for the first frame so the streamer can
    /// build a matching FFmpeg command.
    pub fn start_and_probe_format(
        address: String,
        format_timeout: Duration,
    ) -> Result<(OmtVideoFormat, Self, mpsc::Receiver<Vec<u8>>)> {
        #[cfg(not(feature = "omt"))]
        {
            // Suppress unused-arg warnings.
            let _ = (address, format_timeout);
            Err(anyhow!(
                "OMT support not compiled in. Rebuild with `cargo tauri build --features omt` \
                 and ensure libomt.dylib is on the linker search path."
            ))
        }

        #[cfg(feature = "omt")]
        {
            use libomt::{
                OmtFrameType, OmtPreferredVideoFormat, OmtReceive, OmtReceiveFlags,
            };

            // Receiver: ask for video frames, prefer UYVY-or-BGRA so
            // the SDK can pick the format that minimises decode work
            // (some senders emit UYVY natively at lower bandwidth).
            let recv = OmtReceive::new(
                &address,
                OmtFrameType::Video,
                OmtPreferredVideoFormat::UyvyOrBgra,
                OmtReceiveFlags::None,
            )
            .map_err(|e| anyhow!("OMT receiver create failed for {address:?}: {e:?}"))?;

            // Probe — block for up to format_timeout waiting for the
            // first real video frame. OMT sometimes sends metadata-only
            // frames first, which we skip via the frame_type check.
            let deadline = std::time::Instant::now() + format_timeout;
            let mut probe_fmt: Option<OmtVideoFormat> = None;
            let mut first_packed: Option<Vec<u8>> = None;

            while std::time::Instant::now() < deadline {
                let Some(frame) = recv.receive(OmtFrameType::Video, 500) else {
                    continue;
                };
                if frame.frame_type() != OmtFrameType::Video {
                    continue;
                }
                let width = frame.width().max(0) as u32;
                let height = frame.height().max(0) as u32;
                let data = frame.data();
                if width == 0 || height == 0 || data.is_empty() {
                    continue;
                }

                // libomt-rs 0.1.3 doesn't expose the frame's pixel
                // format or stride via OmtFrameRef — only data/width/
                // height/timestamp/frame_type. We requested
                // UyvyOrBgra so it's one of those; infer from the
                // data size relative to width*height.
                let len = data.len();
                let (pix_fmt, bpp) = if len >= (width as usize) * (height as usize) * 4 {
                    ("bgra", 4)
                } else if len >= (width as usize) * (height as usize) * 2 {
                    ("uyvy422", 2)
                } else {
                    log::warn!(
                        "OMT frame data smaller than expected for either bgra or uyvy: \
                         {width}x{height} data_len={len} — defaulting to bgra"
                    );
                    ("bgra", 4)
                };

                // Frame rate isn't surfaced by libomt-rs 0.1.3's
                // OmtFrameRef. Assume 30 fps for the FFmpeg input
                // timestamps; the output filter chain forces the
                // configured video_mode rate downstream so this is
                // cosmetic. Future libomt-rs versions exposing
                // FrameRateN/D will let us replace the assumption.
                let fmt = OmtVideoFormat {
                    width,
                    height,
                    fps_num: 30,
                    fps_den: 1,
                    ffmpeg_pix_fmt: pix_fmt,
                };

                // Stride-strip via the shared frame_pack module. We
                // don't have stride info from libomt-rs, so pass None
                // — frame_pack returns the data as-is. Most OMT
                // senders emit tight frames; if we encounter a padded
                // sender we'll need to upstream a stride accessor.
                let packed = crate::frame_pack::pack_frame(data, width as usize, height as usize, bpp, None);
                log::info!(
                    "OMT capture probed: {}x{} pix_fmt={pix_fmt} bpp={bpp} \
                     data_len={len} packed_len={}",
                    width,
                    height,
                    packed.len(),
                );
                probe_fmt = Some(fmt);
                first_packed = Some(packed);
                break;
            }

            let format = probe_fmt.ok_or_else(|| {
                anyhow!(
                    "no OMT video frame received within {:?} — is the sender actually publishing?",
                    format_timeout
                )
            })?;

            let (tx, rx) = mpsc::channel::<Vec<u8>>(FRAME_CHANNEL_CAPACITY);
            if let Some(packed) = first_packed {
                let _ = tx.try_send(packed);
            }

            let stop = Arc::new(AtomicBool::new(false));
            let preview = Arc::new(std::sync::Mutex::new(None));
            let stop_w = stop.clone();
            let preview_w = preview.clone();
            let bpp_w = if format.ffmpeg_pix_fmt == "bgra" { 4 } else { 2 };
            let handle = std::thread::Builder::new()
                .name("omt-capture".into())
                .spawn(move || {
                    run_capture_loop(recv, format, bpp_w, stop_w, tx, preview_w);
                })?;

            Ok((
                format,
                OmtCapture {
                    stop,
                    handle: Some(handle),
                    preview,
                },
                rx,
            ))
        }
    }

    /// Signal the capture thread to exit; drops the channel sender,
    /// which closes FFmpeg's stdin and triggers clean shutdown.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for OmtCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(feature = "omt")]
fn run_capture_loop(
    recv: libomt::OmtReceive,
    format: OmtVideoFormat,
    bpp: usize,
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Vec<u8>>,
    preview: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
) {
    use libomt::OmtFrameType;

    let mut frame_counter: u64 = 0;
    let mut last_telemetry = std::time::Instant::now();
    let mut sent_in_window: u64 = 0;
    let mut empty_in_window: u64 = 0;

    while !stop.load(Ordering::Acquire) {
        let Some(frame) = recv.receive(OmtFrameType::Video, 500) else {
            empty_in_window += 1;
            continue;
        };
        if frame.frame_type() != OmtFrameType::Video {
            continue;
        }
        let data = frame.data();
        if data.is_empty() {
            empty_in_window += 1;
            continue;
        }
        let width = frame.width().max(0) as u32;
        let height = frame.height().max(0) as u32;
        if width != format.width || height != format.height {
            // Mid-stream resolution change. Skip — FFmpeg's rawvideo
            // demuxer can't handle a switch, and we'd rather drop
            // frames than corrupt the output. A future enhancement
            // could restart the capture pipeline on resolution change.
            log::warn!(
                "OMT frame size changed: was {}x{}, got {}x{} — skipping",
                format.width,
                format.height,
                width,
                height
            );
            continue;
        }
        let packed =
            crate::frame_pack::pack_frame(data, width as usize, height as usize, bpp, None);

        // Sample one preview JPEG every PREVIEW_FRAME_STRIDE frames.
        // Mirror NDI's pattern for parity with /api/preview.
        if frame_counter % 15 == 0 {
            if let Some(jpeg) = encode_preview_jpeg(&packed, width, height, bpp) {
                *preview.lock().unwrap() = Some(jpeg);
            }
        }

        if tx.blocking_send(packed).is_err() {
            // Channel closed — streamer's writer task exited.
            log::info!("OMT capture: channel closed, exiting capture loop");
            break;
        }
        sent_in_window += 1;
        frame_counter += 1;

        if last_telemetry.elapsed() >= Duration::from_secs(1) {
            log::info!(
                "OMT capture telemetry: sent={sent_in_window} empty={empty_in_window} \
                 channel_remaining_capacity={}",
                tx.capacity()
            );
            sent_in_window = 0;
            empty_in_window = 0;
            last_telemetry = std::time::Instant::now();
        }
    }
    log::info!("OMT capture thread exiting (total frames: {frame_counter})");
}

#[cfg(feature = "omt")]
fn encode_preview_jpeg(packed: &[u8], width: u32, height: u32, bpp: usize) -> Option<Vec<u8>> {
    // Use the `image` crate via grafton-ndi's transitive dep if
    // available, or skip preview when not. For alpha.9 we punt on
    // OMT preview JPEGs to keep the surface minimal — the tile
    // gallery doesn't render previews; preview pane is NDI-only.
    let _ = (packed, width, height, bpp);
    None
}
