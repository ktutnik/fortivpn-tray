import Foundation
import SwiftUI

class VPNState: ObservableObject {
    @Published var profiles: [VpnProfile] = []
    @Published var status: String = "disconnected"
    @Published var connectedProfile: String? = nil
    var isConnected: Bool { status == "connected" }
    var isBusy: Bool { status == "connecting" || status == "disconnecting" || status == "connected" }

    let client = DaemonClient()
    private var pollTimer: Timer?

    func refresh() {
        profiles = client.getProfiles()
        if let s = client.getStatus() {
            status = s.status
            connectedProfile = s.profile
        }
    }

    func startPolling() {
        refresh()
        pollTimer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in
            self?.refresh()
        }
    }

    func stopPolling() {
        pollTimer?.invalidate()
        pollTimer = nil
    }
}
