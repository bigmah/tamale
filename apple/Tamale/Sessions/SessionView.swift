// One session: the conversation, its status, any approval or questions waiting, and the
// composer.

import SwiftUI
import TamaleKit

struct SessionScreen: View {
    var sid: String
    @Environment(AppModel.self) private var app

    var body: some View {
        if let row = app.row(sid) {
            SessionView(row: row, model: app.model(for: row))
        } else {
            ContentUnavailableView("Session not found", systemImage: "questionmark.circle",
                                   description: Text("It may have ended, or its key was removed."))
        }
    }
}

struct SessionView: View {
    var row: SessionRow
    @Bindable var model: SessionModel
    @Environment(AppModel.self) private var app
    @Environment(\.scenePhase) private var phase
    @State private var draft = ""
    @State private var viewing: ArtifactSource?
    @State private var showingGallery = false
    @State private var showingStatus = false
    @State private var confirmingClose: SessionRow?
    /// Revise asked for the composer, which can't take focus until the cover over it has gone.
    @State private var focusAfterCover = false
    @FocusState private var composing: Bool

    var body: some View {
        VStack(spacing: 0) {
            ConnectionBanner(state: model.connection)
            Conversation(model: model, open: { viewing = $0 })
            if let approval = model.approval {
                ApprovalCard(approval: approval) { choice, reason in
                    model.approve(choice, reason: reason)
                }
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }
            if let question = model.question {
                QuestionCard(asked: question) { answers in
                    model.answer(answers)
                }
                // Fresh picks for each new set of questions.
                .id(question)
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }
            Composer(model: model, draft: $draft, focused: $composing)
        }
        .animation(.snappy, value: model.approval)
        .animation(.snappy, value: model.question)
        // With the keyboard up as well, there'd be no conversation left to see what it's about.
        .onChange(of: model.approval == nil) { _, none in
            if !none { composing = false }
        }
        .onChange(of: model.question == nil) { _, none in
            if !none { composing = false }
        }
        .background(Theme.background)
        .navigationTitle(model.title.isEmpty ? row.shownTitle : model.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 0) {
                    Text(model.title.isEmpty ? row.shownTitle : model.title)
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                    Text(row.place)
                        .font(.caption2.monospaced())
                        .foregroundStyle(Theme.dim)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                .onTapGesture { showingStatus = true }
            }
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Button("Artifacts gallery", systemImage: "sparkles.rectangle.stack") { showingGallery = true }
                    Button("Session details", systemImage: "info.circle") { showingStatus = true }
                    if model.busy {
                        Button("Interrupt", systemImage: "stop.circle", role: .destructive) { model.interrupt() }
                    }
                    Divider()
                    Button("Close Session", systemImage: "xmark.circle", role: .destructive) {
                        var current = row
                        current.session.busy = model.busy
                        confirmingClose = current
                    }
                    .disabled(app.closing.contains(row.id))
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
        .onAppear { model.start() }
        .onDisappear {
            // Covers and sheets hide this view too; only leaving the session disconnects.
            if !app.path.contains(.session(row.id)) { model.stop() }
        }
        .onChange(of: phase) { _, phase in
            if phase == .active { model.resume() }
        }
        .fullScreenCover(item: $viewing, onDismiss: focusIfAsked) { source in
            ArtifactViewer(source: source, revise: revise)
        }
        .sheet(isPresented: $showingGallery, onDismiss: focusIfAsked) {
            GalleryView(model: model, revise: revise)
        }
        .closeSessionDialog(row: $confirmingClose)
        .sheet(isPresented: $showingStatus) {
            StatusSheet(row: row, model: model)
                .presentationDetents([.medium, .large])
        }
    }

    private func revise(_ prompt: String) {
        draft = prompt
        focusAfterCover = true
    }

    private func focusIfAsked() {
        if focusAfterCover { composing = true }
        focusAfterCover = false
    }
}

struct ConnectionBanner: View {
    var state: ChannelState

    var body: some View {
        switch state {
        case .live, .idle:
            EmptyView()
        case .connecting, .handshaking:
            banner("Connecting, end-to-end encrypted…", icon: "lock", color: Theme.dim, spinning: true)
        case .retrying(let why):
            banner(why, icon: "wifi.exclamationmark", color: Theme.warning, spinning: true)
        case .stopped(let why):
            banner(why, icon: "xmark.octagon", color: Theme.error, spinning: false)
        }
    }

