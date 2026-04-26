use crate::ffmpeg_path::ffmpeg_path;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use std::collections::HashSet;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Serialize, Clone, Debug)]
pub struct Device {
    pub index: i32,
    pub name: String,
    #[serde(skip)]
    pub kind: DeviceKind,
    pub category: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    Video,
    Audio,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct DeviceList {
    pub video: Vec<Device>,
    pub audio: Vec<Device>,
    #[serde(skip)]
    pub scanned_at: Option<Instant>,
}

static CACHE: Lazy<Mutex<DeviceList>> = Lazy::new(|| Mutex::new(DeviceList::default()));

pub fn list_capture_devices(force: bool) -> DeviceList {
    let mut cache = CACHE.lock().unwrap();
    if !force {
        if let Some(scanned_at) = cache.scanned_at {
            if scanned_at.elapsed() < CACHE_TTL {
                return cache.clone();
            }
        }
    }
    let (video, audio) = if cfg!(target_os = "macos") {
        scan_avfoundation()
    } else if cfg!(target_os = "windows") {
        scan_dshow()
    } else {
        log::debug!("no capture-device scanner for this platform");
        (Vec::new(), Vec::new())
    };
    *cache = DeviceList {
        video,
        audio,
        scanned_at: Some(Instant::now()),
    };
    cache.clone()
}

// ---------------------------------------------------------------------------
// macOS — AVFoundation
// ---------------------------------------------------------------------------

static AVF_DEVICE_LINE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[[^\]]+\]\s*\[(\d+)\]\s*(.+)$").unwrap());

fn scan_avfoundation() -> (Vec<Device>, Vec<Device>) {
    let output = Command::new(ffmpeg_path())
        .args([
            "-hide_banner",
            "-f",
            "avfoundation",
            "-list_devices",
            "true",
            "-i",
            "",
        ])
        .stdout(std::process::Stdio::null())
        .output();
    let stderr = match output {
        Ok(out) => String::from_utf8_lossy(&out.stderr).into_owned(),
        Err(err) => {
            log::warn!("avfoundation scan failed: {err}");
            return (Vec::new(), Vec::new());
        }
    };
    parse_avfoundation(&stderr)
}

fn parse_avfoundation(text: &str) -> (Vec<Device>, Vec<Device>) {
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut section: Option<DeviceKind> = None;
    for line in text.lines() {
        let lower = line.to_lowercase();
        if lower.contains("avfoundation video devices") {
            section = Some(DeviceKind::Video);
            continue;
        }
        if lower.contains("avfoundation audio devices") {
            section = Some(DeviceKind::Audio);
            continue;
        }
        let Some(kind) = section else { continue };
        let Some(caps) = AVF_DEVICE_LINE.captures(line) else {
            continue;
        };
        let idx: i32 = caps.get(1).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        let name = caps.get(2).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
        let category = categorize_device(&name, kind);
        let dev = Device {
            index: idx,
            name,
            kind,
            category,
        };
        match kind {
            DeviceKind::Video => video.push(dev),
            DeviceKind::Audio => audio.push(dev),
        }
    }
    (video, audio)
}

// ---------------------------------------------------------------------------
// Windows — DirectShow
// ---------------------------------------------------------------------------

static DSHOW_DEVICE_LINE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"\[(?:dshow|in#\d+) @ [^\]]+\]\s+"([^"]+)"(?:\s+\(([^)]*)\))?\s*$"#).unwrap()
});

fn scan_dshow() -> (Vec<Device>, Vec<Device>) {
    let output = Command::new(ffmpeg_path())
        .args([
            "-hide_banner",
            "-f",
            "dshow",
            "-list_devices",
            "true",
            "-i",
            "dummy",
        ])
        .stdout(std::process::Stdio::null())
        .output();
    let stderr = match output {
        Ok(out) => String::from_utf8_lossy(&out.stderr).into_owned(),
        Err(err) => {
            log::warn!("dshow scan failed: {err}");
            return (Vec::new(), Vec::new());
        }
    };
    let (mut video, audio) = parse_dshow(&stderr);

    // Prepend the synthetic "Capture screen 0" entry so the UI's
    // screen-capture tile has something to bind to. Bumps every other
    // video index by one, matching v0.1.0 behavior. The source resolver
    // routes this entry through gdigrab instead of dshow.
    for d in video.iter_mut() {
        d.index += 1;
    }
    let desktop = Device {
        index: 0,
        name: "Capture screen 0".into(),
        kind: DeviceKind::Video,
        category: "screen".into(),
    };
    video.insert(0, desktop);
    (video, audio)
}

