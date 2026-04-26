//! Visual dashboard mode — embedded HTTP server + single-page HTML
//! UI driven by JSON polling. Reuses the probe + monitor logic by
//! running them in background threads that write into a shared
//! `Arc<Mutex<DashboardState>>`. The HTTP server reads from that
//! state on each `/api/state` request and the JS UI polls at 1Hz.
//!
//! Why this lives in a separate module: the CLI mode in main.rs is
//! a long-running stdout-printer with no shared-state requirement.
//! The dashboard needs both that data flow AND a passive consumer,
//! so it gets its own surface that reuses the existing helpers
//! (probe(), parse_tshark_line()) without disturbing them.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tiny_http::{Header, Response, Server};

use crate::{
    build_bmd_srt_url, find_tshark, parse_tshark_line, probe as probe_url, FlowKey, FlowStats,
    ProbeOutcome, Stats,
};

const KEEP_LAST_PROBES: usize = 250;
const FLOW_STALE_SECS: u64 = 60;

#[derive(Default)]
pub struct DashboardState {
    pub started_at: u64,
    pub config: ConfigSnapshot,
    pub probes: VecDeque<ProbeRecord>,
    pub per_key: HashMap<String, Stats>,
    pub flows: HashMap<FlowKey, FlowStats>,
    /// Last time any probe was attempted; lets the UI show "X seconds
    /// since last probe" so a hung worker is visible.
    pub last_probe_at: u64,
    /// Last time any flow packet was received; same reason for tshark.
    pub last_flow_at: u64,
    pub probe_thread_alive: bool,
    pub monitor_thread_alive: bool,
}

#[derive(Clone, Default, Serialize)]
pub struct ConfigSnapshot {
    pub url: String,
    pub keys: Vec<String>,
    pub interval_secs: u64,
    pub monitor_iface: Option<String>,
    pub monitor_ports: Vec<u16>,
}

#[derive(Clone, Serialize)]
pub struct ProbeRecord {
    pub timestamp: u64,
    pub key: String,
    pub outcome: String,
    pub latency_ms: u64,
}

#[derive(Serialize)]
pub struct StateResponse<'a> {
    pub started_at: u64,
    pub now: u64,
    pub config: &'a ConfigSnapshot,
    pub probes: Vec<&'a ProbeRecord>,
    pub per_key: HashMap<String, StatsSnapshot>,
    pub flows: Vec<FlowSnapshot>,
    pub last_probe_at: u64,
    pub last_flow_at: u64,
    pub probe_thread_alive: bool,
    pub monitor_thread_alive: bool,
}

#[derive(Serialize)]
pub struct StatsSnapshot {
    pub probes: u64,
    pub connected: u64,
    pub rejected: u64,
    pub timeout: u64,
    pub latency_min_ms: u64,
    pub latency_max_ms: u64,
    pub latency_avg_ms: u64,
    pub success_pct: f64,
    pub last_outcome: Option<String>,
}

#[derive(Serialize)]
pub struct FlowSnapshot {
    pub src_ip: String,
    pub src_port: u16,
    pub dst_ip: String,
    pub dst_port: u16,
    pub is_udp: bool,
    pub protocol: String,
    pub total_bytes: u64,
    pub total_packets: u64,
    pub recent_bytes: u64,
    pub idle_secs: f64,
}

