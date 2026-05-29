//! NDI → FFmpeg audio bridge — alpha.42.
//!
//! Today's gap: NDI sources send audio just fine, but when we hand
//! the NDI feed to FFmpeg via stdin (`pipe:0` for raw video) there's
//! no clean cross-platform way to ALSO hand FFmpeg raw audio. The
//! existing build_ffmpeg_cmd_for_ndi() falls back to a lavfi
//! `anullsrc` (silent) input for audio whenever audio_mode is not
//! "custom" — operator's "Auto" pick produces a stream with NDI
//! video and SILENCE going to ATEM or DeckLink. Production-blocking.
//!
//! Why TCP loopback: the obvious-looking alternatives don't fit.
//!   - Stdin already carries raw video; we'd have to interleave
//!     audio + video into a single container (nut/matroska), which
//!     means building the container in-process. Heavy.
//!   - FD-based pipes (fd 3 etc.) need `pre_exec` on Unix and don't
//!     exist on Windows. Cross-platform = headache.
//!   - libndi_newtek as an FFmpeg input would solve it but our
//!     FFmpeg sidecar doesn't (and can't easily) build that demuxer.
//!
//! TCP on 127.0.0.1 works on every supported platform with the same
//! exact syntax: FFmpeg connects to us via `-i tcp://127.0.0.1:N`,
//! we serve interleaved s16le PCM bytes as they arrive from the NDI
//! capture loop. Loopback overhead is negligible (a memcpy in
//! kernel-space).
//!
//! Format conversion: NDI's audio is planar float32 (channel-major,
//! L L L L L L … then R R R R R R …). FFmpeg's `-f s16le` expects
//! interleaved signed-16 (L R L R L R …). Both straightforward to
//! convert; we do it once per audio chunk on the way to the socket.

use anyhow::Result;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// Audio format we serve to FFmpeg. NDI is almost always 48000 Hz
/// stereo in practice; baking these values into the bridge keeps the
/// FFmpeg input args static (no per-source resampling decisions).
/// If a non-48k source shows up, FFmpeg's `-ar` mismatch will play
/// the audio at the wrong speed and we'll see the issue immediately —
/// at which point we can plumb the sample_rate through. Until then,
/// the simpler design wins.
pub const BRIDGE_SAMPLE_RATE: u32 = 48000;
pub const BRIDGE_CHANNELS: u32 = 2;

#[derive(Clone)]
pub struct AudioBridge {
    inner: Arc<Inner>,
}

struct Inner {
    port: u16,
    tx: mpsc::Sender<Vec<u8>>,
}

impl AudioBridge {
    /// Bind a TCP listener on a random local port and spawn the
    /// accept+write loop. Returns as soon as the listener is bound
    /// (synchronous) — the accept happens asynchronously when FFmpeg
    /// connects. The caller can immediately use `.ffmpeg_input_args()`
    /// to construct the FFmpeg invocation that'll connect back.
    pub async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        // Buffer 128 chunks ≈ 2-3 seconds of audio depending on chunk
        // size. Enough to absorb a brief FFmpeg startup stall without
        // dropping samples; not so much that a stuck FFmpeg piles up
        // unbounded memory.
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(128);

        tokio::spawn(async move {
            // FFmpeg connects exactly once — when the input is opened
            // at process startup. If FFmpeg dies + reconnects (via
            // alpha.30's supervisor) it's a NEW FFmpeg process with
            // a NEW AudioBridge (constructed fresh in run_one_attempt).
            // So a single accept here is correct.
            let (mut socket, peer) = match listener.accept().await {
                Ok(c) => c,
                Err(e) => {
                    log::warn!(
                        "audio_bridge :{port} accept failed: {e} — FFmpeg \
                         won't get NDI audio for this attempt"
                    );
                    return;
                }
            };
            log::info!("audio_bridge :{port} accepted FFmpeg from {peer}");
            // Disable Nagle so audio bytes get to FFmpeg with minimal
            // batching. Loopback should already be ~0 latency but TCP
            // can buffer up to ~1500 bytes waiting for an ack; with
            // ~250-byte audio chunks at 50ms intervals that's the
            // difference between "live" and "noticeably delayed".
            let _ = socket.set_nodelay(true);
            while let Some(bytes) = rx.recv().await {
                if let Err(e) = socket.write_all(&bytes).await {
                    log::warn!("audio_bridge :{port} write failed: {e} — closing");
                    break;
                }
            }
            // mpsc closed (Streamer dropped the bridge) or write error
            // — either way, let FFmpeg see EOF on its side so its
            // audio decoder doesn't hang. Explicit shutdown matches
            // the natural lifecycle.
            let _ = socket.shutdown().await;
            log::info!("audio_bridge :{port} writer task ending");
        });

