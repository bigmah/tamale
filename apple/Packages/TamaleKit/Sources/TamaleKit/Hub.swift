// The hub's REST API, as a native client uses it (see "Talking to the hub" in apple/ios.md):
// `Authorization: Bearer <auth value>` on /api/sessions, /api/ticket and the session socket.
//
// Hubs from before bearer auth ignore the header, so their session list comes back with no
// keys. Then the client falls back to what the hub's page does: trade the auth value for the
// `th` cookie, get a ticket, trade it for the session host's `ts` cookie, and open the socket
// with that cookie and the session host's Origin.

import Foundation

public struct HubKeyInfo: Decodable, Equatable, Sendable {
    public var name: String
    public var id: String
    public var created: Double?
    public var web: Bool?
}

public struct HubSession: Decodable, Equatable, Identifiable, Sendable {
    public var sid: String
    public var key: String
    public var host: String
    public var cwd: String
    public var model: String
    public var title: String
    public var mid: String?
    public var e2e: Bool
    public var sealedLabel: String?
    public var sealedStatus: String?
    public var busy: Bool
    public var approval: Bool
    public var online: Bool
    public var viewers: Int
    /// Seconds since anything happened.
    public var idle: Double

    public var id: String { sid }

    enum CodingKeys: String, CodingKey {
        case sid, key, host, cwd, model, title, mid, e2e, busy, approval, online, viewers, idle
        case sealedLabel = "sealed_label", sealedStatus = "sealed_status"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        func get<T: Decodable>(_ key: CodingKeys, _ fallback: T) -> T {
            (try? c.decodeIfPresent(T.self, forKey: key)) ?? fallback
        }
        sid = try c.decode(String.self, forKey: .sid)
        key = get(.key, "")
        host = get(.host, "")
        cwd = get(.cwd, "")
        model = get(.model, "")
        title = get(.title, "")
        mid = get(.mid, nil as String?)
        e2e = get(.e2e, false)
        sealedLabel = get(.sealedLabel, nil as String?)
        sealedStatus = get(.sealedStatus, nil as String?)
        busy = get(.busy, false)
        approval = get(.approval, false)
        online = get(.online, false)
        viewers = get(.viewers, 0)
        idle = get(.idle, 0)
    }

    /// The label and title, opened with `key` when they're sealed.
    public func shown(with key: TamaleKey) -> (label: SessionLabel, title: String, model: String) {
        var label = SessionLabel(host: host, cwd: cwd, model: model)
        var title = self.title
        var model = self.model
        if let sealed = sealedLabel, let open = try? Sealed.label(sealed, key: key, sid: sid) {
            label = open
            model = open.model
        }
        if let sealed = sealedStatus, let open = try? Sealed.status(sealed, key: key, sid: sid) {
            title = open.title
            if !open.model.isEmpty { model = open.model }
        }
        return (label, title, model)
    }
}

public struct SessionList: Decodable, Equatable, Sendable {
    public var keys: [HubKeyInfo]
    public var sessions: [HubSession]
}

public enum HubError: Error, Equatable, LocalizedError {
    /// The hub doesn't know this key (it was deleted, rotated or never registered).
    case unknownKey
    case tooManyAttempts
    case noSession
    case http(Int, String)
    case badResponse

    public var errorDescription: String? {
        switch self {
        case .unknownKey: "This hub doesn't know that key. It may have been rotated or deleted on the hub's site."
        case .tooManyAttempts: "Too many failed attempts; try again later."
        case .noSession: "This key doesn't have that session, or the session has ended."
        case .http(let code, let message): message.isEmpty ? "The hub answered \(code)." : message
        case .badResponse: "The hub sent something unexpected."
        }
    }
}

