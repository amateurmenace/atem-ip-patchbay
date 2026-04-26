//! ATEM network diagnostic — periodically attempts an SRT/RTMP
//! handshake against a destination and logs whether the destination
//! is currently accepting connections. Designed to run on a peer
//! machine on the same LAN as the ATEM (or anywhere with line-of-
//! sight to the receiver) so the user can characterize the
//! "ATEM stops accepting after a few stream tests" lockout the
//! main app's clean-exit handler can't always cover.
//!
//! Usage:
//!   atem-net-diag <srt://host:port[?streamid=...]>
//!   atem-net-diag <srt://...> --interval 2 --summary-every 12
//!   atem-net-diag <rtmp://host:port/app/key> --csv probes.csv
//!
//! Output: one line per probe, with state-change moments called out
//! loudly (=== STATE CHANGE ===). Optional periodic summary line
//! showing success rate + latency distribution. Optional CSV log of
//! every probe for offline analysis.
//!
//! What this won't do (yet):
//!   - Inspect bandwidth / per-session stats. That requires a real
//!     SRT control-port query (no such public API on most receivers)
//!     OR pcap-level inspection. Future iteration may grow a
//!     `--pcap <iface>` mode that wraps tshark with a port 1935
//!     filter and parses HSv5 control packets.
//!   - Multi-key rotation (probe-with-key-A, probe-with-key-B, ...
//!     to distinguish per-key lockouts from destination-wide).
//!     Today, run multiple instances of the tool with different
//!     URLs in separate terminals if you want this.

use std::fs::OpenOptions;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEFAULT_INTERVAL_SECS: u64 = 5;
const DEFAULT_SUMMARY_EVERY: u32 = 12;
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
    if cli.summary_every > 0 {
        println!(
            "[{}] summary every {} probes ({:.0}s wall-clock)",
            clock_now(),
            cli.summary_every,
            cli.interval.as_secs() as f64 * cli.summary_every as f64,
        );
    }
    if let Some(path) = cli.csv_path.as_deref() {
        println!("[{}] csv log: {path}", clock_now());
        // Write header if the file's empty / new.
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() == 0 {
                csv_write_header(path);
            }
        } else {
            csv_write_header(path);
        }
    }
    println!("[{}] (Ctrl-C to stop)", clock_now());

    let mut stats = Stats::default();
    let mut last_state: Option<bool> = None;
    let mut consecutive: u32 = 0;
    loop {
        let started = Instant::now();
        let result = probe(&cli.url);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let label = result.label();
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

        stats.record(&result, elapsed_ms);
        if let Some(path) = cli.csv_path.as_deref() {
            csv_write_row(path, &result, elapsed_ms);
        }

        if cli.summary_every > 0 && stats.total_probes % cli.summary_every as u64 == 0 {
            println!("[{}] {}", clock_now(), stats.summary_line());
        }

        std::thread::sleep(cli.interval);
    }
}

#[derive(Debug, Clone, Copy)]
enum ProbeOutcome {
    Connected,
    Rejected,
    Timeout,
}

impl ProbeOutcome {
    fn label(&self) -> &'static str {
        match self {
            ProbeOutcome::Connected => "CONNECTED",
            ProbeOutcome::Rejected => "REJECTED ",
            ProbeOutcome::Timeout => "TIMEOUT  ",
        }
    }
    fn csv_label(&self) -> &'static str {
        match self {
            ProbeOutcome::Connected => "connected",
            ProbeOutcome::Rejected => "rejected",
            ProbeOutcome::Timeout => "timeout",
        }
    }
}

#[derive(Default)]
struct Stats {
    total_probes: u64,
    connected: u64,
    rejected: u64,
    timeout: u64,
    /// Latency in ms across all probes regardless of outcome — useful
    /// for spotting "destination is degrading" before it fully fails.
    latency_min_ms: u64,
    latency_max_ms: u64,
    latency_sum_ms: u64,
}

impl Stats {
    fn record(&mut self, outcome: &ProbeOutcome, latency_ms: u64) {
        self.total_probes += 1;
        match outcome {
            ProbeOutcome::Connected => self.connected += 1,
            ProbeOutcome::Rejected => self.rejected += 1,
            ProbeOutcome::Timeout => self.timeout += 1,
        }
        if self.total_probes == 1 || latency_ms < self.latency_min_ms {
            self.latency_min_ms = latency_ms;
        }
        if latency_ms > self.latency_max_ms {
            self.latency_max_ms = latency_ms;
        }
        self.latency_sum_ms += latency_ms;
    }

    fn summary_line(&self) -> String {
        let success_pct = if self.total_probes == 0 {
            0.0
        } else {
            (self.connected as f64 / self.total_probes as f64) * 100.0
        };
        let avg_ms = if self.total_probes == 0 {
            0
        } else {
            self.latency_sum_ms / self.total_probes
        };
        format!(
            "summary  probes={}  ok={} ({:.1}%)  reject={}  timeout={}  latency: min={}ms avg={}ms max={}ms",
            self.total_probes,
            self.connected,
            success_pct,
            self.rejected,
            self.timeout,
            self.latency_min_ms,
            avg_ms,
            self.latency_max_ms,
        )
    }
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

fn csv_write_header(path: &str) {
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "unix_seconds,clock,outcome,latency_ms");
    }
}

