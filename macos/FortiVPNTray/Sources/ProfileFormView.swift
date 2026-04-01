import Security
import SwiftUI

struct ProfileFormView: View {
    @ObservedObject var state: VPNState
    let profile: VpnProfile?
    let onDone: () -> Void

    @State private var name = ""
    @State private var host = ""
    @State private var port = "443"
    @State private var username = ""
    @State private var trustedCert = ""
    @State private var password = ""
    @State private var statusMessage: String?
    @State private var isError = false
    @State private var isFetchingCert = false

    private var isEditing: Bool { profile != nil }

    var body: some View {
        Form {
            Section("Connection") {
                TextField("Name", text: $name, prompt: Text("My VPN"))
                TextField("Host", text: $host, prompt: Text("vpn.example.com"))
                TextField("Port", text: $port, prompt: Text("443"))
                TextField("Username", text: $username, prompt: Text("user@company.com"))
            }

            Section("Server Certificate") {
                HStack {
                    TextField("SHA256 Fingerprint", text: $trustedCert, prompt: Text("Auto-detect or paste manually"))
                    Button(action: fetchCertificate) {
                        if isFetchingCert {
                            ProgressView()
                                .controlSize(.small)
                        } else {
                            Text("Fetch")
                        }
                    }
                    .disabled(host.isEmpty || isFetchingCert)
                }
                if !trustedCert.isEmpty {
                    HStack {
                        Image(systemName: "checkmark.shield.fill")
                            .foregroundStyle(.green)
                        Text("Certificate pinned")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                } else {
                    HStack {
                        Image(systemName: "exclamationmark.shield.fill")
                            .foregroundStyle(.orange)
                        Text("No certificate pinned — click Fetch to auto-detect from server")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }

            Section("Password") {
                HStack {
                    Text("Status:")
                    if profile?.hasPassword == true {
                        Label("Saved in Keychain", systemImage: "checkmark.circle.fill")
                            .foregroundStyle(.green)
                    } else {
                        Label("Not set", systemImage: "xmark.circle.fill")
                            .foregroundStyle(.red)
                    }
                }

                HStack {
                    SecureField("Password", text: $password,
                        prompt: Text(isEditing ? "Leave empty to keep current" : "Enter password"))
                    Button("Save Password") {
                        guard !password.isEmpty, let id = profile?.id else { return }
                        if storeKeychainPassword(profileId: id, password: password) {
                            password = ""
                            statusMessage = "Password saved"
                            isError = false
                            state.refresh()
                        } else {
                            statusMessage = "Failed to save password"
                            isError = true
                        }
                    }
                    .disabled(password.isEmpty || profile == nil)
                }
            }

            if let msg = statusMessage {
                Text(msg)
                    .foregroundStyle(isError ? .red : .green)
                    .font(.callout)
            }
        }
        .formStyle(.grouped)
        .safeAreaInset(edge: .bottom) {
            HStack {
                if isEditing {
                    Button("Delete") {
                        guard let id = profile?.id else { return }
                        _ = state.client.deleteProfile(id: id)
                        onDone()
                    }
                    .buttonStyle(.bordered)
                    .tint(.red)
                }
                Spacer()
                Button(isEditing ? "Save" : "Create") {
                    saveProfile()
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.return, modifiers: .command)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 12)
        }
        .onAppear { loadProfile() }
        .onChange(of: profile) { _ in loadProfile() }
    }

    private func loadProfile() {
        if let p = profile {
            name = p.name; host = p.host; port = String(p.port)
            username = p.username; trustedCert = p.trustedCert
        } else {
            name = ""; host = ""; port = "443"; username = ""; trustedCert = ""
        }
        password = ""; statusMessage = nil
    }

    private func saveProfile() {
        guard !name.isEmpty, !host.isEmpty else {
            statusMessage = "Name and host are required"
            isError = true
            return
        }
        let resp = state.client.saveProfile(
            id: profile?.id, name: name, host: host,
            port: Int(port) ?? 443, username: username, trustedCert: trustedCert
        )
        if resp?.ok == true {
            if !password.isEmpty {
                if let data = resp?.data, case .object(let dict) = data,
                   case .string(let savedId) = dict["id"] {
                    _ = storeKeychainPassword(profileId: savedId, password: password)
                    password = ""
                }
            }
            statusMessage = "Saved"
            isError = false
            onDone()
        } else {
            statusMessage = resp?.message ?? "Save failed"
            isError = true
        }
    }

    private func storeKeychainPassword(profileId: String, password: String) -> Bool {
        let passwordData = password.data(using: .utf8)!
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "fortivpn-tray",
            kSecAttrAccount as String: profileId,
        ]
        let update: [String: Any] = [kSecValueData as String: passwordData]
        let status = SecItemUpdate(query as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var addQuery = query
            addQuery[kSecValueData as String] = passwordData
            return SecItemAdd(addQuery as CFDictionary, nil) == errSecSuccess
        }
        return status == errSecSuccess
    }

    private func fetchCertificate() {
        let targetHost = host
        let targetPort = UInt16(port) ?? 443

        isFetchingCert = true
        statusMessage = nil

        DispatchQueue.global().async {
            let fingerprint = fetchServerCertSHA256(host: targetHost, port: targetPort)
            DispatchQueue.main.async {
                isFetchingCert = false
                if let fp = fingerprint {
                    trustedCert = fp
                    statusMessage = "Certificate fetched — verify the fingerprint is correct"
                    isError = false
                } else {
                    statusMessage = "Could not connect to \(targetHost):\(targetPort)"
                    isError = true
                }
            }
        }
    }
}

/// Connect to a TLS server and return the SHA256 fingerprint of its certificate.
func fetchServerCertSHA256(host: String, port: UInt16) -> String? {
    // Use openssl s_client to fetch the cert — works without importing Security frameworks for TLS
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/openssl")
    process.arguments = ["s_client", "-connect", "\(host):\(port)", "-servername", host]

    let inputPipe = Pipe()
    let outputPipe = Pipe()
    process.standardInput = inputPipe
    process.standardOutput = outputPipe
    process.standardError = FileHandle.nullDevice

    do {
        try process.run()
        // Send empty input to close the connection
        inputPipe.fileHandleForWriting.write("Q\n".data(using: .utf8)!)
        inputPipe.fileHandleForWriting.closeFile()

        // Wait with timeout
        let deadline = Date(timeIntervalSinceNow: 10)
        while process.isRunning && Date() < deadline {
            Thread.sleep(forTimeInterval: 0.1)
        }
        if process.isRunning { process.terminate() }

        let data = outputPipe.fileHandleForReading.readDataToEndOfFile()
        guard let output = String(data: data, encoding: .utf8) else { return nil }

        // Extract the certificate PEM block
        guard let beginRange = output.range(of: "-----BEGIN CERTIFICATE-----"),
              let endRange = output.range(of: "-----END CERTIFICATE-----")
        else { return nil }

        let certPEM = String(output[beginRange.lowerBound...endRange.upperBound])

        // Hash it with openssl
        let hashProcess = Process()
        hashProcess.executableURL = URL(fileURLWithPath: "/usr/bin/openssl")
        hashProcess.arguments = ["x509", "-noout", "-fingerprint", "-sha256"]

        let hashInput = Pipe()
        let hashOutput = Pipe()
        hashProcess.standardInput = hashInput
        hashProcess.standardOutput = hashOutput
        hashProcess.standardError = FileHandle.nullDevice

        try hashProcess.run()
        hashInput.fileHandleForWriting.write(certPEM.data(using: .utf8)!)
        hashInput.fileHandleForWriting.closeFile()
        hashProcess.waitUntilExit()

        let hashData = hashOutput.fileHandleForReading.readDataToEndOfFile()
        guard let hashStr = String(data: hashData, encoding: .utf8) else { return nil }

        // Output: "sha256 Fingerprint=2B:6B:E4:16:CF:..."
        // Extract and clean
        if let eqRange = hashStr.range(of: "=") {
            let fingerprint = String(hashStr[eqRange.upperBound...])
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .replacingOccurrences(of: ":", with: "")
                .lowercased()
            return fingerprint
        }

        return nil
    } catch {
        return nil
    }
}
