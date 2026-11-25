import Foundation

/// The text with its selection as character offsets. Kept free of SwiftUI so
/// the Markdown editing rules run in a plain test.
struct MarkdownSpan: Equatable {
    var text: String
    var start: Int
    var end: Int
}

enum MarkdownLine: Equatable {
    case plain
    case heading(Int)
    case bullet
    case numbered(Int)
    case check(done: Bool)
    case quote

    /// The same kind of line, whatever its number or checkbox state.
    func same(as other: MarkdownLine) -> Bool {
        switch (self, other) {
        case let (.heading(level), .heading(otherLevel)): level == otherLevel
        case (.numbered, .numbered), (.check, .check): true
        case (.plain, .plain), (.bullet, .bullet), (.quote, .quote): true
        default: false
        }
    }

    var isList: Bool {
        switch self {
        case .bullet, .numbered, .check: true
        default: false
        }
    }
}

enum MarkdownCommand: Equatable {
    case mark(String)
    case link(String?)
    case heading(Int)
    case block(MarkdownLine)
    case codeBlock
    case check(nested: Bool)
    case newline
    case nest(outdent: Bool)
}

enum MarkdownEdit {
    /// Nil when the command does not apply here, so the key keeps its normal meaning.
    static func run(_ command: MarkdownCommand, on span: MarkdownSpan) -> MarkdownSpan? {
        switch command {
        case let .mark(mark): wrap(span, mark)
        case let .link(url): link(span, url: url)
        case let .heading(level): heading(span, level: level)
        case let .block(kind): restyle(span) { $0.same(as: kind) ? .plain : kind }
        case .codeBlock: codeBlock(span)
        case let .check(nested): check(span, nested: nested)
        case .newline: newline(span)
        case let .nest(outdent): nest(span, outdent: outdent)
        }
    }

    /// Only a plain web address pastes as a link; anything else keeps the normal paste.
    static func isWebLink(_ text: String) -> Bool {
        guard !text.contains(where: { $0.isWhitespace || "()<>".contains($0) }),
              let url = URL(string: text),
              ["http", "https"].contains(url.scheme?.lowercased() ?? ""),
              url.host?.isEmpty == false
        else { return false }
        return true
    }

    static func parse(_ line: String) -> (indent: String, kind: MarkdownLine, body: String) {
        let indent = String(line.prefix { $0 == " " || $0 == "\t" })
        let rest = String(line.dropFirst(indent.count))

        let hashes = rest.prefix { $0 == "#" }.count
        if (1...3).contains(hashes), rest.dropFirst(hashes).hasPrefix(" ") {
            return (indent, .heading(hashes), String(rest.dropFirst(hashes + 1)))
        }
        if let marker = rest.first, "-*+".contains(marker), rest.dropFirst().hasPrefix(" ") {
            let item = String(rest.dropFirst(2))
            if item.hasPrefix("[ ]") || item.hasPrefix("[x]") || item.hasPrefix("[X]") {
                let after = String(item.dropFirst(3))
                if after.isEmpty || after.hasPrefix(" ") {
                    let body = after.hasPrefix(" ") ? String(after.dropFirst()) : after
                    return (indent, .check(done: !item.hasPrefix("[ ]")), body)
                }
            }
            return (indent, .bullet, item)
        }
        let digits = rest.prefix { $0 >= "0" && $0 <= "9" }
        let afterDigits = rest.dropFirst(digits.count)
        if let number = Int(digits), afterDigits.hasPrefix(". ") || afterDigits.hasPrefix(") ") {
            return (indent, .numbered(number), String(afterDigits.dropFirst(2)))
        }
        if rest.hasPrefix(">") {
            let after = rest.dropFirst()
            return (indent, .quote, String(after.hasPrefix(" ") ? after.dropFirst() : after))
        }
        return (indent, .plain, rest)
    }

    static func render(_ indent: String, _ kind: MarkdownLine, _ body: String) -> String {
        indent + marker(kind) + body
    }

    static func marker(_ kind: MarkdownLine) -> String {
        switch kind {
        case .plain: ""
        case let .heading(level): String(repeating: "#", count: level) + " "
        case .bullet: "- "
        case let .numbered(number): "\(number). "
        case let .check(done): done ? "- [x] " : "- [ ] "
        case .quote: "> "
        }
    }