        Ok(AudioBridge {
            inner: Arc::new(Inner { port, tx }),
        })
    }

    pub fn port(&self) -> u16 {
        self.inner.port
    }

    /// Send a chunk of s16le-interleaved bytes to FFmpeg. Returns
    /// `true` if the chunk was queued for the bridge writer, `false`
    /// if the writer's receiver is gone (FFmpeg disconnected, bridge
    /// task exited). The caller logs failures so a dead bridge
    /// surfaces in the operator log instead of silently dropping
    /// audio forever (the alpha.48 "audio died after a few minutes"
    /// bug surfaced exactly because send failures were swallowed).
    pub async fn send(&self, bytes: Vec<u8>) -> bool {
        self.inner.tx.send(bytes).await.is_ok()
    }

    /// FFmpeg input args to splice into the command. Pair with the
    /// matching map: `-map 0:v -map 1:a` (when this is the second
    /// input alongside the raw video stdin on input 0).
    ///
    /// alpha.49: `-use_wallclock_as_timestamps 1` makes FFmpeg stamp
    /// the audio frames with the receiver's wallclock instead of
    /// inferring time from the byte count alone. Without this,
    /// FFmpeg derives audio timestamps purely from sample count *
    /// (1/48000 Hz), which assumes our send rate matches the
    /// nominal sample rate EXACTLY. If NDI's source clock is even
    /// 50 ppm off the DeckLink card's clock (which is normal —
    /// they're different crystals), the cumulative drift over 5-10
    /// minutes is enough for FFmpeg's a/v sync logic to start
    /// dropping audio chunks, then go silent entirely. Wallclock
    /// stamping combined with aresample=async in build_audio_filter
    /// lets FFmpeg's resampler absorb the drift continuously.
    ///
    /// alpha.60: `use_wallclock` is now caller-controlled. Wallclock
    /// stamping is right for the DeckLink path (the drift remedy above),
    /// but on the SRT/MPEG-TS path it puts the audio on a real-time
    /// timeline while the video (rawvideo pipe) is 0-based frame time —
    /// the mismatched PTS breaks the program PCR so the ATEM can't
    /// present the video (operator-confirmed: NDI→ATEM was black with the
    /// bridge; video appeared the instant audio was set to Silent). So
    /// callers pass `false` for SRT/RTMP (audio PTS from sample count,
    /// 0-based, aligned with the video) and `true` only for DeckLink.
    pub fn ffmpeg_input_args(&self, use_wallclock: bool) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        if use_wallclock {
            args.push("-use_wallclock_as_timestamps".into());
            args.push("1".into());
        }
        args.extend([
            "-f".into(),
            "s16le".into(),
            "-ar".into(),
            BRIDGE_SAMPLE_RATE.to_string(),
            "-ac".into(),
            BRIDGE_CHANNELS.to_string(),
            "-channel_layout".into(),
            "stereo".into(),
            "-i".into(),
            format!("tcp://127.0.0.1:{}?listen=0", self.inner.port),
        ]);
        args
    }
}

