// The /web JSON messages (src/web.rs, src/web/files.rs) the app reads and sends. tamale
// writes them with untyped `json!`, so decoding is lenient: unknown fields are ignored and
// missing ones get defaults. apple/Fixtures/messages has one sample of each, written by
// src/web/fixtures.rs, and the tests decode them all.

import Foundation

// MARK: - Entries

/// One artifact in an assistant entry: where its marker is in `md`, by index.
public struct ArtifactBlock: Codable, Equatable, Hashable, Sendable {
    /// The id tamale serves it by in this process, once its fence has closed.
    public var id: String?
    public var title: String
    /// Whether its closing fence has arrived (else it's still streaming in).
    public var done: Bool
    /// SHA-256 of the document as written: its stable name in the library.
    public var hash: String?

    public init(id: String?, title: String, done: Bool, hash: String?) {
        self.id = id
        self.title = title
        self.done = done
        self.hash = hash
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? "Artifact"
        done = try c.decodeIfPresent(Bool.self, forKey: .done) ?? (id != nil)
        hash = try c.decodeIfPresent(String.self, forKey: .hash)
    }
}

public struct Entry: Codable, Equatable, Sendable {
    public enum Kind: String, Codable, Sendable {
        case user, assistant, note, info, error, tool, unknown

        public init(from decoder: Decoder) throws {
            self = Kind(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
        }
    }

    public var kind: Kind
    /// user, info, error.
    public var text: String?
    /// assistant, note: what the web page shows.
    public var html: String?
    /// assistant, note: the markdown, with artifact markers (newer tamales).
    public var md: String?
    public var artifacts: [ArtifactBlock]?
    /// tool.
    public var name: String?
    public var summary: String?
    public var description: String?
    /// nil while the tool runs.
    public var output: String?
    public var error: Bool?
    public var cut: Bool?

    public init(kind: Kind, text: String? = nil, html: String? = nil, md: String? = nil,
                artifacts: [ArtifactBlock]? = nil, name: String? = nil, summary: String? = nil,
                description: String? = nil, output: String? = nil, error: Bool? = nil, cut: Bool? = nil) {
        self.kind = kind
        self.text = text
        self.html = html
        self.md = md
        self.artifacts = artifacts
        self.name = name
        self.summary = summary
        self.description = description
        self.output = output
        self.error = error
        self.cut = cut
    }

    /// Whether a tool call is still running.
    public var pending: Bool { kind == .tool && output == nil }

    /// The markdown to show, and the artifacts its markers stand for. From `md` when tamale
    /// sends it, else recovered from `html` (tamales from before `md`).
    public var prose: (markdown: String, artifacts: [ArtifactBlock]) {
        if let md { return (md, artifacts ?? []) }
        return HTMLText.markdown(from: html ?? text ?? "")
    }
}

/// The line that stands for artifact `n` in an entry's `md` (`render::artifact_marker`).
public enum ArtifactMarker {
    public static func line(_ n: Int) -> String { "<!-- artifact \(n) -->" }

    /// The artifact index, if `line` is a marker.
    public static func index(_ line: Substring) -> Int? {
        let t = line.trimmingCharacters(in: .whitespaces)
        guard t.hasPrefix("<!-- artifact "), t.hasSuffix(" -->") else { return nil }
        return Int(t.dropFirst("<!-- artifact ".count).dropLast(" -->".count))
    }
}

// MARK: - Status and approval

public struct SessionStatus: Codable, Equatable, Sendable {
    public var project: String
    public var cwd: String
    public var dirs: [String]
    public var model: String
    public var backend: String
    public var signedIn: Bool
    public var mode: String
    public var effort: String?
    public var thinking: String
    public var running: Bool
    public var compacting: Bool
    /// Unix seconds when the current run began.
    public var since: Double?
    public var context: Int
    public var window: Int
    public var inputTokens: Int
    public var outputTokens: Int
    public var cost: Double
    public var costPartial: Bool
    public var goal: String?
    public var queued: [String]

    enum CodingKeys: String, CodingKey {
        case project, cwd, dirs, model, backend, mode, effort, thinking, running, compacting, since
        case context, window, cost, goal, queued
        case signedIn = "signed_in", inputTokens = "input_tokens", outputTokens = "output_tokens"
        case costPartial = "cost_partial"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        func get<T: Decodable>(_ key: CodingKeys, _ fallback: T) -> T {
            (try? c.decodeIfPresent(T.self, forKey: key)) ?? fallback
        }
        project = get(.project, "")
        cwd = get(.cwd, "")
        dirs = get(.dirs, [])
        model = get(.model, "")
        backend = get(.backend, "")
        signedIn = get(.signedIn, false)
        mode = get(.mode, "")
        effort = get(.effort, nil as String?)
        thinking = get(.thinking, "")
        running = get(.running, false)
        compacting = get(.compacting, false)
        since = get(.since, nil as Double?)
        context = get(.context, 0)
        window = get(.window, 0)
        inputTokens = get(.inputTokens, 0)
        outputTokens = get(.outputTokens, 0)
        cost = get(.cost, 0)
        costPartial = get(.costPartial, false)
        goal = get(.goal, nil as String?)
        queued = get(.queued, [])
    }

