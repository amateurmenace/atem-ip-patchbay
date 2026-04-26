use crate::xml::{load_service, load_service_text, StreamConfig, StreamService};
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::RwLock;
use std::time::Instant;
use uuid::Uuid;

/// BMD-spec quality matrix used when no service XML is loaded — so
/// the UI's quality chooser is populated and the user can stream
/// against a manually-entered destination without first dropping an
/// XML. Bitrates match the sample XMLs (Remote 1/2.xml) shipped
/// alongside the binary.
///
/// Lookup order at FFmpeg-cmd-build time: matched (resolution, fps)
/// from the active service's profile -> fallback to this matrix
/// keyed by the same (quality_level, resolution, fps).
pub const DEFAULT_QUALITY_LEVELS: &[&str] =
    &["Streaming High", "Streaming Medium", "Streaming Low"];
const DEFAULT_AUDIO_BITRATE: u64 = 128_000;
const DEFAULT_KEYFRAME_INTERVAL: u32 = 2;

/// (quality_level, resolution, fps) -> video bitrate (bits/sec).
fn default_bitrate(quality: &str, resolution: &str, fps: u32) -> Option<u64> {
    let bps = match (quality, resolution, fps) {
        ("Streaming High",   "1080p", 60) => 9_000_000,
        ("Streaming High",   "1080p", 30) => 6_000_000,
        ("Streaming High",   "720p",  60) => 6_000_000,
        ("Streaming High",   "720p",  30) => 4_000_000,
        ("Streaming Medium", "1080p", 60) => 7_600_000,
        ("Streaming Medium", "1080p", 30) => 4_500_000,
        ("Streaming Medium", "720p",  60) => 4_500_000,
        ("Streaming Medium", "720p",  30) => 3_000_000,
        ("Streaming Low",    "1080p", 60) => 4_500_000,
        ("Streaming Low",    "1080p", 30) => 3_000_000,
        ("Streaming Low",    "720p",  60) => 2_250_000,
        ("Streaming Low",    "720p",  30) => 1_500_000,
        _ => return None,
    };
    Some(bps)
}

pub const AVAILABLE_VIDEO_MODES: &[&str] = &[
    "Auto",
    "2160p23.98",
    "2160p24",
    "2160p25",
    "2160p29.97",
    "2160p30",
    "2160p50",
    "2160p59.94",
    "2160p60",
    "1080p23.98",
    "1080p24",
    "1080p25",
    "1080p29.97",
    "1080p30",
    "1080p50",
    "1080p59.94",
    "1080p60",
    "720p25",
    "720p30",
    "720p50",
    "720p60",
];

#[derive(Debug, Clone)]
pub struct StreamStats {
    pub status: String,
    pub bitrate: u64,
    pub cache_used_pct: u32,
    pub started_at: Option<Instant>,
    pub error: Option<String>,
    pub fps: f32,
    pub speed: f32,
    pub frames_sent: u64,
    pub frames_dropped: u64,
    pub quality: f32,
}

impl Default for StreamStats {
    fn default() -> Self {
        Self {
            status: "Idle".into(),
            bitrate: 0,
            cache_used_pct: 0,
            started_at: None,
            error: None,
            fps: 0.0,
            speed: 0.0,
            frames_sent: 0,
            frames_dropped: 0,
            quality: 0.0,
        }
    }
}

impl StreamStats {
    pub fn duration_string(&self) -> String {
        let Some(started) = self.started_at else {
            return "00:00:00:00".into();
        };
        let elapsed = started.elapsed().as_secs();
        let days = elapsed / 86_400;
        let hours = (elapsed % 86_400) / 3_600;
        let mins = (elapsed % 3_600) / 60;
        let secs = elapsed % 60;
        format!("{days:02}:{hours:02}:{mins:02}:{secs:02}")
    }
}

#[derive(Debug)]
pub struct EncoderState {
    inner: RwLock<Inner>,
}

#[derive(Debug)]
struct Inner {
    label: String,
    model: String,
    unique_id: String,
    device_uuid: String,
    video_mode: String,
    quality_level: String,
    source_id: String,

