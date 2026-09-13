import AppKit
import Foundation
import SwiftUI

/// The release note for this build, shown once.
///
/// Two readers use this store. The gate: on the first launch of a new build,
/// `shouldShowOnLaunch` is true while the preference is on and a bundled note
/// is newer than the version last seen, and dismissing it records the version
/// so it never opens again. The Debug page: `previewLatest()` opens the newest
/// bundled note and dismissing that records nothing.
@MainActor
@Observable
final class WhatsNewStore {
    /// The note the sheet is showing, whether from the gate or the preview.
    private(set) var note: ReleaseNotesNote?
    /// Whether the gate has a note for this launch. The integrator reads this
    /// to decide whether to present the sheet at startup.
    private(set) var shouldShowOnLaunch = false
    /// The running build, which decides how far the notes may reach.
    private(set) var currentVersion: String?
    private(set) var error: String?

    /// A dismissal is final for that version within this run: the settings
    /// re-read that follows the write must not reopen the sheet.
    @ObservationIgnored private var dismissedVersion: String?
    /// True while the open note came from the gate, so closing it is what
    /// marks the version seen. A preview marks nothing.
    @ObservationIgnored private var marksSeenOnDismiss = false
    @ObservationIgnored private let core: Core

    init(core: Core) {
        self.core = core
        currentVersion = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        core.observe(CoreEvent.aboutSettingsChanged) { [weak self] _ in self?.reload() }
    }

    func start() async {
        await load()
    }

    /// The note the gate would show right now, ignoring what is already open.
    private func gateNote(_ settings: AboutSettings) -> ReleaseNotesNote? {
        guard settings.showWhatsNewOnUpdate ?? true, let currentVersion else { return nil }
        let note = ReleaseNotesCatalog.noteToShow(
            currentVersion: currentVersion,
            lastSeenVersion: settings.whatsNewLastSeenVersion ?? ""
        )
        guard let note, note.version != dismissedVersion else { return nil }
        return note
    }

    private func load() async {
        do {
            let settings: AboutSettings = try await core.request("get_app_settings")
            let gate = gateNote(settings)
            shouldShowOnLaunch = gate != nil
            if let gate, note == nil {
                note = gate
                marksSeenOnDismiss = true
            }
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func reload() {
        Task { await load() }
    }

    /// The Debug page's preview: the newest bundled note, whatever version is
    /// running, and closing it leaves the persisted version alone.
    func previewLatest() {
        guard let latest = ReleaseNotesCatalog.latest else {
            error = "No bundled release notes found"
            return
        }
        marksSeenOnDismiss = false
        note = latest
        error = nil
    }

    /// Closes the sheet. A note the gate opened is recorded as seen.
    func dismiss() {
        guard let note else { return }
        let marksSeen = marksSeenOnDismiss
        self.note = nil
        shouldShowOnLaunch = false
        marksSeenOnDismiss = false
        guard marksSeen else { return }
        dismissedVersion = note.version
        Task {
            do {
                try await core.request(
                    "change_whats_new_last_seen_version_setting",
                    ["version": note.version]
                )
                error = nil
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

/// The release note as a sheet: one title, the note's prose, one way out.
struct WhatsNewView: View {
    let store: WhatsNewStore
    var onClose: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(title)
                .font(TypeScale.headline)
                .foregroundStyle(Theme.ink)
                .padding(.horizontal, 28)
                .padding(.top, 28)
                .padding(.bottom, 20)

            ErrorNote(store.error)
                .padding(.horizontal, 28)

            // A note is as long as its release was, so the body scrolls and
            // the title stays put.
            ScrollView {
                ReleaseNotesBody(markdown: store.note?.markdown ?? "")
                    .padding(.horizontal, 28)
                    .padding(.bottom, 24)
            }
            .scrollIndicators(.automatic)
            .frame(maxHeight: 460)

            Hairline()

            HStack {
                Spacer()
                Button("Close") { close() }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.cancelAction)
            }
            .padding(20)
        }
        .frame(width: 620)
        .background(Theme.page)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusDialog))
    }

    private var title: String {
        guard let version = store.note?.version else { return "New in Sona" }
        return "New in Sona v\(version)"
    }

    private func close() {
        store.dismiss()
        onClose()
    }
}

// MARK: - Markdown

/// One block of a release note. The set is what the old renderer allowed:
/// headings, paragraphs, lists, quotes, code, separators and images.
enum ReleaseNotesBlock: Identifiable {
    case heading(level: Int, text: String)
    case paragraph(String)
    case bullets([String])
    case numbered([String])
    case quote(String)
    case code(String)
    case rule
    /// Release-note images lived in the web app's `public/release-notes/`,
    /// which the native app does not ship, so only the alt text survives.
    case image(alt: String)

