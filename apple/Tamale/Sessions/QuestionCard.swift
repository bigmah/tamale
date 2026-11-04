// The agent's `ask_user` questions: tap options (one, or several on a multiple choice), or
// type an answer, then send them all at once.

import SwiftUI
import TamaleKit

struct QuestionCard: View {
    var asked: QuestionSet
    var submit: ([QuestionAnswer]) -> Void
    @State private var answers: [QuestionAnswer]
    @State private var current = 0
    @FocusState private var typing: Bool

    init(asked: QuestionSet, submit: @escaping ([QuestionAnswer]) -> Void) {
        self.asked = asked
        self.submit = submit
        _answers = State(initialValue: Array(repeating: QuestionAnswer(), count: asked.questions.count))
    }

    private var count: Int { asked.questions.count }
    private var last: Bool { current == count - 1 }
    private var question: QuestionSet.Question { asked.questions[current] }
    private var answered: Bool { !answers[current].isEmpty }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Image(systemName: "questionmark.bubble.fill").foregroundStyle(Theme.accent)
                Text(asked.description.isEmpty ? "tamale has a question" : asked.description)
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(2)
                Spacer(minLength: 0)
                if count > 1 {
                    Text("\(current + 1) of \(count)")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(Theme.faint)
                }
            }
            if count > 1 { tabs }
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    Text(question.question)
                        .font(.footnote.weight(.medium))
                        .foregroundStyle(Theme.bright)
                        .fixedSize(horizontal: false, vertical: true)
                    if question.multiSelect {
                        Text("Pick any that apply.").font(.caption).foregroundStyle(Theme.faint)
                    }
                    ForEach(Array(question.options.enumerated()), id: \.offset) { _, option in
                        optionRow(option)
                    }
                    TextField(question.options.isEmpty ? "Your answer" : "Or type your own answer",
                              text: otherBinding, axis: .vertical)
                        .lineLimit(1...4)
                        .focused($typing)
                        .font(.footnote)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 8)
                        .background(Theme.background, in: RoundedRectangle(cornerRadius: 8))
                        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.border, lineWidth: 0.5))
                        .accessibilityLabel("Your own answer")
                }
            }
            .frame(maxHeight: 360)
            .fixedSize(horizontal: false, vertical: true)
            .scrollBounceBehavior(.basedOnSize)
            HStack(spacing: 8) {
                if current > 0 {
                    Button {
                        go(current - 1)
                    } label: {
                        Text("Back").frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.bordered)
                }
                Button {
                    if last { send() } else { go(current + 1) }
                } label: {
                    Text(last ? (count > 1 ? "Send answers" : "Send") : "Next").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.accent)
                .disabled(!answered)
            }
        }
        .padding(14)
        .background(Theme.surface2)
        .overlay(alignment: .top) { Rectangle().fill(Theme.accent.opacity(0.5)).frame(height: 1) }
        .animation(.snappy, value: current)
    }

    /// One chip per question, to see which are answered and jump between them.
    private var tabs: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 6) {
                ForEach(Array(asked.questions.enumerated()), id: \.offset) { i, q in
                    let done = !answers[i].isEmpty
                    Button {
                        go(i)
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: done ? "checkmark.circle.fill" : "circle")
                                .font(.caption2)
                            Text(q.header.isEmpty ? "Q\(i + 1)" : q.header).font(.caption)
                        }
                        .padding(.horizontal, 9)
                        .padding(.vertical, 4)
                        .foregroundStyle(i == current ? Theme.background : done ? Theme.added : Theme.dim)
                        .background(i == current ? Theme.accent : Theme.surface3, in: Capsule())
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }

    private func optionRow(_ option: QuestionSet.Option) -> some View {
        let on = answers[current].chosen.contains(option.label)
        let symbol = question.multiSelect
            ? (on ? "checkmark.square.fill" : "square")
            : (on ? "largecircle.fill.circle" : "circle")
        return Button {
            toggle(option.label)
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: symbol)
                    .foregroundStyle(on ? Theme.accent : Theme.faint)
                    .font(.body)
                VStack(alignment: .leading, spacing: 2) {
                    Text(option.label)
                        .font(.footnote.weight(on ? .semibold : .regular))
                        .foregroundStyle(Theme.foreground)
                    if !option.description.isEmpty {
                        Text(option.description).font(.caption).foregroundStyle(Theme.dim)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .background(on ? Theme.accent.opacity(0.12) : Theme.surface3, in: RoundedRectangle(cornerRadius: 8))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(on ? Theme.accent.opacity(0.6) : .clear, lineWidth: 1))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(on ? .isSelected : [])
    }

    /// What's typed for the current question. On a single choice it replaces the picked option.
    private var otherBinding: Binding<String> {
        Binding(
            get: { answers[current].other },
            set: { text in
                answers[current].other = text
                if !question.multiSelect && !text.trimmingCharacters(in: .whitespaces).isEmpty {
                    answers[current].chosen = []
                }
            })
    }

    private func toggle(_ label: String) {
        var answer = answers[current]
        if question.multiSelect {
            if let i = answer.chosen.firstIndex(of: label) {
                answer.chosen.remove(at: i)
            } else {
                answer.chosen.append(label)
                // Keep the options' order.
                let order = question.options.map(\.label)
                answer.chosen.sort { (order.firstIndex(of: $0) ?? 0) < (order.firstIndex(of: $1) ?? 0) }
            }
        } else {
            answer.chosen = [label]
            answer.other = ""
            typing = false
        }
        answers[current] = answer
    }

    private func go(_ index: Int) {
        typing = false
        current = min(max(index, 0), count - 1)
    }

    private func send() {
        typing = false
        // Jump to whatever's still unanswered rather than send it blank.
        if let missing = answers.firstIndex(where: \.isEmpty) {
            current = missing
            return
        }
        submit(answers)
    }
}

#Preview {
    QuestionCard(asked: QuestionSet(description: "Ask how to fix login", questions: [
        .init(question: "How should the cookie be kept?", header: "Cookie", options: [
            .init(label: "Refresh it (Recommended)", description: "Renew it on every request"),
            .init(label: "Longer expiry"),
        ]),
        .init(question: "Which tests should I add?", header: "Tests",
              options: [.init(label: "Unit"), .init(label: "Integration")], multiSelect: true),
    ])) { _ in }
    .preferredColorScheme(.dark)
}
