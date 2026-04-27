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

use crate::unifi;
use crate::{
    build_bmd_srt_url, extract_bmd_key, find_tshark, parse_tshark_line, probe as probe_url,
    tshark_field_args, FlowKey, FlowStats, ProbeOutcome, Stats, DEFAULT_ATEM_IP, DEFAULT_ATEM_MAC,
    DEFAULT_ATEM_PORT, DEFAULT_UDM_HOST,
};

const KEEP_LAST_PROBES: usize = 250;
const FLOW_STALE_SECS: u64 = 60;

/// Diag mode controls whether the active-probe loop fires. Live is
/// the default — pure passive monitoring (UDM polling + optional
/// tshark) with zero outbound traffic to the ATEM. Standby enables
/// the FFmpeg handshake probe for explicit reachability testing
/// when no production is in progress; the operator must opt in.
///
/// The split exists because active probes consume a connection slot
/// at the receiver and can be REJECTED (or contend with) a
/// production stream that's already using the configured key. The
/// previous default behavior (always probe) was wrong for live use.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiagMode {
    Live,
    Standby,
}

impl Default for DiagMode {
    fn default() -> Self {
        DiagMode::Live
    }
}

impl DiagMode {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "live" => Some(DiagMode::Live),
            "standby" => Some(DiagMode::Standby),
            _ => None,
        }
    }
}

/// The ATEM (or BMD streaming bridge) we're correlating UDM client
/// data against. MAC is the primary identifier — UniFi's stat/sta
/// keys clients by MAC, so a MAC match survives DHCP renewals. IP
/// is used for tshark flow filtering and as the user-visible label.
#[derive(Clone, Debug, Serialize)]
pub struct AtemTarget {
    pub ip: String,
    pub mac: Option<String>,
    pub port: u16,
}

impl Default for AtemTarget {
    fn default() -> Self {
        Self {
            ip: DEFAULT_ATEM_IP.to_string(),
            mac: Some(DEFAULT_ATEM_MAC.to_string()),
            port: DEFAULT_ATEM_PORT,
        }
    }
}

/// State of the UDM connection. Surfaced to the dashboard so the
/// operator sees at a glance whether UDM polling is healthy.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UnifiStatus {
    NotConfigured,
    Connecting,
    Connected { last_poll_at: u64 },
    Failed { error: String, last_attempt: u64 },
}

/// State of the gateway WAN-bandwidth poll. Same shape as UnifiStatus
/// but tracked separately because the WAN data uses a different
/// endpoint and could be working while client polling is broken (or
/// vice versa).
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WanStatus {
    NotConfigured,
    Connecting,
    Connected { last_poll_at: u64 },
    Failed { error: String, last_attempt: u64 },
}

impl Default for WanStatus {
    fn default() -> Self {
        WanStatus::NotConfigured
    }
}

/// Snapshot of the gateway's WAN-side throughput + identity. Polled
/// from the UDM's stat/health endpoint. Fields use **device-perspective**
/// from the gateway's WAN port: upload = bytes leaving the LAN out to
/// the public internet (operator's primary concern for live broadcast
/// upstream), download = bytes coming in from the public internet
/// (the inbound port-forwarded streams to the ATEM, plus all other
/// download traffic on the LAN).
#[derive(Clone, Debug, Serialize)]
pub struct WanSnapshot {
    pub upload_kbps: u64,
    pub download_kbps: u64,
    pub wan_ip: Option<String>,
    pub isp_name: Option<String>,
    pub isp_org: Option<String>,
    pub wan_latency_ms: Option<u64>,
    pub wan_status: Option<String>,
}

impl Default for UnifiStatus {
    fn default() -> Self {
        UnifiStatus::NotConfigured
    }
}

/// Per-client snapshot from the UDM's stat/sta endpoint, augmented
/// with delta-derived bandwidth (kbps over the last poll cycle).
/// One entry per client the UDM currently knows about; we don't
/// filter to the ATEM only because the operator wants to see
/// *which* clients are talking to the ATEM right now.
#[derive(Clone, Debug, Serialize)]
pub struct UnifiClientSnapshot {
    pub mac: String,
    pub ip: Option<String>,
    pub hostname: Option<String>,
    pub name: Option<String>,
    pub oui: Option<String>,
    pub last_seen: u64,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub tx_kbps: u64,
    pub rx_kbps: u64,
    pub is_atem: bool,
    pub is_wired: bool,
}

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
    /// Per-client snapshots from the most recent UDM poll. Empty
    /// when UDM isn't configured or the first poll hasn't completed.
    pub unifi_clients: Vec<UnifiClientSnapshot>,
    pub unifi_status: UnifiStatus,
    pub last_unifi_poll_at: u64,
    pub unifi_thread_alive: bool,
    /// Per-flow → stream-key mapping derived from the SRT HSv5
    /// handshake's SID extension. Captures across the lifetime of
    /// each flow; falls back to empty until a handshake is observed.
    /// Keyed by FlowKey so the stream card can look up its key.
    pub flow_keys: HashMap<FlowKey, String>,

    /// Latest WAN-side throughput snapshot from the UDM's
    /// stat/health endpoint. None until first poll completes (or
    /// when WAN polling isn't configured / failing).
    pub wan_snapshot: Option<WanSnapshot>,
    pub wan_status: WanStatus,
    pub last_wan_poll_at: u64,
    pub wan_thread_alive: bool,
    /// 60-second history of (timestamp, kbps) pairs for graphing the
    /// WAN upload + download rate as sparklines on the dashboard.
    /// Front of deque is oldest sample.
    pub wan_upload_history: VecDeque<(u64, u64)>,
    pub wan_download_history: VecDeque<(u64, u64)>,
}

