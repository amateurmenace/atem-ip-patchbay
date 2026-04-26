//! Global NDI runtime + Finder for source discovery.
//!
//! The grafton-ndi `NDI` handle initializes the underlying NewTek SDK
//! (loads `libndi.dylib` on Mac / `Processing.NDI.Lib.x64.dll` on
//! Windows). We init once at app boot and keep a singleton Finder
//! polling the network. /api/ndi-senders + /api/discover read from
//! its source list — much faster than re-creating a Finder per
//! request, and matches the pattern v0.1.0's mDNS module used.
//!
//! On macOS the grafton-ndi crate flags its support as "experimental
//! with limited testing" — the Phase 4 spike confirmed discovery
//! works, but receive (capture_video) is the larger surface and we
//! verify it end-to-end in [`crate::ndi_capture`].

use anyhow::Result;
use grafton_ndi::{Finder, FinderOptions, NDI};
use serde::Serialize;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

struct State {
    _ndi: NDI,
    finder: Finder,
}

static RUNTIME: OnceLock<Mutex<Option<State>>> = OnceLock::new();

pub fn init() -> Result<()> {
    let cell = RUNTIME.get_or_init(|| Mutex::new(None));
    let mut slot = cell.lock().unwrap();
    if slot.is_some() {
        return Ok(());
    }
    let ndi = NDI::new()?;
    let finder = Finder::new(
        &ndi,
        &FinderOptions::builder().show_local_sources(true).build(),
    )?;
    *slot = Some(State { _ndi: ndi, finder });
    log::info!("NDI runtime initialized");
    Ok(())
}

#[derive(Serialize, Clone, Debug)]
pub struct NdiSource {
    pub name: String,
    pub host: Option<String>,
    pub address: String,
}

/// Snapshot the current list of NDI sources visible on the network.
/// `wait` lets the call block briefly to give discovery time to find
/// new sources — typical caller passes 1-2 seconds. Returns an empty
/// list if NDI runtime isn't initialized.
pub fn discover(wait: Duration) -> Vec<NdiSource> {
    let Some(cell) = RUNTIME.get() else {
        return Vec::new();
    };
    let slot = cell.lock().unwrap();
    let Some(state) = slot.as_ref() else {
        return Vec::new();
    };
    let sources = match state.finder.find_sources(wait) {
        Ok(s) => s,
        Err(err) => {
            log::warn!("NDI find_sources failed: {err}");
            return Vec::new();
        }
    };
    sources
        .into_iter()
        .map(|s| NdiSource {
            host: s.host().map(|h| h.to_string()),
            address: format!("{:?}", s.address),
            name: s.name,
        })
        .collect()
}

/// Look up a source by exact `name` match. Used by the streamer when
/// the user has selected an NDI sender by name and we need the live
/// `Source` handle (with its IP/port discovery) to attach a Receiver.
pub fn find_source_by_name(name: &str) -> Option<grafton_ndi::Source> {
    let cell = RUNTIME.get()?;
    let slot = cell.lock().unwrap();
    let state = slot.as_ref()?;
    let sources = state
        .finder
        .find_sources(Duration::from_millis(500))
        .ok()?;
    sources.into_iter().find(|s| s.name == name)
}
