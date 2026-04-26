mod device_scanner;
mod ffmpeg_path;
mod http;
mod instance;
mod ndi_capture;
mod ndi_runtime;
mod protocol;
mod sources;
mod state;
mod streamer;
mod streamid;
mod xml;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{Manager, RunEvent};

use crate::http::HttpAppState;
use crate::protocol::ProtocolServer;
use crate::state::EncoderState;
use crate::streamer::Streamer;

const HTTP_START_PORT: u16 = 8090;
const BMD_START_PORT: u16 = 9977;

/// Window title format. Phase 6 surfaces the instance name + bound
/// ports so a user with multiple instances open can tell windows
/// apart at a glance.
fn window_title(instance: &str, http_port: u16, bmd_port: u16) -> String {
    if instance == "default" {
        format!("ATEM IP Patchbay  ·  http :{http_port}  ·  bmd :{bmd_port}")
    } else {
        format!("ATEM IP Patchbay [{instance}]  ·  http :{http_port}  ·  bmd :{bmd_port}")
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            // Phase 6: parse CLI args and resolve the per-instance
            // state directory. Surfaces instance_name + bound ports
            // in the window title and isolates persisted state per
            // instance.
            let cli = instance::Cli::from_env();
            let instance_dir = instance::ensure_instance_dir(&cli.instance_name);
            log::info!(
                "instance: {:?} (state dir: {})",
                cli.instance_name,
                instance_dir.display()
            );

            let encoder = Arc::new(EncoderState::new());

            // Tell the FFmpeg path resolver where the bundled sidecar
            // lives. Phase 9 adds bundle.externalBin; for now this just
            // primes the lookup so dev builds prefer $PATH.
            if let Ok(resource_dir) = app.handle().path().resource_dir() {
                ffmpeg_path::set_resource_root(resource_dir);
            }

            load_default_xml_files(app.handle(), &encoder);
            apply_default_devices_at_boot(&encoder);

            // Initialize the NDI runtime (loads libndi.dylib via
            // dlopen). If NDI Tools isn't installed this fails with
            // a clear runtime error and the rest of the app still
            // works — discovery just stays empty.
            if let Err(err) = ndi_runtime::init() {
                log::warn!("NDI runtime init failed (NDI features disabled): {err}");
            }

            let static_dir = resolve_static_dir(app.handle());
            log::info!("static dir: {}", static_dir.display());

            let streamer = Streamer::new(encoder.clone());
            let http_state = HttpAppState {
                encoder: encoder.clone(),
                streamer: streamer.clone(),
            };

            // Bind synchronously so we know the port before creating
            // the webview. The Axum server itself runs in a tokio task.
            let runtime = tauri::async_runtime::handle();
            let http_start = cli.http_port.unwrap_or(HTTP_START_PORT);
            let (port, listener) = runtime
                .block_on(async { crate::http::bind_with_walk(http_start).await })
                .expect("could not bind HTTP API port");
            log::info!("HTTP API listening on http://127.0.0.1:{port}/");

            let router = crate::http::router(http_state, static_dir);
            tauri::async_runtime::spawn(async move {
                if let Err(err) = axum::serve(listener, router).await {
                    log::error!("Axum server stopped: {err}");
                }
            });

            // Start the BMD control protocol server (TCP 9977 with
            // port-walk fallback). Same async runtime as Axum + the
            // streamer monitor task.
            let proto = ProtocolServer::new(encoder.clone(), streamer.clone());
            let proto_for_spawn = proto.clone();
            let bmd_start = cli.bmd_port.unwrap_or(BMD_START_PORT);
            let bmd_port = runtime
                .block_on(async move { proto_for_spawn.start(bmd_start).await })
                .map_err(|e| -> Box<dyn std::error::Error> {
                    Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
                })?;
            log::info!("BMD control protocol bound on TCP {bmd_port}");

            // Manage encoder + streamer + protocol server so future
            // Tauri commands / event handlers can grab them via
            // app.state().
            app.manage(encoder);
            app.manage(streamer);
            app.manage(proto);
            app.manage(http_state_marker(port));
            app.manage(BmdPort(bmd_port));

            // The window is declared in tauri.conf.json (visible: false
            // initially so the placeholder webui/index.html doesn't
            // flash). Navigate it to the embedded HTTP server now that
            // Axum is up, then reveal it.
            let url = format!("http://127.0.0.1:{port}/");
            if let Some(window) = app.get_webview_window("main") {
                let title = window_title(&cli.instance_name, port, bmd_port);
                if let Err(err) = window.set_title(&title) {
                    log::warn!("failed to set window title: {err}");
                }
                if let Err(err) = window.navigate(url.parse().expect("valid http URL")) {
                    log::error!("failed to navigate window to {url}: {err}");
                }
                if let Err(err) = window.show() {
                    log::error!("failed to show window: {err}");
                }
            } else {
                log::error!("no 'main' window declared in tauri.conf.json");
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // On Cmd-Q / window-close-causes-app-quit, Tauri fires
            // ExitRequested then Exit. We hook BOTH because some
            // platforms emit only one — and we synchronously stop
            // the streamer + NDI capture before the parent process
            // dies, otherwise FFmpeg orphans to launchd and keeps
            // streaming to the destination forever.
            match event {
                RunEvent::ExitRequested { .. } | RunEvent::Exit => {
                    if let Some(streamer) = app_handle.try_state::<Arc<Streamer>>() {
                        let s = streamer.inner().clone();
                        // block_on inside the Tauri run-loop is fine
                        // here; we want a hard sync barrier before the
                        // process exits so all child cleanup completes.
                        let runtime = tauri::async_runtime::handle();
                        let _ = runtime.block_on(s.stop());
                    }
                }
                _ => {}
            }
        });
}

