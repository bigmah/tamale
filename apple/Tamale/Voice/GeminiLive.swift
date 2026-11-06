// A spoken conversation with Gemini over the Live API: a WebSocket to Google that takes the
// microphone as 16 kHz PCM and answers with 24 kHz PCM, with transcripts of both sides. The
// caller supplies the system instruction and, optionally, tools: functions Gemini can call
// mid-conversation (the app runs them and answers), and events the app tells it about. Voice processing (echo cancellation)
// stays on, so Gemini doesn't hear itself through the speaker and interrupt its own answer.

import AVFoundation
import Foundation
import Observation

@MainActor
@Observable
final class GeminiLive {
    enum Phase: Equatable {
        case idle, connecting, listening, speaking
        case ended(String?)
    }

    struct Line: Identifiable {
        /// `action`: something the app did or saw for Gemini (a tool call, an event).
        enum Who { case you, gemini, action }
        let id = UUID()
        var who: Who
        var text: String
        var closed = false
    }

    private(set) var phase: Phase = .idle
    private(set) var lines: [Line] = []
    private(set) var model = ""
    /// The microphone's loudness, 0…1, for the UI.
    private(set) var level: Float = 0
    /// Set when Google says the connection will close soon.
    private(set) var notice: String?
    var muted = false {
        didSet {
            pipe.muted = muted
            if muted { sendJSON(["realtimeInput": ["audioStreamEnd": true]]) }
        }
    }

    /// Runs a function Gemini called, by name with its arguments, and returns its result.
    @ObservationIgnored var onToolCall: ((String, [String: Any]) async -> [String: Any])?

