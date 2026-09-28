import AppKit

/// "Copy notes": the notes the review page shows, put on the clipboard once as
/// formatted text that pastes into a mail or a document with its headings and
/// lists, and once as Markdown. The sections, labels and order are the page's
/// and a shared link's: what Sona wrote, then the person's own notes.
enum MeetingNotesCopy {
    struct Item {
        let text: String
        let meta: String?
    }

    struct Section {
        let heading: String
        var paragraphs: [String] = []
        var items: [Item] = []
    }

    /// Whether `copy` has anything to put on the clipboard. Stops at the first
    /// written character: the header asks on every render, typing included.
    static func hasNotes(_ snapshot: MeetingReviewSnapshot, userNotes: String) -> Bool {
        if hasText(userNotes) { return true }
        guard let content = snapshot.readableArtifact?.content else { return false }
        return hasText(content.summary.text) || hasText(content.followUpDraft.text)
            || content.outline.contains { hasText($0.title.text) }
            || content.decisions.contains { hasText($0.text) }
            || content.actionItems.contains { hasText($0.text.text) }
            || content.keyQuestions.contains { hasText($0.text) }
            || content.risks.contains { hasText($0.text) }
    }

    private static func hasText(_ text: String) -> Bool {
        text.contains { !$0.isWhitespace }
    }

    /// Puts the notes on the clipboard; false when there is nothing to copy.
    @discardableResult
    static func copy(_ snapshot: MeetingReviewSnapshot, userNotes: String, asMarkdown: Bool) -> Bool {
        let parts = Self.sections(snapshot, userNotes: userNotes)
        guard !parts.isEmpty else { return false }
        let title = snapshot.session.title
        let text = Self.markdown(title: title, sections: parts)
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        if asMarkdown {
            pasteboard.setString(text, forType: .string)
        } else {
            pasteboard.declareTypes([.html, .string], owner: nil)
            pasteboard.setString(Self.html(title: title, sections: parts), forType: .html)
            pasteboard.setString(text, forType: .string)
        }
        return true
    }

    static func sections(_ snapshot: MeetingReviewSnapshot, userNotes: String) -> [Section] {
        var result: [Section] = []
        func add(_ section: Section) {
            if !section.paragraphs.isEmpty || !section.items.isEmpty { result.append(section) }
        }
        if let content = snapshot.readableArtifact?.content {
            add(Section(heading: "Summary", paragraphs: paragraphs(content.summary.text)))
            add(Section(
                heading: "What was covered",
                items: content.outline.compactMap { item($0.title.text, meta: $0.detail?.text) }))
            add(Section(heading: "Decisions", items: content.decisions.compactMap { item($0.text) }))
            add(Section(heading: "Action items", items: content.actionItems.compactMap { action in
                let owner = [action.ownerText, action.dueText]
                    .compactMap { $0?.trimmingCharacters(in: .whitespacesAndNewlines) }
                    .filter { !$0.isEmpty }
                    .joined(separator: " · ")
                return item(action.text.text, meta: owner)
            }))
            add(Section(
                heading: "Questions worth answering",
                items: content.keyQuestions.compactMap { item($0.text) }))
            add(Section(heading: "Risks", items: content.risks.compactMap { item($0.text) }))
            add(Section(heading: "Follow up", paragraphs: paragraphs(content.followUpDraft.text)))
        }
        add(Section(heading: "Your own notes", paragraphs: paragraphs(userNotes)))
        return result
    }

    static func markdown(title: String, sections: [Section]) -> String {
        var lines = ["# \(title)"]
        for section in sections {
            lines.append("")
            lines.append("## \(section.heading)")
            lines.append("")
            lines.append(contentsOf: section.paragraphs.flatMap { [$0, ""] }.dropLast())
            for item in section.items {
                lines.append("- \(item.text)" + (item.meta.map { " (\($0))" } ?? ""))
            }
        }
        return lines.joined(separator: "\n") + "\n"
    }

    static func html(title: String, sections: [Section]) -> String {
        var html = "<meta charset=\"utf-8\"><h1>\(escape(title))</h1>"
        for section in sections {
            html += "<h2>\(escape(section.heading))</h2>"
            for paragraph in section.paragraphs {
                html += "<p>\(escape(paragraph).replacingOccurrences(of: "\n", with: "<br>"))</p>"
            }
            if !section.items.isEmpty {
                html += "<ul>"
                for item in section.items {
                    html += "<li>\(escape(item.text))"
                    if let meta = item.meta { html += " <em>(\(escape(meta)))</em>" }
                    html += "</li>"
                }
                html += "</ul>"
            }
        }
        return html
    }

    private static func item(_ text: String, meta: String? = nil) -> Item? {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return nil }
        let meta = meta?.trimmingCharacters(in: .whitespacesAndNewlines)
        return Item(text: text, meta: meta?.isEmpty == false ? meta : nil)
    }

    /// Paragraphs are separated by blank lines; single breaks stay inside.
    private static func paragraphs(_ text: String) -> [String] {
        var result: [String] = []
        var current: [String] = []
        let lines = text.replacingOccurrences(of: "\r\n", with: "\n")
            .split(separator: "\n", omittingEmptySubsequences: false)
        for line in lines + [""] {
            if line.trimmingCharacters(in: .whitespaces).isEmpty {
                if !current.isEmpty {
                    result.append(current.joined(separator: "\n"))
                    current = []
                }
            } else {
                current.append(String(line))
            }
        }
        return result
    }

    private static func escape(_ text: String) -> String {
        text.replacingOccurrences(of: "&", with: "&amp;")
            .replacingOccurrences(of: "<", with: "&lt;")
            .replacingOccurrences(of: ">", with: "&gt;")
            .replacingOccurrences(of: "\"", with: "&quot;")
    }
}