    public var busy: Bool { running || compacting }
}

public struct Approval: Codable, Equatable, Sendable {
    public var name: String
    public var summary: String
    public var description: String
    /// Rules "always" would add; empty when it isn't offered.
    public var always: [String]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        summary = try c.decodeIfPresent(String.self, forKey: .summary) ?? ""
        description = try c.decodeIfPresent(String.self, forKey: .description) ?? ""
        always = try c.decodeIfPresent([String].self, forKey: .always) ?? []
    }
}

// MARK: - Artifacts

public struct ArtifactReply: Codable, Equatable, Sendable {
    public var id: String
    /// The page as tamale serves it (with its fit and resizer scripts), or nil if it's gone.
    public var html: String?
    public var hash: String?
}

public struct Gallery: Codable, Equatable, Sendable {
    public struct Item: Codable, Equatable, Hashable, Sendable {
        public var title: String
        public var file: String
        public var modified: Double
        public var hash: String?
        /// `/artifact/<id>`.
        public var url: String

        public var id: String { url.hasPrefix("/artifact/") ? String(url.dropFirst("/artifact/".count)) : url }
    }

    public struct SessionGroup: Codable, Equatable, Sendable {
        public var current: Bool
        public var title: String
        public var updated: Double
        public var items: [Item]
    }

    public var sessions: [SessionGroup]
    public var seq: Int?
}

// MARK: - Messages

public enum Incoming: Equatable, Sendable {
    case entries(from: Int, items: [Entry])
    case status(SessionStatus?)
    case approval(Approval?)
    case artifact(ArtifactReply)
    case artifacts(Gallery)
    case other(String)

    struct Head: Decodable { var t: String }
    struct EntriesBody: Decodable { var from: Int; var items: [Entry] }
    struct StatusBody: Decodable { var status: SessionStatus? }
    struct ApprovalBody: Decodable { var approval: Approval? }

    public static func decode(_ text: String) throws -> Incoming {
        let data = Data(text.utf8)
        let d = JSONDecoder()
        let t = try d.decode(Head.self, from: data).t
        switch t {
        case "entries":
            let body = try d.decode(EntriesBody.self, from: data)
            return .entries(from: body.from, items: body.items)
        case "status": return .status(try d.decode(StatusBody.self, from: data).status)
        case "approval": return .approval(try d.decode(ApprovalBody.self, from: data).approval)
        case "artifact": return .artifact(try d.decode(ArtifactReply.self, from: data))
        case "artifacts": return .artifacts(try d.decode(Gallery.self, from: data))
        default: return .other(t)
        }
    }
}

public enum ApprovalChoice: String, Sendable {
    case yes, always, no
}

public enum Outgoing: Equatable, Sendable {
    case send(String)
    case interrupt
    case approve(ApprovalChoice, reason: String?)
    case artifact(id: String)
    case artifacts(seq: Int)
    /// Ends the session, as `tamale kill` would on its machine; it stays saved there.
    case close

    public var json: String {
        var object: [String: Any]
        switch self {
        case .send(let text): object = ["t": "send", "text": text]
        case .interrupt: object = ["t": "interrupt"]
        case .approve(let choice, let reason):
            object = ["t": "approve", "choice": choice.rawValue]
            if let reason, !reason.trimmingCharacters(in: .whitespaces).isEmpty { object["reason"] = reason }
        case .artifact(let id): object = ["t": "artifact", "id": id]
        case .artifacts(let seq): object = ["t": "artifacts", "seq": seq]
        case .close: object = ["t": "close"]
        }
        let data = try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        return String(decoding: data, as: UTF8.self)
    }
}

// MARK: - Prompts

public enum Prompts {
    /// What the web UI's Revise button starts the prompt with.
    public static func revise(title: String) -> String { "Revise the \"\(title)\" artifact: " }

    /// The gallery's form, for an artifact from an earlier session.
    public static func reviseSaved(title: String, file: String) -> String {
        "Revise the \"\(title)\" artifact from an earlier session (saved at \(file); read it first): "
    }
}