    private var socket: URLSessionWebSocketTask?
    /// Events waiting for Gemini to finish speaking.
    private var pendingEvents: [String] = []
    private let engine = AVAudioEngine()
    private let player = AVAudioPlayerNode()
    private let pipe = MicPipe()
    private var queued = 0
    private var playGeneration = 0
    private var observers: [NSObjectProtocol] = []
    private static let outFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 24000, channels: 1, interleaved: false)!

    var active: Bool {
        switch phase {
        case .idle, .ended: return false
        default: return true
        }
    }

    /// `tools`: Gemini function declarations (`{"name", "description", "parameters"}`).
    func start(apiKey: String, preferredModel: String?, voice: String?, instruction: String, tools: [[String: Any]] = []) async {
        guard !active else { return }
        phase = .connecting
        lines = []
        notice = nil
        muted = false
        pendingEvents = []

        guard await AVAudioApplication.requestRecordPermission() else {
            return end(GeminiError.microphone.localizedDescription)
        }
        do {
            let model: String
            if let preferredModel, !preferredModel.isEmpty {
                model = preferredModel
            } else {
                guard let first = try await Gemini.liveModels(apiKey: apiKey).first else { throw GeminiError.noModel }
                model = first
            }
            self.model = model
            try startAudio()
            connect(apiKey: apiKey, model: model, voice: voice, instruction: instruction, tools: tools)
        } catch {
            end(error.localizedDescription)
        }
    }

    func stop() { end(nil) }

    /// Tells Gemini something happened that it should act on or tell the user about, once it
    /// has finished what it's saying. `shown` goes in the transcript.
    func inform(_ text: String, shown: String) {
        guard socket != nil else { return }
        lines.append(Line(who: .action, text: shown, closed: true))
        pendingEvents.append(text)
        flushEvents()
    }

    private func flushEvents() {
        guard phase == .listening, !pendingEvents.isEmpty else { return }
        let text = pendingEvents.joined(separator: "\n\n")
        pendingEvents = []
        sendJSON(["realtimeInput": ["text": "[A message from the app, not the user]\n\(text)"]])
    }

    /// Typed instead of spoken.
    func send(text: String) {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, socket != nil else { return }
        interruptPlayback()
        append(.you, text)
        close(.you)
        sendJSON(["realtimeInput": ["text": text]])
    }

    // MARK: Socket

    private func connect(apiKey: String, model: String, voice: String?, instruction: String, tools: [[String: Any]]) {
        var parts = URLComponents()
        parts.scheme = "wss"
        parts.host = Gemini.host
        parts.path = "/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent"
        parts.queryItems = [URLQueryItem(name: "key", value: apiKey)]
        let socket = URLSession.shared.webSocketTask(with: parts.url!)
        socket.maximumMessageSize = 16 << 20
        self.socket = socket
        pipe.socket = socket
        socket.resume()

        var generation: [String: Any] = ["responseModalities": ["AUDIO"]]
        if let voice, !voice.isEmpty {
            generation["speechConfig"] = ["voiceConfig": ["prebuiltVoiceConfig": ["voiceName": voice]]]
        }
        var setup: [String: Any] = [
            "model": "models/\(model)",
            "generationConfig": generation,
            "systemInstruction": ["parts": [["text": instruction]]],
            "inputAudioTranscription": [String: Any](),
            "outputAudioTranscription": [String: Any](),
            // Past the Live API's usual 15 minutes of audio, the oldest turns are dropped.
            "contextWindowCompression": ["slidingWindow": [String: Any]()],
        ]
        if !tools.isEmpty { setup["tools"] = [["functionDeclarations": tools]] }
        sendJSON(["setup": setup])
        receive(socket)
    }

    private func receive(_ socket: URLSessionWebSocketTask) {
        socket.receive { [weak self] result in
            Task { @MainActor in
                guard let self, self.socket === socket else { return }
                switch result {
                case .success(let message):
                    let data: Data
                    switch message {
                    case .data(let d): data = d
                    case .string(let s): data = Data(s.utf8)
                    @unknown default: data = Data()
                    }
                    self.handle(data)
                    self.receive(socket)
                case .failure(let error):
                    let reason = socket.closeReason.map { String(decoding: $0, as: UTF8.self) } ?? ""
                    self.end(reason.isEmpty
                        ? (self.phase == .connecting ? error.localizedDescription : GeminiError.closed("").localizedDescription)
                        : GeminiError.closed(reason).localizedDescription)
                }
            }
        }
    }

    private func handle(_ data: Data) {
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
        if json["setupComplete"] != nil {
            phase = .listening
            pipe.ready = true
            flushEvents()
        }
        if let call = json["toolCall"] as? [String: Any], let calls = call["functionCalls"] as? [[String: Any]] {
            Task { await runTools(calls) }
        }
        if let away = json["goAway"] as? [String: Any] {
            notice = "Gemini will end this conversation soon\((away["timeLeft"] as? String).map { " (\($0))" } ?? "")."
        }
        guard let content = json["serverContent"] as? [String: Any] else { return }
        if let t = (content["inputTranscription"] as? [String: Any])?["text"] as? String {
            append(.you, t)
        }
        if let t = (content["outputTranscription"] as? [String: Any])?["text"] as? String {
            close(.you)
            append(.gemini, t)
        }
        if content["interrupted"] as? Bool == true {
            interruptPlayback()
            close(.gemini)
        }
        if let turn = content["modelTurn"] as? [String: Any], let parts = turn["parts"] as? [[String: Any]] {
            for part in parts {
                guard let inline = part["inlineData"] as? [String: Any],
                      let b64 = inline["data"] as? String,
                      let pcm = Data(base64Encoded: b64)
                else { continue }
                play(pcm, rate: Self.rate(inline["mimeType"] as? String) ?? 24000)
            }
        }
        if content["turnComplete"] as? Bool == true {
            close(.you)
            close(.gemini)
            if queued == 0 { flushEvents() }
        }
    }

    private func runTools(_ calls: [[String: Any]]) async {
        var responses: [[String: Any]] = []
        for call in calls {
            let name = call["name"] as? String ?? ""
            let args = call["args"] as? [String: Any] ?? [:]
            let result = await onToolCall?(name, args) ?? ["error": "No such function."]
            var response: [String: Any] = ["name": name, "response": result]
            if let id = call["id"] { response["id"] = id }
            responses.append(response)
        }
        sendJSON(["toolResponse": ["functionResponses": responses]])
    }

    /// A line in the transcript for something the app did.
    func note(_ text: String) {
        lines.append(Line(who: .action, text: text, closed: true))
    }

    private static func rate(_ mime: String?) -> Double? {
        guard let mime, let r = mime.range(of: "rate=") else { return nil }
        return Double(mime[r.upperBound...].prefix { $0.isNumber })
    }

    private func sendJSON(_ object: [String: Any]) {
        guard let socket, let data = try? JSONSerialization.data(withJSONObject: object) else { return }
        socket.send(.string(String(decoding: data, as: UTF8.self))) { _ in }
    }

    // MARK: Transcript

    private func append(_ who: Line.Who, _ text: String) {
        guard !text.isEmpty else { return }
        if let i = lines.indices.last, lines[i].who == who, !lines[i].closed {
            lines[i].text += text
        } else {
            lines.append(Line(who: who, text: text.trimmingCharacters(in: .whitespaces)))
        }
    }

    private func close(_ who: Line.Who) {
        if let i = lines.lastIndex(where: { $0.who == who }) { lines[i].closed = true }
    }

    // MARK: Audio

    private func startAudio() throws {
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.playAndRecord, mode: .voiceChat, options: [.defaultToSpeaker, .allowBluetoothHFP])
        try session.setActive(true)

        let input = engine.inputNode
        try input.setVoiceProcessingEnabled(true)
        engine.attach(player)
        engine.connect(player, to: engine.mainMixerNode, format: Self.outFormat)
        installTap()
        engine.prepare()
        try engine.start()
        player.play()

        let center = NotificationCenter.default
        observers.append(center.addObserver(forName: .AVAudioEngineConfigurationChange, object: engine, queue: .main) { [weak self] _ in
            // A route change (AirPods in or out) stops the engine; start it again on the new route.
            Task { @MainActor in self?.restartAudio() }
        })
        observers.append(center.addObserver(forName: AVAudioSession.interruptionNotification, object: session, queue: .main) { [weak self] note in
            let type = (note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt).flatMap(AVAudioSession.InterruptionType.init)
            if type == .began { Task { @MainActor in self?.end("Another app took the audio (a call?).") } }
        })
    }

    private func installTap() {
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        pipe.prepare(from: format)
        input.removeTap(onBus: 0)
        input.installTap(onBus: 0, bufferSize: 2048, format: format) { [pipe, weak self] buffer, _ in
            let level = pipe.push(buffer)
            Task { @MainActor in self?.level = level }
        }
    }

    private func restartAudio() {
        guard active else { return }
        engine.stop()
        installTap()
        engine.connect(player, to: engine.mainMixerNode, format: Self.outFormat)
        do {
            try engine.start()
            player.play()
        } catch {
            end("The audio stopped: \(error.localizedDescription)")
        }
    }

    private func play(_ pcm: Data, rate: Double) {
        let frames = pcm.count / 2
        guard frames > 0, let buffer = AVAudioPCMBuffer(pcmFormat: Self.outFormat, frameCapacity: AVAudioFrameCount(frames)) else { return }
        buffer.frameLength = AVAudioFrameCount(frames)
        let out = buffer.floatChannelData![0]
        pcm.withUnsafeBytes { raw in
            let samples = raw.bindMemory(to: Int16.self)
            for i in 0..<frames { out[i] = Float(Int16(littleEndian: samples[i])) / 32768 }
        }
        // The Live API speaks at 24 kHz; anything else would need a converter, so it's resampled
        // only in that unlikely case.
        let toPlay = rate == 24000 ? buffer : Self.resample(buffer, from: rate) ?? buffer
        queued += 1
        phase = .speaking
        let generation = playGeneration
        player.scheduleBuffer(toPlay) { [weak self] in
            Task { @MainActor in
                guard let self, self.playGeneration == generation else { return }
                self.queued -= 1
                if self.queued == 0, self.phase == .speaking {
                    self.phase = .listening
                    self.flushEvents()
                }
            }
        }
        if !player.isPlaying { player.play() }
    }

    private static func resample(_ buffer: AVAudioPCMBuffer, from rate: Double) -> AVAudioPCMBuffer? {
        guard let inFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: rate, channels: 1, interleaved: false),
              let source = AVAudioPCMBuffer(pcmFormat: inFormat, frameCapacity: buffer.frameLength),
              let converter = AVAudioConverter(from: inFormat, to: outFormat),
              let out = AVAudioPCMBuffer(pcmFormat: outFormat, frameCapacity: AVAudioFrameCount(Double(buffer.frameLength) * 24000 / rate) + 64)
        else { return nil }
        source.frameLength = buffer.frameLength
        memcpy(source.floatChannelData![0], buffer.floatChannelData![0], Int(buffer.frameLength) * 4)
        var fed = false
        converter.convert(to: out, error: nil) { _, status in
            if fed { status.pointee = .endOfStream; return nil }
            fed = true
            status.pointee = .haveData
            return source
        }
        return out
    }

    /// Whatever Gemini was saying stops now: the user spoke over it.
    private func interruptPlayback() {
        playGeneration += 1
        queued = 0
        player.stop()
        if engine.isRunning { player.play() }
        if phase == .speaking { phase = .listening }
    }

    // MARK: End

    private func end(_ reason: String?) {
        let wasActive = active
        pipe.ready = false
        pipe.socket = nil
        socket?.cancel(with: .normalClosure, reason: nil)
        socket = nil
        for o in observers { NotificationCenter.default.removeObserver(o) }
        observers = []
        if engine.isRunning || wasActive {
            engine.inputNode.removeTap(onBus: 0)
            player.stop()
            engine.stop()
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        }
        level = 0
        phase = .ended(reason)
    }
}

