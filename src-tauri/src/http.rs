use anyhow::{anyhow, Result};
use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
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
use tower_http::set_header::SetResponseHeaderLayer;

use crate::preview::Preview;
use crate::state::EncoderState;
use crate::streamer::Streamer;

#[derive(Clone)]
pub struct HttpAppState {
    pub encoder: Arc<EncoderState>,
    pub streamer: Arc<Streamer>,
    pub preview: Arc<Preview>,
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
    // /static/* prefix routes win over the index fallback. Wrap with
    // a no-cache layer because the WebView (and most browsers without
    // an explicit Cache-Control) will heuristically cache JS/CSS for
    // ~10% of (now - last-modified). After a few minutes that's long
    // enough to make every dev-loop edit invisible until force-reload.
    // Plain Cmd-R now picks up changes.
    use tower_layer::Layer;
    let static_service = SetResponseHeaderLayer::overriding(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-store, must-revalidate"),
    )
    .layer(ServeDir::new(&static_dir));
    let index_path = static_dir.join("index.html");

    // Re-read index.html on every request rather than caching a copy
    // at boot. Caching at boot meant any edit to index.html was
    // invisible until the dev process restarted, even though edits
    // to JS/CSS (served by ServeDir) showed up immediately. The cost
    // is one stat + read per page load, which is negligible at
    // anything below high-traffic-server scale and the file is small
    // enough that the OS will keep it in the page cache anyway. If we
    // ever serve from inside a bundled .app's Resources/ where the
    // file genuinely never changes, we can swap in a OnceCell-cached
    // path keyed off cfg!(debug_assertions).
    Router::new()
        .route("/", get(move || {
            let path = index_path.clone();
            async move {
                match tokio::fs::read_to_string(&path).await {
                    Ok(html) => axum::response::Html(html),
                    Err(err) => {
                        log::warn!(
                            "could not read index.html from {}: {}",
                            path.display(),
                            err
                        );
                        axum::response::Html(
                            "<!doctype html><title>ATEM IP Patchbay</title><h1>UI not found</h1>"
                                .to_string(),
                        )
                    }
                }
            }
        }))
        .route("/api/state", get(api_state))
        .route("/api/lan-ip", get(api_lan_ip))
        .route("/api/preview", get(api_preview))
        .route("/api/preview/start", post(api_preview_start))
        .route("/api/preview/stop", post(api_preview_stop))
        .route("/api/log", get(api_log))
        .route("/api/devices", get(api_devices))
        .route("/api/discover", get(api_discover))
        .route("/api/ndi-senders", get(api_ndi_senders))
        .route("/api/start", post(api_start))
        .route("/api/stop", post(api_stop))
        .route("/api/kill-orphans", post(api_kill_orphans))
        .route("/api/settings", post(api_settings))
        .route("/api/load_xml", post(api_load_xml))
        .route("/api/load_xml_text", post(api_load_xml_text))
        .route("/api/services/clear", post(api_clear_services))
        .route("/api/destination/paste", post(api_destination_paste_stub))
        .nest_service("/static", static_service)
        .with_state(state)
}

// ---- handlers --------------------------------------------------------------

async fn api_state(State(state): State<HttpAppState>) -> impl IntoResponse {
    // Flatten the encoder snapshot with the current preview status so
    // the JS poll loop sees both in one request — keeps the Preview
    // button label in sync with backend state without a second poll.
    #[derive(Serialize)]
    struct StateResponse {
        #[serde(flatten)]
        snapshot: crate::state::Snapshot,
        preview: crate::preview::PreviewStatus,
    }
    Json(StateResponse {
        snapshot: state.encoder.snapshot(),
        preview: state.preview.status().await,
    })
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

/// Latest preview JPEG. Reads from the streamer first (full-bandwidth
/// receiver, more recent frames), falls back to the pre-stream
/// preview backend's last JPEG. Returns 204 when neither has a frame
/// available — the JS poll loop interprets that as "waiting" and
/// shows the SMPTE bars / waiting hint.
async fn api_preview(State(state): State<HttpAppState>) -> Response {
    let jpeg = match state.streamer.current_ndi_preview().await {
        Some(j) => Some(j),
        None => state.preview.latest_jpeg(),
    };
    match jpeg {
        Some(jpeg) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/jpeg")
            .header(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate")
            .header(header::PRAGMA, "no-cache")
            .header(header::EXPIRES, "0")
            .body(Body::from(jpeg))
            .unwrap(),
        None => Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap(),
    }
}

/// Start a pre-stream preview for the source currently configured in
/// EncoderState. NDI sources spin up a low-bandwidth Receiver; other
/// source kinds return 400 until the FFmpeg snapshot backend lands.
/// Idempotent — calling while a preview is already running stops the
/// prior one and starts the new one (so a source-change + Preview
/// click sequence works without an explicit Stop Preview).
async fn api_preview_start(State(state): State<HttpAppState>) -> impl IntoResponse {
    let snap = state.encoder.snapshot();
    let result = match snap.source_id.as_str() {
        "ndi" => {
            if snap.ndi_source_name.is_empty() {
                Err("No NDI sender selected. Pick one from the source gallery first.".to_string())
            } else {
                state
                    .preview
                    .start_ndi(&snap.ndi_source_name)
                    .await
                    .map_err(|e| e.to_string())
            }
        }
        other => Err(format!(
            "Pre-stream preview is only implemented for NDI sources today. \
             Selected source: {other:?}. (FFmpeg-snapshot backend for \
             AVFoundation / pipe / test_pattern lands in a follow-up.)"
        )),
    };
    match result {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!(state.preview.status().await)),
        ),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err})),
        ),
    }
}

