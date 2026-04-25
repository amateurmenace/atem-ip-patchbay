#!/usr/bin/env python3
"""Build ATEM IP Patchbay for the current platform.

Steps (macOS):
  1. Create build/.venv if missing, install pinned PyInstaller into it.
  2. Download a static FFmpeg into build/.cache/ffmpeg.
  3. Run pyinstaller against build/macos.spec.
  4. Sign the .app with the configured Developer ID identity.
  5. Package the .app into a .dmg via create-dmg.

The orchestrator is intentionally Make-style — sequential steps, no
build system to learn, no monorepo tooling. Each step skips its work
if its output already exists, so re-runs are cheap. Pass --clean to
nuke the build venv, ffmpeg cache, and dist output before starting.

Run from the project root:

    python3 build/build.py [--clean] [--skip-sign] [--no-dmg]

The Windows path will land here once the dshow source layer has been
exercised on a real Windows box (or via the GH Actions matrix).
"""

from __future__ import annotations

import argparse
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
CACHE = BUILD / ".cache"
VENV = BUILD / ".venv"
WORK = BUILD / ".work"
DIST = BUILD / "dist"

APP_NAME = "ATEM IP Patchbay"
APP_VERSION = "0.1.0"

PINNED_PYINSTALLER = "6.10.0"

# Jellyfin ships portable static FFmpeg builds for macOS that include
# libsrt — required for our SRT push to ATEM. Evermeet's and OSXExperts'
# builds were both rejected after checking: neither had --enable-libsrt.
# Jellyfin's build is GPL (so are the bundled x264/x265), which is fine
# for an MIT-licensed app distributing FFmpeg as a separate executable
# rather than linking it. We ship a NOTICE file pointing at Jellyfin's
# source so downstream users can comply with the GPL pass-through.
JELLYFIN_FFMPEG_VERSION = "7.1.3-5"
JELLYFIN_FFMPEG_ARM64 = (
    f"https://github.com/jellyfin/jellyfin-ffmpeg/releases/download/"
    f"v{JELLYFIN_FFMPEG_VERSION}/"
    f"jellyfin-ffmpeg_{JELLYFIN_FFMPEG_VERSION}_portable_macarm64-gpl.tar.xz"
)

# BtbN ships static FFmpeg builds for Windows including libsrt. We pull
# the n8.1 (FFmpeg 8.1) GPL static build — single ffmpeg.exe, no DLLs
# to wrangle. Roughly 220 MB zipped, ~270 MB extracted; PyInstaller's
# --onefile mode then re-compresses on top of that for the installer.
BTBN_FFMPEG_VERSION = "n8.1"
BTBN_FFMPEG_WIN64 = (
    f"https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/"
    f"ffmpeg-{BTBN_FFMPEG_VERSION}-latest-win64-gpl-8.1.zip"
)

# Code-signing identity. Populated from the SIGN_IDENTITY env var or
# auto-detected via `security find-identity` when --skip-sign is not
# passed. Override examples:
#   SIGN_IDENTITY="Developer ID Application: Stephen Walter (6M536MV7GT)"
#   SIGN_IDENTITY=6M536MV7GT
DEFAULT_IDENTITY_HINT = "Developer ID Application"


# ---------------------------------------------------------------------------
# Tiny shell helpers
# ---------------------------------------------------------------------------


def log(msg: str) -> None:
    print(f"\033[1;36m▶\033[0m {msg}", flush=True)


def warn(msg: str) -> None:
    print(f"\033[1;33m!\033[0m {msg}", flush=True)


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    print(f"  $ {' '.join(str(c) for c in cmd)}", flush=True)
    return subprocess.run(cmd, check=True, **kw)


def out(cmd: list[str]) -> str:
    return subprocess.check_output(cmd, text=True).strip()


# ---------------------------------------------------------------------------
# Step 1 — venv with pinned PyInstaller
# ---------------------------------------------------------------------------


def ensure_venv() -> Path:
    """Create build/.venv if missing and install PyInstaller into it.

    Returns the path to the venv's python interpreter.
    """
    venv_python = VENV / "bin" / "python"
    if not venv_python.exists():
        log(f"Creating build venv at {VENV.relative_to(ROOT)}")
        run([sys.executable, "-m", "venv", str(VENV)])

    # Idempotent: pip install with the exact pinned version is a no-op
    # if it's already there.
    log(f"Ensuring PyInstaller=={PINNED_PYINSTALLER} in build venv")
    run([
        str(venv_python), "-m", "pip", "install", "--quiet",
        "--upgrade", "pip",
    ])
    run([
        str(venv_python), "-m", "pip", "install", "--quiet",
        f"pyinstaller=={PINNED_PYINSTALLER}",
    ])
    return venv_python


