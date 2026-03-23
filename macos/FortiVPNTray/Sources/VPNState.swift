import Foundation
import Security
import SwiftUI

class VPNState: ObservableObject {
    @Published var profiles: [VpnProfile] = []
    @Published var status: String = "disconnected"
    @Published var connectedProfile: String? = nil
    var isConnected: Bool { status == "connected" }
    var isBusy: Bool { status == "connecting" || status == "disconnecting" || status == "connected" }

    let client = DaemonClient()

    /// Refresh state from daemon. Called on-demand (menu click, after connect/disconnect).
    func refresh() {
        var fetchedProfiles = client.getProfiles()
        // Check password status from Swift Keychain (not daemon — avoids macOS auth prompts)
        for i in fetchedProfiles.indices {
            fetchedProfiles[i].hasPassword = keychainHasPassword(profileId: fetchedProfiles[i].id)
        }
        profiles = fetchedProfiles
        if let s = client.getStatus() {
            status = s.status
            connectedProfile = s.profile
        }
    }

    private func keychainHasPassword(profileId: String) -> Bool {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "fortivpn-tray",
            kSecAttrAccount as String: profileId,
            kSecReturnData as String: false,
        ]
        return SecItemCopyMatching(query as CFDictionary, nil) == errSecSuccess
    }
}
