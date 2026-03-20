#!/bin/bash
set -e

APP_NAME="FortiVPN Tray"
BUNDLE_DIR="target/release/bundle/${APP_NAME}.app"
CONTENTS="${BUNDLE_DIR}/Contents"

rm -rf "${BUNDLE_DIR}"
mkdir -p "${CONTENTS}/MacOS"
mkdir -p "${CONTENTS}/Resources"

# Build Rust daemon + helper + CLI
echo "Building Rust daemon..."
cargo build --release --workspace

# Build Swift app
echo "Building Swift app..."
cd macos/FortiVPNTray && swift build -c release 2>&1 && cd ../..

# Copy binaries
cp target/release/fortivpn-daemon "${CONTENTS}/MacOS/"
cp "macos/FortiVPNTray/.build/release/FortiVPNTray" "${CONTENTS}/MacOS/FortiVPN Tray"
cp target/release/fortivpn-helper "${CONTENTS}/Resources/" 2>/dev/null || true

# Copy resources
cp resources/Info.plist "${CONTENTS}/"
cp icons/icon.icns "${CONTENTS}/Resources/"
cp icons/vpn-connected.png "${CONTENTS}/Resources/"
cp icons/vpn-disconnected.png "${CONTENTS}/Resources/"
cp resources/com.fortivpn-tray.helper.plist "${CONTENTS}/Resources/"

# Update Info.plist to point to the Swift binary as the main executable
/usr/libexec/PlistBuddy -c "Set :CFBundleExecutable 'FortiVPN Tray'" "${CONTENTS}/Info.plist"

echo "Bundle created at: ${BUNDLE_DIR}"
echo "  Run: open '${BUNDLE_DIR}'"
