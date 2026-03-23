import AppKit
import Security
import SwiftUI

class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    var statusItem: NSStatusItem!
    let state = VPNState()
    var settingsWindow: NSWindow?
    var isLoading = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        ensureDaemonRunning()

        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let button = statusItem.button {
            button.image = loadTrayIcon(name: "vpn-disconnected")
            button.image?.isTemplate = true
        }

        let menu = NSMenu()
        menu.delegate = self
        statusItem.menu = menu
    }

    // Rebuild menu fresh every time the user clicks the tray icon
    func menuNeedsUpdate(_ menu: NSMenu) {
        if !isLoading {
            state.refresh()
        }
        menu.removeAllItems()
        populateMenu(menu)

        if !isLoading {
            updateIcon()
        }
    }

    func populateMenu(_ menu: NSMenu) {
        if isLoading {
            let item = NSMenuItem(title: "Connecting...", action: nil, keyEquivalent: "")
            item.isEnabled = false
            menu.addItem(item)
            menu.addItem(.separator())
        } else {
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
        }

        let settingsItem = NSMenuItem(title: "Settings...", action: #selector(openSettings), keyEquivalent: ",")
        settingsItem.target = self
        menu.addItem(settingsItem)
        let quitItem = NSMenuItem(title: "Quit", action: #selector(quitApp), keyEquivalent: "q")
        quitItem.target = self
        menu.addItem(quitItem)
    }

    // MARK: - Connect / Disconnect

    @objc func doConnect(_ sender: NSMenuItem) {
        guard let profile = sender.representedObject as? VpnProfile else { return }

        guard let password = readKeychainPassword(profileId: profile.id) else {
            showPasswordPrompt(profile: profile)
            return
        }

        connectWithLoading(profileName: profile.name, password: password)
    }

    func connectWithLoading(profileName: String, password: String) {
        setLoading(true)

        DispatchQueue.global().async { [weak self] in
            let resp = self?.state.client.connectWithPassword(name: profileName, password: password)
            DispatchQueue.main.async {
                self?.setLoading(false)
                self?.state.refresh()
                self?.updateIcon()
                if resp?.ok == true {
                    self?.showNotification(title: "FortiVPN Connected", body: "Connected to \(profileName)")
                } else {
                    self?.showNotification(title: "Connection Failed", body: resp?.message ?? "Unknown error")
                }
            }
        }
    }

    @objc func doDisconnect() {
        setLoading(true)

        DispatchQueue.global().async { [weak self] in
            _ = self?.state.client.disconnectVPN()
            DispatchQueue.main.async {
                self?.setLoading(false)
                self?.state.refresh()
                self?.updateIcon()
                self?.showNotification(title: "FortiVPN Disconnected", body: "VPN connection closed")
            }
        }
    }

    // MARK: - Loading State

    func setLoading(_ loading: Bool) {
        isLoading = loading
        guard let button = statusItem.button else { return }
        if loading {
            // Shield with hole = loading indicator
            button.image = loadTrayIcon(name: "vpn-disconnected")
            button.image?.isTemplate = true
            // Add a subtle pulsing effect via appearsDisabled
            button.appearsDisabled = true
        } else {
            button.appearsDisabled = false
        }
    }

    // MARK: - Notifications (using Process to call osascript — works for accessory apps)

    func showNotification(title: String, body: String) {
        let script = """
        display notification "\(body.replacingOccurrences(of: "\"", with: "\\\""))" with title "\(title.replacingOccurrences(of: "\"", with: "\\\""))"
        """
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
        task.arguments = ["-e", script]
        try? task.run()
    }

    // MARK: - Settings

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

        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)

        settingsWindow = window
    }

    // MARK: - Quit

    @objc func quitApp() {
        if state.isConnected {
            _ = state.client.disconnectVPN()
        }
        killDaemon()
        NSApp.terminate(nil)
    }

    func killDaemon() {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/pkill")
        task.arguments = ["-f", "fortivpn-daemon"]
        try? task.run()
        task.waitUntilExit()
    }

    // MARK: - Password Prompt

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

            storeKeychainPassword(profileId: profile.id, password: password)
            connectWithLoading(profileName: profile.name, password: password)
        }
    }

    // MARK: - Keychain

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
            var addQuery = query
            addQuery[kSecValueData as String] = passwordData
            SecItemAdd(addQuery as CFDictionary, nil)
        }
    }

    // MARK: - Icon

    func updateIcon() {
        guard let button = statusItem.button else { return }
        button.image = loadTrayIcon(name: state.isConnected ? "vpn-connected" : "vpn-disconnected")
        button.image?.isTemplate = true
    }

    func loadTrayIcon(name: String) -> NSImage? {
        if let path = Bundle.main.path(forResource: name, ofType: "png") {
            let img = NSImage(contentsOfFile: path)
            img?.size = NSSize(width: 18, height: 18)
            return img
        }
        let symbolName = name.contains("connected") && !name.contains("dis") ? "shield.fill" : "shield"
        return NSImage(systemSymbolName: symbolName, accessibilityDescription: "VPN")
    }

    // MARK: - Daemon

    func ensureDaemonRunning() {
        if state.client.isConnected { return }

        killDaemon()
        Thread.sleep(forTimeInterval: 0.5)

        if let url = Bundle.main.url(forAuxiliaryExecutable: "fortivpn-daemon") {
            let process = Process()
            process.executableURL = url
            try? process.run()

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
        NSApp.setActivationPolicy(.accessory)
    }
}
