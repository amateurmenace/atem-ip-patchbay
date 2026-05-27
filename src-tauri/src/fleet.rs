//! Multi-source fleet — Phase B of multi-source mode (alpha.15).
//!
//! Holds N independent EncoderState + Streamer + Preview triples,
//! each one a self-contained streaming pipeline. The fleet itself is
//! a thin wrapper; per-tile lifecycle (start, stop, settings, etc.)
//! goes through the tile's individual Arc'd components.
//!
//! TILE_COUNT is hard-capped at 4 because:
//! - macOS VideoToolbox tops out at ~4 parallel HEVC encoder sessions
//!   on M-series silicon (kVTCouldNotFindVideoEncoderErr above that)
//! - 2x2 is the most natural grid layout for the multiview UI
//! - 4 BMD ports walk cleanly from 9977 with no realistic chance of
//!   a port-collision dragging the whole boot into failure
//!
//! Construction is synchronous and infallible — the BMD ports are
//! pre-resolved by the caller (lib.rs runs the port-walk async then
//! hands us a fixed [u16; 4] block). HTTP handler resolution is via
//! `tile(idx) -> Option<&TileSlot>`; out-of-range returns None and
//! the handler surfaces a 404.

use crate::preview::Preview;
use crate::state::EncoderState;
use crate::streamer::Streamer;
use std::sync::Arc;

/// Maximum number of simultaneous source→destination tiles. Fixed at
/// 4 by hardware/UX constraints; see module docs. If a future macOS
/// generation lifts the VideoToolbox encoder cap meaningfully, bump
/// this AND the multiview.html grid CSS together.
pub const TILE_COUNT: usize = 4;

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