/// One key on one hub.
public actor HubClient {
    public nonisolated let origin: URL
    public nonisolated let key: TamaleKey

    enum Mode: Equatable {
        case unknown
        case bearer
        /// An older hub: the `th` cookie (name=value), once there is one.
        case cookie(String?)
    }

    var mode: Mode = .unknown
    /// Session host cookies (`ts`) by sid, in cookie mode.
    var sessionCookies: [String: String] = [:]
    nonisolated let http: URLSession
    private let noRedirects = NoRedirects()

    public init(origin: URL, key: TamaleKey) {
        self.origin = origin.origin ?? origin
        self.key = key
        let config = URLSessionConfiguration.ephemeral
        config.httpShouldSetCookies = false
        config.httpCookieAcceptPolicy = .never
        config.timeoutIntervalForRequest = 20
        config.waitsForConnectivity = false
        http = URLSession(configuration: config)
    }

    /// Whether the hub takes bearer keys (known after the first list).
    public var usesBearer: Bool { mode == .bearer }

    // MARK: URLs

    public nonisolated var host: String {
        var h = origin.host ?? ""
        if let port = origin.port { h += ":\(port)" }
        return h
    }

    nonisolated var secure: Bool { origin.scheme == "https" }

    /// `https://<sid>.<hub>`.
    public nonisolated func sessionOrigin(_ sid: String) -> String {
        "\(secure ? "https" : "http")://\(sid).\(host)"
    }

    /// `wss://<sid>.<hub>/ws`.
    public nonisolated func socketURL(_ sid: String) -> URL {
        URL(string: "\(secure ? "wss" : "ws")://\(sid).\(host)/ws")!
    }

    func api(_ path: String) -> URL { origin.appendingPathComponent("api").appendingPathComponent(path) }

    // MARK: Requests

    func send(_ request: URLRequest, redirects: Bool = true) async throws -> (Data, HTTPURLResponse) {
        let (data, response) = try await http.data(for: request, delegate: redirects ? nil : noRedirects)
        guard let response = response as? HTTPURLResponse else { throw HubError.badResponse }
        return (data, response)
    }

    static func failure(_ data: Data, _ response: HTTPURLResponse) -> HubError {
        let message = (try? JSONDecoder().decode([String: String].self, from: data))?["error"] ?? ""
        switch response.statusCode {
        case 401: return .unknownKey
        case 404: return .noSession
        case 429: return .tooManyAttempts
        default: return .http(response.statusCode, message)
        }
    }

    static func cookie(_ response: HTTPURLResponse) -> String? {
        guard let value = response.value(forHTTPHeaderField: "Set-Cookie") else { return nil }
        let pair = value.split(separator: ";", maxSplits: 1).first.map(String.init) ?? ""
        return pair.contains("=") && !pair.hasSuffix("=") ? pair : nil
    }

    /// The sessions under this key.
    public func sessions() async throws -> SessionList {
        if case .cookie = mode { return try await cookieSessions(retry: true) }
        var request = URLRequest(url: api("sessions"))
        request.setValue("Bearer \(key.auth)", forHTTPHeaderField: "Authorization")
        let (data, response) = try await send(request)
        guard response.statusCode == 200 else { throw Self.failure(data, response) }
        let list = try JSONDecoder().decode(SessionList.self, from: data)
        if list.keys.isEmpty {
            // A hub from before bearer keys, which looked for a cookie and found none.
            mode = .cookie(nil)
            return try await cookieSessions(retry: false)
        }
        mode = .bearer
        return list
    }

    /// Checks the hub knows this key; throws `HubError.unknownKey` if not.
    public func verify() async throws {
        _ = try await sessions()
    }

    func login() async throws -> String {
        var request = URLRequest(url: api("tokens"))
        request.httpMethod = "POST"
        request.setValue(origin.absoluteString.trimmingCharacters(in: ["/"]), forHTTPHeaderField: "Origin")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["add": key.auth])
        let (data, response) = try await send(request)
        guard response.statusCode == 200, let th = Self.cookie(response) else {
            throw Self.failure(data, response)
        }
        mode = .cookie(th)
        return th
    }

    func cookieSessions(retry: Bool) async throws -> SessionList {
        var th: String
        if case .cookie(let existing?) = mode { th = existing } else { th = try await login() }
        var request = URLRequest(url: api("sessions"))
        request.setValue(th, forHTTPHeaderField: "Cookie")
        let (data, response) = try await send(request)
        guard response.statusCode == 200 else { throw Self.failure(data, response) }
        let list = try JSONDecoder().decode(SessionList.self, from: data)
        if list.keys.isEmpty {
            // The cookie ran out or the hub restarted with a new secret: log in again once.
            mode = .cookie(nil)
            if retry { return try await cookieSessions(retry: false) }
            throw HubError.unknownKey
        }
        return list
    }

    /// The request that opens session `sid`'s socket.
    public func socketRequest(sid: String) async throws -> URLRequest {
        if mode == .unknown { _ = try await sessions() }
        var request = URLRequest(url: socketURL(sid))
        request.timeoutInterval = 20
        switch mode {
        case .cookie:
            let ts = try await sessionCookie(sid)
            request.setValue(sessionOrigin(sid), forHTTPHeaderField: "Origin")
            request.setValue(ts, forHTTPHeaderField: "Cookie")
        default:
            request.setValue("Bearer \(key.auth)", forHTTPHeaderField: "Authorization")
        }
        return request
    }

    /// Forgets a session host cookie that didn't work.
    public func socketFailed(sid: String) {
        sessionCookies[sid] = nil
    }

    func sessionCookie(_ sid: String) async throws -> String {
        if let ts = sessionCookies[sid] { return ts }
        guard case .cookie(let existing) = mode else { throw HubError.badResponse }
        let th: String
        if let existing { th = existing } else { th = try await login() }
        var request = URLRequest(url: api("ticket"))
        request.httpMethod = "POST"
        request.setValue(origin.absoluteString.trimmingCharacters(in: ["/"]), forHTTPHeaderField: "Origin")
        request.setValue(th, forHTTPHeaderField: "Cookie")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["sid": sid])
        let (data, response) = try await send(request)
        guard response.statusCode == 200 else {
            if response.statusCode == 401 { mode = .cookie(nil) }
            throw Self.failure(data, response)
        }
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let link = object["url"] as? String, let url = URL(string: link)
        else { throw HubError.badResponse }
        let (body, redirect) = try await send(URLRequest(url: url), redirects: false)
        guard redirect.statusCode == 303, let ts = Self.cookie(redirect) else {
            throw Self.failure(body, redirect)
        }
        sessionCookies[sid] = ts
        return ts
    }
}

final class NoRedirects: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}
