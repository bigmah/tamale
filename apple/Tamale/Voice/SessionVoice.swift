// Driving a tamale session by voice. Gemini gets tools that act through the session's
// `SessionModel`, the same encrypted channel the composer uses: read the conversation, check
// the status, send a prompt, interrupt, look at the changes, and answer approvals and questions.
// Turns take minutes, so `send_prompt` returns at once. When the turn ends, or tamale stops to
// ask something, the app tells Gemini and Gemini tells the user.

import SwiftUI
import TamaleKit

struct SessionVoiceView: View {
    var row: SessionRow
    var model: SessionModel
    @AppStorage("geminiModel") private var preferredModel = ""
    @AppStorage("geminiVoice") private var voice = ""
    @State private var live = GeminiLive()
    @State private var agent: SessionAgent?

    var body: some View {
        VoiceChatView(live: live, title: "Talk to tamale", prompt: "Ask what tamale is doing, or tell it what to do.") { key in
            let agent = SessionAgent(model: model, row: row, live: live)
            self.agent = agent
            live.onToolCall = { name, args in await agent.call(name, args) }
            await live.start(apiKey: key, preferredModel: preferredModel, voice: voice,
                             instruction: agent.instruction, tools: SessionAgent.tools)
        } accessory: {
            // The same cards as the session's, so a tap answers when that's quicker than words.
            if let approval = model.approval {
                ApprovalCard(approval: approval) { choice, reason in
                    model.approve(choice, reason: reason)
                    live.inform("The user answered the permission request themselves on screen: \(choice.rawValue).",
                                shown: "You answered: \(choice.rawValue)")
                }
            }
            if let question = model.question {
                QuestionCard(asked: question) { answers in
                    model.answer(answers)
                    live.inform("The user answered tamale's questions themselves on screen.", shown: "You answered the questions")
                }
                .id(question)
            }
        }
        .onChange(of: model.busy) { was, now in
            if was && !now { agent?.turnEnded() }
        }
        .onChange(of: model.approval) { _, approval in
            if let approval { agent?.approvalAsked(approval) }
        }
        .onChange(of: model.question) { _, question in
            if let question { agent?.questionsAsked(question) }
        }
    }
}

@MainActor
final class SessionAgent {
    let model: SessionModel
    let row: SessionRow
    weak var live: GeminiLive?

    init(model: SessionModel, row: SessionRow, live: GeminiLive) {
        self.model = model
        self.row = row
        self.live = live
    }

    // MARK: What Gemini is told

    var instruction: String {
        """
        You're a voice assistant for the user's coding agent, tamale, which is running on their \
        computer in \(row.place). The user is on their phone, talking to you; you control that \
        tamale session through your functions, as if typing into it.

        How to work:
        - For anything about the code or the machine (does it build, run the tests, what's in a \
        file, fix something), ask tamale with send_prompt. Write the prompt as a clear \
        instruction, as the user would type it. Say briefly what you asked, then stop talking: \
        tamale takes a while, and the app will tell you when it's done.
        - To answer what tamale did or said, use read_conversation, get_status or get_changes \
        first rather than guessing.
        - When the app tells you a turn finished, tell the user the gist in a sentence or two. \
        Mention errors. Don't read code, paths or long output aloud unless asked.
        - When tamale asks permission, say what it wants to do in plain words and ask yes or no. \
        Call answer_permission only after the user clearly answers that request. Never approve \
        on your own.
        - When tamale asks questions, read them and their options, and answer with \
        answer_questions only with what the user chose.
        - If the user wants tamale to stop, call interrupt.
        Keep it spoken: short sentences, no markdown, no lists of symbols.

        Right now: \(statusLine)
        \(model.approval.map { "tamale is waiting for permission: \(describe($0))" } ?? "")
        \(model.question.map { "tamale is waiting for answers to: \(describe($0))" } ?? "")

        The end of the conversation so far:
        \(conversation(last: 6))
        """
    }

    static let tools: [[String: Any]] = [
        [
            "name": "send_prompt",
            "description": "Sends a message to tamale, the coding agent, as if the user typed it. Returns at once; tamale works on it and the app reports when the turn ends. If tamale is busy, the message is queued.",
            "parameters": ["type": "OBJECT", "properties": [
                "text": ["type": "STRING", "description": "The prompt, written as an instruction or question to the coding agent."],
            ], "required": ["text"]],
        ],
        [
            "name": "get_status",
            "description": "Whether tamale is working, for how long, its model, project and directory, queued prompts, and anything it's waiting on.",
        ],
        [
            "name": "read_conversation",
            "description": "The last entries of the session's conversation: the user's prompts, tamale's replies, tool calls with their output, and errors.",
            "parameters": ["type": "OBJECT", "properties": [
                "last": ["type": "INTEGER", "description": "How many entries, from the end. Default 8, at most 40."],
            ]],
        ],
        [
            "name": "get_changes",
            "description": "The uncommitted changes in tamale's working directory (git diff): files, lines added and removed, and the changed lines.",
        ],
        [
            "name": "interrupt",
            "description": "Stops what tamale is doing now.",
        ],
        [
            "name": "answer_permission",
            "description": "Answers a permission request tamale is waiting on, which the app will have told you about. Only with the user's explicit yes or no to that request. Not for asking tamale to do something: that's send_prompt.",
            "parameters": ["type": "OBJECT", "properties": [
                "allow": ["type": "BOOLEAN", "description": "true if the user said yes."],
                "reason": ["type": "STRING", "description": "When refusing, what the user said to do instead, if anything."],
            ], "required": ["allow"]],
        ],
        [
            "name": "answer_questions",
            "description": "Answers tamale's waiting questions, one answer per question in order.",
            "parameters": ["type": "OBJECT", "properties": [
                "answers": ["type": "ARRAY", "items": ["type": "OBJECT", "properties": [
                    "chosen": ["type": "ARRAY", "items": ["type": "STRING"], "description": "Labels of the options the user picked."],
                    "other": ["type": "STRING", "description": "Anything the user said instead of, or as well as, an option."],
                ]]],
            ], "required": ["answers"]],
        ],
    ]