/// The microphone's side, run on the audio thread: downmix to mono, convert to 16 kHz 16-bit
/// PCM, and send it as `realtimeInput` audio.
private final class MicPipe: @unchecked Sendable {
    private let lock = NSLock()
    private var _socket: URLSessionWebSocketTask?
    private var _ready = false
    private var _muted = false
    private var converter: AVAudioConverter?
    private var mono: AVAudioFormat?
    private let target = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16000, channels: 1, interleaved: true)!

    var socket: URLSessionWebSocketTask? {
        get { lock.withLock { _socket } }
        set { lock.withLock { _socket = newValue } }
    }
    var ready: Bool {
        get { lock.withLock { _ready } }
        set { lock.withLock { _ready = newValue } }
    }
    var muted: Bool {
        get { lock.withLock { _muted } }
        set { lock.withLock { _muted = newValue } }
    }

    func prepare(from format: AVAudioFormat) {
        lock.withLock {
            let mono = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: format.sampleRate, channels: 1, interleaved: false)!
            self.mono = mono
            converter = AVAudioConverter(from: mono, to: target)
        }
    }

    /// Sends the buffer if the conversation is open and not muted. Returns its loudness.
    func push(_ buffer: AVAudioPCMBuffer) -> Float {
        let (socket, ready, muted, converter, mono) = lock.withLock { (_socket, _ready, _muted, self.converter, self.mono) }
        guard let channels = buffer.floatChannelData, buffer.frameLength > 0, let mono, let converter else { return 0 }
        let frames = Int(buffer.frameLength)

        // Voice processing can hand over several channels; the first is the processed voice.
        guard let single = AVAudioPCMBuffer(pcmFormat: mono, frameCapacity: buffer.frameLength) else { return 0 }
        single.frameLength = buffer.frameLength
        memcpy(single.floatChannelData![0], channels[0], frames * 4)

        var sum: Float = 0
        for i in 0..<frames { sum += channels[0][i] * channels[0][i] }
        let level = muted ? 0 : min(1, sqrt(sum / Float(frames)) * 8)

        guard ready, !muted, let socket else { return level }
        let capacity = AVAudioFrameCount(Double(frames) * 16000 / mono.sampleRate) + 64
        guard let out = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return level }
        var fed = false
        var error: NSError?
        converter.convert(to: out, error: &error) { _, status in
            if fed { status.pointee = .noDataNow; return nil }
            fed = true
            status.pointee = .haveData
            return single
        }
        guard error == nil, out.frameLength > 0, let samples = out.int16ChannelData else { return level }
        let pcm = Data(bytes: samples[0], count: Int(out.frameLength) * 2)
        let message = #"{"realtimeInput":{"audio":{"mimeType":"audio/pcm;rate=16000","data":""# + pcm.base64EncodedString() + #""}}}"#
        socket.send(.string(message)) { _ in }
        return level
    }
}