#[derive(Clone, Default, Serialize)]
pub struct ConfigSnapshot {
    pub url: String,
    pub keys: Vec<String>,
    pub interval_secs: u64,
    pub monitor_iface: Option<String>,
    pub monitor_ports: Vec<u16>,
    /// Diag mode — Live (no probes) by default. The operator opts
    /// into Standby explicitly when they want to verify reachability
    /// against a destination not currently in production.
    pub mode: DiagMode,
    /// ATEM identification — used to highlight the ATEM in the
    /// network-clients list (UDM source) and to filter tshark flow
    /// monitoring (when on the streamer's machine). MAC is the
    /// stable identity; IP changes when DHCP rotates leases.
    pub atem: AtemTarget,
    /// UDM controller URL (e.g. https://192.168.20.1). Pre-filled
    /// from DEFAULT_UDM_HOST; the operator can override via the
    /// dashboard form.
    pub unifi_host: String,
    /// True when an API key is configured (env or form). The key
    /// itself is intentionally NOT in this struct — ConfigSnapshot
    /// is serialized into the public /api/state response.
    pub unifi_configured: bool,

    /// Operator's WAN upload cap in Mbps (e.g., 20.0 for a 20-up
    /// fiber plan). Used to render the headroom indicator: "14/20
    /// Mbps used (70%)". 0 = not configured (no headroom shown).
    #[serde(default)]
    pub wan_upload_cap_mbps: f64,
    /// Operator's WAN download cap in Mbps. Same role as upload but
    /// usually less critical — most plans are asymmetric and inbound
    /// streams to the ATEM rarely saturate downstream. 0 = not
    /// configured.
    #[serde(default)]
    pub wan_download_cap_mbps: f64,
    /// Per-source-IP friendly labels for stream identification. Maps
    /// "192.168.5.41" or "207.180.22.3" → "Jamie's basement". Persisted
    /// only in-memory for now; survives the lifetime of the binary.
    /// Operator edits via the dashboard.
    #[serde(default)]
    pub source_labels: HashMap<String, String>,
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
    pub unifi_status: &'a UnifiStatus,
    pub unifi_clients: &'a [UnifiClientSnapshot],
    pub last_unifi_poll_at: u64,
    pub unifi_thread_alive: bool,
    pub wan_status: &'a WanStatus,
    pub wan_snapshot: Option<&'a WanSnapshot>,
    pub last_wan_poll_at: u64,
    pub wan_thread_alive: bool,
    pub wan_upload_kbps_history: Vec<u64>,
    pub wan_download_kbps_history: Vec<u64>,
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
    /// Stream key extracted from the SRT HSv5 conclusion handshake's
    /// SID extension. For BMD-flavored streamids, this is just the
    /// `u=` field (the operator-visible key). For non-BMD streamids,
    /// it's the full SID string. None until a handshake is observed
    /// for this flow.
    pub stream_key: Option<String>,
}

