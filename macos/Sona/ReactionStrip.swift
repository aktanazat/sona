import SwiftUI

/// The emoji on a comment or a task as chips with counts, and a button to
/// add one. A chip the user reacted with is filled; clicking it takes the
/// reaction off.
struct ReactionStrip: View {
    let subject: WorkReactionSubject
    let id: String
    let reactions: [WorkReaction]
    @Environment(AppModel.self) private var model
    @State private var shown: [WorkReaction]?

    static let choices = ["\u{1F44D}", "\u{2764}\u{FE0F}", "\u{1F389}", "\u{1F604}", "\u{1F440}", "\u{1F64F}", "\u{2705}"]

    var body: some View {
        let current = shown ?? reactions
        HStack(spacing: 4) {
            ForEach(current) { reaction in
                Button {
                    toggle(reaction.emoji, on: !reaction.mine)
                } label: {
                    HStack(spacing: 4) {
                        Text(reaction.emoji).font(.system(size: 12))
                        Text("\(reaction.by.count)").font(TypeScale.label(11)).monospacedDigit()
                    }
                    .padding(.horizontal, 6)
                    .frame(height: 20)
                    .background(reaction.mine ? Theme.accentSoft : Theme.inkTertiary.opacity(0.12), in: Capsule())
                }
                .buttonStyle(.plain)
                .help(reaction.by.map { $0.display(me: model.myName) }.joined(separator: ", "))
                .accessibilityLabel("\(reaction.emoji) \(reaction.by.count)")
                .accessibilityHint(reaction.mine ? "Removes your reaction" : "Adds your reaction")
            }
            Menu {
                ForEach(Self.choices, id: \.self) { emoji in
                    Button(emoji) { toggle(emoji, on: !(current.first { $0.emoji == emoji }?.mine ?? false)) }
                }
            } label: {
                Image(systemName: "face.smiling").font(.system(size: 12))
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Add reaction")
            .accessibilityLabel("Add reaction")
        }
        .onChange(of: reactions) { shown = nil }
    }

    private func toggle(_ emoji: String, on: Bool) {
        Task {
            if let next = await model.work.react(subject, id, emoji, on: on) { shown = next }
        }
    }
}
