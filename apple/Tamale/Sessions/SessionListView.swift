// Every key's sessions, as the hub's sidebar lists them: waiting for approval first, then
// busy, then online, then by recent activity. Refreshes every few seconds while on screen.

import SwiftUI
import TamaleKit

struct SessionListView: View {
    @Environment(AppModel.self) private var app
    @State private var adding = false
    @State private var confirmingClose: SessionRow?

    var body: some View {
        Group {
            if app.loaded && app.accounts.isEmpty {
                WelcomeView(add: { adding = true })
            } else {
                list
            }
        }
        .background(Theme.background)
        .navigationTitle("Sessions")
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                NavigationLink(value: Route.library) {
                    Label("Library", systemImage: "books.vertical")
                }
            }
            ToolbarItemGroup(placement: .topBarTrailing) {
                NavigationLink(value: Route.keys) {
                    Label("Keys", systemImage: "key")
                }
                Button("Add", systemImage: "qrcode.viewfinder") { adding = true }
            }
        }
        .sheet(isPresented: $adding, onDismiss: { Task { await app.openPending() } }) {
            AddKeyView()
        }
        .task(id: app.accounts.map(\.id)) {
            // Polls while the list is on screen (the task ends when it goes away).
            while !Task.isCancelled {
                await app.refresh()
                try? await Task.sleep(for: .seconds(4))
            }
        }
    }

    private var chefs: [SessionRow] { app.rows.filter(\.isChef) }

    private func sessionLink(_ row: SessionRow) -> some View {
        NavigationLink(value: Route.session(row.id)) {
            SessionRowView(row: row, showKey: app.accounts.count > 1,
                           closing: app.closing.contains(row.id),
                           stop: row.isChef && row.session.online && row.session.e2e
                               ? { confirmingClose = row } : nil)
        }
        .listRowBackground(Theme.surface)
        .swipeActions(edge: .trailing) {
            if row.session.online && row.session.e2e {
                Button(row.isChef ? "Stop" : "Close",
                       systemImage: row.isChef ? "power" : "xmark.circle") { confirmingClose = row }
                    .tint(Theme.error)
            }
        }
        .contextMenu {
            if row.session.online && row.session.e2e {
                Button(row.isChef ? "Stop Chef" : "Close Session",
                       systemImage: row.isChef ? "power" : "xmark.circle", role: .destructive) {
                    confirmingClose = row
                }
            }
        }
    }

    private var list: some View {
        List {
            ForEach(app.accounts) { account in
                if let problem = app.problems[account.id] {
                    Label {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("\(account.name) on \(account.hubName)").font(.subheadline.weight(.medium))
                            Text(problem).font(.caption).foregroundStyle(Theme.dim)
                        }
                    } icon: {
                        Image(systemName: "exclamationmark.triangle").foregroundStyle(Theme.warning)
                    }
                    .listRowBackground(Theme.surface)
                }
            }
            if app.rows.isEmpty {
                ContentUnavailableView {
                    Label(app.refreshing ? "Looking for sessions…" : "No sessions", systemImage: "terminal")
                } description: {
                    Text("Start tamale and type /remote to make a session reachable from here.")
                }
                .listRowBackground(Color.clear)
            } else {
                // The chef is a switch, as on the hub's page: on with a way to stop it, or off.
                Section {
                    if chefs.isEmpty {
                        ChefOffRow().listRowBackground(Theme.surface)
                    }
                    ForEach(chefs) { row in
                        sessionLink(row)
                    }
                } header: {
                    Text("Chef")
                }
                Section {
                    ForEach(app.rows.filter { !$0.isChef }) { row in
                        sessionLink(row)
                    }
                } header: {
                    Text("Sessions")
                }
            }
        }
        .scrollContentBackground(.hidden)
        .refreshable { await app.refresh() }
        .closeSessionDialog(row: $confirmingClose)
    }
}

extension View {
    /// Asks before closing `row`'s session, then closes it.
    func closeSessionDialog(row: Binding<SessionRow?>) -> some View {
        modifier(CloseSessionDialog(row: row))
    }
}

struct CloseSessionDialog: ViewModifier {
    @Binding var row: SessionRow?
    @Environment(AppModel.self) private var app

    func body(content: Content) -> some View {
        content
            .confirmationDialog(
                row?.isChef == true ? "Stop the tamale chef?" : "Close this session?",
                isPresented: Binding(get: { row != nil }, set: { if !$0 { row = nil } }),
                titleVisibility: .visible,
                presenting: row
            ) { row in
                Button(row.isChef ? "Stop Chef" : row.session.busy ? "Stop and Close" : "Close Session",
                       role: .destructive) {
                    Task { await app.close(row) }
                }
            } message: { row in
                Text(row.isChef
                     ? "It stays saved on \(row.label.host.isEmpty ? "its machine" : row.label.host), and your other sessions keep running. tamale chef or /chef starts it again."
                     : row.session.busy
                     ? "It's working, and stops mid-turn. tamale ends it on \(row.label.host.isEmpty ? "its machine" : row.label.host), where it stays saved: tamale --continue picks it back up."
                     : "tamale ends it on \(row.label.host.isEmpty ? "its machine" : row.label.host), where it stays saved: tamale --continue picks it back up.")
            }
    }
}

