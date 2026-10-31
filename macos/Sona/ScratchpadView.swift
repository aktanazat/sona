import SwiftUI

/// The scratchpad: dictations and drafts kept as notes. The list is the page;
/// one note opens in its place, with its words, its pin, its history and the
/// way back.
struct ScratchpadView: View {
    let store: ScratchpadStore
    /// An action another part of the app offers for the open note, such as
    /// sharing it. Nothing shows until one is passed.
    var share: ((ScratchNote) -> Void)? = nil

    var body: some View {
        Group {
            if store.editing {
                ScratchNoteEditorPage(store: store, share: share)
            } else {
                ScratchNoteListPage(store: store)
            }
        }
        .task { await store.start() }
    }
}

// MARK: - The list

private struct ScratchNoteListPage: View {
    let store: ScratchpadStore

    private var searching: Bool {
        !store.activeQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        @Bindable var store = store
        Page {
            PageTitle(
                "Scratchpad",
                subtitle: "Notes you dictate or type. They stay on this Mac, encrypted."
            ) {
                HStack(spacing: 8) {
                    SearchField(prompt: "Search notes", text: $store.query)
                        .frame(width: 216)
                    Button {
                        Task { await store.newNote() }
                    } label: {
                        Image(systemName: "plus")
                    }
                    .buttonStyle(.circle)
                    .help("New note")
                    .accessibilityLabel("New note")
                }
            }
            ErrorNote(store.error)
            rows
        }
    }

    @ViewBuilder private var rows: some View {
        switch store.phase {
        case .loading:
            Card { CardLine("Reading your notes…") }
        case let .failed(message):
            Card {
                CardRow {
                    Text(message).bodyText(14, Theme.inkSecondary)
                } trailing: {
                    Button("Try again") {
                        Task { await store.start() }
                    }
                    .buttonStyle(.compact)
                }
            }
        case .ready:
            if store.notes.isEmpty {
                Card {
                    if searching {
                        CardLine("No notes match “\(store.activeQuery.trimmingCharacters(in: .whitespacesAndNewlines))”.")
                    } else {
                        CardLine("No notes yet.")
                    }
                }
            } else {
                VStack(spacing: 8) {
                    ForEach(store.notes) { note in
                        Card {
                            ScratchNoteRow(note: note) {
                                Task { await store.open(note) }
                            }
                        }
                    }
                }
            }
        }
    }
}

private struct ScratchNoteRow: View {
    let note: ScratchNote
    let open: () -> Void

    var body: some View {
        CardRow(action: open) {
            VStack(alignment: .leading, spacing: 4) {
                Text(note.title)
                    .bodyText()
                    .lineLimit(1)
                if !note.preview.isEmpty {
                    Text(note.preview)
                        .metaText()
                        .lineLimit(1)
                }
            }
        } trailing: {
            HStack(spacing: 10) {
                if note.pinned {
                    Image(systemName: "pin.fill")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(Theme.accent)
                        .accessibilityLabel("Pinned")
                }
                Text(FeedClock.ago(note.updatedAtMs))
                    .metaText()
                    .monospacedDigit()
                    .fixedSize()
            }
        }
    }
}

// MARK: - The open note

private struct ScratchNoteEditorPage: View {
    let store: ScratchpadStore
    let share: ((ScratchNote) -> Void)?

    @FocusState private var editorFocused: Bool
    @State private var confirmingDelete = false
    @State private var showingHistory = false

    private var bodyText: Binding<String> {
        Binding(get: { store.body }, set: { store.typeBody($0) })
    }

    /// One line under the title: where the words stand against the store.
    private var status: String {
        switch store.saveState {
        case .saving:
            "Saving…"
        case .unsaved:
            "Unsaved changes"
        case .blank:
            store.openId == nil
                ? "A blank note is not saved."
                : "A blank note is not saved. The last saved words stay."
        case .clean:
            if let saved = store.saved {
                "Saved \(FeedClock.ago(saved.updatedAtMs))"
            } else {
                "The note saves itself once it has words."
            }
        }
    }

    /// The share action, bound to the saved note; nothing until both exist.
    private var shareAction: (() -> Void)? {
        guard let share, let note = store.saved else { return nil }
        return { share(note) }
    }

    var body: some View {
        Page {
            HStack(alignment: .top, spacing: 8) {
                BackLink(title: "Scratchpad") {
                    Task { await store.close() }
                }
                Spacer(minLength: 12)
                ScratchNoteActions(
                    store: store,
                    share: shareAction,
                    showHistory: { showingHistory = true },
                    confirmDelete: { confirmingDelete = true })
            }
            VStack(alignment: .leading, spacing: 6) {
                Text(store.saved?.title ?? "New note")
                    .heroText()
                    .lineLimit(2)
                    .accessibilityAddTraits(.isHeader)
                Text(status).metaText(Theme.inkSecondary)
            }
            .padding(.top, 8)
            .padding(.bottom, 20)
            ErrorNote(store.error)
            ScratchNoteEditor(text: bodyText, focused: $editorFocused)
        }
        .onAppear { editorFocused = true }
        .onChange(of: store.focusRequest) { _, _ in
            showingHistory = false
            confirmingDelete = false
            editorFocused = true
        }
        .onChange(of: editorFocused) { _, focused in
            if !focused {
                Task { await store.flush() }
            }
        }
        .onDisappear {
            Task { await store.leave() }
        }
        .alert("Are you sure you want to delete this note?", isPresented: $confirmingDelete) {
            Button("Delete", role: .destructive) {
                Task { await store.delete() }
            }
            Button("Keep it", role: .cancel) {}
        }
        .sheet(isPresented: $showingHistory) {
            ScratchVersionHistorySheet(store: store)
        }
    }
}

