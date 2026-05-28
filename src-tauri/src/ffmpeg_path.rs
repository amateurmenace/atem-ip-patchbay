use std::path::PathBuf;
use std::sync::OnceLock;

/// Cached binary path. Set once at boot via [`set_resource_root`] from
/// the Tauri setup() callback (which knows where the bundle puts the
/// FFmpeg sidecar). Subsequent lookups via [`ffmpeg_path`] are cheap.
static RESOURCE_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Called once from Tauri's setup() with the directory that contains
/// the bundled FFmpeg sidecar (typically `Contents/Resources/`). After
/// this, [`ffmpeg_path`] will prefer the bundled binary over `$PATH`.
pub fn set_resource_root(root: PathBuf) {
    let _ = RESOURCE_ROOT.set(root);
}

/// Resolve the FFmpeg binary to invoke. Search order:
///
/// 1. `ATEM_PATCHBAY_FFMPEG` env var (override — useful for testing
///    a custom build against the dev binary).
/// 2. Bundled sidecar at `<resource-root>/ffmpeg` (or `ffmpeg.exe` on
///    Windows). Set via [`set_resource_root`] in Tauri setup().
/// 3. First `ffmpeg` on `$PATH`.
/// 4. Bare `"ffmpeg"` so the subprocess error surfaces at the call
///    site instead of here.
pub fn ffmpeg_path() -> String {
    if let Ok(override_path) = std::env::var("ATEM_PATCHBAY_FFMPEG") {
        if !override_path.is_empty() && std::path::Path::new(&override_path).exists() {
            return override_path;
        }
    }

    if let Some(root) = RESOURCE_ROOT.get() {
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        // Tauri 2 bundles `bundle.resources` paths verbatim under
        // Contents/Resources/. We download FFmpeg to
        // src-tauri/sidecar/ffmpeg{.exe} in CI so it lands at
        // <resource_root>/sidecar/ffmpeg{.exe} in the .app/.exe
        // bundle. Plain <resource_root>/ffmpeg is checked as a
        // fallback for legacy or hand-bundled layouts.
        for relative in [
            format!("sidecar/ffmpeg{suffix}"),
            format!("ffmpeg{suffix}"),
        ] {
            let candidate = root.join(&relative);
            if candidate.exists() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }

    if let Ok(found) = which::which("ffmpeg") {
        return found.to_string_lossy().into_owned();
    }

    "ffmpeg".into()
}

/// Resolve the atem-net-diag binary to invoke. alpha.17 bundles this
/// in the main app's `sidecar/` resources on Windows so the Net Diag
/// topbar button just works without a separate install. On macOS the
/// existing flow (the standalone `.app` installed at /Applications)
/// is the primary path; this resolver is mostly Windows-facing.
///
/// Search order mirrors [`ffmpeg_path`]:
///   1. `ATEM_NET_DIAG_PATH` env var (testing / dev override).
///   2. Bundled binary in `<resource-root>/sidecar/atem-net-diag{.exe}`.
///   3. Plain `atem-net-diag` on PATH (CLI users who installed
///      separately).
///   4. Returns None — caller decides whether to surface an error
///      or fall through to a different launch strategy.
pub fn net_diag_path() -> Option<String> {
    if let Ok(override_path) = std::env::var("ATEM_NET_DIAG_PATH") {
        if !override_path.is_empty() && std::path::Path::new(&override_path).exists() {
            return Some(override_path);
        }
    }

    if let Some(root) = RESOURCE_ROOT.get() {
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        for relative in [
            format!("sidecar/atem-net-diag{suffix}"),
            format!("atem-net-diag{suffix}"),
        ] {
            let candidate = root.join(&relative);
            if candidate.exists() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }

    if let Ok(found) = which::which("atem-net-diag") {
        return Some(found.to_string_lossy().into_owned());
    }

    None
}

/// Suppress the console window Windows spawns for any console
/// subprocess (FFmpeg, FFprobe, `cmd /c …`). Passes `CREATE_NO_WINDOW`
/// (0x0800_0000) to `CreateProcess` so the child inherits no console.
/// Without this, a black terminal window pops up next to the Tauri
/// window every time we shell out — the user-visible "ffmpeg.exe
/// console" complaint from the alpha.11 Windows test.
///
/// No-op on macOS/Linux.
#[cfg(windows)]
pub fn hide_console_std(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
pub fn hide_console_std(_cmd: &mut std::process::Command) {}

/// Tokio variant of [`hide_console_std`]. `tokio::process::Command`
/// exposes `creation_flags` as an inherent method on Windows so no
/// trait import is needed.
#[cfg(windows)]
pub fn hide_console_tokio(cmd: &mut tokio::process::Command) {
    cmd.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
pub fn hide_console_tokio(_cmd: &mut tokio::process::Command) {}

/// Cached "does our FFmpeg build include DeckLink output support?"
/// Populated on first call via [`ffmpeg_has_decklink`]; the eager
/// path is [`probe_decklink_support_eager`], called from Tauri
/// setup() so the answer is ready before any UI thread asks.
static HAS_DECKLINK: OnceLock<bool> = OnceLock::new();

/// True iff the resolved FFmpeg binary was built with
/// `--enable-decklink` (i.e. the DeckLink output muxer is registered).
/// First call probes `ffmpeg -hide_banner -muxers` and caches; later
/// calls are a single atomic load.
///
/// Used by the DeckLink-output destination path (Session 12) to gate
/// the UI: the destination-type picker disables the DeckLink option
/// when this is false and surfaces a "reinstall ATEM IP Patchbay —
/// this build is missing DeckLink support" hint instead.
///
/// Why -muxers and not -outdevs / -formats / -h muxer=decklink:
/// `-muxers` is the most stable single-purpose list, prints to stdout,
/// always exits 0, and contains exactly one line per muxer ("E decklink
/// Blackmagic DeckLink output" when present). The other options either
/// share output channels with errors (-h exits with stderr on missing
/// muxers) or include incidental matches (-formats lists demuxers too,
/// though only the output direction matters for us).
pub fn ffmpeg_has_decklink() -> bool {
    *HAS_DECKLINK.get_or_init(probe_decklink_support)
}

fn probe_decklink_support() -> bool {
    let mut cmd = std::process::Command::new(ffmpeg_path());
    cmd.args(["-hide_banner", "-muxers"])
        .stderr(std::process::Stdio::null());
    hide_console_std(&mut cmd);
    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            log::warn!("decklink probe: ffmpeg invocation failed: {e}");
            return false;
        }
    };
    if !output.status.success() {
        log::warn!(
            "decklink probe: ffmpeg -muxers exited {:?}",
            output.status.code()
        );
        return false;
    }
    // -muxers output is one line per muxer; the decklink line looks
    // like " E decklink           Blackmagic DeckLink output". A
    // simple substring match is safe — "decklink" doesn't appear in
    // any other registered muxer name in current FFmpeg builds.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let present = stdout
        .lines()
        .any(|line| line.contains("decklink"));
    log::info!("ffmpeg decklink muxer present: {present}");
    present
}