/// Run the dashboard. Spawns probe + (optional) monitor threads,
/// then blocks the main thread on the HTTP server. Loops until
/// SIGINT / Ctrl-C kills the process — no graceful shutdown
/// machinery (the OS handles it).
pub fn run(cli: &crate::Cli, port: u16) -> ! {
    // Read UDM creds from env at startup. The API key (or
    // username/password) never enters DashboardState — that struct
    // is serialized to /api/state and visible to anyone who hits
    // the dashboard URL on this machine. Credentials live only in
    // the UniFi polling thread's local state.
    let unifi_host = std::env::var("UDM_HOST")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_UDM_HOST.to_string());
    let unifi_credentials = unifi::read_credentials_from_env();
    let unifi_configured = unifi_credentials.is_some();

    let state = Arc::new(Mutex::new(DashboardState {
        started_at: now_secs(),
        config: ConfigSnapshot {
            url: cli.url.clone(),
            keys: cli.keys.clone(),
            interval_secs: cli.interval.as_secs(),
            monitor_iface: cli.monitor_iface.clone(),
            monitor_ports: cli.monitor_ports.clone(),
            mode: DiagMode::Live,
            atem: AtemTarget::default(),
            unifi_host,
            unifi_configured,
            wan_upload_cap_mbps: 0.0,
            wan_download_cap_mbps: 0.0,
            source_labels: HashMap::new(),
        },
        ..Default::default()
    }));

    // Always start the probe loop in UI mode. The mode-gate
    // inside the loop suppresses actual probes until the operator
    // switches to Standby; in Live mode the thread sleeps idle.
    // Spawning unconditionally means a mode flip takes effect on
    // the next loop iteration with no thread-spawn delay.
    let probe_state = state.clone();
    std::thread::Builder::new()
        .name("probe-loop".into())
        .spawn(move || probe_loop(probe_state))
        .expect("spawn probe loop");

    // UDM polling thread — only spawn if creds are configured.
    // No creds means the dashboard reports "UDM: not configured"
    // and the operator can still use the tool with active probes
    // (Standby mode) and tshark monitoring as data sources.
    if let Some(creds) = unifi_credentials {
        let unifi_state = state.clone();
        let host = state.lock().unwrap().config.unifi_host.clone();
        let creds_for_clients = creds.clone();
        std::thread::Builder::new()
            .name("unifi-poll".into())
            .spawn(move || unifi::poll_loop(host, creds_for_clients, unifi_state))
            .expect("spawn unifi poll");

        // Separate thread for WAN bandwidth — same UDM, different
        // endpoint. Kept independent so a slow / failing WAN endpoint
        // doesn't starve client polling and vice versa.
        let wan_state = state.clone();
        let wan_host = state.lock().unwrap().config.unifi_host.clone();
        std::thread::Builder::new()
            .name("wan-poll".into())
            .spawn(move || unifi::wan_poll_loop(wan_host, creds, wan_state))
            .expect("spawn wan poll");
    }

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
                stream_key: s.flow_keys.get(k).cloned(),
            })
        })
        .collect();
    flows.sort_by(|a, b| b.recent_bytes.cmp(&a.recent_bytes));
    let wan_upload_kbps_history: Vec<u64> = s.wan_upload_history.iter().map(|(_, kbps)| *kbps).collect();
    let wan_download_kbps_history: Vec<u64> = s.wan_download_history.iter().map(|(_, kbps)| *kbps).collect();
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
        unifi_status: &s.unifi_status,
        unifi_clients: &s.unifi_clients,
        last_unifi_poll_at: s.last_unifi_poll_at,
        unifi_thread_alive: s.unifi_thread_alive,
        wan_status: &s.wan_status,
        wan_snapshot: s.wan_snapshot.as_ref(),
        last_wan_poll_at: s.last_wan_poll_at,
        wan_thread_alive: s.wan_thread_alive,
        wan_upload_kbps_history,
        wan_download_kbps_history,
    };
    serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into())
}

