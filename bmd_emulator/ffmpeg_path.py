"""Resolve the ffmpeg executable to invoke.

When running from source (``python3 run.py``) we just want the user's
PATH ffmpeg — they installed it via Homebrew or apt or chocolatey.

When running inside a packaged app (PyInstaller bundle), we want the
sidecar binary that ships inside the bundle so the app works on
machines without ffmpeg installed.

A debug override (``ATEM_PATCHBAY_FFMPEG``) lets us point at a
specific binary regardless — useful for testing a different ffmpeg
build against a packaged app without rebuilding.
"""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path


def ffmpeg_path() -> str:
    """Return the path to the ffmpeg executable to invoke.

    Resolution order:
      1. ``ATEM_PATCHBAY_FFMPEG`` env var (override).
      2. Sidecar inside the PyInstaller bundle (``sys._MEIPASS/ffmpeg``).
      3. ``shutil.which("ffmpeg")`` — first ffmpeg on PATH.
      4. Bare ``"ffmpeg"`` — lets the call site's subprocess raise
         ``FileNotFoundError`` so the error surfaces at the right layer.
    """
    override = os.environ.get("ATEM_PATCHBAY_FFMPEG")
    if override and Path(override).exists():
        return override

    # PyInstaller sets sys._MEIPASS to the unpacked-bundle root; in a
    # macOS .app this is Contents/Frameworks, in onefile mode it's a
    # tempdir created at launch.
    bundle_root = getattr(sys, "_MEIPASS", None)
    if bundle_root:
        suffix = ".exe" if sys.platform.startswith("win") else ""
        candidate = Path(bundle_root) / f"ffmpeg{suffix}"
        if candidate.exists():
            return str(candidate)

    found = shutil.which("ffmpeg")
    return found or "ffmpeg"
