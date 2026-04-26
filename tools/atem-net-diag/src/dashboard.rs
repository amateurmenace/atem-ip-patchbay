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
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tiny_http::{Header, Response, Server};

use crate::{
    build_bmd_srt_url, find_tshark, parse_tshark_line, probe as probe_url, tshark_field_args,
    FlowKey, FlowStats, ProbeOutcome, Stats,
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
    pub control_packets: u64,
    pub recent_bytes: u64,
    pub idle_secs: f64,
    pub duration_secs: f64,
    /// Per-second byte samples over the last 60s (or however many
    /// have been collected). Front of vec is oldest.
    pub bitrate_samples: Vec<u64>,
    /// Most recent SRT ACK we saw for this flow, if any. None for
    /// non-SRT flows or SRT flows where no ACKs have arrived yet
    /// (e.g. the very first second of a fresh handshake).
    pub last_srt_rtt_ms: Option<u64>,
    pub last_srt_rttvar_ms: Option<u64>,
    pub last_srt_bw_kbps: Option<u32>,
    pub last_srt_rate_kbps: Option<u32>,
    pub last_srt_buf_pkts: Option<u32>,
    pub last_srt_ack_idle_secs: Option<f64>,
    /// Health classification — driven by the data points above.
    /// "streaming" / "stalling" / "idle" / "handshake" / "unknown".
    pub health: String,
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

    // Always start the probe loop in UI mode. Even if the user
    // launched with no key (or url is "monitor://"), they may set
    // one through the dashboard's config form, and we want the loop
    // ready to pick up that change without a process restart.
    let probe_state = state.clone();
    std::thread::Builder::new()
        .name("probe-loop".into())
        .spawn(move || probe_loop(probe_state))
        .expect("spawn probe loop");

    // Auto-start the flow monitor in UI mode even if --monitor
    // wasn't explicitly passed. The dashboard's value proposition
    // is "see what streams are flowing right now", and that needs
    // tshark — making the operator pass an extra flag for the
    // headline feature is a bad default. If tshark is missing, the
    // monitor thread exits cleanly and the dashboard reports the
    // missing dep.
    let monitor_iface = cli.monitor_iface.clone().unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            "en0".to_string()
        } else {
            "any".to_string()
        }
    });
    {
        let mon_state = state.clone();
        let ports = if cli.monitor_ports.is_empty() {
            crate::DEFAULT_MONITOR_PORTS.to_vec()
        } else {
            cli.monitor_ports.clone()
        };
        // Reflect the auto-picked iface into config so the UI can
        // show + edit it.
        state.lock().unwrap().config.monitor_iface = Some(monitor_iface.clone());
        std::thread::Builder::new()
            .name("monitor-loop".into())
            .spawn(move || monitor_loop(monitor_iface, ports, mon_state))
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

    for mut request in server.incoming_requests() {
        let url = request.url().to_string();
        let method = request.method().clone();
        let is_post = matches!(method, tiny_http::Method::Post);
        let response = match (is_post, url.as_str()) {
            (false, "/") | (false, "/index.html") => {
                let mut r = Response::from_string(INDEX_HTML);
                r.add_header(html_header());
                r
            }
            (false, "/api/state") => {
                let body = build_state_json(&state);
                let mut r = Response::from_string(body);
                r.add_header(json_header());
                r.add_header(no_cache_header());
                r
            }
            (true, "/api/config") => {
                let mut body = String::new();
                if request.as_reader().read_to_string(&mut body).is_err() {
                    Response::from_string(r#"{"error":"failed to read body"}"#)
                        .with_status_code(400)
                } else {
                    let result = apply_config_update(&state, &body);
                    let mut r = Response::from_string(match &result {
                        Ok(()) => r#"{"ok":true}"#.to_string(),
                        Err(e) => format!(r#"{{"error":{}}}"#, json_escape(e)),
                    });
                    r.add_header(json_header());
                    r.add_header(no_cache_header());
                    if result.is_err() {
                        r = r.with_status_code(400);
                    }
                    r
                }
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
    let now_inst = Instant::now();
    let mut flows: Vec<FlowSnapshot> = s
        .flows
        .iter()
        .filter_map(|(k, fs)| {
            let idle_secs = fs
                .last_seen
                .map(|t| now_inst.duration_since(t).as_secs_f64())
                .unwrap_or(f64::INFINITY);
            if idle_secs > FLOW_STALE_SECS as f64 {
                return None;
            }
            let duration_secs = fs
                .first_seen
                .map(|t| now_inst.duration_since(t).as_secs_f64())
                .unwrap_or(0.0);
            let bitrate_samples: Vec<u64> = fs.bitrate_samples.iter().copied().collect();
            let ack_idle = fs
                .last_srt_ack_at
                .map(|t| now_inst.duration_since(t).as_secs_f64());
            // SRT ACK fields: bandwidth + rate are reported as
            // packets/second; convert to a kbps approximation
            // assuming 1316-byte SRT payload (the BMD-flavored
            // MPEG-TS-over-SRT default — close enough for live
            // bandwidth display).
            let pkts_to_kbps = |pkts: u32| (pkts as u64 * 1316 * 8 / 1000) as u32;
            let last_srt_rtt_ms = fs.last_srt_ack.as_ref()
                .and_then(|a| a.rtt_us.map(|u| (u / 1000) as u64));
            let last_srt_rttvar_ms = fs.last_srt_ack.as_ref()
                .and_then(|a| a.rttvar_us.map(|u| (u / 1000) as u64));
            let last_srt_bw_kbps = fs.last_srt_ack.as_ref()
                .and_then(|a| a.bw_pkts_s.map(pkts_to_kbps));
            let last_srt_rate_kbps = fs.last_srt_ack.as_ref()
                .and_then(|a| a.rate_pkts_s.map(pkts_to_kbps));
            let last_srt_buf_pkts = fs.last_srt_ack.as_ref()
                .and_then(|a| a.buf_avail_pkts);
            // Health heuristic. "streaming" if bytes flowed in the
            // current/last sample window. "stalling" if the flow
            // was active but recent_bytes is zero (packets stopped
            // mid-stream). "idle" if it's been quiet for several
            // seconds. "handshake" if we've seen control packets
            // but very few data packets — sender warming up.
            let total_data_packets = fs.total_packets.saturating_sub(fs.control_packets);
            let health = if fs.window_bytes > 0 {
                "streaming"
            } else if idle_secs > 5.0 {
                "idle"
            } else if total_data_packets < 10 && fs.control_packets > 0 {
                "handshake"
            } else if fs.total_packets > 0 {
                "stalling"
            } else {
                "unknown"
            }
            .to_string();
            Some(FlowSnapshot {
                src_ip: k.src_ip.clone(),
                src_port: k.src_port,
                dst_ip: k.dst_ip.clone(),
                dst_port: k.dst_port,
                is_udp: k.is_udp,
                protocol: k.protocol.clone(),
                total_bytes: fs.total_bytes,
                total_packets: fs.total_packets,
                control_packets: fs.control_packets,
                recent_bytes: fs.window_bytes,
                idle_secs,
                duration_secs,
                bitrate_samples,
                last_srt_rtt_ms,
                last_srt_rttvar_ms,
                last_srt_bw_kbps,
                last_srt_rate_kbps,
                last_srt_buf_pkts,
                last_srt_ack_idle_secs: ack_idle,
                health,
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

fn probe_loop(state: Arc<Mutex<DashboardState>>) {
    state.lock().unwrap().probe_thread_alive = true;
    loop {
        // Snapshot the config under the lock so we don't hold it
        // through the FFmpeg shell-out (multi-second blocking call).
        let (url, keys, interval) = {
            let s = state.lock().unwrap();
            (
                s.config.url.clone(),
                s.config.keys.clone(),
                Duration::from_secs(s.config.interval_secs.max(1)),
            )
        };
        // No URL configured yet (UI mode launched bare) — skip this
        // cycle and try again. The dashboard's config form sets the
        // URL when the user submits it.
        if url.is_empty() || url == "monitor://" {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        if keys.is_empty() {
            do_one_probe(&url, "", &state);
        } else {
            for key in &keys {
                if let Ok(probe_url_built) = build_bmd_srt_url(&url, key) {
                    do_one_probe(&probe_url_built, key, &state);
                }
            }
        }
        std::thread::sleep(interval);
    }
}

/// Parse a JSON config update from the dashboard form and apply it
/// to the shared state. Per-key Stats / probe history are reset
/// when the URL or keys change so the dashboard's success-rate /
/// latency numbers reflect only the new configuration. If the user
/// just tweaks the interval, history is preserved.
fn apply_config_update(state: &Arc<Mutex<DashboardState>>, body: &str) -> Result<(), String> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("invalid JSON: {e}"))?;
    let url = parsed
        .get("url")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let keys: Vec<String> = parsed
        .get("keys")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let interval = parsed
        .get("interval_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if !url.is_empty()
        && !(url.starts_with("srt://")
            || url.starts_with("rtmp://")
            || url.starts_with("rtmps://"))
    {
        return Err(format!(
            "URL must start with srt://, rtmp://, or rtmps:// (got {url:?})"
        ));
    }
    let mut s = state.lock().unwrap();
    let cfg_changed = s.config.url != url || s.config.keys != keys;
    s.config.url = url;
    s.config.keys = keys;
    if interval > 0 {
        s.config.interval_secs = interval;
    }
    if cfg_changed {
        // Reset per-key stats + probe history when the target /
        // keys change. Otherwise the dashboard mixes data from
        // different probe targets into the same cards.
        s.per_key.clear();
        s.probes.clear();
        s.last_probe_at = 0;
    }
    Ok(())
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
        .args(tshark_field_args(&iface, &filter))
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
            if let Some(ack) = packet.srt {
                stats.record_srt_ack(ack);
            }
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
