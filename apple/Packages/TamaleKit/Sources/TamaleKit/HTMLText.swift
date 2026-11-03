// For tamales from before `md` on entries: turns the page's HTML for an assistant or note
// entry back into markdown the app can render, with artifact markers where the page's
// `<div class="artifact">` placeholders are.

import Foundation

public enum HTMLText {
    public static func markdown(from html: String) -> (markdown: String, artifacts: [ArtifactBlock]) {
        var artifacts: [ArtifactBlock] = []
        var s = html

        // Artifacts, finished and still building.
        s = replace(s, #"<div class="artifact" data-src="/artifact/([A-Za-z0-9]+)" data-title="([^"]*)"></div>"#) { g in
            artifacts.append(ArtifactBlock(id: g[1], title: decodeEntities(g[2]), done: true, hash: nil))
            return "\n\n\u{1}\(artifacts.count - 1)\u{1}\n\n"
        }
        s = replace(s, #"<div class="artifact building">.*?<span class="artifact-title">Building (.*?)…</span>.*?</div></div>"#) { g in
            artifacts.append(ArtifactBlock(id: nil, title: decodeEntities(g[1]), done: false, hash: nil))
            return "\n\n\u{1}\(artifacts.count - 1)\u{1}\n\n"
        }
        // Code blocks: the language label, then the highlighted lines.
        s = replace(s, #"<div class="codeblock">(?:<div class="codeblock-lang">(.*?)</div>)?<pre><code>(.*?)</code></pre></div>"#) { g in
            "\n\n```\(decodeEntities(g[1]))\n\(decodeEntities(stripTags(g[2])))\n```\n\n"
        }
        s = replace(s, #"<h([1-6])>(.*?)</h[1-6]>"#) { g in
            "\n\n" + String(repeating: "#", count: Int(g[1]) ?? 1) + " " + g[2] + "\n\n"
        }
        s = replace(s, #"<li>"#) { _ in "\n- " }
        s = replace(s, #"<input type="checkbox" checked disabled> "#) { _ in "[x] " }
        s = replace(s, #"<input type="checkbox" disabled> "#) { _ in "[ ] " }
        s = replace(s, #"</?(ul|ol)( start="\d+")?>"#) { _ in "\n\n" }
        s = replace(s, #"<blockquote[^>]*>"#) { _ in "\n\n> " }
        s = replace(s, #"<div class="callout-title">(.*?)</div>"#) { g in "**\(g[1])** " }
        s = replace(s, #"</?p>|</blockquote>|<hr>|</li>|<div class="table">|</div>"#) { g in g[0] == "<hr>" ? "\n\n---\n\n" : "\n\n" }
        s = replace(s, #"<br>"#) { _ in "  \n" }
        s = replace(s, #"</?strong>"#) { _ in "**" }
        s = replace(s, #"</?em>"#) { _ in "*" }
        s = replace(s, #"</?del>"#) { _ in "~~" }
        s = replace(s, #"<code[^>]*>(.*?)</code>"#) { g in "`\(g[1])`" }
        s = replace(s, #"<a href="([^"]*)"[^>]*>(.*?)</a>"#) { g in "[\(g[2])](\(g[1]))" }
        // Tables: cells separated by bars, one row per line.
        s = replace(s, #"</t[hd]><t[hd][^>]*>"#) { _ in " | " }
        s = replace(s, #"<tr>"#) { _ in "\n" }
        s = stripTags(s)
        s = decodeEntities(s)
        s = replace(s, #"\x{1}(\d+)\x{1}"#) { g in ArtifactMarker.line(Int(g[1]) ?? 0) }
        s = replace(s, #"\n{3,}"#) { _ in "\n\n" }
        return (s.trimmingCharacters(in: .whitespacesAndNewlines), artifacts)
    }

    static func stripTags(_ s: String) -> String {
        replace(s, #"<[^>]+>"#) { _ in "" }
    }

    public static func decodeEntities(_ s: String) -> String {
        guard s.contains("&") else { return s }
        var out = s
        for (entity, char) in [("&lt;", "<"), ("&gt;", ">"), ("&quot;", "\""), ("&#39;", "'"), ("&nbsp;", " ")] {
            out = out.replacingOccurrences(of: entity, with: char)
        }
        return out.replacingOccurrences(of: "&amp;", with: "&")
    }

    /// Replaces each match of `pattern` (dot matches newlines) with `with(groups)`.
    static func replace(_ s: String, _ pattern: String, with: ([String]) -> String) -> String {
        guard let regex = try? NSRegularExpression(pattern: pattern, options: [.dotMatchesLineSeparators]) else { return s }
        let ns = s as NSString
        var out = ""
        var last = 0
        for match in regex.matches(in: s, range: NSRange(location: 0, length: ns.length)) {
            out += ns.substring(with: NSRange(location: last, length: match.range.location - last))
            let groups = (0..<match.numberOfRanges).map { i -> String in
                let r = match.range(at: i)
                return r.location == NSNotFound ? "" : ns.substring(with: r)
            }
            out += with(groups)
            last = match.range.location + match.range.length
        }
        out += ns.substring(from: last)
        return out
    }
}
