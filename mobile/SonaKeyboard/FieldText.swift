import Foundation

/// The field text an action works on: the selection when there is one, otherwise all the
/// text the field shows the keyboard around the cursor.
struct FieldTarget: Equatable {
    let text: String
    let selected: Bool
}

/// The field as the tools see it: the host's report, with the keyboard's own last edit in
/// place of the text before the cursor while the host still shows the field from before
/// it. Every change the tools make is a typing edit the keyboard applies itself, so the
/// field memory sees it as it sees a key.
@MainActor
struct FieldText: Equatable {
    let before: String
    let after: String
    let selection: String?

    init(hostBefore: String, hostAfter: String, selection: String?, echo: FieldEcho) {
        before = echo.joining(before: hostBefore, after: hostAfter)
        after = hostAfter
        self.selection = selection.flatMap { $0.isEmpty ? nil : $0 }
    }

    /// How to change the field: move the cursor forward past `move` UTF-16 units, which is
    /// how the host counts, then apply `edit` before the cursor.
    struct Step: Equatable {
        var move = 0
        let edit: TypingSession.Edit
    }

    struct Change: Equatable {
        let original: String
        let replacement: String
    }

    var target: FieldTarget {
        if let selection { return FieldTarget(text: selection, selected: true) }
        return FieldTarget(text: before + after, selected: false)
    }

    /// Puts `text` where `target` was; nil when the field changed since it was read.
    func replacing(_ target: FieldTarget, with text: String) -> Step? {
        guard self.target == target else { return nil }
        if target.selected { return Step(edit: TypingSession.Edit(insert: text)) }
        return Step(move: after.utf16.count, edit: TypingSession.Edit(deleteCount: target.text.count, insert: text))
    }

    /// Takes back a replacement while the cursor still sits right after it.
    func undoing(_ change: Change) -> Step? {
        guard before.hasSuffix(change.replacement) else { return nil }
        return Step(edit: TypingSession.Edit(deleteCount: change.replacement.count, insert: change.original))
    }

    /// Text dropped in whole at the cursor, spaced from the word before it.
    func inserting(_ text: String) -> Step {
        Step(edit: TypingSession.Edit(insert: TypingSession.joiningSpace(before: before) + text))
    }
}
