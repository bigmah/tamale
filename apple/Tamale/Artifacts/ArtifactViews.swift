// Artifacts in the conversation: a card that shows the page inline at its own height, and the
// full-screen viewer with share/export (HTML, PDF) and Revise.

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
    @Query private var stored: [StoredArtifact]
    @State private var height: CGFloat = 160
    @State private var link: URL?

    init(block: ArtifactBlock, model: SessionModel, open: @escaping (ArtifactSource) -> Void) {
        self.block = block
        self.model = model
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
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(source == nil)

            if let source {
                Divider().overlay(Theme.border)
                ArtifactWebView(
                    html: source.html, scrolls: false,
                    onHeight: { height = min(max($0, 60), 520) },
                    onLink: { link = $0 })
                    .frame(height: height)
                    .overlay(alignment: .bottom) {
                        if height >= 520 {
                            Button("Open full screen") { open(source) }
                                .font(.footnote.weight(.medium))
                                .padding(.horizontal, 12)
                                .padding(.vertical, 6)
                                .background(.ultraThinMaterial, in: Capsule())
                                .padding(8)
                        }
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
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.border, lineWidth: 0.5))
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .onAppear(perform: request)
        .onChange(of: block.id) { request() }
        .onChange(of: model.connection) { request() }
        .confirmsLinks($link)
    }

    private func request() {
        guard let id = block.id, stored.isEmpty else { return }
        model.requestArtifact(id)
    }
}

/// Full screen: the page scrolls, with share/export and Revise.
struct ArtifactViewer: View {
    var source: ArtifactSource
    /// Puts the Revise prompt in the composer; nil where there's no session to send it to.
    var revise: ((String) -> Void)?
    /// What Revise says for an artifact from an earlier session, if not the usual prompt.
    var revisePrompt: String?
    @Environment(\.dismiss) private var dismiss
    @State private var webView: WKWebView?
    @State private var link: URL?
    @State private var exported: URL?
    @State private var exporting = false

    var body: some View {
        NavigationStack {
            ArtifactWebView(html: source.html, scrolls: true, onLink: { link = $0 }, onWebView: { webView = $0 })
                .ignoresSafeArea(edges: .bottom)
                .background(Color.white)
                .navigationTitle(source.title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Done") { dismiss() }
                    }
                    ToolbarItemGroup(placement: .primaryAction) {
                        Menu {
                            if let file = htmlFile() {
                                ShareLink(item: file) { Label("Share HTML", systemImage: "doc.richtext") }
                            }
                            Button("Export PDF", systemImage: "doc") { exportPDF() }
                            Button("Copy HTML", systemImage: "doc.on.doc") { UIPasteboard.general.string = source.html }
                        } label: {
                            if exporting { ProgressView() } else { Image(systemName: "square.and.arrow.up") }
                        }
                        if let revise {
                            Button("Revise", systemImage: "pencil.line") {
                                revise(revisePrompt ?? Prompts.revise(title: source.title))
                                dismiss()
                            }
                        } else {
                            Button("Copy revise prompt", systemImage: "pencil.line") {
                                UIPasteboard.general.string = revisePrompt ?? Prompts.revise(title: source.title)
                            }
                        }
                    }
                }
                .sheet(item: $exported) { file in
                    ShareSheet(items: [file])
                }
        }
        .confirmsLinks($link)
    }

    private var fileName: String {
        let slug = source.title.components(separatedBy: CharacterSet.alphanumerics.inverted).filter { !$0.isEmpty }.joined(separator: "-")
        return slug.isEmpty ? "artifact" : String(slug.prefix(60))
    }

    private func htmlFile() -> URL? {
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
