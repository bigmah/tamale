// Artifacts in the conversation: a card that shows the page inline at its own height, and the
// edge-to-edge viewer with share/export (HTML, PDF) and Revise.

import SwiftUI
import SwiftData
import TamaleKit
import WebKit

/// The page for an artifact: tamale's reply if it sent one, else the library's copy.
struct ArtifactSource: Identifiable, Hashable {
    var id: String { hash ?? title }
    var title: String
    var html: String
    var hash: String?
}

struct ArtifactCard: View {
    var block: ArtifactBlock
    var model: SessionModel
    var open: (ArtifactSource) -> Void
    /// Runs to the edges of the screen, past the conversation's side padding.
    var bleeds = false
    @Query private var stored: [StoredArtifact]
    @Environment(\.verticalSizeClass) private var verticalSize
    @State private var contentHeight: CGFloat = 120
    @State private var loaded = false
    @State private var link: URL?
    @State private var pageColor = Color.white

    init(block: ArtifactBlock, model: SessionModel, bleeds: Bool = false, open: @escaping (ArtifactSource) -> Void) {
        self.block = block
        self.model = model
        self.bleeds = bleeds
        self.open = open
        let hash = block.hash ?? ""
        _stored = Query(filter: #Predicate<StoredArtifact> { $0.contentHash == hash })
    }

    private var source: ArtifactSource? {
        if let id = block.id, let page = model.pages[id], let html = page.html {
            return ArtifactSource(title: block.title, html: html, hash: page.hash ?? block.hash)
        }
        if let saved = stored.first {
            return ArtifactSource(title: block.title, html: saved.html, hash: saved.contentHash)
        }
        return nil
    }

    private var gone: Bool {
        guard let id = block.id, let page = model.pages[id] else { return false }
        return page.html == nil && stored.isEmpty
    }

    /// The most of a page shown inline. Less on its side, where the screen is short.
    private var tallest: CGFloat { verticalSize == .compact ? 280 : 520 }
    private var clipped: Bool { contentHeight > tallest }
    private var shape: RoundedRectangle { RoundedRectangle(cornerRadius: bleeds ? 0 : 10) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                if let source { open(source) }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: "sparkle")
                        .foregroundStyle(Theme.violet)
                        .symbolEffect(.pulse, isActive: !block.done)
                    Text(block.done ? block.title : "Building \(block.title)…")
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(Theme.bright)
                        .lineLimit(1)
                    Spacer()
                    if source != nil {
                        Image(systemName: "arrow.up.left.and.arrow.down.right")
                            .font(.caption)
                            .foregroundStyle(Theme.dim)
                    }
                }
                .padding(.horizontal, bleeds ? Conversation.sidePadding : 12)
                .frame(minHeight: 44)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(source == nil)

