import AppKit
import SwiftUI

/// The focused Markdown editor's link action, while words are selected. ⌘K
/// typed in the editor is taken by the editor itself; the menu item asks for
/// this when it is chosen with the pointer.
struct MarkdownLinkAction: FocusedValueKey { typealias Value = () -> Void }

extension FocusedValues {
    var markdownLink: MarkdownLinkAction.Value? {
        get { self[MarkdownLinkAction.self] }
        set { self[MarkdownLinkAction.self] = newValue }
    }
}

extension TextSelection {
    /// The selected range in `text`, or nil when the selection was made in
    /// longer text that has since been replaced, so it no longer fits.
    func range(in text: String) -> Range<String.Index>? {
        guard case let .selection(range) = indices, range.upperBound <= text.endIndex else { return nil }
        return range
    }
}

/// A key the editor's owner may take before the editor does.
enum MarkdownEditorKey {
    case up, down, escape, newline, tab
}

/// The editor for Markdown notes and task details: one text view that is as
/// tall as its words, with the formatting keys, list keys and a bar over a
/// selection. A formatting change is undone with ⌘Z like typing.
struct MarkdownTextEditor: View {
    @Binding var text: String
    @Binding var selection: TextSelection?
    @Binding var focused: Bool
    /// Which document the words belong to. A new one starts a fresh undo
    /// history: undoing must never bring one note's words into another.
    let document: AnyHashable
    let label: String
    var fontSize: CGFloat = 14
    var lineSpacing: CGFloat = 0
    var minHeight: CGFloat = 0
    var writingTools = true
    /// Runs before the editor's own keys; true stops there.
    var keys: (MarkdownEditorKey) -> Bool = { _ in false }
    /// Where the line the selection ends on ends, from the editor's top.
    var selectionMoved: (CGFloat) -> Void = { _ in }

    @State private var editor = MarkdownEditorHandle()
    @State private var selectionBottom: CGFloat = 0

    private var wordsSelected: Bool {
        selection?.range(in: text).map { !$0.isEmpty } ?? false
    }

    var body: some View {
        MarkdownTextField(
            text: $text, selection: $selection, focused: $focused, document: document, label: label, fontSize: fontSize,
            lineSpacing: lineSpacing, minHeight: minHeight, writingTools: writingTools, keys: keys,
            handle: editor
        ) { bottom in
            selectionBottom = bottom
            selectionMoved(bottom)
        }
        .focusedValue(\.markdownLink, wordsSelected ? { editor.run(.link(nil)) } : nil)
        .overlay(alignment: .topLeading) {
            if wordsSelected {
                FormatBar(run: { editor.run($0) })
                    .offset(x: 5, y: selectionBottom + 4)
            }
        }
    }
}

/// Reaches the text view from the buttons and menu items drawn around it.
@MainActor
private final class MarkdownEditorHandle {
    weak var view: MarkdownTextView?

    func run(_ command: MarkdownCommand) {
        view?.apply(command)
    }
}

private struct MarkdownTextField: NSViewRepresentable {
    @Binding var text: String
    @Binding var selection: TextSelection?
    @Binding var focused: Bool
    let document: AnyHashable
    let label: String
    let fontSize: CGFloat
    let lineSpacing: CGFloat
    let minHeight: CGFloat
    let writingTools: Bool
    let keys: (MarkdownEditorKey) -> Bool
    let handle: MarkdownEditorHandle
    let selectionMoved: (CGFloat) -> Void

    func makeNSView(context: Context) -> MarkdownTextView {
        let view = MarkdownTextView(usingTextLayoutManager: false)
        view.delegate = context.coordinator
        view.isRichText = false
        view.importsGraphics = false
        view.allowsUndo = true
        view.drawsBackground = false
        view.textContainerInset = .zero
        view.isVerticallyResizable = false
        view.isHorizontallyResizable = false
        view.textContainer?.widthTracksTextView = true
        view.textContainer?.lineFragmentPadding = 5
        view.font = .systemFont(ofSize: fontSize)
        view.textColor = NSColor(Theme.ink)
        view.insertionPointColor = NSColor(Theme.ink)
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineSpacing = lineSpacing
        view.defaultParagraphStyle = paragraph
        view.typingAttributes[.paragraphStyle] = paragraph
        if !writingTools { view.writingToolsBehavior = .none }
        view.string = text
        context.coordinator.document = document
        handle.view = view
        return view
    }

