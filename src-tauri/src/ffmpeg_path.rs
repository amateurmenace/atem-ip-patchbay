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

/// Cached list of video encoders the resolved FFmpeg supports. Used by
/// the alpha.25 encoder picker to gate the dropdown (don't offer
/// nvenc when the bundled FFmpeg lacks it) and by select_encoder's
/// auto-mode to pick the best available.
///
/// First call probes `ffmpeg -hide_banner -encoders` and caches the
/// names of all video encoders. The output looks like:
///
///     Encoders:
///      V..... = Video
///      A..... = Audio
///      S..... = Subtitle
///      --
///      V..... libx264              libx264 H.264 / AVC / MPEG-4 AVC ...
///      V....D h264_nvenc           NVIDIA NVENC H.264 encoder ...
///      V..... hevc_videotoolbox    VideoToolbox H.265 Encoder
///
/// Lines starting with `V` (any combination of flags after) are video
/// encoders; we extract column 2 (the encoder name). Other categories
/// (audio "A", subtitle "S") are filtered out — they don't affect
/// video-encoder picking and pollute the list.
static AVAILABLE_ENCODERS: OnceLock<Vec<String>> = OnceLock::new();

pub fn available_encoders() -> &'static [String] {
    AVAILABLE_ENCODERS
        .get_or_init(probe_available_encoders)
        .as_slice()
}

fn probe_available_encoders() -> Vec<String> {
    let mut cmd = std::process::Command::new(ffmpeg_path());
    cmd.args(["-hide_banner", "-encoders"])
        .stderr(std::process::Stdio::null());
    hide_console_std(&mut cmd);
    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            log::warn!("encoder probe: ffmpeg invocation failed: {e}");
            return Vec::new();
        }
    };
    if !output.status.success() {
        log::warn!(
            "encoder probe: ffmpeg -encoders exited {:?}",
            output.status.code()
        );
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut encoders = Vec::new();
    for line in stdout.lines() {
        // Skip the header / legend block until we hit the divider.
        // The actual encoder lines have leading whitespace + a single
        // letter (V/A/S) + 5 flag chars + space + name + spaces + desc.
        let trimmed = line.trim_start();
        // Video encoder lines look like " V..... libx264              ..."
        // or " V....D h264_nvenc           ..." (the flags vary; we
        // just need the leading V and the second token).
        if !trimmed.starts_with('V') || trimmed.len() < 7 {
            continue;
        }
        // Tokens after the flag word are the encoder name + description.
        let mut iter = trimmed.split_whitespace();
        let flags = iter.next().unwrap_or("");
        if !flags.starts_with('V') || flags.len() != 6 {
            // Header lines like "V..... = Video" also start with V but
            // have different shape — `=` shows up as a separate token.
            continue;
        }
        if let Some(name) = iter.next() {
            // Sanity: encoder names are kebab/underscore lowercase
            // ASCII identifiers. Reject anything that doesn't look the
            // part to avoid pulling in legend text.
            if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                encoders.push(name.to_string());
            }
        }
    }
    log::info!("ffmpeg available video encoders: {} found", encoders.len());
    encoders
}

#[cfg(test)]
mod encoder_probe_tests {
    use super::*;

    #[test]
    fn parses_typical_encoders_output() {
        // Synthetic snippet of `ffmpeg -encoders` output.
        let _sample = "\
Encoders:
 V..... = Video
 A..... = Audio
 S..... = Subtitle
 ------
 V..... libx264              libx264 H.264 / AVC / MPEG-4 AVC ...
 V....D h264_nvenc           NVIDIA NVENC H.264 encoder
 V....D hevc_videotoolbox    VideoToolbox H.265 Encoder
 A..... aac                  AAC (Advanced Audio Coding)
";
        // The probe function shells out to ffmpeg so we can't test it
        // directly without a binary; instead we replicate the parser
        // here. The intent is to verify our matching logic.
        let mut found = Vec::new();
        for line in _sample.lines() {
            let trimmed = line.trim_start();
            if !trimmed.starts_with('V') || trimmed.len() < 7 {
                continue;
            }
            let mut iter = trimmed.split_whitespace();
            let flags = iter.next().unwrap_or("");
            if !flags.starts_with('V') || flags.len() != 6 {
                continue;
            }
            if let Some(name) = iter.next() {
                if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                    found.push(name.to_string());
                }
            }
        }
        assert_eq!(found, vec!["libx264", "h264_nvenc", "hevc_videotoolbox"]);
    }
}
