// One entry of the conversation, the folded runs of tool calls, the approval card and the
// composer.

import SwiftUI
import TamaleKit

struct EntryView: View {
    var entry: Entry
    var model: SessionModel
    var open: (ArtifactSource) -> Void

    var body: some View {
        switch entry.kind {
        case .user:
            HStack {
                Spacer(minLength: 40)
                UserText(text: entry.text ?? "")
                    .padding(.horizontal, 12)
                    .padding(.vertical, 9)
                    .background(Theme.surface3, in: RoundedRectangle(cornerRadius: 14))
            }
        case .assistant:
            prose
        case .note:
            prose
                .font(.subheadline)
                .foregroundStyle(Theme.dim)
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Theme.surface, in: RoundedRectangle(cornerRadius: 8))
        case .info:
            Text(Markdown.inline(entry.text ?? ""))
                .font(.footnote)
                .foregroundStyle(Theme.faint)
                .textSelection(.enabled)
        case .error:
            Label {
                Text(Markdown.inline(entry.text ?? "")).textSelection(.enabled)
            } icon: {
                Image(systemName: "exclamationmark.triangle.fill")
            }
            .font(.footnote)
            .foregroundStyle(Theme.error)
        case .tool:
            ToolRow(entry: entry)
        case .unknown:
            EmptyView()
        }
    }

    private var prose: some View {
        let (markdown, artifacts) = entry.prose
        return MarkdownView(markdown) { n in
            if n < artifacts.count {
                // A note sits in a box of its own, which the card stays inside.
                ArtifactCard(block: artifacts[n], model: model, bleeds: entry.kind == .assistant, open: open)
            }
        }
        .foregroundStyle(Theme.foreground)
        .textSelection(.enabled)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A user message: its text, with fenced blocks (quoted code) set as code.
struct UserText: View {
    var text: String

    var body: some View {
        let parts = text.components(separatedBy: "```")
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(parts.enumerated()), id: \.offset) { i, part in
                if i % 2 == 1 {
                    let body = part.drop { $0 != "\n" }.dropFirst()
                    Text(String(body).trimmingCharacters(in: .newlines))
                        .font(.code)
                        .padding(6)
                        .background(Theme.surface2, in: RoundedRectangle(cornerRadius: 6))
                } else if !part.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    Text(part.trimmingCharacters(in: .newlines))
                }
            }
        }
        .foregroundStyle(Theme.bright)
        .textSelection(.enabled)
    }
}

/// A run of tool calls: the last few, with the rest behind "N earlier steps".
struct ToolGroup: View {
    var entries: [Entry]
    @Binding var expanded: Bool
    static let shown = 4

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            if entries.count > Self.shown {
                Button {
                    withAnimation(.snappy) { expanded.toggle() }
                } label: {
                    let hidden = entries.count - Self.shown
                    Label(expanded ? "Show fewer steps" : "Show \(hidden) earlier step\(hidden == 1 ? "" : "s")",
                          systemImage: expanded ? "chevron.up" : "chevron.down")
                        .font(.caption)
                        .foregroundStyle(Theme.dim)
                }
                .buttonStyle(.plain)
                .padding(.bottom, 4)
            }
            let visible = expanded ? entries[...] : entries.suffix(Self.shown)
            ForEach(Array(visible.enumerated()), id: \.offset) { _, entry in
                ToolRow(entry: entry)
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: 10))
    }
}

struct ToolRow: View {
    var entry: Entry
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                if entry.output != nil { withAnimation(.snappy) { open.toggle() } }
            } label: {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Group {
                        if entry.pending {
                            ProgressView().controlSize(.mini)
                        } else if entry.error == true {
                            Image(systemName: "xmark").foregroundStyle(Theme.error)
                        } else {
                            Image(systemName: "checkmark").foregroundStyle(Theme.added)
                        }
                    }
                    .font(.caption.weight(.bold))
                    .frame(width: 14)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(entry.description?.isEmpty == false ? entry.description! : entry.name ?? "tool")
                            .font(.footnote)
                            .foregroundStyle(Theme.foreground)
                            .lineLimit(open ? nil : 1)
                        if let summary = shownSummary {
                            Text(summary)
                                .font(.caption.monospaced())
                                .foregroundStyle(Theme.faint)
                                .lineLimit(open ? nil : 1)
                                .truncationMode(.middle)
                        }
                    }
                    Spacer(minLength: 0)
                    if entry.output != nil {
                        Image(systemName: "chevron.right")
                            .font(.caption2)
                            .foregroundStyle(Theme.faint)
                            .rotationEffect(.degrees(open ? 90 : 0))
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if open, let output = entry.output {
                ScrollView([.horizontal, .vertical]) {
                    Text(output.isEmpty ? "(no output)" : output + (entry.cut == true ? "\n…" : ""))
                        .font(.caption.monospaced())
                        .foregroundStyle(entry.error == true ? Theme.error : Theme.dim)
                        .textSelection(.enabled)
                        .padding(8)
                }
                .frame(maxHeight: 280)
                .fixedSize(horizontal: false, vertical: true)
                .background(Theme.background, in: RoundedRectangle(cornerRadius: 6))
            }
        }
        .padding(.vertical, 3)
    }

    /// The summary, unless the description already says it ("Read src/main.rs").
    private var shownSummary: String? {
        let summary = (entry.summary ?? "").split(separator: "\n").first.map(String.init) ?? ""
        guard !summary.isEmpty, !(entry.description ?? "").contains(summary) else { return nil }
        return summary
    }
}

