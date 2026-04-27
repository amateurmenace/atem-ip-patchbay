//! UDM (UniFi OS / Network Application) HTTP client. Polls per-
//! client bandwidth stats from the local controller so the
//! dashboard can show "what's flowing to the ATEM right now"
//! without active probing — pure passive data collection that
//! doesn't touch the production stream.
//!
//! Auth surfaces:
//! - **Local Controller API key** (UniFi OS 9.0+): created via
//!   the Network app's Settings → Control Plane → Integrations.
//!   Sent as `X-API-KEY` header. Stateless, no session refresh.
//! - **Local-account cookie auth**: POST /api/auth/login with
//!   username + password, capture TOKEN cookie + CSRF token,
//!   include on subsequent requests. For older UniFi OS that
//!   doesn't yet support local API keys.
//!
//! Endpoint preference: legacy `/proxy/network/api/s/default/
//! stat/sta` first, since it returns per-client tx_bytes /
//! rx_bytes counters that we delta-derive into kbps. The newer
//! integration API at `/proxy/network/integration/v1/sites/
//! {siteId}/clients` is read-mostly and the bandwidth fields are
//! still rolling out as of early 2026 — we fall back to it only
//! if the legacy path returns 401 (i.e. the API key only works
//! against the integration surface).
//!
//! Self-signed certs: the UDM ships with a self-signed cert by
//! default. We accept any cert when talking to the configured
//! UDM host. Trade-off: a MITM on the LAN could intercept the
//! API key. Acceptable for a diagnostic tool on a trusted LAN;
//! NOT acceptable for credential storage or remote access.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use ureq::Agent;

use crate::dashboard::{DashboardState, UnifiClientSnapshot, UnifiStatus};

/// How often to poll. Faster = tighter bandwidth resolution but
/// more load on the UDM. 2s is a good live-monitoring balance —
/// matches the operator's perception of "real time" without
/// hammering the controller's CPU.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// HTTP request timeout. UDM API endpoints typically respond in
/// 100-500ms even for /stat/sta on a busy network. 5s catches
/// a hung controller without long blocking.
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// Backoff after a poll error before retrying. Short enough that
/// transient network blips don't make the dashboard look dead,
/// long enough that a UDM reboot doesn't get hammered.
const ERROR_BACKOFF: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub enum UnifiCredentials {
    ApiKey(String),
    Login { username: String, password: String },
}

/// Read UDM credentials from environment variables. Preferred:
/// `UDM_API_KEY` (Local Controller API key from UniFi OS 9.0+).
/// Fallback: `UDM_USERNAME` + `UDM_PASSWORD` (cookie-auth login
/// flow for older UniFi OS or read-only local accounts). Returns
/// None when neither is set — the polling thread won't start in
/// that case and the dashboard shows "UDM: not configured".
pub fn read_credentials_from_env() -> Option<UnifiCredentials> {
    if let Ok(key) = std::env::var("UDM_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Some(UnifiCredentials::ApiKey(key));
        }
    }
    let user = std::env::var("UDM_USERNAME").ok()?.trim().to_string();
    let pass = std::env::var("UDM_PASSWORD").ok()?;
    if user.is_empty() || pass.is_empty() {
        return None;
    }
    Some(UnifiCredentials::Login {
        username: user,
        password: pass,
    })
}

struct UnifiClient {
    host: String,
    agent: Agent,
    creds: UnifiCredentials,
    site: String,
    /// CSRF token captured from cookie-auth login. Required on
    /// modifying requests; GETs typically work without it but we
    /// send it anyway when present for forward compat.
    csrf: Option<String>,
}

impl UnifiClient {
    fn new(host: String, creds: UnifiCredentials) -> Result<Self, String> {
        let connector = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .map_err(|e| format!("tls connector init failed: {e}"))?;
        let agent = ureq::AgentBuilder::new()
            .timeout(HTTP_TIMEOUT)
            .tls_connector(Arc::new(connector))
            .build();
        Ok(Self {
            host: host.trim_end_matches('/').to_string(),
            agent,
            creds,
            site: "default".to_string(),
            csrf: None,
        })
    }

