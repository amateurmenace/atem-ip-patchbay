mod device_scanner;
mod ffmpeg_path;
mod fleet;
mod frame_pack;
mod http;
mod instance;
mod ndi_capture;
mod ndi_runtime;
mod omt_capture;
mod omt_runtime;
mod omt_sender;
mod preview;
mod protocol;
mod sources;
mod state;
mod streamer;
mod streamid;
mod xml;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{Manager, RunEvent};

use crate::fleet::{EncoderFleet, TILE_COUNT};
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

/// Tauri command that spawns a new patchbay process via the existing
/// `--instance-name` CLI flag. Used by the multiview shell's
/// "+ New Window" button to let a power user run a 5th+ stream in
/// its own window with its own state directory + port pair, on top
/// of the in-app 2x2 grid.
///
/// Detaches properly so closing the parent doesn't kill the spawn:
/// macOS/Linux use `setsid` to put the child in a new session +
/// process group; Windows uses `CREATE_NEW_PROCESS_GROUP |
/// DETACHED_PROCESS` so the child survives the parent's exit and
/// gets its own console (or none, with our existing hide-console
/// helper).
#[tauri::command]
async fn spawn_instance(name: String) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("instance name cannot be empty".into());
    }
    // Sanitize — instance name becomes a directory path component.
    // Reject anything that would create a weird filesystem layout.
    if trimmed.contains(['/', '\\', ':', '\0']) {
        return Err(format!("instance name contains invalid characters: {trimmed:?}"));
    }
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("could not resolve current exe: {e}"))?;

    log::info!("spawning new instance: name={trimmed:?} exe={}", current_exe.display());

    let mut cmd = std::process::Command::new(&current_exe);
    cmd.args(["--instance-name", trimmed]);

    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            // setsid creates a new session + process group with the
            // child as leader. Without this, the spawned instance
            // shares our process group → Cmd-Q on the parent kills
            // the spawn too. setsid detaches it cleanly.
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP = 0x0000_0200, the child can be
        // signaled independently of our process group.
        // DETACHED_PROCESS = 0x0000_0008, no shared console with
        // the parent — pairs with the hide-console helper in
        // ffmpeg_path.rs to keep the spawn console-less on Windows.
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|child| {
            log::info!(
                "spawned instance {trimmed:?} pid={}",
                child.id()
            );
            // We intentionally drop the Child handle here — the
            // OS keeps the process alive (detached). If we held
            // onto it, the kill_on_drop default would terminate
            // the spawn when our process exits, which is the
            // opposite of what we want.
        })
        .map_err(|e| format!("failed to spawn instance: {e}"))
}

/// Tauri command that opens (or focuses, if already open) a small
/// companion window pointed at /static/monitor.html. The monitor
/// shows live preview + condensed stats — designed to be positioned
/// on screen alongside others like a video-switcher multiview
/// output. Each instance (process) has its own main window + one
/// monitor; operators run multiple instances via `spawn_instance`
/// to drive multi-source workflows.
///
/// The label "monitor" is fixed (only one per instance). Clicking
/// "Open Monitor" again when the monitor is already open just
/// focuses the existing window — no duplicates.
#[tauri::command]
async fn open_monitor_window(handle: tauri::AppHandle) -> Result<(), String> {
    use tauri::WebviewUrl;
    // The monitor is served from our embedded Axum HTTP server — the
    // main window was navigated to http://127.0.0.1:PORT/ at boot
    // (see setup()), and the monitor lives at /static/monitor.html
    // on the same origin. Resolve the main window's URL so we know
    // which port to target.
    let main_url = match handle.get_webview_window("main") {
        Some(win) => win
            .url()
            .map_err(|e| format!("could not read main window URL: {e}"))?,
        None => return Err("main window not found".into()),
    };
    let scheme = main_url.scheme();
    let host = main_url.host_str().unwrap_or("127.0.0.1");
    let port = main_url
        .port_or_known_default()
        .ok_or_else(|| "main window URL has no port".to_string())?;
    let monitor_url_str = format!("{scheme}://{host}:{port}/static/monitor.html");
    let monitor_url = tauri::Url::parse(&monitor_url_str)
        .map_err(|e| format!("invalid monitor URL {monitor_url_str:?}: {e}"))?;

    // If a monitor window is already open, just bring it to front
    // instead of spawning a second one.
    if let Some(existing) = handle.get_webview_window("monitor") {
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        let _ = existing.show();
        return Ok(());
    }

    // Otherwise spawn a fresh monitor window. Default size is small
    // enough to drop into a corner; resizable so the user can scale
    // up if they want to read the preview at higher resolution.
    tauri::WebviewWindowBuilder::new(
        &handle,
        "monitor",
        WebviewUrl::External(monitor_url),
    )
    .title("ATEM Patchbay — Monitor")
    .inner_size(360.0, 320.0)
    .min_inner_size(220.0, 200.0)
    .resizable(true)
    .always_on_top(false)
    .build()
    .map_err(|e| format!("failed to open monitor window: {e}"))?;
    Ok(())
}

