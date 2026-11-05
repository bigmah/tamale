// The session's uncommitted changes (`diff` in src/web/files.rs), and how to lay them out: the
// app's counterpart of the terminal's `/diff` viewer (src/app/diff.rs). It folds unchanged lines
// the same way, lists files in the same tree, and quotes lines into a prompt the same way.

import Foundation

public struct DiffLine: Codable, Equatable, Hashable, Sendable {
    public enum Kind: String, Codable, Sendable {
        case context = " ", added = "+", removed = "-", noNewline = "\\"
    }

    public var kind: Kind
    public var old: Int?
    public var new: Int?
    public var text: String
    /// The part of the line that changed, in characters, when it pairs with one opposite.
    public var emphasis: [Int]?

    enum CodingKeys: String, CodingKey {
        case kind = "k", old = "o", new = "n", text = "t", emphasis = "e"
    }

    public init(kind: Kind, old: Int? = nil, new: Int? = nil, text: String, emphasis: [Int]? = nil) {
        self.kind = kind
        self.old = old
        self.new = new
        self.text = text
        self.emphasis = emphasis
    }

    /// The text to show, split around what to emphasize. Tabs are widened and a trailing
    /// carriage return dropped, as in the terminal.
    public var parts: (before: String, changed: String, after: String) {
        func clean<S: Sequence>(_ scalars: S) -> String where S.Element == Unicode.Scalar {
            var out = String.UnicodeScalarView()
            out.append(contentsOf: scalars)
            return String(out).replacingOccurrences(of: "\t", with: "    ")
        }
        var scalars = Array(text.unicodeScalars)
        if scalars.last == "\r" { scalars.removeLast() }
        guard let emphasis, emphasis.count == 2, emphasis[0] >= 0, emphasis[0] <= emphasis[1],
              emphasis[1] <= scalars.count else { return (clean(scalars), "", "") }
        return (clean(scalars[..<emphasis[0]]), clean(scalars[emphasis[0]..<emphasis[1]]), clean(scalars[emphasis[1]...]))
    }

    /// The line number to show: the new file's, or the old one's for a removed line.
    public var number: Int? { new ?? old }
}

public struct DiffFile: Codable, Equatable, Identifiable, Sendable {
    struct Hunk: Codable, Equatable, Sendable { var lines: [DiffLine] }

    public var path: String
    /// The path before a rename.
    public var oldPath: String?
    /// A, D, M, R or U, as in the terminal.
    public var status: String
    public var binary: Bool
    /// `[old, new]` file modes when they changed.
    public var modeChange: [String]?
    public var added: Int
    public var removed: Int
    /// Too much diff to send: only the counts came.
    public var omitted: Bool
    /// Every line of the file's diff. Mostly one run, the whole file; a big one's are separate
    /// runs with what's between them not sent.
    public var lines: [DiffLine]
    /// Indexes in `lines` where a run starts, after the first.
    public var breaks: Set<Int>

    public var id: String { path }

    enum CodingKeys: String, CodingKey {
        case path, status, binary, added, removed, omitted, hunks
        case oldPath = "old_path", modeChange = "mode_change"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = try c.decodeIfPresent(String.self, forKey: .path) ?? ""
        oldPath = try c.decodeIfPresent(String.self, forKey: .oldPath)
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? "M"
        binary = try c.decodeIfPresent(Bool.self, forKey: .binary) ?? false
        modeChange = try c.decodeIfPresent([String].self, forKey: .modeChange)
        added = try c.decodeIfPresent(Int.self, forKey: .added) ?? 0
        removed = try c.decodeIfPresent(Int.self, forKey: .removed) ?? 0
        omitted = try c.decodeIfPresent(Bool.self, forKey: .omitted) ?? false
        let hunks = try c.decodeIfPresent([Hunk].self, forKey: .hunks) ?? []
        lines = []
        breaks = []
        for hunk in hunks {
            if !lines.isEmpty { breaks.insert(lines.count) }
            lines += hunk.lines
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(path, forKey: .path)
        try c.encode(status, forKey: .status)
    }