fn csv_write_row(path: &str, outcome: &ProbeOutcome, latency_ms: u64) {
    if let Ok(mut f) = OpenOptions::new().append(true).open(path) {
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(
            f,
            "{},{},{},{}",
            unix,
            clock_now(),
            outcome.csv_label(),
            latency_ms,
        );
    }
}

#[derive(Debug)]
struct Cli {
    url: String,
    interval: Duration,
    summary_every: u32,
    csv_path: Option<String>,
}

/// Build a BMD-flavored SRT URL from an `srt://host:port` base + a
/// stream key. The streamid is the URL-encoded form of
/// `#!::bmd_uuid=<random>,bmd_name=ATEM-net-diag,u=<key>` — same
/// shape the main app sends, so the receiver's accept rules see
/// the same handshake. Caller-mode + 500ms latency match what the
/// streamer uses by default.
fn build_bmd_srt_url(base: &str, key: &str) -> Result<String, String> {
    // Split off any query string / streamid the user may have already
    // included; we're going to overwrite it.
    let host_port = base.split('?').next().unwrap_or(base);
    if !host_port.starts_with("srt://") {
        return Err(format!(
            "--key requires an srt:// base URL, got {base:?}"
        ));
    }
    // A fixed pseudo-UUID per-process is fine — the receiver doesn't
    // care, it just needs to PARSE the streamid. Use a constant
    // tag-style identifier so log lines are matchable across runs.
    let bmd_uuid = "00000000-0000-0000-0000-617465616d646";
    // bmd_name is a label the receiver may show in its UI; identifies
    // this probe so the operator can spot diag traffic vs real streams.
    let bmd_name = "ATEM-net-diag";
    let streamid = format!("#!::bmd_uuid={bmd_uuid},bmd_name={bmd_name},u={key}");
    let encoded = url_encode(&streamid);
    Ok(format!(
        "{host_port}?mode=caller&latency=500000&streamid={encoded}"
    ))
}

/// Minimal URL-component percent-encoding for the streamid value.
/// Intentionally aggressive (encodes everything but unreserved
/// chars) — the receiver decodes per RFC 3986 so over-encoding is
/// safe; under-encoding (e.g. leaving `=` or `,` unescaped) breaks
/// the URL parser.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        let safe =
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if safe {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
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
        let mut key: Option<String> = None;
        let mut interval = Duration::from_secs(DEFAULT_INTERVAL_SECS);
        let mut summary_every = DEFAULT_SUMMARY_EVERY;
        let mut csv_path: Option<String> = None;
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
            } else if a == "--summary-every" {
                let v = iter
                    .next()
                    .ok_or_else(|| "--summary-every needs a value (probes)".to_string())?;
                summary_every = v
                    .parse()
                    .map_err(|_| format!("--summary-every value not an integer: {v}"))?;
            } else if a == "--no-summary" {
                summary_every = 0;
            } else if a == "--csv" {
                csv_path = Some(
                    iter.next()
                        .ok_or_else(|| "--csv needs a path".to_string())?,
                );
            } else if a == "--key" {
                key = Some(
                    iter.next()
                        .ok_or_else(|| "--key needs a value".to_string())?,
                );
            } else if !a.starts_with('-') && url.is_none() {
                url = Some(a);
            } else {
                return Err(format!("unknown argument: {a}"));
            }
        }
        let mut url = url.ok_or_else(usage)?;
        if !(url.starts_with("srt://") || url.starts_with("rtmp://") || url.starts_with("rtmps://"))
        {
            return Err(format!(
                "URL should start with srt:// or rtmp(s):// — got {url}"
            ));
        }
        // --key K rebuilds the URL with the BMD-flavored streamid the
        // main app sends. SRT-only — RTMP keys go in the URL path,
        // not the streamid, so --key on rtmp:// is a no-op.
        if let Some(k) = key {
            if url.starts_with("srt://") {
                url = build_bmd_srt_url(&url, &k)?;
            }
        }
        Ok(Self {
            url,
            interval,
            summary_every,
            csv_path,
        })
    }
}

fn usage() -> String {
    "atem-net-diag — probe an SRT/RTMP destination on a fixed interval, \
     log connect-state transitions and latency

usage:
    atem-net-diag <srt://host:port[?streamid=...]> [flags]
    atem-net-diag <rtmp://host:port/app/key>       [flags]

flags:
    --interval N         Probe every N seconds (default 5)
    --summary-every N    Print a summary every N probes (default 12 == 1min @ 5s)
    --no-summary         Don't print periodic summaries
    --csv FILE           Append every probe to FILE as CSV (auto-creates with
                         header: unix_seconds,clock,outcome,latency_ms)
    --key K              Build a BMD-flavored streamid (#!::bmd_uuid=...,u=K)
                         and append it to the SRT URL — the same handshake
                         the main app sends. Use this to probe-with-a-key
                         without hand-crafting the streamid.
    -h, --help           Show this help

what to look for:
    CONNECTED in a row — destination is happy.
    REJECTED bursts after a stream — receiver-state lockout (the
        slot is held for ~30s-2min after a previous source disconnects).
        Wait it out, or use a different key.
    TIMEOUT — destination is unreachable. Network / power / wrong IP.
    State-change moments are called out with === STATE CHANGE ===.

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
