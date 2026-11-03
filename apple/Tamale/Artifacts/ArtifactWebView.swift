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

    /// A page that doesn't size itself would otherwise be laid out 980px wide and shrunk to fit.
    static let viewport = "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"

    /// The bottom of the page's content. Not `documentElement.scrollHeight`, which is never less
    /// than the frame, so a card could grow to fit a page but never shrink to one.
    static let measureScript = """
        (() => {
          const b = document.body;
          if (!b) return 0;
          const margin = parseFloat(getComputedStyle(b).marginBottom) || 0;
          const bottoms = [...b.children].map((c) => c.getBoundingClientRect().bottom);
          return Math.ceil(Math.max(b.getBoundingClientRect().bottom + margin, ...bottoms) + scrollY);
        })()
        """

    /// `html` with the CSP `<meta>` first in its head, and a viewport if it has none.
    static func secured(_ html: String) -> String {
        var meta = "<meta http-equiv=\"Content-Security-Policy\" content=\"\(csp)\">"
        if html.range(of: #"<meta[^>]+name\s*=\s*["']?viewport"#, options: [.regularExpression, .caseInsensitive]) == nil {
            meta += viewport
        }
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
        // Opaque white, as in a browser: a page with no background of its own is written for
        // that, and its black text can't be read on the app's dark background.
        view.isOpaque = true
        view.backgroundColor = .white
        view.allowsLinkPreview = false
        if #available(iOS 16.4, *) { view.isInspectable = false }
        return view
    }
}

enum ArtifactScroll {
    case hideControls, showControls, pulledPastTop
}

struct ArtifactWebView: UIViewRepresentable {
    var html: String
    /// Inline cards don't scroll; they grow to the page's height (reported here).
    var scrolls = true
    var onHeight: ((CGFloat) -> Void)?
    var onLink: ((URL) -> Void)?
    /// Hands the web view out, for PDF export.
    var onWebView: ((WKWebView) -> Void)?
    /// The first load finished (or failed), so there's something to show.
    var onLoaded: (() -> Void)?
    /// The color the page wants around it: its `theme-color`, else its background.
    var onPageColor: ((UIColor) -> Void)?
    /// What the reader's finger is doing, for controls floating over a scrolling page.
    var onScroll: ((ArtifactScroll) -> Void)?
    /// Room below the end of the page, so floating controls can't cover its last line.
    var bottomInset: CGFloat = 0

    func makeUIView(context: Context) -> WKWebView {
        let view = ArtifactPage.makeWebView()
        view.navigationDelegate = context.coordinator
        view.uiDelegate = context.coordinator
        view.scrollView.isScrollEnabled = scrolls
        view.scrollView.bounces = scrolls
        if scrolls {
            // A page shorter than the screen can still be pulled down to close.
            view.scrollView.alwaysBounceVertical = true
            view.scrollView.contentInset.bottom = bottomInset
            view.scrollView.verticalScrollIndicatorInsets.bottom = bottomInset
            view.scrollView.delegate = context.coordinator
        }
        context.coordinator.observe(view)
        onWebView?(view)
        return view
    }

    func updateUIView(_ view: WKWebView, context: Context) {
        context.coordinator.parent = self
        context.coordinator.load(html, in: view)
        context.coordinator.widthMayHaveChanged(view)
    }

    static func dismantleUIView(_ view: WKWebView, coordinator: Coordinator) {
        coordinator.observations = []
        view.scrollView.delegate = nil
        view.stopLoading()
    }

    func makeCoordinator() -> Coordinator { Coordinator(parent: self) }

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate, WKUIDelegate, UIScrollViewDelegate {
        var parent: ArtifactWebView
        var loaded: String?
        var allowNext = false
        var observations: [NSKeyValueObservation] = []
        var lastHeight: CGFloat = 0
        var lastWidth: CGFloat = 0
        var lastOffset: CGFloat = 0
        /// How far the finger has gone in the current direction.
        var travel: CGFloat = 0
        /// How far past the top a pull has to go to close the page.
        static let pullToClose: CGFloat = 90
        /// How far a scroll has to go in one direction to hide or show the controls.
        static let chromeTravel: CGFloat = 16

        init(parent: ArtifactWebView) { self.parent = parent }

        func load(_ html: String, in view: WKWebView) {
            guard loaded != html else { return }
            loaded = html
            allowNext = true
            view.loadHTMLString(ArtifactPage.secured(html), baseURL: nil)
        }

        func observe(_ view: WKWebView) {
            observations = [
                view.scrollView.observe(\.contentSize, options: [.new]) { [weak self, weak view] _, _ in
                    Task { @MainActor in
                        guard let self, let view else { return }
                        self.measure(view)
                    }
                },
                view.observe(\.underPageBackgroundColor, options: [.new]) { [weak self, weak view] _, _ in
                    Task { @MainActor in
                        guard let self, let view else { return }
                        self.reportColor(view)
                    }
                },
                view.observe(\.themeColor, options: [.new]) { [weak self, weak view] _, _ in
                    Task { @MainActor in
                        guard let self, let view else { return }
                        self.reportColor(view)
                    }
                },
            ]
        }

        func reportColor(_ view: WKWebView) {
            if let color = view.themeColor ?? view.underPageBackgroundColor { parent.onPageColor?(color) }
        }

        /// A rotation reflows the page, which doesn't have to change its content size. Waits for
        /// the new layout.
        func widthMayHaveChanged(_ view: WKWebView) {
            guard parent.onHeight != nil, view.bounds.width != lastWidth else { return }
            lastWidth = view.bounds.width
            measureSoon(view)
        }

        /// Pages draw late too (charts, say), and tell us nothing when they do.
        func measureSoon(_ view: WKWebView) {
            for delay in [0.3, 1.2] {
                DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self, weak view] in
                    guard let self, let view else { return }
                    self.measure(view)
                }
            }
        }

        func measure(_ view: WKWebView) {
            guard parent.onHeight != nil else { return }
            view.evaluateJavaScript(ArtifactPage.measureScript) { [weak self] value, _ in
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
            measureSoon(webView)
            reportColor(webView)
            parent.onLoaded?()
        }

        func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
            parent.onLoaded?()
        }

        func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
            parent.onLoaded?()
        }

        // MARK: Scrolling

        func scrollViewDidScroll(_ scrollView: UIScrollView) {
            let y = scrollView.contentOffset.y + scrollView.adjustedContentInset.top
            defer { lastOffset = y }
            // Only the reader's scrolling: a rotation or a page's own layout moves this too.
            guard scrollView.isTracking || scrollView.isDecelerating else { return }
            let end = scrollView.contentSize.height + scrollView.adjustedContentInset.top
                + scrollView.adjustedContentInset.bottom - scrollView.bounds.height
            if y <= 4 || y >= end - 4 {
                travel = 0
                parent.onScroll?(.showControls)
                return
            }
            let step = y - lastOffset
            travel = travel * step < 0 ? step : travel + step
            if travel > Self.chromeTravel {
                parent.onScroll?(.hideControls)
            } else if travel < -Self.chromeTravel {
                parent.onScroll?(.showControls)
            }
        }

        func scrollViewWillEndDragging(_ scrollView: UIScrollView, withVelocity velocity: CGPoint,
                                       targetContentOffset: UnsafeMutablePointer<CGPoint>) {
            if scrollView.contentOffset.y + scrollView.adjustedContentInset.top < -Self.pullToClose {
                parent.onScroll?(.pulledPastTop)
            }
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