fn probe_loop(state: Arc<Mutex<DashboardState>>) {
    state.lock().unwrap().probe_thread_alive = true;
    loop {
        // Snapshot the config under the lock so we don't hold it
        // through the FFmpeg shell-out (multi-second blocking call).
        let (url, keys, interval, mode) = {
            let s = state.lock().unwrap();
            (
                s.config.url.clone(),
                s.config.keys.clone(),
                Duration::from_secs(s.config.interval_secs.max(1)),
                s.config.mode,
            )
        };
        // Active probes only fire in Standby mode. The default Live
        // mode is pure passive monitoring (UDM polling + tshark) so
        // the tool can run alongside an in-progress production
        // without sending any handshakes that could be REJECTED by
        // the receiver or contend with the production for the slot.
        if mode != DiagMode::Standby || url.is_empty() {
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
/// to the shared state. All fields are optional — missing fields
/// preserve the existing value. Per-key Stats / probe history are
/// reset when the probe target (url or keys) changes so the
/// dashboard's success-rate / latency numbers reflect only the new
/// configuration. Tweaking interval, mode, atem, or unifi_host
/// preserves probe history.
fn apply_config_update(state: &Arc<Mutex<DashboardState>>, body: &str) -> Result<(), String> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("invalid JSON: {e}"))?;

    let url_field = parsed
        .get("url")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string());
    if let Some(u) = &url_field {
        if !u.is_empty()
            && !(u.starts_with("srt://")
                || u.starts_with("rtmp://")
                || u.starts_with("rtmps://"))
        {
            return Err(format!(
                "URL must start with srt://, rtmp://, or rtmps:// (got {u:?})"
            ));
        }
    }

    let keys_field: Option<Vec<String>> = parsed
        .get("keys")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        });

    let interval_field = parsed
        .get("interval_secs")
        .and_then(|v| v.as_u64())
        .filter(|&i| i > 0);

    let mode_field = parsed
        .get("mode")
        .and_then(|v| v.as_str())
        .and_then(DiagMode::from_str);

    let atem_field = parsed.get("atem").map(|v| AtemTarget {
        ip: v
            .get("ip")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        mac: v
            .get("mac")
            .and_then(|x| x.as_str())
            .map(|s| normalize_mac(s))
            .filter(|s| !s.is_empty()),
        port: v
            .get("port")
            .and_then(|x| x.as_u64())
            .and_then(|p| u16::try_from(p).ok())
            .unwrap_or(DEFAULT_ATEM_PORT),
    });

    let unifi_host_field = parsed
        .get("unifi_host")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string());

    let wan_up_cap_field = parsed
        .get("wan_upload_cap_mbps")
        .and_then(|v| v.as_f64())
        .filter(|f| *f >= 0.0);
    let wan_down_cap_field = parsed
        .get("wan_download_cap_mbps")
        .and_then(|v| v.as_f64())
        .filter(|f| *f >= 0.0);

    // Source labels are sent as either:
    //   - {"source_labels": {"1.2.3.4": "Jamie"}}  (full replacement), or
    //   - {"source_label_set": {"ip": "1.2.3.4", "label": "Jamie"}}
    //     to set/remove a single entry (label="" deletes).
    let labels_replace = parsed
        .get("source_labels")
        .and_then(|v| v.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect::<HashMap<String, String>>()
        });
    let label_set: Option<(String, String)> = parsed.get("source_label_set").and_then(|v| {
        let ip = v.get("ip").and_then(|x| x.as_str())?.trim().to_string();
        let label = v.get("label").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if ip.is_empty() {
            None
        } else {
            Some((ip, label))
        }
    });

    let mut s = state.lock().unwrap();
    let mut probe_target_changed = false;
    if let Some(u) = url_field {
        if s.config.url != u {
            probe_target_changed = true;
        }
        s.config.url = u;
    }
    if let Some(k) = keys_field {
        if s.config.keys != k {
            probe_target_changed = true;
        }
        s.config.keys = k;
    }
    if let Some(i) = interval_field {
        s.config.interval_secs = i;
    }
    if let Some(m) = mode_field {
        s.config.mode = m;
    }
    if let Some(a) = atem_field {
        s.config.atem = a;
    }
    if let Some(h) = unifi_host_field {
        s.config.unifi_host = h;
    }
    if let Some(c) = wan_up_cap_field {
        s.config.wan_upload_cap_mbps = c;
    }
    if let Some(c) = wan_down_cap_field {
        s.config.wan_download_cap_mbps = c;
    }
    if let Some(map) = labels_replace {
        s.config.source_labels = map;
    }
    if let Some((ip, label)) = label_set {
        if label.is_empty() {
            s.config.source_labels.remove(&ip);
        } else {
            s.config.source_labels.insert(ip, label);
        }
    }
    if probe_target_changed {
        // Reset per-key stats + probe history when the target /
        // keys change. Otherwise the dashboard mixes data from
        // different probe targets into the same cards.
        s.per_key.clear();
        s.probes.clear();
        s.last_probe_at = 0;
    }
    Ok(())
}

/// Normalize a MAC address to lowercase colon-separated format —
/// "7C-2E-0D-21-AB-FE" / "7C2E0D21ABFE" / "7c:2e:0d:21:ab:fe" all
/// become "7c:2e:0d:21:ab:fe". UniFi keys clients in lowercase
/// colons; matching against any other capitalization or separator
/// silently fails.
fn normalize_mac(input: &str) -> String {
    let hex: String = input
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if hex.len() != 12 {
        return input.trim().to_ascii_lowercase();
    }
    let mut out = String::with_capacity(17);
    for (i, c) in hex.chars().enumerate() {
        if i > 0 && i % 2 == 0 {
            out.push(':');
        }
        out.push(c);
    }
    out
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
            let flow_key = packet.flow_key();
            let stats = s.flows.entry(flow_key.clone()).or_default();
            stats.record(packet.bytes);
            if let Some(ack) = packet.srt {
                stats.record_srt_ack(ack);
            }
            // Stream-key extraction from the SRT HSv5 conclusion
            // handshake. Happens once per connection (handshake is
            // a single packet at session setup), so we cache in
            // flow_keys keyed by the flow tuple. For BMD-flavored
            // streamids we strip down to the operator-visible
            // `u=` field; for other senders we keep the full SID.
            if let Some(streamid) = packet.stream_id {
                let display_key = extract_bmd_key(&streamid).unwrap_or(streamid);
                s.flow_keys.insert(flow_key, display_key);
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
