import SwiftUI

struct SettingsView: View {
    @ObservedObject var state: VPNState
    @State private var selectedProfileId: String?
    @State private var isCreatingNew = false

    var body: some View {
        NavigationSplitView {
            List(selection: $selectedProfileId) {
                Section("Profiles") {
                    ForEach(state.profiles) { profile in
                        HStack {
                            Circle()
                                .fill(profile.hasPassword ? .green : .red)
                                .frame(width: 8, height: 8)
                            Text(profile.name)
                        }
                        .tag(profile.id)
                    }
                }
            }
            .listStyle(.sidebar)
            .frame(minWidth: 160)
            .toolbar {
                ToolbarItem {
                    Button(action: {
                        isCreatingNew = true
                        selectedProfileId = nil
                    }) {
                        Label("New Profile", systemImage: "plus")
                    }
                }
            }
            .onChange(of: selectedProfileId) { _ in
                isCreatingNew = false
            }
        } detail: {
            if isCreatingNew {
                ProfileFormView(state: state, profile: nil, onDone: {
                    isCreatingNew = false
                    state.refresh()
                })
            } else if let id = selectedProfileId,
                      let profile = state.profiles.first(where: { $0.id == id }) {
                ProfileFormView(state: state, profile: profile, onDone: {
                    state.refresh()
                })
            } else {
                VStack {
                    Image(systemName: "network")
                        .font(.largeTitle)
                        .foregroundStyle(.secondary)
                    Text("No Profile Selected")
                        .font(.title2)
                    Text("Select a profile or create a new one")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .frame(minWidth: 550, minHeight: 400)
        .onAppear {
            state.refresh()
            // Auto-select first profile
            if selectedProfileId == nil, let first = state.profiles.first {
                selectedProfileId = first.id
            }
        }
    }
}
