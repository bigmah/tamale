// One session's socket, `wss://<sid>.<hub>/ws`, as the web page's `connect()` runs it
// (src/web/app.js): tamale opens an encrypted channel with `hs0` (a nonce), the app answers
// `hs1`, tamale `hs2`, and from then on every /web message travels sealed in `box` frames.
// Reconnects with backoff, keeps retrying while the session's tamale is offline (503), and
// stops for good on close code 4000 (the session ended, or the key is wrong).
//
// Nothing here takes plaintext. Whether a session is encrypted is only the hub's word, and a
// hub that said one wasn't could then read what's typed here and forge its approvals. So, as in
// the terminal, an unencrypted session (`[hub] e2e = false`) only opens in a browser.

import Foundation

public enum ChannelState: Equatable, Sendable {
    case idle
    case connecting
    /// Connected; the handshake is under way.
    case handshaking
    case live
    /// tamale is offline or the connection dropped; retrying.
    case retrying(String)
    /// Won't retry: the session ended, or the key doesn't fit.
    case stopped(String)

    public var isLive: Bool { if case .live = self { true } else { false } }
}

@MainActor
public final class SessionChannel {
    public let hub: HubClient
    public let sid: String

    public var onState: (ChannelState) -> Void = { _ in }
    public var onMessage: (Incoming) -> Void = { _ in }
    /// Bytes in so far of a message that's still arriving in pieces.
    public var onProgress: (Int) -> Void = { _ in }

    /// Entries asked for on connecting. Older tamales ignore this and send them all.
    static let tail = 60

    public private(set) var state: ChannelState = .idle {
        didSet { if state != oldValue { onState(state) } }
    }

    private var task: URLSessionWebSocketTask?
    private var handshake: NoiseHandshake?
    private var transport: NoiseTransport?
    private var retry = 0
    private var generation = 0
    private var wanted = false
    private var pinger: Task<Void, Never>?
    private let socketSession: URLSession

    public init(hub: HubClient, sid: String) {
        self.hub = hub
        self.sid = sid
        let config = URLSessionConfiguration.ephemeral
        config.httpShouldSetCookies = false
        config.httpCookieAcceptPolicy = .never
        socketSession = URLSession(configuration: config)
    }

    public func start() {
        wanted = true
        if case .live = state { return }
        if task != nil, state == .connecting || state == .handshaking { return }
        connect()
    }

    public func stop() {
        wanted = false
        teardown()
        state = .idle
    }

    /// Reconnects now if the connection isn't live (after the app comes back to the
    /// foreground, say, when iOS has quietly dropped the socket).
    public func resume() {
        guard wanted else { return }
        switch state {
        case .live:
            task?.sendPing { [weak self] error in
                guard error != nil else { return }
                Task { @MainActor in self?.dropped("Reconnecting…") }
            }
        case .stopped: break
        default:
            retry = 0
            teardown()
            connect()
        }
    }

    /// Sends one message; false if not connected.
    @discardableResult
    public func send(_ message: Outgoing) -> Bool {
        guard state.isLive, let task, var transport else { return false }
        let boxes = transport.seal(message.json)
        self.transport = transport
        for m in boxes {
            let frame = Self.frame(["t": "box", "m": m])
            task.send(.string(frame)) { _ in }
        }
        return true
    }

    // MARK: - Connection

    private func teardown() {
        generation += 1
        pinger?.cancel()
        pinger = nil
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        handshake = nil
        transport = nil
    }

    private func connect() {
        teardown()
        let generation = self.generation
        state = .connecting
        Task {
            do {
                let request = try await hub.socketRequest(sid: sid)
                guard generation == self.generation, wanted else { return }
                let task = socketSession.webSocketTask(with: request)
                task.maximumMessageSize = 64 << 20
                self.task = task
                task.resume()
                state = .handshaking
                receive(task, generation)
                startPinging(task, generation)
            } catch let error as HubError {
                guard generation == self.generation else { return }
                switch error {
                case .unknownKey, .noSession: state = .stopped(error.localizedDescription)
                default: scheduleRetry(error.localizedDescription)
                }
            } catch {
                guard generation == self.generation else { return }
                scheduleRetry(error.localizedDescription)
            }
        }
    }

