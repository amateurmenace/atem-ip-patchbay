use crate::ffmpeg_path::{ffmpeg_has_decklink, ffmpeg_path};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
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
    let mut cmd = Command::new(ffmpeg_path());
    cmd.args([
        "-hide_banner",
        "-f",
        "avfoundation",
        "-list_devices",
        "true",
        "-i",
        "",
    ])
    .stdout(std::process::Stdio::null());
    crate::ffmpeg_path::hide_console_std(&mut cmd);
    let output = cmd.output();
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
    let mut cmd = Command::new(ffmpeg_path());
    cmd.args([
        "-hide_banner",
        "-f",
        "dshow",
        "-list_devices",
        "true",
        "-i",
        "dummy",
    ])
    .stdout(std::process::Stdio::null());
    crate::ffmpeg_path::hide_console_std(&mut cmd);
    let output = cmd.output();
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
    let mut cmd = Command::new(ffmpeg_path());
    cmd.args([
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
    ]);
    crate::ffmpeg_path::hide_console_std(&mut cmd);
    let output = cmd.output();
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

// ---------------------------------------------------------------------------
// DeckLink — Session 12 bidirectional patchbay (output direction).
//
// Discovers Blackmagic DeckLink output devices via the FFmpeg
// decklink muxer, and probes per-device supported output modes.
// Gated on `ffmpeg_has_decklink()` so builds without
// `--enable-decklink` return empty without spending an ffmpeg
// invocation; the UI uses both signals (build-support absent vs.
// hardware/driver absent) to render distinct error states.
//
// Discovery is a single shared cache (60s TTL like
// list_capture_devices). Mode probing is a separate per-device
// cache populated lazily on first lookup; mirrors how
// probe_avf_modes is invoked from sources.rs.
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
pub struct DecklinkDevice {
    pub name: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct DecklinkMode {
    pub format_code: String,    // e.g. "Hp59"
    pub description: String,    // raw line from -list_formats, sans prefix
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub interlaced: bool,
}

static DECKLINK_DEVICE_CACHE: Lazy<Mutex<(Vec<DecklinkDevice>, Option<Instant>)>> =
    Lazy::new(|| Mutex::new((Vec::new(), None)));

static DECKLINK_MODE_CACHE: Lazy<Mutex<HashMap<String, Vec<DecklinkMode>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

// Captures the device-name (single-quoted) from FFmpeg's decklink
// device listing. Two prefix conventions seen in the wild:
//
//   FFmpeg < 8.x:  [decklink @ 0x...] 'DeckLink Mini Monitor 4K'
//   FFmpeg ≥ 8.x:  [in#0 @ 0x...]	'DeckLink Studio 4K'
//
// alpha.35 shipped the first DeckLink-enabled FFmpeg (n8.1.1, our
// own build via build-ffmpeg.yml); the new `[in#N @ ...]` prefix
// caused the original `[decklink @ ...]`-anchored regex to miss
// every device. Generalize to any bracketed prefix followed by a
// single-quoted payload. The only other lines that have ANY single
// quotes are the "Supported formats for 'NAME':" header (which is
// in a different parser) and bare error messages (no quotes), so
// matching any `[*] '*'` is safe.
static DECKLINK_DEVICE_LINE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\[[^\]]+\]\s+'([^']+)'").unwrap()
});

// Captures format-code + WxH + fps + interlace marker from the
// per-device -list_formats output. Two prefix conventions:
//
//   FFmpeg < 8.x:  [decklink @ 0x...]   Hp59           1920x1080 at 60000/1001 fps
//   FFmpeg ≥ 8.x:  \tHp59\t\t1920x1080 at 60000/1001 fps
//
// In the new format, FFmpeg drops the bracket prefix entirely on
// the per-format rows and emits them with just tab indentation.
// Make the bracket optional. False-positive risk is low: the
// "format_code description" header row is caught by the explicit
// check in parse_decklink_modes; other lines lack the "WxH at
// N/D fps" structure entirely.
static DECKLINK_FORMAT_LINE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^\s*(?:\[[^\]]+\]\s+)?(\S+)\s+(\d+)x(\d+)\s+at\s+(\d+)/(\d+)\s+fps(.*)$",
    )
    .unwrap()
});

/// Return the set of DeckLink output devices visible to FFmpeg right
/// now. Returns an empty Vec if the FFmpeg build lacks DeckLink
/// support OR the Blackmagic DeckLink Driver isn't installed OR no
/// cards are connected — the UI distinguishes these by also reading
/// `ffmpeg_has_decklink()` separately.
pub fn list_decklink_outputs(force: bool) -> Vec<DecklinkDevice> {
    let mut cache = DECKLINK_DEVICE_CACHE.lock().unwrap();
    if !force {
        if let Some(scanned_at) = cache.1 {
            if scanned_at.elapsed() < CACHE_TTL {
                return cache.0.clone();
            }
        }
    }
    let devices = if ffmpeg_has_decklink() {
        scan_decklink_devices()
    } else {
        Vec::new()
    };
    // Card add/remove invalidates every per-device mode probe too.
    DECKLINK_MODE_CACHE.lock().unwrap().clear();
    *cache = (devices.clone(), Some(Instant::now()));
    devices
}