struct ApprovalCard: View {
    var approval: Approval
    var choose: (ApprovalChoice, String?) -> Void
    @State private var refusing = false
    @State private var reason = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Image(systemName: "hand.raised.fill").foregroundStyle(Theme.warning)
                Text("Allow \(approval.name)?").font(.subheadline.weight(.semibold))
            }
            if !approval.description.isEmpty {
                Text(approval.description).font(.footnote).foregroundStyle(Theme.foreground)
            }
            if !approval.summary.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    Text(approval.summary)
                        .font(.caption.monospaced())
                        .foregroundStyle(Theme.bright)
                        .textSelection(.enabled)
                        .padding(8)
                }
                .frame(maxHeight: 120)
                .fixedSize(horizontal: false, vertical: true)
                .background(Theme.background, in: RoundedRectangle(cornerRadius: 6))
            }
            if refusing {
                TextField("Tell tamale why, or what to do instead (optional)", text: $reason, axis: .vertical)
                    .textFieldStyle(.roundedBorder)
                    .lineLimit(1...4)
                HStack {
                    Button("Back") { refusing = false }
                        .buttonStyle(.bordered)
                    Spacer()
                    Button("Refuse", role: .destructive) {
                        choose(.no, reason)
                        refusing = false
                        reason = ""
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.error)
                }
            } else {
                HStack(spacing: 8) {
                    Button { choose(.yes, nil) } label: { Text("Yes").frame(maxWidth: .infinity) }
                        .buttonStyle(.borderedProminent)
                        .tint(Theme.added)
                    if !approval.always.isEmpty {
                        Button { choose(.always, nil) } label: { Text("Always").frame(maxWidth: .infinity) }
                            .buttonStyle(.bordered)
                    }
                    Button { refusing = true } label: { Text("No").frame(maxWidth: .infinity) }
                        .buttonStyle(.bordered)
                        .tint(Theme.error)
                }
                if !approval.always.isEmpty {
                    Text("Always allows: \(approval.always.joined(separator: ", "))")
                        .font(.caption2)
                        .foregroundStyle(Theme.faint)
                }
            }
        }
        .padding(14)
        .background(Theme.surface2)
        .overlay(alignment: .top) { Rectangle().fill(Theme.warning.opacity(0.5)).frame(height: 1) }
    }
}

struct Composer: View {
    var model: SessionModel
    @Binding var draft: String
    var focused: FocusState<Bool>.Binding

    private var empty: Bool { draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let queued = model.status?.queued, !queued.isEmpty {
                Text("Queued: \(queued.joined(separator: " · "))")
                    .font(.caption)
                    .foregroundStyle(Theme.dim)
                    .lineLimit(2)
            }
            HStack(alignment: .bottom, spacing: 8) {
                TextField(model.question != nil ? "Or reply in your own words…" : model.busy ? "Queue a message…" : "Ask tamale…",
                          text: $draft, axis: .vertical)
                    .lineLimit(1...8)
                    .focused(focused)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 9)
                    .background(Theme.surface2, in: RoundedRectangle(cornerRadius: 18))
                    .overlay(RoundedRectangle(cornerRadius: 18).stroke(Theme.border, lineWidth: 0.5))
                    .disabled(!model.connection.isLive)
                if model.busy && empty {
                    Button {
                        model.interrupt()
                    } label: {
                        Image(systemName: "stop.circle.fill").font(.system(size: 32))
                    }
                    .tint(Theme.error)
                    .accessibilityLabel("Interrupt")
                } else {
                    Button {
                        if model.send(draft) { draft = "" }
                    } label: {
                        Image(systemName: "arrow.up.circle.fill").font(.system(size: 32))
                    }
                    .disabled(empty || !model.connection.isLive)
                    .accessibilityLabel("Send")
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Theme.surface)
    }
}
