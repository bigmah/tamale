// The keys on this iPhone: rename, remove, the Gemini key, and the Face ID setting.

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
            GeminiSection()
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

/// Your own Gemini API key, for talking about artifacts by voice.
struct GeminiSection: View {
    @AppStorage("geminiModel") private var model = ""
    @AppStorage("geminiVoice") private var voice = ""
    @State private var saved = Gemini.apiKey != nil
    @State private var pasted = ""
    @State private var models: [String] = []
    @State private var problem: String?
    @State private var checking = false

    var body: some View {
        Section {
            if saved {
                LabeledContent("API key", value: "Saved")
                Picker("Model", selection: $model) {
                    Text(models.first.map { "Automatic (\($0))" } ?? "Automatic").tag("")
                    ForEach(models, id: \.self) { Text($0).tag($0) }
                    if !model.isEmpty && !models.contains(model) { Text(model).tag(model) }
                }
                Picker("Voice", selection: $voice) {
                    Text("Gemini's choice").tag("")
                    ForEach(Gemini.voices, id: \.self) { Text($0).tag($0) }
                }
                Button("Remove Gemini key", role: .destructive) {
                    Gemini.removeKey()
                    saved = false
                    models = []
                    model = ""
                }
            } else {
                SecureField("Paste GEMINI_API_KEY", text: $pasted)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .accessibilityIdentifier("gemini-key")
                Button {
                    Task { await save() }
                } label: {
                    if checking { ProgressView() } else { Text("Save") }
                }
                .disabled(Gemini.clean(pasted).isEmpty || checking)
            }
            if let problem {
                Text(problem).font(.footnote).foregroundStyle(Theme.warning)
            }
        } header: {
            Text("Gemini")
        } footer: {
            Text("With your own key from aistudio.google.com, the waveform button on an artifact starts a voice conversation with Gemini about it. The key stays on this iPhone and goes only to Google.")
        }
        .task { if saved { await loadModels() } }
    }

    private func save() async {
        let key = Gemini.clean(pasted)
        checking = true
        defer { checking = false }
        problem = nil
        do {
            let found = try await Gemini.liveModels(apiKey: key)
            try Gemini.save(apiKey: key)
            models = found
            pasted = ""
            saved = true
            if found.isEmpty { problem = GeminiError.noModel.localizedDescription }
        } catch {
            problem = error.localizedDescription
        }
    }

    private func loadModels() async {
        guard let key = Gemini.apiKey else { return }
        do { models = try await Gemini.liveModels(apiKey: key) } catch { problem = error.localizedDescription }
    }
}
