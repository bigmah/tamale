// Artifacts are model-written HTML with scripts. Like the web UI's sandboxed iframes, each
// one runs where it can't reach the app: its own WKWebView with a non-persistent data store,
// no script message handlers, a nil base URL (an opaque origin), and a CSP with the source
// lists of tamale's ARTIFACT_CSP (src/web.rs). Every navigation after the first load is
// cancelled; link taps go to Safari after asking.

import SwiftUI
import UIKit
import WebKit

enum ArtifactPage {
    /// tamale's ARTIFACT_CSP without `sandbox` and `frame-ancestors`, which a `<meta>` ignores.
    static let csp = "default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' https:; "
        + "style-src 'unsafe-inline' https:; img-src data: blob: https:; font-src data: https:; "
        + "media-src data: blob: https:; connect-src https:"

    /// `html` with the CSP `<meta>` first in its head.
    static func secured(_ html: String) -> String {
        let meta = "<meta http-equiv=\"Content-Security-Policy\" content=\"\(csp)\">"
        func after(_ tag: String) -> String.Index? {
            guard let open = html.range(of: tag, options: .caseInsensitive),
                  let close = html[open.lowerBound...].firstIndex(of: ">")
            else { return nil }
            return html.index(after: close)
        }
        if let at = after("<head>") ?? after("<head ") {
            return String(html[..<at]) + meta + String(html[at...])
        }
        if let at = after("<html>") ?? after("<html ") ?? (html.lowercased().hasPrefix("<!doctype") ? after("<!doctype") : nil) {
            return String(html[..<at]) + "<head>\(meta)</head>" + String(html[at...])
        }
        return "<head>\(meta)</head>" + html
    }

    static func makeWebView() -> WKWebView {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.userContentController = WKUserContentController()
        config.allowsInlineMediaPlayback = true
        config.defaultWebpagePreferences.allowsContentJavaScript = true
        config.preferences.javaScriptCanOpenWindowsAutomatically = false
        config.dataDetectorTypes = []
        let view = WKWebView(frame: CGRect(x: 0, y: 0, width: 320, height: 200), configuration: config)
        view.isOpaque = false
        view.backgroundColor = .clear
        view.scrollView.backgroundColor = .clear
        view.allowsLinkPreview = false
        if #available(iOS 16.4, *) { view.isInspectable = false }
        return view
    }
}

struct ArtifactWebView: UIViewRepresentable {
    var html: String
    /// Inline cards don't scroll; they grow to the page's height (reported here).
    var scrolls = true
    var onHeight: ((CGFloat) -> Void)?
    var onLink: ((URL) -> Void)?
    /// Hands the web view out, for PDF export.
    var onWebView: ((WKWebView) -> Void)?

    func makeUIView(context: Context) -> WKWebView {
        let view = ArtifactPage.makeWebView()
        view.navigationDelegate = context.coordinator
        view.uiDelegate = context.coordinator
        view.scrollView.isScrollEnabled = scrolls
        view.scrollView.bounces = scrolls
        context.coordinator.observe(view)
        onWebView?(view)
        return view
    }

    func updateUIView(_ view: WKWebView, context: Context) {
        context.coordinator.parent = self
        context.coordinator.load(html, in: view)
    }

    static func dismantleUIView(_ view: WKWebView, coordinator: Coordinator) {
        coordinator.observation = nil
        view.stopLoading()
    }

    func makeCoordinator() -> Coordinator { Coordinator(parent: self) }

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate, WKUIDelegate {
        var parent: ArtifactWebView
        var loaded: String?
        var allowNext = false
        var observation: NSKeyValueObservation?
        var lastHeight: CGFloat = 0

        init(parent: ArtifactWebView) { self.parent = parent }

        func load(_ html: String, in view: WKWebView) {
            guard loaded != html else { return }
            loaded = html
            allowNext = true
            view.loadHTMLString(ArtifactPage.secured(html), baseURL: nil)
        }

        func observe(_ view: WKWebView) {
            observation = view.scrollView.observe(\.contentSize, options: [.new]) { [weak self, weak view] _, _ in
                Task { @MainActor in
                    guard let self, let view else { return }
                    self.measure(view)
                }
            }
        }

        func measure(_ view: WKWebView) {
            guard parent.onHeight != nil else { return }
            view.evaluateJavaScript("Math.ceil(document.documentElement.scrollHeight)") { [weak self] value, _ in
                guard let self, let height = (value as? NSNumber).map({ CGFloat(truncating: $0) }), height > 0,
                      abs(height - self.lastHeight) > 1
                else { return }
                self.lastHeight = height
                self.parent.onHeight?(height)
            }
        }

        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                     decisionHandler: @escaping @MainActor (WKNavigationActionPolicy) -> Void) {
            // The document itself, and frames inside it that need nothing from us.
            if allowNext && action.targetFrame?.isMainFrame == true {
                allowNext = false
                return decisionHandler(.allow)
            }
            if action.targetFrame?.isMainFrame == false, action.request.url?.scheme == "about" {
                return decisionHandler(.allow)
            }
            if action.navigationType == .linkActivated, let url = action.request.url,
               ["http", "https", "mailto"].contains(url.scheme?.lowercased() ?? "") {
                parent.onLink?(url)
            }
            decisionHandler(.cancel)
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            measure(webView)
        }

        /// `window.open` and `target=_blank`: offered to Safari like other links, never a new view.
        func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                     for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
            if let url = action.request.url, ["http", "https"].contains(url.scheme?.lowercased() ?? "") {
                parent.onLink?(url)
            }
            return nil
        }
    }
}

extension View {
    /// Asks before sending an artifact's link to Safari.
    func confirmsLinks(_ url: Binding<URL?>) -> some View {
        modifier(ConfirmLinks(url: url))
    }
}

struct ConfirmLinks: ViewModifier {
    @Binding var url: URL?
    @Environment(\.openURL) private var openURL

    func body(content: Content) -> some View {
        content.confirmationDialog(
            "Open this link in Safari?",
            isPresented: Binding(get: { url != nil }, set: { if !$0 { url = nil } }),
            titleVisibility: .visible
        ) {
            Button("Open") {
                if let url { openURL(url) }
                url = nil
            }
            Button("Cancel", role: .cancel) { url = nil }
        } message: {
            Text(url?.absoluteString ?? "")
        }
    }
}
