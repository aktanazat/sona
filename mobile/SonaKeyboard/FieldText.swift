import UIKit

/// The field text an action works on: the selection when there is one, otherwise all the
/// text the field shows the keyboard around the cursor.
struct FieldTarget: Equatable {
    let text: String
    let selected: Bool
}

/// Reading and rewriting the host field through the document proxy, which only deletes
/// before the cursor, moves it and inserts.
@MainActor
enum FieldText {
    static func target(in proxy: UITextDocumentProxy) -> FieldTarget {
        if let selection = proxy.selectedText, !selection.isEmpty { return FieldTarget(text: selection, selected: true) }
        return FieldTarget(text: (proxy.documentContextBeforeInput ?? "") + (proxy.documentContextAfterInput ?? ""), selected: false)
    }

    /// Puts `text` where `target` was. False when the field changed since it was read.
    static func replace(_ target: FieldTarget, with text: String, in proxy: UITextDocumentProxy) -> Bool {
        guard self.target(in: proxy) == target else { return false }
        if !target.selected {
            let after = proxy.documentContextAfterInput ?? ""
            /* The host moves the cursor by UTF-16 units, not characters. */
            if !after.isEmpty { proxy.adjustTextPosition(byCharacterOffset: after.utf16.count) }
            for _ in 0..<target.text.count { proxy.deleteBackward() }
        }
        proxy.insertText(text)
        return true
    }

    /// Takes back a replacement while the cursor still sits right after it.
    static func undo(_ change: Change, in proxy: UITextDocumentProxy) -> Bool {
        guard (proxy.documentContextBeforeInput ?? "").hasSuffix(change.replacement) else { return false }
        for _ in 0..<change.replacement.count { proxy.deleteBackward() }
        proxy.insertText(change.original)
        return true
    }

    struct Change: Equatable {
        let original: String
        let replacement: String
    }
}
