import AppKit
import Security
import SwiftUI

class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    var statusItem: NSStatusItem!
    let state = VPNState()
    var settingsWindow: NSWindow?

    func applicationDidFinishLaunching(_ notification: Notification) {
        ensureDaemonRunning()

        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let button = statusItem.button {
            button.image = loadTrayIcon(connected: false)
            button.image?.isTemplate = true
        }

        // No polling — menu refreshes on click via NSMenuDelegate.
        // Icon updates only after connect/disconnect actions.

        let menu = NSMenu()
        menu.delegate = self
        statusItem.menu = menu
    }

    // Rebuild menu fresh every time the user clicks the tray icon
    func menuNeedsUpdate(_ menu: NSMenu) {
        state.refresh()
        menu.removeAllItems()
        populateMenu(menu)

        // Update tray icon to match current state
        if let button = statusItem.button {
            button.image = loadTrayIcon(connected: state.isConnected)
            button.image?.isTemplate = true
        }
    }

    func populateMenu(_ menu: NSMenu) {
        for profile in state.profiles {
            let connected = state.isConnected && state.connectedProfile == profile.name
            if connected {
                let item = NSMenuItem(title: "\u{25CF} \(profile.name) \u{2014} Disconnect", action: #selector(doDisconnect), keyEquivalent: "")
                item.target = self
                menu.addItem(item)
            } else {
                let item = NSMenuItem(title: "\u{25CB} \(profile.name) \u{2014} Connect", action: #selector(doConnect(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = profile
                item.isEnabled = !state.isBusy
                menu.addItem(item)
            }
        }

        menu.addItem(.separator())

        let statusText: String
        if state.status.hasPrefix("error") {
            statusText = "Status: \(state.status)"
        } else if state.isConnected, let name = state.connectedProfile {
            statusText = "Status: Connected to \(name)"
        } else {
            statusText = "Status: \(state.status.capitalized)"
        }
        let statusMenuItem = NSMenuItem(title: statusText, action: nil, keyEquivalent: "")
        statusMenuItem.isEnabled = false
        menu.addItem(statusMenuItem)

        menu.addItem(.separator())
        let settingsItem = NSMenuItem(title: "Settings...", action: #selector(openSettings), keyEquivalent: ",")
        settingsItem.target = self
        menu.addItem(settingsItem)
        let quitItem = NSMenuItem(title: "Quit", action: #selector(quitApp), keyEquivalent: "q")
        quitItem.target = self
        menu.addItem(quitItem)

    }

    @objc func doConnect(_ sender: NSMenuItem) {
        guard let profile = sender.representedObject as? VpnProfile else { return }

        // Read password from Keychain (Swift has UI access — no blocked keyboard)
        guard let password = readKeychainPassword(profileId: profile.id) else {
            showPasswordPrompt(profile: profile)
            return
        }

        DispatchQueue.global().async { [weak self] in
            // Send password to daemon so it never touches Keychain
            let resp = self?.state.client.connectWithPassword(name: profile.name, password: password)
            DispatchQueue.main.async {
                self?.state.refresh()
                self?.updateIcon()
                if resp?.ok != true {
                    let alert = NSAlert()
                    alert.messageText = "Connection Failed"
                    alert.informativeText = resp?.message ?? "Unknown error"
                    alert.alertStyle = .warning
                    alert.runModal()
                }
            }
        }
    }

    @objc func doDisconnect() {
        DispatchQueue.global().async { [weak self] in
            _ = self?.state.client.disconnectVPN()
            DispatchQueue.main.async {
                self?.state.refresh()
                self?.updateIcon()
            }
        }
    }

    @objc func openSettings() {
        if let window = settingsWindow {
            window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }

        let view = SettingsView(state: state)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 600, height: 480),
            styleMask: [.titled, .closable, .resizable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.title = "FortiVPN Settings"
        window.contentView = NSHostingView(rootView: view)
        window.center()
        window.isReleasedWhenClosed = false
        window.delegate = self
        window.makeKeyAndOrderFront(nil)

        // Show in Dock while settings is open
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)

        settingsWindow = window
    }

    @objc func quitApp() {
        if state.isConnected {
            _ = state.client.disconnectVPN()
        }
        // Kill the daemon process so it doesn't linger
        killDaemon()
        NSApp.terminate(nil)
    }

    func killDaemon() {
        // Find and kill any fortivpn-daemon processes we spawned
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/pkill")
        task.arguments = ["-f", "fortivpn-daemon"]
        try? task.run()
        task.waitUntilExit()
    }

    func showPasswordPrompt(profile: VpnProfile) {
        let alert = NSAlert()
        alert.messageText = "Enter password for \"\(profile.name)\""
        alert.informativeText = "Your password will be stored in Keychain."
        alert.alertStyle = .informational
        alert.addButton(withTitle: "Connect")
        alert.addButton(withTitle: "Cancel")

        let input = NSSecureTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        input.placeholderString = "Password"
        alert.accessoryView = input
        alert.window.initialFirstResponder = input

        if alert.runModal() == .alertFirstButtonReturn {
            let password = input.stringValue
            guard !password.isEmpty else { return }

            // Store password in Keychain via Swift (not daemon)
            storeKeychainPassword(profileId: profile.id, password: password)

            DispatchQueue.global().async { [weak self] in
                let resp = self?.state.client.connectWithPassword(name: profile.name, password: password)
                DispatchQueue.main.async {
                    self?.state.refresh()
                    self?.updateIcon()
                }
            }
        }
    }

    // MARK: - Keychain (Swift-side, avoids daemon Keychain access on macOS)

    func readKeychainPassword(profileId: String) -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "fortivpn-tray",
            kSecAttrAccount as String: profileId,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess, let data = result as? Data,
              let password = String(data: data, encoding: .utf8)
        else { return nil }
        return password
    }

    func storeKeychainPassword(profileId: String, password: String) {
        let passwordData = password.data(using: .utf8)!

        // Try to update first
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "fortivpn-tray",
            kSecAttrAccount as String: profileId,
        ]
        let update: [String: Any] = [
            kSecValueData as String: passwordData,
        ]
        let status = SecItemUpdate(query as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            // Add new
            var addQuery = query
            addQuery[kSecValueData as String] = passwordData
            SecItemAdd(addQuery as CFDictionary, nil)
        }
    }

    func updateIcon() {
        if let button = statusItem.button {
            button.image = loadTrayIcon(connected: state.isConnected)
            button.image?.isTemplate = true
        }
    }

    func loadTrayIcon(connected: Bool) -> NSImage? {
        let name = connected ? "vpn-connected" : "vpn-disconnected"
        // Look in bundle Resources first, then next to executable
        if let path = Bundle.main.path(forResource: name, ofType: "png") {
            let img = NSImage(contentsOfFile: path)
            img?.size = NSSize(width: 18, height: 18)
            return img
        }
        // Fallback to SF Symbol
        return NSImage(systemSymbolName: connected ? "shield.fill" : "shield", accessibilityDescription: "VPN")
    }

    func ensureDaemonRunning() {
        if state.client.isConnected { return }

        // Kill any orphan daemon that might be holding the socket
        killDaemon()
        Thread.sleep(forTimeInterval: 0.5)

        // Launch fresh daemon from bundle
        if let url = Bundle.main.url(forAuxiliaryExecutable: "fortivpn-daemon") {
            let process = Process()
            process.executableURL = url
            try? process.run()

            // Wait for daemon to be ready (up to 5 seconds)
            for _ in 0..<10 {
                Thread.sleep(forTimeInterval: 0.5)
                if state.client.isConnected { return }
            }
            print("Warning: daemon did not become ready")
        }
    }
}

extension AppDelegate: NSWindowDelegate {
    func windowWillClose(_ notification: Notification) {
        settingsWindow = nil
        // Hide from Dock when settings closes
        NSApp.setActivationPolicy(.accessory)
    }
}