            if let source {
                ArtifactWebView(
                    html: source.html, scrolls: false,
                    onHeight: { contentHeight = max($0, 44) },
                    onLink: { link = $0 },
                    onLoaded: { withAnimation(.easeOut(duration: 0.2)) { loaded = true } },
                    onPageColor: { pageColor = Color(uiColor: $0) })
                    .frame(height: min(contentHeight, tallest))
                    .overlay {
                        // Not a white flash while a dark page loads.
                        if !loaded { Theme.surface.allowsHitTesting(false) }
                    }
                    .overlay(alignment: .bottom) {
                        if clipped { more(source) }
                    }
                    .task {
                        // A script that never finishes loading still leaves a page to see.
                        try? await Task.sleep(for: .seconds(3))
                        loaded = true
                    }
            } else if gone {
                Text("tamale no longer has this artifact (it restarted). Ask for it again, or find it in the gallery.")
                    .font(.footnote)
                    .foregroundStyle(Theme.dim)
                    .padding(12)
            } else if block.done {
                ProgressView()
                    .frame(maxWidth: .infinity)
                    .padding(16)
            }
        }
        .background(Theme.surface, in: shape)
        .overlay {
            if bleeds {
                VStack {
                    Theme.border.frame(height: 0.5)
                    Spacer()
                    Theme.border.frame(height: 0.5)
                }
                .allowsHitTesting(false)
            } else {
                shape.stroke(Theme.border, lineWidth: 0.5)
            }
        }
        .clipShape(shape)
        .padding(.horizontal, bleeds ? -Conversation.sidePadding : 0)
        .onAppear(perform: request)
        .onChange(of: block.id) { request() }
        .onChange(of: model.connection) { request() }
        .confirmsLinks($link)
    }

    /// The page goes on past the card: says so, and goes there.
    private func more(_ source: ArtifactSource) -> some View {
        Button {
            open(source)
        } label: {
            Label("Open full screen", systemImage: "arrow.up.left.and.arrow.down.right")
                .font(.footnote.weight(.semibold))
                .padding(.horizontal, 14)
                .frame(height: 36)
                .background(.regularMaterial, in: Capsule())
                .padding(.bottom, 12)
                .frame(maxWidth: .infinity)
                .padding(.top, 48)
                .background(
                    LinearGradient(colors: [pageColor.opacity(0), pageColor], startPoint: .top, endPoint: .bottom)
                )
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private func request() {
        guard let id = block.id, stored.isEmpty else { return }
        model.requestArtifact(id)
    }
}

/// Full screen, all of it: the page runs edge to edge under a small bar of controls that gets
/// out of the way as you read. Scroll back up or tap the handle to bring it back; pull down from
/// the top to close.
struct ArtifactViewer: View {
    var source: ArtifactSource
    /// Puts the Revise prompt in the composer; nil where there's no session to send it to.
    var revise: ((String) -> Void)?
    /// What Revise says for an artifact from an earlier session, if not the usual prompt.
    var revisePrompt: String?
    @Environment(\.dismiss) private var dismiss
    @Environment(\.accessibilityVoiceOverEnabled) private var voiceOver
    @State private var webView: WKWebView?
    @State private var link: URL?
    @State private var htmlFile: URL?
    @State private var exported: URL?
    @State private var exporting = false
    @State private var loaded = false
    @State private var controls = true
    @State private var pageColor: Color?
    @State private var canTalk = false
    @State private var talking: VoiceTopic?

    /// Height of the bar and the gap under it, kept clear below the end of the page.
    private static let barSpace: CGFloat = 68

    var body: some View {
        GeometryReader { geometry in
            ZStack {
                Theme.background.ignoresSafeArea()
                ArtifactWebView(
                    html: source.html, scrolls: true,
                    onLink: { link = $0 },
                    onWebView: { view in DispatchQueue.main.async { webView = view } },
                    onLoaded: { withAnimation(.easeOut(duration: 0.2)) { loaded = true } },
                    onPageColor: { pageColor = Color(uiColor: $0) },
                    onScroll: scrolled,
                    bottomInset: Self.barSpace)
                    .ignoresSafeArea()
                    .opacity(loaded ? 1 : 0)

                if !loaded {
                    ProgressView()
                }

                // Around the Dynamic Island the page keeps its color, rather than text sliding
                // through the gap above a sticky header.
                VStack(spacing: 0) {
                    (pageColor ?? Theme.background)
                        .frame(height: geometry.safeAreaInsets.top)
                        .ignoresSafeArea(edges: .top)
                        .opacity(loaded ? 1 : 0)
                    Spacer()
                }
                .allowsHitTesting(false)

                VStack(spacing: 0) {
                    Spacer()
                    if controls || voiceOver {
                        bar.transition(.move(edge: .bottom).combined(with: .opacity))
                    } else {
                        handle.transition(.opacity)
                    }
                }
                // A form in the page needs the space the keyboard leaves.
                .ignoresSafeArea(.keyboard)
            }
        }
        .statusBarHidden()
        .persistentSystemOverlays(controls ? .automatic : .hidden)
        .task {
            canTalk = Gemini.apiKey != nil
            htmlFile = writeHTMLFile()
            // A script that never finishes loading still leaves a page to see.
            try? await Task.sleep(for: .seconds(3))
            withAnimation { loaded = true }
        }
        .sheet(item: $exported) { file in
            ShareSheet(items: [file])
        }
        .sheet(item: $talking) { topic in
            VoiceChatView(title: source.title, html: source.html, pageText: topic.text)
                .presentationDetents([.medium, .large])
                .presentationBackgroundInteraction(.enabled(upThrough: .medium))
        }
        .confirmsLinks($link)
    }

    /// Opens the voice conversation with the page's text as it's shown (scripts may have drawn
    /// some of it), or its HTML stripped of tags if the page won't say.
    private func talk() {
        let fallback = HTMLText.plain(source.html)
        guard let webView else {
            talking = VoiceTopic(text: fallback)
            return
        }
        webView.evaluateJavaScript("document.body ? document.body.innerText : ''") { result, _ in
            let text = (result as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            talking = VoiceTopic(text: text.isEmpty ? fallback : text)
        }
    }

    private func scrolled(_ event: ArtifactScroll) {
        switch event {
        case .pulledPastTop:
            dismiss()
        case .hideControls, .showControls:
            let show = event == .showControls
            if show != controls { withAnimation(.snappy(duration: 0.25)) { controls = show } }
        }
    }

    private var bar: some View {
        HStack(spacing: 0) {
            barButton("Done", systemImage: "xmark") { dismiss() }
            // The status bar is hidden, so this is the way back to the top.
            Button {
                guard let scroll = webView?.scrollView else { return }
                scroll.setContentOffset(CGPoint(x: 0, y: -scroll.adjustedContentInset.top), animated: true)
            } label: {
                Text(source.title)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.bright)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, minHeight: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityHint("Scrolls to the top")
            .accessibilityIdentifier("artifact-title")
            if canTalk {
                barButton("Talk with Gemini", systemImage: "waveform") { talk() }
            }
            shareMenu
            if let revise {
                barButton("Revise", systemImage: "pencil.line") {
                    revise(revisePrompt ?? Prompts.revise(title: source.title))
                    dismiss()
                }
            }
        }
        .padding(.horizontal, 4)
        .frame(height: 52)
        .background(.regularMaterial, in: Capsule())
        .overlay(Capsule().stroke(Color.white.opacity(0.12), lineWidth: 0.5))
        .shadow(color: .black.opacity(0.25), radius: 12, y: 4)
        .frame(maxWidth: 560)
        .padding(.horizontal, 12)
        .padding(.bottom, 8)
    }

    /// All that's left of the bar while reading.
    private var handle: some View {
        Button {
            withAnimation(.snappy(duration: 0.25)) { controls = true }
        } label: {
            Capsule()
                .fill(.regularMaterial)
                .overlay(Capsule().stroke(Color.white.opacity(0.25), lineWidth: 0.5))
                .shadow(color: .black.opacity(0.3), radius: 3)
                .frame(width: 44, height: 6)
                .frame(width: 96, height: 44)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Show controls")
    }

    private func barButton(_ title: String, systemImage: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.body.weight(.medium))
                .foregroundStyle(Theme.bright)
                .frame(width: 44, height: 44)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
    }

    private var shareMenu: some View {
        Menu {
            if let htmlFile {
                ShareLink(item: htmlFile) { Label("Share HTML", systemImage: "doc.richtext") }
            }
            Button("Export PDF", systemImage: "doc") { exportPDF() }
            Button("Copy HTML", systemImage: "doc.on.doc") { UIPasteboard.general.string = source.html }
            if revise == nil {
                // No session here to revise it in; the prompt can go to one.
                Button("Copy revise prompt", systemImage: "pencil.line") {
                    UIPasteboard.general.string = revisePrompt ?? Prompts.revise(title: source.title)
                }
            }
        } label: {
            Group {
                if exporting {
                    ProgressView()
                } else {
                    Image(systemName: "square.and.arrow.up")
                        .font(.body.weight(.medium))
                        .foregroundStyle(Theme.bright)
                }
            }
            .frame(width: 44, height: 44)
            .contentShape(Rectangle())
        }
        .accessibilityLabel("Share")
    }

    private var fileName: String {
        let slug = source.title.components(separatedBy: CharacterSet.alphanumerics.inverted).filter { !$0.isEmpty }.joined(separator: "-")
        return slug.isEmpty ? "artifact" : String(slug.prefix(60))
    }

    private func writeHTMLFile() -> URL? {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("\(fileName).html")
        try? source.html.write(to: url, atomically: true, encoding: .utf8)
        return url
    }

    private func exportPDF() {
        guard let webView else { return }
        exporting = true
        let config = WKPDFConfiguration()
        webView.createPDF(configuration: config) { result in
            Task { @MainActor in
                exporting = false
                guard case .success(let data) = result else { return }
                let url = FileManager.default.temporaryDirectory.appendingPathComponent("\(fileName).pdf")
                try? data.write(to: url)
                exported = url
            }
        }
    }
}

struct VoiceTopic: Identifiable {
    let id = UUID()
    var text: String
}

extension URL: @retroactive Identifiable {
    public var id: String { absoluteString }
}

struct ShareSheet: UIViewControllerRepresentable {
    var items: [Any]

    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }

    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}
