use anyhow::{anyhow, Result};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

use crate::state::EncoderState;
use crate::streamer::Streamer;

#[derive(Clone)]
pub struct HttpAppState {
    pub encoder: Arc<EncoderState>,
    pub streamer: Arc<Streamer>,
}

/// Bind a TCP listener on the requested port, walking forward up to nine
/// adjacent ports if the first is taken — same fallback behavior as
/// the v0.1.0 Python server (commit 607271d). Returns the bound port +
/// listener.
pub async fn bind_with_walk(start_port: u16) -> Result<(u16, TcpListener)> {
    let mut last_err: Option<std::io::Error> = None;
    for offset in 0..10 {
        let port = start_port + offset;
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        match TcpListener::bind(addr).await {
            Ok(listener) => return Ok((port, listener)),
            Err(err) => last_err = Some(err),
        }
    }
    Err(anyhow!(
        "could not bind HTTP API on TCP {start_port}-{}: {}",
        start_port + 9,
        last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "no error".into())
    ))
}

/// Build the Axum router that drives the existing JS UI. `static_dir`
/// is the on-disk path to `bmd_emulator/static/` (or its bundled
/// equivalent inside the .app's Resources/).
pub fn router(state: HttpAppState, static_dir: PathBuf) -> Router {
    // Static-file path mount — must come before nest_service('/') so the
    // /static/* prefix routes win over the index fallback.
    let static_service = ServeDir::new(&static_dir);
    let index_path = static_dir.join("index.html");
    let index_html = std::fs::read_to_string(&index_path).unwrap_or_else(|err| {
        log::warn!(
            "could not read index.html from {}: {} — serving placeholder",
            index_path.display(),
            err
        );
        "<!doctype html><title>ATEM IP Patchbay</title><h1>UI not found</h1>".to_string()
    });

    Router::new()
        .route("/", get(move || {
            let html = index_html.clone();
            async move { axum::response::Html(html) }
        }))
        .route("/api/state", get(api_state))
        .route("/api/lan-ip", get(api_lan_ip))
        .route("/api/log", get(api_log))
        .route("/api/devices", get(api_devices))
        .route("/api/discover", get(api_discover_stub))
        .route("/api/ndi-senders", get(api_ndi_senders_stub))
        .route("/api/start", post(api_start))
        .route("/api/stop", post(api_stop))
        .route("/api/settings", post(api_settings))
        .route("/api/load_xml", post(api_load_xml))
        .route("/api/load_xml_text", post(api_load_xml_text))
        .route("/api/destination/paste", post(api_destination_paste_stub))
        .nest_service("/static", static_service)
        .with_state(state)
}

// ---- handlers --------------------------------------------------------------

async fn api_state(State(state): State<HttpAppState>) -> impl IntoResponse {
    Json(state.encoder.snapshot())
}

#[derive(Serialize)]
struct LanIp {
    ip: String,
}

async fn api_lan_ip() -> impl IntoResponse {
    let ip = local_ip_address::local_ip()
        .map(|i| i.to_string())
        .unwrap_or_else(|_| guess_loopback());
    Json(LanIp { ip })
}

fn guess_loopback() -> String {
    "127.0.0.1".to_string()
}

#[derive(Serialize)]
struct LogResponse {
    command: String,
    lines: Vec<String>,
}

async fn api_log(State(state): State<HttpAppState>) -> impl IntoResponse {
    let command = state.streamer.last_command().await;
    let lines = state.streamer.last_log_tail(50).await;
    Json(LogResponse { command, lines })
}

/// AVFoundation (Mac) / DirectShow (Win) device scan, served from the
/// 60-second-TTL cache in [`crate::device_scanner`]. `?force=1` bypasses
/// the cache.
async fn api_devices(Query(q): Query<HashMap<String, String>>) -> impl IntoResponse {
    let force = matches!(q.get("force").map(String::as_str), Some("1") | Some("true"));
    let devs = crate::device_scanner::list_capture_devices(force);
    Json(devs)
}