/// Convert NDI's planar float32 audio chunk to FFmpeg-ready s16le
/// interleaved bytes.
///
/// Layout details: NDI delivers `samples_per_channel * num_channels`
/// float32 values where channel C's samples occupy positions
/// `[C * samples_per_channel .. (C+1) * samples_per_channel]`. The
/// scalar values are normalized to [-1.0, 1.0]; we scale to the i16
/// range with clamp-on-overflow so a clipping signal doesn't
/// wrap-around to silence (which sounds AWFUL — i16 underflow plays
/// as a static-pop pattern).
///
/// Output bytes: L_s0 L_s0 R_s0 R_s0 L_s1 L_s1 R_s1 R_s1 … (2-byte
/// little-endian s16 per sample, channel-interleaved by sample
/// index). Matches what `-f s16le -ar 48000 -ac 2` expects.
///
/// Channel-count mismatch: if the NDI source advertises a different
/// channel count than BRIDGE_CHANNELS (2), we fold by taking the
/// first BRIDGE_CHANNELS channels (mono → mono+mute(zero), 4ch →
/// L+R only). Operators wanting Dante-style multi-channel routing
/// already pick "custom" mode (DSHOW/AVF) which goes through the
/// separate per-device path — this auto path is for the common
/// case of "NDI source's own stereo".
pub fn ndi_planar_f32_to_s16le_interleaved(
    planar_samples: &[f32],
    src_num_channels: i32,
) -> Vec<u8> {
    let src_channels = src_num_channels.max(1) as usize;
    let total = planar_samples.len();
    if total == 0 || src_channels == 0 {
        return Vec::new();
    }
    let samples_per_channel = total / src_channels;
    let out_channels = BRIDGE_CHANNELS as usize;

    let mut out = Vec::with_capacity(samples_per_channel * out_channels * 2);
    for s in 0..samples_per_channel {
        for c in 0..out_channels {
            // Take the matching source channel if it exists. Mono
            // sources duplicate channel 0 across L+R; quad+ sources
            // truncate to first BRIDGE_CHANNELS. This keeps stereo
            // ATEM/DeckLink targets always-stereo without making the
            // bridge configurable.
            let src_c = if c < src_channels { c } else { 0 };
            let idx = src_c * samples_per_channel + s;
            let f = if idx < total { planar_samples[idx] } else { 0.0 };
            let scaled = (f * 32767.0).clamp(-32768.0, 32767.0);
            let i = scaled as i16;
            out.extend_from_slice(&i.to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_source_duplicates_to_stereo() {
        // 4 mono samples at maximum positive amplitude
        let planar = vec![1.0_f32, 1.0, 1.0, 1.0];
        let bytes = ndi_planar_f32_to_s16le_interleaved(&planar, 1);
        // 4 samples × 2 channels × 2 bytes = 16 bytes
        assert_eq!(bytes.len(), 16);
        // Every value should be i16::MAX (32767) little-endian
        for chunk in bytes.chunks_exact(2) {
            let v = i16::from_le_bytes([chunk[0], chunk[1]]);
            assert_eq!(v, 32767);
        }
    }

    #[test]
    fn stereo_source_planar_to_interleaved() {
        // 2 samples per channel, stereo. Planar layout: L L R R
        // L0 = 0.5, L1 = -0.5, R0 = 0.25, R1 = -0.25
        let planar = vec![0.5_f32, -0.5, 0.25, -0.25];
        let bytes = ndi_planar_f32_to_s16le_interleaved(&planar, 2);
        // 2 samples × 2 channels × 2 bytes = 8 bytes
        assert_eq!(bytes.len(), 8);
        // Interleaved: L0 R0 L1 R1
        let s = |i: usize| {
            i16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]])
        };
        assert_eq!(s(0), (0.5 * 32767.0) as i16);
        assert_eq!(s(1), (0.25 * 32767.0) as i16);
        assert_eq!(s(2), (-0.5 * 32767.0) as i16);
        assert_eq!(s(3), (-0.25 * 32767.0) as i16);
    }

    #[test]
    fn clipping_signal_clamps_not_wraps() {
        // 1.5 would multiply to 49150 which overflows i16 — must clamp.
        let planar = vec![1.5_f32, -1.5];
        let bytes = ndi_planar_f32_to_s16le_interleaved(&planar, 2);
        let l = i16::from_le_bytes([bytes[0], bytes[1]]);
        let r = i16::from_le_bytes([bytes[2], bytes[3]]);
        assert_eq!(l, 32767); // clamped positive max
        assert_eq!(r, -32768); // clamped negative max
    }

    #[test]
    fn quad_source_truncates_to_stereo() {
        // 4 channels, 1 sample each. Planar: C0 C1 C2 C3
        let planar = vec![0.1_f32, 0.2, 0.3, 0.4];
        let bytes = ndi_planar_f32_to_s16le_interleaved(&planar, 4);
        // Output is 1 sample × 2 channels × 2 bytes = 4 bytes
        assert_eq!(bytes.len(), 4);
        let l = i16::from_le_bytes([bytes[0], bytes[1]]);
        let r = i16::from_le_bytes([bytes[2], bytes[3]]);
        assert_eq!(l, (0.1 * 32767.0) as i16);
        assert_eq!(r, (0.2 * 32767.0) as i16);
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let bytes = ndi_planar_f32_to_s16le_interleaved(&[], 2);
        assert!(bytes.is_empty());
    }
}