    // MARK: Calls

    func call(_ name: String, _ args: [String: Any]) async -> [String: Any] {
        switch name {
        case "send_prompt":
            let text = (args["text"] as? String ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else { return ["error": "Empty prompt."] }
            let wasBusy = model.busy
            guard model.send(text) else { return ["error": "The session isn't connected right now (\(connection))."] }
            live?.note("Sent to tamale: \(text)")
            return ["sent": true, "queued": wasBusy,
                    "next": wasBusy
                        ? "tamale is busy; this runs after the current turn. Tell the user so in a few words."
                        : "tamale is working on it. Tell the user in a few words what you asked; you'll be told when it's done."]

        case "get_status":
            return status

        case "read_conversation":
            let last = min(max((args["last"] as? NSNumber)?.intValue ?? 8, 1), 40)
            live?.note("Read the conversation")
            return ["conversation": conversation(last: last), "older_entries_not_loaded": model.base > 0]

        case "get_changes":
            live?.note("Looked at the changes")
            return await changes()

        case "interrupt":
            guard model.busy else { return ["error": "tamale isn't doing anything."] }
            guard model.interrupt() else { return ["error": "The session isn't connected right now."] }
            live?.note("Interrupted tamale")
            return ["interrupted": true, "next": "Confirm it to the user in a few words."]

        case "answer_permission":
            guard let approval = model.approval else {
                return ["error": "tamale isn't waiting for permission. To ask tamale to do something, use send_prompt."]
            }
            let allow = (args["allow"] as? NSNumber)?.boolValue ?? (args["allow"] as? Bool) ?? false
            let reason = (args["reason"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            // Never "always": a standing rule is the user's to tap, not a voice model's to infer.
            guard model.approve(allow ? .yes : .no, reason: allow ? nil : reason) else {
                return ["error": "The session isn't connected right now."]
            }
            live?.note(allow ? "Allowed: \(approval.summary)" : "Refused: \(approval.summary)")
            return ["answered": allow ? "yes" : "no", "next": "Confirm it to the user in a few words."]

        case "answer_questions":
            guard let question = model.question else { return ["error": "tamale isn't asking anything."] }
            let raw = args["answers"] as? [[String: Any]] ?? []
            let answers = raw.map { QuestionAnswer(chosen: $0["chosen"] as? [String] ?? [], other: $0["other"] as? String ?? "") }
            guard answers.count == question.questions.count else {
                return ["error": "tamale asked \(question.questions.count) questions; give one answer for each."]
            }
            guard model.answer(answers) else { return ["error": "Every question needs an answer, and the session must be connected."] }
            live?.note("Answered tamale's questions")
            return ["answered": true]

        default:
            return ["error": "No function called \(name)."]
        }
    }

    // MARK: Events

    func turnEnded() {
        // The reply's last entries can come just after the status that says it's done.
        Task {
            try? await Task.sleep(for: .milliseconds(600))
            guard !model.busy else { return }
            live?.inform("tamale finished its turn. Tell the user how it went.\n\(lastTurn)", shown: "tamale finished")
        }
    }

    func approvalAsked(_ approval: Approval) {
        live?.inform("tamale is waiting for permission: \(describe(approval)). Ask the user whether to allow it.",
                     shown: "tamale asks permission: \(approval.summary)")
    }

    func questionsAsked(_ question: QuestionSet) {
        live?.inform("tamale is asking the user: \(describe(question)). Read the questions and options to them.",
                     shown: "tamale has questions")
    }

    // MARK: Text for Gemini

    private var connection: String {
        switch model.connection {
        case .live: "connected"
        case .connecting, .handshaking: "connecting"
        case .retrying(let why): "reconnecting (\(why))"
        case .idle: "not connected"
        case .stopped(let why): "unreachable: \(why)"
        }
    }

    private var statusLine: String {
        guard let s = model.status else { return "Session \(connection); no status yet." }
        var line = s.busy ? "tamale is working" : "tamale is idle"
        if s.busy, let since = s.since {
            line += " (for \(formatElapsed(Int(Date().timeIntervalSince1970 - since))))"
            if !s.step.isEmpty {
                let step = Int(Date().timeIntervalSince1970 - (s.stepSince ?? since))
                line += "; now: \(s.step), for \(formatElapsed(step))"
                if s.steps > 0 { line += ", \(s.steps) steps so far" }
            }
        }
        return line + ". Project \(s.project), model \(s.model). Session \(connection)."
    }

    private var status: [String: Any] {
        var out: [String: Any] = ["connection": connection, "summary": statusLine]
        if let s = model.status {
            out["busy"] = s.busy
            out["compacting"] = s.compacting
            out["project"] = s.project
            out["directory"] = s.cwd
            out["model"] = s.model
            out["mode"] = s.mode
            out["queued_prompts"] = s.queued
            if let goal = s.goal { out["goal"] = goal }
            if s.window > 0 { out["context_used_percent"] = Int(Double(s.context) / Double(s.window) * 100) }
        }
        if let a = model.approval { out["waiting_for_permission"] = describe(a) }
        if let q = model.question { out["waiting_for_answers"] = describe(q) }
        if let entry = model.entries.last(where: { $0.pending }) {
            out["running_now"] = "\(entry.name ?? "tool"): \(entry.summary ?? entry.description ?? "")"
        }
        return out
    }

    /// What happened since the last prompt: replies, errors, and how the tools went.
    private var lastTurn: String {
        let start = model.entries.lastIndex(where: { $0.kind == .user }) ?? max(0, model.entries.count - 6)
        let turn = model.entries[start...]
        let tools = turn.filter { $0.kind == .tool }
        let failed = tools.filter { $0.error == true }
        var out = "It ran \(tools.count) tool call\(tools.count == 1 ? "" : "s")"
        out += failed.isEmpty ? ".\n" : ", \(failed.count) failed.\n"
        for entry in turn where entry.kind != .tool || entry.error == true {
            out += Self.text(entry, room: 2500) + "\n"
        }
        return String(out.suffix(6000))
    }

    private func conversation(last n: Int) -> String {
        let entries = model.entries.suffix(n)
        if entries.isEmpty { return model.awaitingEntries ? "(still loading)" : "(nothing yet)" }
        return entries.map { Self.text($0, room: 1500) }.joined(separator: "\n")
    }

    private static func text(_ e: Entry, room: Int) -> String {
        func cut(_ s: String, _ n: Int) -> String { s.count > n ? s.prefix(n) + " …(cut)" : s }
        switch e.kind {
        case .user: return "User: " + cut(e.text ?? "", room)
        case .assistant, .note:
            let (md, artifacts) = e.prose
            let lines = md.split(separator: "\n", omittingEmptySubsequences: false).map { line -> String in
                if let i = ArtifactMarker.index(line) {
                    return "[artifact: \(i < artifacts.count ? artifacts[i].title : "page")]"
                }
                return String(line)
            }
            return (e.kind == .note ? "Note: " : "tamale: ") + cut(lines.joined(separator: "\n"), room)
        case .tool:
            let what = "\(e.name ?? "tool") \(e.summary ?? e.description ?? "")"
            guard let output = e.output else { return "Tool \(what): running" }
            return "Tool \(what)\(e.error == true ? " FAILED" : ""): " + cut(output, min(room, e.error == true ? 1500 : 500))
        case .info: return "Info: " + cut(e.text ?? "", room)
        case .error: return "Error: " + cut(e.text ?? "", room)
        case .unknown: return ""
        }
    }

    private func describe(_ a: Approval) -> String {
        var s = "\(a.name): \(a.summary)"
        if !a.description.isEmpty, a.description != a.summary { s += "\n\(a.description.prefix(1500))" }
        return s
    }

    private func describe(_ q: QuestionSet) -> String {
        q.questions.enumerated().map { i, item in
            let options = item.options.map { $0.description.isEmpty ? $0.label : "\($0.label) (\($0.description))" }
            return "\(i + 1). \(item.question)\(item.multiSelect ? " (pick any)" : "") Options: \(options.joined(separator: "; "))"
        }.joined(separator: "\n")
    }

    private func changes() async -> [String: Any] {
        model.requestDiff()
        let deadline = ContinuousClock.now + .seconds(15)
        while model.loadingDiff, ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(150))
        }
        guard !model.loadingDiff, let diff = model.diff else { return ["error": "tamale didn't send the changes in time."] }
        if let error = diff.error { return ["error": error] }
        var out = "\(diff.label): \(diff.files.count) file\(diff.files.count == 1 ? "" : "s"), +\(diff.added) −\(diff.removed)\n"
        for file in diff.files {
            out += "\n\(file.status) \(file.path) +\(file.added) −\(file.removed)\n"
            if file.binary || file.omitted { continue }
            for line in file.lines where line.kind == .added || line.kind == .removed {
                out += "\(line.kind.rawValue)\(line.text)\n"
            }
            if out.count > 12_000 { out += "…(cut)\n"; break }
        }
        return ["changes": String(out.prefix(12_000))]
    }
}