    private func startPinging(_ task: URLSessionWebSocketTask, _ generation: Int) {
        pinger = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(25))
                guard !Task.isCancelled else { return }
                task.sendPing { error in
                    guard error != nil else { return }
                    Task { @MainActor in
                        guard let self, generation == self.generation else { return }
                        self.dropped("Reconnecting…")
                    }
                }
            }
        }
    }

    private func receive(_ task: URLSessionWebSocketTask, _ generation: Int) {
        task.receive { [weak self] result in
            Task { @MainActor in
                guard let self, generation == self.generation else { return }
                switch result {
                case .success(.string(let text)):
                    self.frame(text, task)
                    if generation == self.generation { self.receive(task, generation) }
                case .success(.data):
                    self.receive(task, generation)
                case .success:
                    self.receive(task, generation)
                case .failure:
                    self.closed(task)
                }
            }
        }
    }

    private func closed(_ task: URLSessionWebSocketTask) {
        let status = (task.response as? HTTPURLResponse)?.statusCode
        let code = task.closeCode.rawValue
        let reason = task.closeReason.flatMap { String(data: $0, encoding: .utf8) } ?? ""
        if code == 4000 {
            teardown()
            state = .stopped(reason.isEmpty ? "tamale stopped serving this session." : Self.explain(reason))
            return
        }
        switch status {
        case 401:
            teardown()
            state = .stopped(HubError.unknownKey.localizedDescription)
        case 404:
            Task { await hub.socketFailed(sid: sid) }
            teardown()
            state = .stopped(HubError.noSession.localizedDescription)
        case 503:
            dropped("tamale is offline. Retrying…")
        case 429:
            dropped("Too many connections. Retrying…")
        default:
            dropped(code == 1012 ? "tamale reconnected. Reconnecting…" : "Reconnecting…")
        }
    }

    static func explain(_ reason: String) -> String {
        switch reason {
        case "e2e: wrong key": "This key doesn't fit this session. Scan the key the session was started with."
        default: reason.hasPrefix("e2e: ") ? String(reason.dropFirst(5)).capitalizedFirst : reason
        }
    }

    private func dropped(_ why: String) {
        teardown()
        scheduleRetry(why)
    }

    private func scheduleRetry(_ why: String) {
        guard wanted else { return }
        state = .retrying(why)
        let delay = min(5.0, 0.25 * pow(2, Double(retry)))
        retry += 1
        let generation = self.generation
        Task {
            try? await Task.sleep(for: .seconds(delay))
            guard generation == self.generation, wanted, case .retrying = state else { return }
            connect()
        }
    }

    /// Handles one frame; anything out of place drops the connection, and with it
    /// whatever was forged.
    func frame(_ text: String, _ task: URLSessionWebSocketTask) {
        do {
            guard let object = try JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any],
                  let t = object["t"] as? String
            else { throw NoiseError.badFlag }
            switch t {
            case "hs0" where handshake == nil && transport == nil:
                guard let n = object["n"] as? String, let nonce = Data(lenientBase64: n) else { throw NoiseError.notBase64 }
                var hs = NoiseHandshake(psk: hub.key.psk, sid: sid, nonce: nonce)
                // `tail`: start with the end of the session, and page back for the rest.
                let first = hs.first(Data(#"{"v":1,"tail":\#(Self.tail)}"#.utf8))
                handshake = hs
                task.send(.string(Self.frame(["t": "hs1", "m": first.base64EncodedString()]))) { _ in }
            case "hs2" where handshake != nil:
                guard let m = object["m"] as? String, let message = Data(lenientBase64: m) else { throw NoiseError.notBase64 }
                transport = try handshake!.finish(message)
                handshake = nil
                ready()
            case "box" where transport != nil:
                guard let m = object["m"] as? String else { throw NoiseError.notBase64 }
                if let message = try transport!.open(m) {
                    deliver(message)
                } else {
                    onProgress(transport!.partialBytes)
                }
            default:
                guard handshake == nil, transport == nil else { throw NoiseError.badFlag }
                // Plaintext from the start: not worth retrying, whoever's sending it.
                teardown()
                state = .stopped("This session isn't encrypted ([hub] e2e = false there), so it only opens in a browser.")
            }
        } catch {
            dropped("The encrypted connection broke. Reconnecting…")
        }
    }

    private func ready() {
        retry = 0
        state = .live
    }

    private func deliver(_ text: String) {
        guard let message = try? Incoming.decode(text) else { return }
        onMessage(message)
    }

    static func frame(_ object: [String: String]) -> String {
        let data = try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        return String(decoding: data, as: UTF8.self)
    }
}

extension String {
    var capitalizedFirst: String { prefix(1).uppercased() + dropFirst() }
}
