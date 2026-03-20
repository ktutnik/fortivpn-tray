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

    private var isEditing: Bool { profile != nil }

    var body: some View {
        Form {
            Section("Connection") {
                TextField("Name", text: $name, prompt: Text("My VPN"))
                TextField("Host", text: $host, prompt: Text("vpn.example.com"))
                TextField("Port", text: $port, prompt: Text("443"))
                TextField("Username", text: $username, prompt: Text("user@company.com"))
                TextField("Certificate SHA256", text: $trustedCert, prompt: Text("Optional"))
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
                        let resp = state.client.setPassword(id: id, password: password)
                        if resp?.ok == true {
                            password = ""
                            statusMessage = "Password saved"
                            isError = false
                            state.refresh()
                        } else {
                            statusMessage = resp?.message ?? "Failed"
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
        .toolbar {
            ToolbarItemGroup {
                if isEditing {
                    Button("Delete", role: .destructive) {
                        guard let id = profile?.id else { return }
                        _ = state.client.deleteProfile(id: id)
                        onDone()
                    }
                }
                Button(isEditing ? "Save" : "Create") {
                    saveProfile()
                }
                .buttonStyle(.borderedProminent)
            }
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
            // Save password if provided
            if !password.isEmpty {
                // Get the saved profile ID from response
                if let data = resp?.data, case .object(let dict) = data,
                   case .string(let savedId) = dict["id"] {
                    _ = state.client.setPassword(id: savedId, password: password)
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
}
