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

use std::collections::{HashMap, HashSet, VecDeque};
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

/// Window the auto-detect waits before flagging "you appear blind."
/// Short enough that the wizard surfaces during a normal session
/// (operators routinely sit for >30s with the dashboard open while
/// pre-show checks complete), long enough to ride out a quiet patch
/// where ATEM traffic happens to pause briefly.
const LAN_VISIBILITY_WINDOW_SECS: u64 = 30;

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

/// Mirror-mode auto-detect state. Modern Ethernet switches don't
/// broadcast unicast; a peer Mac plugged into the same switch as the
/// ATEM can't see the streamer's traffic to the ATEM unless port
/// mirroring (SPAN) is configured. This enum tracks whether we
/// appear to be on a mirrored / streamer-side port (`SeesPeers`) or
/// stuck on a normal port that only sees our own traffic
/// (`PossiblyBlind`). The dashboard surfaces a wizard with UDM SPAN
/// setup steps when `PossiblyBlind` is detected.
///
/// Only relevant when capture is enabled (tshark running). UDM
/// polling is unaffected — that's the recommended primary data
/// source precisely because it's topology-independent.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum LanVisibility {
    /// Either capture isn't running yet, or it's been running less
    /// than 30s — too early to call.
    Unknown,
    /// We've observed at least one flow with a src_ip that isn't ours.
    /// Either we're on the streamer's machine (we see its outbound)
    /// or a SPAN port is mirroring traffic to us. Either way, the
    /// capture data source is healthy; no wizard needed.
    SeesPeers,
    /// Capture has been running >30s, we've seen our own outbound
    /// (so capture itself is working) but ZERO peer flows. Almost
    /// certainly a switched-LAN visibility problem — show the wizard.
    PossiblyBlind { since: u64 },
}

impl Default for LanVisibility {
    fn default() -> Self {
        LanVisibility::Unknown
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
    pub gw_cpu_pct: Option<f64>,
    pub gw_mem_pct: Option<f64>,
    pub gw_uptime_secs: Option<u64>,
}

/// Status of the slower-cadence "system" poll (switches, alarms).
/// Separate from UnifiStatus so a slow / failing system endpoint
/// doesn't make the client poll look broken.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SystemStatus {
    NotConfigured,
    Connecting,
    Connected { last_poll_at: u64 },
    Failed { error: String, last_attempt: u64 },
}

impl Default for SystemStatus {
    fn default() -> Self { SystemStatus::NotConfigured }
}

/// Snapshot of one UniFi switch (USW). Surfaces per-port real-time
/// utilization, errors, and link state — operator can spot a
/// saturated port, errors creeping up on the ATEM port, or an SFP
/// running hot before it fails. Only USW devices populate; UAPs
/// and the UDM itself are excluded since their "ports" are wireless
/// or routed and have different surface.
#[derive(Clone, Debug, Serialize)]
pub struct SwitchSnapshot {
    pub mac: String,
    pub name: String,
    pub model: String,
    pub cpu_pct: Option<f64>,
    pub mem_pct: Option<f64>,
    pub uptime_secs: Option<u64>,
    pub ports: Vec<SwitchPortSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SwitchPortSnapshot {
    pub port_idx: u32,
    pub name: String,
    pub media: Option<String>,
    pub speed_mbps: u32,
    pub up: bool,
    pub tx_kbps: u64,
    pub rx_kbps: u64,
    pub tx_errors: u64,
    pub rx_errors: u64,
    pub tx_dropped: u64,
    pub rx_dropped: u64,
    pub link_down_count: u64,
    pub connected_mac: Option<String>,
    pub connected_ip: Option<String>,
    pub is_atem_port: bool,
    pub sfp_temp_c: Option<f64>,
    pub sfp_tx_dbm: Option<f64>,
    pub sfp_rx_dbm: Option<f64>,
}

/// Active alarm reported by the UDM. Empty list = healthy. Surfaces
/// security warnings, link-down events, AP-adoption issues, etc.
#[derive(Clone, Debug, Serialize)]
pub struct AlarmSnapshot {
    pub key: String,
    pub msg: String,
    pub subsystem: String,
    pub time: u64,
    pub severity: Option<String>,
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

    /// Switch + alarm data from the slower-cadence system poll.
    pub switches: Vec<SwitchSnapshot>,
    pub alarms: Vec<AlarmSnapshot>,
    pub system_status: SystemStatus,
    pub last_system_poll_at: u64,
    pub system_thread_alive: bool,