    var id: String {
        switch self {
        case let .heading(level, text): "h\(level):\(text)"
        case let .paragraph(text): "p:\(text)"
        case let .bullets(items): "ul:\(items.joined(separator: "|"))"
        case let .numbered(items): "ol:\(items.joined(separator: "|"))"
        case let .quote(text): "q:\(text)"
        case let .code(text): "c:\(text)"
        case .rule: "hr"
        case let .image(alt): "img:\(alt)"
        }
    }
}

/// Markdown into blocks. Raw HTML is not a case, so it never renders — the
/// old renderer skipped it too.
enum ReleaseNotesParser {
    static func blocks(from markdown: String) -> [ReleaseNotesBlock] {
        var blocks: [ReleaseNotesBlock] = []
        var paragraph: [String] = []
        var bullets: [String] = []
        var numbered: [String] = []
        var quote: [String] = []
        var fence: [String]?

        func flushParagraph() {
            guard !paragraph.isEmpty else { return }
            blocks.append(.paragraph(paragraph.joined()))
            paragraph = []
        }
        func flushLists() {
            if !bullets.isEmpty {
                blocks.append(.bullets(bullets))
                bullets = []
            }
            if !numbered.isEmpty {
                blocks.append(.numbered(numbered))
                numbered = []
            }
            if !quote.isEmpty {
                blocks.append(.quote(quote.joined(separator: " ")))
                quote = []
            }
        }
        func flushAll() {
            flushParagraph()
            flushLists()
        }

        for rawLine in markdown.components(separatedBy: "\n") {
            let line = rawLine.trimmingCharacters(in: .whitespaces)

            if var open = fence {
                if line.hasPrefix("```") {
                    blocks.append(.code(open.joined(separator: "\n")))
                    fence = nil
                } else {
                    open.append(rawLine)
                    fence = open
                }
                continue
            }
            if line.hasPrefix("```") {
                flushAll()
                fence = []
                continue
            }
            if line.isEmpty {
                flushAll()
                continue
            }
            if line == "---" || line == "***" || line == "___" {
                flushAll()
                blocks.append(.rule)
                continue
            }
            if let heading = heading(line) {
                flushAll()
                blocks.append(heading)
                continue
            }
            if let alt = image(line) {
                flushAll()
                blocks.append(.image(alt: alt))
                continue
            }
            if let item = listItem(line, markers: ["- ", "* ", "+ "]) {
                flushParagraph()
                if !numbered.isEmpty || !quote.isEmpty { flushLists() }
                bullets.append(item)
                continue
            }
            if let item = orderedItem(line) {
                flushParagraph()
                if !bullets.isEmpty || !quote.isEmpty { flushLists() }
                numbered.append(item)
                continue
            }
            if line.hasPrefix(">") {
                flushParagraph()
                if !bullets.isEmpty || !numbered.isEmpty { flushLists() }
                quote.append(String(line.dropFirst()).trimmingCharacters(in: .whitespaces))
                continue
            }
            flushLists()
            // A soft wrap inside a paragraph is a space; two trailing spaces
            // or a trailing backslash is the hard break `br` stood for.
            if rawLine.hasSuffix("  ") || rawLine.hasSuffix("\\") {
                var text = line
                if text.hasSuffix("\\") { text.removeLast() }
                paragraph.append(text.trimmingCharacters(in: .whitespaces) + "\n")
            } else {
                paragraph.append(paragraph.isEmpty ? line : " " + line)
            }
        }
        if let open = fence {
            blocks.append(.code(open.joined(separator: "\n")))
        }
        flushAll()
        return blocks
    }

