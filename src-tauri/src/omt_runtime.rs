//! OMT (Open Media Transport) runtime + discovery surface.
//!
//! Parallel to `ndi_runtime.rs`. OMT is the open-source alternative
//! to NDI from VideoRT/Vizrt — wire-compatible with NDI in spirit,
//! royalty-free, and has its own libomt C library + OpenScreen
//! Discovery protocol. The libomt-rs crate (raycaster-io) wraps
//! the C SDK with a Rust-native API.
//!
//! v0.2.0-alpha.9 ships OMT behind a `--features omt` cargo flag.
//! libomt isn't bundled into our DMG/installer yet (the libomt-rs
//! build.rs needs `vendor/libomt/<os>/libomt.<ext>` or LIBOMT_PATH
//! pointing at one), so the default build skips it and `discover()`
//! returns an empty list. Users who want OMT today build from
//! source with `--features omt` and a libomt.dylib in their lib
//! search path. Future alpha will bundle libomt next to libndi
//! once we figure out a stable distribution channel.

use serde::Serialize;
use std::time::Duration;

/// Discovered OMT source — same JSON shape as `NdiSource` so the UI
/// can render either with the same tile component. The `address`
/// field is libomt's `"HOSTNAME (Source Name)"` discovery string,
/// which the streamer hands back to `OmtReceive::new()` verbatim.
#[derive(Serialize, Clone, Debug)]
pub struct OmtSource {
    pub name: String,
    pub host: Option<String>,
    pub address: String,
}

/// Initialize the OMT runtime. No-op when the `omt` feature is off
/// (so callers can wire this in unconditionally next to
/// `ndi_runtime::init()`). Errors surface via log::warn — same
/// philosophy as NDI: features that need a runtime quietly disable
/// when the runtime isn't available.
pub fn init() -> anyhow::Result<()> {
    #[cfg(feature = "omt")]
    {
        // libomt has no explicit init — discovery and receive create
        // their own internal state on first use. We log success here
        // for parity with the NDI init message.
        log::info!("OMT runtime available (libomt loaded)");
        Ok(())
    }
    #[cfg(not(feature = "omt"))]
    {
        log::info!("OMT support not compiled in (rebuild with --features omt)");
        Ok(())
    }
}

/// Snapshot the current list of OMT sources visible on the network.
/// `wait` is honored only when the `omt` feature is enabled; the
/// no-op path returns immediately. Empty list when:
///   - `omt` feature is off (most builds)
///   - libomt is loaded but discovery hasn't seen anything yet
///   - libomt fails internally (logged, swallowed)
pub fn discover(_wait: Duration) -> Vec<OmtSource> {
    #[cfg(feature = "omt")]
    {
        // libomt's OmtDiscovery::addresses() returns a Vec<String>
        // of "HOSTNAME (Source Name)" entries. We split that back
        // into name + host for our shared tile shape with NDI.
        // libomt does its own internal polling; the wait arg is
        // unused on this side (kept in the signature for API parity
        // with ndi_runtime::discover).
        libomt::OmtDiscovery::addresses()
            .into_iter()
            .map(|addr| {
                // Format is "HOSTNAME (Source Name)". Parse defensively
                // — if it doesn't match, fall back to using the whole
                // string as the name with no host.
                if let Some((host, name_paren)) = addr.split_once(" (") {
                    let name = name_paren.trim_end_matches(')').to_string();
                    OmtSource {
                        name,
                        host: Some(host.to_string()),
                        address: addr,
                    }
                } else {
                    OmtSource {
                        name: addr.clone(),
                        host: None,
                        address: addr,
                    }
                }
            })
            .collect()
    }
    #[cfg(not(feature = "omt"))]
    {
        Vec::new()
    }
}

/// Look up a discovered source by exact name. Returns the libomt
/// address string when the `omt` feature is on; None on no match
/// or when the feature is off. The streamer hands the returned
/// string to `OmtReceive::new()` directly.
pub fn find_address_by_name(name: &str) -> Option<String> {
    #[cfg(feature = "omt")]
    {
        libomt::OmtDiscovery::addresses()
            .into_iter()
            .find(|addr| {
                if let Some((_host, name_paren)) = addr.split_once(" (") {
                    name_paren.trim_end_matches(')') == name
                } else {
                    addr == name
                }
            })
    }
    #[cfg(not(feature = "omt"))]
    {
        let _ = name;
        None
    }
}
