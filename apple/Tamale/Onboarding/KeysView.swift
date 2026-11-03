// The keys on this iPhone: rename, remove, and the Face ID setting.

import SwiftUI
import TamaleKit

struct KeysView: View {
    @Environment(AppModel.self) private var app
    @AppStorage("requireFaceID") private var requireFaceID = false
    @State private var adding = false
    @State private var renaming: Account?
    @State private var newName = ""
    @State private var removing: Account?

    var body: some View {
        List {
            Section {
                ForEach(app.accounts) { account in
                    VStack(alignment: .leading, spacing: 3) {
                        Text(account.name).font(.body.weight(.medium))
                        Text(account.hubName).font(.footnote).foregroundStyle(Theme.dim)
                        Text("id \(account.key.id)").font(.caption.monospaced()).foregroundStyle(Theme.faint)
                        if let problem = app.problems[account.id] {
                            Text(problem).font(.caption).foregroundStyle(Theme.warning)
                        }
                    }
                    .swipeActions {
                        Button("Remove", role: .destructive) { removing = account }
                        Button("Rename") {
                            newName = account.name
                            renaming = account
                        }
                    }
                    .contextMenu {
                        Button("Rename", systemImage: "pencil") {
                            newName = account.name
                            renaming = account
                        }
                        Button("Remove from this iPhone", systemImage: "trash", role: .destructive) { removing = account }
                    }
                }
                Button("Add a key", systemImage: "plus") { adding = true }
            } header: {
                Text("Keys")
            } footer: {
                Text("Removing a key only forgets it here. To revoke it everywhere, rotate or delete it on the hub's site.")
            }
            Section {
                Toggle("Require Face ID", isOn: $requireFaceID)
            } footer: {
                Text("Asks for Face ID or your passcode whenever tamale opens.")
            }
        }
        .navigationTitle("Keys")
        .sheet(isPresented: $adding, onDismiss: { Task { await app.openPending() } }) { AddKeyView() }
        .alert("Rename key", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("Name", text: $newName)
            Button("Save") {
                if let renaming, !newName.trimmingCharacters(in: .whitespaces).isEmpty {
                    app.rename(renaming, to: newName.trimmingCharacters(in: .whitespaces))
                }
                renaming = nil
            }
            Button("Cancel", role: .cancel) { renaming = nil }
        }
        .confirmationDialog(
            "Remove \(removing?.name ?? "this key") from this iPhone?",
            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
            titleVisibility: .visible
        ) {
            Button("Remove", role: .destructive) {
                if let removing { app.remove(removing) }
                removing = nil
            }
        } message: {
            Text("You'll need to scan or paste it again to see its sessions.")
        }
    }
}