/// Tauri command that brings the main configuration window back to
/// front. Wired to the monitor window's "Expand ↗" button so the
/// operator can jump back to the full UI without having to navigate
/// the OS window list. Leaves the monitor window open — the
/// operator can close it manually when done.
#[tauri::command]
async fn focus_main_window(handle: tauri::AppHandle) -> Result<(), String> {
    let main = handle
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    let _ = main.unminimize();
    main.show().map_err(|e| format!("show failed: {e}"))?;
    main.set_focus().map_err(|e| format!("set_focus failed: {e}"))?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            spawn_instance,
            open_monitor_window,
            focus_main_window,
        ])
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

            // alpha.15 multi-source: the fleet holds N independent
            // tiles, each one a self-contained source→destination
            // pipeline. For this phase, only tile 0 is wired through
            // the existing HTTP API + BMD protocol server; tiles 1-3
            // exist in memory but are dormant until later commits
            // expose them via /api/i/:idx/* routes and start
            // additional protocol servers. Cost of the dormant tiles
            // is ~a few KB of struct overhead each — no FFmpeg, no
            // SDK threads.
            let bmd_start = cli.bmd_port.unwrap_or(BMD_START_PORT);
            // Per-tile BMD port plan: each tile gets a 4-port range
            // starting at BMD_START_PORT + idx*4 (so tile 0 walks
            // 9977-9980, tile 1 walks 9981-9984, etc.). For Phase 1
            // only tile 0 is bound; tiles 1-3 carry the planned
            // start ports as a forward-looking marker.
            let bmd_ports: [u16; TILE_COUNT] = std::array::from_fn(|i| {
                bmd_start.saturating_add((i * 4) as u16)
            });
            let fleet = EncoderFleet::new(bmd_ports);
            log::info!(
                "encoder fleet: {} tiles, BMD start ports {:?}",
                TILE_COUNT,
                bmd_ports
            );

            // Tile 0 is the "default" tile that the existing single-
            // source UI talks to. XML configs + default device
            // selection apply to it; future commits will replay the
            // same boot work per-tile when each loads its own
            // state-N.json.
            let tile0 = fleet
                .tile(0)
                .expect("fleet must always have at least tile 0");
            let encoder = tile0.encoder.clone();
            let streamer = tile0.streamer.clone();

            // Tell the FFmpeg path resolver where the bundled sidecar
            // lives. Phase 9 adds bundle.externalBin; for now this just
            // primes the lookup so dev builds prefer $PATH.
            //
            // On Windows we ALSO have to teach the OS DLL loader where
            // to look for Processing.NDI.Lib.x64.dll (and any future
            // OMT dylibs we bundle). macOS's dyld picks them up via
            // the @executable_path/../Frameworks rpath set in build.rs;
            // Windows has no equivalent baked into the binary, so we
            // call SetDllDirectoryW at startup instead. Without this,
            // grafton-ndi's runtime-load fails with "module not found"
            // even though the DLL is sitting in resources\sidecar\
            // right next to ffmpeg.exe.
            if let Ok(resource_dir) = app.handle().path().resource_dir() {
                ffmpeg_path::set_resource_root(resource_dir.clone());
                add_windows_dll_search_path(&resource_dir);
            }

            // Probe DeckLink output support eagerly so the first UI
            // request that reads `/api/state` doesn't pay the
            // ffmpeg-spawn cost. Result is cached for the process
            // lifetime; if the user reinstalls FFmpeg between
            // launches the probe runs fresh on next boot.
            let _ = ffmpeg_path::ffmpeg_has_decklink();

            load_default_xml_files(app.handle(), &encoder);
            apply_default_devices_at_boot(&encoder);

            // Initialize the NDI runtime (loads libndi.dylib via
            // dlopen). If NDI Tools isn't installed this fails with
            // a clear runtime error and the rest of the app still
            // works — discovery just stays empty.
            if let Err(err) = ndi_runtime::init() {
                log::warn!("NDI runtime init failed (NDI features disabled): {err}");
            }
            // OMT runtime — same pattern, no-op when the omt cargo
            // feature is off (default). Discovery will return empty
            // and OMT tile gallery will be empty.
            if let Err(err) = omt_runtime::init() {
                log::warn!("OMT runtime init failed (OMT features disabled): {err}");
            }

            let static_dir = resolve_static_dir(app.handle());
            log::info!("static dir: {}", static_dir.display());

            // alpha.15: the router now takes the fleet directly and
            // mounts per-tile API sub-routers at /api/i/0..3 plus a
            // /api/* alias for tile 0. The HttpAppState wrapper that
            // existed in earlier code paths is constructed internally
            // by the router for each mount point.

            // Bind synchronously so we know the port before creating
            // the webview. The Axum server itself runs in a tokio task.
            let runtime = tauri::async_runtime::handle();
            let http_start = cli.http_port.unwrap_or(HTTP_START_PORT);
            let (port, listener) = runtime
                .block_on(async { crate::http::bind_with_walk(http_start).await })
                .expect("could not bind HTTP API port");
            log::info!("HTTP API listening on http://127.0.0.1:{port}/");

            let router = crate::http::router(fleet.clone(), static_dir);
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

            // Manage fleet (the new shared multi-tile state) +
            // tile-0 aliases (used by the legacy single-source flows)
            // + the protocol server. The fleet is the long-term home;
            // legacy fields stay around so existing Tauri commands
            // grabbing Arc<Streamer> via app.state() still resolve.
            app.manage(fleet.clone());
            app.manage(encoder);
            app.manage(streamer);
            app.manage(tile0.preview.clone());
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
            // every tile's streamer before the parent process dies,
            // otherwise FFmpeg orphans to launchd and keeps streaming
            // to the destination forever.
            //
            // alpha.15 multi-source: shutdown_all iterates every
            // tile, not just the singleton, so spawning N FFmpegs
            // across 4 tiles cleans up uniformly. Phase 1 only ever
            // has tile 0 running, but the iteration cost is
            // negligible and the hook is correct for future phases.
            match event {
                RunEvent::ExitRequested { .. } | RunEvent::Exit => {
                    if let Some(fleet) = app_handle.try_state::<Arc<EncoderFleet>>() {
                        let f = fleet.inner().clone();
                        let runtime = tauri::async_runtime::handle();
                        runtime.block_on(f.shutdown_all());
                    } else if let Some(streamer) = app_handle.try_state::<Arc<Streamer>>() {
                        // Fallback in case fleet management hasn't
                        // landed (test harness / partial init).
                        let s = streamer.inner().clone();
                        let runtime = tauri::async_runtime::handle();
                        let _ = runtime.block_on(s.stop());
                    }
                }
                _ => {}
            }
        });
}

