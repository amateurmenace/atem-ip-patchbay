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

# Check for ffmpeg — only needed for Standby mode active probes.
if ! command -v ffmpeg >/dev/null 2>&1; then
  cat <<'WARN'

⚠  ffmpeg not found in PATH.

The active probe (Standby mode handshake against your ATEM) needs
ffmpeg with libsrt support. Install via Homebrew (https://brew.sh):

    brew install ffmpeg

Live mode (default) doesn't need ffmpeg — the dashboard will launch
fine and you can use UDM polling + tshark capture without it.

WARN
fi

# Check for tshark — needed for the CAPTURE data source. Looked for
# at the standard paths the binary itself searches.
if [ ! -x /Applications/Wireshark.app/Contents/MacOS/tshark ] && \
   [ ! -x /usr/local/bin/tshark ] && \
   [ ! -x /opt/homebrew/bin/tshark ]; then
  cat <<'WARN'

⚠  tshark not found.

The CAPTURE data source (per-flow visibility, stream-key correlation)
needs tshark. Install Wireshark via Homebrew (includes the ChmodBPF
helper that grants packet-capture permission):

    brew install --cask wireshark

UDM polling (the headline data source in v0.2.0) doesn't need tshark.

WARN
fi

# Prompt for UDM_API_KEY if not already set in the environment. The
# key is the only credential — never written to disk by this script,
# only passed through to the binary via env. To make it persistent,
# add `export UDM_API_KEY=...` to your ~/.zshrc.
if [ -z "$UDM_API_KEY" ] && [ -z "$UDM_USERNAME" ]; then
  cat <<'PROMPT'

UDM controller credentials are not set in your environment.

The UDM polling data source (the headline feature in v0.2.0) needs
either a Local Controller API key (preferred) or a local-account
username + password.

Create a Local Controller API key in your UDM web UI:
  Settings → Control Plane → Integrations → Create API Key

Paste the key here to launch with UDM polling enabled (or just press
Enter to skip and run with capture-only / probe-only modes):

PROMPT
  read -r -p "UDM_API_KEY: " entered_key
  if [ -n "$entered_key" ]; then
    export UDM_API_KEY="$entered_key"
    echo
    echo "✓ UDM_API_KEY set for this session."
    echo "  (To make it persistent, add 'export UDM_API_KEY=...' to ~/.zshrc.)"
    echo
  else
    echo
    echo "→ No key entered. Launching without UDM polling."
    echo "  Set UDM_API_KEY in your shell and re-run start.command to enable it."
    echo
  fi
fi

URL="http://127.0.0.1:8092/"

cat <<HEADER

==================================================================
  atem-net-diag dashboard  ·  v0.2.0

  Default mode: LIVE (passive monitoring, no outbound to ATEM)

  Open this URL in your browser:

    $URL

  (We'll also try to auto-open it in 2 seconds.)

  Configure the ATEM target / UDM host in the dashboard's top
  panels. Switch to STANDBY mode in the banner to enable active
  probes against the configured target.

  Press Ctrl-C in this window to stop the dashboard.
==================================================================

HEADER

# Auto-open the browser after a short delay so the URL on screen has
# a moment to register with the user (and gives the HTTP server time
# to bind the port). Backgrounded so it doesn't block the binary.
( sleep 2 && open "$URL" ) &

# Run the binary in foreground. UDM_API_KEY (and friends) inherit
# from this script's exported environment. When the user hits Ctrl-C,
# the binary exits cleanly; the trailing prompt keeps the terminal
# open so any error output is visible.
./atem-net-diag --ui "$@"
RC=$?

echo
echo "atem-net-diag exited with code $RC"
read -p "Press Enter to close this Terminal window..." _
