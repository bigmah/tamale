// Opens the session list's sealed labels (see `Keys::seal` in src/web/e2e.rs): base64 of a
// 12-byte nonce and AES-256-GCM ciphertext under the key's meta key, bound to the session id
// and the kind of label.

import CryptoKit
import Foundation

public enum SealedError: Error {
    case malformed
    case wrongKey
}

/// What a session's `sealed_label` holds.
public struct SessionLabel: Codable, Equatable, Sendable {
    public var host: String
    public var cwd: String
    public var model: String

    public init(host: String = "", cwd: String = "", model: String = "") {
        self.host = host
        self.cwd = cwd
        self.model = model
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        host = try c.decodeIfPresent(String.self, forKey: .host) ?? ""
        cwd = try c.decodeIfPresent(String.self, forKey: .cwd) ?? ""
        model = try c.decodeIfPresent(String.self, forKey: .model) ?? ""
    }
}

/// What a session's `sealed_status` holds.
public struct SessionTitle: Codable, Equatable, Sendable {
    public var title: String
    public var model: String

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        model = try c.decodeIfPresent(String.self, forKey: .model) ?? ""
    }
}

public enum Sealed {
    /// The plaintext of a label sealed for session `sid`; throws if anything is wrong.
    public static func open(_ sealed: String, key: TamaleKey, sid: String, kind: String) throws -> Data {
        guard let bytes = Data(lenientBase64: sealed), bytes.count >= 12 + 16 else {
            throw SealedError.malformed
        }
        do {
            let box = try AES.GCM.SealedBox(
                nonce: AES.GCM.Nonce(data: bytes.prefix(12)),
                ciphertext: bytes.dropFirst(12).dropLast(16),
                tag: bytes.suffix(16))
            return try AES.GCM.open(
                box, using: SymmetricKey(data: key.meta), authenticating: Data((sid + kind).utf8))
        } catch {
            throw SealedError.wrongKey
        }
    }

    /// Seals as tamale does, for tests and previews.
    public static func seal(_ plain: Data, key: TamaleKey, sid: String, kind: String, nonce: Data? = nil) throws -> String {
        let nonce = try nonce.map { try AES.GCM.Nonce(data: $0) } ?? AES.GCM.Nonce()
        let box = try AES.GCM.seal(
            plain, using: SymmetricKey(data: key.meta), nonce: nonce,
            authenticating: Data((sid + kind).utf8))
        return (Data(nonce) + box.ciphertext + box.tag).base64EncodedString()
    }

    public static func label(_ sealed: String, key: TamaleKey, sid: String) throws -> SessionLabel {
        try JSONDecoder().decode(SessionLabel.self, from: open(sealed, key: key, sid: sid, kind: "label"))
    }

    public static func status(_ sealed: String, key: TamaleKey, sid: String) throws -> SessionTitle {
        try JSONDecoder().decode(SessionTitle.self, from: open(sealed, key: key, sid: sid, kind: "status"))
    }
}