/// Return the list of supported output modes for the named DeckLink
/// device, populating the cache on first lookup. Empty Vec on
/// unknown device, parse failure, or missing DeckLink build support.
pub fn probe_decklink_modes(device_name: &str) -> Vec<DecklinkMode> {
    {
        let cache = DECKLINK_MODE_CACHE.lock().unwrap();
        if let Some(modes) = cache.get(device_name) {
            return modes.clone();
        }
    }
    if !ffmpeg_has_decklink() {
        return Vec::new();
    }
    let modes = scan_decklink_modes(device_name);
    DECKLINK_MODE_CACHE
        .lock()
        .unwrap()
        .insert(device_name.to_string(), modes.clone());
    modes
}

fn scan_decklink_devices() -> Vec<DecklinkDevice> {
    let mut cmd = Command::new(ffmpeg_path());
    cmd.args([
        "-hide_banner",
        "-f",
        "decklink",
        "-list_devices",
        "true",
        "-i",
        "dummy",
    ])
    .stdout(std::process::Stdio::null());
    crate::ffmpeg_path::hide_console_std(&mut cmd);
    let stderr = match cmd.output() {
        Ok(out) => String::from_utf8_lossy(&out.stderr).into_owned(),
        Err(err) => {
            log::warn!("decklink device scan failed: {err}");
            return Vec::new();
        }
    };
    parse_decklink_devices(&stderr)
}

fn parse_decklink_devices(text: &str) -> Vec<DecklinkDevice> {
    let mut devices = Vec::new();
    let mut seen = HashSet::new();
    for line in text.lines() {
        // The header line "Blackmagic DeckLink devices:" itself
        // matches our regex (it's wrapped in the [decklink @ ...]
        // prefix on some builds, naked on others). Skip any name
        // that's the header phrase.
        let Some(caps) = DECKLINK_DEVICE_LINE.captures(line) else {
            continue;
        };
        let name = caps[1].trim().to_string();
        if name.is_empty()
            || name.eq_ignore_ascii_case("Blackmagic DeckLink devices")
            || seen.contains(&name)
        {
            continue;
        }
        seen.insert(name.clone());
        devices.push(DecklinkDevice { name });
    }
    devices
}

fn scan_decklink_modes(device_name: &str) -> Vec<DecklinkMode> {
    let mut cmd = Command::new(ffmpeg_path());
    cmd.args([
        "-hide_banner",
        "-f",
        "decklink",
        "-list_formats",
        "1",
        "-i",
        device_name,
    ])
    .stdout(std::process::Stdio::null());
    crate::ffmpeg_path::hide_console_std(&mut cmd);
    let stderr = match cmd.output() {
        Ok(out) => String::from_utf8_lossy(&out.stderr).into_owned(),
        Err(err) => {
            log::warn!("decklink mode probe for {device_name:?} failed: {err}");
            return Vec::new();
        }
    };
    parse_decklink_modes(&stderr)
}

fn parse_decklink_modes(text: &str) -> Vec<DecklinkMode> {
    let mut modes = Vec::new();
    for line in text.lines() {
        let Some(caps) = DECKLINK_FORMAT_LINE.captures(line) else {
            continue;
        };
        let format_code = caps[1].to_string();
        // The header row of the formats table has `format_code` as
        // its first column literally — skip it.
        if format_code.eq_ignore_ascii_case("format_code") {
            continue;
        }
        let width: u32 = caps[2].parse().unwrap_or(0);
        let height: u32 = caps[3].parse().unwrap_or(0);
        let fps_num: u32 = caps[4].parse().unwrap_or(0);
        let fps_den: u32 = caps[5].parse().unwrap_or(1).max(1);
        let trailing = caps.get(7).map(|m| m.as_str()).unwrap_or("");
        let interlaced = trailing.to_lowercase().contains("interlaced");
        let description = line
            .splitn(2, ']')
            .nth(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| line.trim().to_string());
        if width == 0 || height == 0 || fps_num == 0 {
            continue;
        }
        modes.push(DecklinkMode {
            format_code,
            description,
            width,
            height,
            fps_num,
            fps_den,
            interlaced,
        });
    }
    modes
}

#[cfg(test)]
mod decklink_tests {
    use super::*;