    func updateNSView(_ view: MarkdownTextView, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        handle.view = view
        view.keys = keys
        view.focusChanged = { [weak coordinator] now in coordinator?.focusChanged(now) }
        view.setAccessibilityLabel(label)
        coordinator.applying = true
        defer { coordinator.applying = false }
        if coordinator.document != document {
            coordinator.document = document
            view.unmarkText()
            view.string = text
            coordinator.undo.removeAllActions()
        } else if !view.hasMarkedText(), view.string != text {
            if view.window?.firstResponder === view {
                view.replace(with: text)
            } else {
                view.string = text
                coordinator.undo.removeAllActions()
            }
        }
        if !view.hasMarkedText(), let range = selection?.range(in: text) {
            let wanted = NSRange(range, in: text)
            if wanted != view.selectedRange(), NSMaxRange(wanted) <= (view.string as NSString).length {
                view.setSelectedRange(wanted)
            }
        }
        if focused != coordinator.focused {
            coordinator.focused = focused
            view.wantsFocus = focused
        }
        coordinator.reportSelection(of: view)
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: MarkdownTextView, context: Context) -> CGSize? {
        guard let width = proposal.width, width.isFinite, width > 0 else { return nil }
        return CGSize(width: width, height: max(nsView.height(at: width), minHeight))
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(parent: self)
    }

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: MarkdownTextField
        /// Set while SwiftUI's values are written into the view, so the view
        /// does not hand them back mid-update.
        var applying = false
        /// The focus last asked for or reported, so only a change acts.
        var focused = false
        /// The editor's own history, cleared when other words are put in.
        let undo = UndoManager()
        var document: AnyHashable?
        private var bottom: CGFloat?

        init(parent: MarkdownTextField) {
            self.parent = parent
        }

        func undoManager(for view: NSTextView) -> UndoManager? {
            undo
        }

        func textDidChange(_ notification: Notification) {
            guard !applying, let view = notification.object as? MarkdownTextView else { return }
            parent.text = view.string
        }

        func textViewDidChangeSelection(_ notification: Notification) {
            guard !applying, let view = notification.object as? MarkdownTextView, !view.hasMarkedText() else { return }
            let words = view.string
            parent.selection = Range(view.selectedRange(), in: words).map { TextSelection(range: $0) }
            reportSelection(of: view)
        }

        /// The caret is given and taken inside `updateNSView` too, where
        /// SwiftUI state may not change, so the report waits for the next turn.
        func focusChanged(_ now: Bool) {
            guard focused != now else { return }
            focused = now
            let binding = parent.$focused
            DispatchQueue.main.async { binding.wrappedValue = now }
        }

        func reportSelection(of view: MarkdownTextView) {
            let now = view.selectionBottom()
            guard now != bottom else { return }
            bottom = now
            let moved = parent.selectionMoved
            DispatchQueue.main.async { moved(now) }
        }
    }
}

/// The text view under the editor: formatting keys arrive before the menus
/// do, and list keys as the commands the system makes of them, so an input
/// method composing a word keeps Return and Tab for itself.
final class MarkdownTextView: NSTextView {
    var keys: (MarkdownEditorKey) -> Bool = { _ in false }
    var focusChanged: (Bool) -> Void = { _ in }
    var wantsFocus = false {
        didSet { claimFocus() }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        claimFocus()
    }

    private func claimFocus() {
        guard let window else { return }
        if wantsFocus, window.firstResponder !== self {
            window.makeFirstResponder(self)
        } else if !wantsFocus, window.firstResponder === self {
            window.makeFirstResponder(nil)
        }
    }

    override func becomeFirstResponder() -> Bool {
        let became = super.becomeFirstResponder()
        if became { focusChanged(true) }
        return became
    }

    override func resignFirstResponder() -> Bool {
        let resigned = super.resignFirstResponder()
        if resigned { focusChanged(false) }
        return resigned
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard window?.firstResponder === self, event.modifierFlags.contains(.command),
              let command = Self.command(
                  typed: event.charactersIgnoringModifiers, code: event.keyCode,
                  modifiers: event.modifierFlags, link: pastedLink),
              apply(command)
        else { return super.performKeyEquivalent(with: event) }
        return true
    }

    override func doCommand(by selector: Selector) {
        let key: MarkdownEditorKey? = switch selector {
        case #selector(moveUp(_:)): .up
        case #selector(moveDown(_:)): .down
        case #selector(cancelOperation(_:)): .escape
        case #selector(insertNewline(_:)): .newline
        case #selector(insertTab(_:)): .tab
        default: nil
        }
        if let key, keys(key) { return }
        let command: MarkdownCommand? = switch selector {
        case #selector(insertNewline(_:)): .newline
        case #selector(insertTab(_:)): .nest(outdent: false)
        case #selector(insertBacktab(_:)): .nest(outdent: true)
        default: nil
        }
        if let command, apply(command) { return }
        super.doCommand(by: selector)
    }

