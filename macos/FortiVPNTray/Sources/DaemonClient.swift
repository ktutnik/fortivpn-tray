import Foundation

class DaemonClient {
    private let socketPath: String

    init() {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        socketPath = "\(home)/.config/fortivpn-tray/ipc.sock"
    }

    /// Send a command and return the raw JSON response
    func send(command: String) -> IpcResponse? {
        let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return nil }
        defer { Darwin.close(fd) }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        withUnsafeMutableBytes(of: &addr.sun_path) { buf in
            let bytes = Array(socketPath.utf8)
            for i in 0..<min(bytes.count, buf.count - 1) {
                buf[i] = bytes[i]
            }
            buf[min(bytes.count, buf.count - 1)] = 0
        }

        let len = socklen_t(MemoryLayout<sockaddr_un>.size)
        let connected = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(fd, $0, len)
            }
        }
        guard connected == 0 else { return nil }

        // Send
        let msg = command + "\n"
        msg.withCString { ptr in
            _ = Darwin.write(fd, ptr, strlen(ptr))
        }

        // Read response
        var buffer = [UInt8](repeating: 0, count: 65536)
        let n = Darwin.read(fd, &buffer, buffer.count - 1)
        guard n > 0 else { return nil }

        let data = Data(buffer[0..<n])
        return try? JSONDecoder().decode(IpcResponse.self, from: data)
    }

    // Convenience methods
    func getStatus() -> StatusResponse? {
        guard let resp = send(command: "status"), resp.ok, let data = resp.data else { return nil }
        // Re-encode data and decode as StatusResponse
        guard let jsonData = try? JSONEncoder().encode(data),
              let status = try? JSONDecoder().decode(StatusResponse.self, from: jsonData)
        else { return nil }
        return status
    }

    func getProfiles() -> [VpnProfile] {
        guard let resp = send(command: "get_profiles"), resp.ok, let data = resp.data else { return [] }
        guard let jsonData = try? JSONEncoder().encode(data),
              let profiles = try? JSONDecoder().decode([VpnProfile].self, from: jsonData)
        else { return [] }
        return profiles
    }

    func connectVPN(name: String) -> IpcResponse? {
        send(command: "connect \(name)")
    }

    func disconnectVPN() -> IpcResponse? {
        send(command: "disconnect")
    }

    func saveProfile(id: String?, name: String, host: String, port: Int, username: String, trustedCert: String) -> IpcResponse? {
        var dict: [String: Any] = [
            "name": name, "host": host, "port": port,
            "username": username, "trusted_cert": trustedCert
        ]
        if let id = id, !id.isEmpty { dict["id"] = id }
        guard let jsonData = try? JSONSerialization.data(withJSONObject: dict),
              let jsonStr = String(data: jsonData, encoding: .utf8)
        else { return nil }
        return send(command: "save_profile \(jsonStr)")
    }

    func deleteProfile(id: String) -> IpcResponse? {
        send(command: "delete_profile \(id)")
    }

    func setPassword(id: String, password: String) -> IpcResponse? {
        send(command: "set_password \(id) \(password)")
    }

    func hasPassword(id: String) -> Bool {
        guard let resp = send(command: "has_password \(id)"), resp.ok, let data = resp.data else { return false }
        if case .object(let dict) = data, case .bool(let v) = dict["has_password"] { return v }
        return false
    }

    var isConnected: Bool {
        getStatus() != nil
    }
}