    #[test]
    fn parse_devices_from_typical_output() {
        // What `-f decklink -list_devices true -i dummy` writes
        // on a machine with two cards. The "dummy:" trailing line
        // is an immediate-exit error FFmpeg always prints.
        let text = "\
[decklink @ 0x600003b1c000] Blackmagic DeckLink devices:
[decklink @ 0x600003b1c000] 'DeckLink Mini Monitor 4K'
[decklink @ 0x600003b1c000] 'DeckLink 8K Pro (1)'
[decklink @ 0x600003b1c000] 'DeckLink 8K Pro (2)'
dummy: Immediate exit requested
";
        let devices = parse_decklink_devices(text);
        let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "DeckLink Mini Monitor 4K",
                "DeckLink 8K Pro (1)",
                "DeckLink 8K Pro (2)",
            ]
        );
    }

    #[test]
    fn parse_modes_skips_header_row() {
        let text = "\
[decklink @ 0x600003b1c000] Supported formats for 'DeckLink Mini Monitor 4K':
[decklink @ 0x600003b1c000]   format_code  description
[decklink @ 0x600003b1c000]   ntsc         720x486 at 30000/1001 fps (interlaced, lower field first)
[decklink @ 0x600003b1c000]   Hp59         1920x1080 at 60000/1001 fps
[decklink @ 0x600003b1c000]   Hp60         1920x1080 at 60000/1000 fps
";
        let modes = parse_decklink_modes(text);
        assert_eq!(modes.len(), 3);
        assert_eq!(modes[0].format_code, "ntsc");
        assert!(modes[0].interlaced);
        assert_eq!(modes[1].format_code, "Hp59");
        assert_eq!(modes[1].width, 1920);
        assert_eq!(modes[1].height, 1080);
        assert_eq!(modes[1].fps_num, 60000);
        assert_eq!(modes[1].fps_den, 1001);
        assert!(!modes[1].interlaced);
    }

    #[test]
    fn parse_devices_empty_on_no_decklink_output() {
        // What we see when ffmpeg lacks decklink support OR the
        // driver is missing — the prefix lines just aren't there.
        assert!(parse_decklink_devices("").is_empty());
        assert!(parse_decklink_devices("ffmpeg version blah blah").is_empty());
    }

    #[test]
    fn parse_devices_n8_1_1_format() {
        // FFmpeg n8.1.1 (the first FFmpeg shipped in alpha.35 with
        // --enable-decklink, our own build via build-ffmpeg.yml)
        // changed the device-list log prefix from `[decklink @ ...]`
        // to `[in#0 @ ...]` and added a deprecation warning above
        // the device list. The original prefix-anchored regex
        // missed every device, so the UI showed "No DeckLink devices
        // found" on alpha.35 even though the cards were healthy.
        // Live capture below is from the Broadcast Pix test rig
        // (DeckLink Studio 4K + multiple NDI Machine virtual cards).
        let text = "\
[Blackmagic DeckLink indev @ 0x295c995dd00] The \"list_devices\" option is deprecated: use ffmpeg -sources decklink instead
[in#0 @ 0x295c995d5c0] Blackmagic DeckLink input devices:
[in#0 @ 0x295c995d5c0] \t'DeckLink Studio 4K'
[in#0 @ 0x295c995d5c0] \t'NDI Machine'
[in#0 @ 0x295c995d5c0] \t'NDI Machine 2'
[in#0 @ 0x295c995d5c0] \t'DeckLink 8K Pro (2)'
";
        let devices = parse_decklink_devices(text);
        let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "DeckLink Studio 4K",
                "NDI Machine",
                "NDI Machine 2",
                "DeckLink 8K Pro (2)",
            ]
        );
    }

    #[test]
    fn parse_modes_n8_1_1_format() {
        // FFmpeg n8.1.1's -list_formats output drops the bracket
        // prefix on per-format rows entirely and indents with tabs.
        // The header row is `\tformat_code\tdescription` — it
        // doesn't match the regex's required "WxH at N/D fps"
        // structure so it's skipped automatically (the explicit
        // format_code check is belt-and-suspenders).
        let text = "\
[in#0 @ 0x2bd4a16d5c0] Supported formats for 'DeckLink Studio 4K':
\tformat_code\tdescription
\tntsc\t\t720x486 at 30000/1001 fps (interlaced, lower field first)
\tHp59\t\t1920x1080 at 60000/1001 fps
\tHp60\t\t1920x1080 at 60000/1000 fps
";
        let modes = parse_decklink_modes(text);
        assert_eq!(modes.len(), 3);
        assert_eq!(modes[0].format_code, "ntsc");
        assert!(modes[0].interlaced);
        assert_eq!(modes[1].format_code, "Hp59");
        assert_eq!(modes[1].width, 1920);
        assert_eq!(modes[1].height, 1080);
        assert_eq!(modes[1].fps_num, 60000);
        assert_eq!(modes[1].fps_den, 1001);
        assert!(!modes[1].interlaced);
    }
}