    av_video_index: i32,
    av_audio_index: i32,
    av_video_name: String,
    av_audio_name: String,
    /// 1-indexed channel numbers routed to the L/R of the outgoing
    /// stereo AAC stream when the active audio device is multi-channel
    /// (Dante VSC, CoreAudio aggregate). Defaults 1 and 2 give plain
    /// front-stereo behavior. Wired into the FFmpeg cmd as
    /// `-af pan="stereo|c0=cN-1|c1=cM-1"` only for devices the streamer
    /// recognizes as multi-channel — for normal stereo mics the filter
    /// is omitted so FFmpeg's default channel handling applies.
    audio_pan_l: u8,
    audio_pan_r: u8,

    pipe_path: String,

    /// Name of the selected NDI sender (for source_id="ndi"). Phase 4.
    /// Populated by the UI after the user picks one from /api/ndi-senders.
    ndi_source_name: String,

    relay_bind_host: String,
    relay_srt_port: u16,
    relay_srt_latency_us: u32,
    relay_srt_passphrase: String,
    relay_rtmp_port: u16,
    relay_rtmp_app: String,
    relay_rtmp_key: String,

    overlay_title: String,
    overlay_subtitle: String,
    overlay_logo_path: String,
    overlay_clock: bool,

    services: BTreeMap<String, StreamService>,
    current_service_name: String,
    current_server_name: String,
    stream_key: String,
    passphrase: String,

    custom_url: String,

    srt_mode: String,
    srt_latency_us: u32,
    srt_listen_port: u16,
    streamid_override: String,
    streamid_legacy: bool,

    video_codec: String,

    stats: StreamStats,
}

impl EncoderState {
    pub fn new() -> Self {
        let unique_id = Uuid::new_v4().simple().to_string().to_uppercase();
        let device_uuid = Uuid::new_v4().to_string();
        Self {
            inner: RwLock::new(Inner {
                label: "Blackmagic Streaming Encoder Emulator".into(),
                model: "Blackmagic Streaming Encoder HD".into(),
                unique_id,
                device_uuid,
                video_mode: "1080p30".into(),
                quality_level: "Streaming High".into(),
                source_id: "test_pattern".into(),
                av_video_index: 0,
                av_audio_index: -1,
                av_video_name: String::new(),
                av_audio_name: String::new(),
                audio_pan_l: 1,
                audio_pan_r: 2,
                pipe_path: String::new(),
                ndi_source_name: String::new(),
                relay_bind_host: "0.0.0.0".into(),
                relay_srt_port: 9710,
                relay_srt_latency_us: 200_000,
                relay_srt_passphrase: String::new(),
                relay_rtmp_port: 1935,
                relay_rtmp_app: "live".into(),
                relay_rtmp_key: "stream".into(),
                overlay_title: String::new(),
                overlay_subtitle: String::new(),
                overlay_logo_path: String::new(),
                overlay_clock: false,
                services: BTreeMap::new(),
                current_service_name: String::new(),
                current_server_name: "SRT".into(),
                stream_key: String::new(),
                passphrase: String::new(),
                custom_url: String::new(),
                srt_mode: "caller".into(),
                srt_latency_us: 500_000,
                srt_listen_port: 9710,
                streamid_override: String::new(),
                streamid_legacy: false,
                video_codec: "h265".into(),
                stats: StreamStats::default(),
            }),
        }
    }

    /// Load a streaming service from an XML file.
    ///
    /// `make_active` semantics:
    /// - `None`: only become current if no service is set yet (boot-time
    ///   "first XML wins"). This matches Python's `make_active=None`.
    /// - `Some(true)`: always become current (UI imports clear `custom_url`
    ///   so the new service drives the destination).
    /// - `Some(false)`: register but don't switch.
    ///
    /// Disambiguation: if the loaded service's `name` collides with one
    /// already in the registry (common when a user has multiple XMLs
    /// pointing at the same ATEM that all use service.name="ATEM Mini
    /// Extreme ISO G2"), the new entry's name is suffixed with the
    /// XML filename stem so both show up as separate dropdown entries.
    pub fn add_service_from_xml(&self, path: &Path, make_active: Option<bool>) -> Result<()> {
        let mut svc = load_service(path)?;
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        {
            let inner = self.inner.read().unwrap();
            if inner.services.contains_key(&svc.name) {
                svc.name = format!("{} [{}]", svc.name, stem);
            }
        }
        self.register_service(svc, make_active);
        Ok(())
    }

