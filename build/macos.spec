# PyInstaller spec for the macOS .app bundle.
#
# Driven by build/build.py — that script downloads the FFmpeg sidecar
# into build/.cache/ before invoking PyInstaller against this spec.
# Running pyinstaller directly against the spec works as long as
# build/.cache/ffmpeg exists.
#
# Usage (via the orchestrator):
#   python3 build/build.py
#
# Usage (direct):
#   pyinstaller build/macos.spec \
#     --workpath build/.work --distpath build/dist --noconfirm

# ruff: noqa
from pathlib import Path

# SPECPATH is injected by PyInstaller — equals the directory of this spec.
SPEC_DIR = Path(SPECPATH).resolve()
PROJECT_ROOT = SPEC_DIR.parent
CACHE = SPEC_DIR / ".cache"

APP_NAME = "ATEM IP Patchbay"
BUNDLE_ID = "org.weirdmachine.atem-ip-patchbay"
APP_VERSION = "0.1.0"

ffmpeg_sidecar = CACHE / "ffmpeg"
if not ffmpeg_sidecar.exists():
    raise SystemExit(
        f"ffmpeg sidecar not found at {ffmpeg_sidecar}. "
        f"Run `python3 build/build.py` to download it before invoking pyinstaller."
    )

a = Analysis(
    [str(PROJECT_ROOT / "run.py")],
    pathex=[str(PROJECT_ROOT)],
    # Sidecar binary — '.' places it next to the main executable in
    # the bundle (Contents/Frameworks/ for a .app, or the _MEIPASS
    # tempdir in onefile mode). The bmd_emulator.ffmpeg_path helper
    # resolves it via sys._MEIPASS.
    binaries=[(str(ffmpeg_sidecar), ".")],
    datas=[
        (str(PROJECT_ROOT / "bmd_emulator" / "static"), "bmd_emulator/static"),
        (str(PROJECT_ROOT / "config" / "example.xml"), "config"),
    ],
    # PyInstaller's static analysis catches top-level imports, but
    # several modules are imported lazily inside functions. List them
    # explicitly so they're sure to land in the bundle.
    hiddenimports=[
        "bmd_emulator",
        "bmd_emulator.device_scanner",
        "bmd_emulator.discover",
        "bmd_emulator.ffmpeg_path",
        "bmd_emulator.netinfo",
        "bmd_emulator.paste_parser",
        "bmd_emulator.protocol",
        "bmd_emulator.sources",
        "bmd_emulator.state",
        "bmd_emulator.streamer",
        "bmd_emulator.streamid",
        "bmd_emulator.web",
        "bmd_emulator.xml_loader",
    ],
    hookspath=[],
    runtime_hooks=[],
    excludes=[
        # Pure stdlib + a tiny http server — nothing pulls in tk/numpy/etc.
        "tkinter", "numpy", "scipy", "matplotlib", "PIL", "PySide2",
        "PySide6", "PyQt5", "PyQt6", "IPython",
    ],
    noarchive=False,
)

pyz = PYZ(a.pure, a.zipped_data)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name=APP_NAME,
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    console=False,           # windowed mode — no terminal popup
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,        # inherit the running interpreter's arch
    codesign_identity=None,  # we sign manually after-the-fact for clarity
    entitlements_file=None,
)

coll = COLLECT(
    exe,
    a.binaries,
    a.zipfiles,
    a.datas,
    strip=False,
    upx=False,
    name=APP_NAME,
)

app = BUNDLE(
    coll,
    name=f"{APP_NAME}.app",
    icon=None,               # TODO: ship a real icon
    bundle_identifier=BUNDLE_ID,
    info_plist={
        "CFBundleShortVersionString": APP_VERSION,
        "CFBundleVersion": APP_VERSION,
        "NSHighResolutionCapable": True,
        # AVFoundation device enumeration triggers a permission prompt
        # the first time. Provide a usage description so the OS shows
        # something readable instead of "ATEM IP Patchbay would like to…".
        "NSCameraUsageDescription":
            "ATEM IP Patchbay enumerates and captures from your video "
            "devices to forward them to your ATEM.",
        "NSMicrophoneUsageDescription":
            "ATEM IP Patchbay enumerates and captures audio devices to "
            "forward them to your ATEM.",
        "NSAppTransportSecurity": {
            "NSAllowsLocalNetworking": True,
        },
    },
)