    func banner(_ text: String, icon: String, color: Color, spinning: Bool) -> some View {
        HStack(spacing: 8) {
            if spinning { ProgressView().controlSize(.small) } else { Image(systemName: icon) }
            Text(text).font(.footnote).lineLimit(3)
            Spacer(minLength: 0)
        }
        .foregroundStyle(color)
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(Theme.surface)
    }
}

/// The entries, newest at the bottom, sticking to the bottom as they stream in.
struct Conversation: View {
    var model: SessionModel
    var open: (ArtifactSource) -> Void
    @State private var expandedGroups: Set<Int> = []
    /// Artifact cards run past this to the screen's edges.
    static let sidePadding: CGFloat = 14

    /// Entries with runs of tool calls folded into groups.
    enum Item: Identifiable {
        case entry(Int)
        case tools(Int, [Int])
        var id: Int {
            switch self {
            case .entry(let i): i
            case .tools(let first, _): first
            }
        }
    }

    var items: [Item] {
        var out: [Item] = []
        var run: [Int] = []
        for (i, entry) in model.entries.enumerated() {
            if entry.kind == .tool {
                run.append(i)
                continue
            }
            if let first = run.first { out.append(.tools(first, run)) }
            run = []
            // The startup greeting, which the empty screen says better.
            if i == 0, entry.kind == .info, entry.text?.hasPrefix("Welcome!") == true { continue }
            out.append(.entry(i))
        }
        if let first = run.first { out.append(.tools(first, run)) }
        return out
    }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    if model.entries.allSatisfy({ $0.kind == .info }) && model.connection.isLive {
                        Text("Send a prompt to get started.")
                            .foregroundStyle(Theme.dim)
                            .frame(maxWidth: .infinity)
                            .padding(.top, 60)
                    }
                    ForEach(items) { item in
                        switch item {
                        case .entry(let i):
                            EntryView(entry: model.entries[i], model: model, open: open)
                        case .tools(let first, let indices):
                            ToolGroup(entries: indices.map { model.entries[$0] }, expanded: Binding(
                                get: { expandedGroups.contains(first) },
                                set: { if $0 { expandedGroups.insert(first) } else { expandedGroups.remove(first) } }))
                        }
                    }
                    if model.busy {
                        WorkingIndicator(status: model.status)
                    }
                    Color.clear.frame(height: 1).id("bottom")
                }
                .padding(.horizontal, Self.sidePadding)
                .padding(.vertical, 12)
            }
            .defaultScrollAnchor(.bottom)
            .scrollDismissesKeyboard(.interactively)
            .onChange(of: model.entries.count) {
                withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
            }
        }
    }
}

struct WorkingIndicator: View {
    var status: SessionStatus?

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text(status?.compacting == true ? "Compacting" : "Working")
                if let since = status?.since {
                    Text(formatElapsed(max(0, Int(context.date.timeIntervalSince1970 - since))))
                        .monospacedDigit()
                }
            }
            .font(.footnote)
            .foregroundStyle(Theme.dim)
        }
    }
}

struct StatusSheet: View {
    var row: SessionRow
    var model: SessionModel

    var body: some View {
        NavigationStack {
            List {
                Section("Session") {
                    LabeledContent("Where", value: row.place)
                    if let status = model.status {
                        if !status.project.isEmpty { LabeledContent("Project", value: status.project) }
                        LabeledContent("Model", value: status.model)
                        if !status.backend.isEmpty { LabeledContent("Backend", value: status.backend) }
                        if !status.mode.isEmpty { LabeledContent("Permissions", value: status.mode) }
                        if let effort = status.effort { LabeledContent("Effort", value: effort) }
                        if let goal = status.goal { LabeledContent("Goal", value: goal) }
                    }
                    LabeledContent("Key", value: row.account.name)
                    LabeledContent("Connection") {
                        Text(model.connection.isLive ? "End-to-end encrypted" : "Not connected")
                    }
                }
                if let status = model.status {
                    Section("Usage") {
                        if status.window > 0 {
                            LabeledContent("Context", value: "\(formatTokens(status.context)) of \(formatTokens(status.window)) (\(status.context * 100 / max(status.window, 1))%)")
                        }
                        LabeledContent("Tokens", value: "\(formatTokens(status.inputTokens)) in, \(formatTokens(status.outputTokens)) out")
                        if status.cost > 0 {
                            LabeledContent("Cost", value: String(format: "$%.2f%@", status.cost, status.costPartial ? "+" : ""))
                        }
                    }
                    if !status.queued.isEmpty {
                        Section("Queued") {
                            ForEach(status.queued, id: \.self) { Text($0) }
                        }
                    }
                }
            }
            .navigationTitle("Details")
            .navigationBarTitleDisplayMode(.inline)
        }
    }
}