# ---------------------------------------------------------------------------
# Step 2 — FFmpeg sidecar
# ---------------------------------------------------------------------------


def ensure_ffmpeg_macos() -> Path:
    """Download Jellyfin's static FFmpeg arm64 build into build/.cache.

    Returns the path to the executable. Caches across builds — delete
    the cache or pass --clean to refresh. Verifies libsrt is present
    in the downloaded binary; aborts the build if it isn't.

    Universal2 (arm64 + x86_64 lipo'd together) is left for the GH
    Actions phase. Local builds are arm64-only since the dev machine
    is Apple Silicon.
    """
    CACHE.mkdir(parents=True, exist_ok=True)
    binary = CACHE / "ffmpeg"
    version_file = CACHE / "ffmpeg.version"

    if binary.exists() and version_file.exists() and version_file.read_text().strip() == JELLYFIN_FFMPEG_VERSION:
        log(f"FFmpeg sidecar already present (version: {JELLYFIN_FFMPEG_VERSION})")
        return binary

    # Cache miss or version mismatch → fresh download.
    log(f"Downloading jellyfin-ffmpeg {JELLYFIN_FFMPEG_VERSION} (arm64) for libsrt support")
    tmp_path = Path(tempfile.mkdtemp()) / "ffmpeg.tar.xz"
    # urllib's urlopen choked on GitHub's release-asset redirect mid-stream.
    # Curl follows redirects, retries on transient failures, and surfaces
    # SSL/network errors clearly — exactly what we want for a build.
    run([
        "curl", "-fsSL", "--retry", "3", "--retry-delay", "2",
        "-o", str(tmp_path),
        JELLYFIN_FFMPEG_ARM64,
    ])
    if tmp_path.stat().st_size < 1_000_000:
        raise SystemExit(f"Downloaded ffmpeg tarball is suspiciously small ({tmp_path.stat().st_size} bytes)")

    log("Extracting ffmpeg from tarball")
    with tarfile.open(tmp_path, "r:xz") as t:
        members = [m for m in t.getmembers() if m.name.endswith("ffmpeg") and m.isfile()]
        if not members:
            raise SystemExit("No ffmpeg binary inside the jellyfin-ffmpeg tarball")
        m = members[0]
        with t.extractfile(m) as src, binary.open("wb") as dst:
            shutil.copyfileobj(src, dst)
    binary.chmod(0o755)
    tmp_path.unlink()

    # Sanity-check the binary: must be arm64 and must have libsrt.
    file_info = out(["file", str(binary)])
    if "arm64" not in file_info:
        raise SystemExit(f"Downloaded ffmpeg is not arm64: {file_info}")
    proto_check = subprocess.run(
        [str(binary), "-protocols"], capture_output=True, text=True
    )
    if not any(line.strip() == "srt" for line in proto_check.stdout.splitlines()):
        raise SystemExit(
            "Downloaded ffmpeg lacks libsrt support. The Jellyfin URL or "
            "build flags may have changed; check the release notes at "
            "https://github.com/jellyfin/jellyfin-ffmpeg/releases"
        )

    version_file.write_text(JELLYFIN_FFMPEG_VERSION)
    log(f"FFmpeg ready at {binary.relative_to(ROOT)}")
    print(f"    {file_info}")
    print(f"    libsrt: ✓")
    return binary


# ---------------------------------------------------------------------------
# Step 2b — FFmpeg sidecar (Windows path)
# ---------------------------------------------------------------------------


