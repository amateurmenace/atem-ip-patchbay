//! decklink-spike — Session 16 Phase A diagnostic.
//!
//! Spawns N concurrent FFmpeg children, each driving an
//! `lavfi testsrc2 + anullsrc → format=uyvy422 → wrapped_avframe +
//! pcm_s16le → -f decklink` pipeline against a distinct DeckLink
//! output device. Tracks each child's per-second fps via the
//! progress lines in stderr, prints a status grid every 5 seconds,
//! and surfaces a final PASS/FAIL verdict.
//!
//! Purpose: answer "can a single host drive 8 concurrent
//! decklink_enc instances without driver contention or SDK
//! starvation" on the operator's real rig. The answer drives
//! Phase B's process-model decision:
//!   PASS → Option A (single Tauri process, EncoderFleet
//!          TILE_COUNT=8, per-tile supervisor isolation).
//!   FAIL → Option B (N separate Tauri processes via the existing
//!          --instance-name flag + a coordinator).
//!
//! Why testsrc2 and not real NDI ingest: isolates the failure
//! domain. If 8 testsrc2-fed FFmpegs survive, the decklink driver
//! handles 8 concurrent producers fine. If they fall over, adding
//! NDI receive on top wouldn't have helped. Conversely, if testsrc2
//! works but NDI doesn't, that's a separate (NDI-side) problem to
//! debug.
//!
//! Codec config matches alpha.37 production exactly:
//!   -c:v wrapped_avframe -c:a pcm_s16le -ar 48000 -ac 2
//!   -vf format=uyvy422
//!   -f decklink -format_code <code> <device>

use std::env;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use regex::Regex;

const DEFAULT_DURATION_SECS: u64 = 300;
const DEFAULT_COUNT: usize = 8;
const DEFAULT_FORMAT_CODE: &str = "Hp29";
const REPORT_INTERVAL_SECS: u64 = 5;
const NO_PROGRESS_TIMEOUT_SECS: u64 = 15;

fn main() {
    let args: Vec<String> = env::args().collect();

    let want_help = args.iter().any(|a| a == "--help" || a == "-h");
    let want_list_only = args.iter().any(|a| a == "--list");
    let want_run = args.iter().any(|a| a == "--run");

    if want_help || (!want_list_only && !want_run) {
        print_usage();
        if !want_help {
            // Bare run with no action flag — still try to be useful.
            println!();
            println!("[spike] No action given; running --list to show devices.");
            println!();
        } else {
            return;
        }
    }

    let ffmpeg = parse_string_arg(&args, "--ffmpeg").unwrap_or_else(resolve_ffmpeg);

    println!("[spike] FFmpeg binary: {}", ffmpeg);
    println!();

    let devices = match enumerate_decklink_outputs(&ffmpeg) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[spike] FATAL: enumeration failed: {}", e);
            std::process::exit(2);
        }
    };

    if devices.is_empty() {
        eprintln!("[spike] FATAL: no DeckLink devices visible to FFmpeg.");
        eprintln!("[spike] Check that:");
        eprintln!("[spike]   1. Blackmagic DeckLink Driver is installed");
        eprintln!("[spike]   2. FFmpeg build supports --enable-decklink");
        eprintln!("[spike]   3. At least one DeckLink card / Videohub / NDI Machine route is online");
        std::process::exit(2);
    }

    println!("[spike] Discovered {} DeckLink device(s):", devices.len());
    for (i, d) in devices.iter().enumerate() {
        println!("[spike]   [{}] {}", i, d);
    }
    println!();

    if want_list_only && !want_run {
        return;
    }

    let count = parse_usize_arg(&args, "--count").unwrap_or(DEFAULT_COUNT);
    let duration = parse_u64_arg(&args, "--duration").unwrap_or(DEFAULT_DURATION_SECS);
    let format_code = parse_string_arg(&args, "--format-code")
        .unwrap_or_else(|| DEFAULT_FORMAT_CODE.to_string());

    let chosen_devices: Vec<String> = match parse_string_arg(&args, "--devices") {
        Some(csv) => csv.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        None => devices.iter().take(count).cloned().collect(),
    };

    if chosen_devices.len() < count {
        eprintln!(
            "[spike] WARNING: requested --count {} but only {} device(s) available; running with what we have.",
            count,
            chosen_devices.len()
        );
    }

    println!("[spike] Plan:");
    println!("[spike]   children:    {}", chosen_devices.len());
    println!("[spike]   duration:    {}s", duration);
    println!("[spike]   format_code: {}", format_code);
    println!("[spike]   codec:       wrapped_avframe + pcm_s16le @ 48 kHz stereo");
    println!("[spike]   source:      lavfi testsrc2 (uyvy422)");
    println!("[spike]   devices:");
    for (i, d) in chosen_devices.iter().enumerate() {
        println!("[spike]     [{}] {}", i, d);
    }
    println!();

    let exit_code = run_spike(&ffmpeg, &chosen_devices, duration, &format_code);
    std::process::exit(exit_code);
}

