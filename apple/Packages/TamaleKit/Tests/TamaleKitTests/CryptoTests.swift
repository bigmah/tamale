import CryptoKit
import Foundation
import XCTest

@testable import TamaleKit

/// apple/Fixtures, which src/web/fixtures.rs and hub/src/serve.rs write.
let fixtures = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    .deletingLastPathComponent().deletingLastPathComponent()
    .appendingPathComponent("Fixtures")

func fixture(_ name: String) throws -> Data {
    try Data(contentsOf: fixtures.appendingPathComponent(name))
}

func fixtureText(_ name: String) throws -> String {
    String(decoding: try fixture(name), as: UTF8.self)
}

struct Vectors: Decodable {
    struct Sealed: Decodable { var plain: String; var sealed: [String] }
    struct Label: Decodable { var kind: String; var plain: String; var sealed: String }
    struct Long: Decodable { var `repeat`: String; var count: Int }
    var key, auth, id, psk, meta, mid, sid, nonce: String
    var initiator_ephemeral, responder_ephemeral, hs1_payload, hs1, hs2: String
    var long: Long
    var down: [Sealed]
    var up: [Sealed]
    var sealed: [Label]
    var not_keys: [String]

    func plain(_ text: String) -> String {
        text == "<long>" ? String(repeating: long.repeat, count: long.count) : text
    }
}

final class CryptoTests: XCTestCase {
    var v: Vectors!

    override func setUpWithError() throws {
        v = try JSONDecoder().decode(Vectors.self, from: fixture("e2e-vectors.json"))
    }

    func testKeyDerivationsMatchTamale() throws {
        let key = try XCTUnwrap(TamaleKey(v.key))
        XCTAssertEqual(key.text, v.key)
        XCTAssertEqual(key.auth, v.auth)
        XCTAssertEqual(key.psk.hex, v.psk)
        XCTAssertEqual(key.meta.hex, v.meta)
        XCTAssertEqual(key.mid, v.mid)
        XCTAssertEqual(key.id, v.id)
        XCTAssertEqual(TamaleKey(bytes: key.bytes).text, v.key, "one spelling")
        for bad in v.not_keys {
            XCTAssertNil(TamaleKey(bad), bad)
        }
    }

    func testKeyLinksFromTheSitesQRCodeOrAPaste() throws {
        let link = try XCTUnwrap(KeyLink("https://tamale-hub.com/#\(v.key)"))
        XCTAssertEqual(link.key.text, v.key)
        XCTAssertEqual(link.hub?.absoluteString, "https://tamale-hub.com")
        let local = try XCTUnwrap(KeyLink("http://hub.localhost:8787/#\(v.key)"))
        XCTAssertEqual(local.hub?.absoluteString, "http://hub.localhost:8787")
        let bare = try XCTUnwrap(KeyLink("  \(v.key)\n"))
        XCTAssertNil(bare.hub)
        XCTAssertNil(KeyLink("https://tamale-hub.com/#nope"))
        XCTAssertNil(KeyLink("ftp://x/#\(v.key)"))

        let open = try XCTUnwrap(SessionLink("https://tamale-hub.com/open/\(v.sid)"))
        XCTAssertEqual(open.sid, v.sid)
        XCTAssertEqual(open.hub.absoluteString, "https://tamale-hub.com")
        XCTAssertNil(SessionLink("https://tamale-hub.com/open/xyz"))
    }

    func testHandshakeAndTransportMatchSnowByteForByte() throws {
        let key = try XCTUnwrap(TamaleKey(v.key))
        let ephemeral = try Curve25519.KeyAgreement.PrivateKey(
            rawRepresentation: XCTUnwrap(Data(hex: v.initiator_ephemeral)))
        var hs = NoiseHandshake(
            psk: key.psk, sid: v.sid, nonce: try XCTUnwrap(Data(lenientBase64: v.nonce)), ephemeral: ephemeral)
        let hs1 = hs.first(Data(v.hs1_payload.utf8))
        XCTAssertEqual(hs1.base64EncodedString(), v.hs1)
        var transport = try hs.finish(XCTUnwrap(Data(lenientBase64: v.hs2)))

        for message in v.down {
            var opened: [String?] = []
            for sealed in message.sealed { opened.append(try transport.open(sealed)) }
            XCTAssertEqual(opened.last!, v.plain(message.plain))
            XCTAssertTrue(opened.dropLast().allSatisfy { $0 == nil })
        }
        XCTAssertEqual(v.down[2].sealed.count, 2, "the long message goes in two pieces")

        for message in v.up {
            XCTAssertEqual(transport.seal(v.plain(message.plain)), message.sealed)
        }
    }

    func testATamperedHandshakeOrMessageFails() throws {
        let key = try XCTUnwrap(TamaleKey(v.key))
        let ephemeral = try Curve25519.KeyAgreement.PrivateKey(
            rawRepresentation: XCTUnwrap(Data(hex: v.initiator_ephemeral)))
        let nonce = try XCTUnwrap(Data(lenientBase64: v.nonce))
        var hs2 = try XCTUnwrap(Data(lenientBase64: v.hs2))

        // Another session or nonce: the transcript differs, so the answer doesn't verify.
        var other = NoiseHandshake(psk: key.psk, sid: String(repeating: "f", count: 32), nonce: nonce, ephemeral: ephemeral)
        _ = other.first(Data(v.hs1_payload.utf8))
        XCTAssertThrowsError(try other.finish(hs2))

        var hs = NoiseHandshake(psk: key.psk, sid: v.sid, nonce: nonce, ephemeral: ephemeral)
        _ = hs.first(Data(v.hs1_payload.utf8))
        hs2[40] ^= 1
        XCTAssertThrowsError(try hs.finish(hs2))

        var good = NoiseHandshake(psk: key.psk, sid: v.sid, nonce: nonce, ephemeral: ephemeral)
        _ = good.first(Data(v.hs1_payload.utf8))
        var transport = try good.finish(XCTUnwrap(Data(lenientBase64: v.hs2)))
        // Out of order: the second message first.
        XCTAssertThrowsError(try transport.open(v.down[1].sealed[0]))
    }

    func testSealedLabelsOpen() throws {
        let key = try XCTUnwrap(TamaleKey(v.key))
        for label in v.sealed {
            let plain = try Sealed.open(label.sealed, key: key, sid: v.sid, kind: label.kind)
            XCTAssertEqual(String(decoding: plain, as: UTF8.self), label.plain)
            XCTAssertThrowsError(try Sealed.open(label.sealed, key: key, sid: v.sid, kind: "other"))
            XCTAssertThrowsError(
                try Sealed.open(label.sealed, key: key, sid: String(repeating: "f", count: 32), kind: label.kind))
        }
        let label = try Sealed.label(v.sealed[0].sealed, key: key, sid: v.sid)
        XCTAssertEqual(label, SessionLabel(host: "laptop", cwd: "~/dev/tamale", model: "opus"))
        let status = try Sealed.status(v.sealed[1].sealed, key: key, sid: v.sid)
        XCTAssertEqual(status.title, "Fix the login bug")

        let other = TamaleKey(bytes: Data(repeating: 9, count: 32))
        XCTAssertThrowsError(try Sealed.open(v.sealed[0].sealed, key: other, sid: v.sid, kind: "label"))
        let resealed = try Sealed.seal(Data("x".utf8), key: key, sid: v.sid, kind: "label")
        XCTAssertEqual(try Sealed.open(resealed, key: key, sid: v.sid, kind: "label"), Data("x".utf8))
    }
}
