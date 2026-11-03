// Adding a key: scan the QR code on the hub's site (`https://<hub>/#thk_…`) or paste the key.
// A terminal's session code (`https://<hub>/open/<sid>`) scanned here opens that session.

import SwiftUI
import TamaleKit
import VisionKit

struct AddKeyView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var hub = "https://tamale-hub.com"
    @State private var name = ""
    @State private var scanning = DataScannerViewController.isSupported
    @State private var working = false
    @State private var problem: String?
    /// A terminal's session code, scanned before this iPhone had the key for its hub.
    @State private var waitingSession: SessionLink?

    private var link: KeyLink? { KeyLink(text) }

    var body: some View {
        NavigationStack {
            Form {
                if scanning && DataScannerViewController.isAvailable {
                    Section {
                        QRScanner { payload in scanned(payload) }
                            .frame(height: 300)
                            .clipShape(RoundedRectangle(cornerRadius: 10))
                            .listRowInsets(EdgeInsets())
                        } footer: {
                        Text("Point the camera at your key's QR code on the hub's site, or at the session code /remote shows in the terminal.")
                    }
                }
                if let waitingSession {
                    Section {
                        Label {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("That's a session's code").font(.subheadline.weight(.semibold))
                                Text("It opens the session once this iPhone has the key, which the terminal's code doesn't carry. Scan the key's code on \(waitingSession.hub.host ?? "the hub")'s site (Show), or paste the key (`TAMALE_HUB_KEY`, thk_…). The session opens after.")
                                    .font(.footnote)
                                    .foregroundStyle(Theme.dim)
                            }
                        } icon: {
                            Image(systemName: "key.viewfinder").foregroundStyle(Theme.accent)
                        }
                    }
                }
                Section {
                    TextField("thk_… or https://…/#thk_…", text: $text, axis: .vertical)
                        .font(.body.monospaced())
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .lineLimit(1...3)
                    if link != nil && link?.hub == nil {
                        TextField("Hub", text: $hub)
                            .keyboardType(.URL)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    }
                    TextField("Name (optional)", text: $name)
                } header: {
                    Text("Key")
                } footer: {
                    if !text.isEmpty && link == nil {
                        Text("That isn't a key. Keys start with thk_ and have 52 more letters and digits.")
                            .foregroundStyle(Theme.error)
                    } else {
                        Text("The key stays on this iPhone, in the Keychain. The hub only ever sees a value derived from it.")
                    }
                }
                if let problem {
                    Section {
                        Label(problem, systemImage: "exclamationmark.triangle")
                            .foregroundStyle(Theme.warning)
                    }
                }
            }
            .onChange(of: text) {
                // A session link pasted instead of a key.
                if let session = SessionLink(text) {
                    openSession(session)
                    // Cleared on the next turn: a field being edited ignores a change made
                    // while it reports one.
                    Task { @MainActor in text = "" }
                }
            }
            .navigationTitle("Add a key")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if working {
                        ProgressView()
                    } else {
                        Button("Add") { Task { await add() } }
                            .disabled(link == nil)
                    }
                }
            }
        }
    }

    private func scanned(_ payload: String) {
        if let session = SessionLink(payload) {
            openSession(session)
            return
        }
        guard KeyLink(payload) != nil, !working else { return }
        text = payload
        scanning = false
        Task { await add() }
    }

    /// Opens a terminal's session code: now if a key for its hub is here, else once one is added.
    private func openSession(_ session: SessionLink) {
        if app.hasKey(for: session.hub) {
            app.pendingLink = session
            dismiss()
        } else {
            waitingSession = session
            hub = session.hub.absoluteString
            problem = nil
        }
    }

    private func add() async {
        guard let link else { return }
        let hubURL = link.hub ?? URL(string: hub.trimmingCharacters(in: .whitespaces))?.origin
        guard let hubURL else {
            problem = "That hub address isn't a web address."
            return
        }
        working = true
        problem = nil
        defer { working = false }
        do {
            let label = name.trimmingCharacters(in: .whitespaces)
            try await app.add(link.key, hub: hubURL, name: label.isEmpty ? hubURL.host ?? "Key" : label)
            if let waitingSession { app.pendingLink = waitingSession }
            dismiss()
        } catch {
            problem = error.localizedDescription
        }
    }
}

/// VisionKit's live QR scanner.
struct QRScanner: UIViewControllerRepresentable {
    var found: (String) -> Void

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let scanner = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            qualityLevel: .balanced,
            recognizesMultipleItems: false,
            isHighFrameRateTrackingEnabled: false,
            isHighlightingEnabled: true)
        scanner.delegate = context.coordinator
        try? scanner.startScanning()
        return scanner
    }

    func updateUIViewController(_ controller: DataScannerViewController, context: Context) {
        context.coordinator.found = found
    }

    static func dismantleUIViewController(_ controller: DataScannerViewController, coordinator: Coordinator) {
        controller.stopScanning()
    }

    func makeCoordinator() -> Coordinator { Coordinator(found: found) }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        var found: (String) -> Void
        private var last: String?

        init(found: @escaping (String) -> Void) { self.found = found }

        func dataScanner(_ scanner: DataScannerViewController, didAdd items: [RecognizedItem], allItems: [RecognizedItem]) {
            for case .barcode(let code) in items {
                guard let payload = code.payloadStringValue, payload != last else { continue }
                last = payload
                found(payload)
            }
        }

        func dataScanner(_ scanner: DataScannerViewController, didTapOn item: RecognizedItem) {
            if case .barcode(let code) = item, let payload = code.payloadStringValue { found(payload) }
        }
    }
}
