#!/bin/bash
set -e

echo "FortiVPN Tray — Build & Install"
echo "================================"
echo ""

if ! command -v cargo &>/dev/null; then
    echo "Error: Rust toolchain not found. Install from https://rustup.rs"
    exit 1
fi

OS="$(uname -s)"

case "$OS" in
    Darwin)
        # --- macOS ---
        echo "[1/3] Building Rust workspace..."
        cargo build --release --workspace

        echo "[2/3] Creating app bundle..."
        APP_NAME="FortiVPN Tray"
        BUNDLE_DIR="target/release/bundle/${APP_NAME}.app"
        CONTENTS="${BUNDLE_DIR}/Contents"

        rm -rf "${BUNDLE_DIR}"
        mkdir -p "${CONTENTS}/MacOS" "${CONTENTS}/Resources"

        cp target/release/fortivpn-daemon "${CONTENTS}/MacOS/"
        cp target/release/fortivpn-app "${CONTENTS}/MacOS/FortiVPN Tray"
        cp target/release/fortivpn-helper "${CONTENTS}/Resources/"
        cp resources/Info.plist "${CONTENTS}/"
        cp icons/icon.icns "${CONTENTS}/Resources/"
        cp icons/vpn-connected.png "${CONTENTS}/Resources/"
        cp icons/vpn-disconnected.png "${CONTENTS}/Resources/"
        cp resources/com.fortivpn-tray.helper.plist "${CONTENTS}/Resources/"
        /usr/libexec/PlistBuddy -c "Set :CFBundleExecutable 'FortiVPN Tray'" "${CONTENTS}/Info.plist"

        echo "[3/3] Installing..."
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
        echo "✓ FortiVPN Tray installed to /Applications/"
        echo ""
        read -p "Launch now? [Y/n] " -n 1 -r
        echo
        if [[ ! $REPLY =~ ^[Nn]$ ]]; then
            open "/Applications/${APP_NAME}.app"
        fi
        ;;

    Linux)
        # --- Linux ---
        echo "[1/2] Building..."
        cargo build --release -p fortivpn-daemon -p fortivpn-app -p fortivpn-helper -p fortivpn-cli

        echo "[2/2] Installing..."
        INSTALL_DIR="${HOME}/.local/bin"
        DESKTOP_DIR="${HOME}/.local/share/applications"
        ICON_DIR="${HOME}/.local/share/icons"
        mkdir -p "$INSTALL_DIR" "$DESKTOP_DIR" "$ICON_DIR"

        cp target/release/fortivpn-daemon "$INSTALL_DIR/"
        cp target/release/fortivpn-app "$INSTALL_DIR/"
        cp target/release/fortivpn "$INSTALL_DIR/"
        cp target/release/fortivpn-helper "$INSTALL_DIR/"
        cp icons/icon.png "$ICON_DIR/fortivpn-tray.png"

        cat > "$DESKTOP_DIR/fortivpn-tray.desktop" << DEOF
[Desktop Entry]
Name=FortiVPN Tray
Comment=FortiGate SSL-VPN client
Exec=${INSTALL_DIR}/fortivpn-app
Icon=fortivpn-tray
Type=Application
Categories=Network;VPN;
StartupNotify=false
DEOF

        echo ""
        echo "✓ Installed to ${INSTALL_DIR}/"
        echo ""
        echo "Start:     fortivpn-daemon & fortivpn-app"
        echo "Autostart: cp ${DESKTOP_DIR}/fortivpn-tray.desktop ~/.config/autostart/"
        ;;

    MINGW*|MSYS*|CYGWIN*)
        # --- Windows (Git Bash / MSYS2) ---
        echo "[1/2] Building..."
        cargo build --release -p fortivpn-daemon -p fortivpn-app -p fortivpn-cli

        echo "[2/2] Installing..."
        INSTALL_DIR="${LOCALAPPDATA}/FortiVPN Tray"
        mkdir -p "$INSTALL_DIR"

        cp target/release/fortivpn-daemon.exe "$INSTALL_DIR/"
        cp target/release/fortivpn-app.exe "$INSTALL_DIR/"
        cp target/release/fortivpn.exe "$INSTALL_DIR/"
        cp icons/icon.png "$INSTALL_DIR/"
        cp icons/vpn-connected.png "$INSTALL_DIR/"
        cp icons/vpn-disconnected.png "$INSTALL_DIR/"

        echo ""
        echo "✓ Installed to ${INSTALL_DIR}/"
        echo ""
        echo "Start: \"${INSTALL_DIR}/fortivpn-app.exe\""
        echo ""
        echo "Autostart: create shortcut in"
        echo "  %APPDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\Startup"
        ;;

    *)
        echo "Unsupported OS: $OS"
        exit 1
        ;;
esac
