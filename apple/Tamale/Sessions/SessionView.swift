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
    @State private var showingDiff = false
    @State private var showingStatus = false
    @State private var confirmingClose: SessionRow?
    /// Revise asked for the composer, which can't take focus until the cover over it has gone.
    @State private var focusAfterCover = false
    @FocusState private var composing: Bool

    var body: some View {
        VStack(spacing: 0) {
            if model.awaitingEntries {
                ConversationLoadBar(model: model)
            } else {
                ConnectionBanner(state: model.connection)
            }
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
                Button("Changes", systemImage: "plus.forwardslash.minus") { showingDiff = true }
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
        .sheet(isPresented: $showingDiff, onDismiss: focusIfAsked) {
            DiffScreen(model: model, ask: askAbout)
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

    /// Adds lines quoted from the diff to what's being written, as the terminal's Ask does.
    private func askAbout(_ quote: String) {
        if !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, !draft.hasSuffix("\n\n") {
            draft += draft.hasSuffix("\n") ? "\n" : "\n\n"
        }
        draft += quote
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

/// A bar with a known end, or one that keeps moving when there's no telling how far along it is.
struct LoadingBar: View {
    var fraction: Double?

    var body: some View {
        GeometryReader { space in
            ZStack(alignment: .leading) {
                Capsule().fill(Theme.surface3)
                if let fraction {
                    Capsule().fill(Theme.accent)
                        .frame(width: space.size.width * min(max(fraction, 0), 1))
                        .animation(.easeOut(duration: 0.25), value: fraction)
                } else {
                    TimelineView(.animation) { context in
                        let width = space.size.width * 0.3
                        let time = context.date.timeIntervalSinceReferenceDate
                        let phase = time.truncatingRemainder(dividingBy: 1.4) / 1.4
                        Capsule().fill(Theme.accent)
                            .frame(width: width)
                            .offset(x: (space.size.width + width) * phase - width)
                    }
                }
            }
            .clipShape(Capsule())
        }
        .frame(height: 4)
    }
}

/// While a session's conversation is on its way: how far along, and what's happening.
struct ConversationLoadBar: View {
    var model: SessionModel

    private var caption: String {
        switch model.connection {
        case .live: break
        default: return "Connecting, end-to-end encrypted…"
        }
        guard model.receivedBytes > 0 else { return "Loading the conversation…" }
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        let received = formatter.string(fromByteCount: Int64(model.receivedBytes))
        guard let expected = model.expectedBytes else { return "Loading the conversation… \(received)" }
        return "Loading the conversation… \(received) of \(formatter.string(fromByteCount: Int64(expected)))"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(caption)
                .font(.footnote.monospacedDigit())
                .foregroundStyle(Theme.dim)
            LoadingBar(fraction: model.loadFraction)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(Theme.surface)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("conversation-loading")
    }
}

/// Above the loaded part of a long conversation: fetches more when it comes into view.
struct OlderEntriesRow: View {
    var model: SessionModel

    var body: some View {
        let total = model.base + model.entries.count
        VStack(spacing: 6) {
            Text(model.loadingOlder || model.connection.isLive
                 ? "Loading earlier messages… \(model.entries.count) of \(total)"
                 : "Earlier messages load when connected")
                .font(.caption.monospacedDigit())
                .foregroundStyle(Theme.faint)
            LoadingBar(fraction: Double(model.entries.count) / Double(max(total, 1)))
                .frame(maxWidth: 200)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 8)
        // Runs again after each page while this is still on screen. The wait lets the scroll
        // position settle, so a page that pushed this off screen doesn't fetch another.
        .task(id: "\(model.base) \(model.connection.isLive)") {
            try? await Task.sleep(for: .milliseconds(250))
            if !Task.isCancelled { model.loadOlder() }
        }
    }
}

/// The entries, newest at the bottom, sticking to the bottom as they stream in. Starts at the
/// end of a long conversation, and loads earlier parts when scrolled up to them.
struct Conversation: View {
    var model: SessionModel
    var open: (ArtifactSource) -> Void
    /// Tool runs that are open, by their first entry's place in the whole conversation.
    @State private var expandedGroups: Set<Int> = []
    /// Whether the end is on screen: new entries only pull the view down if so.
    @State private var atBottom = true
    /// Going to the end, if a pin is still settling.
    @State private var pinning: Task<Void, Never>?
    /// Artifact cards run past this to the screen's edges.
    static let sidePadding: CGFloat = 14
    /// How near the start of what's loaded a row coming on screen fetches the page before it.
    static let fetchAhead = 20
    /// How close to the end counts as being at it, for following new entries.
    static let nearEnd: CGFloat = 300
    /// iOS 18 can say how far from the end the view is. Before it, all there is to go on is
    /// whether the last row is on screen.
    static var measuresScroll: Bool {
        if #available(iOS 18, *) { true } else { false }
    }

    /// Entries with runs of tool calls folded into groups, numbered by their place in the whole
    /// conversation, which stays put as earlier pages arrive.
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
        for (at, entry) in model.entries.enumerated() {
            let i = model.base + at
            if entry.kind == .tool {
                run.append(i)
                continue
            }
            // A run reaching the start of what's loaded may go on before it. It waits for the
            // page that says, or it'd change identity and lose the scroll position then.
            if let first = run.first, first != model.base || model.base == 0 { out.append(.tools(first, run)) }
            run = []
            // The startup greeting, which the empty screen says better.
            if i == 0, entry.kind == .info, entry.text?.hasPrefix("Welcome!") == true { continue }
            out.append(.entry(i))
        }
        if let first = run.first, first != model.base || model.base == 0 { out.append(.tools(first, run)) }
        return out
    }

    /// Goes to the end if that's where they are. Again shortly after too, since lazy rows only
    /// learn their heights as they're laid out, which leaves the first try short of it.
    private func pin(_ proxy: ScrollViewProxy) {
        guard atBottom else { return }
        pinning?.cancel()
        proxy.scrollTo("bottom", anchor: .bottom)
        pinning = Task {
            for wait in [100, 250, 400] {
                try? await Task.sleep(for: .milliseconds(wait))
                guard !Task.isCancelled, atBottom else { return }
                proxy.scrollTo("bottom", anchor: .bottom)
            }
        }
    }

    var body: some View {
        let items = items
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    if model.base > 0 {
                        // A new identity with each page: a scroll view holds on to the top row, and
                        // holding on to this one would push what's being read down the screen.
                        OlderEntriesRow(model: model)
                            .id("older \(model.base)")
                    }
                    if model.base == 0 && !model.awaitingEntries && model.connection.isLive
                        && model.entries.allSatisfy({ $0.kind == .info }) {
                        Text("Send a prompt to get started.")
                            .foregroundStyle(Theme.dim)
                            .frame(maxWidth: .infinity)
                            .padding(.top, 60)
                    }
                    ForEach(items) { item in
                        Group {
                            switch item {
                            case .entry(let i):
                                EntryView(entry: model.entries[i - model.base], model: model, open: open)
                            case .tools(let first, let indices):
                                ToolGroup(entries: indices.map { model.entries[$0 - model.base] }, expanded: Binding(
                                    get: { expandedGroups.contains(first) },
                                    set: { if $0 { expandedGroups.insert(first) } else { expandedGroups.remove(first) } }))
                            }
                        }
                        // Fetches ahead, so a page usually lands before it's scrolled up to.
                        .onAppear {
                            if item.id - model.base < Self.fetchAhead { model.loadOlder() }
                        }
                    }
                    if model.busy {
                        WorkingIndicator(status: model.status)
                    }
                    Color.clear.frame(height: 1).id("bottom")
                        .onAppear { if !Self.measuresScroll { atBottom = true } }
                        .onDisappear { if !Self.measuresScroll { atBottom = false } }
                }
                .padding(.horizontal, Self.sidePadding)
                .padding(.vertical, 12)
            }
            .defaultScrollAnchor(.bottom)
            .scrollDismissesKeyboard(.interactively)
            // What moves the end: entries changing, and the view's own size (the keyboard, an
            // approval card). Going by these rather than by the content's size, which scrolling
            // itself changes as lazy rows measure, it can't chase its own tail.
            .onChange(of: model.entries.last) { pin(proxy) }
            .onChange(of: model.base + model.entries.count) { pin(proxy) }
            .onChange(of: model.approval == nil) { pin(proxy) }
            .onChange(of: model.question == nil) { pin(proxy) }
            .onChange(of: model.promptsSent) {
                atBottom = true
                pin(proxy)
            }
            .onChange(of: model.awaitingEntries) { _, waiting in
                if !waiting { pin(proxy) }
            }
            .modifier(TracksEnd(near: Self.nearEnd, atBottom: $atBottom) { pin(proxy) })
        }
    }
}

/// Keeps `atBottom` up to date, where the system can say how far off the end is (see
/// `Conversation.measuresScroll`). Only their own scrolling changes it: rows settling their
/// heights and replies growing move the end too, and don't mean they've gone anywhere. Calls
/// `resized` when the view's height changes.
struct TracksEnd: ViewModifier {
    var near: CGFloat
    @Binding var atBottom: Bool
    var resized: () -> Void
    @State private var userScrolling = false

    func body(content: Content) -> some View {
        if #available(iOS 18, *) {
            content.onScrollPhaseChange { _, phase in
                userScrolling = phase == .tracking || phase == .interacting || phase == .decelerating
            }
            .onScrollGeometryChange(for: CGFloat.self) { geometry in
                geometry.contentSize.height + geometry.contentInsets.bottom
                    - geometry.contentOffset.y - geometry.containerSize.height
            } action: { _, fromEnd in
                if userScrolling { atBottom = fromEnd < near }
            }
            .onScrollGeometryChange(for: CGFloat.self) { $0.containerSize.height } action: { _, _ in
                resized()
            }
        } else {
            content
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
