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
mod unifi;

use std::collections::{HashMap, VecDeque};
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

// Default target: the user's actual ATEM. Pre-populating these in
// the dashboard form means a bare `--ui` launch is immediately
// useful against the production destination — no copy-pasting an
// IP from the patchbay's UI to start monitoring. UDM correlation
// uses the MAC (UniFi keys clients by MAC primarily), so even if
// the ATEM gets a new DHCP lease the right device is still found.
pub(crate) const DEFAULT_ATEM_IP: &str = "192.168.20.189";
pub(crate) const DEFAULT_ATEM_MAC: &str = "7c:2e:0d:21:ab:fe";
pub(crate) const DEFAULT_ATEM_PORT: u16 = 1935;
pub(crate) const DEFAULT_UDM_HOST: &str = "https://192.168.20.1";

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

/// Build the tshark argv for live capture with the field set we
/// want for the dashboard. Shared between the CLI monitor mode and
/// the UI's background monitor thread so we get identical
/// behaviour from both surfaces.
pub(crate) fn tshark_field_args(iface: &str, filter: &str) -> Vec<String> {
    [
        "-i", iface, "-l", "-f", filter,
        "-T", "fields", "-E", "separator=,",
        // Per-frame identity: timestamp, IPs, ports, protocol label,
        // total wire length.
        "-e", "frame.time_relative",
        "-e", "ip.src",
        "-e", "tcp.srcport",
        "-e", "udp.srcport",
        "-e", "ip.dst",
        "-e", "tcp.dstport",
        "-e", "udp.dstport",
        "-e", "_ws.col.Protocol",
        "-e", "frame.len",
        // SRT-only fields, populated by the dissector when this
        // frame is a recognized SRT control packet (typically an
        // ACKD with bandwidth + RTT). Empty strings on non-SRT or
        // SRT-data frames; that's fine, the parser handles it.
        "-e", "srt.iscontrol",
        "-e", "srt.bw",
        "-e", "srt.rate",
        "-e", "srt.rtt",
        "-e", "srt.rttvar",
        "-e", "srt.bufavail",
        // Raw UDP payload — populated for every UDP frame as hex.
        // We only parse it when we suspect an SRT control packet
        // (srt.iscontrol == 1) carrying a handshake with the SID
        // extension. Wireshark's SRT dissector exposes only a few
        // SRT fields; the streamid extension isn't among them, so
        // we extract it from the raw bytes ourselves. Cost: ~1500
        // bytes of stdout per UDP frame, which is bounded and
        // tolerable for diagnostic use.
        "-e", "udp.payload",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

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
        .args(tshark_field_args(iface, &filter))
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
    /// SRT-only: present when tshark's SRT dissector parsed an ACKD
    /// control packet from this frame. Receivers send ACKs every
    /// ~10ms during a healthy stream so these tick frequently.
    pub srt: Option<SrtAck>,
    /// Stream ID extracted from the SRT HSv5 conclusion handshake's
    /// SID extension (extension type 0x0005). Present only on the
    /// rare frames that carry the conclusion handshake — once per
    /// connection. The dashboard caches this against the flow key
    /// so subsequent data packets on the same flow can render the
    /// stream ID without re-parsing the handshake.
    pub stream_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SrtAck {
    pub is_control: bool,
    /// Bandwidth estimate from receiver (in pkts/s; multiply by MTU
    /// for byte rate — most BMD streams use 1316-byte SRT payload
    /// in MPEG-TS-over-SRT mode).
    pub bw_pkts_s: Option<u32>,
    /// Receive rate from receiver (pkts/s).
    pub rate_pkts_s: Option<u32>,
    /// Round-trip time in microseconds.
    pub rtt_us: Option<u32>,
    /// RTT variance in microseconds.
    pub rttvar_us: Option<u32>,
    /// Receiver buffer available (pkts).
    pub buf_avail_pkts: Option<u32>,
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
    pub control_packets: u64,
    pub last_seen: Option<Instant>,
    pub first_seen: Option<Instant>,
    /// Last SRT ACK received from the receiver of this flow. For
    /// SRT senders, this is the destination's ack to our packets.
    /// For receivers, it's our ack to the sender. Either way, the
    /// numbers describe live network conditions for THIS flow.
    pub last_srt_ack: Option<SrtAck>,
    pub last_srt_ack_at: Option<Instant>,
    /// Rolling bitrate samples (bytes per second over 1s windows).
    /// Capped at 60 entries so the UI can draw a 60-second sparkline.
    pub bitrate_samples: VecDeque<u64>,
    /// When the last 1-second window started (for emitting samples).
    pub bitrate_window_start: Option<Instant>,
    /// Bytes accumulated in the current 1-second sample window.
    pub bitrate_window_bytes: u64,
}

impl FlowStats {
    pub fn record(&mut self, bytes: u64) {
        let now = Instant::now();
        self.total_bytes += bytes;
        self.total_packets += 1;
        self.window_bytes += bytes;
        self.window_packets += 1;
        self.last_seen = Some(now);
        if self.first_seen.is_none() {
            self.first_seen = Some(now);
        }
        // Bitrate sampling: emit a sample every ~1s of wall clock.
        match self.bitrate_window_start {
            Some(start) if now.duration_since(start) >= Duration::from_millis(1000) => {
                self.bitrate_samples.push_back(self.bitrate_window_bytes);
                while self.bitrate_samples.len() > 60 {
                    self.bitrate_samples.pop_front();
                }
                self.bitrate_window_start = Some(now);
                self.bitrate_window_bytes = bytes;
            }
            None => {
                self.bitrate_window_start = Some(now);
                self.bitrate_window_bytes = bytes;
            }
            _ => {
                self.bitrate_window_bytes += bytes;
            }
        }
    }

    pub fn record_srt_ack(&mut self, ack: SrtAck) {
        self.control_packets += 1;
        self.last_srt_ack = Some(ack);
        self.last_srt_ack_at = Some(Instant::now());
    }
}

pub(crate) fn parse_tshark_line(line: &str) -> Option<PacketInfo> {
    // 16 fields separated by commas (was 15 before SID parsing):
    //   0  time.relative
    //   1  ip.src
    //   2  tcp.srcport
    //   3  udp.srcport
    //   4  ip.dst
    //   5  tcp.dstport
    //   6  udp.dstport
    //   7  _ws.col.Protocol
    //   8  frame.len
    //   9  srt.iscontrol
    //  10  srt.bw           (ACKD bandwidth, pkts/s)
    //  11  srt.rate         (ACKD receive rate, pkts/s)
    //  12  srt.rtt          (ACKD RTT, microseconds)
    //  13  srt.rttvar       (ACKD RTT variance, microseconds)
    //  14  srt.bufavail     (receiver buffer available, pkts)
    //  15  udp.payload      (raw UDP payload as hex; we parse it
    //                       only when srt.iscontrol == 1 to find
    //                       the streamid in HSv5 conclusion HS)
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

    // SRT fields are present only on UDP frames the dissector
    // recognized as SRT — non-SRT UDP and TCP packets have empty
    // strings for these slots. Treat any populated field as a
    // signal that this is an SRT control packet worth recording.
    let (srt, is_srt_control) = if fields.len() >= 15 {
        let is_control = parse_tshark_bool(fields[9]);
        let bw = fields[10].trim().parse::<u32>().ok();
        let rate = fields[11].trim().parse::<u32>().ok();
        let rtt = fields[12].trim().parse::<u32>().ok();
        let rttvar = fields[13].trim().parse::<u32>().ok();
        let bufavail = fields[14].trim().parse::<u32>().ok();
        if is_control.is_some()
            || bw.is_some()
            || rate.is_some()
            || rtt.is_some()
            || rttvar.is_some()
            || bufavail.is_some()
        {
            (
                Some(SrtAck {
                    is_control: is_control.unwrap_or(false),
                    bw_pkts_s: bw,
                    rate_pkts_s: rate,
                    rtt_us: rtt,
                    rttvar_us: rttvar,
                    buf_avail_pkts: bufavail,
                }),
                is_control.unwrap_or(false),
            )
        } else {
            (None, false)
        }
    } else {
        (None, false)
    };

    // Streamid extraction — only attempted when this is an SRT
    // control packet AND we have payload bytes. Most control
    // packets are ACKD/NAK (carry no SID); only the conclusion
    // handshake does. parse_srt_handshake_streamid returns None
    // for any other control packet, so the cost is one early-out
    // header check per ACK. Cheap.
    let stream_id = if is_srt_control && fields.len() >= 16 {
        let payload_hex = fields[15].trim();
        if payload_hex.is_empty() {
            None
        } else {
            let payload = hex_decode(payload_hex);
            parse_srt_handshake_streamid(&payload)
        }
    } else {
        None
    };

    Some(PacketInfo {
        src_ip,
        src_port,
        dst_ip,
        dst_port,
        is_udp,
        protocol,
        bytes,
        srt,
        stream_id,
    })
}

/// Parse the streamid from an SRT HSv5 conclusion handshake's SID
/// extension. Returns None when the payload is too short, isn't a
/// control packet, isn't a handshake, isn't a conclusion, or has
/// no SID extension.
///
/// Wire format (per RFC 8723 §3.2.1 and SRT spec):
/// - Control packet header (16 bytes):
///   - Word 0: bit 0 = control flag (1), bits 1-15 = control type,
///     bits 16-31 = subtype (typically 0 for handshake)
///   - Word 1: type-specific
///   - Word 2: timestamp
///   - Word 3: destination socket ID
/// - Handshake body (48 bytes):
///   - +0  4B: version (5 for HSv5)
///   - +4  2B: encryption field
///   - +6  2B: extension field bitmap
///   - +8  4B: initial seq #
///   - +12 4B: MTU
///   - +16 4B: max flow window
///   - +20 4B: handshake type (0xFFFFFFFF = conclusion)
///   - +24 4B: SRT socket ID
///   - +28 4B: SYN cookie
///   - +32 16B: peer IP
/// - Extensions follow at body offset 48 — only on conclusion
///   handshakes with the SID bit (0x04 in the extension bitmap).
///   Each extension: 2B type, 2B length-in-words, N*4B data.
///   SID extension is type 0x0005.
pub(crate) fn parse_srt_handshake_streamid(payload: &[u8]) -> Option<String> {
    // Minimum: 16 control header + 48 handshake body + 4 ext header + 4 ext data = 72 bytes.
    if payload.len() < 72 {
        return None;
    }
    // Control bit must be set.
    if payload[0] & 0x80 == 0 {
        return None;
    }
    // Control type — first 16 bits with control bit masked off.
    // Type 0 = handshake.
    let control_type = u16::from_be_bytes([payload[0] & 0x7F, payload[1]]);
    if control_type != 0 {
        return None;
    }
    let body = &payload[16..];
    if body.len() < 48 {
        return None;
    }
    let version = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
    if version != 5 {
        return None;
    }
    // Handshake type at body+20 — conclusion is -1 (0xFFFFFFFF).
    let hs_type = u32::from_be_bytes([body[20], body[21], body[22], body[23]]);
    if hs_type != 0xFFFFFFFF {
        return None;
    }
    // Walk extensions starting at body+48.
    let mut ext = &body[48..];
    while ext.len() >= 4 {
        let ext_type = u16::from_be_bytes([ext[0], ext[1]]);
        let ext_len_words = u16::from_be_bytes([ext[2], ext[3]]) as usize;
        let ext_data_len = ext_len_words.saturating_mul(4);
        if ext.len() < 4 + ext_data_len {
            break;
        }
        if ext_type == 0x0005 {
            // SID extension — bytes are 4-byte words with bytes
            // reversed per word from the original UTF-8 string.
            let sid_bytes = &ext[4..4 + ext_data_len];
            return decode_srt_sid(sid_bytes);
        }
        ext = &ext[4 + ext_data_len..];
    }
    None
}

/// Decode an SRT SID payload into a UTF-8 string. Each 4-byte word
/// in the payload is byte-reversed from the source string (per RFC
/// 8723 §3.2.1.1.3); we reverse each chunk back, concatenate, and
/// strip trailing nulls (string padding to 4-byte alignment).
pub(crate) fn decode_srt_sid(bytes: &[u8]) -> Option<String> {
    let mut decoded = Vec::with_capacity(bytes.len());
    for chunk in bytes.chunks(4) {
        let mut word: Vec<u8> = chunk.to_vec();
        word.reverse();
        decoded.extend_from_slice(&word);
    }
    while decoded.last() == Some(&0) {
        decoded.pop();
    }
    String::from_utf8(decoded).ok()
}

/// Extract the BMD stream key from a streamid string. BMD-flavored
/// streamids look like `#!::bmd_uuid=UUID,bmd_name=NAME,u=KEY` —
/// the key is the value of the `u=` field. Returns None when the
/// streamid isn't BMD-flavored or the key field is missing/empty.
pub(crate) fn extract_bmd_key(streamid: &str) -> Option<String> {
    let body = streamid.strip_prefix("#!::").unwrap_or(streamid);
    for field in body.split(',') {
        if let Some(val) = field.strip_prefix("u=") {
            let val = val.trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Decode a hex string (lowercase or uppercase, optionally with
/// `:` separators) into bytes. Returns an empty Vec on any
/// non-hex character (no errors — tshark sometimes emits
/// truncated payloads which we tolerate).
pub(crate) fn hex_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut nibble: Option<u8> = None;
    for &b in bytes {
        if let Some(d) = hex_digit(b) {
            match nibble.take() {
                Some(hi) => out.push((hi << 4) | d),
                None => nibble = Some(d),
            }
        }
        // Non-hex (`:`, whitespace) just skipped — no nibble reset
        // because tshark sometimes uses `:` between every byte.
    }
    out
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn parse_tshark_bool(field: &str) -> Option<bool> {
    match field.trim() {
        "1" | "True" | "true" => Some(true),
        "0" | "False" | "false" => Some(false),
        _ => None,
    }
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
        // UI mode also tolerates a missing URL: the dashboard
        // pre-fills the configured ATEM target as the default, but
        // the active-probe loop is gated by the diag mode (Live by
        // default = no probes), so the URL existing here doesn't
        // mean we'll hammer the destination on launch.
        if (monitor_iface.is_some() || ui_port.is_some()) && url.is_none() {
            return Ok(Self {
                url: format!("srt://{DEFAULT_ATEM_IP}:{DEFAULT_ATEM_PORT}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_sid_single_word() {
        // "abcd" is stored byte-reversed per 4-byte word, so the
        // wire bytes are [d, c, b, a]. Decode reverses each word
        // back to UTF-8 order.
        let bytes = [b'd', b'c', b'b', b'a'];
        assert_eq!(decode_srt_sid(&bytes), Some("abcd".to_string()));
    }

    #[test]
    fn decode_sid_with_null_padding() {
        // "abcde" pads to 8 bytes (2 words). Word 1 = "abcd" sent
        // as "dcba"; word 2 = "e\0\0\0" sent as "\0\0\0e". After
        // reversal we get "abcde\0\0\0" then strip trailing nulls.
        let bytes = [b'd', b'c', b'b', b'a', 0, 0, 0, b'e'];
        assert_eq!(decode_srt_sid(&bytes), Some("abcde".to_string()));
    }

    #[test]
    fn extract_bmd_key_full_streamid() {
        let s = "#!::bmd_uuid=d1a90517-1c00-4e57-9fab-617465616d64,bmd_name=ATEM-net-diag,u=q1ry-abcd-1234";
        assert_eq!(extract_bmd_key(s), Some("q1ry-abcd-1234".to_string()));
    }

    #[test]
    fn extract_bmd_key_no_prefix() {
        assert_eq!(extract_bmd_key("u=onlykey"), Some("onlykey".to_string()));
    }

    #[test]
    fn extract_bmd_key_missing() {
        assert_eq!(extract_bmd_key("no key here"), None);
        assert_eq!(extract_bmd_key("user=foo,name=bar"), None);
    }

    #[test]
    fn hex_decode_basic() {
        assert_eq!(hex_decode("64636261"), vec![0x64, 0x63, 0x62, 0x61]);
        assert_eq!(hex_decode("64:63:62:61"), vec![0x64, 0x63, 0x62, 0x61]);
        assert_eq!(hex_decode(""), Vec::<u8>::new());
        // Mixed case + odd characters tolerated; `:` separator is
        // ignored whether single or repeated.
        assert_eq!(hex_decode("FF::aa"), vec![0xFF, 0xAA]);
    }

    #[test]
    fn parse_full_hsv5_conclusion_with_sid() {
        // Build a synthetic SRT HSv5 conclusion handshake control
        // packet carrying a SID extension with streamid "abcd".
        let mut packet = Vec::new();
        // Control packet header — 16 bytes.
        packet.extend_from_slice(&[0x80, 0x00]); // control bit + type 0 (handshake)
        packet.extend_from_slice(&[0x00, 0x00]); // subtype
        packet.extend_from_slice(&[0u8; 4]); // type-specific
        packet.extend_from_slice(&[0u8; 4]); // timestamp
        packet.extend_from_slice(&[0u8; 4]); // dst socket ID
        // Handshake body — 48 bytes.
        packet.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]); // version 5
        packet.extend_from_slice(&[0x00, 0x00]); // encryption
        packet.extend_from_slice(&[0x00, 0x04]); // ext flag with SID bit
        packet.extend_from_slice(&[0u8; 4]); // initial seq
        packet.extend_from_slice(&[0x00, 0x00, 0x05, 0xDC]); // MTU 1500
        packet.extend_from_slice(&[0x00, 0x00, 0x20, 0x00]); // max flow window
        packet.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // hs type = conclusion
        packet.extend_from_slice(&[0u8; 4]); // SRT socket ID
        packet.extend_from_slice(&[0u8; 4]); // SYN cookie
        packet.extend_from_slice(&[0u8; 16]); // peer IP
        // SID extension.
        packet.extend_from_slice(&[0x00, 0x05]); // ext type = SID
        packet.extend_from_slice(&[0x00, 0x01]); // length = 1 word
        packet.extend_from_slice(&[b'd', b'c', b'b', b'a']); // "abcd" byte-swapped
        assert_eq!(
            parse_srt_handshake_streamid(&packet),
            Some("abcd".to_string())
        );
    }

    #[test]
    fn parse_rejects_non_handshake_control() {
        // Control bit set but type ≠ 0 — should reject early.
        let mut packet = vec![0x80, 0x02]; // control + type 2 (ACK)
        packet.extend_from_slice(&[0u8; 70]);
        assert_eq!(parse_srt_handshake_streamid(&packet), None);
    }

    #[test]
    fn parse_rejects_data_packet() {
        // Control bit clear — data packet, not control.
        let packet = vec![0x00; 100];
        assert_eq!(parse_srt_handshake_streamid(&packet), None);
    }

    #[test]
    fn parse_rejects_too_short() {
        assert_eq!(parse_srt_handshake_streamid(&[]), None);
        assert_eq!(parse_srt_handshake_streamid(&[0x80; 16]), None);
    }
}