    /// Cookie-auth login. No-op for ApiKey auth — that path uses
    /// the X-API-KEY header on each request directly.
    fn login(&mut self) -> Result<(), String> {
        let creds = self.creds.clone();
        match &creds {
            UnifiCredentials::ApiKey(_) => Ok(()),
            UnifiCredentials::Login { username, password } => {
                let url = format!("{}/api/auth/login", self.host);
                let body = serde_json::json!({
                    "username": username,
                    "password": password,
                    "remember": true,
                });
                let resp = self
                    .agent
                    .post(&url)
                    .set("Content-Type", "application/json")
                    .send_string(&body.to_string())
                    .map_err(stringify_ureq_err)?;
                // The CSRF token comes back in one of two headers
                // depending on UniFi OS version. Capture either.
                if let Some(csrf) = resp.header("X-Updated-CSRF-Token-After-Login") {
                    self.csrf = Some(csrf.to_string());
                } else if let Some(csrf) = resp.header("X-CSRF-Token") {
                    self.csrf = Some(csrf.to_string());
                }
                Ok(())
            }
        }
    }

    fn apply_auth_headers(&self, mut req: ureq::Request) -> ureq::Request {
        match &self.creds {
            UnifiCredentials::ApiKey(key) => {
                req = req.set("X-API-KEY", key);
                req = req.set("Accept", "application/json");
            }
            UnifiCredentials::Login { .. } => {
                if let Some(c) = &self.csrf {
                    req = req.set("X-CSRF-Token", c);
                }
            }
        }
        req
    }

    fn get(&self, path: &str) -> Result<ureq::Response, String> {
        let url = format!("{}{}", self.host, path);
        let req = self.apply_auth_headers(self.agent.get(&url));
        req.call().map_err(stringify_ureq_err)
    }

    /// Returns one entry per client the controller has data for
    /// (currently active + recently disconnected). Caller filters
    /// by last_seen for "currently online".
    fn list_clients(&self) -> Result<Vec<RawClient>, String> {
        // Try the legacy stat/sta endpoint first — it returns the
        // bandwidth counters we delta-derive. If the API key
        // doesn't grant access to the legacy surface (some older
        // UniFi OS releases gate it differently), fall back to
        // the newer integration API.
        let legacy_path = format!("/proxy/network/api/s/{}/stat/sta", self.site);
        match self.get(&legacy_path) {
            Ok(resp) => {
                let parsed: LegacyStatResponse = resp
                    .into_json()
                    .map_err(|e| format!("legacy stat/sta JSON parse: {e}"))?;
                Ok(parsed.data)
            }
            Err(legacy_err) => {
                // Some newer UDMs gate the legacy path; try the
                // integration API as fallback. Note: the integration
                // API doesn't expose tx_bytes/rx_bytes counters as
                // of early 2026, so bandwidth will read 0 in this
                // path. We still surface the client list so the
                // operator can see the ATEM and the source clients.
                let integ_path = format!(
                    "/proxy/network/integration/v1/sites/{}/clients",
                    self.site
                );
                match self.get(&integ_path) {
                    Ok(resp) => {
                        let parsed: IntegrationClientsResponse = resp
                            .into_json()
                            .map_err(|e| format!("integration clients JSON parse: {e}"))?;
                        Ok(parsed.data.into_iter().map(RawClient::from_integration).collect())
                    }
                    Err(integ_err) => Err(format!(
                        "legacy stat/sta failed: {legacy_err}; integration API also failed: {integ_err}"
                    )),
                }
            }
        }
    }
}