/// Run the dashboard. Spawns probe + (optional) monitor threads,
/// then blocks the main thread on the HTTP server. Loops until
/// SIGINT / Ctrl-C kills the process — no graceful shutdown
/// machinery (the OS handles it).
pub fn run(cli: &crate::Cli, port: u16) -> ! {
    let state = Arc::new(Mutex::new(DashboardState {
        started_at: now_secs(),
        config: ConfigSnapshot {
            url: cli.url.clone(),
            keys: cli.keys.clone(),
            interval_secs: cli.interval.as_secs(),
            monitor_iface: cli.monitor_iface.clone(),
            monitor_ports: cli.monitor_ports.clone(),
        },
        ..Default::default()
    }));

    if !cli.url.starts_with("monitor://") {
        let probe_state = state.clone();
        let probe_cli = ProbeThreadConfig {
            url: cli.url.clone(),
            keys: cli.keys.clone(),
            interval: cli.interval,
        };
        std::thread::Builder::new()
            .name("probe-loop".into())
            .spawn(move || probe_loop(probe_cli, probe_state))
            .expect("spawn probe loop");
    }

    if let Some(iface) = cli.monitor_iface.clone() {
        let mon_state = state.clone();
        let ports = if cli.monitor_ports.is_empty() {
            crate::DEFAULT_MONITOR_PORTS.to_vec()
        } else {
            cli.monitor_ports.clone()
        };
        std::thread::Builder::new()
            .name("monitor-loop".into())
            .spawn(move || monitor_loop(iface, ports, mon_state))
            .expect("spawn monitor loop");
    }

    let bind_addr = format!("127.0.0.1:{port}");
    let server = match Server::http(&bind_addr) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("[dashboard] failed to bind {bind_addr}: {err}");
            std::process::exit(3);
        }
    };
    eprintln!(
        "[dashboard] listening on http://{bind_addr}/  ·  open in your browser"
    );

    let _ = Command::new("open")
        .arg(format!("http://{bind_addr}/"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    for request in server.incoming_requests() {
        let url = request.url().to_string();
        let response = match url.as_str() {
            "/" | "/index.html" => {
                let mut r = Response::from_string(INDEX_HTML);
                r.add_header(html_header());
                r
            }
            "/api/state" => {
                let body = build_state_json(&state);
                let mut r = Response::from_string(body);
                r.add_header(json_header());
                r.add_header(no_cache_header());
                r
            }
            _ => Response::from_string("404").with_status_code(404),
        };
        let _ = request.respond(response);
    }
    unreachable!("incoming_requests is an infinite iterator");
}

fn build_state_json(state: &Arc<Mutex<DashboardState>>) -> String {
    let s = state.lock().unwrap();
    let probes: Vec<&ProbeRecord> = s.probes.iter().collect();
    let mut per_key: HashMap<String, StatsSnapshot> = HashMap::new();
    for (k, st) in &s.per_key {
        let last_outcome = s
            .probes
            .iter()
            .rev()
            .find(|p| &p.key == k)
            .map(|p| p.outcome.clone());
        per_key.insert(
            k.clone(),
            StatsSnapshot {
                probes: st.total_probes,
                connected: st.connected,
                rejected: st.rejected,
                timeout: st.timeout,
                latency_min_ms: st.latency_min_ms,
                latency_max_ms: st.latency_max_ms,
                latency_avg_ms: if st.total_probes == 0 {
                    0
                } else {
                    st.latency_sum_ms / st.total_probes
                },
                success_pct: if st.total_probes == 0 {
                    0.0
                } else {
                    (st.connected as f64 / st.total_probes as f64) * 100.0
                },
                last_outcome,
            },
        );
    }
    let now = now_secs();
    let mut flows: Vec<FlowSnapshot> = s
        .flows
        .iter()
        .filter_map(|(k, fs)| {
            let idle_secs = fs
                .last_seen
                .map(|t| Instant::now().duration_since(t).as_secs_f64())
                .unwrap_or(f64::INFINITY);
            if idle_secs > FLOW_STALE_SECS as f64 {
                return None;
            }
            Some(FlowSnapshot {
                src_ip: k.src_ip.clone(),
                src_port: k.src_port,
                dst_ip: k.dst_ip.clone(),
                dst_port: k.dst_port,
                is_udp: k.is_udp,
                protocol: k.protocol.clone(),
                total_bytes: fs.total_bytes,
                total_packets: fs.total_packets,
                recent_bytes: fs.window_bytes,
                idle_secs,
            })
        })
        .collect();
    flows.sort_by(|a, b| b.recent_bytes.cmp(&a.recent_bytes));
    let resp = StateResponse {
        started_at: s.started_at,
        now,
        config: &s.config,
        probes,
        per_key,
        flows,
        last_probe_at: s.last_probe_at,
        last_flow_at: s.last_flow_at,
        probe_thread_alive: s.probe_thread_alive,
        monitor_thread_alive: s.monitor_thread_alive,
    };
    serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into())
}

struct ProbeThreadConfig {
    url: String,
    keys: Vec<String>,
    interval: Duration,
}

fn probe_loop(cli: ProbeThreadConfig, state: Arc<Mutex<DashboardState>>) {
    state.lock().unwrap().probe_thread_alive = true;
    loop {
        if cli.keys.is_empty() {
            do_one_probe(&cli.url, "", &state);
        } else {
            for key in &cli.keys {
                if let Ok(url) = build_bmd_srt_url(&cli.url, key) {
                    do_one_probe(&url, key, &state);
                }
            }
        }
        std::thread::sleep(cli.interval);
    }
}

fn do_one_probe(url: &str, key: &str, state: &Arc<Mutex<DashboardState>>) {
    let started = Instant::now();
    let result = probe_url(url);
    let latency_ms = started.elapsed().as_millis() as u64;
    let outcome_label = match result {
        ProbeOutcome::Connected => "connected",
        ProbeOutcome::Rejected => "rejected",
        ProbeOutcome::Timeout => "timeout",
    };
    let mut s = state.lock().unwrap();
    let stats = s.per_key.entry(key.to_string()).or_default();
    stats.record(&result, latency_ms);
    s.probes.push_back(ProbeRecord {
        timestamp: now_secs(),
        key: key.to_string(),
        outcome: outcome_label.into(),
        latency_ms,
    });
    while s.probes.len() > KEEP_LAST_PROBES {
        s.probes.pop_front();
    }
    s.last_probe_at = now_secs();
}

fn monitor_loop(iface: String, ports: Vec<u16>, state: Arc<Mutex<DashboardState>>) {
    let tshark = match find_tshark() {
        Some(p) => p,
        None => {
            eprintln!("[monitor] tshark not found; install Wireshark");
            return;
        }
    };
    let filter = ports
        .iter()
        .map(|p| format!("port {p}"))
        .collect::<Vec<_>>()
        .join(" or ");
    let mut child = match Command::new(tshark)
        .args([
            "-i", &iface, "-l", "-f", &filter, "-T", "fields", "-E", "separator=,",
            "-e", "frame.time_relative", "-e", "ip.src", "-e", "tcp.srcport",
            "-e", "udp.srcport", "-e", "ip.dst", "-e", "tcp.dstport", "-e",
            "udp.dstport", "-e", "_ws.col.Protocol", "-e", "frame.len",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(err) => {
            eprintln!("[monitor] failed to spawn tshark: {err}");
            return;
        }
    };
    state.lock().unwrap().monitor_thread_alive = true;
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return,
    };
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(packet) = parse_tshark_line(&line) {
            let mut s = state.lock().unwrap();
            let key = packet.flow_key();
            let stats = s.flows.entry(key).or_default();
            stats.record(packet.bytes);
            s.last_flow_at = now_secs();
        }
    }
    state.lock().unwrap().monitor_thread_alive = false;
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn html_header() -> Header {
    Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap()
}
fn json_header() -> Header {
    Header::from_bytes("Content-Type", "application/json").unwrap()
}
fn no_cache_header() -> Header {
    Header::from_bytes("Cache-Control", "no-cache, no-store, must-revalidate").unwrap()
}

const INDEX_HTML: &str = include_str!("dashboard.html");
