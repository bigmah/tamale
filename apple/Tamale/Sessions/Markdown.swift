// Assistant markdown, rendered natively: a small block parser (headings, paragraphs, lists,
// quotes and callouts, fenced code, tables, rules, and tamale's artifact markers), with inline
// markup from Foundation's markdown support.

import SwiftUI
import TamaleKit

struct MDListItem: Hashable {
    var depth: Int
    /// The number of an ordered item.
    var number: Int?
    var checked: Bool?
    var text: String
}

indirect enum MDBlock: Hashable {
    case paragraph(String)
    case heading(Int, String)
    case code(lang: String, body: String)
    case list([MDListItem])
    case quote([MDBlock], callout: String?)
    case table(header: [String], rows: [[String]])
    case rule
    case artifact(Int)
}

enum Markdown {
    static func parse(_ text: String) -> [MDBlock] {
        let lines = text.replacingOccurrences(of: "\r\n", with: "\n").split(separator: "\n", omittingEmptySubsequences: false)
        var blocks: [MDBlock] = []
        var paragraph: [Substring] = []
        var i = 0

        func flush() {
            if !paragraph.isEmpty {
                blocks.append(.paragraph(paragraph.map { $0.trimmingCharacters(in: .whitespaces) }.joined(separator: " ")))
                paragraph = []
            }
        }

        while i < lines.count {
            let line = lines[i]
            let trimmed = line.trimmingCharacters(in: .whitespaces)

            if let n = ArtifactMarker.index(line) {
                flush()
                blocks.append(.artifact(n))
                i += 1
                continue
            }
            if let fence = fenceOpening(trimmed) {
                flush()
                let lang = String(trimmed.dropFirst(fence.count)).trimmingCharacters(in: .whitespaces)
                var body: [Substring] = []
                i += 1
                while i < lines.count {
                    let t = lines[i].trimmingCharacters(in: .whitespaces)
                    if t.hasPrefix(fence) && t.allSatisfy({ $0 == fence.first }) { i += 1; break }
                    body.append(lines[i])
                    i += 1
                }
                blocks.append(.code(lang: lang, body: body.joined(separator: "\n")))
                continue
            }
            if trimmed.isEmpty {
                flush()
                i += 1
                continue
            }
            if let (level, title) = heading(trimmed) {
                flush()
                blocks.append(.heading(level, title))
                i += 1
                continue
            }
            if isRule(trimmed) {
                flush()
                blocks.append(.rule)
                i += 1
                continue
            }
            if trimmed.hasPrefix(">") {
                flush()
                var inner: [String] = []
                while i < lines.count {
                    let t = lines[i].trimmingCharacters(in: .whitespaces)
                    guard t.hasPrefix(">") else { break }
                    var rest = t.dropFirst()
                    if rest.hasPrefix(" ") { rest = rest.dropFirst() }
                    inner.append(String(rest))
                    i += 1
                }
                var callout: String?
                if let first = inner.first, first.hasPrefix("[!"), let end = first.firstIndex(of: "]") {
                    callout = first[first.index(first.startIndex, offsetBy: 2)..<end].capitalized
                    inner.removeFirst()
                }
                blocks.append(.quote(parse(inner.joined(separator: "\n")), callout: callout))
                continue
            }
            if listItem(line) != nil {
                flush()
                var items: [MDListItem] = []
                while i < lines.count {
                    if var item = listItem(lines[i]) {
                        if item.text.hasPrefix("[ ] ") { item.checked = false; item.text.removeFirst(4) }
                        else if item.text.lowercased().hasPrefix("[x] ") { item.checked = true; item.text.removeFirst(4) }
                        items.append(item)
                        i += 1
                    } else if !lines[i].trimmingCharacters(in: .whitespaces).isEmpty,
                              lines[i].hasPrefix("  ") || lines[i].hasPrefix("\t"), !items.isEmpty {
                        // A continuation of the item above.
                        items[items.count - 1].text += " " + lines[i].trimmingCharacters(in: .whitespaces)
                        i += 1
                    } else if lines[i].trimmingCharacters(in: .whitespaces).isEmpty,
                              i + 1 < lines.count, listItem(lines[i + 1]) != nil {
                        i += 1  // A loose list.
                    } else {
                        break
                    }
                }
                blocks.append(.list(items))
                continue
            }
            if trimmed.contains("|"), i + 1 < lines.count, isTableSeparator(lines[i + 1]) {
                flush()
                let header = cells(trimmed)
                var rows: [[String]] = []
                i += 2
                while i < lines.count {
                    let t = lines[i].trimmingCharacters(in: .whitespaces)
                    guard t.contains("|"), !t.isEmpty else { break }
                    rows.append(cells(t))
                    i += 1
                }
                blocks.append(.table(header: header, rows: rows))
                continue
            }
            paragraph.append(line)
            i += 1
        }
        flush()
        return blocks
    }

    static func fenceOpening(_ line: String) -> String? {
        for fence in ["````", "```", "~~~"] where line.hasPrefix(fence) { return fence }
        return nil
    }

    static func heading(_ line: String) -> (Int, String)? {
        let hashes = line.prefix { $0 == "#" }.count
        guard (1...6).contains(hashes), line.dropFirst(hashes).first == " " else { return nil }
        var title = line.dropFirst(hashes).trimmingCharacters(in: .whitespaces)
        while title.hasSuffix("#") { title.removeLast() }
        return (hashes, title.trimmingCharacters(in: .whitespaces))
    }

    static func isRule(_ line: String) -> Bool {
        let chars = line.filter { $0 != " " }
        guard chars.count >= 3, let first = chars.first, "-*_".contains(first) else { return false }
        return chars.allSatisfy { $0 == first }
    }