/// Phase 4 will replace these with real NDI discovery via the
/// `grafton-ndi` crate. Returning empty lists keeps the JS UI happy.
#[derive(Serialize)]
struct DiscoverStub {
    devices: Vec<String>,
}
async fn api_discover_stub(Query(_q): Query<HashMap<String, String>>) -> impl IntoResponse {
    Json(DiscoverStub { devices: vec![] })
}

#[derive(Serialize)]
struct NdiSendersStub {
    senders: Vec<String>,
}
async fn api_ndi_senders_stub(Query(_q): Query<HashMap<String, String>>) -> impl IntoResponse {
    Json(NdiSendersStub { senders: vec![] })
}

async fn api_start(State(state): State<HttpAppState>) -> impl IntoResponse {
    match state.streamer.start().await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!(state.encoder.snapshot()))),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn api_stop(State(state): State<HttpAppState>) -> impl IntoResponse {
    match state.streamer.stop().await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!(state.encoder.snapshot()))),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

/// Settings mutation — accepts a JSON object with any subset of the
/// snapshot fields and applies them. Phase 1 supports the core string
/// / int fields; Phase 2+ extends to source/device fields.
#[derive(Deserialize)]
struct SettingsPayload {
    video_mode: Option<String>,
    quality_level: Option<String>,
    source_id: Option<String>,
    custom_url: Option<String>,
    stream_key: Option<String>,
    passphrase: Option<String>,
    srt_mode: Option<String>,
    srt_latency_us: Option<u32>,
    srt_listen_port: Option<u16>,
    streamid_override: Option<String>,
    streamid_legacy: Option<bool>,
    video_codec: Option<String>,
    current_service_name: Option<String>,
    current_server_name: Option<String>,
}

async fn api_settings(
    State(state): State<HttpAppState>,
    Json(payload): Json<SettingsPayload>,
) -> impl IntoResponse {
    state.encoder.apply_settings(&payload.into());
    Json(state.encoder.snapshot())
}

#[derive(Deserialize)]
struct LoadXmlPayload {
    path: String,
    make_active: Option<bool>,
}

async fn api_load_xml(
    State(state): State<HttpAppState>,
    Json(payload): Json<LoadXmlPayload>,
) -> impl IntoResponse {
    match state
        .encoder
        .add_service_from_xml(std::path::Path::new(&payload.path), payload.make_active)
    {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!(state.encoder.snapshot()))),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

#[derive(Deserialize)]
struct LoadXmlTextPayload {
    text: String,
    make_active: Option<bool>,
}

async fn api_load_xml_text(
    State(state): State<HttpAppState>,
    Json(payload): Json<LoadXmlTextPayload>,
) -> impl IntoResponse {
    match state
        .encoder
        .add_service_from_xml_text(&payload.text, payload.make_active.unwrap_or(true))
    {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!(state.encoder.snapshot()))),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn api_destination_paste_stub() -> impl IntoResponse {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({"error": "paste parser not yet ported (Phase 2)"})),
    )
}

// ---- settings adapter ------------------------------------------------------
//
// Bridges the HTTP DTO to the EncoderState's apply_settings() method.

impl From<SettingsPayload> for crate::state::SettingsUpdate {
    fn from(p: SettingsPayload) -> Self {
        Self {
            video_mode: p.video_mode,
            quality_level: p.quality_level,
            source_id: p.source_id,
            custom_url: p.custom_url,
            stream_key: p.stream_key,
            passphrase: p.passphrase,
            srt_mode: p.srt_mode,
            srt_latency_us: p.srt_latency_us,
            srt_listen_port: p.srt_listen_port,
            streamid_override: p.streamid_override,
            streamid_legacy: p.streamid_legacy,
            video_codec: p.video_codec,
            current_service_name: p.current_service_name,
            current_server_name: p.current_server_name,
        }
    }
}