    /// Reverse-DNS cache + work queue. The poller adds source IPs
    /// (from observed flows) to `rdns_pending`; a background worker
    /// resolves them and writes the result here. Cache value is
    /// `Some(hostname)` on success, `None` after a failed lookup
    /// (so we don't retry forever for IPs without PTR records).
    pub rdns_cache: HashMap<String, Option<String>>,
    pub rdns_pending: HashSet<String>,

    /// PID of the currently-running tshark child process. Used by
    /// `apply_config_update` to send SIGTERM when the operator
    /// changes `monitor_iface` — the monitor_loop's outer respawn
    /// wrapper then re-reads the new iface from config and starts
    /// fresh. None when the capture thread isn't running (no
    /// tshark, missing dep, etc.).
    pub monitor_pid: Option<u32>,

    /// This machine's own IPv4 addresses, gathered once at startup
    /// via `local_ip_address::list_afinet_netifas`. Used by the
    /// monitor loop to classify each captured flow as "ours" (src_ip
    /// matches one of these) or "peer" (src_ip doesn't, indicating
    /// we're on a SPAN port or the streamer's machine). The set is
    /// frozen at startup; if the user changes Wi-Fi networks mid-
    /// session we don't auto-refresh — they'd need to relaunch.
    pub local_ips: HashSet<String>,
    /// Auto-detect state for mirror-mode/SPAN visibility — see
    /// `LanVisibility`. Updated by the monitor loop on each captured
    /// packet (cheap atomic-ish updates) plus periodic 5s checks.
    pub lan_visibility: LanVisibility,
    /// Counts of own-host vs peer flows observed since capture
    /// started. The 30s "are we blind?" check uses these to decide
    /// PossiblyBlind: if `peer_flow_count == 0` AND
    /// `own_flow_count > 0` AND >30s elapsed since capture started,
    /// we conclude visibility is restricted.
    pub own_flow_count: u64,
    pub peer_flow_count: u64,
    /// Wall-clock when the monitor's outer loop first started this
    /// session — drives the 30s minimum window before flipping to
    /// PossiblyBlind. None until the first tshark child spawns.
    pub monitor_started_at: Option<u64>,
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
    pub switches: &'a [SwitchSnapshot],
    pub alarms: &'a [AlarmSnapshot],
    pub system_status: &'a SystemStatus,
    pub last_system_poll_at: u64,
    pub system_thread_alive: bool,
    pub rdns_cache: HashMap<String, Option<String>>,
    /// Mirror-mode auto-detect — surfaces "you appear blind to ATEM
    /// traffic, set up port mirroring" wizard when capture's been
    /// running >30s and only own-host flows have been seen.
    pub lan_visibility: LanVisibility,
    /// This machine's own IPs. Sent to the UI so the wizard can
    /// pre-fill Step 1 ("Identify the port the peer Mac is on").
    pub local_ips: Vec<String>,
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

    // Snapshot this machine's own IPv4 addresses once at startup.
    // The monitor loop uses these to classify each captured flow's
    // src_ip as "ours" (own outbound) vs "peer" (we have visibility
    // into other devices' traffic — either we're on the streamer's
    // machine OR a SPAN/mirror port is delivering traffic to us).
    // Skipping IPv6 for now: the auto-detect is concerned with v4
    // SRT/RTMP flows on the LAN, and tshark's flow_key.src_ip is
    // emitted as a v4 dotted-quad in our parser.
    let local_ips: HashSet<String> = local_ip_address::list_afinet_netifas()
        .map(|nics| {
            nics.into_iter()
                .filter_map(|(_name, ip)| match ip {
                    std::net::IpAddr::V4(v4) => Some(v4.to_string()),
                    std::net::IpAddr::V6(_) => None,
                })
                .collect()
        })
        .unwrap_or_default();
    eprintln!(
        "[dashboard] local IPs detected for visibility classification: {:?}",
        local_ips
    );

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
        local_ips,
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
        let wan_creds = creds.clone();
        std::thread::Builder::new()
            .name("wan-poll".into())
            .spawn(move || unifi::wan_poll_loop(wan_host, wan_creds, wan_state))
            .expect("spawn wan poll");

        // System poll: switches + alarms (slower cadence: 5s).
        let sys_state = state.clone();
        let sys_host = state.lock().unwrap().config.unifi_host.clone();
        std::thread::Builder::new()
            .name("system-poll".into())
            .spawn(move || unifi::system_poll_loop(sys_host, creds, sys_state))
            .expect("spawn system poll");

