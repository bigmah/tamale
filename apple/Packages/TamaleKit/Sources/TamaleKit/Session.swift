// One session as the app shows it: the conversation, status and approval, kept in step with
// tamale's /web messages over a `SessionChannel`.

import Foundation
import Observation

@MainActor
@Observable
public final class SessionModel {
    public let sid: String
    /// The part of the conversation loaded so far: its end, back to `base`.
    public private(set) var entries: [Entry] = []
    /// Where `entries` starts in the whole conversation; above 0, there's older history to get.
    public private(set) var base = 0
    /// A page of older history is on its way.
    public private(set) var loadingOlder = false
    /// Connected or connecting, and yet to get the conversation.
    public private(set) var awaitingEntries = false
    /// How much of the conversation has arrived, while `awaitingEntries`.
    public private(set) var receivedBytes = 0
    /// How big it is, if tamale said (older ones don't).
    public private(set) var expectedBytes: Int?
    /// Prompts sent from here, so the view can go to the end for what comes back.
    public private(set) var promptsSent = 0
    /// The latest `diff` reply, and whether a newer one has been asked for.
    public private(set) var diff: DiffReply?
    public private(set) var loadingDiff = false
    public private(set) var status: SessionStatus?
    public private(set) var approval: Approval?
    /// The agent's questions, waiting for answers.
    public private(set) var question: QuestionSet?
    public private(set) var connection: ChannelState = .idle
    /// Artifact pages tamale sent, by id.
    public private(set) var pages: [String: ArtifactReply] = [:]
    /// The latest gallery (`artifacts` reply).
    public private(set) var gallery: Gallery?
    /// Ids asked for and not yet answered.
    @ObservationIgnored private var due: Set<String> = []
    @ObservationIgnored private var gallerySeq = 0
    @ObservationIgnored private var diffSeq = 0
    /// The next `entries` is a new connection's first, not an update.
    @ObservationIgnored private var welcomePending = false
    /// Called with each artifact page as it arrives (the app keeps them in its library).
    @ObservationIgnored public var onArtifact: (ArtifactReply) -> Void = { _ in }

    @ObservationIgnored public let channel: SessionChannel?

    public init(channel: SessionChannel) {
        self.channel = channel
        sid = channel.sid
        channel.onState = { [weak self] state in self?.stateChanged(state) }
        channel.onMessage = { [weak self] message in self?.apply(message) }
        channel.onProgress = { [weak self] bytes in self?.received(bytes) }
    }

    /// A model with no connection, for previews and tests.
    public init(sid: String) {
        self.sid = sid
        channel = nil
    }

    public func start() { channel?.start() }
    public func stop() { channel?.stop() }
    public func resume() { channel?.resume() }

    private func stateChanged(_ state: ChannelState) {
        connection = state
        switch state {
        case .connecting:
            receivedBytes = 0
            expectedBytes = nil
            welcomePending = true
            awaitingEntries = true
        case .handshaking:
            break
        case .live:
            // Whatever was asked for on a connection that dropped.
            for id in due { channel?.send(.artifact(id: id)) }
            if loadingDiff { channel?.send(.diff(seq: diffSeq)) }
        case .idle, .retrying, .stopped:
            awaitingEntries = false
        }
        loadingOlder = false
    }

    private func received(_ bytes: Int) {
        if awaitingEntries { receivedBytes = bytes }
    }

    public func apply(_ message: Incoming) {
        switch message {
        case .entries(let from, let items):
            let from = max(from, 0)
            let end = base + entries.count
            let welcome = welcomePending
            welcomePending = false
            awaitingEntries = false
            if from < base || from > end || (welcome && from + items.count < end) {
                // It doesn't join up with what's here (the history before it was rewritten, or
                // much happened while away), so that's no longer known to be right.
                base = from
                entries = items
            } else {
                entries.replaceSubrange((from - base)..., with: items)
            }
        case .loading(let bytes):
            if awaitingEntries { expectedBytes = bytes }
        case .older(let before, let from, let items):
            // Otherwise it answers for a history that has since been replaced.
            guard before == base else { break }
            loadingOlder = false
            base = max(from, 0)
            entries.insert(contentsOf: items, at: 0)
        case .diff(let reply):
            guard reply.seq == nil || reply.seq == diffSeq else { break }
            loadingDiff = false
            diff = reply
        case .status(let status):
            self.status = status
        case .approval(let approval):
            self.approval = approval
        case .question(let question):
            self.question = question
        case .artifact(let reply):
            due.remove(reply.id)
            pages[reply.id] = reply
            if reply.html != nil { onArtifact(reply) }
        case .artifacts(let gallery):
            if gallery.seq == nil || gallery.seq == gallerySeq { self.gallery = gallery }
        case .other:
            break
        }
    }

