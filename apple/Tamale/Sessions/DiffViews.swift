// The session's uncommitted changes, like the terminal's `/diff`: a tree of the changed files,
// and each file's diff with unchanged lines folded. Tap lines to select them and ask about them.

import SwiftUI
import TamaleKit
import UIKit

extension Theme {
    // The terminal viewer's colours (src/app/diff.rs).
    static let addedLine = Color(red: 18 / 255, green: 48 / 255, blue: 28 / 255)
    static let addedChange = Color(red: 36 / 255, green: 100 / 255, blue: 52 / 255)
    static let removedLine = Color(red: 58 / 255, green: 20 / 255, blue: 24 / 255)
    static let removedChange = Color(red: 122 / 255, green: 38 / 255, blue: 46 / 255)
    static let gapLine = Color(red: 24 / 255, green: 34 / 255, blue: 52 / 255)
    static let selectedLine = Color(red: 52 / 255, green: 64 / 255, blue: 112 / 255)

    static func status(_ letter: String) -> Color {
        switch letter {
        case "A", "U": added
        case "D": error
        case "R": violet
        default: warning
        }
    }
}

/// "+12 −4", as the terminal's footer writes it.
struct DiffCounts: View {
    var added: Int
    var removed: Int

    var body: some View {
        HStack(spacing: 6) {
            if added > 0 { Text("+\(added)").foregroundStyle(Theme.added) }
            if removed > 0 { Text("−\(removed)").foregroundStyle(Theme.error) }
        }
        .font(.caption.monospacedDigit())
    }
}

/// The changed files, opening onto each one's diff. `ask` gets a quote for the composer.
struct DiffScreen: View {
    var model: SessionModel
    var ask: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var collapsed: Set<String> = []
    @State private var reveals: [String: DiffReveal] = [:]
    @State private var opened: Int?
    /// Asked long enough ago that this tamale must not know about diffs.
    @State private var unanswered = false

