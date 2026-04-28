#!/usr/bin/env bash
# Reproducible packaging script for atem-net-diag.
#
# Builds the release binary, signs it (Developer ID + hardened runtime
# when available; ad-hoc otherwise — Apple Silicon requires at least
# ad-hoc), strips, and produces THREE distributable artifacts under
# dist/:
#
#   1. The classic tarball (folder of binary + start.command + README)
#      for command-line users:
#        dist/atem-net-diag-<version>-macos-arm64.tar.gz
#
#   2. A self-contained .app bundle for double-click launch:
#        dist/atem-net-diag.app/
#      Drag to /Applications, double-click → opens Terminal running
#      the dashboard, browser auto-pops to http://127.0.0.1:8092/.
#
#   3. A zip of the .app for AirDrop / iCloud sync to other Macs:
#        dist/atem-net-diag-<version>-macos-arm64.app.zip
#
# Run from anywhere — the script cd's to its own dir first.
#
# Usage:
#   ./build-package.sh                # build all three artifacts
#   ./build-package.sh --copy-icloud  # also copy tarball + .app.zip
#                                     # to iCloud Drive root

set -euo pipefail

cd "$(dirname "$0")"

VERSION=$(grep -E '^version = "' Cargo.toml | head -1 | sed -E 's/version = "([^"]+)"/\1/')
ARCH="macos-arm64"
PKG_NAME="atem-net-diag-${VERSION}-${ARCH}"
DIST_DIR="dist"
PKG_DIR="${DIST_DIR}/${PKG_NAME}"
TARBALL="${DIST_DIR}/${PKG_NAME}.tar.gz"
IDENTIFIER="org.weirdmachine.atem-net-diag"
DEV_ID_HINT="Developer ID Application: Stephen Walter (6M536MV7GT)"

echo "==> Building atem-net-diag v${VERSION} for ${ARCH}"
cargo build --release --quiet

BIN="target/release/atem-net-diag"
if [[ ! -x "$BIN" ]]; then
  echo "ERROR: build did not produce $BIN" >&2
  exit 1
fi

echo "==> Stripping symbols"
strip "$BIN"

echo "==> Codesigning"
if security find-identity -v -p codesigning 2>/dev/null | grep -q "$DEV_ID_HINT"; then
  echo "   Found Developer ID identity — using hardened runtime signing."
  codesign --force --options runtime --timestamp \
    --sign "$DEV_ID_HINT" \
    --identifier "$IDENTIFIER" \
    "$BIN"
else
  echo "   No Developer ID identity in keychain — ad-hoc signing."
  echo "   (Fine for your own Macs. For distribution outside, build on the dev Mac.)"
  codesign --force --sign - --identifier "$IDENTIFIER" "$BIN"
fi

echo "==> Assembling package directory: ${PKG_DIR}"
rm -rf "$PKG_DIR"
mkdir -p "$PKG_DIR"
cp "$BIN" "$PKG_DIR/atem-net-diag"
cp package/start.command "$PKG_DIR/start.command"
cp package/README.txt "$PKG_DIR/README.txt"
chmod +x "$PKG_DIR/start.command" "$PKG_DIR/atem-net-diag"

echo "==> Creating tarball: ${TARBALL}"
# Use COPYFILE_DISABLE so macOS doesn't smuggle ._* AppleDouble files
# into the tarball (annoying when extracted on Linux / shows up as
# extra files for the operator).
COPYFILE_DISABLE=1 tar -czf "$TARBALL" -C "$DIST_DIR" "$PKG_NAME"

TARBALL_SIZE=$(du -h "$TARBALL" | cut -f1)
echo "==> Tarball: ${TARBALL_SIZE} at ${TARBALL}"

# ---- Self-contained .app bundle ------------------------------------------
#
# Standard Mac .app directory structure. Double-clicking the .app runs
# Contents/MacOS/launcher (a small shell script) which uses osascript
# to spawn a Terminal window running start.command from Resources/.
# The operator gets the same log-visible UX as the tarball but launches
# from a single drag-to-Applications icon.

APP_DIR="${DIST_DIR}/atem-net-diag.app"
APP_ZIP="${DIST_DIR}/${PKG_NAME}.app.zip"

echo "==> Assembling .app bundle: ${APP_DIR}"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"

# Resources: binary + start.command + README. start.command's
# `cd "$(dirname "$0")"` resolves to Resources/ at runtime, so the
# binary lives next to it as expected.
cp "$BIN" "$APP_DIR/Contents/Resources/atem-net-diag"
cp package/start.command "$APP_DIR/Contents/Resources/start.command"
cp package/README.txt "$APP_DIR/Contents/Resources/README.txt"
chmod +x "$APP_DIR/Contents/Resources/atem-net-diag" "$APP_DIR/Contents/Resources/start.command"

# Info.plist — minimal but complete. CFBundleExecutable points at our
# launcher script in MacOS/. LSMinimumSystemVersion 11.0 matches the
# binary's macOS-arm64 target.
cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>${IDENTIFIER}</string>
    <key>CFBundleName</key>
    <string>atem-net-diag</string>
    <key>CFBundleDisplayName</key>
    <string>ATEM Net Diag</string>
    <key>CFBundleExecutable</key>
    <string>launcher</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>LSUIElement</key>
    <false/>
