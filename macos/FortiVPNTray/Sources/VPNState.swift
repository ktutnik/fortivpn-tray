import Foundation
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
        profiles = client.getProfiles()
        if let s = client.getStatus() {
            status = s.status
            connectedProfile = s.profile
        }
    }
}