/// Convert ureq's typed errors into a flat string suitable for
/// surfacing in UnifiStatus::Failed { error }. ureq's error type
/// distinguishes Status (got an HTTP response with bad status)
/// from Transport (connection / TLS / timeout failure); we
/// preserve enough detail to diagnose without exposing the
/// internal type.
fn stringify_ureq_err(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let snippet = body.chars().take(200).collect::<String>();
            format!("HTTP {code}: {snippet}")
        }
        ureq::Error::Transport(t) => format!("transport: {t}"),
    }
}

#[derive(Deserialize)]
struct LegacyStatResponse {
    data: Vec<RawClient>,
}

#[derive(Deserialize)]
struct IntegrationClientsResponse {
    data: Vec<IntegrationClient>,
}

/// Fields we use from the integration API response. The integration
/// API uses camelCase. Fewer fields are exposed here than via the
/// legacy stat/sta — most notably no per-client byte counters as
/// of early 2026, so tx/rx kbps will read 0 when this fallback
/// fires.
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct IntegrationClient {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    mac_address: Option<String>,
    #[serde(default)]
    ip_address: Option<String>,
    #[serde(default)]
    uplink_device_id: Option<String>,
    #[serde(default)]
    last_seen: Option<String>, // ISO-8601 timestamp
    #[serde(default)]
    connected_at: Option<String>,
    #[serde(default)]
    r#type: Option<String>, // "WIRED" | "WIRELESS"
}

/// Normalized client record fed into the snapshot builder. Either
/// source path (legacy / integration) maps into this shape.
///
/// Byte counters: the legacy stat/sta endpoint reports two
/// disjoint pairs depending on how the client connects. Wireless
/// clients populate `tx_bytes`/`rx_bytes`; clients attached via a
/// USW switch populate `wired-tx_bytes`/`wired-rx_bytes` and leave
/// the plain pair zero. We deserialize both and let
/// [`Self::effective_tx_bytes`] / [`Self::effective_rx_bytes`] sum
/// them — safe because the two pairs are mutually exclusive in the
/// observed data (a client is either wired-via-switch or wireless,
/// never both at once for the same poll).
#[derive(Deserialize, Debug, Clone, Default)]
struct RawClient {
    #[serde(default)]
    mac: String,
    #[serde(default)]
    ip: Option<String>,
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    oui: Option<String>,
    #[serde(default)]
    last_seen: Option<u64>,
    #[serde(default)]
    tx_bytes: u64,
    #[serde(default)]
    rx_bytes: u64,
    #[serde(default, rename = "wired-tx_bytes")]
    wired_tx_bytes: u64,
    #[serde(default, rename = "wired-rx_bytes")]
    wired_rx_bytes: u64,
    /// UDM-reported rate fields (bytes/sec). The controller smooths
    /// these over its internal sampling window (~5-10s), which is
    /// LONGER than our 2s poll interval — so deriving kbps from
    /// our own (current_bytes - prev_bytes) / dt produces wildly
    /// jumpy numbers (most polls see no counter movement, then one
    /// poll sees ~10s of accumulated bytes). When these rate
    /// fields are populated we use them directly for stable kbps
    /// and only fall back to delta math when the UDM doesn't
    /// surface a rate (e.g., the integration API path).
    #[serde(default, rename = "tx_bytes-r")]
    tx_bytes_r: f64,
    #[serde(default, rename = "rx_bytes-r")]
    rx_bytes_r: f64,
    #[serde(default, rename = "wired-tx_bytes-r")]
    wired_tx_bytes_r: f64,
    #[serde(default, rename = "wired-rx_bytes-r")]
    wired_rx_bytes_r: f64,
    #[serde(default)]
    is_wired: bool,
}

impl RawClient {
    /// Bytes leaving this client (toward the switch/AP). UniFi's
    /// stat/sta reports counters from the **infrastructure's**
    /// perspective: a client's TX is what the AP/switch port RX'd
    /// from it. We invert here so the dashboard's `tx_kbps` /
    /// `rx_kbps` reflect device-perspective — which matches the
    /// documented intent ("show what's flowing to the ATEM").
    fn effective_tx_bytes(&self) -> u64 {
        self.rx_bytes.saturating_add(self.wired_rx_bytes)
    }
    fn effective_rx_bytes(&self) -> u64 {
        self.tx_bytes.saturating_add(self.wired_tx_bytes)
    }