    static func listItem(_ line: Substring) -> MDListItem? {
        let indent = line.prefix { $0 == " " || $0 == "\t" }.count
        let rest = line.dropFirst(indent)
        if let marker = rest.first, "-*+".contains(marker), rest.dropFirst().first == " " {
            return MDListItem(depth: indent / 2, number: nil, checked: nil, text: String(rest.dropFirst(2)))
        }
        let digits = rest.prefix { $0.isNumber }
        if !digits.isEmpty, digits.count <= 9 {
            let after = rest.dropFirst(digits.count)
            if let dot = after.first, dot == "." || dot == ")", after.dropFirst().first == " " {
                return MDListItem(depth: indent / 2, number: Int(digits), checked: nil, text: String(after.dropFirst(2)))
            }
        }
        return nil
    }

    static func isTableSeparator(_ line: Substring) -> Bool {
        let t = line.trimmingCharacters(in: .whitespaces)
        guard t.contains("-") else { return false }
        return t.allSatisfy { "|-: ".contains($0) }
    }

    static func cells(_ line: String) -> [String] {
        var t = line
        if t.hasPrefix("|") { t.removeFirst() }
        if t.hasSuffix("|") { t.removeLast() }
        return t.split(separator: "|", omittingEmptySubsequences: false).map { $0.trimmingCharacters(in: .whitespaces) }
    }

    /// Inline markup: emphasis, code, links, strikethrough.
    static func inline(_ text: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(
            allowsExtendedAttributes: false, interpretedSyntax: .inlineOnlyPreservingWhitespace, failurePolicy: .returnPartiallyParsedIfPossible)
        return (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
    }
}

/// Renders markdown, with `artifact(n)` drawing the card for each artifact marker.
struct MarkdownView<Artifact: View>: View {
    var blocks: [MDBlock]
    @ViewBuilder var artifact: (Int) -> Artifact

    init(_ text: String, @ViewBuilder artifact: @escaping (Int) -> Artifact) {
        blocks = Markdown.parse(text)
        self.artifact = artifact
    }

    init(blocks: [MDBlock], @ViewBuilder artifact: @escaping (Int) -> Artifact) {
        self.blocks = blocks
        self.artifact = artifact
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                view(for: block)
            }
        }
    }

    @ViewBuilder
    func view(for block: MDBlock) -> some View {
        switch block {
        case .paragraph(let text):
            Text(Markdown.inline(text))
                .fixedSize(horizontal: false, vertical: true)
        case .heading(let level, let text):
            Text(Markdown.inline(text))
                .font(level == 1 ? .title2.bold() : level == 2 ? .title3.bold() : .headline)
                .foregroundStyle(Theme.bright)
                .padding(.top, 4)
        case .code(let lang, let body):
            CodeBlock(lang: lang, code: body)
        case .list(let items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Group {
                            if let checked = item.checked {
                                Image(systemName: checked ? "checkmark.square.fill" : "square")
                                    .foregroundStyle(checked ? Theme.added : Theme.dim)
                            } else if let n = item.number {
                                Text("\(n).").monospacedDigit()
                            } else {
                                Text(item.depth == 0 ? "•" : "◦")
                            }
                        }
                        .foregroundStyle(Theme.dim)
                        Text(Markdown.inline(item.text))
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.leading, CGFloat(item.depth) * 16)
                }
            }
        case .quote(let inner, let callout):
            HStack(alignment: .top, spacing: 10) {
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(calloutColor(callout))
                    .frame(width: 3)
                VStack(alignment: .leading, spacing: 6) {
                    if let callout {
                        Text(callout).font(.subheadline.weight(.semibold)).foregroundStyle(calloutColor(callout))
                    }
                    MarkdownView<Artifact>(blocks: inner, artifact: artifact)
                }
                .foregroundStyle(Theme.dim)
            }
            .fixedSize(horizontal: false, vertical: true)
        case .table(let header, let rows):
            ScrollView(.horizontal, showsIndicators: false) {
                Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 6) {
                    GridRow {
                        ForEach(Array(header.enumerated()), id: \.offset) { _, cell in
                            Text(Markdown.inline(cell)).font(.subheadline.weight(.semibold))
                        }
                    }
                    Divider()
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                        GridRow {
                            ForEach(Array(row.enumerated()), id: \.offset) { _, cell in
                                Text(Markdown.inline(cell)).font(.subheadline)
                            }
                        }
                    }
                }
                .padding(10)
            }
            .background(Theme.surface2, in: RoundedRectangle(cornerRadius: 8))
        case .rule:
            Divider().overlay(Theme.border)
        case .artifact(let n):
            artifact(n)
        }
    }

    func calloutColor(_ callout: String?) -> Color {
        switch callout?.lowercased() {
        case "warning": Theme.warning
        case "caution": Theme.error
        case "tip": Theme.added
        case "important": Theme.violet
        case "note": Theme.accent
        default: Theme.border
        }
    }
}

struct CodeBlock: View {
    var lang: String
    var code: String

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(lang.split(separator: " ").first.map(String.init) ?? "")
                    .font(.caption.monospaced())
                    .foregroundStyle(Theme.faint)
                Spacer()
                Button {
                    UIPasteboard.general.string = code
                } label: {
                    Image(systemName: "doc.on.doc").font(.caption)
                }
                .buttonStyle(.plain)
                .foregroundStyle(Theme.faint)
            }
            .padding(.horizontal, 10)
            .padding(.top, 6)
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .font(.code)
                    .foregroundStyle(Theme.foreground)
                    .textSelection(.enabled)
                    .padding(10)
            }
        }
        .background(Theme.surface2, in: RoundedRectangle(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.border, lineWidth: 0.5))
    }
}