        // Reverse-DNS resolver thread: pulls source IPs out of the
        // pending queue, runs `host` to look them up, caches results.
        let rdns_state = state.clone();
        std::thread::Builder::new()
            .name("rdns-worker".into())
            .spawn(move || unifi::rdns_worker_loop(rdns_state))
            .expect("spawn rdns worker");
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
    let rdns_cache = s.rdns_cache.clone();
    let lan_visibility = compute_lan_visibility(&s, now);
    // Sort local IPs deterministically so the UI doesn't churn on
    // every poll. Step 1 of the wizard pre-fills the first one as a
    // hint, so stable ordering matters for keyboard-focus UX too.
    let mut local_ips: Vec<String> = s.local_ips.iter().cloned().collect();
    local_ips.sort();
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
        switches: &s.switches,
        alarms: &s.alarms,
        system_status: &s.system_status,
        last_system_poll_at: s.last_system_poll_at,
        system_thread_alive: s.system_thread_alive,
        rdns_cache,
        lan_visibility,
        local_ips,
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

    let monitor_iface_field = parsed
        .get("monitor_iface")
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
    if let Some(new_iface) = monitor_iface_field {
        if !new_iface.is_empty() && Some(&new_iface) != s.config.monitor_iface.as_ref() {
            s.config.monitor_iface = Some(new_iface);
            // Kick the running tshark child so the monitor_loop's
            // outer wrapper respawns on the new interface. Use
            // `kill -TERM` via Command (no extra deps); the loop's
            // wait() reaps the child after EOF.
            if let Some(pid) = s.monitor_pid.take() {
                let _ = std::process::Command::new("kill")
                    .arg("-TERM")
                    .arg(pid.to_string())
                    .status();
            }
        }
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

fn monitor_loop(default_iface: String, ports: Vec<u16>, state: Arc<Mutex<DashboardState>>) {
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
    state.lock().unwrap().monitor_thread_alive = true;

    // Outer loop: each iteration spawns a fresh tshark on whatever
    // interface is currently configured. When the iface is changed
    // via /api/config, apply_config_update sends SIGTERM to the
    // running child; tshark exits, the inner read loop hits EOF,
    // and we land back here to spawn anew on the updated interface.
    loop {
        let iface = state
            .lock()
            .unwrap()
            .config
            .monitor_iface
            .clone()
            .unwrap_or_else(|| default_iface.clone());
        let mut child = match Command::new(&tshark)
            .args(tshark_field_args(&iface, &filter))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(err) => {
                eprintln!("[monitor] failed to spawn tshark on {iface}: {err}");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        eprintln!("[monitor] tshark spawned on {iface} (pid {})", child.id());
        let spawn_time = Instant::now();
        {
            // Reset visibility-classification counters on each fresh
            // tshark spawn. Operator changing iface via /api/config
            // should get a clean 30s window to evaluate the new
            // capture surface, not be stuck with the previous iface's
            // tally. The outer loop respawns tshark on any iface
            // change, so this hits the right cadence automatically.
            let mut s = state.lock().unwrap();
            s.monitor_pid = Some(child.id());
            s.monitor_started_at = Some(now_secs());
            s.own_flow_count = 0;
            s.peer_flow_count = 0;
            s.lan_visibility = LanVisibility::Unknown;
        }
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => continue,
        };
        let stderr = child.stderr.take();
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(packet) = parse_tshark_line(&line) {
            let mut s = state.lock().unwrap();
            let flow_key = packet.flow_key();
            // Enqueue the flow's source IP for reverse DNS resolution
            // if we haven't seen it before. Skip RFC1918 / loopback
            // since reverse DNS for those rarely produces useful
            // names and we don't want to spam the resolver.
            let src = flow_key.src_ip.clone();
            if !is_private_or_loopback(&src)
                && !s.rdns_cache.contains_key(&src)
                && !s.rdns_pending.contains(&src)
            {
                s.rdns_pending.insert(src);
            }
            // alpha.11 simplified the auto-detect: we no longer
            // pre-classify flows into own_flow_count vs peer_flow_count.
            // The visibility check (compute_lan_visibility) now scans
            // s.flows directly for the configured ATEM IP — which is
            // a more reliable signal than local-vs-peer (the streamer's
            // own outbound to the ATEM was being counted as "own" and
            // tripping the wizard incorrectly). Counters preserved as
            // dead state for one release in case downstream logging
            // surfaces them; remove in alpha.12 if nothing reads them.
            let _ = &flow_key; // silence warnings — flow_key still used below
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
        // tshark stdout EOF — process exited (either we asked
        // it to, via SIGTERM from apply_config_update on an
        // iface change, or it crashed). Reap the child, clear
        // the PID, and let the outer loop respawn.
        let _ = child.wait();
        state.lock().unwrap().monitor_pid = None;

        // If tshark exited within a couple of seconds it almost
        // certainly hit a permission error or an interface that
        // doesn't support capture (BPF denied, virtual NIC, bad
        // iface name). Drain stderr to surface the actual error
        // and back off longer so we don't peg CPU respawning.
        let exited_fast = spawn_time.elapsed() < Duration::from_secs(2);
        if exited_fast {
            if let Some(mut e) = stderr {
                let mut buf = String::new();
                use std::io::Read as _;
                let _ = e.read_to_string(&mut buf);
                let trimmed = buf.trim();
                if !trimmed.is_empty() {
                    eprintln!("[monitor] tshark on {iface} exited quickly: {trimmed}");
                }
            }
            std::thread::sleep(Duration::from_secs(5));
        } else {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Decide the LAN visibility state from the captured flows. Called
/// inline by `build_state_json` so the answer is always fresh.
///
/// alpha.11 rewrote this heuristic. The original alpha.9 version
/// used local-IP-vs-peer-IP classification (own_flow_count vs
/// peer_flow_count), which was wrong in two directions:
///
/// - On the **streamer's own Mac**, all ATEM traffic has one local
///   end (us → ATEM, ATEM → us) so everything got counted as
///   "own_flow_count" and the wizard surfaced as a false positive
///   ("you appear blind") even though the streamer has direct
///   visibility.
/// - On a **peer Mac without SPAN**, the tshark filter
///   (`port 1935 or 9710 or ...`) sees zero traffic — neither
///   counter increments — so the heuristic stayed Unknown and the
///   wizard never fired in the case it was designed to catch.
///
/// New heuristic: visibility means "we can see ATEM-related flows
/// regardless of who's the source." Direct test against the
/// configured ATEM IP, no local-vs-peer ambiguity.
fn compute_lan_visibility(s: &DashboardState, now: u64) -> LanVisibility {
    let Some(started_at) = s.monitor_started_at else {
        return LanVisibility::Unknown;
    };

    let atem_ip = s.config.atem.ip.trim();
    if atem_ip.is_empty() {
        // No ATEM IP configured yet — operator hasn't set the target.
        // Don't surface the wizard; they have to configure first.
        return LanVisibility::Unknown;
    }

    // Have we captured any flow involving the ATEM? Either direction
    // counts (streamer→ATEM AND ATEM→streamer-ack are both ATEM
    // visibility). When this fires, capture is healthy — hide the
    // wizard regardless of whether we're on the streamer's machine
    // or a peer Mac on a SPAN port.
    let saw_atem_flow = s.flows.keys().any(|k| k.src_ip == atem_ip || k.dst_ip == atem_ip);
    if saw_atem_flow {
        return LanVisibility::SeesPeers;
    }

    if now.saturating_sub(started_at) < LAN_VISIBILITY_WINDOW_SECS {
        // Too early to call. ATEM might be quiet, capture might be
        // ramping up — give it the dwell window before alarming.
        return LanVisibility::Unknown;
    }

    // >30s elapsed and we haven't seen the ATEM in any captured
    // flow. Surface the wizard. This correctly fires in BOTH the
    // peer-Mac-no-SPAN case (s.flows is empty) AND the peer-Mac-on-
    // wrong-iface case (s.flows has unrelated hosts but no ATEM).
    LanVisibility::PossiblyBlind {
        since: started_at + LAN_VISIBILITY_WINDOW_SECS,
    }
}

/// Heuristic: skip reverse-DNS lookups for IPs that almost never have
/// useful PTR records (RFC1918 LAN ranges, loopback, link-local). The
/// public-internet sources we DO want to resolve get queued normally.
pub fn is_private_or_loopback(ip: &str) -> bool {
    let octets: Vec<u8> = ip.split('.').filter_map(|o| o.parse().ok()).collect();
    if octets.len() != 4 { return true; } // IPv6 or malformed — skip for now
    let (a, b) = (octets[0], octets[1]);
    a == 10
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || a == 127
        || a == 0
        || (a == 169 && b == 254)
        || a >= 224 // multicast / reserved
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
