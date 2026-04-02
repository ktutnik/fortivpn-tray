#!/bin/bash
set -e

APP_NAME="FortiVPN Tray"
BUNDLE_DIR="target/release/bundle/${APP_NAME}.app"
CONTENTS="${BUNDLE_DIR}/Contents"

rm -rf "${BUNDLE_DIR}"
mkdir -p "${CONTENTS}/MacOS"
mkdir -p "${CONTENTS}/Resources"

# Build all Rust binaries
echo "Building..."
cargo build --release --workspace

# Copy binaries
cp target/release/fortivpn-daemon "${CONTENTS}/MacOS/"
cp target/release/fortivpn-app "${CONTENTS}/MacOS/FortiVPN Tray"
cp target/release/fortivpn-helper "${CONTENTS}/Resources/" 2>/dev/null || true

# Copy resources
cp resources/Info.plist "${CONTENTS}/"
cp icons/icon.icns "${CONTENTS}/Resources/"
cp icons/vpn-connected.png "${CONTENTS}/Resources/"
cp icons/vpn-disconnected.png "${CONTENTS}/Resources/"
cp resources/com.fortivpn-tray.helper.plist "${CONTENTS}/Resources/"

# Update Info.plist executable name
/usr/libexec/PlistBuddy -c "Set :CFBundleExecutable 'FortiVPN Tray'" "${CONTENTS}/Info.plist"

echo "Bundle created at: ${BUNDLE_DIR}"
echo "  Run: open '${BUNDLE_DIR}'"
