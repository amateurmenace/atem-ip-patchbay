//! Encoder fleet — minimal post-alpha.15-pivot wrapper.
//!
//! Originally built for alpha.15's in-app 2x2 multi-tile grid; the
//! iframe-based grid was too cramped to be operationally useful and
//! got pulled in alpha.16. The new multi-source story is multi-
//! INSTANCE via the existing `--instance-name` CLI flag (each
//! instance = its own OS process / Tauri window / state dir), plus
//! a small monitor window per instance that the operator positions
//! on their screen like a video-switcher multiview output.
//!
//! With one tile per process, TILE_COUNT is back to 1 — the fleet
//! is a thin wrapper around the singleton encoder + streamer +
//! preview. We keep the abstraction around because:
//!   - the /api/i/0/* routes still work (alongside /api/* aliases),
//!     so a future multi-tile UI could light them up without ripping
//!     out routing again
//!   - the shutdown_all() hook in RunEvent::Exit still does the
//!     right thing whether N is 1 or 4
//!
//! If we never go back to multi-tile-per-process this whole module
//! could be deleted in favor of the original single-Arc pattern.
//! Cost of keeping it: ~30 bytes of struct overhead per process.

use crate::preview::Preview;
use crate::state::EncoderState;
use crate::streamer::Streamer;
use std::sync::Arc;

/// Number of tiles in this Tauri process. Originally 1 (alpha.16);
/// bumped to 8 in alpha.40 for the broadcast-multiview rework;
/// dropped to 4 in alpha.41 after a UX + reliability review; raised
/// to 6 in alpha.62 once the Phase A spike validated 6 concurrent
/// DeckLink outputs on the operator's rig (the broadcast goal:
/// 6 NDI sources → 6 DeckLink SDI outputs).
///
/// Why 6: it's the operator's stated channel count, and the Phase A
/// `tools/decklink-spike` confirmed 6 simultaneous `decklink_enc`
/// FFmpeg outputs hold for a full 180s with zero stalls/drops on the
/// real hardware — single-process Option A holds. A 3x2 grid gives
/// each tile a comfortable ~470x500 at the rig's resolution. (The one
/// spike anomaly was an FFmpeg access-violation when *closing* a
/// real-SDI DeckLink output — a teardown-only crash that recovers on
/// the next Start; tracked separately, not a per-tile-count concern.)
/// Operators who need still more channels can launch a second instance
/// via the existing `--instance-name` flag (Session 11 alpha.16).
///
/// Per-tile resource footprint: ~one EncoderState + one Streamer +
/// one Preview struct (a few KB each) when idle. At full saturation:
/// 6 FFmpeg children + 6 NDI/OMT receivers + 6 watchdogs. For the
/// NDI→DeckLink path (raw frames in, wrapped_avframe out — no encode)
/// CPU stays light; the real ceilings are NDI receive bandwidth (NIC)
/// and memory bandwidth. Expect ~1.2-1.6 GB RSS at 1080p across 6
/// tiles, extrapolating the ~800 MB-1 GB measured across 4.
pub const TILE_COUNT: usize = 6;

/// One tile slot — a complete self-contained pipeline. Every field
/// is Arc'd so axum handlers can grab them by reference without
/// touching the rest of the fleet. The `idx` field is denormalized
/// for log messages + lifecycle hooks that don't have a reverse
/// pointer back to the parent Vec.
pub struct TileSlot {
    pub idx: u8,
    pub encoder: Arc<EncoderState>,
    pub streamer: Arc<Streamer>,
    pub preview: Arc<Preview>,
    /// TCP port this tile's BMD control protocol server is bound to.
    /// Each tile needs its own port because an ATEM (or BMD encoder
    /// monitoring tool) connects to a specific TCP socket per
    /// encoder; multiplexing on one port would confuse the wire
    /// protocol's per-connection state machine.
    pub bmd_port: u16,
}

/// The set of tiles managed by this Tauri window. Wrapped in Arc by
/// `new()` so HTTP handlers + Tauri command handlers + the exit
/// handler can all clone the fleet cheaply.
pub struct EncoderFleet {
    tiles: Vec<TileSlot>,
}

impl EncoderFleet {
    /// Build TILE_COUNT independent tiles, each with its own state +
    /// streamer + preview. `bmd_ports` carries the pre-resolved TCP
    /// ports (caller does the port-walk because that's async and the
    /// fleet constructor is sync).
    pub fn new(bmd_ports: [u16; TILE_COUNT]) -> Arc<Self> {
        let tiles = (0..TILE_COUNT as u8)
            .map(|idx| {
                let encoder = Arc::new(EncoderState::new());
                let preview = Preview::new();
                let streamer = Streamer::new(encoder.clone(), preview.clone());
                TileSlot {
                    idx,
                    encoder,
                    streamer,
                    preview,
                    bmd_port: bmd_ports[idx as usize],
                }
            })
            .collect();
        Arc::new(EncoderFleet { tiles })
    }

    /// Look up a tile by index. Returns None if idx ≥ TILE_COUNT.
    /// HTTP handlers map this to a 404 with a clear "no tile at
    /// index N" message rather than a generic 500.
    pub fn tile(&self, idx: u8) -> Option<&TileSlot> {
        self.tiles.get(idx as usize)
    }

    /// Iterate over all tiles in order. Used by lib.rs to start
    /// per-tile BMD protocol servers, and by the exit handler to
    /// stop every streamer before the process dies (otherwise FFmpeg
    /// children reparent to launchd and keep streaming).
    pub fn tiles(&self) -> impl Iterator<Item = &TileSlot> {
        self.tiles.iter()
    }

    /// Stop every running streamer. Called synchronously (block_on)
    /// from RunEvent::ExitRequested so all per-tile FFmpegs get a
    /// graceful SIGTERM + the SRT/RTMP receivers' accept slots get
    /// freed before the parent exits.
    pub async fn shutdown_all(&self) {
        for tile in &self.tiles {
            log::info!("fleet shutdown: stopping tile {}", tile.idx);
            let _ = tile.streamer.stop().await;
        }
    }
}