    /// Adds the mark around the span, or removes it when the span already has it.
    private static func wrap(_ span: MarkdownSpan, _ mark: String) -> MarkdownSpan {
        var chars = Array(span.text)
        let marks = Array(mark)
        let (s, e, n) = (span.start, span.end, marks.count)
        let open = s >= n && Array(chars[(s - n)..<s]) == marks
        let close = e + n <= chars.count && Array(chars[e..<(e + n)]) == marks
        // A single * next to a second * is bold's marker, not italic's.
        let doubled = n == 1
            && ((s >= 2 && chars[s - 2] == marks[0]) || (e + 1 < chars.count && chars[e + 1] == marks[0]))
        if open && close && !doubled {
            chars.removeSubrange(e..<(e + n))
            chars.removeSubrange((s - n)..<s)
            return MarkdownSpan(text: String(chars), start: s - n, end: e - n)
        }
        chars.insert(contentsOf: marks, at: e)
        chars.insert(contentsOf: marks, at: s)
        return MarkdownSpan(text: String(chars), start: s + n, end: e + n)
    }

    /// Replaces the span with a link. Without a url the placeholder is selected for typing over.
    private static func link(_ span: MarkdownSpan, url: String?) -> MarkdownSpan {
        var chars = Array(span.text)
        let label = String(chars[span.start..<span.end])
        let target = url ?? "https://"
        let markdown = "[" + label + "](" + target + ")"
        chars.replaceSubrange(span.start..<span.end, with: Array(markdown))
        let text = String(chars)
        if url == nil {
            let start = span.start + label.count + 3
            return MarkdownSpan(text: text, start: start, end: start + target.count)
        }
        let caret = span.start + markdown.count
        return MarkdownSpan(text: text, start: caret, end: caret)
    }

    private static func heading(_ span: MarkdownSpan, level: Int) -> MarkdownSpan {
        restyle(span) { line in
            if level == 0 {
                if case .heading = line { return .plain }
                return line
            }
            return line == .heading(level) ? .plain : .heading(level)
        }
    }

    /// Restyles every line the span touches, then renumbers the lists it changed.
    private static func restyle(_ span: MarkdownSpan, _ next: (MarkdownLine) -> MarkdownLine) -> MarkdownSpan {
        var doc = Document(span)
        let rows = doc.start.line...doc.end.line
        for row in rows {
            let parts = parse(doc.lines[row])
            let kind = next(parts.kind)
            doc.lines[row] = render(parts.indent, kind, parts.body)
            doc.shift(row, by: prefixLength(parts.indent, kind) - prefixLength(parts.indent, parts.kind))
        }
        doc.renumber(rows)
        return doc.span
    }

    private static func codeBlock(_ span: MarkdownSpan) -> MarkdownSpan {
        var doc = Document(span)
        let rows = doc.start.line...doc.end.line
        doc.lines.insert("```", at: rows.upperBound + 1)
        doc.lines.insert("```", at: rows.lowerBound)
        doc.start.line += 1
        doc.end.line += 1
        return doc.span
    }

    /// Flips the checkbox on the caret line. Nested checks follow when asked.
    private static func check(_ span: MarkdownSpan, nested: Bool) -> MarkdownSpan {
        var doc = Document(span)
        let row = doc.start.line
        let parts = parse(doc.lines[row])
        let done = parts.kind == .check(done: false)
        let kind = MarkdownLine.check(done: done)
        doc.lines[row] = render(parts.indent, kind, parts.body)
        doc.shift(row, by: prefixLength(parts.indent, kind) - prefixLength(parts.indent, parts.kind))
        if nested {
            var child = row + 1
            while child < doc.lines.count {
                let parsed = parse(doc.lines[child])
                guard parsed.indent.count > parts.indent.count else { break }
                if case .check = parsed.kind {
                    doc.lines[child] = render(parsed.indent, .check(done: done), parsed.body)
                }
                child += 1
            }
        }
        return doc.span
    }