    /// Drop every loaded service and reset the destination state to
    /// "no service selected". Used by the UI's Clear XML button so the
    /// user can switch to a manual `custom_url` destination, and by
    /// `add_service_from_xml_text(replace=true)` so dropping a new
    /// XML replaces the old one instead of accumulating.
    pub fn clear_services(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.services.clear();
        inner.current_service_name.clear();
        inner.current_server_name = "SRT".into();
        inner.stream_key.clear();
        inner.passphrase.clear();
        // Don't touch custom_url — clearing services + having a custom
        // URL is the "I want to use a manual destination" state.
    }

    pub fn add_service_from_xml_text(&self, text: &str, make_active: bool) -> Result<()> {
        let mut svc = load_service_text(text)?;
        // Same disambiguation as the file path; pasted text has no
        // filename so we use a "(pasted N)" suffix where N is the
        // first integer that doesn't collide.
        {
            let inner = self.inner.read().unwrap();
            if inner.services.contains_key(&svc.name) {
                let mut n = 2;
                loop {
                    let candidate = format!("{} (pasted {n})", svc.name);
                    if !inner.services.contains_key(&candidate) {
                        svc.name = candidate;
                        break;
                    }
                    n += 1;
                }
            }
        }
        self.register_service(svc, Some(make_active));
        Ok(())
    }

    fn register_service(&self, svc: StreamService, make_active: Option<bool>) {
        let mut inner = self.inner.write().unwrap();
        let first_load = inner.current_service_name.is_empty();
        let should_activate = make_active.unwrap_or(first_load);
        let svc_name = svc.name.clone();
        let svc_key = svc.key.clone();
        let default_profile_name = svc.get_default_profile().map(|p| p.name.clone());
        let first_srt_name = svc.srt_servers().first().map(|s| s.name.clone());
        let first_any_name = svc.servers.first().map(|s| s.name.clone());
        inner.services.insert(svc_name.clone(), svc);
        if should_activate {
            inner.current_service_name = svc_name;
            inner.stream_key = svc_key;
            if let Some(p) = default_profile_name {
                inner.quality_level = p;
            }
            if let Some(n) = first_srt_name.or(first_any_name) {
                inner.current_server_name = n;
            }
            if make_active == Some(true) {
                inner.custom_url.clear();
            }
        }
    }

    /// Capture a read-only view of just the source-relevant fields.
    /// Avoids exposing the inner RwLock and lets sources::resolve_source
    /// build an FFmpeg command without holding the lock.
    pub fn source_selection(&self) -> SourceSelection {
        let inner = self.inner.read().unwrap();
        let dimensions = video_dimensions(&inner.video_mode);
        SourceSelection {
            source_id: inner.source_id.clone(),
            dimensions,
            av_video_index: inner.av_video_index,
            av_audio_index: inner.av_audio_index,
            av_video_name: inner.av_video_name.clone(),
            av_audio_name: inner.av_audio_name.clone(),
            pipe_path: inner.pipe_path.clone(),
            ndi_source_name: inner.ndi_source_name.clone(),
            relay_bind_host: inner.relay_bind_host.clone(),
            relay_srt_port: inner.relay_srt_port,
            relay_srt_latency_us: inner.relay_srt_latency_us,
            relay_srt_passphrase: inner.relay_srt_passphrase.clone(),
            relay_rtmp_port: inner.relay_rtmp_port,
            relay_rtmp_app: inner.relay_rtmp_app.clone(),
            relay_rtmp_key: inner.relay_rtmp_key.clone(),
        }
    }

