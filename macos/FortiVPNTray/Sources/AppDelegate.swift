import AppKit
import SwiftUI

class AppDelegate: NSObject, NSApplicationDelegate {
    var statusItem: NSStatusItem!
    let state = VPNState()
    var settingsWindow: NSWindow?

    func applicationDidFinishLaunching(_ notification: Notification) {
        ensureDaemonRunning()

        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let button = statusItem.button {
            button.image = NSImage(systemSymbolName: "shield", accessibilityDescription: "VPN")
            button.image?.isTemplate = true
        }

        state.startPolling()
        rebuildMenu()

        Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in
            self?.rebuildMenu()
        }
    }

    func rebuildMenu() {
        let menu = NSMenu()

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
                item.isEnabled = !state.isConnected
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

        self.statusItem.menu = menu

        // Update icon
        if let button = self.statusItem.button {
            let symbolName = state.isConnected ? "shield.fill" : "shield"
            button.image = NSImage(systemSymbolName: symbolName, accessibilityDescription: "VPN")
            button.image?.isTemplate = true
        }
    }

    @objc func doConnect(_ sender: NSMenuItem) {
        guard let profile = sender.representedObject as? VpnProfile else { return }

        if !profile.hasPassword {
            showPasswordPrompt(profile: profile)
            return
        }

        DispatchQueue.global().async { [weak self] in
            let resp = self?.state.client.connectVPN(name: profile.name)
            DispatchQueue.main.async {
                self?.state.refresh()
                self?.rebuildMenu()
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
                self?.rebuildMenu()
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
        state.stopPolling()
        if state.isConnected {
            _ = state.client.disconnectVPN()
        }
        NSApp.terminate(nil)
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

            _ = state.client.setPassword(id: profile.id, password: password)
            state.refresh()

            DispatchQueue.global().async { [weak self] in
                _ = self?.state.client.connectVPN(name: profile.name)
                DispatchQueue.main.async {
                    self?.state.refresh()
                    self?.rebuildMenu()
                }
            }
        }
    }

    func ensureDaemonRunning() {
        if state.client.isConnected { return }
        // Try to launch daemon from bundle
        if let url = Bundle.main.url(forAuxiliaryExecutable: "fortivpn-daemon") {
            let process = Process()
            process.executableURL = url
            try? process.run()
            Thread.sleep(forTimeInterval: 1.0)
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
