// The app's end of tamale's end-to-end encryption (see e2e.md and src/web/e2e.rs):
// Noise_NNpsk0_25519_AESGCM_SHA256 as an initiator, over CryptoKit, with the fragmenting of
// long messages above it. A port of src/web/noise.js, checked byte for byte against the
// `snow` crate by the vectors in apple/Fixtures/e2e-vectors.json.

import CryptoKit
import Foundation

public enum NoiseError: Error, Equatable {
    case shortMessage
    case badFlag
    case emptyMessage
    case notUTF8
    case notBase64
    case tooLong
    case decryptFailed
}

enum Noise {
    static let name = Data("Noise_NNpsk0_25519_AESGCM_SHA256".utf8)
    static let prologue = Data("tamale-hub e2e v1".utf8)
    static let maxMessage = 65535
    static let tag = 16
    static let more: UInt8 = 0
    static let last: UInt8 = 1
    static let maxPiece = maxMessage - tag - 1

    static func hmac(_ key: Data, _ data: Data) -> Data {
        Data(HMAC<SHA256>.authenticationCode(for: data, using: SymmetricKey(data: key)))
    }

    /// Noise's HKDF: `count` 32-byte outputs.
    static func hkdf(_ chainKey: Data, _ input: Data, count: Int) -> [Data] {
        let temp = hmac(chainKey, input)
        var out: [Data] = []
        var last = Data()
        for i in 1...count {
            last = hmac(temp, last + Data([UInt8(i)]))
            out.append(last)
        }
        return out
    }

    static func sha256(_ data: Data) -> Data { Data(SHA256.hash(data: data)) }
}

/// AES-GCM under one key, with Noise's nonces: four zero bytes, then a big-endian counter.
struct NoiseCipher {
    let key: SymmetricKey
    var n: UInt64 = 0

    init(_ key: Data) { self.key = SymmetricKey(data: key) }

    private mutating func nonce() -> AES.GCM.Nonce {
        var bytes = Data(count: 4)
        withUnsafeBytes(of: n.bigEndian) { bytes.append(contentsOf: $0) }
        n += 1
        return try! AES.GCM.Nonce(data: bytes)
    }

    mutating func encrypt(ad: Data, _ plain: Data) -> Data {
        let box = try! AES.GCM.seal(plain, using: key, nonce: nonce(), authenticating: ad)
        return box.ciphertext + box.tag
    }

    mutating func decrypt(ad: Data, _ sealed: Data) throws -> Data {
        let nonce = nonce()
        guard sealed.count >= Noise.tag else { throw NoiseError.shortMessage }
        let box = try AES.GCM.SealedBox(
            nonce: nonce, ciphertext: sealed.prefix(sealed.count - Noise.tag),
            tag: sealed.suffix(Noise.tag))
        do {
            return try AES.GCM.open(box, using: key, authenticating: ad)
        } catch {
            throw NoiseError.decryptFailed
        }
    }
}

/// The handshake, as the app runs it:
///
///     var hs = NoiseHandshake(psk: key.psk, sid: sid, nonce: n)
///     let hs1 = hs.first(payload)          // to tamale
///     let transport = try hs.finish(hs2)   // from tamale; throws if it's not genuine
public struct NoiseHandshake {
    var ck: Data
    var h: Data
    var cipher: NoiseCipher
    let ephemeral: Curve25519.KeyAgreement.PrivateKey

    public init(psk: Data, sid: String, nonce: Data, ephemeral: Curve25519.KeyAgreement.PrivateKey? = nil) {
        // The protocol name is exactly 32 bytes, so it's used as is, not hashed.
        ck = Noise.name
        h = Noise.name
        cipher = NoiseCipher(Data(count: 32))
        self.ephemeral = ephemeral ?? Curve25519.KeyAgreement.PrivateKey()
        mixHash(Noise.prologue + Data(sid.utf8) + nonce)
        // psk: MixKeyAndHash.
        let out = Noise.hkdf(ck, psk, count: 3)
        ck = out[0]
        mixHash(out[1])
        cipher = NoiseCipher(out[2])
    }

    mutating func mixHash(_ data: Data) { h = Noise.sha256(h + data) }

    mutating func mixKey(_ input: Data) {
        let out = Noise.hkdf(ck, input, count: 2)
        ck = out[0]
        cipher = NoiseCipher(out[1])
    }

    /// A public key in a message: with a PSK in play, it's mixed into the key too.
    mutating func mixEphemeral(_ publicKey: Data) {
        mixHash(publicKey)
        mixKey(publicKey)
    }

    /// Message 1: `psk, e`.
    public mutating func first(_ payload: Data) -> Data {
        let pub = ephemeral.publicKey.rawRepresentation
        mixEphemeral(pub)
        let sealed = cipher.encrypt(ad: h, payload)
        mixHash(sealed)
        return pub + sealed
    }

    /// Message 2: `e, ee`.
    public mutating func finish(_ message: Data) throws -> NoiseTransport {
        let message = Data(message)
        guard message.count >= 32 + Noise.tag else { throw NoiseError.shortMessage }
        let theirs = message.prefix(32)
        mixEphemeral(theirs)
        let theirKey = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: theirs)
        let shared = try ephemeral.sharedSecretFromKeyAgreement(with: theirKey)
        mixKey(shared.withUnsafeBytes { Data($0) })
        let sealed = message.suffix(from: 32)
        _ = try cipher.decrypt(ad: h, Data(sealed))
        mixHash(Data(sealed))
        let out = Noise.hkdf(ck, Data(), count: 2)
        return NoiseTransport(send: NoiseCipher(out[0]), receive: NoiseCipher(out[1]))
    }
}

/// One channel after its handshake. Noise's counters mean a message that was dropped,
/// repeated, reordered or reflected fails to decrypt; a failure is final.
public struct NoiseTransport {
    var send: NoiseCipher
    var receive: NoiseCipher
    var partial = Data()
    /// Caps a whole message, across its pieces.
    public var maxMessage = 64 << 20

    /// Seals one message as one or more transport messages, in base64.
    public mutating func seal(_ message: String) -> [String] {
        let bytes = Data(message.utf8)
        var sealed: [String] = []
        var at = 0
        repeat {
            let end = min(at + Noise.maxPiece, bytes.count)
            let piece = bytes[at..<end]
            at = end
            let flag = at < bytes.count ? Noise.more : Noise.last
            sealed.append(send.encrypt(ad: Data(), Data([flag]) + piece).base64EncodedString())
        } while at < bytes.count
        return sealed
    }

    /// Opens one transport message: the message once its last piece is in, else nil.
    public mutating func open(_ sealed: String) throws -> String? {
        guard let data = Data(lenientBase64: sealed) else { throw NoiseError.notBase64 }
        guard data.count <= Noise.maxMessage else { throw NoiseError.tooLong }
        let plain = try receive.decrypt(ad: Data(), data)
        guard let flag = plain.first else { throw NoiseError.emptyMessage }
        guard flag == Noise.more || flag == Noise.last else { throw NoiseError.badFlag }
        guard partial.count + plain.count - 1 <= maxMessage else { throw NoiseError.tooLong }
        partial.append(plain.dropFirst())
        if flag == Noise.more { return nil }
        defer { partial = Data() }
        guard let text = String(data: partial, encoding: .utf8) else { throw NoiseError.notUTF8 }
        return text
    }
}