fn print_usage() {
    println!("decklink-spike — Session 16 Phase A diagnostic");
    println!();
    println!("USAGE:");
    println!("    decklink-spike [--list | --run] [OPTIONS]");
    println!();
    println!("ACTIONS:");
    println!("    --list                List visible DeckLink devices, then exit.");
    println!("    --run                 Spawn N concurrent FFmpeg → decklink children");
    println!("                          and watch them for the configured duration.");
    println!();
    println!("OPTIONS:");
    println!("    --count N             Number of concurrent children. Default: {}.", DEFAULT_COUNT);
    println!("    --duration N          Run for N seconds. Default: {}.", DEFAULT_DURATION_SECS);
    println!("    --format-code CODE    BMD format_code (e.g. Hp29, Hp59). Default: {}.", DEFAULT_FORMAT_CODE);
    println!("    --devices \"a,b,c\"   Explicit device-name CSV (overrides first-N pick).");
    println!("    --ffmpeg PATH         Override FFmpeg path. Default: bundled sidecar if");
    println!("                          present (Windows), else PATH lookup.");
    println!("    --help, -h            Show this message.");
    println!();
    println!("EXIT CODES:");
    println!("    0   PASS — all children held their output for the full duration.");
    println!("    1   FAIL — one or more children died or stalled.");
    println!("    2   Setup error (no devices, FFmpeg missing, etc.).");
}

// ---------------------------------------------------------------------------
// FFmpeg resolution
// ---------------------------------------------------------------------------

fn resolve_ffmpeg() -> String {
    // 1. Env override (matches src-tauri/src/ffmpeg_path.rs).
    if let Ok(p) = env::var("ATEM_PATCHBAY_FFMPEG") {
        if !p.is_empty() && std::path::Path::new(&p).exists() {
            return p;
        }
    }

    // 2. Windows: try the installed app's bundled sidecar so the
    //    operator doesn't need a separate FFmpeg install.
    #[cfg(windows)]
    {
        // %LOCALAPPDATA%\ATEM IP Patchbay\resources\sidecar\ffmpeg.exe is
        // where Tauri's NSIS installer drops the bundled FFmpeg by default.
        if let Ok(local_appdata) = env::var("LOCALAPPDATA") {
            let candidate = format!(
                r"{}\ATEM IP Patchbay\resources\sidecar\ffmpeg.exe",
                local_appdata
            );
            if std::path::Path::new(&candidate).exists() {
                return candidate;
            }
        }
        // %ProgramFiles% fallback for system-wide installs.
        if let Ok(pf) = env::var("ProgramFiles") {
            let candidate = format!(
                r"{}\ATEM IP Patchbay\resources\sidecar\ffmpeg.exe",
                pf
            );
            if std::path::Path::new(&candidate).exists() {
                return candidate;
            }
        }
    }

    // 3. Bare "ffmpeg" — assume PATH.
    "ffmpeg".to_string()
}

// ---------------------------------------------------------------------------
// DeckLink device enumeration
// ---------------------------------------------------------------------------