/// Run an initial AVF / DirectShow scan and pre-select the most
/// reasonable video + audio defaults so the source dropdowns aren't
/// empty on first launch. Mirrors run.py's boot-time scan.
fn apply_default_devices_at_boot(encoder: &EncoderState) {
    let devs = device_scanner::list_capture_devices(true);
    let v = device_scanner::find_default_video(&devs);
    let a = device_scanner::find_default_audio(&devs);
    if v.is_none() && a.is_none() {
        return;
    }
    let v_index = v.map(|d| d.index).unwrap_or(0);
    let v_name = v.map(|d| d.name.clone()).unwrap_or_default();
    let a_index = a.map(|d| d.index).unwrap_or(-1);
    let a_name = a.map(|d| d.name.clone()).unwrap_or_default();
    log::info!(
        "default devices picked: video=[{v_index}] {v_name:?}, audio=[{a_index}] {a_name:?}"
    );
    encoder.apply_default_devices(v_index, &v_name, a_index, &a_name);
}

/// Look for an existing config dir alongside the binary (prod path) or
/// at the project root (dev path) and load every `*.xml` we find. Any
/// parse errors are logged and skipped — first XML wins as the active
/// service, matching v0.1.0 boot semantics.
fn load_default_xml_files(app_handle: &tauri::AppHandle, encoder: &EncoderState) {
    let candidate_dirs = candidate_config_dirs(app_handle);
    for dir in candidate_dirs {
        if !dir.is_dir() {
            continue;
        }
        log::info!("scanning config dir: {}", dir.display());
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut xml_paths: Vec<_> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("xml"))
            .collect();
        // Load in newest-first order so the most recently modified
        // XML wins as the active service via add_service_from_xml's
        // first-load-wins semantics. UX: drop a fresh XML in
        // config/, it becomes the default destination on next launch
        // without the user having to pick it from the dropdown.
        // Older XMLs still register in the services map for the
        // dropdown — first-modified-wins would have made the user's
        // newest test config invisible until manually selected.
        xml_paths.sort_by_key(|p| {
            std::cmp::Reverse(
                std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .ok(),
            )
        });
        for path in &xml_paths {
            match encoder.add_service_from_xml(path, None) {
                Ok(()) => log::info!("loaded service from {}", path.display()),
                Err(err) => log::warn!("failed to load {}: {err}", path.display()),
            }
        }
        if !xml_paths.is_empty() {
            return; // first dir with XMLs wins
        }
    }
    log::warn!(
        "no service XML loaded — drop a Blackmagic streaming XML next to the .app or in ./config/"
    );
}

/// In dev (`cargo tauri dev`), config/ lives at <repo-root>/config.
/// In a bundled .app, we look beside the executable and inside
/// `Contents/Resources/config`.
fn candidate_config_dirs(app_handle: &tauri::AppHandle) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(resource_dir) = app_handle.path().resource_dir() {
        out.push(resource_dir.join("config"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            out.push(parent.join("config"));
        }
    }
    // Dev fallback — relative to src-tauri/Cargo.toml.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(repo_root) = manifest_dir.parent() {
        out.push(repo_root.join("config"));
    }
    out
}

/// Resolve the directory containing `index.html` + `app.js` + `style.css`.
/// Prod: bundled into Contents/Resources/. Dev: bmd_emulator/static/ at
/// the repo root.
fn resolve_static_dir(app_handle: &tauri::AppHandle) -> PathBuf {
    if let Ok(resource_dir) = app_handle.path().resource_dir() {
        let candidate = resource_dir.join("static");
        if candidate.join("index.html").exists() {
            return candidate;
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(repo_root) = manifest_dir.parent() {
        let candidate = repo_root.join("bmd_emulator").join("static");
        if candidate.join("index.html").exists() {
            return candidate;
        }
        // Fall back to the Phase 0 placeholder so the window is never blank.
        return repo_root.join("webui");
    }
    PathBuf::from("./webui")
}

/// Tiny wrapper so we can manage the bound HTTP port via Tauri state
/// without exposing the raw u16 (lets future code request it for the
/// LAN address that the relay listener publishes, etc.).
#[derive(Clone, Copy)]
pub struct HttpPort(pub u16);

#[derive(Clone, Copy)]
pub struct BmdPort(pub u16);

fn http_state_marker(port: u16) -> HttpPort {
    HttpPort(port)
}