</dict>
</plist>
PLIST

# MacOS/launcher — the executable Finder runs when the .app is
# double-clicked. Resolves the Resources/ path relative to itself
# and spawns a Terminal window running start.command. Uses osascript
# because Finder doesn't give us a Terminal by default for a .app.
cat > "$APP_DIR/Contents/MacOS/launcher" <<'LAUNCHER'
#!/bin/bash
# atem-net-diag self-contained .app launcher. Double-clicked by Finder
# (no controlling terminal). Spawns a Terminal window running the
# diagnostic binary so the operator sees the same log output as the
# tarball workflow.

set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RES_DIR="$(cd "$SCRIPT_DIR/../Resources" && pwd)"

# Strip the macOS quarantine attribute from the binary on first launch.
# Files inside an .app downloaded from iCloud / AirDrop / the internet
# get tagged with com.apple.quarantine, which causes Gatekeeper to
# challenge each invocation. Clearing once at first launch (the user
# already approved the parent .app via right-click → Open) means future
# launches Just Work.
xattr -dr com.apple.quarantine "$RES_DIR/atem-net-diag" 2>/dev/null || true

# Spawn Terminal running start.command. AppleScript-quote the Resources
# path so spaces / unicode in the install directory don't break it.
RES_QUOTED="${RES_DIR//\"/\\\"}"
/usr/bin/osascript <<APPLESCRIPT
tell application "Terminal"
    activate
    do script "cd \"${RES_QUOTED}\" && ./start.command"
end tell
APPLESCRIPT
LAUNCHER
chmod +x "$APP_DIR/Contents/MacOS/launcher"

echo "==> Codesigning .app bundle"
# Sign nested executables first, then the bundle as a whole. Newer
# codesign deprecates --deep; explicit per-target signing avoids the
# warning and is the modern recommendation. For ad-hoc we don't pass
# --options runtime; that flag is hardened-runtime-only.
if security find-identity -v -p codesigning 2>/dev/null | grep -q "$DEV_ID_HINT"; then
  codesign --force --options runtime --timestamp --sign "$DEV_ID_HINT" \
    "$APP_DIR/Contents/Resources/atem-net-diag"
  codesign --force --options runtime --timestamp --sign "$DEV_ID_HINT" \
    "$APP_DIR/Contents/MacOS/launcher"
  codesign --force --options runtime --timestamp --sign "$DEV_ID_HINT" \
    --identifier "$IDENTIFIER" "$APP_DIR"
else
  codesign --force --sign - "$APP_DIR/Contents/Resources/atem-net-diag"
  codesign --force --sign - "$APP_DIR/Contents/MacOS/launcher"
  codesign --force --sign - --identifier "$IDENTIFIER" "$APP_DIR"
fi

# Verify bundle structure.
codesign --verify --verbose=2 "$APP_DIR" 2>&1 | tail -3 || true

echo "==> Zipping .app for distribution: ${APP_ZIP}"
# `ditto -c -k --keepParent` is the macOS-recommended way to zip a
# .app — preserves resource forks, extended attributes, and code
# signatures correctly (regular `zip` mangles them).
rm -f "$APP_ZIP"
ditto -c -k --keepParent "$APP_DIR" "$APP_ZIP"

APP_SIZE=$(du -h "$APP_ZIP" | cut -f1)
echo "==> .app.zip: ${APP_SIZE} at ${APP_ZIP}"

# ---- Optional: copy artifacts to iCloud Drive ----------------------------

if [[ "${1:-}" == "--copy-icloud" ]]; then
  ICLOUD_ROOT="$HOME/Library/Mobile Documents/com~apple~CloudDocs"
  if [[ ! -d "$ICLOUD_ROOT" ]]; then
    echo "WARN: iCloud Drive not found at $ICLOUD_ROOT — skipping copy."
  else
    cp "$TARBALL" "$ICLOUD_ROOT/"
    cp "$APP_ZIP" "$ICLOUD_ROOT/"
    echo "==> Copied to iCloud:"
    echo "    $ICLOUD_ROOT/${PKG_NAME}.tar.gz"
    echo "    $ICLOUD_ROOT/${PKG_NAME}.app.zip"
  fi
fi

echo
echo "Install on another Mac (recommended .app path):"
echo "  1. iCloud or AirDrop the .app.zip over."
echo "  2. Double-click the .zip — extracts atem-net-diag.app"
echo "  3. Drag atem-net-diag.app to /Applications (or run from anywhere)."
echo "  4. RIGHT-CLICK the .app → Open (first time only — Gatekeeper challenges"
echo "     ad-hoc-signed apps). Click Open in the dialog."
echo "  5. Terminal opens. Paste your UDM_API_KEY when prompted."
echo "  6. Browser auto-opens to http://127.0.0.1:8092/."
echo
echo "Or via the tarball (CLI users):"
echo "  tar -xzf ${PKG_NAME}.tar.gz && cd ${PKG_NAME} && ./start.command"