    // MARK: - Actions

    @discardableResult
    public func send(_ text: String) -> Bool {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return false }
        guard channel?.send(.send(text)) == true else { return false }
        promptsSent += 1
        return true
    }

    @discardableResult
    public func interrupt() -> Bool { channel?.send(.interrupt) ?? false }

    @discardableResult
    public func approve(_ choice: ApprovalChoice, reason: String? = nil) -> Bool {
        channel?.send(.approve(choice, reason: reason)) ?? false
    }

    /// Answers the pending questions, one answer per question.
    @discardableResult
    public func answer(_ answers: [QuestionAnswer]) -> Bool {
        guard let question, answers.count == question.questions.count,
              !answers.contains(where: \.isEmpty) else { return false }
        return channel?.send(.answer(answers)) ?? false
    }

    /// Ends the session on its machine, as `tamale kill` would there (it stays saved), and
    /// waits for it to go. Connects first if need be. Throws if it can't be reached.
    public func close(timeout: Duration = .seconds(20)) async throws {
        guard let channel else { return }
        let deadline = ContinuousClock.now + timeout
        func waitFor(_ done: () -> Bool) async throws {
            while !done() {
                if case .stopped(let why) = connection { throw CloseError.unreachable(why) }
                guard ContinuousClock.now < deadline else { throw CloseError.timedOut }
                try await Task.sleep(for: .milliseconds(100))
            }
        }
        channel.start()
        try await waitFor { connection.isLive }
        guard channel.send(.close) else { throw CloseError.timedOut }
        // Once it has the message, the channel ending is the session ending.
        do {
            try await waitFor { !connection.isLive }
        } catch CloseError.unreachable {}
    }

    /// Asks tamale for an artifact's page, once.
    public func requestArtifact(_ id: String) {
        guard pages[id] == nil, !due.contains(id) else { return }
        due.insert(id)
        channel?.send(.artifact(id: id))
    }

    /// Asks for the page of history before what's loaded, unless one's on its way.
    public func loadOlder() {
        guard base > 0, !loadingOlder, connection.isLive else { return }
        if channel?.send(.older(before: base)) == true { loadingOlder = true }
    }

    /// Asks for the session's uncommitted changes.
    public func requestDiff() {
        diffSeq += 1
        loadingDiff = true
        channel?.send(.diff(seq: diffSeq))
    }

    /// Asks for the gallery of every artifact this project's sessions saved.
    public func requestGallery() {
        gallerySeq += 1
        channel?.send(.artifacts(seq: gallerySeq))
    }

    // MARK: - Derived

    public var busy: Bool { status?.busy ?? false }

    /// How far the first load has got, from 0 to 1, or nil if there's no telling (a tamale from
    /// before `loading`, once it's sending). Connecting counts for a little.
    public var loadFraction: Double? {
        switch connection {
        case .connecting: return 0.05
        case .handshaking: return 0.15
        default: break
        }
        guard let expectedBytes else { return receivedBytes > 0 ? nil : 0.25 }
        guard expectedBytes > 0 else { return 1 }
        return 0.25 + 0.75 * min(1, Double(receivedBytes) / Double(expectedBytes))
    }

    /// The first prompt's first line, as the hub's list titles a session. Empty while the start
    /// of the conversation isn't loaded.
    public var title: String {
        guard base == 0 else { return "" }
        return entries.first { $0.kind == .user }?.text?.split(separator: "\n").first.map(String.init) ?? ""
    }
}

public enum CloseError: LocalizedError, Equatable {
    /// Couldn't reach the session, and why.
    case unreachable(String)
    case timedOut

    public var errorDescription: String? {
        switch self {
        case .unreachable(let why): why
        case .timedOut: "The session didn't answer in time."
        }
    }
}
