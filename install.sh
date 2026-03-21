#!/bin/bash
set -e

echo "FortiVPN Tray — Build & Install"
echo "================================"
echo ""

# Check prerequisites
if ! command -v cargo &>/dev/null; then
    echo "Error: Rust toolchain not found. Install from https://rustup.rs"
    exit 1
fi

if ! command -v swift &>/dev/null; then
    echo "Error: Swift not found. Run: xcode-select --install"
    exit 1
fi

# Build everything
echo "[1/4] Building Rust daemon + helper..."
cargo build --release --workspace

echo "[2/4] Building Swift app..."
cd macos/FortiVPNTray && swift build -c release 2>&1 && cd ../..

# Create .app bundle
echo "[3/4] Creating app bundle..."
APP_NAME="FortiVPN Tray"
BUNDLE_DIR="target/release/bundle/${APP_NAME}.app"
CONTENTS="${BUNDLE_DIR}/Contents"

rm -rf "${BUNDLE_DIR}"
mkdir -p "${CONTENTS}/MacOS"
mkdir -p "${CONTENTS}/Resources"

cp target/release/fortivpn-daemon "${CONTENTS}/MacOS/"
cp "macos/FortiVPNTray/.build/release/FortiVPNTray" "${CONTENTS}/MacOS/FortiVPN Tray"
cp target/release/fortivpn-helper "${CONTENTS}/Resources/"
cp resources/Info.plist "${CONTENTS}/"
cp icons/icon.icns "${CONTENTS}/Resources/"
cp icons/vpn-connected.png "${CONTENTS}/Resources/"
cp icons/vpn-disconnected.png "${CONTENTS}/Resources/"
cp resources/com.fortivpn-tray.helper.plist "${CONTENTS}/Resources/"
/usr/libexec/PlistBuddy -c "Set :CFBundleExecutable 'FortiVPN Tray'" "${CONTENTS}/Info.plist"

# Install
echo "[4/4] Installing..."
pkill -9 -f "FortiVPN Tray" 2>/dev/null || true
pkill -9 -f "fortivpn-daemon" 2>/dev/null || true
sleep 1

rm -rf "/Applications/${APP_NAME}.app"
cp -r "${BUNDLE_DIR}" /Applications/

echo ""
echo "Installing privileged helper (requires admin password)..."
sudo bash -c "
cp target/release/fortivpn-helper /Library/PrivilegedHelperTools/fortivpn-helper &&
chmod 755 /Library/PrivilegedHelperTools/fortivpn-helper &&
chown root:wheel /Library/PrivilegedHelperTools/fortivpn-helper &&
cp resources/com.fortivpn-tray.helper.plist /Library/LaunchDaemons/ &&
chown root:wheel /Library/LaunchDaemons/com.fortivpn-tray.helper.plist &&
launchctl bootout system /Library/LaunchDaemons/com.fortivpn-tray.helper.plist 2>/dev/null;
launchctl bootstrap system /Library/LaunchDaemons/com.fortivpn-tray.helper.plist
"

echo ""
echo "✓ FortiVPN Tray installed successfully!"
echo ""

read -p "Launch now? [Y/n] " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Nn]$ ]]; then
    open "/Applications/${APP_NAME}.app"
fi
