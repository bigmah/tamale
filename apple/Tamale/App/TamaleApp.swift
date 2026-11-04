// tamale for iPhone: a remote control for tamale sessions reached through a tamale-hub.
// See apple/ios.md.

import LocalAuthentication
import SwiftData
import SwiftUI
import TamaleKit

@main
struct TamaleApp: App {
    let container: ModelContainer
    @State private var app = AppModel()

    init() {
        container = LibraryStore.open()
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(app)
                .preferredColorScheme(.dark)
                .tint(Theme.accent)
                .onAppear { app.load(container.mainContext) }
        }
        .modelContainer(container)
    }
}

struct RootView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.scenePhase) private var phase
    @AppStorage("requireFaceID") private var requireFaceID = false
    @State private var locked = true
    @State private var unlocking = false

    var body: some View {
        @Bindable var app = app
        NavigationStack(path: $app.path) {
            SessionListView()
                .navigationDestination(for: Route.self) { route in
                    switch route {
                    case .session(let sid): SessionScreen(sid: sid)
                    case .library: LibraryView()
                    case .keys: KeysView()
                    }
                }
        }
        .overlay {
            if requireFaceID && locked {
                LockView(unlock: unlock)
            }
        }
        .onAppear {
            if requireFaceID { unlock() } else { locked = false }
        }
        .onChange(of: phase) { _, phase in
            if phase == .background && requireFaceID { locked = true }
            if phase == .active && requireFaceID && locked { unlock() }
        }
        .onOpenURL { url in handle(url) }
        .onContinueUserActivity(NSUserActivityTypeBrowsingWeb) { activity in
            if let url = activity.webpageURL { handle(url) }
        }
        .alert("Can't open that link", isPresented: Binding(
            get: { app.linkProblem != nil }, set: { if !$0 { app.linkProblem = nil } })
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(app.linkProblem ?? "")
        }
        .alert("Couldn't close the session", isPresented: Binding(
            get: { app.closeProblem != nil }, set: { if !$0 { app.closeProblem = nil } })
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(app.closeProblem ?? "")
        }
    }

    /// `https://<hub>/open/<sid>` (a universal link or the terminal's QR code), or
    /// `tamale://open?url=<that link>`.
    private func handle(_ url: URL) {
        var target = url
        if url.scheme == "tamale",
           let inner = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first(where: { $0.name == "url" })?.value,
           let parsed = URL(string: inner) {
            target = parsed
        }
        guard let link = SessionLink(target) else { return }
        Task { await app.open(link) }
    }

    private func unlock() {
        guard !unlocking else { return }
        unlocking = true
        let context = LAContext()
        var error: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &error) else {
            locked = false
            unlocking = false
            return
        }
        context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: "Show your tamale sessions") { ok, _ in
            Task { @MainActor in
                if ok { locked = false }
                unlocking = false
            }
        }
    }
}

struct LockView: View {
    var unlock: () -> Void

    var body: some View {
        ZStack {
            Theme.background.ignoresSafeArea()
            VStack(spacing: 16) {
                Text("🫔").font(.system(size: 64))
                Button("Unlock", systemImage: "faceid", action: unlock)
                    .buttonStyle(.borderedProminent)
            }
        }
    }
}
