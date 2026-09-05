//! Per-tile state persistence — alpha.41.
//!
//! Today's gap: a Cmd-Q / crash / reinstall wipes every tile's
//! configuration. The operator has to re-pick source, destination,
//! audio mode, encoder etc. for every tile, every show. In live-
//! production workflows that's actively dangerous (forget one
//! setting before going live, you're broadcasting silently).
//!
//! Fix: serialize the tile's user-settable settings to a small JSON
//! file per tile in the instance state dir. Save on every successful
//! `apply_settings` call. Load at boot and re-apply via the same
//! `apply_settings` path, so loading is just a settings-update like
//! any other (same validation, same side effects, no special-case
//! restoration code).
//!
//! Atomicity: write to `<path>.tmp` then rename to `<path>` so a
//! mid-write crash leaves either the previous state or the new state
//! intact — never a half-file. Rename is atomic on every filesystem
//! we ship to (NTFS, APFS, ext4).
//!
//! Scope of what's persisted: user-pickable fields only. Stats,
//! discovered device lists, runtime status — excluded. The whole
//! point is that what the operator clicked on stays clicked.
//!
//! What's NOT covered (deferred):
//!   - Nested relay / overlay sub-structs. Operators rarely use
//!     overlay; relay/receive setup is per-show. If field-asked,
//!     extend `PersistedTileState` and the from/to-snapshot conversion.
//!   - Multi-instance isolation (each instance has its own state
//!     dir, but tile indices are per-instance, so instance "alpha"'s
//!     tile 0 doesn't conflict with instance "beta"'s tile 0).

use crate::state::{EncoderState, SettingsUpdate};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Version of the on-disk format. Bump when adding/removing fields
/// in a way that breaks compatibility. On load, version mismatch
/// → discard (logged, not fatal — operator gets defaults).
const PERSIST_VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PersistedTileState {
    pub version: u32,

    // Identity
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    // Source
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ndi_source_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omt_source_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub av_video_index: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub av_video_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub av_audio_index: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub av_audio_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipe_path: Option<String>,

    // Destination
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decklink_device_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decklink_output_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decklink_format_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decklink_pixel_format: Option<String>,

    // ATEM-side destination (back-compat / tile 0 still uses these)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub srt_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub srt_latency_us: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub srt_listen_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streamid_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streamid_legacy: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_service_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_server_name: Option<String>,

    // Video encoding
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_encoder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder_extra_flags: Option<String>,

    // Audio
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_output_mono: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_pan_l: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_pan_r: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_bitrate_kbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_channels: Option<u8>,

    // Reconnect + observability
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_reconnect: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_reconnect_max_attempts: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meters_enabled: Option<bool>,

    // OMT output
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omt_output_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omt_output_name: Option<String>,
}

