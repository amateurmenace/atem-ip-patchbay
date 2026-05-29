//! OMT sender — publishes captured frames as an OMT source on the
//! local network so OMT-aware switchers / multiviewers can pick it up
//! while we're simultaneously streaming to an ATEM via SRT.
//!
//! alpha.9 scope: frame-tee at the source. When the user enables OMT
//! output AND the source is NDI (already raw frames), the streamer's
//! writer task fans out — one Vec<u8> goes to FFmpeg's stdin (existing
//! ATEM-bound encode path), a clone goes to `OmtSender::feed_frame`.
//! Other source types (AVF webcam, pipe / RTSP, SRT/RTMP relay) need
//! FFmpeg-tee plumbing to expose raw frames; that lands in alpha.10.
//!
//! alpha.12 adds `feed_audio_frame` — OMT audio publishing for NDI
//! sources. libomt doesn't expose audio-specific setters on its
//! `OmtMediaFrame` (only video setters); we reach through
//! `as_raw_mut()` to set the C struct's `SampleRate`, `Channels`,
//! `SamplesPerChannel` fields directly. The audio codec is FPA1
//! (32-bit floating point planar) which is the only audio codec OMT
//! defines, so the layout matches grafton-ndi's planar `AudioFrame`
//! bytes exactly — no re-interleaving needed.
//!
//! Like omt_runtime + omt_capture, this whole module compiles to a
//! no-op when the `omt` cargo feature is off — `start_for_format`
//! returns an error stating that OMT support isn't compiled in. The
//! streamer's tee branch only fires when `omt_output_enabled` AND the
//! sender successfully starts, so feature-off builds silently fall
//! through to the regular single-output path.

use anyhow::{anyhow, Result};

#[cfg(feature = "omt")]
use std::sync::{Arc, Mutex};

/// Active OMT publisher. Holds the libomt OmtSend handle plus a
/// pre-built MediaFrame template we mutate in place each tick.
/// Drop sends `OMT_SendDestroy` so the network announcement
/// disappears cleanly.
pub struct OmtSender {
    #[cfg(feature = "omt")]
    inner: Arc<Mutex<OmtSenderInner>>,
    #[cfg(not(feature = "omt"))]
    _phantom: std::marker::PhantomData<()>,
}

#[cfg(feature = "omt")]
struct OmtSenderInner {
    send: libomt::OmtSend,
    width: i32,
    height: i32,
    /// FFmpeg-side pixel format token, used to pick the matching
    /// libomt OmtCodec for outgoing frames. Currently "bgra" or
    /// "uyvy422" — set at construction to whatever the source feed
    /// uses, kept stable for the lifetime of the sender.
    pix_fmt: &'static str,
}

impl OmtSender {
    /// Start an OMT publisher with the given name. The `width`,
    /// `height`, `pix_fmt` describe the source format coming in via
    /// `feed_frame`. quality defaults to High; future revision can
    /// expose this to the user.
    pub fn start_for_format(
        name: &str,
        width: u32,
        height: u32,
        pix_fmt: &'static str,
    ) -> Result<Self> {
        #[cfg(not(feature = "omt"))]
        {
            let _ = (name, width, height, pix_fmt);
            Err(anyhow!(
                "OMT support not compiled in. Rebuild with `--features omt` and ensure \
                 libomt.dylib is on the linker search path."
            ))
        }

        #[cfg(feature = "omt")]
        {
            use libomt::{OmtQuality, OmtSend};
            let send = OmtSend::new(name, OmtQuality::High)
                .map_err(|e| anyhow!("OmtSend::new({name:?}) failed: {e:?}"))?;
            log::info!(
                "OMT sender started: name={name:?} {width}x{height} pix_fmt={pix_fmt}"
            );
            Ok(OmtSender {
                inner: Arc::new(Mutex::new(OmtSenderInner {
                    send,
                    width: width as i32,
                    height: height as i32,
                    pix_fmt,
                })),
            })
        }
    }

