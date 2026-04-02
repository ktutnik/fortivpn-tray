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

        # Kill running instances
        echo "[1/4] Stopping running instances..."
        taskkill //F //IM fortivpn-app.exe 2>/dev/null || true
        taskkill //F //IM fortivpn-daemon.exe 2>/dev/null || true
        sleep 1

        echo "[2/4] Building..."
        cargo build --release -p fortivpn-daemon -p fortivpn-app -p fortivpn-cli

        echo "[3/5] Installing..."
        INSTALL_DIR="${LOCALAPPDATA}/FortiVPN Tray"
        mkdir -p "$INSTALL_DIR"

        cp target/release/fortivpn-daemon.exe "$INSTALL_DIR/"
        cp target/release/fortivpn-app.exe "$INSTALL_DIR/"
        cp target/release/fortivpn.exe "$INSTALL_DIR/"
        cp icons/icon.ico "$INSTALL_DIR/"
        cp icons/icon.png "$INSTALL_DIR/"
        cp icons/vpn-connected.png "$INSTALL_DIR/"
        cp icons/vpn-disconnected.png "$INSTALL_DIR/"

        # Download WinTun driver if not already present
        echo "[4/5] Checking WinTun driver..."
        if [ ! -f "$INSTALL_DIR/wintun.dll" ]; then
            echo "  Downloading wintun.dll..."
            WINTUN_URL="https://www.wintun.net/builds/wintun-0.14.1.zip"
            WINTUN_ZIP="$INSTALL_DIR/wintun.zip"
            curl -sL "$WINTUN_URL" -o "$WINTUN_ZIP" 2>/dev/null || \
                powershell.exe -NoProfile -Command "Invoke-WebRequest -Uri '$WINTUN_URL' -OutFile '$(cygpath -w "$WINTUN_ZIP")'" 2>/dev/null
            if [ -f "$WINTUN_ZIP" ]; then
                # Extract the correct architecture DLL
                ARCH=$(uname -m)
                case "$ARCH" in
                    x86_64|AMD64) WINTUN_ARCH="amd64" ;;
                    aarch64|ARM64) WINTUN_ARCH="arm64" ;;
                    *) WINTUN_ARCH="amd64" ;;
                esac
                powershell.exe -NoProfile -Command "
                    Add-Type -AssemblyName System.IO.Compression.FileSystem;
                    \$zip = [System.IO.Compression.ZipFile]::OpenRead('$(cygpath -w "$WINTUN_ZIP")');
                    \$entry = \$zip.Entries | Where-Object { \$_.FullName -like \"*wintun/bin/${WINTUN_ARCH}/wintun.dll\" };
                    if (\$entry) {
                        [System.IO.Compression.ZipFileExtensions]::ExtractToFile(\$entry, '$(cygpath -w "$INSTALL_DIR/wintun.dll")', \$true);
                    }
                    \$zip.Dispose();
                "
                rm -f "$WINTUN_ZIP"
                if [ -f "$INSTALL_DIR/wintun.dll" ]; then
                    echo "  wintun.dll installed"
                else
                    echo "  WARNING: Could not extract wintun.dll — VPN connections will fail"
                    echo "  Download manually from https://www.wintun.net/ and place wintun.dll in:"
                    echo "    $INSTALL_DIR/"
                fi
            else
                echo "  WARNING: Could not download WinTun — VPN connections will fail"
                echo "  Download manually from https://www.wintun.net/ and place wintun.dll in:"
                echo "    $INSTALL_DIR/"
            fi
        else
            echo "  wintun.dll already present"
        fi

        # Create Start Menu shortcut with icon
        echo "[5/5] Creating shortcuts..."
        START_MENU_DIR="${APPDATA}/Microsoft/Windows/Start Menu/Programs"
        STARTUP_DIR="${APPDATA}/Microsoft/Windows/Start Menu/Programs/Startup"
        mkdir -p "$START_MENU_DIR" "$STARTUP_DIR"

        # Windows shortcuts (.lnk) require PowerShell
        INSTALL_DIR_WIN=$(cygpath -w "$INSTALL_DIR")
        powershell.exe -NoProfile -Command "
            \$ws = New-Object -ComObject WScript.Shell;
            \$s = \$ws.CreateShortcut('$(cygpath -w "$START_MENU_DIR")\FortiVPN Tray.lnk');
            \$s.TargetPath = '${INSTALL_DIR_WIN}\fortivpn-app.exe';
            \$s.WorkingDirectory = '${INSTALL_DIR_WIN}';
            \$s.IconLocation = '${INSTALL_DIR_WIN}\icon.ico,0';
            \$s.Description = 'FortiGate SSL-VPN Client';
            \$s.Save();
            \$s = \$ws.CreateShortcut('$(cygpath -w "$STARTUP_DIR")\FortiVPN Tray.lnk');
            \$s.TargetPath = '${INSTALL_DIR_WIN}\fortivpn-app.exe';
            \$s.WorkingDirectory = '${INSTALL_DIR_WIN}';
            \$s.IconLocation = '${INSTALL_DIR_WIN}\icon.ico,0';
            \$s.Description = 'FortiGate SSL-VPN Client';
            \$s.Save();
        "

        # Also create Desktop shortcut
        DESKTOP_DIR="${USERPROFILE}/Desktop"
        if [ -d "$DESKTOP_DIR" ]; then
            powershell.exe -NoProfile -Command "
                \$ws = New-Object -ComObject WScript.Shell;
                \$s = \$ws.CreateShortcut('$(cygpath -w "$DESKTOP_DIR")\FortiVPN Tray.lnk');
                \$s.TargetPath = '${INSTALL_DIR_WIN}\fortivpn-app.exe';
                \$s.WorkingDirectory = '${INSTALL_DIR_WIN}';
                \$s.IconLocation = '${INSTALL_DIR_WIN}\icon.ico,0';
                \$s.Description = 'FortiGate SSL-VPN Client';
                \$s.Save();
            "
        fi

        echo ""
        echo "Installed to: ${INSTALL_DIR}/"
        echo "Shortcuts:    Start Menu + Desktop + Autostart"
        echo ""

        # Launch the app
        echo "Launching FortiVPN Tray..."
        start "" "$(cygpath -w "$INSTALL_DIR/fortivpn-app.exe")"
        ;;

    *)
        echo "Unsupported OS: $OS"
        exit 1
        ;;
esac