    /// Update the human-friendly device label (the bmd_name in SRT
    /// streamids and the IDENTITY block's Label field). The BMD
    /// control protocol's IDENTITY handler calls this when a control
    /// client sends `Label: ...`.
    pub fn set_label(&self, label: &str) {
        let mut inner = self.inner.write().unwrap();
        inner.label = label.to_string();
    }

    /// Mutate the StreamStats in place under the write lock. Used by
    /// the Phase 3 streamer's monitor task to push telemetry updates
    /// (bitrate, fps, frames_sent, etc.) atomically and cheaply
    /// (~200 ns per call).
    pub fn stats_in_place<F: FnOnce(&mut StreamStats)>(&self, f: F) {
        let mut inner = self.inner.write().unwrap();
        f(&mut inner.stats);
    }

    /// Set the AVFoundation / DirectShow defaults from a fresh device
    /// scan. Called once at boot from Tauri's setup() so the source
    /// dropdowns have something selected on first launch.
    pub fn apply_default_devices(&self, video_index: i32, video_name: &str, audio_index: i32, audio_name: &str) {
        let mut inner = self.inner.write().unwrap();
        inner.av_video_index = video_index;
        inner.av_video_name = video_name.to_string();
        inner.av_audio_index = audio_index;
        inner.av_audio_name = audio_name.to_string();
    }