fn parse_dshow(text: &str) -> (Vec<Device>, Vec<Device>) {
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut section: Option<DeviceKind> = None;
    let mut v_idx = 0;
    let mut a_idx = 0;
    for line in text.lines() {
        let lower = line.to_lowercase();
        if lower.contains("directshow video devices") {
            section = Some(DeviceKind::Video);
            continue;
        }
        if lower.contains("directshow audio devices") {
            section = Some(DeviceKind::Audio);
            continue;
        }
        if lower.contains("alternative name") {
            continue;
        }
        let Some(caps) = DSHOW_DEVICE_LINE.captures(line) else {
            continue;
        };
        let name = caps.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
        let kinds = resolve_dshow_kinds(caps.get(2).map(|m| m.as_str()), section);
        if kinds.contains(&DeviceKind::Video) {
            let cat = categorize_device(&name, DeviceKind::Video);
            video.push(Device {
                index: v_idx,
                name: name.clone(),
                kind: DeviceKind::Video,
                category: cat,
            });
            v_idx += 1;
        }
        if kinds.contains(&DeviceKind::Audio) {
            let cat = categorize_device(&name, DeviceKind::Audio);
            audio.push(Device {
                index: a_idx,
                name,
                kind: DeviceKind::Audio,
                category: cat,
            });
            a_idx += 1;
        }
    }
    (video, audio)
}

/// Translate a dshow inline kind marker (or legacy section header) to
/// the set of kinds that apply. Modern FFmpeg emits `video`, `audio`,
/// `audio, video` (combined-input capture devices like Blackmagic WDM),
/// `video, audio` (same, reversed), or `none` (FFmpeg can't determine
/// the type — typically OBS Virtual Camera). Combined markers map to
/// both lists; `none` is treated as video on the practical observation
/// that virtually every real-world `(none)` device is a virtual camera.
fn resolve_dshow_kinds(marker: Option<&str>, section: Option<DeviceKind>) -> HashSet<DeviceKind> {
    let mut out = HashSet::new();
    let Some(marker) = marker else {
        if let Some(s) = section {
            out.insert(s);
        }
        return out;
    };
    let parts: HashSet<String> = marker
        .split(',')
        .map(|p| p.trim().to_lowercase())
        .collect();
    if parts.contains("video") {
        out.insert(DeviceKind::Video);
    }
    if parts.contains("audio") {
        out.insert(DeviceKind::Audio);
    }
    if parts.contains("none") && out.is_empty() {
        out.insert(DeviceKind::Video);
    }
    out
}

// ---------------------------------------------------------------------------
// Categorisation — buckets a device into a UI tile category
// ---------------------------------------------------------------------------

static VIDEO_CATEGORIES: Lazy<Vec<(&'static str, Regex)>> = Lazy::new(|| {
    vec![
        (
            "screen",
            Regex::new(r"(?i)capture screen|desk view|screen capture|desktop").unwrap(),
        ),
        (
            "capture_card",
            Regex::new(r"(?i)ultrastudio|decklink|intensity|aja|magewell|elgato|epiphan|wdm capture|blackmagic")
                .unwrap(),
        ),
        ("ndi", Regex::new(r"(?i)\bndi\b").unwrap()),
        ("iphone", Regex::new(r"(?i)iphone|ipad").unwrap()),
        (
            "virtual",
            Regex::new(r"(?i)virtual|obs|sysram|loopback|syphon|vmix").unwrap(),
        ),
    ]
});

static AUDIO_CATEGORIES: Lazy<Vec<(&'static str, Regex)>> = Lazy::new(|| {
    vec![
        ("ndi", Regex::new(r"(?i)\bndi\b").unwrap()),
        (
            "virtual",
            Regex::new(
                r"(?i)virtual|loopback|aggregate|blackhole|soundflower|background music|stereo mix|vmix",
            )
            .unwrap(),
        ),
    ]
});

pub fn categorize_device(name: &str, kind: DeviceKind) -> String {
    let pats: &[(&'static str, Regex)] = match kind {
        DeviceKind::Video => &VIDEO_CATEGORIES,
        DeviceKind::Audio => &AUDIO_CATEGORIES,
    };
    for (cat, pat) in pats {
        if pat.is_match(name) {
            return (*cat).to_string();
        }
    }
    match kind {
        DeviceKind::Video => "camera".into(),
        DeviceKind::Audio => "microphone".into(),
    }
}

// ---------------------------------------------------------------------------
// AVFoundation mode probe — find what (width, height, fps) the device
// natively supports so the source factory can pick a real mode instead
// of asking for one the device will reject.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct AvfMode {
    pub width: u32,
    pub height: u32,
    pub fps_lo: f64,
    pub fps_hi: f64,
}

