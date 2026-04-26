//! Pre-stream live preview backend.
//!
//! Spins up a separate NDI receiver (no FFmpeg, no mpsc to the
//! streamer) at full bandwidth so the user can see a source BEFORE
//! clicking Start Stream. Encodes JPEG snapshots into the same
//! `latest` slot the streaming path uses so the existing
//! /api/preview poll loop on the JS side just works — the JPEGs
//! coming through the wire don't care whether they came from the
//! preview backend or the live encoder.
//!
//! Handoff with [`crate::streamer::Streamer`]:
//! - On `Streamer::start()`, the streamer calls `Preview::stop()`
//!   FIRST so the NDI SDK handle isn't held twice (one preview +
//!   one streaming Receiver from the same process is wasteful and
//!   would race the JPEG slot anyway).
//! - `/api/preview` reads the streamer's current preview if it has
//!   one, falling back to this module's last JPEG. Smooth handoff
//!   when the streamer's full-bandwidth receiver takes over.
//!
//! Today: NDI sources only. Other source kinds (avfoundation, pipe,
//! test_pattern) get a "preview not yet supported" error from
//! `start()`. The next iteration adds an FFmpeg JPEG-snapshot
//! backend so any FFmpeg-decodable input can be previewed.

use crate::ndi_runtime;

use anyhow::{anyhow, Result};
use grafton_ndi::{
    Receiver, ReceiverBandwidth, ReceiverColorFormat, ReceiverOptions, NDI,
};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio::sync::Mutex as TokioMutex;

/// JPEG quality for preview snapshots. Same value the streaming-path
/// sampler uses — looks fine at the small Monitor pane size and keeps
/// the per-frame payload at tens of KB even at 1080p.
const PREVIEW_JPEG_QUALITY: u8 = 60;
/// Sample one JPEG every N captured frames. With ReceiverBandwidth::
/// Highest the source rate matches the sender's native (typically
/// 30/60 fps), so stride 15 -> ~2-4 fps preview — same cadence as
/// the streaming path's sampler, well above the visual flicker
/// floor, and inexpensive at JPEG encode time on M-series.
const PREVIEW_FRAME_STRIDE: u64 = 15;

#[derive(Debug, Clone, Serialize)]
pub struct PreviewStatus {
    pub active: bool,
    /// "ndi" today; "ffmpeg" once that backend lands.
    pub backend: Option<String>,
    pub source_name: Option<String>,
}

pub struct Preview {
    inner: TokioMutex<Inner>,
    /// Latest JPEG produced by the active backend. Shared with the
    /// capture thread; cloned out by `latest_jpeg()` for the HTTP
    /// handler. Persisted across `stop()` so the streamer's start-up
    /// gap (between preview-stop and streamer's first JPEG) doesn't
    /// flicker the UI's preview pane.
    latest: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
}

struct Inner {
    backend: Option<NdiBackend>,
}

struct NdiBackend {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    source_name: String,
}

impl Drop for NdiBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Preview {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: TokioMutex::new(Inner { backend: None }),
            latest: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    pub async fn status(&self) -> PreviewStatus {
        let inner = self.inner.lock().await;
        match inner.backend.as_ref() {
            Some(b) => PreviewStatus {
                active: true,
                backend: Some("ndi".into()),
                source_name: Some(b.source_name.clone()),
            },
            None => PreviewStatus {
                active: false,
                backend: None,
                source_name: None,
            },
        }
    }

    /// Returns the latest JPEG, if any. Cheap clone of the bytes.
    pub fn latest_jpeg(&self) -> Option<Vec<u8>> {
        self.latest.lock().unwrap().clone()
    }

