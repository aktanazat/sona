import SwiftUI

/// Bring recordings in from disk, and watch the ones already running.
///
/// Files arrive from the picker or from a drag, each becomes a row, and the
/// rows keep reporting after the button is pressed. A file is handed to the
/// core one at a time and runs in the core's own queue, so this page can be
/// left over a run without stopping anything: the queue below is the same
/// jobs Library lists.
struct ImportView: View {
    let store: ImportStore
    /// Show what a routed file became. The shell owns navigation.
    var openLink: (String) -> Void = { _ in }

    @State private var dragging = false

    var body: some View {
        Page {
            PageTitle("Import audio", subtitle: "Recordings and video from this Mac, transcribed into history.")
            ErrorNote(store.error)
            routedNotice
            dropZone
            chosenRows
            queue
        }
    }

    // MARK: - Choosing

    private var dropZone: some View {
        VStack(spacing: 12) {
            Text("Drag and drop, or choose files to import.")
                .bodyText(14, Theme.inkSecondary)
            Button("Choose files") { store.chooseFiles() }
                .buttonStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 36)
        .background(
            RoundedRectangle(cornerRadius: Theme.radiusCard)
                .fill(dragging ? Theme.selection : Theme.surface))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.radiusCard)
                .strokeBorder(
                    dragging ? Theme.accent : Theme.border,
                    style: StrokeStyle(lineWidth: 1, dash: [4, 4])))
        // An OS drop is native here, where the React window needed a webview
        // drag event: same files, one fewer layer.
        .dropDestination(for: URL.self) { urls, _ in
            store.add(paths: urls.map(\.path))
            return true
        } isTargeted: { dragging = $0 }
    }

    @ViewBuilder
    private var chosenRows: some View {
        if !store.rows.isEmpty {
            PageSection("Chosen files") {
                Card {
                    ForEach(store.rows) { row in
                        ImportFileRow(row: row)
                    }
                    CardRow {
                        Text(store.pending == 0
                            ? "Nothing left to import."
                            : "Each file is transcribed on this Mac unless its mode says otherwise.")
                            .bodyText(14, Theme.inkSecondary)
                    } trailing: {
                        Button(importLabel) {
                            Task { await store.runImport() }
                        }
                        .buttonStyle(.primary)
                        .disabled(store.pending == 0 || store.running)
                    }
                }
            }
        }
    }

    private var importLabel: String {
        switch store.pending {
        case 0: "Import files"
        case 1: "Import file"
        default: "Import \(store.pending) files"
        }
    }

    // MARK: - The queue

    @ViewBuilder
    private var queue: some View {
        if !store.jobs.isEmpty {
            PageSection("File imports") {
                Card {
                    if let sentence = store.liveSentence {
                        CardRow {
                            Text(sentence).bodyText(14, Theme.inkSecondary)
                        }
                    }
                    ForEach(store.jobs.reversed()) { job in
                        ImportJobRow(job: job) {
                            Task { await store.cancel(job) }
                        } open: { link in
                            openLink(link)
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var routedNotice: some View {
        if let routed = store.routed {
            Card {
                CardRow {
                    Text(routed.sentence).bodyText()
                } trailing: {
                    HStack(spacing: 12) {
                        Button("Open") {
                            Task { await store.openRouted() }
                        }
                        .buttonStyle(.compact)
                        Button("Dismiss") { store.dismissRouted() }
                            .buttonStyle(.quiet)
                    }
                }
            }
        }
    }
}

/// One chosen file. The name truncates in the middle, so the extension and
/// the digits before it survive at any width.
private struct ImportFileRow: View {
    let row: ImportRow

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(row.name)
                    .bodyText(14)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let failure = row.failure {
                    Text(failure).metaText(Theme.live)
                }
            }
            .help(row.path)
        } trailing: {
            Text(row.state.word)
                .metaText(row.state == .failed ? Theme.live : Theme.inkSecondary)
        }
    }
}

/// One core job: what it is doing, what it became, and the one control that
/// can stop it.
private struct ImportJobRow: View {
    let job: AudioImportJob
    let cancel: () -> Void
    let open: (String) -> Void

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(job.fileName)
                    .bodyText(14)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let sentence = job.sentence {
                    Text(sentence).metaText(Theme.live)
                }
            }
        } trailing: {
            HStack(spacing: 12) {
                Text(job.word)
                    .metaText(job.status == .failed ? Theme.live : Theme.inkSecondary)
                if job.canCancel {
                    Button("Cancel", action: cancel).buttonStyle(.compact)
                } else if let link = job.result?.link {
                    Button("Open") { open(link) }.buttonStyle(.compact)
                }
            }
        }
    }
}

/// The documents Sona was given to read.
///
/// A document is text handed over deliberately, and the list says what Sona
/// holds and where each piece came from. Deleting one takes the revision the
/// list was read at, so a delete decided against a stale list is refused by
/// the core rather than applied to the wrong document.
struct DocumentsView: View {
    let store: DocumentStore

    var body: some View {
        Page {
            PageTitle("Documents", subtitle: "Text you gave Sona to read.") {
                Button("Add document") {
                    Task { await store.importDocument() }
                }
                .buttonStyle(.primary)
                .disabled(store.busy)
            }
            ErrorNote(store.error)
            DocumentList(store: store)
        }
    }
}

/// The list on its own, for a screen that already has a title — a person's
/// page shows their documents under their own heading.
struct DocumentList: View {
    let store: DocumentStore
    var title = "Documents"

    var body: some View {
        PageSection(title) {
            Card {
                if store.documents.isEmpty {
                    CardRow {
                        Text(store.loadFailed
                            ? "Documents couldn't be loaded."
                            : "No documents yet.")
                            .bodyText(14, store.loadFailed ? Theme.live : Theme.inkSecondary)
                    }
                } else {
                    ForEach(store.documents) { document in
                        DocumentRow(document: document, busy: store.busy) {
                            Task { await store.delete(id: document.id) }
                        }
                    }
                }
            }
        }
    }
}

private struct DocumentRow: View {
    let document: DocumentEntry
    let busy: Bool
    let delete: () -> Void

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(document.summary.title).bodyText()
                Text("\(document.summary.sourceName) · \(document.summary.created.relativeDay)")
                    .metaText()
            }
        } trailing: {
            Button("Delete", action: delete)
                .buttonStyle(.quiet)
                .disabled(busy)
        }
    }
}
