#!/bin/bash
set -e

echo "FortiVPN Tray — Uninstall"
echo "========================="
echo ""

OS="$(uname -s)"

case "$OS" in
    Darwin)
        # --- macOS ---
        echo "Stopping processes..."
        pkill -9 -f "FortiVPN Tray" 2>/dev/null || true
        pkill -9 -f "fortivpn-daemon" 2>/dev/null || true
        sleep 1

        echo "Removing app..."
        rm -rf "/Applications/FortiVPN Tray.app"

        echo "Removing helper daemon (requires admin password)..."
        sudo bash -c '
        launchctl bootout system /Library/LaunchDaemons/com.fortivpn-tray.helper.plist 2>/dev/null || true
        rm -f /Library/PrivilegedHelperTools/fortivpn-helper
        rm -f /Library/LaunchDaemons/com.fortivpn-tray.helper.plist
        '

        echo ""
        read -p "Remove profiles and config? [y/N] " -n 1 -r
        echo
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            rm -rf "${HOME}/Library/Application Support/fortivpn-tray"
            echo "Config removed."
        fi

        echo ""
        echo "✓ FortiVPN Tray uninstalled."
        echo ""
        echo "Note: Keychain passwords are preserved. To remove them:"
        echo "  security delete-generic-password -s fortivpn-tray"
        ;;

    Linux)
        # --- Linux ---
        INSTALL_DIR="${HOME}/.local/bin"

        echo "Stopping processes..."
        pkill -9 -f "fortivpn-app" 2>/dev/null || true
        pkill -9 -f "fortivpn-daemon" 2>/dev/null || true

        echo "Removing binaries..."
        rm -f "$INSTALL_DIR/fortivpn-daemon"
        rm -f "$INSTALL_DIR/fortivpn-app"
        rm -f "$INSTALL_DIR/fortivpn"
        rm -f "$INSTALL_DIR/fortivpn-helper"

        echo "Removing desktop entry..."
        rm -f "${HOME}/.local/share/applications/fortivpn-tray.desktop"
        rm -f "${HOME}/.config/autostart/fortivpn-tray.desktop"
        rm -f "${HOME}/.local/share/icons/fortivpn-tray.png"

        echo ""
        read -p "Remove profiles and config? [y/N] " -n 1 -r
        echo
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            rm -rf "${HOME}/.config/fortivpn-tray"
            echo "Config removed."
        fi

        echo ""
        echo "✓ FortiVPN Tray uninstalled."
        ;;

    MINGW*|MSYS*|CYGWIN*)
        # --- Windows (Git Bash / MSYS2) ---
        INSTALL_DIR="${LOCALAPPDATA}/FortiVPN Tray"

        echo "Stopping processes..."
        taskkill /F /IM fortivpn-app.exe 2>/dev/null || true
        taskkill /F /IM fortivpn-daemon.exe 2>/dev/null || true

        echo "Removing installation..."
        rm -rf "$INSTALL_DIR"

        echo ""
        read -p "Remove profiles and config? [y/N] " -n 1 -r
        echo
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            rm -rf "${APPDATA}/fortivpn-tray"
            echo "Config removed."
        fi

        echo ""
        echo "✓ FortiVPN Tray uninstalled."
        echo ""
        echo "Note: Remove any startup shortcut from:"
        echo "  %APPDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\Startup"
        ;;

    *)
        echo "Unsupported OS: $OS"
        exit 1
        ;;
esac