    /// The formatting a ⌘ key asks for. A letter goes by the letter the
    /// layout types, so ⌘B is bold on Dvorak and AZERTY too; a layout with no
    /// Latin letters, and the digit and symbol keys, go by the US position.
    static func command(
        typed: String?, code: UInt16, modifiers: NSEvent.ModifierFlags, link: String?
    ) -> MarkdownCommand? {
        let shift = modifiers.contains(.shift)
        let option = modifiers.contains(.option)
        let character = typed?.lowercased().count == 1 ? typed?.lowercased().first : nil
        let letter: Character? = if let character, character.isLetter {
            character.isASCII ? character : usLetters[code]
        } else {
            nil
        }
        switch (letter, shift, option) {
        case ("b", false, false): return .mark("**")
        case ("i", false, false): return .mark("*")
        case ("s", true, false), ("x", true, false): return .mark("~~")
        case ("e", false, false): return .mark("`")
        case ("v", false, false): return link.map { .link($0) }
        case (.some, _, _): return nil
        default: break
        }
        switch (code, shift, option) {
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

    /// The letters the shortcuts use, by their US key code.
    private static let usLetters: [UInt16: Character] = [11: "b", 34: "i", 1: "s", 7: "x", 14: "e", 9: "v"]

    /// A web address on the clipboard, pasted over a selection as a link.
    private var pastedLink: String? {
        guard selectedRange().length > 0,
              let copied = NSPasteboard.general.string(forType: .string)?
                  .trimmingCharacters(in: .whitespacesAndNewlines),
              MarkdownEdit.isWebLink(copied)
        else { return nil }
        return copied
    }

    /// Runs one formatting command on the selection, as one step of undo.
    @discardableResult
    func apply(_ command: MarkdownCommand) -> Bool {
        let words = string
        guard let range = Range(selectedRange(), in: words) else { return false }
        let span = MarkdownSpan(
            text: words,
            start: words.distance(from: words.startIndex, to: range.lowerBound),
            end: words.distance(from: words.startIndex, to: range.upperBound))
        guard let edited = MarkdownEdit.run(command, on: span) else { return false }
        breakUndoCoalescing()
        replace(with: edited.text)
        breakUndoCoalescing()
        let result = edited.text
        let start = result.index(result.startIndex, offsetBy: edited.start)
        let end = result.index(result.startIndex, offsetBy: edited.end)
        setSelectedRange(NSRange(start..<end, in: result))
        return true
    }

    /// Writes `new` over the words through the path typing takes, so it can
    /// be undone, touching only the part that differs.
    func replace(with new: String) {
        let old = string as NSString
        let next = new as NSString
        let shorter = min(old.length, next.length)
        var prefix = 0
        while prefix < shorter, old.character(at: prefix) == next.character(at: prefix) { prefix += 1 }
        var suffix = 0
        while suffix < shorter - prefix,
              old.character(at: old.length - 1 - suffix) == next.character(at: next.length - 1 - suffix) {
            suffix += 1
        }
        let range = NSRange(location: prefix, length: old.length - prefix - suffix)
        let replacement = next.substring(with: NSRange(location: prefix, length: next.length - prefix - suffix))
        guard shouldChangeText(in: range, replacementString: replacement) else { return }
        textStorage?.replaceCharacters(in: range, with: replacement)
        didChangeText()
    }

    /// The height the words take at this width. SwiftUI asks at several
    /// widths before it places the view, and the frame it then sets may not
    /// change, which would leave the words wrapped at the last width asked;
    /// so the container goes back to the view's own width.
    func height(at width: CGFloat) -> CGFloat {
        guard let layout = layoutManager, let container = textContainer else { return 0 }
        container.containerSize = NSSize(width: width, height: .greatestFiniteMagnitude)
        layout.ensureLayout(for: container)
        let height = ceil(layout.usedRect(for: container).height)
        if bounds.width > 0, bounds.width != width {
            container.containerSize = NSSize(width: bounds.width, height: .greatestFiniteMagnitude)
        }
        return height
    }

    /// Where the line the selection ends on ends, from the top.
    func selectionBottom() -> CGFloat {
        guard let layout = layoutManager, let container = textContainer else { return 0 }
        layout.ensureLayout(for: container)
        let length = (string as NSString).length
        let location = NSMaxRange(selectedRange())
        let line: NSRect = if location < length {
            layout.lineFragmentRect(forGlyphAt: layout.glyphIndexForCharacter(at: location), effectiveRange: nil)
        } else if !layout.extraLineFragmentRect.isEmpty || length == 0 {
            layout.extraLineFragmentRect
        } else {
            layout.lineFragmentRect(forGlyphAt: layout.glyphIndexForCharacter(at: length - 1), effectiveRange: nil)
        }
        return line.maxY
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