async fn api_preview_stop(State(state): State<HttpAppState>) -> impl IntoResponse {
    state.preview.stop().await;
    (
        StatusCode::OK,
        Json(serde_json::json!(state.preview.status().await)),
    )
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

/// NDI source list, fed by the grafton-ndi Finder. /api/discover and
/// /api/ndi-senders return the same payload shape — the v0.1.0 UI
/// hits both endpoints from different code paths.
#[derive(Serialize)]
struct NdiSendersResponse {
    senders: Vec<crate::ndi_runtime::NdiSource>,
}

#[derive(Serialize)]
struct DiscoverResponse {
    devices: Vec<crate::ndi_runtime::NdiSource>,
}

async fn api_discover(Query(q): Query<HashMap<String, String>>) -> impl IntoResponse {
    let wait = if matches!(q.get("force").map(String::as_str), Some("1") | Some("true")) {
        std::time::Duration::from_secs(2)
    } else {
        std::time::Duration::from_millis(500)
    };
    let devices = crate::ndi_runtime::discover(wait);
    Json(DiscoverResponse { devices })
}

async fn api_ndi_senders(Query(q): Query<HashMap<String, String>>) -> impl IntoResponse {
    let wait = if matches!(q.get("force").map(String::as_str), Some("1") | Some("true")) {
        std::time::Duration::from_secs(2)
    } else {
        std::time::Duration::from_millis(500)
    };
    let senders = crate::ndi_runtime::discover(wait);
    Json(NdiSendersResponse { senders })
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

/// Kill any FFmpeg subprocess pushing a BMD-flavored SRT URL — i.e.
/// any leftover from a prior run of this app whose parent died
/// without taking the FFmpeg child with it. Matches on streamid=
/// in the cmdline so unrelated FFmpeg jobs (transcodes, captures
/// for other apps) don't get hit.
///
/// The dev-rebuild path under `cargo tauri dev` is the most common
/// trigger for these orphans: cargo-tauri SIGKILLs the binary on
/// rebuild, so the Drop on Streamer never runs and the FFmpeg
/// process group survives.
async fn api_kill_orphans(_state: State<HttpAppState>) -> impl IntoResponse {
    #[cfg(target_os = "macos")]
    let result = run_pkill("ffmpeg.*streamid=");
    #[cfg(target_os = "linux")]
    let result = run_pkill("ffmpeg.*streamid=");
    #[cfg(target_os = "windows")]
    let result: Result<usize, String> = Err(
        "Orphan-kill not yet implemented on Windows. Use Task Manager to end \
         any 'ffmpeg.exe' processes if a prior run left one streaming."
            .into(),
    );
    match result {
        Ok(killed) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "killed": killed,
                "message": if killed == 0 {
                    "No orphan FFmpeg streams found.".to_string()
                } else {
                    format!("Killed {killed} orphan FFmpeg stream(s).")
                }
            })),
        ),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err})),
        ),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run_pkill(pattern: &str) -> Result<usize, String> {
    use std::process::Command;
    // First a -SIGTERM pass for graceful shutdown (FFmpeg flushes
    // its SRT/RTMP buffers and closes the connection cleanly), then
    // a SIGKILL fallback after a brief settle for anything that
    // ignored the TERM. pkill exits 0 when matches were found and
    // signaled, 1 when none, >=2 on error.
    let term = Command::new("pkill")
        .args(["-TERM", "-f", pattern])
        .status()
        .map_err(|e| format!("pkill failed to launch: {e}"))?;
    let killed_term = matches!(term.code(), Some(0));
    if killed_term {
        std::thread::sleep(std::time::Duration::from_millis(800));
        let _ = Command::new("pkill")
            .args(["-KILL", "-f", pattern])
            .status();
    }
    // Re-check via pgrep to count what actually went away. pgrep -c
    // prints the matching count to stdout.
    let probe = Command::new("pgrep")
        .args(["-c", "-f", pattern])
        .output();
    let still_alive = probe
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);
    // Best-effort count: if pkill said it found something, assume it
    // killed at least one; subtract any that survived (which would
    // mean SIGKILL also failed — extremely rare).
    let killed = if killed_term {
        if still_alive == 0 { 1 } else { 0 }
    } else {
        0
    };
    Ok(killed)
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
    ndi_source_name: Option<String>,
    av_video_index: Option<i32>,
    av_video_name: Option<String>,
    av_audio_index: Option<i32>,
    av_audio_name: Option<String>,
    audio_mode: Option<String>,
    audio_output_mono: Option<bool>,
    audio_pan_l: Option<u8>,
    audio_pan_r: Option<u8>,
    pipe_path: Option<String>,
    label: Option<String>,
    relay: Option<RelayPayload>,
    overlay: Option<OverlayPayload>,
}