/// Teach Windows' DLL loader to find bundled sidecar dylibs (currently
/// `Processing.NDI.Lib.x64.dll`; Phase D adds `libomt.dll` next to it).
/// macOS handles this via `@executable_path/../Frameworks` rpath set in
/// build.rs; Windows has no equivalent baked into the executable, so
/// we call into the Win32 API at startup. Stashed here as its own
/// `#[cfg]`-gated helper so it stays out of the way on Mac/Linux.
///
/// Without this, `grafton-ndi`'s dlopen-equivalent fails with "module
/// not found" even though the DLL is sitting in `resources\sidecar\`
/// right next to `ffmpeg.exe` — Windows only searches the executable's
/// own dir + System32 + PATH by default. End users see "NDI features
/// disabled" with no actionable cause; this fixes that.
#[cfg(target_os = "windows")]
fn add_windows_dll_search_path(resource_dir: &std::path::Path) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let sidecar = resource_dir.join("sidecar");
    if !sidecar.exists() {
        log::warn!(
            "sidecar dir not found at {} — NDI/OMT may fail to load",
            sidecar.display()
        );
        return;
    }

    let wide: Vec<u16> = OsStr::new(&sidecar)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    extern "system" {
        fn SetDllDirectoryW(lp_path_name: *const u16) -> i32;
    }

    unsafe {
        if SetDllDirectoryW(wide.as_ptr()) == 0 {
            log::warn!(
                "SetDllDirectoryW failed for {}; NDI/OMT runtime load may fail",
                sidecar.display()
            );
        } else {
            log::info!("Windows DLL search path added: {}", sidecar.display());
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn add_windows_dll_search_path(_resource_dir: &std::path::Path) {
    // macOS uses @executable_path/../Frameworks rpath set in build.rs;
    // Linux uses LD_LIBRARY_PATH (we don't currently bundle libndi
    // for Linux — users install NDI Tools).
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