static AVF_MODE_LINE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(\d+)x(\d+)@\[([\d.]+)\s+([\d.]+)\]fps").unwrap());

/// Probe an AVFoundation device for supported video modes. Asks FFmpeg
/// to capture at 1 fps (which the device will refuse), parsing the
/// resulting "Selected framerate not supported, supported modes are…"
/// listing.
pub fn probe_avf_modes(device_index: i32) -> Vec<AvfMode> {
    let output = Command::new(ffmpeg_path())
        .args([
            "-hide_banner",
            "-f",
            "avfoundation",
            "-framerate",
            "1",
            "-i",
            &device_index.to_string(),
            "-t",
            "0",
            "-f",
            "null",
            "-",
        ])
        .output();
    let stderr = match output {
        Ok(out) => String::from_utf8_lossy(&out.stderr).into_owned(),
        Err(err) => {
            log::warn!("AVF mode probe failed for device {device_index}: {err}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for line in stderr.lines() {
        if let Some(caps) = AVF_MODE_LINE.captures(line) {
            out.push(AvfMode {
                width: caps[1].parse().unwrap_or(0),
                height: caps[2].parse().unwrap_or(0),
                fps_lo: caps[3].parse().unwrap_or(0.0),
                fps_hi: caps[4].parse().unwrap_or(0.0),
            });
        }
    }
    out
}

/// Pick (width, height, fps) closest to the desired output. Strategy:
/// 1. Prefer modes matching the requested resolution exactly.
/// 2. Within those, prefer modes whose fps range covers the request.
/// 3. Otherwise pick the mode with the closest fps endpoint.
/// 4. If forced to compromise on fps, pick the mode's max fps —
///    high-rate input downsampled at the encoder is cleaner than
///    low-rate input upsampled.
pub fn pick_best_avf_mode(modes: &[AvfMode], want_w: u32, want_h: u32, want_fps: f64) -> (u32, u32, f64) {
    if modes.is_empty() {
        return (want_w, want_h, want_fps);
    }
    let matching: Vec<&AvfMode> = modes
        .iter()
        .filter(|m| m.width == want_w && m.height == want_h)
        .collect();
    let pool: Vec<&AvfMode> = if matching.is_empty() {
        modes.iter().collect()
    } else {
        matching
    };
    let best = pool
        .iter()
        .min_by(|a, b| {
            let sa = score(a, want_fps);
            let sb = score(b, want_fps);
            sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .copied()
        .copied()
        .unwrap();
    let chosen_fps = if best.fps_lo <= want_fps && want_fps <= best.fps_hi {
        want_fps
    } else {
        best.fps_hi
    };
    (best.width, best.height, chosen_fps)
}

fn score(m: &AvfMode, want_fps: f64) -> (i32, f64) {
    if m.fps_lo <= want_fps && want_fps <= m.fps_hi {
        (0, 0.0)
    } else {
        let diff = (m.fps_lo - want_fps).abs().min((m.fps_hi - want_fps).abs());
        (1, diff)
    }
}

// ---------------------------------------------------------------------------
// Default-device heuristics — used at boot to pick something sensible.
// ---------------------------------------------------------------------------

static AUDIO_PRIORITIES: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)macbook.*microphone").unwrap(),
        Regex::new(r"(?i)built.*microphone").unwrap(),
        Regex::new(r"(?i)^microphone$").unwrap(),
    ]
});
static AUDIO_SKIP: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(zoom|ndi audio|aggregate|dante|virtual|stereo mix)").unwrap()
});
static VIDEO_SKIP: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(virtual|capture screen|desk view|screen capture)").unwrap()
});

pub fn find_default_audio(devs: &DeviceList) -> Option<&Device> {
    for pat in AUDIO_PRIORITIES.iter() {
        for d in &devs.audio {
            if pat.is_match(&d.name) {
                return Some(d);
            }
        }
    }
    devs.audio.iter().find(|d| !AUDIO_SKIP.is_match(&d.name))
        .or(devs.audio.first())
}

pub fn find_default_video(devs: &DeviceList) -> Option<&Device> {
    for d in &devs.video {
        if d.name.to_lowercase().contains("facetime") {
            return Some(d);
        }
    }
    devs.video.iter().find(|d| !VIDEO_SKIP.is_match(&d.name))
        .or(devs.video.first())
}
