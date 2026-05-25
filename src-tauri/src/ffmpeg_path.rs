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