struct SessionRowView: View {
    var row: SessionRow
    var showKey: Bool
    var closing = false
    /// The chef's Stop button, when it can be stopped from here.
    var stop: (() -> Void)?

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            if row.isChef {
                Text("🫔").font(.body).padding(.top, 1)
            } else {
                StatusDot(session: row.session)
                    .padding(.top, 6)
            }
            VStack(alignment: .leading, spacing: 3) {
                Text(row.shownTitle)
                    .font(.body.weight(.medium))
                    .foregroundStyle(row.session.online ? Theme.bright : Theme.dim)
                    .lineLimit(2)
                Text(row.place)
                    .font(.footnote.monospaced())
                    .foregroundStyle(Theme.dim)
                    .lineLimit(1)
                    .truncationMode(.head)
                HStack(spacing: 6) {
                    if row.isChef {
                        ChefSwitch(on: row.session.online && !closing)
                    }
                    if closing {
                        Badge(text: row.isChef ? "Stopping…" : "Closing…", color: Theme.faint)
                    } else if row.session.approval {
                        Badge(text: "Needs approval", color: Theme.warning)
                    } else if row.session.busy {
                        Badge(text: "Working", color: Theme.accent)
                    } else if !row.session.online {
                        Badge(text: "Offline", color: Theme.faint)
                    }
                    if !row.model.isEmpty {
                        Text(row.model).lineLimit(1)
                    }
                    Spacer(minLength: 0)
                    Text(formatIdle(row.session.idle))
                    if showKey {
                        Text("· \(row.account.name)").lineLimit(1)
                    }
                }
                .font(.caption)
                .foregroundStyle(Theme.faint)
            }
            if let stop {
                Button(closing ? "Stopping" : "Stop", systemImage: "power", action: stop)
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .tint(Theme.chef)
                    .disabled(closing)
                    .padding(.top, 2)
            }
        }
        .padding(.vertical, 4)
    }
}

/// The chef's on/off state, the same capsule as the hub's page.
struct ChefSwitch: View {
    var on: Bool

    var body: some View {
        Text(on ? "ON" : "OFF")
            .font(.caption2.weight(.bold))
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .foregroundStyle(on ? Color(hex: 0x1F1A12) : Theme.dim)
            .background(on ? Theme.chef : Theme.surface3, in: Capsule())
            .accessibilityLabel(on ? "Chef on" : "Chef off")
    }
}

/// Where the chef goes when none is running: off, and how to turn it on.
struct ChefOffRow: View {
    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Text("🫔").font(.body).grayscale(1).opacity(0.7).padding(.top, 1)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 8) {
                    Text("tamale chef")
                        .font(.body.weight(.medium))
                        .foregroundStyle(Theme.dim)
                    ChefSwitch(on: false)
                }
                Text("Start it on one of your machines with tamale chef, or /chef in any session. It looks after every session on your key.")
                    .font(.footnote)
                    .foregroundStyle(Theme.faint)
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
    }
}

struct StatusDot: View {
    var session: HubSession

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 9, height: 9)
            .overlay {
                if session.busy && !session.approval {
                    Circle().stroke(color.opacity(0.5), lineWidth: 3).scaleEffect(1.8)
                }
            }
    }

    var color: Color {
        if !session.online { return Theme.faint.opacity(0.6) }
        if session.approval { return Theme.warning }
        if session.busy { return Theme.accent }
        return Theme.added
    }
}

struct Badge: View {
    var text: String
    var color: Color

    var body: some View {
        Text(text)
            .font(.caption2.weight(.semibold))
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .foregroundStyle(color)
            .background(color.opacity(0.15), in: Capsule())
    }
}

struct WelcomeView: View {
    var add: () -> Void

    var body: some View {
        VStack(spacing: 20) {
            Spacer()
            Text("🫔").font(.system(size: 72))
            Text("tamale").font(.largeTitle.weight(.semibold))
            Text("See what your sessions are doing, answer approvals and send prompts from your phone.\n\nAdd the key from your hub's site: open it on a computer and scan its QR code, or paste the key.")
                .multilineTextAlignment(.center)
                .foregroundStyle(Theme.dim)
                .padding(.horizontal, 32)
            Button(action: add) {
                Label("Add a key", systemImage: "qrcode.viewfinder")
                    .frame(maxWidth: 260)
                    .padding(.vertical, 6)
            }
            .buttonStyle(.borderedProminent)
            Spacer()
            Spacer()
        }
        .frame(maxWidth: .infinity)
    }
}
