import Foundation
import XCTest

@testable import TamaleKit

/// Against a real `tamale-hub serve` and a stand-in tamale session on this machine; run by
/// apple/scripts/integration.sh, which sets TAMALE_IT_HUB, TAMALE_IT_KEY and TAMALE_IT_SID.
/// Skipped without them.
final class IntegrationTests: XCTestCase {
    struct Setup {
        var hub: URL
        var key: TamaleKey
        var sid: String
    }

    func setup() throws -> Setup {
        let env = ProcessInfo.processInfo.environment
        guard let hub = env["TAMALE_IT_HUB"].flatMap(URL.init(string:)),
              let key = env["TAMALE_IT_KEY"].flatMap(TamaleKey.init),
              let sid = env["TAMALE_IT_SID"]
        else { throw XCTSkip("run apple/scripts/integration.sh") }
        return Setup(hub: hub, key: key, sid: sid)
    }

    func testListsDecryptsAndDrivesASession() async throws {
        let setup = try setup()
        try await drive(setup, cookies: false)
    }

    /// The path for hubs from before bearer keys: cookies, a ticket and the Origin header.
    func testTheCookieFallbackForOlderHubs() async throws {
        let setup = try setup()
        try await drive(setup, cookies: true)
    }

    func testAnUnknownKeyIsRefused() async throws {
        let setup = try setup()
        let client = HubClient(origin: setup.hub, key: TamaleKey(bytes: Data(repeating: 3, count: 32)))
        do {
            _ = try await client.sessions()
            XCTFail("listed with an unknown key")
        } catch let error as HubError {
            XCTAssertEqual(error, .unknownKey)
        }
    }

    /// Ends the stand-in session, so it runs last: XCTest goes in alphabetical order.
    @MainActor
    func testZClosingASessionEndsItAndTheHubForgetsIt() async throws {
        let setup = try setup()
        let client = HubClient(origin: setup.hub, key: setup.key)
        let model = SessionModel(channel: SessionChannel(hub: client, sid: setup.sid))
        try await model.close()
        model.stop()
        // The session ended and told the hub, which dropped it rather than listing it offline.
        let listed = try await client.sessions().sessions.contains { $0.sid == setup.sid }
        XCTAssertFalse(listed)
    }

    @MainActor
    func drive(_ setup: Setup, cookies: Bool) async throws {
        let client = HubClient(origin: setup.hub, key: setup.key)
        if cookies { await client.forceCookies() }
        let list = try await client.sessions()
        let usesBearer = await client.usesBearer
        XCTAssertEqual(usesBearer, !cookies)
        let row = try XCTUnwrap(list.sessions.first { $0.sid == setup.sid })
        XCTAssertTrue(row.e2e)
        XCTAssertTrue(row.online)
        XCTAssertEqual(row.host, "", "the hub sees no label")
        let shown = row.shown(with: setup.key)
        XCTAssertEqual(shown.label.host, "fake-host")
        XCTAssertEqual(shown.label.cwd, "~/dev/fake")

        let model = SessionModel(channel: SessionChannel(hub: client, sid: setup.sid))
        model.start()
        try await until("live") { model.connection == .live }
        try await until("entries") { model.entries.count >= 3 }

        let prompt = "hello \(cookies ? "cookies" : "bearer") \(Int.random(in: 0..<10000))"
        let sent = model.send(prompt)
        XCTAssertTrue(sent)
        try await until("reply") {
            model.entries.contains { $0.kind == .assistant && ($0.md ?? "").contains(prompt) }
        }
        let reply = try XCTUnwrap(model.entries.last { $0.kind == .assistant })
        let block = try XCTUnwrap(reply.prose.artifacts.first)
        XCTAssertEqual(block.title, "Echo")
        let id = try XCTUnwrap(block.id)
        model.requestArtifact(id)
        try await until("artifact") { model.pages[id] != nil }
        let page = try XCTUnwrap(model.pages[id])
        XCTAssertTrue(page.html?.contains(prompt) ?? false)
        XCTAssertEqual(page.hash, block.hash)

        // A long message crosses in pieces.
        let long = String(repeating: "λ", count: 40_000)
        _ = model.send(long)
        try await until("long reply") {
            model.entries.contains { $0.kind == .user && $0.text == long }
        }

        try await until("an approval") { model.approval?.name == "bash" }
        _ = model.approve(.no, reason: "not now")
        try await until("approval answered") { model.approval == nil }
        model.stop()
    }

    @MainActor
    func until(_ what: String, timeout: Double = 15, _ check: @escaping () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if check() { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        XCTFail("timed out waiting for \(what)")
        throw CancellationError()
    }
}

extension HubClient {
    /// Acts as against a hub from before bearer keys.
    func forceCookies() { mode = .cookie(nil) }
}