    /// Feed one frame of raw video data. Format must match what was
    /// passed to `start_for_format` (width, height, pix_fmt). Returns
    /// the libomt send return code (negative on error). The data
    /// buffer is borrowed for the duration of the call; libomt copies
    /// internally before queueing.
    pub fn feed_frame(&self, data: &[u8]) -> Result<i32> {
        #[cfg(not(feature = "omt"))]
        {
            let _ = data;
            Err(anyhow!("OMT support not compiled in"))
        }

        #[cfg(feature = "omt")]
        {
            use libomt::{OmtCodec, OmtFrameType, OmtMediaFrame, OmtVideoFlags};
            let inner = self.inner.lock().unwrap();
            let mut frame = OmtMediaFrame::new();
            frame.set_type(OmtFrameType::Video);
            // Match the source codec to the corresponding libomt enum.
            // BGRA stays BGRA; uyvy422 maps to libomt's UYVY. Anything
            // else is a misuse — we'd have caught it at start_for_format.
            let codec = match inner.pix_fmt {
                "bgra" => OmtCodec::Bgra,
                "uyvy422" => OmtCodec::Uyvy,
                other => {
                    return Err(anyhow!(
                        "unsupported pix_fmt for OMT send: {other:?} \
                         (expected bgra or uyvy422)"
                    ));
                }
            };
            frame.set_codec(codec);
            frame.set_width(inner.width);
            frame.set_height(inner.height);
            // Stride: bgra=4 bytes/pixel, uyvy422=2 bytes/pixel. Tight
            // packing because frame_pack stripped any padding before
            // we got here.
            let stride = match inner.pix_fmt {
                "bgra" => inner.width * 4,
                "uyvy422" => inner.width * 2,
                _ => unreachable!(), // checked above
            };
            frame.set_stride(stride);
            frame.set_flags(OmtVideoFlags::None);
            // SAFETY: data borrow outlives the OMT_Send call below.
            // libomt copies internally before returning.
            frame.set_data(data.as_ptr() as *mut std::ffi::c_void, data.len() as i32);
            let rc = inner.send.send(&mut frame);
            Ok(rc)
        }
    }

    /// Feed one chunk of OMT audio. `samples` is planar 32-bit
    /// floating point PCM (channel-major: all of ch0's samples, then
    /// all of ch1's, ...). `num_channels` and `sample_rate` describe
    /// the chunk's layout. Length of `samples` must equal
    /// `num_channels * (samples.len() / num_channels)`, i.e. the
    /// caller has already laid out exactly N channels of M samples.
    ///
    /// Returns the libomt send return code (negative on error). When
    /// the `omt` feature is off, returns an error instead of a no-op
    /// so misuse is loud.
    pub fn feed_audio_frame(
        &self,
        samples: &[f32],
        num_channels: i32,
        sample_rate: i32,
    ) -> Result<i32> {
        #[cfg(not(feature = "omt"))]
        {
            let _ = (samples, num_channels, sample_rate);
            Err(anyhow!("OMT support not compiled in"))
        }

        #[cfg(feature = "omt")]
        {
            use libomt::{OmtCodec, OmtFrameType, OmtMediaFrame};
            if num_channels <= 0 {
                return Err(anyhow!("num_channels must be > 0 (got {num_channels})"));
            }
            let total = samples.len() as i32;
            if total % num_channels != 0 {
                return Err(anyhow!(
                    "samples.len() ({total}) is not a multiple of num_channels ({num_channels})"
                ));
            }
            let samples_per_channel = total / num_channels;
            if samples_per_channel == 0 {
                return Err(anyhow!("empty audio chunk"));
            }
            let inner = self.inner.lock().unwrap();
            let mut frame = OmtMediaFrame::new();
            frame.set_type(OmtFrameType::Audio);
            // FPA1 = the only audio codec OMT defines (32-bit floating
            // point planar). libomt's enum maps to the C
            // OMTCodec_FPA1 constant.
            frame.set_codec(OmtCodec::Fpa1);
            // libomt 0.1.3 has no audio-specific setters — reach
            // through to the C struct directly. Field names follow
            // bindgen's PascalCase preservation of the libomt.h
            // OMTMediaFrame definition (verified against the
            // vendor/libomt/libomt.h source).
            let raw = frame.as_raw_mut();
            unsafe {
                (*raw).SampleRate = sample_rate;
                (*raw).Channels = num_channels;
                (*raw).SamplesPerChannel = samples_per_channel;
            }
            // DataLength = SamplesPerChannel * Channels * 4 bytes,
            // which equals samples.len() * 4 since the planar buffer
            // is already laid out contiguously.
            let len_bytes = (samples.len() * std::mem::size_of::<f32>()) as i32;
            frame.set_data(samples.as_ptr() as *mut std::ffi::c_void, len_bytes);
            let rc = inner.send.send(&mut frame);
            Ok(rc)
        }
    }

    /// Number of currently-connected OMT receivers. 0 means "we're
    /// publishing but no one's listening" — useful for UI feedback
    /// ("OMT output: 2 connected").
    pub fn connection_count(&self) -> i32 {
        #[cfg(not(feature = "omt"))]
        {
            0
        }
        #[cfg(feature = "omt")]
        {
            self.inner.lock().unwrap().send.connections()
        }
    }
}

#[cfg(not(feature = "omt"))]
impl OmtSender {
    pub fn _new_disabled() -> Self {
        OmtSender {
            _phantom: std::marker::PhantomData,
        }
    }
}
