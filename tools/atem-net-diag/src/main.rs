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
//!   atem-net-diag <srt://host:port> --key q1ry-...
//!   atem-net-diag <srt://host:port> --key A --key B --key C    # rotation
//!   atem-net-diag <srt://...> --interval 2 --summary-every 12
//!   atem-net-diag <rtmp://host:port/app/key> --csv probes.csv
//!
//! Output: one line per probe. With multiple --key flags, each key
//! is probed in a burst (in order) per cycle, with per-key state
//! tracking — most useful diagnostic for the "is it this key or
//! the whole destination?" question. State-change moments are
//! called out loudly. Optional periodic summary line shows
//! success rate + latency distribution per key. Optional CSV log
//! of every probe for offline analysis.
//!
//! What this won't do (yet):
//!   - Inspect bandwidth / per-session stats. That requires a real
//!     SRT control-port query (no such public API on most receivers)
//!     OR pcap-level inspection. Future iteration may grow a
//!     `--pcap <iface>` mode that wraps tshark with a port 1935
//!     filter and parses HSv5 control packets.

mod dashboard;

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
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

    if let Some(port) = cli.ui_port {
        dashboard::run(&cli, port);
    }

    if let Some(iface) = cli.monitor_iface.as_deref() {
        run_monitor(&cli, iface);
        return;
    }

    if !ffmpeg_available() {
        eprintln!(
            "ffmpeg not found in PATH. Install it via Homebrew (`brew install ffmpeg`) \
             or point PATH at a build that includes libsrt."
        );
        std::process::exit(3);
    }

    if cli.keys.is_empty() {
        println!("[{}] probing {}  every {}s", clock_now(), cli.url, cli.interval.as_secs());
    } else {
        println!(
            "[{}] probing {}  with {} key(s) {:?}  every {}s/cycle",
            clock_now(),
            cli.url,
            cli.keys.len(),
            cli.keys,
            cli.interval.as_secs(),
        );
    }
    if cli.summary_every > 0 {
        println!(
            "[{}] summary every {} probes",
            clock_now(),
            cli.summary_every,
        );
    }
    if let Some(path) = cli.csv_path.as_deref() {
        println!("[{}] csv log: {path}", clock_now());
        if std::fs::metadata(path).map(|m| m.len() == 0).unwrap_or(true) {
            csv_write_header(path);
        }
    }
    println!("[{}] (Ctrl-C to stop)", clock_now());

    // Per-key tracking. Empty-key string "" represents the no-key
    // case (single URL probe), so the same data structures handle
    // both modes uniformly.
    let mut tracker = Tracker::default();

    loop {
        if cli.keys.is_empty() {
            do_probe(&cli, "", &cli.url, &mut tracker);
        } else {
            for key in &cli.keys {
                match build_bmd_srt_url(&cli.url, key) {
                    Ok(url) => do_probe(&cli, key, &url, &mut tracker),
                    Err(err) => {
                        eprintln!("[{}] {key}  url-build failed: {err}", clock_now());
                    }
                }
            }
        }
        std::thread::sleep(cli.interval);
    }
}

