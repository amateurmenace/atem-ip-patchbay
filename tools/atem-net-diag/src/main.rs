//! ATEM network diagnostic — periodically attempts an SRT/RTMP
//! handshake against a destination and logs whether the destination
//! is currently accepting connections. Designed to run on a peer
//! machine on the same LAN as the ATEM (or anywhere with line-of-
//! sight to the receiver) so the user can characterize the
//! "ATEM stops accepting after a few stream tests" lockout the
//! main app's clean-exit handler can't cover.
//!
//! Usage:
//!   atem-net-diag <srt://host:port[?streamid=...]>           # default 5s interval
//!   atem-net-diag <srt://...> --interval 2                   # 2s interval
//!   atem-net-diag <rtmp://host:port/app/key>                 # works on RTMP too
//!
//! Output: one line per probe, with state-change moments called out
//! loudly. Unbuffered so it can pipe into `tee` / a logfile and stay
//! readable while running.
//!
//! What this won't do (yet):
//!   - Inspect bandwidth / per-session stats. That requires a real
//!     SRT control-port query (no such public API on most receivers)
//!     OR pcap-level inspection. Future iteration may grow a
//!     `--pcap <iface>` mode that wraps tshark with a port 1935
//!     filter and parses HSv5 control packets.
//!   - Distinguish "destination dropped THIS connection" vs
//!     "destination accepting nobody". The probe is yes/no — the
//!     transition pattern (CONNECTED → REJECTED → REJECTED → ...)
//!     is itself a strong diagnostic.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEFAULT_INTERVAL_SECS: u64 = 5;
/// Per-probe FFmpeg timeout in microseconds (FFmpeg's `-timeout`
/// flag for SRT/RTMP). 3s is enough for the handshake on a healthy
/// receiver; longer values just slow down the loop when the
/// destination is genuinely down.
const PROBE_TIMEOUT_US: &str = "3000000";

fn main() {
    let cli = match Cli::parse() {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    if !ffmpeg_available() {
        eprintln!(
            "ffmpeg not found in PATH. Install it via Homebrew (`brew install ffmpeg`) \
             or point PATH at a build that includes libsrt."
        );
        std::process::exit(3);
    }

    println!("[{}] probing {}  every {}s", clock_now(), cli.url, cli.interval.as_secs());
    println!("[{}] (Ctrl-C to stop)", clock_now());

    let mut last_state: Option<bool> = None;
    let mut consecutive: u32 = 0;
    loop {
        let started = Instant::now();
        let result = probe(&cli.url);
        let elapsed_ms = started.elapsed().as_millis();
        let label = match result {
            ProbeOutcome::Connected => "CONNECTED",
            ProbeOutcome::Rejected => "REJECTED ",
            ProbeOutcome::Timeout => "TIMEOUT  ",
        };
        let state_bool = matches!(result, ProbeOutcome::Connected);
        let changed = last_state.is_some() && last_state != Some(state_bool);
        if changed {
            consecutive = 1;
            println!(
                "[{}] === STATE CHANGE === -> {label}   ({elapsed_ms}ms)",
                clock_now()
            );
        } else {
            consecutive += 1;
            println!(
                "[{}] {label}  ({elapsed_ms}ms){}",
                clock_now(),
                if consecutive > 1 {
                    format!("  [streak {consecutive}]")
                } else {
                    String::new()
                }
            );
        }
        last_state = Some(state_bool);

        std::thread::sleep(cli.interval);
    }
}

#[derive(Debug)]
enum ProbeOutcome {
    Connected,
    Rejected,
    Timeout,
}

/// Spawn FFmpeg with a tight read window against the URL. Exit code
/// 0 = handshake + first packet read worked = receiver is accepting.
/// Anything else = receiver rejected, timed out, or unreachable.
/// We discard stdout/stderr because the loop output is already
/// noisy enough; users who want detail can run FFmpeg directly with
/// the same URL.
fn probe(url: &str) -> ProbeOutcome {
    let started = Instant::now();
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-timeout",
            PROBE_TIMEOUT_US,
            "-i",
            url,
            "-t",
            "0.001",
            "-f",
            "null",
            "-",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => ProbeOutcome::Connected,
        Ok(_) => {
            // Heuristic: if we ran for ~the timeout window, it was a
            // timeout (network unreachable / silent drop). If we
            // exited fast, the receiver actively rejected (HSv5
            // conclusion with a reject reason, or TCP RST for RTMP).
            let elapsed = started.elapsed();
            if elapsed >= Duration::from_millis(2500) {
                ProbeOutcome::Timeout
            } else {
                ProbeOutcome::Rejected
            }
        }
        Err(_) => ProbeOutcome::Rejected,
    }
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[derive(Debug)]
struct Cli {
    url: String,
    interval: Duration,
}

impl Cli {
    fn parse() -> Result<Self, String> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.is_empty() {
            return Err(usage());
        }
        if args.iter().any(|a| a == "-h" || a == "--help") {
            return Err(usage());
        }
        let mut url: Option<String> = None;
        let mut interval = Duration::from_secs(DEFAULT_INTERVAL_SECS);
        let mut iter = args.into_iter();
        while let Some(a) = iter.next() {
            if a == "--interval" {
                let v = iter
                    .next()
                    .ok_or_else(|| "--interval needs a value (seconds)".to_string())?;
                let secs: u64 = v
                    .parse()
                    .map_err(|_| format!("--interval value not an integer: {v}"))?;
                if secs == 0 {
                    return Err("--interval must be > 0".into());
                }
                interval = Duration::from_secs(secs);
            } else if !a.starts_with('-') && url.is_none() {
                url = Some(a);
            } else {
                return Err(format!("unknown argument: {a}"));
            }
        }
        let url = url.ok_or_else(|| usage())?;
        if !(url.starts_with("srt://") || url.starts_with("rtmp://") || url.starts_with("rtmps://"))
        {
            return Err(format!(
                "URL should start with srt:// or rtmp(s):// — got {url}"
            ));
        }
        Ok(Self { url, interval })
    }
}

fn usage() -> String {
    "atem-net-diag — probe an SRT/RTMP destination on a fixed interval, \
     log connect-state transitions

usage:
    atem-net-diag <srt://host:port[?streamid=...]> [--interval SECONDS]
    atem-net-diag <rtmp://host:port/app/key> [--interval SECONDS]

flags:
    --interval N      Probe every N seconds (default 5)
    -h, --help        Show this help

requires:
    ffmpeg in PATH (with libsrt for srt:// URLs)
"
        .into()
}

/// HH:MM:SS local time, day-rolled. Good enough for human-readable
/// log lines without pulling in a date crate.
fn clock_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Naive local-time conversion; we just want elapsed-of-day.
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

// Ctrl-C handling intentionally left to the OS — terminating the
// process at SIGINT is exactly the right behavior for a probe
// loop. No need to pull in the `ctrlc` crate or a libc dep for a
// graceful "stopped" log line.
