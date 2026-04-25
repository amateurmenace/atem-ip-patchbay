# PyInstaller spec for the Windows .exe build.
#
# Driven by build/build.py — that script downloads the FFmpeg sidecar
# into build/.cache/ before invoking PyInstaller against this spec.
# Running pyinstaller directly works as long as build/.cache/ffmpeg.exe
# exists.
#
# Usage (via the orchestrator):
#   python build\build.py
#
# Usage (direct):
#   pyinstaller build\windows.spec ^
#     --workpath build\.work --distpath build\dist --noconfirm
#
# Windows uses --onefile mode: the produced .exe is a self-extracting
# bundle that drops Python + dependencies into a temp dir on first run
# and then re-execs itself. Slower first-launch than .app/Contents,
# but a single file is simpler to install + ship.

# ruff: noqa
from pathlib import Path

SPEC_DIR = Path(SPECPATH).resolve()
PROJECT_ROOT = SPEC_DIR.parent
CACHE = SPEC_DIR / ".cache"

APP_NAME = "ATEM IP Patchbay"
APP_VERSION = "0.1.0"

ffmpeg_sidecar = CACHE / "ffmpeg.exe"
if not ffmpeg_sidecar.exists():
    raise SystemExit(
        f"ffmpeg sidecar not found at {ffmpeg_sidecar}. "
        f"Run `python build\\build.py` to download it before invoking pyinstaller."
    )

a = Analysis(
    [str(PROJECT_ROOT / "run.py")],
    pathex=[str(PROJECT_ROOT)],
    # Sidecar binary — '.' places it at the root of the unpacked
    # bundle (sys._MEIPASS at runtime), where bmd_emulator.ffmpeg_path
    # looks for it.
    binaries=[(str(ffmpeg_sidecar), ".")],
    datas=[
        (str(PROJECT_ROOT / "bmd_emulator" / "static"), "bmd_emulator/static"),
        (str(PROJECT_ROOT / "config" / "example.xml"), "config"),
    ],
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
        "tkinter", "numpy", "scipy", "matplotlib", "PIL", "PySide2",
        "PySide6", "PyQt5", "PyQt6", "IPython",
    ],
    noarchive=False,
)

pyz = PYZ(a.pure, a.zipped_data)

# --onefile: bundle everything into a single .exe that self-extracts.
# Slower cold start than --onedir, but a single file is simpler to
# distribute via Inno Setup.
exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.zipfiles,
    a.datas,
    [],
    name=APP_NAME,
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,             # UPX often trips Windows Defender — leave off
    runtime_tmpdir=None,
    console=False,         # windowed (no terminal) — same UX as the .app
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon=None,             # TODO: ship a .ico once artwork exists
    version=None,          # TODO: optional Windows VERSIONINFO
)