fn do_probe(cli: &Cli, key: &str, url: &str, tracker: &mut Tracker) {
    let started = Instant::now();
    let result = probe(url);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let label = result.label();
    let state_bool = matches!(result, ProbeOutcome::Connected);
    let stats = tracker.stats.entry(key.to_string()).or_default();
    let last = tracker.last_state.get(key).copied().flatten();
    let changed = last.is_some() && last != Some(state_bool);
    let prefix = if key.is_empty() { String::new() } else { format!("{key}  ") };
    if changed {
        tracker.consecutive.insert(key.to_string(), 1);
        let prev_label = match last {
            Some(true) => "CONNECTED",
            Some(false) => "REJECTED ",
            None => "??",
        };
        println!(
            "[{}] === STATE CHANGE === {prefix}{prev_label} -> {label}   ({elapsed_ms}ms)",
            clock_now()
        );
    } else {
        let consec = tracker.consecutive.entry(key.to_string()).or_insert(0);
        *consec += 1;
        let streak = if *consec > 1 {
            format!("  [streak {}]", *consec)
        } else {
            String::new()
        };
        println!(
            "[{}] {prefix}{label}  ({elapsed_ms}ms){streak}",
            clock_now()
        );
    }
    tracker.last_state.insert(key.to_string(), Some(state_bool));
    stats.record(&result, elapsed_ms);

    if let Some(path) = cli.csv_path.as_deref() {
        csv_write_row(path, key, &result, elapsed_ms);
    }

    if cli.summary_every > 0 && stats.total_probes % cli.summary_every as u64 == 0 {
        let label = if key.is_empty() {
            "summary".to_string()
        } else {
            format!("summary  key={key}")
        };
        println!("[{}] {} {}", clock_now(), label, stats.summary_tail());
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ProbeOutcome {
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
struct Tracker {
    stats: HashMap<String, Stats>,
    last_state: HashMap<String, Option<bool>>,
    consecutive: HashMap<String, u32>,
}

#[derive(Default)]
pub(crate) struct Stats {
    pub total_probes: u64,
    pub connected: u64,
    pub rejected: u64,
    pub timeout: u64,
    pub latency_min_ms: u64,
    pub latency_max_ms: u64,
    pub latency_sum_ms: u64,
}

impl Stats {
    pub fn record(&mut self, outcome: &ProbeOutcome, latency_ms: u64) {
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

    fn summary_tail(&self) -> String {
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
            "probes={}  ok={} ({:.1}%)  reject={}  timeout={}  latency: min={}ms avg={}ms max={}ms",
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
pub(crate) fn probe(url: &str) -> ProbeOutcome {
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

// ---- Monitor mode (tshark wrapper) ------------------------------------------

pub(crate) const TSHARK_PATHS: &[&str] = &[
    "/Applications/Wireshark.app/Contents/MacOS/tshark",
    "/usr/local/bin/tshark",
    "/opt/homebrew/bin/tshark",
    "/usr/bin/tshark",
];

pub(crate) const DEFAULT_MONITOR_PORTS: &[u16] = &[1935, 9710, 9977, 1936];
const MONITOR_PRINT_EVERY: Duration = Duration::from_secs(5);
/// Hide flows we haven't seen a packet for in this long. Stale rows
/// pile up over a long-running session otherwise.
const FLOW_STALE_THRESHOLD: Duration = Duration::from_secs(60);

pub(crate) fn find_tshark() -> Option<&'static str> {
    for p in TSHARK_PATHS {
        if std::path::Path::new(p).is_file() {
            return Some(*p);
        }
    }
    None
}

fn run_monitor(cli: &Cli, iface: &str) {
    let tshark = match find_tshark() {
        Some(p) => p,
        None => {
            eprintln!(
                "tshark not found. Install Wireshark to monitor network traffic:\n  \
                 macOS:  brew install --cask wireshark\n  \
                 Linux:  sudo apt install tshark   (or your distro's equivalent)\n\
                 \n\
                 The monitor mode runs tshark with a port-1935+9710 capture filter,\n\
                 parses the packet stream, and shows a live flow table.\n\
                 \n\
                 On macOS, capture permissions need to be set up once via the\n\
                 \"Install ChmodBPF\" item in Wireshark's installer (or via sudo)."
            );
            std::process::exit(3);
        }
    };

    let ports: &[u16] = if cli.monitor_ports.is_empty() {
        DEFAULT_MONITOR_PORTS
    } else {
        &cli.monitor_ports
    };
    let filter = ports
        .iter()
        .map(|p| format!("port {p}"))
        .collect::<Vec<_>>()
        .join(" or ");
    println!(
        "[{}] tshark monitor on iface {iface:?}, filter: {filter:?}",
        clock_now()
    );
    println!(
        "[{}] flow table refresh every {}s; flows idle for >{}s hidden",
        clock_now(),
        MONITOR_PRINT_EVERY.as_secs(),
        FLOW_STALE_THRESHOLD.as_secs(),
    );
    println!("[{}] (Ctrl-C to stop)", clock_now());

    let mut child = match Command::new(tshark)
        .args([
            "-i",
            iface,
            "-l", // line-buffered output so we see packets in real time
            "-f",
            &filter,
            "-T",
            "fields",
            "-E",
            "separator=,",
            "-e",
            "frame.time_relative",
            "-e",
            "ip.src",
            "-e",
            "tcp.srcport",
            "-e",
            "udp.srcport",
            "-e",
            "ip.dst",
            "-e",
            "tcp.dstport",
            "-e",
            "udp.dstport",
            "-e",
            "_ws.col.Protocol",
            "-e",
            "frame.len",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[{}] failed to spawn tshark: {e}", clock_now());
            std::process::exit(3);
        }
    };

    // Pipe stderr to stdout so permission errors etc. surface.
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if let Ok(l) = line {
                    if !l.is_empty() {
                        eprintln!("[tshark err] {l}");
                    }
                }
            }
        });
    }

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            eprintln!("[{}] tshark stdout was not piped", clock_now());
            std::process::exit(3);
        }
    };

    // Reader thread → channel → main loop. Lets us print periodic
    // updates even when tshark is silent (no packets matching).
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if let Ok(l) = line {
                if tx.send(l).is_err() {
                    break;
                }
            }
        }
    });

    let mut flows: HashMap<FlowKey, FlowStats> = HashMap::new();
    let mut last_print = Instant::now();

    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(line) => {
                if let Some(packet) = parse_tshark_line(&line) {
                    let key = packet.flow_key();
                    let stats = flows.entry(key).or_default();
                    stats.record(packet.bytes);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // No packet — fine, just check if it's print time.
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("[{}] tshark exited", clock_now());
                break;
            }
        }
        if last_print.elapsed() >= MONITOR_PRINT_EVERY {
            print_flow_table(&flows, MONITOR_PRINT_EVERY);
            for (_, stats) in flows.iter_mut() {
                stats.window_bytes = 0;
                stats.window_packets = 0;
            }
            last_print = Instant::now();
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PacketInfo {
    pub src_ip: String,
    pub src_port: u16,
    pub dst_ip: String,
    pub dst_port: u16,
    pub is_udp: bool,
    pub protocol: String,
    pub bytes: u64,
}

impl PacketInfo {
    pub fn flow_key(&self) -> FlowKey {
        FlowKey {
            src_ip: self.src_ip.clone(),
            src_port: self.src_port,
            dst_ip: self.dst_ip.clone(),
            dst_port: self.dst_port,
            is_udp: self.is_udp,
            protocol: self.protocol.clone(),
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct FlowKey {
    pub src_ip: String,
    pub src_port: u16,
    pub dst_ip: String,
    pub dst_port: u16,
    pub is_udp: bool,
    pub protocol: String,
}

#[derive(Default)]
pub(crate) struct FlowStats {
    pub total_bytes: u64,
    pub total_packets: u64,
    pub window_bytes: u64,
    pub window_packets: u64,
    pub last_seen: Option<Instant>,
}

impl FlowStats {
    pub fn record(&mut self, bytes: u64) {
        self.total_bytes += bytes;
        self.total_packets += 1;
        self.window_bytes += bytes;
        self.window_packets += 1;
        self.last_seen = Some(Instant::now());
    }
}

pub(crate) fn parse_tshark_line(line: &str) -> Option<PacketInfo> {
    // 9 fields separated by commas:
    // time, ip.src, tcp.srcport, udp.srcport, ip.dst, tcp.dstport, udp.dstport, proto, frame.len
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() < 9 {
        return None;
    }
    let src_ip = fields[1].trim().to_string();
    let dst_ip = fields[4].trim().to_string();
    if src_ip.is_empty() || dst_ip.is_empty() {
        return None; // ARP / non-IP, skip
    }
    let tcp_src = fields[2].trim().parse::<u16>().ok();
    let udp_src = fields[3].trim().parse::<u16>().ok();
    let tcp_dst = fields[5].trim().parse::<u16>().ok();
    let udp_dst = fields[6].trim().parse::<u16>().ok();
    let (src_port, dst_port, is_udp) = if let (Some(s), Some(d)) = (udp_src, udp_dst) {
        (s, d, true)
    } else if let (Some(s), Some(d)) = (tcp_src, tcp_dst) {
        (s, d, false)
    } else {
        return None;
    };
    let protocol = fields[7].trim().to_string();
    let bytes: u64 = fields[8].trim().parse().unwrap_or(0);
    Some(PacketInfo {
        src_ip,
        src_port,
        dst_ip,
        dst_port,
        is_udp,
        protocol,
        bytes,
    })
}

fn print_flow_table(flows: &HashMap<FlowKey, FlowStats>, window: Duration) {
    let now = Instant::now();
    let mut rows: Vec<(&FlowKey, &FlowStats)> = flows
        .iter()
        .filter(|(_, s)| {
            s.last_seen
                .map(|t| now.duration_since(t) < FLOW_STALE_THRESHOLD)
                .unwrap_or(false)
        })
        .collect();
    rows.sort_by(|a, b| b.1.window_bytes.cmp(&a.1.window_bytes));
    println!("[{}] flows ({} active):", clock_now(), rows.len());
    if rows.is_empty() {
        println!("    (no traffic on monitored ports in the last {}s)", FLOW_STALE_THRESHOLD.as_secs());
        return;
    }
    println!(
        "    {:<22} {:1} {:<22} {:6} {:>11} {:>10} {:>5}",
        "source", "", "destination", "proto", "total", "rate/s", "idle"
    );
    for (key, stats) in rows.iter().take(20) {
        let src = format!("{}:{}", key.src_ip, key.src_port);
        let dst = format!("{}:{}", key.dst_ip, key.dst_port);
        let proto_label = if !key.protocol.is_empty() {
            key.protocol.clone()
        } else if key.is_udp {
            "UDP".into()
        } else {
            "TCP".into()
        };
        let total_str = format_bytes(stats.total_bytes);
        let rate_str = if window.as_secs() > 0 {
            format_bytes(stats.window_bytes / window.as_secs())
        } else {
            "0 B".into()
        };
        let idle_str = match stats.last_seen {
            Some(t) => {
                let s = now.duration_since(t).as_secs_f32();
                if s < 1.0 {
                    "<1s".to_string()
                } else {
                    format!("{:.0}s", s)
                }
            }
            None => "?".into(),
        };
        println!(
            "    {:<22} {:<1} {:<22} {:<6} {:>11} {:>9}/s {:>5}",
            src,
            if stats.window_packets > 0 { "→" } else { " " },
            dst,
            proto_label,
            total_str,
            rate_str,
            idle_str,
        );
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GB", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
    }
}

fn csv_write_header(path: &str) {
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "unix_seconds,clock,key,outcome,latency_ms");
    }
}

fn csv_write_row(path: &str, key: &str, outcome: &ProbeOutcome, latency_ms: u64) {
    if let Ok(mut f) = OpenOptions::new().append(true).open(path) {
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(
            f,
            "{},{},{},{},{}",
            unix,
            clock_now(),
            key,
            outcome.csv_label(),
            latency_ms,
        );
    }
}

/// Build a BMD-flavored SRT URL from an `srt://host:port` base + a
/// stream key. The streamid is the URL-encoded form of
/// `#!::bmd_uuid=<random>,bmd_name=ATEM-net-diag,u=<key>` — same
/// shape the main app sends.
pub(crate) fn build_bmd_srt_url(base: &str, key: &str) -> Result<String, String> {
    let host_port = base.split('?').next().unwrap_or(base);
    if !host_port.starts_with("srt://") {
        return Err(format!("--key requires an srt:// base URL, got {base:?}"));
    }
    // Valid UUID v4 format (8-4-4-4-12 hex chars). The previous
    // hand-crafted value had 13 chars in the last group — malformed
    // UUIDs make the BMD receiver fail handshake validation, so
    // probes reported REJECTED even when the receiver was healthy.
    // This UUID is fixed per-process; bmd_uuid is just an opaque
    // identifier the receiver echoes back, so a deterministic value
    // is fine and makes log correlation easier.
    let bmd_uuid = "d1a90517-1c00-4e57-9fab-617465616d64";
    let bmd_name = "ATEM-net-diag";
    let streamid = format!("#!::bmd_uuid={bmd_uuid},bmd_name={bmd_name},u={key}");
    let encoded = url_encode(&streamid);
    Ok(format!(
        "{host_port}?mode=caller&latency=500000&streamid={encoded}"
    ))
}

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

#[derive(Debug)]
pub(crate) struct Cli {
    pub url: String,
    pub keys: Vec<String>,
    pub interval: Duration,
    pub summary_every: u32,
    pub csv_path: Option<String>,
    /// Some(iface) when --monitor is set; runs the tshark wrapper
    /// instead of the probe loop. URL/keys still required by parser
    /// but ignored in monitor mode.
    pub monitor_iface: Option<String>,
    pub monitor_ports: Vec<u16>,
    /// Some(port) when --ui is set. Spins up the embedded HTTP
    /// dashboard at http://127.0.0.1:port/ instead of the CLI.
    pub ui_port: Option<u16>,
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
        let mut keys: Vec<String> = Vec::new();
        let mut interval = Duration::from_secs(DEFAULT_INTERVAL_SECS);
        let mut summary_every = DEFAULT_SUMMARY_EVERY;
        let mut csv_path: Option<String> = None;
        let mut monitor_iface: Option<String> = None;
        let mut monitor_ports: Vec<u16> = Vec::new();
        let mut ui_port: Option<u16> = None;
        let mut iter = args.into_iter();
        while let Some(a) = iter.next() {
            if a == "--ui" {
                // Optional port arg; default 8092.
                let next = iter.clone().next();
                let port = if let Some(v) = next {
                    if let Ok(p) = v.parse::<u16>() {
                        iter.next();
                        p
                    } else {
                        8092
                    }
                } else {
                    8092
                };
                ui_port = Some(port);
            } else if a == "--monitor" {
                monitor_iface = Some(iter.next().unwrap_or_else(|| {
                    if cfg!(target_os = "macos") {
                        "en0".into()
                    } else {
                        "any".into()
                    }
                }));
            } else if a == "--port" {
                let v = iter.next().ok_or("--port needs a value")?;
                let p: u16 = v
                    .parse()
                    .map_err(|_| format!("--port value not 1..65535: {v}"))?;
                monitor_ports.push(p);
            } else if a == "--interval" {
                let v = iter.next().ok_or("--interval needs a value (seconds)")?;
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
                    .ok_or("--summary-every needs a value (probes)")?;
                summary_every = v
                    .parse()
                    .map_err(|_| format!("--summary-every value not an integer: {v}"))?;
            } else if a == "--no-summary" {
                summary_every = 0;
            } else if a == "--csv" {
                csv_path = Some(iter.next().ok_or("--csv needs a path")?);
            } else if a == "--key" {
                let k = iter.next().ok_or("--key needs a value")?;
                keys.push(k);
            } else if !a.starts_with('-') && url.is_none() {
                url = Some(a);
            } else {
                return Err(format!("unknown argument: {a}"));
            }
        }
        // Monitor mode short-circuits the URL requirement — tshark
        // captures from a network interface, not from a remote URL.
        // UI mode also tolerates a missing URL: the dashboard's
        // config form is the canonical way to set the target, so
        // `atem-net-diag --ui` alone is a valid bare launch (the
        // probe loop sleeps until the user submits the form).
        if (monitor_iface.is_some() || ui_port.is_some()) && url.is_none() {
            return Ok(Self {
                url: "monitor://".into(),
                keys,
                interval,
                summary_every,
                csv_path,
                monitor_iface,
                monitor_ports,
                ui_port,
            });
        }
        let url = url.ok_or_else(usage)?;
        if !(url.starts_with("srt://") || url.starts_with("rtmp://") || url.starts_with("rtmps://"))
        {
            return Err(format!(
                "URL should start with srt:// or rtmp(s):// — got {url}"
            ));
        }
        // RTMP + --key combinations are silently treated as no-key
        // probes today — RTMP keys go in the URL path, not an SRT-
        // style streamid query param. Surface the mismatch loudly
        // so a user typing `--key K` on an rtmp:// URL doesn't get
        // a confusing "key was ignored" experience.
        if !keys.is_empty() && !url.starts_with("srt://") {
            return Err(
                "--key is SRT-only — RTMP keys are part of the URL path. \
                 Drop --key and put the key in the URL like rtmp://host:port/app/KEY."
                    .into(),
            );
        }
        Ok(Self {
            url,
            keys,
            interval,
            summary_every,
            csv_path,
            monitor_iface,
            monitor_ports,
            ui_port,
        })
    }
}

fn usage() -> String {
    "atem-net-diag — probe an SRT/RTMP destination on a fixed interval, \
     log connect-state transitions and latency

usage:
    atem-net-diag <srt://host:port[?streamid=...]> [flags]
    atem-net-diag <srt://host:port> --key KEY [flags]
    atem-net-diag <srt://host:port> --key KEY1 --key KEY2 ... [flags]
    atem-net-diag <rtmp://host:port/app/KEY>       [flags]

flags:
    --interval N         Probe every N seconds (default 5; per cycle when
                         multiple --key flags are used)
    --summary-every N    Print a summary every N probes per key (default 12)
    --no-summary         Don't print periodic summaries
    --csv FILE           Append every probe to FILE as CSV (auto-creates with
                         header: unix_seconds,clock,key,outcome,latency_ms)
    --key K              Build a BMD-flavored streamid (#!::bmd_uuid=...,u=K)
                         and append it to the SRT URL. Repeat --key to rotate
                         through multiple keys per cycle — useful for telling
                         apart 'this key is locked' (only one rejects) vs
                         'destination is locked' (all reject) vs 'destination
                         is unreachable' (all timeout).
    --monitor [IFACE]    Switch to passive-capture mode: spawn tshark on
                         IFACE (default en0 on macOS), watch the configured
                         ports, and print a live flow table every 5s
                         showing source IP:port, destination IP:port, total
                         bytes per flow, and current bandwidth. Requires
                         tshark in PATH and capture permissions (on macOS:
                         the Wireshark installer's ChmodBPF helper, or run
                         this tool with sudo).
    --port P             Add a port to the monitor capture filter. Default
                         monitored ports: 1935 (RTMP/SRT), 9710 (SRT
                         listener), 9977 (BMD ctrl), 1936. Repeat to add
                         multiple. Without --monitor, this flag has no
                         effect.
    --ui [PORT]          Spin up a visual dashboard at http://127.0.0.1:PORT
                         (default 8092) and open it in your browser. The
                         dashboard polls /api/state every 1s and shows
                         per-key probe status, recent probe timeline, and
                         (if --monitor is also set) the live flow table.
                         Combine with all the other flags — probes still
                         run in the background, the UI is just a passive
                         consumer of the same state. Ctrl-C to quit.
    -h, --help           Show this help

what to look for:
    CONNECTED in a row  destination is happy.
    REJECTED bursts after a stream — receiver-state lockout (the
        slot is held for ~30s-2min after a previous source disconnects).
        Wait it out, or use a different key.
    Mixed CONNECTED+REJECTED across keys — per-key lockout: the key
        that REJECTs is held; others work.
    All TIMEOUT — destination is unreachable. Network / power / wrong IP.
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
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

// Ctrl-C handling intentionally left to the OS — terminating the
// process at SIGINT is exactly the right behavior for a probe
// loop. No need to pull in the `ctrlc` crate or a libc dep for a
// graceful "stopped" log line.