    private static func heading(_ line: String) -> ReleaseNotesBlock? {
        for level in [3, 2, 1] {
            let marker = String(repeating: "#", count: level) + " "
            if line.hasPrefix(marker) {
                return .heading(
                    level: level,
                    text: String(line.dropFirst(marker.count)).trimmingCharacters(in: .whitespaces)
                )
            }
        }
        return nil
    }

    private static func listItem(_ line: String, markers: [String]) -> String? {
        for marker in markers where line.hasPrefix(marker) {
            return String(line.dropFirst(marker.count))
        }
        return nil
    }

    /// "1. item", the only ordered form the notes use.
    private static func orderedItem(_ line: String) -> String? {
        let digits = line.prefix { $0.isNumber }
        guard !digits.isEmpty else { return nil }
        let rest = line.dropFirst(digits.count)
        guard rest.hasPrefix(". ") else { return nil }
        return String(rest.dropFirst(2))
    }

    /// `![alt](source)` alone on a line.
    private static func image(_ line: String) -> String? {
        guard line.hasPrefix("!["), let close = line.firstIndex(of: "]") else { return nil }
        let alt = line[line.index(line.startIndex, offsetBy: 2)..<close]
        return String(alt)
    }
}

/// Inline Markdown as an `AttributedString`, with the same link rule the old
/// renderer enforced: only http, https and mailto survive as links, and
/// anything else keeps its text and loses its destination.
enum ReleaseNotesInline {
    static let safeSchemes = ["http", "https", "mailto"]

    static func attributed(_ text: String, size: CGFloat = 15) -> AttributedString {
        var attributed: AttributedString
        do {
            attributed = try AttributedString(
                markdown: text,
                options: AttributedString.MarkdownParsingOptions(
                    interpretedSyntax: .inlineOnlyPreservingWhitespace
                )
            )
        } catch {
            attributed = AttributedString(text)
        }
        for run in attributed.runs {
            if let link = attributed[run.range].link,
               !safeSchemes.contains(link.scheme?.lowercased() ?? "") {
                attributed[run.range].link = nil
            }
            if let intent = attributed[run.range].inlinePresentationIntent,
               intent.contains(.code) {
                attributed[run.range].font = TypeScale.mono(size - 2)
            }
        }
        return attributed
    }
}

/// A release note, rendered. Headings, body, lists and quotes in the app's own
/// ladder; no colour of their own beyond the ink ramp.
struct ReleaseNotesBody: View {
    let markdown: String

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(ReleaseNotesParser.blocks(from: markdown)) { block in
                switch block {
                case let .heading(level, text):
                    Text(ReleaseNotesInline.attributed(text, size: level == 1 ? 17 : 15))
                        .font(level == 1 ? TypeScale.headline : TypeScale.label())
                        .foregroundStyle(Theme.ink)
                        .padding(.top, level == 1 ? 0 : 6)
                case let .paragraph(text):
                    Text(ReleaseNotesInline.attributed(text))
                        .bodyText(15, Theme.inkSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                case let .bullets(items):
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                            ReleaseNotesListItem(marker: "•", text: item)
                        }
                    }
                case let .numbered(items):
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                            ReleaseNotesListItem(marker: "\(index + 1).", text: item)
                        }
                    }
                case let .quote(text):
                    HStack(alignment: .top, spacing: 12) {
                        Rectangle().fill(Theme.border).frame(width: 2)
                        Text(ReleaseNotesInline.attributed(text))
                            .bodyText(15, Theme.inkSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                case let .code(text):
                    Text(text)
                        .font(TypeScale.mono(13))
                        .foregroundStyle(Theme.ink)
                        .textSelection(.enabled)
                        .padding(12)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                case .rule:
                    Hairline()
                case let .image(alt):
                    Text(alt.isEmpty ? "Image" : alt)
                        .metaText()
                        .italic()
                }
            }
        }
        .textSelection(.enabled)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// One list row: its marker in quiet ink, its text hanging beside it.
private struct ReleaseNotesListItem: View {
    let marker: String
    let text: String

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Text(marker)
                .bodyText(15, Theme.inkTertiary)
                .frame(minWidth: 14, alignment: .leading)
            Text(ReleaseNotesInline.attributed(text))
                .bodyText(15, Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}
