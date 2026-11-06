// Gemini, for talking about an artifact by voice. The API key is the user's own, pasted in
// Keys; it stays in the Keychain on this device and goes only to Google's API.

import Foundation
import Security

enum Gemini {
    private static let service = "com.radkey.tamale.gemini"
    private static let account = "GEMINI_API_KEY"
    static let host = "generativelanguage.googleapis.com"

    /// The voices the Live API offers, a few of many. Gemini picks one if none is set.
    static let voices = ["Puck", "Charon", "Kore", "Fenrir", "Aoede", "Leda", "Orus", "Zephyr"]

    static var apiKey: String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
        ]
        var out: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess, let data = out as? Data else { return nil }
        let key = String(decoding: data, as: UTF8.self)
        return key.isEmpty ? nil : key
    }

    static func save(apiKey: String) throws {
        let base: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(base as CFDictionary)
        var add = base
        add[kSecValueData as String] = Data(apiKey.utf8)
        add[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let status = SecItemAdd(add as CFDictionary, nil)
        guard status == errSecSuccess else { throw KeychainError(status: status) }
    }

    static func removeKey() {
        SecItemDelete([
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ] as CFDictionary)
    }

    /// `GEMINI_API_KEY=AIza…`, quotes and all, as it might be pasted from a `.env` file.
    static func clean(_ pasted: String) -> String {
        var s = pasted.trimmingCharacters(in: .whitespacesAndNewlines)
        if let eq = s.firstIndex(of: "="), s[..<eq].uppercased().contains("KEY") {
            s = String(s[s.index(after: eq)...])
        }
        return s.trimmingCharacters(in: CharacterSet(charactersIn: "\"' \n\t"))
    }

    /// The models this key can hold a live conversation with (those offering
    /// `bidiGenerateContent`), the best guess for voice first.
    static func liveModels(apiKey: String) async throws -> [String] {
        struct Page: Decodable {
            struct Model: Decodable {
                var name: String
                var supportedGenerationMethods: [String]?
            }
            var models: [Model]?
            var nextPageToken: String?
        }
        var names: [String] = []
        var token: String?
        repeat {
            var parts = URLComponents(string: "https://\(host)/v1beta/models")!
            parts.queryItems = [URLQueryItem(name: "pageSize", value: "1000")]
            if let token { parts.queryItems?.append(URLQueryItem(name: "pageToken", value: token)) }
            var request = URLRequest(url: parts.url!)
            request.setValue(apiKey, forHTTPHeaderField: "x-goog-api-key")
            let (data, response) = try await URLSession.shared.data(for: request)
            if let http = response as? HTTPURLResponse, http.statusCode != 200 {
                throw GeminiError.http(http.statusCode, errorMessage(data))
            }
            let page = try JSONDecoder().decode(Page.self, from: data)
            names += (page.models ?? [])
                .filter { $0.supportedGenerationMethods?.contains("bidiGenerateContent") == true }
                .map { $0.name.hasPrefix("models/") ? String($0.name.dropFirst(7)) : $0.name }
                .filter { name in !["transcribe", "translate", "robotics"].contains { name.contains($0) } }
            token = page.nextPageToken?.isEmpty == false ? page.nextPageToken : nil
        } while token != nil
        return names.sorted { a, b in
            let (va, vb) = (version(a), version(b))
            if va != vb { return va > vb }
            if penalty(a) != penalty(b) { return penalty(a) < penalty(b) }
            return a < b
        }
    }

    /// 3.8 for `gemini-3.8-live`, 2.5 for `gemini-2.5-flash-native-audio-latest`.
    private static func version(_ name: String) -> Double {
        guard let r = name.range(of: #"\d+(\.\d+)?"#, options: .regularExpression) else { return 0 }
        return Double(name[r]) ?? 0
    }

    /// Within a version, the plain conversational model first: thinking ones answer slower.
    private static func penalty(_ name: String) -> Int {
        (name.contains("thinking") ? 2 : 0) + (name.contains("preview") || name.contains("exp") ? 1 : 0)
    }

    static func errorMessage(_ data: Data) -> String {
        struct Body: Decodable { struct E: Decodable { var message: String? }; var error: E? }
        return (try? JSONDecoder().decode(Body.self, from: data))?.error?.message
            ?? String(decoding: data.prefix(300), as: UTF8.self)
    }
}

enum GeminiError: LocalizedError {
    case http(Int, String)
    case noModel
    case closed(String)
    case microphone

    var errorDescription: String? {
        switch self {
        case .http(400, let m), .http(403, let m): return "Gemini refused the key: \(m)"
        case .http(let code, let m): return "Gemini answered \(code): \(m)"
        case .noModel: return "This key can't use any Gemini Live model."
        case .closed(let reason): return reason.isEmpty ? "Gemini ended the conversation." : "Gemini ended the conversation: \(reason)"
        case .microphone: return "tamale can't use the microphone. Allow it in Settings › tamale."
        }
    }
}