def ensure_ffmpeg_windows() -> Path:
    """Download BtbN's static Windows FFmpeg into build/.cache.

    Returns the path to ffmpeg.exe. Verifies libsrt is present before
    accepting the download — same belt-and-braces check we do on Mac.
    """
    CACHE.mkdir(parents=True, exist_ok=True)
    binary = CACHE / "ffmpeg.exe"
    version_file = CACHE / "ffmpeg.version"

    if binary.exists() and version_file.exists() and version_file.read_text().strip() == BTBN_FFMPEG_VERSION:
        log(f"FFmpeg sidecar already present (version: {BTBN_FFMPEG_VERSION})")
        return binary

    log(f"Downloading BtbN FFmpeg {BTBN_FFMPEG_VERSION} (win64-gpl) for libsrt support")
    tmp_zip = Path(tempfile.mkdtemp()) / "ffmpeg.zip"
    run([
        "curl", "-fsSL", "--retry", "3", "--retry-delay", "2",
        "-o", str(tmp_zip),
        BTBN_FFMPEG_WIN64,
    ])
    if tmp_zip.stat().st_size < 50_000_000:
        raise SystemExit(f"Downloaded ffmpeg zip is suspiciously small ({tmp_zip.stat().st_size} bytes)")

    log("Extracting ffmpeg.exe from zip")
    import zipfile
    with zipfile.ZipFile(tmp_zip) as z:
        # BtbN zips look like: ffmpeg-n8.1-latest-win64-gpl-8.1/bin/ffmpeg.exe
        members = [m for m in z.namelist() if m.endswith("bin/ffmpeg.exe")]
        if not members:
            raise SystemExit("No bin/ffmpeg.exe inside the BtbN zip")
        with z.open(members[0]) as src, binary.open("wb") as dst:
            shutil.copyfileobj(src, dst)
    tmp_zip.unlink()

    # Sanity check — running ffmpeg.exe -protocols on Windows confirms libsrt
    # works. On non-Windows platforms (e.g. running this code under WSL or
    # a CI setup matrix) we just trust the BtbN build flags.
    if sys.platform.startswith("win"):
        proto_check = subprocess.run(
            [str(binary), "-protocols"], capture_output=True, text=True
        )
        if not any(line.strip() == "srt" for line in proto_check.stdout.splitlines()):
            raise SystemExit(
                "Downloaded ffmpeg.exe lacks libsrt support. The BtbN URL or "
                "build flags may have changed; check the release notes at "
                "https://github.com/BtbN/FFmpeg-Builds/releases"
            )

    version_file.write_text(BTBN_FFMPEG_VERSION)
    log(f"FFmpeg ready at {binary.relative_to(ROOT)}")
    print(f"    {binary.stat().st_size // (1024 * 1024)} MB")
    return binary


# ---------------------------------------------------------------------------
# Step 3 — PyInstaller
# ---------------------------------------------------------------------------


def run_pyinstaller(venv_python: Path, spec_path: Path, expected_output: Path) -> Path:
    """Run pyinstaller against the spec; return the path to the produced
    bundle (a .app on macOS or .exe on Windows)."""
    log(f"Running PyInstaller against {spec_path.relative_to(ROOT)}")
    run([
        str(venv_python), "-m", "PyInstaller",
        str(spec_path),
        "--noconfirm",
        "--workpath", str(WORK),
        "--distpath", str(DIST),
    ])
    if not expected_output.exists():
        raise SystemExit(f"PyInstaller did not produce {expected_output}")
    return expected_output


# ---------------------------------------------------------------------------
# Step 4 — code signing
# ---------------------------------------------------------------------------


def find_signing_identity() -> str | None:
    """Return the first Developer ID Application identity in the keychain.

    Honors the SIGN_IDENTITY env var verbatim if set.
    """
    override = os.environ.get("SIGN_IDENTITY")
    if override:
        return override
    try:
        listing = out(["security", "find-identity", "-v", "-p", "codesigning"])
    except subprocess.CalledProcessError:
        return None
    for line in listing.splitlines():
        if DEFAULT_IDENTITY_HINT in line and "valid" not in line.lower():
            # lines look like: '  2) HASH "Developer ID Application: Name (TEAMID)"'
            quoted = line.split('"', 1)
            if len(quoted) > 1:
                return quoted[1].rstrip('"')
    return None


def sign_app(app_path: Path, identity: str) -> None:
    """Sign the .app and every nested binary with the given identity.

    Hardened Runtime is intentionally NOT enabled here — that's
    required for notarization, not for plain Developer ID signing.
    When we add notarization in a follow-up, flip the --options
    runtime flag on and provide an entitlements file.
    """
    log(f"Code-signing with identity: {identity}")
    # --deep recurses into the bundle, signing every Mach-O binary
    # (including the bundled ffmpeg sidecar).
    run([
        "codesign",
        "--force",
        "--deep",
        "--sign", identity,
        "--timestamp",
        str(app_path),
    ])
    # Quick verify so we fail loud here instead of at first launch.
    run(["codesign", "--verify", "--verbose=2", str(app_path)])


# ---------------------------------------------------------------------------
# Step 5 — .dmg packaging
# ---------------------------------------------------------------------------