    /// Return on a list line starts the next item. Return on an empty item ends the list.
    private static func newline(_ span: MarkdownSpan) -> MarkdownSpan? {
        guard span.start == span.end else { return nil }
        var doc = Document(span)
        let row = doc.start.line
        let line = doc.lines[row]
        let parts = parse(line)
        let column = doc.start.column
        guard parts.kind.isList, column >= prefixLength(parts.indent, parts.kind) else { return nil }
        if parts.body.isEmpty {
            doc.lines[row] = ""
            doc.start = Document.Position(line: row, column: 0)
            doc.end = doc.start
            return doc.span
        }
        let after = String(line.dropFirst(column))
        let next: MarkdownLine = switch parts.kind {
        case .numbered: .numbered(1)
        case .check: .check(done: false)
        default: .bullet
        }
        doc.lines[row] = String(line.prefix(column))
        doc.lines.insert(render(parts.indent, next, after), at: row + 1)
        doc.renumber((row + 1)...(row + 1))
        let caret = doc.lines[row + 1].count - after.count
        doc.start = Document.Position(line: row + 1, column: caret)
        doc.end = doc.start
        return doc.span
    }

    /// Indents or outdents the list lines in the span by two spaces. Nil when none are list lines.
    private static func nest(_ span: MarkdownSpan, outdent: Bool) -> MarkdownSpan? {
        var doc = Document(span)
        let rows = doc.start.line...doc.end.line
        var changed = false
        for row in rows where parse(doc.lines[row]).kind.isList {
            let line = doc.lines[row]
            if outdent {
                let removed = line.hasPrefix("\t") ? 1 : min(2, line.prefix { $0 == " " }.count)
                guard removed > 0 else { continue }
                doc.lines[row] = String(line.dropFirst(removed))
                doc.shift(row, by: -removed)
            } else {
                doc.lines[row] = "  " + line
                doc.shift(row, by: 2)
            }
            changed = true
        }
        guard changed else { return nil }
        doc.renumber(rows)
        return doc.span
    }

    private static func prefixLength(_ indent: String, _ kind: MarkdownLine) -> Int {
        indent.count + marker(kind).count
    }

    /// The text split into lines, with the caret or selection as line and column.
    private struct Document {
        struct Position {
            var line: Int
            var column: Int
        }

        var lines: [String]
        var start: Position
        var end: Position

        init(_ span: MarkdownSpan) {
            lines = span.text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
            start = Document.position(span.start, in: lines)
            end = Document.position(span.end, in: lines)
        }

        var span: MarkdownSpan {
            let first = offset(start)
            let last = offset(end)
            return MarkdownSpan(text: lines.joined(separator: "\n"), start: min(first, last), end: max(first, last))
        }

        mutating func shift(_ row: Int, by delta: Int) {
            if start.line == row { start.column = max(0, start.column + delta) }
            if end.line == row { end.column = max(0, end.column + delta) }
        }

        /// Numbers the numbered lines from the first row on, and any numbered lines straight after the rows.
        mutating func renumber(_ rows: ClosedRange<Int>) {
            var row = rows.lowerBound
            while row < lines.count {
                let parts = MarkdownEdit.parse(lines[row])
                if case .numbered = parts.kind {
                    lines[row] = MarkdownEdit.render(parts.indent, .numbered(number(at: row)), parts.body)
                } else if !rows.contains(row) {
                    break
                }
                row += 1
            }
        }

        /// One more than the item above at the same indent, or 1 when the list starts here.
        private func number(at row: Int) -> Int {
            let indent = MarkdownEdit.parse(lines[row]).indent.count
            var above = row - 1
            while above >= 0 {
                let parts = MarkdownEdit.parse(lines[above])
                let depth = parts.indent.count
                if depth < indent { break }
                if depth == indent {
                    if case let .numbered(number) = parts.kind { return number + 1 }
                    break
                }
                above -= 1
            }
            return 1
        }

        private func offset(_ position: Position) -> Int {
            let row = min(position.line, lines.count - 1)
            let before = lines.prefix(row).reduce(0) { $0 + $1.count + 1 }
            return before + min(max(position.column, 0), lines[row].count)
        }

        private static func position(_ offset: Int, in lines: [String]) -> Position {
            var rest = offset
            for (row, line) in lines.enumerated() {
                if rest <= line.count { return Position(line: row, column: rest) }
                rest -= line.count + 1
            }
            return Position(line: lines.count - 1, column: lines.last?.count ?? 0)
        }
    }
}