    /// Device-perspective TX/RX rates in kbps, sourced from the
    /// UDM's smoothed rate fields. Returns `None` if the UDM
    /// didn't include any rate field (caller falls back to delta-
    /// derived rates from the byte counters). Same perspective
    /// inversion as the byte counters.
    fn udm_rate_kbps(&self) -> Option<(u64, u64)> {
        let any_present = self.tx_bytes_r > 0.0
            || self.rx_bytes_r > 0.0
            || self.wired_tx_bytes_r > 0.0
            || self.wired_rx_bytes_r > 0.0;
        if !any_present {
            return None;
        }
        let device_tx_bps = self.rx_bytes_r + self.wired_rx_bytes_r;
        let device_rx_bps = self.tx_bytes_r + self.wired_tx_bytes_r;
        Some((
            (device_tx_bps * 8.0 / 1000.0) as u64,
            (device_rx_bps * 8.0 / 1000.0) as u64,
        ))
    }
}

impl RawClient {
    fn from_integration(c: IntegrationClient) -> Self {
        Self {
            mac: c.mac_address.unwrap_or_default(),
            ip: c.ip_address,
            hostname: None,
            name: c.name,
            oui: None,
            last_seen: parse_iso8601_to_unix(c.last_seen.as_deref()),
            tx_bytes: 0,
            rx_bytes: 0,
            wired_tx_bytes: 0,
            wired_rx_bytes: 0,
            tx_bytes_r: 0.0,
            rx_bytes_r: 0.0,
            wired_tx_bytes_r: 0.0,
            wired_rx_bytes_r: 0.0,
            is_wired: c.r#type.as_deref().map(|t| t.eq_ignore_ascii_case("WIRED")).unwrap_or(false),
        }
    }
}

