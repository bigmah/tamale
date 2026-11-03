// A tamale key (`thk_` + 52 lowercase base32 characters) and what's derived from it, as in
// `hub/protocol/src/lib.rs`, `src/web/e2e.rs` and the hub page's `seal.js`. The key itself
// never leaves the device: the hub only ever gets `auth`.

import CryptoKit
import Foundation

public struct TamaleKey: Hashable, Sendable {
    public static let prefix = "thk_"
    static let alphabet = Array("abcdefghijklmnopqrstuvwxyz234567".utf8)

    /// The 32 bytes.
    public let bytes: Data
    /// The key as written: `thk_…`.
    public let text: String
    /// What proves the key to the hub, in lowercase hex. The hub keeps only its SHA-256.
    public let auth: String
    /// The end-to-end handshake's secret, which never reaches the hub.
    public let psk: Data
    /// Seals session labels (AES-256-GCM).
    public let meta: Data
    /// The id sessions name this key by (16 bytes, hex), revealing nothing about it.
    public let mid: String
    /// The id the hub's key list names this key by: the start of SHA-256(auth), hex.
    public let id: String

    /// The key `text` stands for, if it's one. A key has exactly one spelling.
    public init?(_ text: String) {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let bytes = TamaleKey.decode(text) else { return nil }
        self.init(bytes: bytes)
    }

    public init(bytes: Data) {
        precondition(bytes.count == 32)
        self.bytes = bytes
        text = TamaleKey.encode(bytes)
        let key = SymmetricKey(data: bytes)
        auth = TamaleKey.derive(key, info: "tamale-hub auth", count: 32).hex
        psk = TamaleKey.derive(key, info: "tamale-hub e2e", count: 32)
        let pskKey = SymmetricKey(data: psk)
        mid = TamaleKey.derive(pskKey, info: "tamale-hub mid", count: 16).hex
        meta = TamaleKey.derive(pskKey, info: "tamale-hub meta", count: 32)
        id = String(Data(SHA256.hash(data: Data(auth.utf8))).hex.prefix(16))
    }

    static func derive(_ key: SymmetricKey, info: String, count: Int) -> Data {
        let out = HKDF<SHA256>.deriveKey(
            inputKeyMaterial: key, salt: Data(), info: Data(info.utf8), outputByteCount: count)
        return out.withUnsafeBytes { Data($0) }
    }

    static func encode(_ bytes: Data) -> String {
        var out = prefix
        var buffer: UInt32 = 0
        var bits = 0
        for byte in bytes {
            buffer = ((buffer << 8) | UInt32(byte)) & 0xfff
            bits += 8
            while bits >= 5 {
                bits -= 5
                out.append(Character(UnicodeScalar(alphabet[Int((buffer >> UInt32(bits)) & 31)])))
            }
        }
        out.append(Character(UnicodeScalar(alphabet[Int((buffer << UInt32(5 - bits)) & 31)])))
        return out
    }

    static func decode(_ text: String) -> Data? {
        guard text.hasPrefix(prefix) else { return nil }
        let body = Array(text.utf8.dropFirst(prefix.utf8.count))
        guard body.count == 52 else { return nil }
        var out = Data()
        var buffer: UInt32 = 0
        var bits = 0
        for c in body {
            guard let value = alphabet.firstIndex(of: c) else { return nil }
            buffer = ((buffer << 5) | UInt32(value)) & 0xfff
            bits += 5
            if bits >= 8 {
                bits -= 8
                out.append(UInt8((buffer >> UInt32(bits)) & 0xff))
            }
        }
        // The last character's spare bits are zero, so each key has one spelling.
        guard out.count == 32, buffer & ((1 << UInt32(bits)) - 1) == 0 else { return nil }
        return out
    }
}

/// What the site's QR code or a paste gives: a key, and the hub it's for if known.
public struct KeyLink: Equatable, Sendable {
    public var key: TamaleKey
    /// `https://tamale-hub.com`, without a trailing slash.
    public var hub: URL?

    /// Reads `https://<hub>/#thk_…`, or a bare `thk_…`.
    public init?(_ text: String) {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if let key = TamaleKey(text) {
            self.key = key
            hub = nil
            return
        }
        guard let url = URL(string: text), let fragment = url.fragment,
              let key = TamaleKey(fragment), let origin = url.origin
        else { return nil }
        self.key = key
        hub = origin
    }
}

/// A terminal's QR code for one session: `https://<hub>/open/<sid>`.
public struct SessionLink: Equatable, Sendable {
    public var hub: URL
    public var sid: String

    public init?(_ url: URL) {
        let parts = url.path.split(separator: "/")
        guard parts.count == 2, parts[0] == "open", isSid(String(parts[1])),
              let origin = url.origin
        else { return nil }
        hub = origin
        sid = String(parts[1])
    }

    public init?(_ text: String) {
        guard let url = URL(string: text.trimmingCharacters(in: .whitespacesAndNewlines)) else { return nil }
        self.init(url)
    }
}

/// 32 lowercase hex characters, as session ids are.
public func isSid(_ text: String) -> Bool {
    text.utf8.count == 32 && text.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
}

extension URL {
    /// `scheme://host[:port]`, if this is an http(s) URL.
    public var origin: URL? {
        guard let scheme = scheme?.lowercased(), scheme == "https" || scheme == "http",
              let host = host?.lowercased(), !host.isEmpty
        else { return nil }
        var c = URLComponents()
        c.scheme = scheme
        c.host = host
        c.port = port
        return c.url
    }
}

extension Data {
    public var hex: String { map { String(format: "%02x", $0) }.joined() }

    public init?(hex: String) {
        guard hex.count % 2 == 0 else { return nil }
        var out = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index..<next], radix: 16) else { return nil }
            out.append(byte)
            index = next
        }
        self = out
    }

    /// Standard or URL-safe base64, padded or not, as the page's `fromBase64` takes.
    public init?(lenientBase64 text: String) {
        var s = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while s.count % 4 != 0 { s.append("=") }
        self.init(base64Encoded: s)
    }
}