def make_dmg(app_path: Path) -> Path:
    """Wrap the .app in a .dmg with a drag-to-/Applications layout."""
    if not shutil.which("create-dmg"):
        warn("create-dmg not found on PATH — skipping .dmg packaging.")
        warn("Install with: brew install create-dmg")
        return app_path
    dmg_path = DIST / f"ATEM-IP-Patchbay-{APP_VERSION}-arm64.dmg"
    if dmg_path.exists():
        dmg_path.unlink()
    log(f"Packaging into {dmg_path.relative_to(ROOT)}")
    run([
        "create-dmg",
        "--volname", APP_NAME,
        "--window-size", "540", "320",
        "--icon-size", "96",
        "--icon", f"{APP_NAME}.app", "140", "150",
        "--app-drop-link", "400", "150",
        "--no-internet-enable",
        str(dmg_path),
        str(app_path),
    ])
    return dmg_path


# ---------------------------------------------------------------------------
# Step 5 (Windows) — Inno Setup installer
# ---------------------------------------------------------------------------


def make_inno_installer(exe_path: Path) -> Path:
    """Compile build/installer.iss into ATEM-IP-Patchbay-Setup-x64.exe.

    Inno Setup ships its compiler as ``ISCC.exe``; on a default install
    it lives at ``C:\\Program Files (x86)\\Inno Setup 6\\ISCC.exe``.
    Honors the ``ISCC`` env var if you've put it somewhere else.
    """
    iscc = os.environ.get("ISCC") or shutil.which("ISCC") or shutil.which("iscc")
    if not iscc:
        # Fall back to the default install location.
        default = Path("C:/Program Files (x86)/Inno Setup 6/ISCC.exe")
        if default.exists():
            iscc = str(default)
    if not iscc:
        warn(
            "ISCC.exe (Inno Setup compiler) not found. Skipping installer.\n"
            "    Install Inno Setup 6 from https://jrsoftware.org/isdl.php "
            "or set the ISCC env var to its path."
        )
        return exe_path

    iss_script = BUILD / "installer.iss"
    log(f"Compiling Inno Setup installer via {iscc}")
    run([
        str(iscc),
        f"/DAppVersion={APP_VERSION}",
        f"/DAppExePath={exe_path}",
        f"/DOutputDir={DIST}",
        str(iss_script),
    ])
    installer = DIST / f"ATEM-IP-Patchbay-Setup-{APP_VERSION}-x64.exe"
    if not installer.exists():
        raise SystemExit(f"Inno Setup did not produce {installer}")
    return installer


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--clean", action="store_true",
        help="Wipe build venv, ffmpeg cache, and PyInstaller output before building",
    )
    parser.add_argument(
        "--skip-sign", action="store_true",
        help="Don't code-sign the macOS .app (no-op on Windows for now)",
    )
    parser.add_argument(
        "--no-dmg", action="store_true",
        help="Skip the final packaging step — stop at the .app or .exe",
    )
    args = parser.parse_args()

    if args.clean:
        for d in (VENV, CACHE, WORK, DIST):
            if d.exists():
                log(f"Removing {d.relative_to(ROOT)}")
                shutil.rmtree(d)

    venv_python = ensure_venv()

    if sys.platform == "darwin":
        ensure_ffmpeg_macos()
        spec = BUILD / "macos.spec"
        expected = DIST / f"{APP_NAME}.app"
        app_path = run_pyinstaller(venv_python, spec, expected)

        if not args.skip_sign:
            identity = find_signing_identity()
            if not identity:
                warn(
                    "No Developer ID Application identity found and SIGN_IDENTITY not set. "
                    "Build will be unsigned — first launch will trigger Gatekeeper."
                )
            else:
                sign_app(app_path, identity)

        final = app_path if args.no_dmg else make_dmg(app_path)

    elif sys.platform.startswith("win"):
        ensure_ffmpeg_windows()
        spec = BUILD / "windows.spec"
        expected = DIST / f"{APP_NAME}.exe"
        exe_path = run_pyinstaller(venv_python, spec, expected)
        # No code-signing for Windows in the alpha — that needs an EV cert
        # which the user hasn't budgeted for yet. The Inno installer step
        # is the Windows equivalent of the macOS .dmg packaging step.
        final = exe_path if args.no_dmg else make_inno_installer(exe_path)

    else:
        raise SystemExit(
            f"This builder handles macOS and Windows; you're on {platform.system()}. "
            "Linux support depends on adding v4l2/X11grab source factories first."
        )

    log(f"Done.\n  → {final.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
