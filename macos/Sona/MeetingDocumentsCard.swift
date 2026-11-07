import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// `MeetingDocument`: a document added to one meeting, with what was said
/// there that it says otherwise.
struct MeetingDocument: Decodable, Identifiable {
    let document: DocumentSummary
    /// Nil once the transcript is deleted and nothing can be compared.
    let differences: [DocumentDifference]?
    var id: String { document.id }
    var isPDF: Bool { document.mediaType == "application/pdf" }
}

/// `DocumentDifference`.
struct DocumentDifference: Decodable, Hashable {
    let page: Int?
    let passage: String
    let written: String
    let said: String
    let quote: String
    let speaker: String?
    let citation: ArtifactCitation
}

struct MarkedCopyParams: Encodable {
    struct Mark: Encodable {
        let page: Int
        let passage: String
        let note: String
    }

    struct Request: Encodable {
        let sessionId: String
        let documentId: String
        let destinationPath: String
        let marks: [Mark]

        enum CodingKeys: String, CodingKey {
            case sessionId = "session_id"
            case documentId = "document_id"
            case destinationPath = "destination_path"
            case marks
        }
    }

    let request: Request
}

/// The documents added to this meeting and where what was said differs from
/// them. A marked copy is written only when the person saves one, and the
/// PDF the meeting keeps never changes.
struct MeetingDocumentsCard: View {
    let core: Core
    let store: MeetingsStore
    let session: MeetingSessionSnapshot
    @State private var documents: [MeetingDocument] = []
    @State private var saved: [String: SavedCopy] = [:]
    @State private var failure: String?
    @State private var busy = false

    private struct SavedCopy {
        let url: URL
        let marks: Int
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            ForEach(documents) { document in section(document) }
            if let failure { Text(failure).metaText(Theme.live) }
        }
        .padding(.bottom, documents.isEmpty ? 0 : 16)
        .task(id: "\(session.sessionId):\(session.revision):\(store.documentsRevision)") { await load() }
    }

    private func section(_ document: MeetingDocument) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "doc.text")
                    .foregroundStyle(Theme.inkSecondary)
                    .accessibilityHidden(true)
                Text(document.document.title).bodyText(14).lineLimit(1)
            }
            Text(summary(document.differences)).metaText()
            ForEach(document.differences ?? [], id: \.self) { difference in row(difference) }
            if document.isPDF, let differences = document.differences, !differences.isEmpty {
                saveRow(document, differences)
            }
        }
    }

    private func summary(_ differences: [DocumentDifference]?) -> String {
        switch differences?.count {
        case nil: "The transcript was deleted, so there is nothing to compare."
        case 0: "No differences found in amounts, durations or percentages."
        case 1: "1 difference between what was said and the document"
        case let count?: "\(count) differences between what was said and the document"
        }
    }

    private func row(_ difference: DocumentDifference) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(emphasized("“\(flattened(difference.passage))”", difference.written))
                    .bodyText(13)
                    .lineLimit(3)
                Spacer(minLength: 8)
                if let page = difference.page { Text("Page \(page)").metaText() }
            }
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(emphasized(said(difference), difference.said))
                    .bodyText(13, Theme.inkSecondary)
                    .lineLimit(3)
                Spacer(minLength: 8)
                Button { store.playContribution(difference.citation.segmentId) } label: {
                    Image(systemName: "play.fill")
                }
                .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                .disabled(!store.canPlay(difference.citation.segmentId))
                .accessibilityLabel("Play where this was said")
            }
        }
    }

    private func saveRow(_ document: MeetingDocument, _ differences: [DocumentDifference]) -> some View {
        HStack(spacing: 12) {
            Button("Save a marked copy…") { save(document, differences) }
                .buttonStyle(.quiet)
                .disabled(busy)
            if let copy = saved[document.id] {
                Text(copy.marks == 0 ? "Saved a copy. No passage could be marked."
                     : copy.marks == 1 ? "Saved a copy with 1 mark." : "Saved a copy with \(copy.marks) marks.")
                    .metaText()
                Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([copy.url]) }
                    .buttonStyle(.quiet)
            }
        }
    }

    private func said(_ difference: DocumentDifference) -> String {
        "\(difference.speaker ?? "Someone") said “\(difference.quote)” at \(difference.citation.startOffsetNs.meetingOffsetClock)"
    }

    /// The note beside a marked passage, which has to make sense in the PDF
    /// on its own.
    private func note(_ difference: DocumentDifference) -> String {
        let day = session.startedAtUtcMs.map { " on \($0.meetingDate.formatted(date: .abbreviated, time: .omitted))" }
        return "\(said(difference)) in “\(session.title)”\(day ?? "")."
    }

    private func save(_ document: MeetingDocument, _ differences: [DocumentDifference]) {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.pdf]
        panel.nameFieldStringValue = "\(document.document.title) marked.pdf"
        panel.message = "Sona saves a marked copy. The PDF you added stays as it is."
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let marks = differences.compactMap { difference in
            difference.page.map { MarkedCopyParams.Mark(page: $0, passage: difference.passage, note: note(difference)) }
        }
        busy = true
        Task {
            defer { busy = false }
            do {
                let count: Int = try await core.request("doc_marked_copy", MarkedCopyParams(request: .init(
                    sessionId: session.sessionId, documentId: document.id,
                    destinationPath: url.path, marks: marks)))
                saved[document.id] = SavedCopy(url: url, marks: count)
                failure = nil
            } catch {
                failure = (error as? CoreError)?.remote(as: MeetingCommandError.self)?.label
                    ?? error.localizedDescription
            }
        }
    }

    private func load() async {
        do {
            let result: [MeetingDocument] = try await core.request(
                "doc_meeting_list", ["sessionId": session.sessionId])
            guard !Task.isCancelled else { return }
            documents = result
            failure = nil
        } catch {
            guard !Task.isCancelled else { return }
            failure = "This meeting's documents could not be read."
        }
    }

    private func flattened(_ text: String) -> String {
        text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    private func emphasized(_ text: String, _ part: String) -> AttributedString {
        var attributed = AttributedString(text)
        if let range = attributed.range(of: flattened(part)) {
            attributed[range].inlinePresentationIntent = .stronglyEmphasized
        }
        return attributed
    }
}
