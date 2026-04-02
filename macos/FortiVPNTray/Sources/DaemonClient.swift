import Foundation

class DaemonClient {
    /// Send a command and return the raw JSON response
    func send(command: String) -> IpcResponse? {
        let fd = Darwin.socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return nil }
        defer { Darwin.close(fd) }

        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = UInt16(9847).bigEndian
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")

        var timeout = timeval(tv_sec: 30, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))

        let len = socklen_t(MemoryLayout<sockaddr_in>.size)
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

    func connectWithPassword(name: String, password: String) -> IpcResponse? {
        let json: [String: Any] = ["name": name, "password": password]
        guard let jsonData = try? JSONSerialization.data(withJSONObject: json),
              let jsonStr = String(data: jsonData, encoding: .utf8)
        else { return nil }
        return send(command: "connect_with_password \(jsonStr)")
    }

    var isConnected: Bool {
        getStatus() != nil
    }
}