fn enumerate_decklink_outputs(ffmpeg: &str) -> Result<Vec<String>, String> {
    // Mirror src-tauri/src/device_scanner.rs's scan_decklink_devices.
    // The `-list_devices true` flag is deprecated in FFmpeg 8.x (a
    // warning prints to stderr) but still functions. Keep using it so
    // we don't have to fork the parser for `-sources decklink` until
    // the old flag is actually removed.
    let mut cmd = Command::new(ffmpeg);
    cmd.args([
        "-hide_banner",
        "-f",
        "decklink",
        "-list_devices",
        "true",
        "-i",
        "dummy",
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .stdin(Stdio::null());

    let output = cmd
        .output()
        .map_err(|e| format!("failed to invoke ffmpeg: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    let re = Regex::new(r"\[[^\]]+\]\s+'([^']+)'").unwrap();
    let mut devices = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for line in stderr.lines() {
        let Some(caps) = re.captures(line) else { continue };
        let name = caps[1].trim().to_string();
        // Skip section header that's wrapped in the same bracket prefix.
        if name.is_empty()
            || name.eq_ignore_ascii_case("Blackmagic DeckLink devices")
            || name.eq_ignore_ascii_case("Blackmagic DeckLink input devices")
            || name.eq_ignore_ascii_case("Blackmagic DeckLink output devices")
        {
            continue;
        }
        if seen.contains(&name) {
            continue;
        }
        seen.insert(name.clone());
        devices.push(name);
    }

    Ok(devices)
}

// ---------------------------------------------------------------------------
// Per-child tracking
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct TileState {
    idx: usize,
    device: String,
    fps: f32,
    frames: u64,
    bitrate_kbps: f32,
    started_at: Instant,
    last_progress: Instant,
    alive: bool,
    exit_status: Option<String>,
    last_err_line: Option<String>,
    decklink_negotiated: bool,
}

impl TileState {
    fn new(idx: usize, device: String) -> Self {
        let now = Instant::now();
        Self {
            idx,
            device,
            fps: 0.0,
            frames: 0,
            bitrate_kbps: 0.0,
            started_at: now,
            last_progress: now,
            alive: true,
            exit_status: None,
            last_err_line: None,
            decklink_negotiated: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Spike runner
// ---------------------------------------------------------------------------

fn run_spike(ffmpeg: &str, devices: &[String], duration_secs: u64, format_code: &str) -> i32 {
    let n = devices.len();
    if n == 0 {
        eprintln!("[spike] no devices to test");
        return 2;
    }

    let states: Vec<Arc<Mutex<TileState>>> = devices
        .iter()
        .enumerate()
        .map(|(i, d)| Arc::new(Mutex::new(TileState::new(i, d.clone()))))
        .collect();

    let mut children: Vec<Option<Child>> = Vec::with_capacity(n);

    // Preview the command we're about to spawn (tile 0).
    let tile0_args = build_ffmpeg_args(&devices[0], duration_secs, format_code);
    let cmd_preview = format!(
        "{} {}",
        ffmpeg,
        tile0_args
            .iter()
            .map(|a| if a.contains(' ') { format!("\"{}\"", a) } else { a.clone() })
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!("[spike] tile-0 command:");
    println!("[spike]   {}", cmd_preview);
    println!();

    println!("[spike] spawning {} child(ren)…", n);
    for (i, dev) in devices.iter().enumerate() {
        let args = build_ffmpeg_args(dev, duration_secs, format_code);
        let mut cmd = Command::new(ffmpeg);
        cmd.args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        match cmd.spawn() {
            Ok(mut child) => {
                let stderr = child.stderr.take().expect("stderr was piped");
                let st = states[i].clone();
                let device_name = dev.clone();
                thread::spawn(move || stderr_reader(stderr, st, device_name));
                println!("[spike]   [{}] spawned: {}", i, dev);
                children.push(Some(child));
            }
            Err(e) => {
                eprintln!("[spike]   [{}] SPAWN FAILED: {}: {}", i, dev, e);
                let mut s = states[i].lock().unwrap();
                s.alive = false;
                s.exit_status = Some(format!("spawn failed: {e}"));
                children.push(None);
            }
        }
    }
    println!();

    // Main loop: every REPORT_INTERVAL_SECS, print a status grid;
    // reap exited children at every iteration so we notice death
    // promptly; flag stalls when a tile makes no progress for
    // NO_PROGRESS_TIMEOUT_SECS.
    let started = Instant::now();
    let report = Duration::from_secs(REPORT_INTERVAL_SECS);
    let mut next_report = started + report;
    let total = Duration::from_secs(duration_secs);

    loop {
        thread::sleep(Duration::from_millis(500));
        let elapsed = started.elapsed();

        // Reap any exited children.
        for (i, slot) in children.iter_mut().enumerate() {
            if let Some(child) = slot.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    let mut s = states[i].lock().unwrap();
                    if s.alive {
                        s.alive = false;
                        s.exit_status = Some(format!(
                            "exited with {:?} after {:.1}s",
                            status.code(),
                            s.started_at.elapsed().as_secs_f32()
                        ));
                    }
                    *slot = None;
                }
            }
        }

        // Stall detection — alive but no progress for too long.
        let now = Instant::now();
        for s in &states {
            let mut s = s.lock().unwrap();
            if s.alive && now.duration_since(s.last_progress).as_secs() >= NO_PROGRESS_TIMEOUT_SECS {
                s.alive = false;
                s.exit_status = Some(format!(
                    "stalled — no progress for {}s",
                    NO_PROGRESS_TIMEOUT_SECS
                ));
            }
        }

        if now >= next_report {
            print_status_grid(&states, elapsed);
            next_report = now + report;
        }

        if elapsed >= total {
            break;
        }

        // Early-out: if all children are dead, no point waiting.
        let any_alive = states.iter().any(|s| s.lock().unwrap().alive);
        if !any_alive {
            println!(
                "[spike] all children dead at t+{:.1}s; aborting wait.",
                elapsed.as_secs_f32()
            );
            break;
        }
    }

    // Cleanup: kill any survivors.
    for slot in children.iter_mut() {
        if let Some(mut child) = slot.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    // Final status grid + verdict.
    println!();
    println!("[spike] === final grid ===");
    print_status_grid(&states, started.elapsed());
    println!();
    println!("[spike] === verdict ===");
    let alive_count = states.iter().filter(|s| s.lock().unwrap().alive).count();
    let dead_count = n - alive_count;
    if dead_count == 0 {
        println!(
            "[spike] PASS: all {} child(ren) held their DeckLink output for the full {}s.",
            n, duration_secs
        );
        println!("[spike] Implication: single-process Option A is viable.");
        println!("[spike]   Bump EncoderFleet::TILE_COUNT to 8 in fleet.rs and proceed.");
        0
    } else {
        println!(
            "[spike] FAIL: {} of {} child(ren) died or stalled during the run.",
            dead_count, n
        );
        for s in &states {
            let s = s.lock().unwrap();
            if !s.alive {
                println!(
                    "[spike]   [{}] {} — {}",
                    s.idx,
                    s.device,
                    s.exit_status.as_deref().unwrap_or("unknown")
                );
                if let Some(err) = &s.last_err_line {
                    println!("[spike]       last err: {}", err);
                }
            }
        }
        println!("[spike] Implication: pursue Option B (N processes via spawn_instance).");
        println!("[spike]   Even one death under sustained load = the multiview UI");
        println!("[spike]   needs kernel-level isolation, not just per-tile supervisor.");
        1
    }
}

fn build_ffmpeg_args(device: &str, duration_secs: u64, format_code: &str) -> Vec<String> {
    // Pair the lavfi input rate to the BMD format. format_code uses
    // the BMD-API two-letter scheme:
    //   Hp29 = 1080p29.97 → rate=30000/1001
    //   Hp30 = 1080p30    → rate=30
    //   Hp59 = 1080p59.94 → rate=60000/1001
    //   Hp60 = 1080p60    → rate=60
    // If the operator picks something else, we default to 30000/1001
    // (most DeckLink cards accept it; FFmpeg auto-resamples if not).
    let rate = match format_code {
        "Hp30" => "30",
        "Hp59" => "60000/1001",
        "Hp60" => "60",
        "Hp25" => "25",
        "Hp50" => "50",
        _ => "30000/1001",
    };

    let testsrc = format!(
        "testsrc2=size=1920x1080:rate={}:duration={}",
        rate, duration_secs
    );
    let anullsrc = format!(
        "anullsrc=channel_layout=stereo:sample_rate=48000:duration={}",
        duration_secs
    );

    vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "info".into(),
        // -stats is on by default at -loglevel info; we want progress
        // lines for parse_progress() to read fps/frames from. Setting
        // -stats_period 1 forces a tick every second so the dashboard
        // updates predictably even when FFmpeg's default would be
        // chunkier.
        "-stats_period".into(),
        "1".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        testsrc,
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        anullsrc,
        "-vf".into(),
        "format=uyvy422".into(),
        "-c:v".into(),
        "wrapped_avframe".into(),
        "-c:a".into(),
        "pcm_s16le".into(),
        "-ar".into(),
        "48000".into(),
        "-ac".into(),
        "2".into(),
        "-t".into(),
        duration_secs.to_string(),
        "-f".into(),
        "decklink".into(),
        "-format_code".into(),
        format_code.to_string(),
        device.to_string(),
    ]
}

fn print_status_grid(states: &[Arc<Mutex<TileState>>], elapsed: Duration) {
    println!("[spike] --- t+{:>4.0}s ---", elapsed.as_secs_f32());
    println!(
        "[spike] {:>3} {:>4} {:>40} {:>6} {:>10} {:>8} {}",
        "idx", "live", "device", "fps", "frames", "kbit/s", "note"
    );
    for s in states {
        let s = s.lock().unwrap();
        let badge = if s.alive {
            if s.decklink_negotiated {
                "OK"
            } else {
                "WAIT"
            }
        } else {
            "DEAD"
        };
        let note = if !s.alive {
            s.exit_status
                .as_deref()
                .unwrap_or("dead")
                .chars()
                .take(60)
                .collect::<String>()
        } else if let Some(err) = &s.last_err_line {
            format!("err: {}", err.chars().take(50).collect::<String>())
        } else if s.decklink_negotiated {
            String::new()
        } else {
            "negotiating decklink…".to_string()
        };
        let device_trunc: String = if s.device.len() > 38 {
            format!("{}…", &s.device[..37])
        } else {
            s.device.clone()
        };
        println!(
            "[spike] {:>3} {:>4} {:>40} {:>6.1} {:>10} {:>8.0} {}",
            s.idx, badge, device_trunc, s.fps, s.frames, s.bitrate_kbps, note
        );
    }
}

// ---------------------------------------------------------------------------
// Per-child stderr reader
// ---------------------------------------------------------------------------

fn stderr_reader(stderr: std::process::ChildStderr, st: Arc<Mutex<TileState>>, _device: String) {
    // FFmpeg's progress lines are terminated by \r (carriage return),
    // not \n — same gotcha alpha.3 hit with the streamer's progress
    // parser. Read byte-by-byte and split on either.
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut bytes = stderr.bytes();
    while let Some(byte) = bytes.next() {
        let b = match byte {
            Ok(b) => b,
            Err(_) => break,
        };
        if b == b'\r' || b == b'\n' {
            if !buf.is_empty() {
                let line = String::from_utf8_lossy(&buf).into_owned();
                process_stderr_line(&line, &st);
                buf.clear();
            }
        } else {
            buf.push(b);
            if buf.len() >= 4096 {
                // Pathological line — flush.
                let line = String::from_utf8_lossy(&buf).into_owned();
                process_stderr_line(&line, &st);
                buf.clear();
            }
        }
    }
    // Trailing buffer.
    if !buf.is_empty() {
        let line = String::from_utf8_lossy(&buf).into_owned();
        process_stderr_line(&line, &st);
    }
}

fn process_stderr_line(line: &str, st: &Arc<Mutex<TileState>>) {
    let lower = line.to_lowercase();

    // DeckLink "Found Decklink mode" line confirms format negotiation.
    if lower.contains("found decklink mode") {
        let mut s = st.lock().unwrap();
        s.decklink_negotiated = true;
        s.last_progress = Instant::now();
        return;
    }

    // FFmpeg progress: lines like "frame= 1234 fps=30 q=-0.0 size=...
    //   bitrate=6201.4kbits/s speed=1x"
    if line.contains("frame=") && line.contains("fps=") {
        let (frames, fps, bitrate_kbps) = parse_progress(line);
        let mut s = st.lock().unwrap();
        if frames > s.frames {
            s.frames = frames;
        }
        s.fps = fps;
        if bitrate_kbps > 0.0 {
            s.bitrate_kbps = bitrate_kbps;
        }
        s.last_progress = Instant::now();
        return;
    }

    // Anything that smells like a hard error — record but keep reading.
    if lower.contains("error")
        || lower.contains("could not")
        || lower.contains("failed")
        || lower.contains("invalid")
        || lower.contains("unable")
    {
        let mut s = st.lock().unwrap();
        s.last_err_line = Some(line.trim().to_string());
    }
}

fn parse_progress(line: &str) -> (u64, f32, f32) {
    // Tokens may use "name=value" or "name= value" depending on
    // FFmpeg's padding. Walk tokens and track the previous one so
    // both forms work.
    let mut frames: u64 = 0;
    let mut fps: f32 = 0.0;
    let mut bitrate_kbps: f32 = 0.0;
    let mut prev_key: Option<&str> = None;

    for tok in line.split_whitespace() {
        if let Some((k, v)) = tok.split_once('=') {
            if v.is_empty() {
                prev_key = Some(k);
                continue;
            }
            assign_progress(k, v, &mut frames, &mut fps, &mut bitrate_kbps);
            prev_key = None;
        } else if let Some(k) = prev_key.take() {
            assign_progress(k, tok, &mut frames, &mut fps, &mut bitrate_kbps);
        }
    }

    (frames, fps, bitrate_kbps)
}

fn assign_progress(key: &str, value: &str, frames: &mut u64, fps: &mut f32, bitrate: &mut f32) {
    match key {
        "frame" => {
            if let Ok(v) = value.parse::<u64>() {
                *frames = v;
            }
        }
        "fps" => {
            if let Ok(v) = value.parse::<f32>() {
                *fps = v;
            }
        }
        "bitrate" => {
            // Forms: "6201.4kbits/s" or "N/A"
            if let Some(num) = value.strip_suffix("kbits/s") {
                if let Ok(v) = num.parse::<f32>() {
                    *bitrate = v;
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Tiny arg parsing
// ---------------------------------------------------------------------------

fn parse_string_arg(args: &[String], name: &str) -> Option<String> {
    let pos = args.iter().position(|a| a == name)?;
    args.get(pos + 1).cloned()
}

fn parse_u64_arg(args: &[String], name: &str) -> Option<u64> {
    parse_string_arg(args, name)?.parse().ok()
}

fn parse_usize_arg(args: &[String], name: &str) -> Option<usize> {
    parse_string_arg(args, name)?.parse().ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_progress_with_padded_equals() {
        // FFmpeg-style "name= value" form (spaces between = and value).
        let line = "frame=  120 fps= 30 q=-0.0 size=N/A time=00:00:04.00 bitrate=N/A speed=1.01x";
        let (frames, fps, _) = parse_progress(line);
        assert_eq!(frames, 120);
        assert!((fps - 30.0).abs() < 0.01);
    }

    #[test]
    fn parses_progress_with_compact_equals() {
        // Compact "name=value" form (no space).
        let line = "frame=120 fps=29.97 q=20.0 size=1024kB time=00:00:04.00 bitrate=2048.0kbits/s speed=1.01x";
        let (frames, fps, bitrate) = parse_progress(line);
        assert_eq!(frames, 120);
        assert!((fps - 29.97).abs() < 0.01);
        assert!((bitrate - 2048.0).abs() < 0.01);
    }

    #[test]
    fn ignores_non_progress_lines() {
        let line = "Input #0, lavfi, from 'testsrc2=size=1920x1080:rate=30':";
        let (frames, fps, _) = parse_progress(line);
        assert_eq!(frames, 0);
        assert_eq!(fps, 0.0);
    }
}
