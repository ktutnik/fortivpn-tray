#!/bin/bash
# Create a macOS .app bundle from the release binary
set -e

APP_NAME="FortiVPN Tray"
BUNDLE_DIR="target/release/bundle/${APP_NAME}.app"
CONTENTS="${BUNDLE_DIR}/Contents"

rm -rf "${BUNDLE_DIR}"
mkdir -p "${CONTENTS}/MacOS"
mkdir -p "${CONTENTS}/Resources"

# Copy binary
cp target/release/fortivpn-tray "${CONTENTS}/MacOS/"

# Copy helper binary
cp target/release/fortivpn-helper "${CONTENTS}/Resources/" 2>/dev/null || true

# Copy Info.plist
cp resources/Info.plist "${CONTENTS}/"

# Copy icon
cp icons/icon.icns "${CONTENTS}/Resources/"

# Copy helper plist
cp resources/com.fortivpn-tray.helper.plist "${CONTENTS}/Resources/"

echo "✓ Bundle created at: ${BUNDLE_DIR}"