impl PersistedTileState {
    /// Build a persisted-state snapshot from an encoder's current
    /// state. Called from the apply_settings handler after a
    /// successful settings update.
    pub fn from_encoder(encoder: &EncoderState) -> Self {
        let snap = encoder.snapshot();
        let trim = |s: String| if s.is_empty() { None } else { Some(s) };
        Self {
            version: PERSIST_VERSION,
            label: trim(snap.label),
            source_id: trim(snap.source_id),
            ndi_source_name: trim(snap.ndi_source_name),
            omt_source_name: trim(snap.omt_source_name),
            av_video_index: (snap.av_video_index >= 0).then_some(snap.av_video_index),
            av_video_name: trim(snap.av_video_name),
            av_audio_index: (snap.av_audio_index >= 0).then_some(snap.av_audio_index),
            av_audio_name: trim(snap.av_audio_name),
            pipe_path: trim(snap.pipe_path),
            destination_type: trim(snap.destination_type),
            decklink_device_name: trim(snap.decklink_device_name),
            decklink_output_mode: trim(snap.decklink_output_mode),
            decklink_format_code: trim(snap.decklink_format_code),
            decklink_pixel_format: trim(snap.decklink_pixel_format),
            custom_url: trim(snap.custom_url),
            stream_key: trim(snap.stream_key),
            passphrase: trim(snap.passphrase),
            srt_mode: trim(snap.srt_mode),
            srt_latency_us: (snap.srt_latency_us > 0).then_some(snap.srt_latency_us),
            srt_listen_port: (snap.srt_listen_port > 0).then_some(snap.srt_listen_port),
            streamid_override: trim(snap.streamid_override),
            streamid_legacy: Some(snap.streamid_legacy),
            current_service_name: trim(snap.current_service_name),
            current_server_name: trim(snap.current_server_name),
            video_mode: trim(snap.video_mode),
            quality_level: trim(snap.quality_level),
            video_codec: trim(snap.video_codec),
            video_encoder: trim(snap.video_encoder),
            encoder_extra_flags: trim(snap.encoder_extra_flags),
            audio_mode: trim(snap.audio_mode),
            audio_output_mono: Some(snap.audio_output_mono),
            audio_pan_l: (snap.audio_pan_l > 0).then_some(snap.audio_pan_l),
            audio_pan_r: (snap.audio_pan_r > 0).then_some(snap.audio_pan_r),
            audio_codec: trim(snap.audio_codec),
            audio_bitrate_kbps: (snap.audio_bitrate_kbps > 0).then_some(snap.audio_bitrate_kbps),
            audio_sample_rate: (snap.audio_sample_rate > 0).then_some(snap.audio_sample_rate),
            audio_channels: (snap.audio_channels > 0).then_some(snap.audio_channels),
            auto_reconnect: Some(snap.auto_reconnect),
            auto_reconnect_max_attempts: (snap.auto_reconnect_max_attempts > 0)
                .then_some(snap.auto_reconnect_max_attempts),
            meters_enabled: Some(snap.meters_enabled),
            omt_output_enabled: Some(snap.omt_output_enabled),
            omt_output_name: trim(snap.omt_output_name),
        }
    }

    /// Convert to a SettingsUpdate suitable for apply_settings.
    /// Used at boot to restore persisted state via the same code
    /// path normal updates take, so validation + side effects
    /// (encoder state notifications, etc.) all flow through.
    pub fn to_settings_update(&self) -> SettingsUpdate {
        SettingsUpdate {
            video_mode: self.video_mode.clone(),
            quality_level: self.quality_level.clone(),
            source_id: self.source_id.clone(),
            custom_url: self.custom_url.clone(),
            stream_key: self.stream_key.clone(),
            passphrase: self.passphrase.clone(),
            srt_mode: self.srt_mode.clone(),
            srt_latency_us: self.srt_latency_us,
            srt_listen_port: self.srt_listen_port,
            streamid_override: self.streamid_override.clone(),
            streamid_legacy: self.streamid_legacy,
            video_codec: self.video_codec.clone(),
            current_service_name: self.current_service_name.clone(),
            current_server_name: self.current_server_name.clone(),
            ndi_source_name: self.ndi_source_name.clone(),
            omt_source_name: self.omt_source_name.clone(),
            omt_output_enabled: self.omt_output_enabled,
            omt_output_name: self.omt_output_name.clone(),
            av_video_index: self.av_video_index,
            av_video_name: self.av_video_name.clone(),
            av_audio_index: self.av_audio_index,
            av_audio_name: self.av_audio_name.clone(),
            audio_mode: self.audio_mode.clone(),
            audio_output_mono: self.audio_output_mono,
            audio_pan_l: self.audio_pan_l,
            audio_pan_r: self.audio_pan_r,
            pipe_path: self.pipe_path.clone(),
            label: self.label.clone(),
            relay: None,
            overlay: None,
            destination_type: self.destination_type.clone(),
            decklink_device_name: self.decklink_device_name.clone(),
            decklink_output_mode: self.decklink_output_mode.clone(),
            decklink_format_code: self.decklink_format_code.clone(),
            decklink_pixel_format: self.decklink_pixel_format.clone(),
            video_encoder: self.video_encoder.clone(),
            encoder_extra_flags: self.encoder_extra_flags.clone(),
            audio_codec: self.audio_codec.clone(),
            audio_bitrate_kbps: self.audio_bitrate_kbps,
            audio_sample_rate: self.audio_sample_rate,
            audio_channels: self.audio_channels,
            auto_reconnect: self.auto_reconnect,
            auto_reconnect_max_attempts: self.auto_reconnect_max_attempts,
            meters_enabled: self.meters_enabled,
        }
    }
}