    var body: some View {
        NavigationStack {
            content
                .background(Theme.background)
                .navigationTitle("Changes")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Done") { dismiss() }
                    }
                    ToolbarItem(placement: .primaryAction) {
                        if model.loadingDiff {
                            ProgressView().controlSize(.small)
                        } else {
                            Button("Reload", systemImage: "arrow.clockwise") { model.requestDiff() }
                                .disabled(!model.connection.isLive)
                        }
                    }
                }
                .navigationDestination(item: $opened) { index in
                    if let reply = model.diff {
                        FileDiffScreen(files: reply.files, index: index, reveals: $reveals) { quote in
                            ask(quote)
                            dismiss()
                        }
                    }
                }
        }
        .onAppear { model.requestDiff() }
        // A turn that just finished has probably changed something.
        .onChange(of: model.busy) { _, busy in
            if !busy { model.requestDiff() }
        }
        .task(id: "\(model.loadingDiff) \(model.connection.isLive)") {
            unanswered = false
            guard model.loadingDiff, model.connection.isLive else { return }
            try? await Task.sleep(for: .seconds(10))
            if !Task.isCancelled { unanswered = true }
        }
    }

    @ViewBuilder
    private var content: some View {
        if let error = model.diff?.error {
            ContentUnavailableView("No changes to show", systemImage: "plus.forwardslash.minus", description: Text(error))
        } else if let reply = model.diff {
            if reply.files.isEmpty {
                ContentUnavailableView("No changes", systemImage: "checkmark.circle",
                                       description: Text("Nothing differs from HEAD."))
            } else {
                fileList(reply)
            }
        } else if unanswered {
            ContentUnavailableView("Can't show changes", systemImage: "arrow.up.circle",
                                   description: Text("This session's tamale doesn't send diffs yet. Update tamale and restart the session."))
        } else if case .stopped(let why) = model.connection {
            ContentUnavailableView("Can't show changes", systemImage: "xmark.octagon", description: Text(why))
        } else {
            ProgressView("Loading changes…").frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func fileList(_ reply: DiffReply) -> some View {
        List {
            Section {
                ForEach(DiffTree.rows(reply.files, collapsed: collapsed)) { row in
                    if let index = row.file {
                        fileRow(reply.files[index], name: row.name, depth: row.depth) { opened = index }
                    } else if let dir = row.dir {
                        dirRow(dir, name: row.name, depth: row.depth)
                    }
                }
            } header: {
                HStack {
                    Text("\(reply.files.count) file\(reply.files.count == 1 ? "" : "s")")
                    DiffCounts(added: reply.added, removed: reply.removed)
                    Spacer()
                    Text(reply.label)
                }
                .font(.caption)
                .textCase(nil)
            }
            .listRowBackground(Theme.surface)
        }
        .scrollContentBackground(.hidden)
        .refreshable { model.requestDiff() }
    }

    private func dirRow(_ dir: String, name: String, depth: Int) -> some View {
        Button {
            withAnimation(.snappy) {
                if collapsed.remove(dir) == nil { collapsed.insert(dir) }
            }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.semibold))
                    .rotationEffect(.degrees(collapsed.contains(dir) ? 0 : 90))
                    .foregroundStyle(Theme.faint)
                Text(name).font(.footnote.monospaced()).foregroundStyle(Theme.dim).lineLimit(1).truncationMode(.head)
                Spacer(minLength: 0)
            }
            .padding(.leading, CGFloat(depth) * 14)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private func fileRow(_ file: DiffFile, name: String, depth: Int, open: @escaping () -> Void) -> some View {
        Button(action: open) {
            HStack(spacing: 8) {
                Text(file.status)
                    .font(.caption.monospaced().weight(.bold))
                    .foregroundStyle(Theme.status(file.status))
                Text(name)
                    .font(.footnote.monospaced())
                    .foregroundStyle(Theme.foreground)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 4)
                DiffCounts(added: file.added, removed: file.removed)
            }
            .padding(.leading, CGFloat(depth) * 14)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// One file's diff, with steps to the other files and between changes.
struct FileDiffScreen: View {
    var files: [DiffFile]
    @Binding var reveals: [String: DiffReveal]
    var ask: (String) -> Void
    /// Files in the order the tree lists them.
    private let order: [Int]
    @State private var position: Int
    @AppStorage("diffWrapsLines") private var wraps = true

    init(files: [DiffFile], index: Int, reveals: Binding<[String: DiffReveal]>, ask: @escaping (String) -> Void) {
        self.files = files
        _reveals = reveals
        self.ask = ask
        let order = DiffTree.order(files)
        self.order = order
        _position = State(initialValue: order.firstIndex(of: index) ?? 0)
    }

    private var file: DiffFile { files[order[position]] }

    var body: some View {
        FileDiffView(file: file, wraps: wraps, reveal: Binding(
            get: { reveals[file.path] ?? DiffReveal() },
            set: { reveals[file.path] = $0 }
        ), position: position, count: order.count, step: { position += $0 }, ask: ask)
        // Scroll and selection start over with each file.
        .id(file.path)
        .background(Theme.background)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 0) {
                    Text((file.path as NSString).lastPathComponent)
                        .font(.subheadline.weight(.semibold).monospaced())
                        .lineLimit(1)
                    let dir = (file.path as NSString).deletingLastPathComponent
                    if !dir.isEmpty {
                        Text(dir).font(.caption2.monospaced()).foregroundStyle(Theme.dim).lineLimit(1).truncationMode(.head)
                    }
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Toggle("Wrap Lines", systemImage: "text.justify.leading", isOn: $wraps)
                    Toggle("Show Whole File", systemImage: "doc.plaintext", isOn: Binding(
                        get: { reveals[file.path]?.all ?? false },
                        set: { all in
                            var reveal = reveals[file.path] ?? DiffReveal()
                            reveal.all = all
                            if !all { reveal.lines = [] }
                            reveals[file.path] = reveal
                        }))
                    Button("Copy Path", systemImage: "doc.on.doc") { UIPasteboard.general.string = file.path }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
    }
}

struct FileDiffView: View {
    var file: DiffFile
    var wraps: Bool
    @Binding var reveal: DiffReveal
    var position: Int
    var count: Int
    var step: (Int) -> Void
    var ask: (String) -> Void
    /// The selected rows: where it started and the other end, by row index.
    @State private var selection: (anchor: Int, head: Int)?
    @State private var jump = 0

    private static let fontSize: CGFloat = 12
    private static let charWidth = ("M" as NSString).size(withAttributes: [
        .font: UIFont.monospacedSystemFont(ofSize: fontSize, weight: .regular),
    ]).width

    private var rows: [DiffRow] { file.rows(reveal) }
    private var selectedRange: ClosedRange<Int>? {
        selection.map { min($0.anchor, $0.head)...max($0.anchor, $0.head) }
    }

    /// Characters needed for the line numbers.
    private var numberWidth: CGFloat {
        let largest = file.lines.lazy.compactMap(\.number).max() ?? 0
        return CGFloat(max(String(largest).count, 2)) * Self.charWidth + 6
    }

    var body: some View {
        let rows = rows
        ScrollViewReader { proxy in
            GeometryReader { space in
                ScrollView(wraps ? [.vertical] : [.vertical, .horizontal]) {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                            rowView(row, index: index)
                                .id(row.id)
                        }
                    }
                    .frame(width: wraps ? space.size.width : max(space.size.width, unwrappedWidth), alignment: .leading)
                    .padding(.bottom, 8)
                }
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                if let range = selectedRange {
                    selectionBar(rows, range)
                } else {
                    navigationBar(rows, proxy)
                }
            }
        }
    }

    private var unwrappedWidth: CGFloat {
        let longest = file.lines.lazy.map { $0.parts }.map { $0.before.count + $0.changed.count + $0.after.count }.max() ?? 0
        return numberWidth + 2 * Self.charWidth + CGFloat(longest) * Self.charWidth + 24
    }

    @ViewBuilder
    private func rowView(_ row: DiffRow, index: Int) -> some View {
        switch row {
        case .note(let text):
            Text(text)
                .font(.footnote.italic())
                .foregroundStyle(Theme.faint)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
        case .cut:
            Text("⋯")
                .font(.system(size: Self.fontSize, design: .monospaced))
                .foregroundStyle(Theme.faint)
                .padding(.horizontal, 12)
                .padding(.vertical, 4)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Theme.gapLine)
        case .gap(let start, let end, let section):
            Button {
                withAnimation(.snappy) { reveal = file.expanding(reveal, start: start, end: end) }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: "arrow.up.and.down").font(.caption2)
                    Text("\(end - start) unchanged line\(end - start == 1 ? "" : "s")")
                    if !section.isEmpty {
                        Text(section).foregroundStyle(Theme.dim).lineLimit(1)
                    }
                    Spacer(minLength: 0)
                }
                .font(.system(size: Self.fontSize, design: .monospaced))
                .foregroundStyle(Theme.accent)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .frame(maxWidth: wraps ? .infinity : nil, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.gapLine)
            .accessibilityHint("Shows more of the unchanged lines")
        case .line(let i):
            lineView(file.lines[i], selected: selectedRange?.contains(index) ?? false)
                .contentShape(Rectangle())
                .onTapGesture { select(index) }
        }
    }

    private func lineView(_ line: DiffLine, selected: Bool) -> some View {
        let (sign, signColor, background, emphasis): (String, Color, Color?, Color?) = switch line.kind {
        case .added: ("+", Theme.added, Theme.addedLine, Theme.addedChange)
        case .removed: ("-", Theme.error, Theme.removedLine, Theme.removedChange)
        case .context: (" ", Theme.faint, nil, nil)
        case .noNewline: (" ", Theme.faint, nil, nil)
        }
        return HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text(line.number.map(String.init) ?? "")
                .foregroundStyle(Theme.faint)
                .frame(width: numberWidth, alignment: .trailing)
            Text(sign)
                .foregroundStyle(signColor)
                .fontWeight(.bold)
                .frame(width: 2 * Self.charWidth)
            lineText(line, emphasis: emphasis)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .font(.system(size: Self.fontSize, design: .monospaced))
        .padding(.vertical, 1)
        .padding(.trailing, 8)
        .background(selected ? Theme.selectedLine : background ?? .clear)
    }

    private func lineText(_ line: DiffLine, emphasis: Color?) -> Text {
        if line.kind == .noNewline {
            return Text(line.text.replacingOccurrences(of: "\\ ", with: "")).italic().foregroundColor(Theme.faint)
        }
        let parts = line.parts
        var changed = AttributedString(parts.changed)
        changed.backgroundColor = emphasis
        let color = line.kind == .context ? Theme.dim : Theme.foreground
        return (Text(parts.before) + Text(changed) + Text(parts.after)).foregroundColor(color)
    }

    /// A first tap selects a line, the next stretches the selection to the line tapped; a tap
    /// on the only selected line clears it.
    private func select(_ index: Int) {
        guard let current = selection else {
            selection = (index, index)
            return
        }
        selection = current.anchor == index && current.head == index ? nil : (current.anchor, index)
    }

    private func selectionBar(_ rows: [DiffRow], _ range: ClosedRange<Int>) -> some View {
        let quote = file.quote(rows: rows, first: range.lowerBound, last: range.upperBound)
        return HStack(spacing: 12) {
            Button("Clear") { selection = nil }
                .buttonStyle(.bordered)
            Spacer()
            Text("\(range.count) line\(range.count == 1 ? "" : "s")")
                .font(.footnote)
                .foregroundStyle(Theme.dim)
            Spacer()
            Button("Copy", systemImage: "doc.on.doc") {
                UIPasteboard.general.string = quote
                selection = nil
            }
            .labelStyle(.iconOnly)
            .buttonStyle(.bordered)
            .accessibilityLabel("Copy lines")
            Button("Ask") {
                if let quote { ask(quote) }
            }
            .buttonStyle(.borderedProminent)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Theme.surface)
    }

    private func navigationBar(_ rows: [DiffRow], _ proxy: ScrollViewProxy) -> some View {
        // Each change: the first changed line after a gap, or the top.
        let stops = rows.indices.filter { i in
            guard case .line(let at) = rows[i], file.lines[at].kind != .context else { return false }
            guard i > 0, case .line(let before) = rows[i - 1] else { return true }
            return file.lines[before].kind == .context
        }
        return HStack(spacing: 18) {
            Button("Previous File", systemImage: "chevron.left") { step(-1) }
                .disabled(position == 0)
            Text("\(position + 1) of \(count)")
                .font(.footnote.monospacedDigit())
                .foregroundStyle(Theme.dim)
            Button("Next File", systemImage: "chevron.right") { step(1) }
                .disabled(position + 1 >= count)
            Spacer()
            DiffCounts(added: file.added, removed: file.removed)
            Spacer()
            Button("Previous Change", systemImage: "arrow.up") { go(-1, stops, rows, proxy) }
                .disabled(stops.isEmpty)
            Button("Next Change", systemImage: "arrow.down") { go(1, stops, rows, proxy) }
                .disabled(stops.isEmpty)
        }
        .labelStyle(.iconOnly)
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(Theme.surface)
    }

    private func go(_ by: Int, _ stops: [Int], _ rows: [DiffRow], _ proxy: ScrollViewProxy) {
        guard !stops.isEmpty else { return }
        jump = (jump + by + stops.count) % stops.count
        withAnimation(.snappy) {
            proxy.scrollTo(rows[stops[jump]].id, anchor: UnitPoint(x: 0, y: 0.25))
        }
    }
}