    public init(path: String, status: String = "M", lines: [DiffLine], breaks: Set<Int> = []) {
        self.path = path
        self.status = status
        self.lines = lines
        self.breaks = breaks
        oldPath = nil
        binary = false
        modeChange = nil
        added = lines.filter { $0.kind == .added }.count
        removed = lines.filter { $0.kind == .removed }.count
        omitted = false
    }
}

public struct DiffReply: Codable, Equatable, Sendable {
    /// What's compared, e.g. "working tree vs HEAD".
    public var label: String
    public var files: [DiffFile]
    /// Why there's no diff, e.g. not a git repository.
    public var error: String?
    public var seq: Int?

    public var added: Int { files.reduce(0) { $0 + $1.added } }
    public var removed: Int { files.reduce(0) { $0 + $1.removed } }

    enum CodingKeys: String, CodingKey { case label, files, error, seq }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        label = try c.decodeIfPresent(String.self, forKey: .label) ?? ""
        files = try c.decodeIfPresent([DiffFile].self, forKey: .files) ?? []
        error = try c.decodeIfPresent(String.self, forKey: .error)
        seq = try c.decodeIfPresent(Int.self, forKey: .seq)
    }
}

// MARK: - Folding

/// One row of a file's diff.
public enum DiffRow: Equatable, Identifiable, Sendable {
    /// Renamed from, mode changed, binary, empty, too big.
    case note(String)
    /// Hidden unchanged lines `start..<end`, by index in `lines`; `section` is the definition
    /// the change after them is in.
    case gap(start: Int, end: Int, section: String)
    /// Lines the app wasn't sent, between two runs of a big file.
    case cut(Int)
    case line(Int)

    public var id: String {
        switch self {
        case .note(let text): "n\(text)"
        case .gap(let start, _, _): "g\(start)"
        case .cut(let at): "c\(at)"
        case .line(let i): "l\(i)"
        }
    }

    /// Where jumping between changes stops.
    public var isStop: Bool {
        switch self {
        case .gap, .cut: true
        default: false
        }
    }
}

/// Unchanged lines opened in a file, by new line number.
public struct DiffReveal: Equatable, Sendable {
    public var all = false
    public var lines: Set<Int> = []

    public init() {}
}

extension DiffFile {
    /// Unchanged lines shown around each change.
    public static let context = 3
    /// Hidden lines opened per tap on a gap.
    public static let expandStep = 20

    func shownLines(_ reveal: DiffReveal) -> [Bool] {
        if reveal.all { return Array(repeating: true, count: lines.count) }
        var shown = Array(repeating: false, count: lines.count)
        for (i, line) in lines.enumerated() {
            if line.kind == .added || line.kind == .removed {
                for j in max(0, i - Self.context)...min(lines.count - 1, i + Self.context) { shown[j] = true }
            } else if let new = line.new, reveal.lines.contains(new) {
                shown[i] = true
            }
        }
        // `\ No newline at end of file` belongs to the line before it.
        for i in lines.indices.dropFirst() where lines[i].kind == .noNewline { shown[i] = shown[i - 1] }
        return shown
    }

    /// The closest line before `start` that starts a definition (git's section heading).
    func section(before start: Int) -> String {
        for line in lines[..<start].reversed() where line.kind != .removed {
            if let first = line.text.first, first.isLetter || first == "_" || first == "$" {
                return line.parts.before
            }
        }
        return ""
    }

    /// The rows to show, with unchanged lines folded unless `reveal` opened them.
    public func rows(_ reveal: DiffReveal = DiffReveal()) -> [DiffRow] {
        var rows: [DiffRow] = []
        if let oldPath { rows.append(.note("Renamed from \(oldPath)")) }
        if let modeChange, modeChange.count == 2 { rows.append(.note("Mode changed \(modeChange[0]) → \(modeChange[1])")) }
        if omitted {
            rows.append(.note("Too large to show here. See it with /diff in the terminal."))
        } else if binary {
            rows.append(.note("Binary file not shown"))
        } else if lines.isEmpty && oldPath == nil && modeChange == nil {
            rows.append(.note("Empty file"))
        }
        let shown = shownLines(reveal)
        var i = 0
        while i < lines.count {
            if breaks.contains(i) { rows.append(.cut(i)) }
            guard !shown[i] else {
                rows.append(.line(i))
                i += 1
                continue
            }
            let start = i
            while i < lines.count, !shown[i], i == start || !breaks.contains(i) { i += 1 }
            rows.append(.gap(start: start, end: i, section: i < lines.count ? section(before: i) : ""))
        }
        return rows
    }