#[derive(Deserialize)]
struct RelayPayload {
    bind_host: Option<String>,
    srt_port: Option<u16>,
    srt_latency_us: Option<u32>,
    srt_passphrase: Option<String>,
    rtmp_port: Option<u16>,
    rtmp_app: Option<String>,
    rtmp_key: Option<String>,
}

#[derive(Deserialize)]
struct OverlayPayload {
    title: Option<String>,
    subtitle: Option<String>,
    logo_path: Option<String>,
    clock: Option<bool>,
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
    /// When true (default for UI loads), wipe the existing service
    /// registry before loading. The boot loader passes false so all
    /// config/*.xml files accumulate at startup.
    replace: Option<bool>,
}

async fn api_load_xml(
    State(state): State<HttpAppState>,
    Json(payload): Json<LoadXmlPayload>,
) -> impl IntoResponse {
    if payload.replace.unwrap_or(true) {
        state.encoder.clear_services();
    }
    match state
        .encoder
        .add_service_from_xml(std::path::Path::new(&payload.path), payload.make_active)
    {
        Ok(()) => xml_load_response(&state),
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
    replace: Option<bool>,
}

async fn api_load_xml_text(
    State(state): State<HttpAppState>,
    Json(payload): Json<LoadXmlTextPayload>,
) -> impl IntoResponse {
    if payload.replace.unwrap_or(true) {
        state.encoder.clear_services();
    }
    match state
        .encoder
        .add_service_from_xml_text(&payload.text, payload.make_active.unwrap_or(true))
    {
        Ok(()) => xml_load_response(&state),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

/// Wraps the snapshot in `{service, snapshot}` shape so the UI can
/// show the just-loaded service's name in the status chip without
/// having to re-derive it from current_service_name.
fn xml_load_response(state: &HttpAppState) -> (StatusCode, Json<serde_json::Value>) {
    let snap = state.encoder.snapshot();
    let service = snap.current_service_name.clone();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "service": service,
            "snapshot": snap,
        })),
    )
}

#[derive(Deserialize, Default)]
struct ClearServicesPayload {
    /// When true (default), also clear `custom_url`. Pass false to
    /// retain a manual destination across the clear.
    clear_custom_url: Option<bool>,
}

async fn api_clear_services(
    State(state): State<HttpAppState>,
    Json(payload): Json<ClearServicesPayload>,
) -> impl IntoResponse {
    state.encoder.clear_services();
    if payload.clear_custom_url.unwrap_or(true) {
        // Wipe the custom URL too so the UI is back to a fully blank
        // destination state. The Clear XML button calls this with no
        // body so the default takes effect.
        state.encoder.apply_settings(&crate::state::SettingsUpdate {
            custom_url: Some(String::new()),
            ..Default::default()
        });
    }
    Json(state.encoder.snapshot())
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
            ndi_source_name: p.ndi_source_name,
            av_video_index: p.av_video_index,
            av_video_name: p.av_video_name,
            av_audio_index: p.av_audio_index,
            av_audio_name: p.av_audio_name,
            audio_mode: p.audio_mode,
            audio_output_mono: p.audio_output_mono,
            audio_pan_l: p.audio_pan_l,
            audio_pan_r: p.audio_pan_r,
            pipe_path: p.pipe_path,
            label: p.label,
            relay: p.relay.map(|r| crate::state::RelaySettingsUpdate {
                bind_host: r.bind_host,
                srt_port: r.srt_port,
                srt_latency_us: r.srt_latency_us,
                srt_passphrase: r.srt_passphrase,
                rtmp_port: r.rtmp_port,
                rtmp_app: r.rtmp_app,
                rtmp_key: r.rtmp_key,
            }),
            overlay: p.overlay.map(|o| crate::state::OverlaySettingsUpdate {
                title: o.title,
                subtitle: o.subtitle,
                logo_path: o.logo_path,
                clock: o.clock,
            }),
        }
    }
}
