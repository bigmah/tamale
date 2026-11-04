// One session as the app shows it: the conversation, status and approval, kept in step with
// tamale's /web messages over a `SessionChannel`.

import Foundation
import Observation

@MainActor
@Observable
public final class SessionModel {
    public let sid: String
    public private(set) var entries: [Entry] = []
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
    /// Called with each artifact page as it arrives (the app keeps them in its library).
    @ObservationIgnored public var onArtifact: (ArtifactReply) -> Void = { _ in }

    @ObservationIgnored public let channel: SessionChannel?

    public init(channel: SessionChannel) {
        self.channel = channel
        sid = channel.sid
        channel.onState = { [weak self] state in self?.stateChanged(state) }
        channel.onMessage = { [weak self] message in self?.apply(message) }
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
        if state.isLive {
            // Whatever was asked for on a connection that dropped.
            for id in due { channel?.send(.artifact(id: id)) }
        }
    }

    public func apply(_ message: Incoming) {
        switch message {
        case .entries(let from, let items):
            let keep = min(max(from, 0), entries.count)
            entries.replaceSubrange(keep..., with: items)
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
        return channel?.send(.send(text)) ?? false
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

    /// Asks for the gallery of every artifact this project's sessions saved.
    public func requestGallery() {
        gallerySeq += 1
        channel?.send(.artifacts(seq: gallerySeq))
    }

    // MARK: - Derived

    public var busy: Bool { status?.busy ?? false }

    /// The first prompt's first line, as the hub's list titles a session.
    public var title: String {
        entries.first { $0.kind == .user }?.text?.split(separator: "\n").first.map(String.init) ?? ""
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