    /// Apply a partial settings update from the HTTP API. Each `Some`
    /// field overwrites the corresponding inner field; `None` leaves it
    /// unchanged. Validation happens here — unknown video modes / codecs
    /// are silently rejected (Python's behavior).
    pub fn apply_settings(&self, update: &SettingsUpdate) {
        let mut inner = self.inner.write().unwrap();
        if let Some(v) = &update.video_mode {
            if AVAILABLE_VIDEO_MODES.contains(&v.as_str()) {
                inner.video_mode = v.clone();
            }
        }
        if let Some(v) = &update.quality_level {
            inner.quality_level = v.clone();
        }
        if let Some(v) = &update.source_id {
            inner.source_id = v.clone();
        }
        if let Some(v) = &update.custom_url {
            inner.custom_url = v.clone();
        }
        if let Some(v) = &update.stream_key {
            inner.stream_key = v.clone();
        }
        if let Some(v) = &update.passphrase {
            inner.passphrase = v.clone();
        }
        if let Some(v) = &update.srt_mode {
            if matches!(v.as_str(), "caller" | "listener" | "rendezvous") {
                inner.srt_mode = v.clone();
            }
        }
        if let Some(v) = update.srt_latency_us {
            inner.srt_latency_us = v;
        }
        if let Some(v) = update.srt_listen_port {
            inner.srt_listen_port = v;
        }
        if let Some(v) = &update.streamid_override {
            inner.streamid_override = v.clone();
        }
        if let Some(v) = update.streamid_legacy {
            inner.streamid_legacy = v;
        }
        if let Some(v) = &update.video_codec {
            if matches!(v.as_str(), "h264" | "h265") {
                inner.video_codec = v.clone();
            }
        }
        if let Some(v) = &update.current_service_name {
            if inner.services.contains_key(v) {
                inner.current_service_name = v.clone();
                if let Some(svc) = inner.services.get(v) {
                    inner.stream_key = svc.key.clone();
                }
            }
        }
        if let Some(v) = &update.current_server_name {
            inner.current_server_name = v.clone();
        }
        if let Some(v) = &update.ndi_source_name {
            inner.ndi_source_name = v.clone();
        }
        if let Some(v) = update.av_video_index {
            inner.av_video_index = v;
        }
        if let Some(v) = &update.av_video_name {
            inner.av_video_name = v.clone();
        }
        if let Some(v) = update.av_audio_index {
            inner.av_audio_index = v;
        }
        if let Some(v) = &update.av_audio_name {
            inner.av_audio_name = v.clone();
        }
        if let Some(v) = update.audio_pan_l {
            inner.audio_pan_l = v.max(1);
        }
        if let Some(v) = update.audio_pan_r {
            inner.audio_pan_r = v.max(1);
        }
        if let Some(v) = &update.pipe_path {
            inner.pipe_path = v.clone();
        }
        if let Some(v) = &update.label {
            inner.label = v.clone();
        }
        if let Some(r) = &update.relay {
            if let Some(v) = &r.bind_host { inner.relay_bind_host = v.clone(); }
            if let Some(v) = r.srt_port { inner.relay_srt_port = v; }
            if let Some(v) = r.srt_latency_us { inner.relay_srt_latency_us = v; }
            if let Some(v) = &r.srt_passphrase { inner.relay_srt_passphrase = v.clone(); }
            if let Some(v) = r.rtmp_port { inner.relay_rtmp_port = v; }
            if let Some(v) = &r.rtmp_app { inner.relay_rtmp_app = v.clone(); }
            if let Some(v) = &r.rtmp_key { inner.relay_rtmp_key = v.clone(); }
        }
        if let Some(o) = &update.overlay {
            if let Some(v) = &o.title { inner.overlay_title = v.clone(); }
            if let Some(v) = &o.subtitle { inner.overlay_subtitle = v.clone(); }
            if let Some(v) = &o.logo_path { inner.overlay_logo_path = v.clone(); }
            if let Some(v) = o.clock { inner.overlay_clock = v; }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = self.inner.read().unwrap();
        let svc = inner.services.get(&inner.current_service_name);
        let active = current_active_server(&inner);

        let available_servers: Vec<ServerSnapshot> = svc
            .map(|s| {
                s.servers
                    .iter()
                    .map(|sv| ServerSnapshot {
                        name: sv.name.clone(),
                        url: sv.url.clone(),
                        protocol: sv.protocol(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let available_services: Vec<String> = inner.services.keys().cloned().collect();
        let (_, height, fps) = video_dimensions(&inner.video_mode);
        let res = if height == 1080 { "1080p" } else { "720p" };

        // Quality levels + per-mode bitrates. Source the active
        // service's profile data when one is loaded; otherwise fall
        // back to the BMD-spec defaults so the UI chooser is always
        // populated and the user can stream against a manually-
        // entered destination without first loading an XML.
        let (available_quality_levels, quality_options): (Vec<String>, Vec<QualityOption>) =
            if let Some(s) = svc {
                let names: Vec<String> = s.profiles.iter().map(|p| p.name.clone()).collect();
                let opts: Vec<QualityOption> = s
                    .profiles
                    .iter()
                    .map(|p| {
                        let bitrate = p
                            .find_config(res, fps)
                            .map(|c| c.bitrate)
                            .or_else(|| p.configs.iter().max_by_key(|c| c.bitrate).map(|c| c.bitrate))
                            .unwrap_or(0);
                        QualityOption {
                            name: p.name.clone(),
                            bitrate,
                        }
                    })
                    .collect();
                (names, opts)
            } else {
                let names: Vec<String> = DEFAULT_QUALITY_LEVELS.iter().map(|s| s.to_string()).collect();
                let opts: Vec<QualityOption> = DEFAULT_QUALITY_LEVELS
                    .iter()
                    .map(|q| QualityOption {
                        name: (*q).to_string(),
                        bitrate: default_bitrate(q, res, fps).unwrap_or(0),
                    })
                    .collect();
                (names, opts)
            };

        let active_config = resolve_active_config(&inner).map(|cfg| ActiveConfig {
            resolution: cfg.resolution.clone(),
            fps: cfg.fps,
            codec: cfg.codec.clone(),
            bitrate: cfg.bitrate,
            audio_bitrate: cfg.audio_bitrate,
            keyframe_interval: cfg.keyframe_interval,
        });

        Snapshot {
            label: inner.label.clone(),
            model: inner.model.clone(),
            unique_id: inner.unique_id.clone(),
            device_uuid: inner.device_uuid.clone(),
            video_mode: inner.video_mode.clone(),
            quality_level: inner.quality_level.clone(),
            source_id: inner.source_id.clone(),
            available_video_modes: AVAILABLE_VIDEO_MODES.iter().map(|s| s.to_string()).collect(),
            available_quality_levels,
            quality_options,
            available_services,
            available_servers,
            current_service_name: inner.current_service_name.clone(),
            current_server_name: inner.current_server_name.clone(),
            current_url: active.as_ref().map(|a| a.1.clone()).unwrap_or_default(),
            current_protocol: active.as_ref().map(|a| a.2.clone()).unwrap_or_default(),
            custom_url: inner.custom_url.clone(),
            stream_key: inner.stream_key.clone(),
            passphrase: inner.passphrase.clone(),
            srt_mode: inner.srt_mode.clone(),
            srt_latency_us: inner.srt_latency_us,
            srt_listen_port: inner.srt_listen_port,
            streamid_override: inner.streamid_override.clone(),
            streamid_legacy: inner.streamid_legacy,
            video_codec: inner.video_codec.clone(),
            av_video_index: inner.av_video_index,
            av_audio_index: inner.av_audio_index,
            av_video_name: inner.av_video_name.clone(),
            av_audio_name: inner.av_audio_name.clone(),
            audio_pan_l: inner.audio_pan_l,
            audio_pan_r: inner.audio_pan_r,
            pipe_path: inner.pipe_path.clone(),
            ndi_source_name: inner.ndi_source_name.clone(),
            relay: RelaySnapshot {
                bind_host: inner.relay_bind_host.clone(),
                srt_port: inner.relay_srt_port,
                srt_latency_us: inner.relay_srt_latency_us,
                srt_passphrase: inner.relay_srt_passphrase.clone(),
                rtmp_port: inner.relay_rtmp_port,
                rtmp_app: inner.relay_rtmp_app.clone(),
                rtmp_key: inner.relay_rtmp_key.clone(),
            },
            overlay: OverlaySnapshot {
                title: inner.overlay_title.clone(),
                subtitle: inner.overlay_subtitle.clone(),
                logo_path: inner.overlay_logo_path.clone(),
                clock: inner.overlay_clock,
            },
            active_config,
            stats: StatsSnapshot {
                status: inner.stats.status.clone(),
                bitrate: inner.stats.bitrate,
                duration: inner.stats.duration_string(),
                cache_used: inner.stats.cache_used_pct,
                error: inner.stats.error.clone(),
                fps: round1(inner.stats.fps),
                speed: round2(inner.stats.speed),
                frames_sent: inner.stats.frames_sent,
                frames_dropped: inner.stats.frames_dropped,
                quality: round1(inner.stats.quality),
            },
        }
    }
}

fn current_active_server(inner: &Inner) -> Option<(String, String, String)> {
    if !inner.custom_url.is_empty() {
        let scheme = inner
            .custom_url
            .split_once("://")
            .map(|(s, _)| s.to_lowercase())
            .unwrap_or_default();
        if matches!(scheme.as_str(), "srt" | "rtmp" | "rtmps") {
            return Some(("Custom".into(), inner.custom_url.clone(), scheme));
        }
    }
    let svc = inner.services.get(&inner.current_service_name)?;
    if svc.servers.is_empty() {
        return None;
    }
    if let Some(s) = svc.servers.iter().find(|s| s.name == inner.current_server_name) {
        return Some((s.name.clone(), s.url.clone(), s.protocol()));
    }
    if let Some(s) = svc.srt_servers().first() {
        return Some(((*s).name.clone(), (*s).url.clone(), (*s).protocol()));
    }
    let s = &svc.servers[0];
    Some((s.name.clone(), s.url.clone(), s.protocol()))
}

fn resolve_active_config(inner: &Inner) -> Option<StreamConfig> {
    let (_, height, fps) = video_dimensions(&inner.video_mode);
    let res = if height == 1080 { "1080p" } else { "720p" };

    // Service-loaded path: pick the matching profile + config.
    if let Some(svc) = inner.services.get(&inner.current_service_name) {
        if let Some(profile) = svc
            .find_profile(&inner.quality_level)
            .or_else(|| svc.get_default_profile())
        {
            if let Some(cfg) = profile
                .find_config(res, fps)
                .or_else(|| profile.configs.iter().max_by_key(|c| c.bitrate))
            {
                return Some(cfg.clone());
            }
        }
    }

    // No-service fallback: synthesize a config from the default
    // BMD-spec quality matrix so the user can stream against a
    // manually-entered destination. quality_level may be a default
    // ("Streaming High") or a stale name from a previous XML — we
    // try the explicit value first, then High as the safest default.
    let codec_str = if inner.video_codec == "h264" { "H264" } else { "H265" };
    let bitrate = default_bitrate(&inner.quality_level, res, fps)
        .or_else(|| default_bitrate("Streaming High", res, fps))?;
    Some(StreamConfig {
        resolution: res.to_string(),
        fps,
        codec: codec_str.to_string(),
        bitrate,
        audio_bitrate: DEFAULT_AUDIO_BITRATE,
        keyframe_interval: DEFAULT_KEYFRAME_INTERVAL,
    })
}

/// Returns (width, height, fps) for a "1080p59.94"-style video mode string.
/// Falls back to 1080p30 for "Auto" or unknown modes. Fractional rates round
/// up (29.97 -> 30, 59.94 -> 60) to match FFmpeg's GOP math.
pub fn video_dimensions(mode: &str) -> (u32, u32, u32) {
    let normalized = if mode == "Auto" || !AVAILABLE_VIDEO_MODES.contains(&mode) {
        "1080p30"
    } else {
        mode
    };
    let Some((h, fps)) = normalized.split_once('p') else {
        return (1920, 1080, 30);
    };
    let height: u32 = h.parse().unwrap_or(1080);
    // 4K UHD (3840x2160) for the 4K ATEM switchers (Constellation 4K,
    // upcoming ST2110 hardware), 1080p for the HD line, 720p for older
    // gear and the original Mini.
    let width = match height {
        2160 => 3840,
        1080 => 1920,
        _ => 1280,
    };
    let fps = fps.parse::<f32>().unwrap_or(30.0).round() as u32;
    (width, height, fps)
}

fn round1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

fn round2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}

// ---- Source selection view -------------------------------------------------
//
// A frozen snapshot of just the source-relevant fields, so sources::
// resolve_source can build an FFmpeg command without holding the
// EncoderState lock during the (slow, IO-heavy) device probe.

#[derive(Debug, Clone)]
pub struct SourceSelection {
    pub source_id: String,
    pub dimensions: (u32, u32, u32),
    pub av_video_index: i32,
    pub av_audio_index: i32,
    pub av_video_name: String,
    pub av_audio_name: String,
    pub pipe_path: String,
    pub ndi_source_name: String,
    pub relay_bind_host: String,
    pub relay_srt_port: u16,
    pub relay_srt_latency_us: u32,
    pub relay_srt_passphrase: String,
    pub relay_rtmp_port: u16,
    pub relay_rtmp_app: String,
    pub relay_rtmp_key: String,
}

// ---- Settings update DTO ---------------------------------------------------
//
// Mirrors the subset of EncoderState fields the HTTP API can mutate.
// `None` means "don't touch this field." Wired into http.rs via a
// `From<SettingsPayload>` impl over there.

#[derive(Debug, Default)]
pub struct SettingsUpdate {
    pub video_mode: Option<String>,
    pub quality_level: Option<String>,
    pub source_id: Option<String>,
    pub custom_url: Option<String>,
    pub stream_key: Option<String>,
    pub passphrase: Option<String>,
    pub srt_mode: Option<String>,
    pub srt_latency_us: Option<u32>,
    pub srt_listen_port: Option<u16>,
    pub streamid_override: Option<String>,
    pub streamid_legacy: Option<bool>,
    pub video_codec: Option<String>,
    pub current_service_name: Option<String>,
    pub current_server_name: Option<String>,
    pub ndi_source_name: Option<String>,
    // AVF / DirectShow device selection — Phase 8b fix.
    pub av_video_index: Option<i32>,
    pub av_video_name: Option<String>,
    pub av_audio_index: Option<i32>,
    pub av_audio_name: Option<String>,
    // Multi-channel audio L/R routing (Dante VSC, CoreAudio aggregate).
    pub audio_pan_l: Option<u8>,
    pub audio_pan_r: Option<u8>,
    // Pipe / URL source path.
    pub pipe_path: Option<String>,
    // Device label (IDENTITY's Label, also bmd_name in streamid).
    pub label: Option<String>,
    // Nested relay + overlay sub-structs. Each field optional so a
    // partial update from the UI doesn't clobber unrelated values.
    pub relay: Option<RelaySettingsUpdate>,
    pub overlay: Option<OverlaySettingsUpdate>,
}

#[derive(Debug, Default)]
pub struct RelaySettingsUpdate {
    pub bind_host: Option<String>,
    pub srt_port: Option<u16>,
    pub srt_latency_us: Option<u32>,
    pub srt_passphrase: Option<String>,
    pub rtmp_port: Option<u16>,
    pub rtmp_app: Option<String>,
    pub rtmp_key: Option<String>,
}

#[derive(Debug, Default)]
pub struct OverlaySettingsUpdate {
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub logo_path: Option<String>,
    pub clock: Option<bool>,
}

// ---- Snapshot DTO ----------------------------------------------------------
//
// Matches the JSON shape that `bmd_emulator/static/app.js` reads from
// `/api/state`. Field names use snake_case to match the Python dict keys
// exactly — no serde rename needed.

#[derive(Serialize, Debug)]
pub struct Snapshot {
    pub label: String,
    pub model: String,
    pub unique_id: String,
    pub device_uuid: String,
    pub video_mode: String,
    pub quality_level: String,
    pub source_id: String,
    pub available_video_modes: Vec<String>,
    pub available_quality_levels: Vec<String>,
    pub quality_options: Vec<QualityOption>,
    pub available_services: Vec<String>,
    pub available_servers: Vec<ServerSnapshot>,
    pub current_service_name: String,
    pub current_server_name: String,
    pub current_url: String,
    pub current_protocol: String,
    pub custom_url: String,
    pub stream_key: String,
    pub passphrase: String,
    pub srt_mode: String,
    pub srt_latency_us: u32,
    pub srt_listen_port: u16,
    pub streamid_override: String,
    pub streamid_legacy: bool,
    pub video_codec: String,
    pub av_video_index: i32,
    pub av_audio_index: i32,
    pub av_video_name: String,
    pub av_audio_name: String,
    pub audio_pan_l: u8,
    pub audio_pan_r: u8,
    pub pipe_path: String,
    pub ndi_source_name: String,
    pub relay: RelaySnapshot,
    pub overlay: OverlaySnapshot,
    pub active_config: Option<ActiveConfig>,
    pub stats: StatsSnapshot,
}

#[derive(Serialize, Debug)]
pub struct QualityOption {
    pub name: String,
    pub bitrate: u64,
}

#[derive(Serialize, Debug)]
pub struct ServerSnapshot {
    pub name: String,
    pub url: String,
    pub protocol: String,
}

#[derive(Serialize, Debug)]
pub struct RelaySnapshot {
    pub bind_host: String,
    pub srt_port: u16,
    pub srt_latency_us: u32,
    pub srt_passphrase: String,
    pub rtmp_port: u16,
    pub rtmp_app: String,
    pub rtmp_key: String,
}

#[derive(Serialize, Debug)]
pub struct OverlaySnapshot {
    pub title: String,
    pub subtitle: String,
    pub logo_path: String,
    pub clock: bool,
}

#[derive(Serialize, Debug)]
pub struct ActiveConfig {
    pub resolution: String,
    pub fps: u32,
    pub codec: String,
    pub bitrate: u64,
    pub audio_bitrate: u64,
    pub keyframe_interval: u32,
}

#[derive(Serialize, Debug)]
pub struct StatsSnapshot {
    pub status: String,
    pub bitrate: u64,
    pub duration: String,
    pub cache_used: u32,
    pub error: Option<String>,
    pub fps: f32,
    pub speed: f32,
    pub frames_sent: u64,
    pub frames_dropped: u64,
    pub quality: f32,
}