/// The open note's actions, in one row: the pin, its history, whatever
/// another part of the app offers for it, and the way to delete it. A note
/// that has not been saved yet has nothing to act on.
struct ScratchNoteActions: View {
    let store: ScratchpadStore
    var share: (() -> Void)?
    let showHistory: () -> Void
    let confirmDelete: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            if let saved = store.saved {
                Button {
                    Task { await store.togglePin() }
                } label: {
                    Image(systemName: saved.pinned ? "pin.slash" : "pin")
                }
                .buttonStyle(.circle)
                .help(saved.pinned ? "Unpin" : "Pin")
                .accessibilityLabel(saved.pinned ? "Unpin" : "Pin")
                Button(action: showHistory) {
                    Image(systemName: "clock.arrow.circlepath")
                }
                .buttonStyle(.circle)
                .help("Version history")
                .accessibilityLabel("Version history")
                if let share {
                    Button(action: share) {
                        Image(systemName: "square.and.arrow.up")
                    }
                    .buttonStyle(.circle)
                    .help("Share link…")
                    .accessibilityLabel("Share link…")
                }
                Button(action: confirmDelete) {
                    Image(systemName: "trash")
                }
                .buttonStyle(.circle)
                .help("Delete")
                .accessibilityLabel("Delete")
            }
        }
    }
}

/// The note's words, growing with them: a few lines when empty, a line more
/// for every line written, so the page scrolls and the editor never does.
/// A `TextEditor` is an `NSTextView`, which is where the system's dictation
/// insertion lands while the cursor is in it.
private struct ScratchNoteEditor: View {
    @Binding var text: String
    var focused: FocusState<Bool>.Binding

    var body: some View {
        // The editor cannot size itself to its words; a hidden copy of them
        // can. The copy wraps a little narrower, so it is never the shorter.
        Text(text + " ")
            .font(TypeScale.body())
            .padding(.horizontal, 8)
            .frame(maxWidth: .infinity, minHeight: 240, alignment: .topLeading)
            .padding(.bottom, 8)
            .hidden()
            .overlay(alignment: .topLeading) {
                ZStack(alignment: .topLeading) {
                    if text.isEmpty {
                        Text("Start typing, or dictate.")
                            .font(TypeScale.body())
                            .foregroundStyle(Theme.inkTertiary)
                            .padding(.horizontal, 5)
                    }
                    TextEditor(text: $text)
                        .font(TypeScale.body())
                        .foregroundStyle(Theme.ink)
                        .scrollContentBackground(.hidden)
                        .scrollDisabled(true)
                        .focused(focused)
                        .accessibilityLabel("Note")
                }
            }
            .padding(.horizontal, -5)
    }
}

// MARK: - Version history

/// The note's saved states, newest first, each with the way to put it back.
private struct ScratchVersionHistorySheet: View {
    let store: ScratchpadStore
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Version history").titleText().accessibilityAddTraits(.isHeader)
                    Text("The last 50 saves of this note. Restoring one keeps the words it replaces as the version before it.")
                        .metaText(Theme.inkSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 16)
                Button { dismiss() } label: {
                    Image(systemName: "xmark")
                }
                .buttonStyle(.circle)
                .help("Done")
                .accessibilityLabel("Done")
            }
            ErrorNote(store.versionsError)
            ScrollView {
                Card { rows }
            }
            .scrollIndicators(.never)
        }
        .padding(24)
        .frame(width: 560, height: 520)
        .background(Theme.page)
        .task { await store.loadVersions() }
    }

    @ViewBuilder private var rows: some View {
        if let versions = store.versions {
            if versions.isEmpty {
                CardLine("No saved versions yet.")
            } else {
                ForEach(Array(versions.enumerated()), id: \.element.id) { index, version in
                    ScratchVersionRow(version: version, current: index == 0) {
                        Task { await store.restore(version) }
                    }
                    .disabled(store.restoring)
                }
            }
        } else if store.versionsLoading {
            CardLine("Reading the history…")
        }
    }
}

private struct ScratchVersionRow: View {
    let version: ScratchNoteVersion
    /// The newest version is the note as it is now; nothing to restore.
    let current: Bool
    let restore: () -> Void

    private var when: String {
        let ago = FeedClock.ago(version.savedAtMs)
        return version.kind == .restore ? "Restored \(ago)" : "Saved \(ago)"
    }

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text(when).bodyText(14)
                    if current {
                        Chip("Current")
                    }
                }
                Text(version.body.trimmingCharacters(in: .whitespacesAndNewlines))
                    .metaText()
                    .lineLimit(2)
            }
        } trailing: {
            if !current {
                Button("Restore", action: restore)
                    .buttonStyle(.compact)
            }
        }
    }
}
