#!/bin/bash
# Double-click launcher for atem-net-diag.
# macOS opens .command files in Terminal automatically.

cd "$(dirname "$0")" || exit 1

# Clear macOS quarantine flag — files synced from iCloud / AirDrop /
# downloaded from the internet get tagged with com.apple.quarantine,
# which makes Gatekeeper challenge the binary on first run. Doing
# this once at launch lets the user double-click forever after.
xattr -dr com.apple.quarantine ./atem-net-diag 2>/dev/null

# Verify ffmpeg is on PATH; the active probe shells out to it.
if ! command -v ffmpeg >/dev/null 2>&1; then
  cat <<'WARN'

⚠  ffmpeg not found in PATH.

The active probe (handshake against your ATEM) needs ffmpeg with
libsrt support. Install via Homebrew (https://brew.sh):

    brew install ffmpeg

The dashboard / monitor mode will still launch, but the active probe
will be skipped until ffmpeg is available.

WARN
fi

# Launch with the visual dashboard. Browser auto-opens to localhost.
# Pass any additional CLI flags through (so power users can override).
echo "Starting atem-net-diag dashboard…"
echo "(Ctrl-C in this window to stop. Browser will auto-open.)"
echo
exec ./atem-net-diag --ui "$@"
