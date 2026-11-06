// Talking to Gemini about the artifact on screen: the conversation starts when the sheet
// opens and ends when it closes. What both sides said shows as it's said.

import SwiftUI

struct VoiceChatView: View {
    var title: String
    var html: String
    /// The page's text as rendered, which is what Gemini is told about.
    var pageText: String

    @Environment(\.dismiss) private var dismiss
    @AppStorage("geminiModel") private var preferredModel = ""
    @AppStorage("geminiVoice") private var voice = ""
    @State private var live = GeminiLive()
    @State private var typed = ""
    @State private var typing = false

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                transcript
                controls
            }
            .background(Theme.background)
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .task { await begin() }
        .onDisappear { live.stop() }
        .interactiveDismissDisabled(live.active)
    }

    private func begin() async {
        guard let key = Gemini.apiKey else { return }
        await live.start(apiKey: key, preferredModel: preferredModel, voice: voice, title: title, pageText: pageText, html: html)
    }

    private var transcript: some View {
        ScrollViewReader { scroll in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    if live.lines.isEmpty {
                        Text(hint)
                            .font(.callout)
                            .foregroundStyle(Theme.dim)
                            .frame(maxWidth: .infinity)
                            .multilineTextAlignment(.center)
                            .padding(.top, 40)
                    }
                    ForEach(live.lines) { line in
                        VStack(alignment: line.who == .you ? .trailing : .leading, spacing: 3) {
                            Text(line.who == .you ? "You" : "Gemini")
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(line.who == .you ? Theme.accent : Theme.violet)
                            Text(line.text)
                                .font(.body)
                                .foregroundStyle(Theme.foreground)
                                .multilineTextAlignment(line.who == .you ? .trailing : .leading)
                                .textSelection(.enabled)
                        }
                        .frame(maxWidth: .infinity, alignment: line.who == .you ? .trailing : .leading)
                        .id(line.id)
                    }
                    if let notice = live.notice {
                        Text(notice).font(.footnote).foregroundStyle(Theme.warning)
                    }
                    if case .ended(let reason?) = live.phase {
                        Text(reason).font(.footnote).foregroundStyle(Theme.error).textSelection(.enabled)
                    }
                }
                .padding(16)
            }
            .onChange(of: live.lines.last?.text) {
                if let id = live.lines.last?.id { withAnimation { scroll.scrollTo(id, anchor: .bottom) } }
            }
        }
    }

    private var hint: String {
        if Gemini.apiKey == nil { return "Add a Gemini API key in Keys to talk about this page." }
        switch live.phase {
        case .idle, .connecting: return "Connecting to Gemini…"
        case .listening: return "Ask anything about “\(title)”."
        case .speaking: return ""
        case .ended: return ""
        }
    }

    private var controls: some View {
        VStack(spacing: 12) {
            if typing {
                HStack {
                    TextField("Type instead", text: $typed)
                        .textFieldStyle(.roundedBorder)
                        .submitLabel(.send)
                        .onSubmit(sendTyped)
                    Button("Send", systemImage: "arrow.up.circle.fill", action: sendTyped)
                        .labelStyle(.iconOnly)
                        .font(.title2)
                        .disabled(typed.trimmingCharacters(in: .whitespaces).isEmpty || !live.active)
                }
            }
            HStack(spacing: 28) {
                circle(typing ? "keyboard.chevron.compact.down" : "keyboard", label: "Type", tint: Theme.surface3) {
                    typing.toggle()
                }
                .disabled(!live.active)

                orb

                if live.active {
                    circle(live.muted ? "mic.slash.fill" : "mic.fill", label: live.muted ? "Unmute" : "Mute",
                           tint: live.muted ? Theme.warning : Theme.surface3) {
                        live.muted.toggle()
                    }
                } else {
                    circle("arrow.clockwise", label: "Start again", tint: Theme.surface3) {
                        Task { await begin() }
                    }
                    .disabled(Gemini.apiKey == nil)
                }
            }
            Text(status)
                .font(.footnote)
                .foregroundStyle(Theme.dim)
            if !live.model.isEmpty {
                Text(live.model).font(.caption2.monospaced()).foregroundStyle(Theme.faint)
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 12)
        .padding(.bottom, 8)
        .background(Theme.surface)
    }

    private var status: String {
        switch live.phase {
        case .idle, .connecting: return "Connecting…"
        case .listening: return live.muted ? "Muted" : "Listening"
        case .speaking: return "Gemini is speaking. Talk to interrupt."
        case .ended: return "Ended"
        }
    }

    /// Pulses with your voice, glows while Gemini speaks; tap to end.
    private var orb: some View {
        let speaking = live.phase == .speaking
        let scale = 1 + CGFloat(speaking ? 0.12 : live.level) * 0.35
        return Button {
            if live.active { live.stop() } else { Task { await begin() } }
        } label: {
            ZStack {
                Circle()
                    .fill(speaking ? Theme.violet : Theme.accent)
                    .opacity(0.25)
                    .scaleEffect(scale)
                    .animation(.easeOut(duration: 0.12), value: scale)
                Circle()
                    .fill(live.active ? (speaking ? Theme.violet : Theme.accent) : Theme.surface3)
                    .frame(width: 64, height: 64)
                Group {
                    if live.phase == .connecting {
                        ProgressView().tint(.white)
                    } else {
                        Image(systemName: live.active ? "phone.down.fill" : "waveform")
                            .font(.title2)
                            .foregroundStyle(.white)
                    }
                }
            }
            .frame(width: 88, height: 88)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(live.active ? "End conversation" : "Start conversation")
        .disabled(Gemini.apiKey == nil)
    }

    private func circle(_ symbol: String, label: String, tint: Color, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.title3)
                .foregroundStyle(Theme.bright)
                .frame(width: 52, height: 52)
                .background(tint, in: Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
    }

    private func sendTyped() {
        live.send(text: typed)
        typed = ""
    }
}
