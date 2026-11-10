import Foundation
import XCTest

@testable import TamaleKit

final class MessageTests: XCTestCase {
    func message(_ name: String) throws -> Incoming {
        try Incoming.decode(fixtureText("messages/\(name)"))
    }

    func testEveryFixtureDecodes() throws {
        let names = try FileManager.default.contentsOfDirectory(atPath: fixtures.appendingPathComponent("messages").path)
        XCTAssertGreaterThanOrEqual(names.count, 8)
        for name in names where name.hasSuffix(".json") {
            XCTAssertNoThrow(try message(name), name)
        }
    }

    func testEntries() throws {
        guard case .entries(let from, let items) = try message("entries.json") else { return XCTFail() }
        XCTAssertEqual(from, 0)
        XCTAssertEqual(items.map(\.kind), [.info, .user, .tool, .tool, .tool, .assistant, .note, .error, .assistant])
        XCTAssertEqual(items[2].name, "read_file")
        XCTAssertEqual(items[3].error, true)
        XCTAssertTrue(items[4].pending)
        let reply = items[5]
        let prose = reply.prose
        XCTAssertTrue(prose.markdown.contains(ArtifactMarker.line(0)))
        XCTAssertEqual(prose.artifacts.count, 1)
        XCTAssertEqual(prose.artifacts[0].title, "Login flow")
        XCTAssertTrue(prose.artifacts[0].done)
        XCTAssertEqual(prose.artifacts[0].hash?.count, 64)
        XCTAssertEqual(items[6].prose.markdown, "Compacted the conversation: *3 turns* summarised.")
        XCTAssertFalse(items[8].prose.artifacts[0].done)
        XCTAssertNil(items[8].prose.artifacts[0].id)
    }

    func testOlderTamalesWithoutMarkdownFallBackToTheHTML() throws {
        guard case .entries(_, var items) = try message("entries.json") else { return XCTFail() }
        for i in items.indices {
            items[i].md = nil
            items[i].artifacts = nil
        }
        let reply = items[5].prose
        XCTAssertEqual(reply.artifacts.map(\.title), ["Login flow"])
        XCTAssertEqual(reply.artifacts[0].id, "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0")
        XCTAssertTrue(reply.markdown.contains("I traced the bug."))
        XCTAssertTrue(reply.markdown.contains("- `auth.rs` drops the cookie"), reply.markdown)
        XCTAssertTrue(reply.markdown.contains("**fix**"), reply.markdown)
        XCTAssertTrue(reply.markdown.contains(ArtifactMarker.line(0)))
        let building = items[8].prose
        XCTAssertEqual(building.artifacts.map(\.title), ["Draft"])
        XCTAssertFalse(building.artifacts[0].done)
        let code = HTMLText.markdown(from: "<div class=\"codeblock\"><div class=\"codeblock-lang\">rust</div><pre><code><span class=\"t-kw\">fn</span> a() &lt;&gt;</code></pre></div>")
        XCTAssertEqual(code.markdown, "```rust\nfn a() <>\n```", code.markdown)
    }

    func testStatusApprovalAndArtifacts() throws {
        guard case .status(let status?) = try message("status.json") else { return XCTFail() }
        XCTAssertTrue(status.running)
        XCTAssertEqual(status.model, "claude-opus-4")
        XCTAssertEqual(status.queued, ["then run the tests"])
        XCTAssertEqual(status.effort, "high")
        XCTAssertEqual(status.step, "Run the tests")
        XCTAssertEqual(status.stepKind, "tool")
        XCTAssertEqual(status.stepSince, 1_700_000_090)
        XCTAssertEqual(status.steps, 3)
        XCTAssertNil(status.longStepNote(seconds: 60, elapsed: "1m 0s"))
        XCTAssertEqual(status.longStepNote(seconds: 600, elapsed: "10m 0s"),
                       "This step has been running for 10m 0s. Its output shows up when it finishes.")

        guard case .approval(let approval?) = try message("approval.json") else { return XCTFail() }
        XCTAssertEqual(approval.name, "bash")
        XCTAssertEqual(approval.always, ["bash: rm *"])
        guard case .approval(nil) = try message("approval-none.json") else { return XCTFail() }

        guard case .question(let question?) = try message("question.json") else { return XCTFail() }
        XCTAssertEqual(question.description, "Ask how to fix login")
        XCTAssertEqual(question.questions.map(\.header), ["Cookie", "Tests"])
        XCTAssertEqual(question.questions[0].options[0].description, "Renew it on every request")
        XCTAssertEqual(question.questions.map(\.multiSelect), [false, true])
        guard case .question(nil) = try message("question-none.json") else { return XCTFail() }

        guard case .artifact(let artifact) = try message("artifact.json") else { return XCTFail() }
        XCTAssertTrue(artifact.html?.contains("Login flow") ?? false)
        XCTAssertEqual(artifact.hash?.count, 64)
        guard case .artifact(let missing) = try message("artifact-missing.json") else { return XCTFail() }
        XCTAssertNil(missing.html)

        guard case .artifacts(let gallery) = try message("artifacts.json") else { return XCTFail() }
        XCTAssertEqual(gallery.seq, 7)
        XCTAssertEqual(gallery.sessions[0].items[0].id, "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0")
        XCTAssertEqual(gallery.sessions[0].items[0].hash, artifact.hash, "the gallery and the reply agree")

        guard case .other("hs0") = try message("hs0.json") else { return XCTFail() }
    }

