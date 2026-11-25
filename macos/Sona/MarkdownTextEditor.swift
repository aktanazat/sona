import AppKit
import SwiftUI

/// The focused Markdown editor's link action, while words are selected. ⌘K is
/// a menu command, and a menu takes its key before the editor sees it, so the
/// menu item asks for this first.
struct MarkdownLinkAction: FocusedValueKey { typealias Value = () -> Void }

extension FocusedValues {
    var markdownLink: MarkdownLinkAction.Value? {
        get { self[MarkdownLinkAction.self] }
        set { self[MarkdownLinkAction.self] = newValue }
    }
}

/// The editor for Markdown notes and task details. It keeps the system text
/// editor, and adds the formatting keys, list keys and a bar over a selection.
/// Key codes are US ANSI, so the shortcuts match the keys' positions.
struct MarkdownTextEditor: View {
    @Binding var text: String
    @Binding var selection: TextSelection?
    var focused: FocusState<Bool>.Binding
    let label: String
    /// Runs before the formatting keys; a handled key stops there.
    var keys: (KeyPress) -> KeyPress.Result = { _ in .ignored }

    @State private var selectionBottom: CGFloat = 0

    private var span: MarkdownSpan? {
        guard case let .selection(range)? = selection?.indices else { return nil }
        return MarkdownSpan(
            text: text,
            start: text.distance(from: text.startIndex, to: range.lowerBound),
            end: text.distance(from: text.startIndex, to: range.upperBound)
        )
    }

    var body: some View {
        TextEditor(text: $text, selection: $selection)
            .scrollContentBackground(.hidden)
            .scrollDisabled(true)
            .focused(focused)
            .accessibilityLabel(label)
            .focusedValue(\.markdownLink, span.flatMap { $0.start == $0.end ? nil : { run(.link(nil)) } })
            .onKeyPress { press in
                if keys(press) == .handled { return .handled }
                guard let command = command(for: press), run(command) else { return .ignored }
                return .handled
            }
            .background(alignment: .topLeading) {
                // The editor cannot report where a selection is; a hidden copy
                // of the words up to its end can.
                Text(String(text.prefix(span?.end ?? 0)) + " ")
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                    .hidden()
                    .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { selectionBottom = $0 }
            }
            .overlay(alignment: .topLeading) {
                if let span, span.start != span.end {
                    FormatBar(run: { run($0) })
                        .offset(x: 5, y: selectionBottom + 4)
                }
            }
    }

    private func command(for press: KeyPress) -> MarkdownCommand? {
        let modifiers = press.modifiers
        // Shift-Tab arrives as the backtab character, not as Tab with Shift.
        if press.key == .tab || press.characters == "\u{19}", !modifiers.contains(.command) {
            return .nest(outdent: modifiers.contains(.shift) || press.characters == "\u{19}")
        }
        if press.key == .return, modifiers.isEmpty {
            return .newline
        }
        guard modifiers.contains(.command), let code = NSApp.currentEvent?.keyCode else { return nil }
        let shift = modifiers.contains(.shift)
        let option = modifiers.contains(.option)
        switch (code, shift, option) {
        case (11, false, false): return .mark("**")
        case (34, false, false): return .mark("*")
        case (1, true, false), (7, true, false): return .mark("~~")
        case (14, false, false): return .mark("`")
        case (9, false, false): return pastedLink.map { MarkdownCommand.link($0) }
        case (29, false, true): return .heading(0)
        case (18, false, true): return .heading(1)
        case (19, false, true): return .heading(2)
        case (20, false, true): return .heading(3)
        case (26, true, false): return .block(.numbered(1))
        case (28, true, false): return .block(.bullet)
        case (25, true, false): return .block(.check(done: false))
        case (42, true, false): return .codeBlock
        case (36, _, false): return .check(nested: shift)
        default: return nil
        }
    }

    /// A web address on the clipboard, pasted over a selection as a link.
    private var pastedLink: String? {
        guard let span, span.start != span.end,
              let copied = NSPasteboard.general.string(forType: .string)?
                  .trimmingCharacters(in: .whitespacesAndNewlines),
              MarkdownEdit.isWebLink(copied)
        else { return nil }
        return copied
    }

    @discardableResult
    private func run(_ command: MarkdownCommand) -> Bool {
        guard let span, let edited = MarkdownEdit.run(command, on: span) else { return false }
        // The binding may hand back the old text until the next update, so the
        // selection is placed in the edited text itself.
        let words = edited.text
        let start = words.index(words.startIndex, offsetBy: edited.start)
        let end = words.index(words.startIndex, offsetBy: edited.end)
        text = words
        selection = TextSelection(range: start..<end)
        return true
    }
}

/// The buttons over a selection. They sit under the selected words, and a
/// press keeps the selection so the next one still applies to it.
private struct FormatBar: View {
    let run: (MarkdownCommand) -> Void

    var body: some View {
        HStack(spacing: 2) {
            button("bold", "Bold", .mark("**"))
            button("italic", "Italic", .mark("*"))
            button("strikethrough", "Strikethrough", .mark("~~"))
            button("link", "Link", .link(nil))
            button("chevron.left.forwardslash.chevron.right", "Code", .mark("`"))
            button("text.quote", "Quote", .block(.quote))
            Menu {
                ForEach(1...3, id: \.self) { level in
                    Button("Heading \(level)") { run(.heading(level)) }
                }
            } label: {
                Image(systemName: "textformat.size")
                    .frame(width: 22, height: 20)
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .help("Heading")
            .accessibilityLabel("Heading")
        }
        .padding(4)
        .background(Theme.page, in: RoundedRectangle(cornerRadius: 6))
        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(Theme.inkTertiary.opacity(0.4)))
        .shadow(color: .black.opacity(0.12), radius: 6, y: 2)
    }

    private func button(_ symbol: String, _ title: String, _ command: MarkdownCommand) -> some View {
        Button { run(command) } label: {
            Image(systemName: symbol)
                .frame(width: 22, height: 20)
                .foregroundStyle(Theme.inkSecondary)
        }
        .buttonStyle(.borderless)
        .help(title)
        .accessibilityLabel(title)
    }
}
