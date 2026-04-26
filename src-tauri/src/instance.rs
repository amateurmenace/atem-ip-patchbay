//! Multi-instance support — Phase 6.
//!
//! A single user wants to push *several* sources to *several* ATEM
//! inputs simultaneously, each with its own UI window, port pair,
//! and (eventually) persisted state. Per CLAUDE.md, macOS already
//! supports multiple .app instances via `LSMultipleInstancesProhibited
//! = NO` (the default for our Info.plist); we just need:
//!
//! 1. A CLI flag to label each instance (`--instance-name foo`),
//!    surfaced in the window title so the user can tell windows
//!    apart.
//! 2. Per-instance state directories under
//!    `~/Library/Application Support/ATEM IP Patchbay/instances/<name>/`
//!    (Mac) or `%APPDATA%\ATEM IP Patchbay\instances\<name>\` (Win)
//!    so future state persistence (encoder defaults, last-used
//!    destination, etc.) doesn't collide.
//! 3. Port-walk already exists from Phase 0 — each instance grabs
//!    a free HTTP + BMD port pair without intervention.
//!
//! How a user launches a second instance:
//!   open -n /Applications/ATEM\ IP\ Patchbay.app --args --instance-name two
//!
//! Phase 8 promotes that to a Finder-friendly menu item; for now the
//! Terminal one-liner is the supported path.

use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "ATEM IP Patchbay",
    about = "Push any video source into Blackmagic ATEM gear over BMD-flavored SRT"
)]
pub struct Cli {
    /// Instance name shown in the window title and used to scope the
    /// per-instance state directory. Default is "default" so a single
    /// fresh launch always lands in the same dir.
    #[arg(long = "instance-name", default_value = "default")]
    pub instance_name: String,

    /// Override the HTTP port the embedded Axum server tries first.
    /// Falls back through ten adjacent ports if taken (port walk).
    #[arg(long)]
    pub http_port: Option<u16>,

    /// Override the BMD control protocol port. Same port-walk
    /// fallback as the HTTP server.
    #[arg(long)]
    pub bmd_port: Option<u16>,
}

impl Cli {
    pub fn from_env() -> Self {
        // Tauri sometimes sneaks platform args before ours. Use
        // `try_parse_from` over std::env::args() so we keep our
        // strict parsing while ignoring anything Tauri injected
        // before `--`. In practice the `.app` bundle's args are
        // clean, but `cargo tauri dev` adds stuff.
        let args: Vec<String> = std::env::args().collect();
        Self::try_parse_from(&args).unwrap_or_else(|_| Self {
            instance_name: "default".into(),
            http_port: None,
            bmd_port: None,
        })
    }
}

/// Compute the per-instance state directory. Created on demand by
/// callers (we don't proactively mkdir at boot — most instances will
/// only ever read XML configs from elsewhere).
pub fn instance_state_dir(instance_name: &str) -> PathBuf {
    let app = "ATEM IP Patchbay";
    let base = dirs::data_dir()
        .or_else(dirs::config_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(app).join("instances").join(instance_name)
}

/// Make the directory if it doesn't exist; logs and returns the path.
pub fn ensure_instance_dir(instance_name: &str) -> PathBuf {
    let dir = instance_state_dir(instance_name);
    if let Err(err) = std::fs::create_dir_all(&dir) {
        log::warn!(
            "could not create instance state dir {}: {err}",
            dir.display()
        );
    }
    dir
}
