#!/usr/bin/env bash
# Reproducible packaging script for atem-net-diag.
#
# Builds the release binary, ad-hoc signs it (Apple Silicon requires
# at least ad-hoc signing), strips, bundles with start.command +
# README.txt, and produces a tarball + folder under dist/.
#
# Run from the tools/atem-net-diag/ directory (or anywhere — the
# script cd's to its own dir first).
#
# If a Developer ID Application identity is in the keychain, also
# performs hardened-runtime signing for distribution outside your
# own machines. Otherwise just ad-hoc signs (sufficient for personal
# use; macOS Gatekeeper will challenge first run on a new Mac, the
# operator right-clicks → Open once and it's done).
#
# Output:
#   dist/atem-net-diag-<version>-macos-arm64/         (loose folder)
#   dist/atem-net-diag-<version>-macos-arm64.tar.gz   (tarball)
#
# Usage:
#   ./build-package.sh                # build + tar
#   ./build-package.sh --copy-icloud  # also copy tarball to iCloud Drive

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

SIZE=$(du -h "$TARBALL" | cut -f1)
echo "==> Done. Tarball is ${SIZE}: ${TARBALL}"

if [[ "${1:-}" == "--copy-icloud" ]]; then
  ICLOUD_ROOT="$HOME/Library/Mobile Documents/com~apple~CloudDocs"
  if [[ ! -d "$ICLOUD_ROOT" ]]; then
    echo "WARN: iCloud Drive not found at $ICLOUD_ROOT — skipping copy."
  else
    cp "$TARBALL" "$ICLOUD_ROOT/"
    echo "==> Copied to: $ICLOUD_ROOT/${PKG_NAME}.tar.gz"
  fi
fi

echo
echo "Install on another Mac:"
echo "  1. AirDrop or sync the tarball over."
echo "  2. Extract: tar -xzf ${PKG_NAME}.tar.gz"
echo "  3. Right-click → Open the start.command (first time only — Gatekeeper)."
echo "  4. Paste your UDM_API_KEY when prompted."