    /// Spin up a full-bandwidth NDI preview for the named sender.
    /// Idempotent re: an already-running backend — stops the prior
    /// one before starting the new one. Returns once the receiver is
    /// constructed; the first JPEG arrives ~250-1000ms later
    /// depending on the sender.
    pub async fn start_ndi(&self, source_name: &str) -> Result<()> {
        // Tear down any prior backend before claiming a new SDK
        // handle. Without this, double-start would leave two threads
        // racing the latest_jpeg slot AND would hold two NDI
        // Receivers per source, doubling the network bandwidth.
        self.stop_inner(false).await;

        let source = ndi_runtime::find_source_by_name(source_name)
            .ok_or_else(|| anyhow!("NDI source not found: {source_name:?}. Refresh discovery."))?;

        // Each preview gets its own NDI handle. Discovery uses its
        // own handle in `ndi_runtime`; receivers and finders are
        // independent SDK objects so this is safe.
        let ndi = NDI::new()?;
        let receiver = Receiver::new(
            &ndi,
            &ReceiverOptions::builder(source)
                .color(ReceiverColorFormat::BGRX_BGRA)
                // Highest-bandwidth — the sender's full-quality
                // stream, same as the streaming path's receiver.
                // Lowest (the NDI proxy stream, ~640x360 at reduced
                // fps) is the obvious bandwidth-saving choice but
                // produces a preview ugly enough that users assume
                // their camera is broken — the whole point of this
                // button is to verify the source LOOKS RIGHT before
                // they commit to a real outbound stream. On a LAN
                // NDI source the upload cost is negligible; we only
                // sample one frame per PREVIEW_FRAME_STRIDE for the
                // JPEG encode so our local CPU cost stays bounded.
                .bandwidth(ReceiverBandwidth::Highest)
                .build(),
        )?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_w = stop.clone();
        let latest_w = self.latest.clone();
        let name_for_thread = source_name.to_string();
        let handle = thread::Builder::new()
            .name("ndi-preview".into())
            .spawn(move || {
                let _ndi = ndi; // pin handle for receiver lifetime
                run_preview_loop(receiver, stop_w, latest_w);
                log::info!("NDI preview thread exited (source: {name_for_thread:?})");
            })?;

        let mut inner = self.inner.lock().await;
        inner.backend = Some(NdiBackend {
            stop,
            handle: Some(handle),
            source_name: source_name.to_string(),
        });
        log::info!("NDI preview started for {source_name:?}");
        Ok(())
    }

    /// Stop any active preview backend. Clears the latest JPEG so the
    /// UI immediately stops painting frames if no streamer is also
    /// running. The streamer's own start path uses the
    /// `stop_for_streamer` shortcut to keep the latest JPEG visible
    /// during the brief gap before its full-bandwidth receiver
    /// produces its first frame.
    pub async fn stop(&self) {
        self.stop_inner(true).await;
    }

    /// Tear down the backend but keep the last JPEG visible — used by
    /// the streamer to avoid an empty-pane flicker during the
    /// preview→stream handoff. The streamer's own JPEG sampler will
    /// overwrite the slot within 1-2 frames once the SRT/RTMP output
    /// connects.
    pub async fn stop_for_streamer(&self) {
        self.stop_inner(false).await;
    }

    async fn stop_inner(&self, clear_latest: bool) {
        let mut inner = self.inner.lock().await;
        if let Some(backend) = inner.backend.take() {
            // Drop runs the AtomicBool flip + thread join.
            drop(backend);
        }
        if clear_latest {
            *self.latest.lock().unwrap() = None;
        }
    }
}

fn run_preview_loop(
    receiver: Receiver,
    stop: Arc<AtomicBool>,
    latest: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
) {
    let mut frame_counter: u64 = 0;
    while !stop.load(Ordering::Acquire) {
        match receiver.capture_video(Duration::from_millis(500)) {
            Ok(frame) if !frame.data.is_empty() => {
                if frame_counter % PREVIEW_FRAME_STRIDE == 0 {
                    match frame.encode_jpeg(PREVIEW_JPEG_QUALITY) {
                        Ok(jpeg) => {
                            *latest.lock().unwrap() = Some(jpeg);
                        }
                        Err(err) => log::debug!("preview JPEG encode failed: {err}"),
                    }
                }
                frame_counter = frame_counter.wrapping_add(1);
            }
            // Empty status frames are normal during sender warm-up;
            // just spin again. Errors are logged and we back off
            // briefly so a misbehaving sender doesn't pin a CPU.
            Ok(_) => continue,
            Err(err) => {
                log::warn!("NDI preview capture_video error: {err}");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}