/// Best-effort ISO-8601 → Unix-seconds parser. Returns None on
/// any parse error so the caller can fall through to "unknown".
/// Avoids pulling in `chrono` for a one-line conversion.
fn parse_iso8601_to_unix(s: Option<&str>) -> Option<u64> {
    let s = s?;
    // Format examples: "2026-04-26T18:30:45.123Z" or with
    // "+00:00" offset. We accept both by stripping the fraction
    // and timezone suffix and parsing the YYYY-MM-DDTHH:MM:SS
    // prefix as UTC. Good enough for "show last_seen idle time".
    let head = s.split('.').next().unwrap_or(s);
    let head = head.trim_end_matches('Z').trim_end_matches('+').trim_end_matches('-');
    let head = head.split('+').next().unwrap_or(head);
    let head = head.split_once('T')?;
    let date = head.0;
    let time = head.1;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    // Days from year-0001 → epoch 1970-01-01. Use the standard
    // civil-from-days algorithm (Howard Hinnant). Slightly more
    // code than necessary but avoids a chrono dep.
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as u64;
    let doy = (153 * (month + (if month > 2 { -3 } else { 9 })) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as u64;
    let days_from_epoch = era * 146097 + doe as i64 - 719468;
    let secs = days_from_epoch * 86400 + hour * 3600 + minute * 60 + second;
    if secs < 0 {
        None
    } else {
        Some(secs as u64)
    }
}

/// Polling loop. Reads creds at startup, polls forever, updates
/// DashboardState on each cycle.
pub fn poll_loop(host: String, creds: UnifiCredentials, state: Arc<Mutex<DashboardState>>) {
    let mut client = match UnifiClient::new(host, creds) {
        Ok(c) => c,
        Err(e) => {
            let mut s = state.lock().unwrap();
            s.unifi_status = UnifiStatus::Failed {
                error: format!("init: {e}"),
                last_attempt: now_secs(),
            };
            return;
        }
    };

    {
        let mut s = state.lock().unwrap();
        s.unifi_thread_alive = true;
        s.unifi_status = UnifiStatus::Connecting;
    }

    if let Err(e) = client.login() {
        // Login failure is fatal for cookie-auth; for ApiKey auth
        // login is a no-op, so this branch only fires for the
        // username/password path. Surface the error and bail —
        // the operator needs to fix the password before retrying.
        let mut s = state.lock().unwrap();
        s.unifi_status = UnifiStatus::Failed {
            error: format!("login: {e}"),
            last_attempt: now_secs(),
        };
        s.unifi_thread_alive = false;
        return;
    }

    // Per-MAC last-poll counters → delta → kbps. This map can
    // grow unbounded if clients churn fast (a noisy guest network
    // would feed thousands over hours), but in a typical
    // production network it stays under a few hundred entries.
    // Add LRU eviction later if it becomes a problem.
    let mut prev: HashMap<String, (u64, u64, Instant)> = HashMap::new();

    loop {
        let atem_mac_lower = state
            .lock()
            .unwrap()
            .config
            .atem
            .mac
            .as_deref()
            .map(|m| m.to_ascii_lowercase());
        match client.list_clients() {
            Ok(raw_clients) => {
                let now = Instant::now();
                let snapshots: Vec<UnifiClientSnapshot> = raw_clients
                    .into_iter()
                    .map(|r| {
                        let mac_lower = r.mac.to_ascii_lowercase();
                        let is_atem = atem_mac_lower
                            .as_deref()
                            .map(|m| m == mac_lower)
                            .unwrap_or(false);
                        let tx_total = r.effective_tx_bytes();
                        let rx_total = r.effective_rx_bytes();
                        let (tx_kbps, rx_kbps) = if let Some(rates) = r.udm_rate_kbps() {
                            rates
                        } else {
                            // Fallback path (integration API or other source
                            // without rate fields): derive from byte deltas.
                            // Subject to the same UDM-refresh-cadence jitter
                            // we'd see on the legacy path without rates, so
                            // these numbers may be jumpy.
                            match prev.get(&mac_lower) {
                                Some((prev_tx, prev_rx, prev_at)) => {
                                    let dt = now.duration_since(*prev_at).as_secs_f64();
                                    if dt > 0.1 {
                                        let tx_dbits = tx_total.saturating_sub(*prev_tx) as f64 * 8.0;
                                        let rx_dbits = rx_total.saturating_sub(*prev_rx) as f64 * 8.0;
                                        (
                                            (tx_dbits / dt / 1000.0) as u64,
                                            (rx_dbits / dt / 1000.0) as u64,
                                        )
                                    } else {
                                        (0, 0)
                                    }
                                }
                                None => (0, 0),
                            }
                        };
                        prev.insert(mac_lower.clone(), (tx_total, rx_total, now));
                        UnifiClientSnapshot {
                            mac: mac_lower,
                            ip: r.ip,
                            hostname: r.hostname,
                            name: r.name,
                            oui: r.oui,
                            last_seen: r.last_seen.unwrap_or(0),
                            tx_bytes: tx_total,
                            rx_bytes: rx_total,
                            tx_kbps,
                            rx_kbps,
                            is_atem,
                            is_wired: r.is_wired,
                        }
                    })
                    .collect();
                let now_unix = now_secs();
                let mut s = state.lock().unwrap();
                s.unifi_clients = snapshots;
                s.last_unifi_poll_at = now_unix;
                s.unifi_status = UnifiStatus::Connected {
                    last_poll_at: now_unix,
                };
                drop(s);
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => {
                let mut s = state.lock().unwrap();
                s.unifi_status = UnifiStatus::Failed {
                    error: e,
                    last_attempt: now_secs(),
                };
                drop(s);
                std::thread::sleep(ERROR_BACKOFF);
            }
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