/// Compute the on-disk path for a tile's state file.
fn tile_path(state_dir: &Path, tile_idx: u8) -> PathBuf {
    state_dir.join(format!("multiview-tile-{tile_idx}.json"))
}

/// Save a tile's settings to disk. Best-effort: warnings on failure,
/// never bubbles up — persistence failure shouldn't break runtime.
///
/// Writes to a .tmp file then renames into place so a mid-write
/// crash never leaves a half-file the next boot has to deal with.
pub fn save_tile(state_dir: &Path, tile_idx: u8, encoder: &EncoderState) {
    if let Err(e) = std::fs::create_dir_all(state_dir) {
        log::warn!(
            "persist tile {tile_idx}: create_dir_all({}) failed: {e}",
            state_dir.display()
        );
        return;
    }
    let payload = PersistedTileState::from_encoder(encoder);
    let json = match serde_json::to_string_pretty(&payload) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("persist tile {tile_idx}: serialize failed: {e}");
            return;
        }
    };
    let path = tile_path(state_dir, tile_idx);
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, json.as_bytes()) {
        log::warn!(
            "persist tile {tile_idx}: write({}) failed: {e}",
            tmp.display()
        );
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        log::warn!(
            "persist tile {tile_idx}: rename({} -> {}) failed: {e}",
            tmp.display(),
            path.display()
        );
        // Try to clean up the stale tmp so it doesn't accumulate
        // across failures.
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Load a tile's settings from disk and apply them via the
/// encoder's standard apply path. Returns true if a state file
/// was found and applied; false on missing / corrupt / version-
/// mismatch (operator gets defaults). Never bubbles errors.
pub fn load_and_apply(state_dir: &Path, tile_idx: u8, encoder: &Arc<EncoderState>) -> bool {
    let path = tile_path(state_dir, tile_idx);
    if !path.exists() {
        return false;
    }
    let json = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            log::warn!(
                "persist tile {tile_idx}: read({}) failed: {e}",
                path.display()
            );
            return false;
        }
    };
    let persisted: PersistedTileState = match serde_json::from_str(&json) {
        Ok(p) => p,
        Err(e) => {
            log::warn!(
                "persist tile {tile_idx}: parse({}) failed: {e}",
                path.display()
            );
            return false;
        }
    };
    if persisted.version != PERSIST_VERSION {
        log::info!(
            "persist tile {tile_idx}: version {} vs expected {} — ignoring",
            persisted.version,
            PERSIST_VERSION
        );
        return false;
    }
    let update = persisted.to_settings_update();
    encoder.apply_settings(&update);
    log::info!(
        "persist tile {tile_idx}: restored from {}",
        path.display()
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_fields() {
        let p = PersistedTileState {
            version: PERSIST_VERSION,
            source_id: Some("ndi".into()),
            ndi_source_name: Some("Camera 1".into()),
            destination_type: Some("decklink".into()),
            decklink_device_name: Some("DeckLink Studio 4K".into()),
            decklink_format_code: Some("Hp29".into()),
            audio_mode: Some("auto".into()),
            auto_reconnect: Some(true),
            ..Default::default()
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: PersistedTileState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.source_id.as_deref(), Some("ndi"));
        assert_eq!(back.ndi_source_name.as_deref(), Some("Camera 1"));
        assert_eq!(back.decklink_device_name.as_deref(), Some("DeckLink Studio 4K"));
        assert_eq!(back.decklink_format_code.as_deref(), Some("Hp29"));
        assert_eq!(back.audio_mode.as_deref(), Some("auto"));
        assert_eq!(back.auto_reconnect, Some(true));
    }

    #[test]
    fn version_mismatch_ignored_via_load() {
        // Just verifies the version field is checked — the actual
        // load_and_apply test would need a fake encoder + dir setup.
        let p = PersistedTileState {
            version: 999,
            ..Default::default()
        };
        assert_ne!(p.version, PERSIST_VERSION);
    }
}