    func testOutgoingMessages() {
        XCTAssertEqual(Outgoing.send("hi").json, #"{"t":"send","text":"hi"}"#)
        XCTAssertEqual(Outgoing.interrupt.json, #"{"t":"interrupt"}"#)
        XCTAssertEqual(Outgoing.approve(.no, reason: "not now").json, #"{"choice":"no","reason":"not now","t":"approve"}"#)
        XCTAssertEqual(Outgoing.approve(.yes, reason: " ").json, #"{"choice":"yes","t":"approve"}"#)
        XCTAssertEqual(Outgoing.artifact(id: "ab").json, #"{"id":"ab","t":"artifact"}"#)
        XCTAssertEqual(Outgoing.artifacts(seq: 3).json, #"{"seq":3,"t":"artifacts"}"#)
        XCTAssertEqual(Outgoing.close.json, #"{"t":"close"}"#)
        XCTAssertEqual(
            Outgoing.answer([QuestionAnswer(chosen: ["Unit"]), QuestionAnswer(chosen: ["A", "B"], other: " more ")]).json,
            #"{"answers":[{"chosen":["Unit"]},{"chosen":["A","B"],"other":"more"}],"t":"answer"}"#)
        XCTAssertEqual(Prompts.revise(title: "Plan"), "Revise the \"Plan\" artifact: ")
    }

    func testTheHubsSessionList() throws {
        let list = try JSONDecoder().decode(SessionList.self, from: fixture("hub/sessions.json"))
        XCTAssertEqual(list.keys.map(\.name), ["mine"])
        XCTAssertEqual(list.sessions.count, 2)
        XCTAssertTrue(list.sessions[0].e2e)
        XCTAssertEqual(list.sessions[0].sealedLabel, "SEALED-LABEL")
        XCTAssertTrue(list.sessions[0].approval)
        XCTAssertEqual(list.sessions[1].host, "vps-b")
        // Labels that don't open show what the hub has.
        let shown = list.sessions[1].shown(with: TamaleKey(bytes: Data(repeating: 1, count: 32)))
        XCTAssertEqual(shown.label.cwd, "~/api")
    }

    func testDiffDecodes() throws {
        guard case .diff(let reply) = try message("diff.json") else { return XCTFail() }
        XCTAssertEqual(reply.label, "working tree vs HEAD")
        XCTAssertEqual(reply.files.map(\.status), ["M", "A", "M", "R"])
        XCTAssertEqual(reply.added, 3)
        XCTAssertEqual(reply.removed, 1)
        let auth = reply.files[0]
        XCTAssertEqual(auth.lines.map(\.kind), [.context, .removed, .added, .added, .context, .context, .noNewline])
        XCTAssertEqual(auth.lines[1].old, 2)
        XCTAssertNil(auth.lines[1].new)
        XCTAssertEqual(auth.lines[1].parts.changed, "dro")
        XCTAssertEqual(auth.lines[2].parts.before, "    ")
        XCTAssertEqual(auth.lines[2].parts.after, "p(cookie);")
        XCTAssertTrue(reply.files[2].binary)
        XCTAssertEqual(reply.files[2].rows(), [.note("Binary file not shown")])
        XCTAssertEqual(reply.files[3].oldPath, "old/name.rs")
        XCTAssertEqual(reply.files[3].rows().count, 2, "renamed, and mode changed")
        guard case .diff(let failed) = try message("diff-error.json") else { return XCTFail() }
        XCTAssertNotNil(failed.error)
    }

    func file(lines count: Int, changedAt changes: Set<Int>) -> DiffFile {
        DiffFile(path: "a.rs", lines: (1...count).map { n in
            DiffLine(kind: changes.contains(n) ? .added : .context, old: changes.contains(n) ? nil : n, new: n, text: "line \(n)")
        })
    }

    func testUnchangedLinesFoldAroundChanges() {
        let file = file(lines: 100, changedAt: [50])
        let rows = file.rows()
        XCTAssertEqual(rows.first, .gap(start: 0, end: 46, section: "line 46"))
        XCTAssertEqual(rows.dropFirst().prefix(7), (46...52).map { .line($0) }[...])
        XCTAssertEqual(rows.last, .gap(start: 53, end: 100, section: ""))

        // A long gap at the top opens next to the change, one at the bottom likewise.
        var reveal = file.expanding(DiffReveal(), start: 0, end: 46)
        XCTAssertEqual(file.rows(reveal).first, .gap(start: 0, end: 26, section: "line 26"))
        reveal = file.expanding(reveal, start: 53, end: 100)
        XCTAssertEqual(file.rows(reveal).last, .gap(start: 73, end: 100, section: ""))

        // A small one opens fully, and `all` shows everything.
        let short = self.file(lines: 15, changedAt: [10])
        guard case .gap(let start, let end, _) = short.rows()[0] else { return XCTFail() }
        XCTAssertFalse(short.rows(short.expanding(DiffReveal(), start: start, end: end)).contains { $0.isStop && $0.id == "g0" })
        var all = DiffReveal()
        all.all = true
        XCTAssertEqual(file.rows(all).count, 100)

        // One in the middle opens from both ends.
        let two = self.file(lines: 200, changedAt: [10, 190])
        let middle = two.expanding(DiffReveal(), start: 13, end: 186)
        XCTAssertTrue(two.rows(middle).contains(.gap(start: 23, end: 176, section: "line 176")))
    }

    func testABigFilesRunsAreKeptApart() {
        let lines = (1...4).map { DiffLine(kind: .added, new: $0, text: "x") }
        let file = DiffFile(path: "big", lines: lines, breaks: [2])
        XCTAssertEqual(file.rows(), [.line(0), .line(1), .cut(2), .line(2), .line(3)])
    }

    func testQuotingLinesForAPrompt() {
        let file = DiffFile(path: "src/a.rs", lines: [
            DiffLine(kind: .context, old: 1, new: 1, text: "keep"),
            DiffLine(kind: .removed, old: 2, text: "old\r"),
            DiffLine(kind: .added, new: 2, text: "new"),
        ])
        let rows = file.rows()
        XCTAssertEqual(file.quote(rows: rows, first: 0, last: 2), "In `src/a.rs` (lines 1–2):\n```diff\n keep\n-old\n+new\n```\n\n")
        XCTAssertEqual(file.quote(rows: rows, first: 1, last: 1), "In `src/a.rs` (removed line 2):\n```diff\n-old\n```\n\n")
    }

    func testTheFileTree() {
        let files = ["src/app/diff.rs", "src/app/view.rs", "src/main.rs", "README.md", "docs/deep/er/a.md"]
            .map { DiffFile(path: $0, lines: []) }
        XCTAssertEqual(DiffTree.rows(files).map(\.name), ["docs/deep/er", "a.md", "src", "app", "diff.rs", "view.rs", "main.rs", "README.md"])
        XCTAssertEqual(DiffTree.rows(files).map(\.depth), [0, 1, 0, 1, 2, 2, 1, 0])
        XCTAssertEqual(DiffTree.rows(files, collapsed: ["src/app"]).map(\.name), ["docs/deep/er", "a.md", "src", "app", "main.rs", "README.md"])
        XCTAssertEqual(DiffTree.order(files), [4, 0, 1, 2, 3])
    }

    func testEmphasisCountsCharactersNotBytes() {
        let line = DiffLine(kind: .added, new: 1, text: "é\tb→c", emphasis: [3, 4])
        XCTAssertEqual(line.parts.before, "é    b")
        XCTAssertEqual(line.parts.changed, "→")
        XCTAssertEqual(line.parts.after, "c")
        // Numbers that don't fit the line show it plain.
        XCTAssertEqual(DiffLine(kind: .added, text: "ab", emphasis: [1, 9]).parts.before, "ab")
    }

    func entries(_ range: Range<Int>) -> [Entry] {
        range.map { Entry(kind: .user, text: "m\($0)") }
    }

    @MainActor
    func testTheModelPagesBackFromTheEnd() throws {
        let model = SessionModel(sid: String(repeating: "0", count: 32))
        XCTAssertEqual(model.loadFraction, 0.25)
        model.apply(.loading(bytes: 100))
        model.apply(.entries(from: 190, items: entries(190..<250)))
        XCTAssertEqual(model.base, 190)
        XCTAssertEqual(model.entries.first?.text, "m190")
        XCTAssertEqual(model.title, "", "the first prompt isn't here to name it")

        // Streaming goes by the whole conversation's numbering.
        model.apply(.entries(from: 249, items: [Entry(kind: .user, text: "new")]))
        XCTAssertEqual(model.entries.count, 60)
        XCTAssertEqual(model.entries.last?.text, "new")

        model.apply(.older(before: 190, from: 90, items: entries(90..<190)))
        XCTAssertEqual(model.base, 90)
        XCTAssertEqual(model.entries.count, 160)
        XCTAssertEqual(model.entries.first?.text, "m90")
        XCTAssertEqual(model.entries[100].text, "m190")

        // An answer for a history that's gone is dropped.
        model.apply(.older(before: 190, from: 0, items: entries(0..<3)))
        XCTAssertEqual(model.base, 90)

        // A rewind past what's loaded replaces it all.
        model.apply(.entries(from: 40, items: entries(40..<42)))
        XCTAssertEqual(model.base, 40)
        XCTAssertEqual(model.entries.count, 2)

        // As does a gap: this far on, what's between isn't known.
        model.apply(.entries(from: 300, items: entries(300..<301)))
        XCTAssertEqual(model.base, 300)
        XCTAssertEqual(model.entries.count, 1)

        model.apply(.older(before: 300, from: 0, items: entries(0..<300)))
        XCTAssertEqual(model.base, 0)
        XCTAssertEqual(model.title, "m0")
    }

    @MainActor
    func testTheModelKeepsADiffOnlyFromItsLatestRequest() throws {
        let model = SessionModel(sid: String(repeating: "0", count: 32))
        model.requestDiff()
        XCTAssertTrue(model.loadingDiff)
        guard case .diff(var reply) = try message("diff.json") else { return XCTFail() }
        reply.seq = 99
        model.apply(.diff(reply))
        XCTAssertNil(model.diff)
        XCTAssertTrue(model.loadingDiff)
        reply.seq = 1
        model.apply(.diff(reply))
        XCTAssertEqual(model.diff?.files.count, 4)
        XCTAssertFalse(model.loadingDiff)
    }

    @MainActor
    func testTheModelSplicesEntries() throws {
        let model = SessionModel(sid: String(repeating: "0", count: 32))
        model.apply(try message("entries.json"))
        XCTAssertEqual(model.entries.count, 9)
        model.apply(.entries(from: 7, items: [Entry(kind: .info, text: "x")]))
        XCTAssertEqual(model.entries.count, 8)
        XCTAssertEqual(model.entries.last?.text, "x")
        model.apply(.entries(from: 0, items: []))
        XCTAssertTrue(model.entries.isEmpty)
        model.apply(try message("status.json"))
        XCTAssertTrue(model.busy)
        model.apply(try message("approval.json"))
        XCTAssertNotNil(model.approval)
        XCTAssertFalse(model.answer([QuestionAnswer(chosen: ["Unit"])]), "nothing to answer")
        model.apply(try message("question.json"))
        XCTAssertEqual(model.question?.questions.count, 2)
        XCTAssertFalse(model.answer([QuestionAnswer(chosen: ["Unit"])]), "one answer per question")
        XCTAssertFalse(model.answer([QuestionAnswer(chosen: ["Unit"]), QuestionAnswer()]), "each answered")
        model.apply(try message("question-none.json"))
        XCTAssertNil(model.question)
        var saved: [ArtifactReply] = []
        model.onArtifact = { saved.append($0) }
        model.apply(try message("artifact.json"))
        XCTAssertEqual(saved.count, 1)
        XCTAssertNotNil(model.pages["a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0"])
    }
}
