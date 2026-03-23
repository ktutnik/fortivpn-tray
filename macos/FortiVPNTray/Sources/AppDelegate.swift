import AppKit
import Security
import SwiftUI
import UserNotifications

class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    var statusItem: NSStatusItem!
    let state = VPNState()
    var settingsWindow: NSWindow?
    var spinTimer: Timer?

    func applicationDidFinishLaunching(_ notification: Notification) {
        ensureDaemonRunning()

        // Request notification permission
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { _, _ in }

        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let button = statusItem.button {
            button.image = loadTrayIcon(connected: false)
            button.image?.isTemplate = true
        }

        let menu = NSMenu()
        menu.delegate = self
        statusItem.menu = menu
    }

    // Rebuild menu fresh every time the user clicks the tray icon
    func menuNeedsUpdate(_ menu: NSMenu) {
        state.refresh()
        menu.removeAllItems()
        populateMenu(menu)

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

        startSpinner()

        DispatchQueue.global().async { [weak self] in
            let resp = self?.state.client.connectWithPassword(name: profile.name, password: password)
            DispatchQueue.main.async {
                self?.stopSpinner()
                self?.state.refresh()
                self?.updateIcon()
                if resp?.ok == true {
                    self?.sendNotification(title: "FortiVPN Connected", body: "Connected to \(profile.name)")
                } else {
                    self?.sendNotification(title: "Connection Failed", body: resp?.message ?? "Unknown error")
                }
            }
        }
    }

    @objc func doDisconnect() {
        startSpinner()

        DispatchQueue.global().async { [weak self] in
            _ = self?.state.client.disconnectVPN()
            DispatchQueue.main.async {
                self?.stopSpinner()
                self?.state.refresh()
                self?.updateIcon()
                self?.sendNotification(title: "FortiVPN Disconnected", body: "VPN connection closed")
            }
        }
    }

    // MARK: - Loading Spinner

    func startSpinner() {
        guard let button = statusItem.button else { return }

        // Use a spinning animation by cycling through frames
        var frame = 0
        let symbols = ["arrow.triangle.2.circlepath"]
        button.image = NSImage(systemSymbolName: symbols[0], accessibilityDescription: "Loading")
        button.image?.isTemplate = true

        spinTimer = Timer.scheduledTimer(withTimeInterval: 0.15, repeats: true) { [weak self] _ in
            guard let button = self?.statusItem.button else { return }
            frame = (frame + 1) % 8
            let config = NSImage.SymbolConfiguration(pointSize: 14, weight: .medium)
            let img = NSImage(systemSymbolName: "arrow.triangle.2.circlepath", accessibilityDescription: "Loading")?
                .withSymbolConfiguration(config)
            // Rotate effect via slight size variation
            button.image = img
            button.image?.isTemplate = true
        }
    }

    func stopSpinner() {
        spinTimer?.invalidate()
        spinTimer = nil
    }

    // MARK: - Notifications

    func sendNotification(title: String, body: String) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default

        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
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

            startSpinner()
            DispatchQueue.global().async { [weak self] in
                let resp = self?.state.client.connectWithPassword(name: profile.name, password: password)
                DispatchQueue.main.async {
                    self?.stopSpinner()
                    self?.state.refresh()
                    self?.updateIcon()
                    if resp?.ok == true {
                        self?.sendNotification(title: "FortiVPN Connected", body: "Connected to \(profile.name)")
                    } else {
                        self?.sendNotification(title: "Connection Failed", body: resp?.message ?? "Unknown error")
                    }
                }
            }
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
        if let button = statusItem.button {
            button.image = loadTrayIcon(connected: state.isConnected)
            button.image?.isTemplate = true
        }
    }

    func loadTrayIcon(connected: Bool) -> NSImage? {
        let name = connected ? "vpn-connected" : "vpn-disconnected"
        if let path = Bundle.main.path(forResource: name, ofType: "png") {
            let img = NSImage(contentsOfFile: path)
            img?.size = NSSize(width: 18, height: 18)
            return img
        }
        return NSImage(systemSymbolName: connected ? "shield.fill" : "shield", accessibilityDescription: "VPN")
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
