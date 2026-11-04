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

        guard case .approval(let approval?) = try message("approval.json") else { return XCTFail() }
        XCTAssertEqual(approval.name, "bash")
        XCTAssertEqual(approval.always, ["bash: rm *"])
        guard case .approval(nil) = try message("approval-none.json") else { return XCTFail() }

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
        var saved: [ArtifactReply] = []
        model.onArtifact = { saved.append($0) }
        model.apply(try message("artifact.json"))
        XCTAssertEqual(saved.count, 1)
        XCTAssertNotNil(model.pages["a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0"])
    }
}