    /// `reveal` with more of the gap `start..<end` open, next to the changes around it; a
    /// small gap opens fully.
    public func expanding(_ reveal: DiffReveal, start: Int, end: Int) -> DiffReveal {
        let half = Self.expandStep / 2
        let opened: [Int]
        if end - start <= Self.expandStep {
            opened = Array(start..<end)
        } else if start == 0 {
            opened = Array((end - Self.expandStep)..<end)
        } else if end == lines.count {
            opened = Array(start..<(start + Self.expandStep))
        } else {
            opened = Array(start..<(start + half)) + Array((end - half)..<end)
        }
        var out = reveal
        out.lines.formUnion(opened.compactMap { lines[$0].new })
        return out
    }

    /// `rows[first...last]` quoted for a prompt: the path, line numbers and a diff block, as
    /// the terminal's viewer quotes a selection. Nil if there are no lines in it.
    public func quote(rows: [DiffRow], first: Int, last: Int) -> String? {
        var body = ""
        var new: [Int] = []
        var old: [Int] = []
        for row in rows[first...last] {
            switch row {
            case .line(let i):
                let line = lines[i]
                let sign = line.kind == .added ? "+" : line.kind == .removed ? "-" : " "
                var text = line.text
                if text.hasSuffix("\r") { text.removeLast() }
                body += sign + text + "\n"
                if let n = line.new { new.append(n) }
                if let n = line.old { old.append(n) }
            case .gap(let start, let end, _):
                body += " … \(end - start) unchanged lines\n"
            case .cut, .note:
                break
            }
        }
        guard !body.isEmpty else { return nil }
        func range(_ numbers: [Int], _ what: String) -> String {
            let (a, b) = (numbers.min() ?? 0, numbers.max() ?? 0)
            return a == b ? "\(what) \(a)" : "\(what)s \(a)–\(b)"
        }
        let where_ = !new.isEmpty ? range(new, "line") : range(old, "removed line")
        return "In `\(path)` (\(where_)):\n```diff\n\(body)```\n\n"
    }
}

// MARK: - The file tree

/// One row of the changed files' tree: a directory, by its full path, or a file.
public struct DiffTreeRow: Equatable, Identifiable, Sendable {
    public var depth: Int
    public var name: String
    /// Index in the files; nil for a directory.
    public var file: Int?
    /// The full path of a directory, for collapsing it.
    public var dir: String?

    public var id: String { dir.map { "d" + $0 } ?? "f\(file ?? -1)" }
}

public enum DiffTree {
    final class Node {
        var dirs: [String: Node] = [:]
        var files: [(name: String, index: Int)] = []
    }

    /// Directories first, then files, each sorted; chains like `src/app` merge into one row.
    public static func rows(_ files: [DiffFile], collapsed: Set<String> = []) -> [DiffTreeRow] {
        let root = Node()
        for (i, file) in files.enumerated() {
            var parts = file.path.split(separator: "/").map(String.init)
            let name = parts.popLast() ?? file.path
            var node = root
            for part in parts {
                if node.dirs[part] == nil { node.dirs[part] = Node() }
                node = node.dirs[part]!
            }
            node.files.append((name, i))
        }
        var rows: [DiffTreeRow] = []
        flatten(root, prefix: "", depth: 0, collapsed: collapsed, into: &rows)
        return rows
    }

    private static func flatten(_ node: Node, prefix: String, depth: Int, collapsed: Set<String>, into rows: inout [DiffTreeRow]) {
        for (name, child) in node.dirs.sorted(by: { $0.key < $1.key }) {
            var name = name
            var child = child
            while child.files.isEmpty, child.dirs.count == 1, let (next, grandchild) = child.dirs.first {
                name += "/" + next
                child = grandchild
            }
            let path = prefix + name
            rows.append(DiffTreeRow(depth: depth, name: name, file: nil, dir: path))
            if !collapsed.contains(path) {
                flatten(child, prefix: path + "/", depth: depth + 1, collapsed: collapsed, into: &rows)
            }
        }
        for (name, index) in node.files.sorted(by: { $0.name < $1.name }) {
            rows.append(DiffTreeRow(depth: depth, name: name, file: index, dir: nil))
        }
    }

    /// The files in the order the tree shows them, ignoring what's collapsed.
    public static func order(_ files: [DiffFile]) -> [Int] {
        rows(files).compactMap(\.file)
    }
}
