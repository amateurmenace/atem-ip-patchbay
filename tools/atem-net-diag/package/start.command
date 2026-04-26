#!/bin/bash
# Double-click launcher for atem-net-diag.
# macOS opens .command files in Terminal automatically.

cd "$(dirname "$0")" || exit 1

# Clear macOS quarantine flag — files synced from iCloud / AirDrop /
# downloaded from the internet get tagged with com.apple.quarantine,
# which makes Gatekeeper challenge the binary on first run. Doing
# this once at launch lets the user double-click forever after.
xattr -dr com.apple.quarantine ./atem-net-diag 2>/dev/null

if [ ! -x ./atem-net-diag ]; then
  echo
  echo "ERROR: ./atem-net-diag not found or not executable in $(pwd)"
  echo "Make sure you double-clicked start.command from inside the unpacked"
  echo "atem-net-diag-*-macos-arm64 folder."
  echo
  read -p "Press Enter to close..." _
  exit 1
fi

# Verify ffmpeg is on PATH; the active probe shells out to it.
if ! command -v ffmpeg >/dev/null 2>&1; then
  cat <<'WARN'

⚠  ffmpeg not found in PATH.

The active probe (handshake against your ATEM) needs ffmpeg with
libsrt support. Install via Homebrew (https://brew.sh):

    brew install ffmpeg

The dashboard will still launch — you can use it to test the network,
but the active probe will be skipped until ffmpeg is available.

WARN
fi

URL="http://127.0.0.1:8092/"

cat <<HEADER

==================================================================
  atem-net-diag dashboard

  Open this URL in your browser:

    $URL

  (We'll also try to auto-open it in 2 seconds.)

  Configure the ATEM IP / port / stream key in the form at the top
  of the dashboard, then click Apply. The probe loop reconfigures
  live.

  Press Ctrl-C in this window to stop the dashboard.
==================================================================

HEADER

# Auto-open the browser after a short delay so the URL on screen has
# a moment to register with the user (and gives the HTTP server time
# to bind the port). Backgrounded so it doesn't block the binary.
( sleep 2 && open "$URL" ) &

# Run the binary in foreground. When the user hits Ctrl-C, the binary
# exits cleanly; the trailing prompt keeps the terminal open so any
# error output is visible.
./atem-net-diag --ui "$@"
RC=$?

echo
echo "atem-net-diag exited with code $RC"
read -p "Press Enter to close this Terminal window..." _
