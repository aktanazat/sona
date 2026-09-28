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
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Scratchpad").titleText()
                    Text("Notes you dictate or type. They stay on this Mac, encrypted.")
                        .bodyText(15, Theme.inkSecondary)
                }
                Spacer(minLength: 16)
                SearchField(prompt: "Search notes", text: $store.query)
                    .frame(width: 240)
                Button("New note") {
                    Task { await store.newNote() }
                }
                .buttonStyle(.primary)
            }
            .padding(.bottom, 28)
            ErrorNote(store.error)
            Card { rows }
        }
    }

    @ViewBuilder private var rows: some View {
        switch store.phase {
        case .loading:
            CardLine("Reading your notes…")
        case let .failed(message):
            CardRow {
                Text(message).bodyText(15, Theme.inkSecondary)
            } trailing: {
                Button("Try again") {
                    Task { await store.start() }
                }
                .buttonStyle(.compact)
            }
        case .ready:
            if store.notes.isEmpty {
                if searching {
                    CardLine("No notes match “\(store.activeQuery.trimmingCharacters(in: .whitespacesAndNewlines))”.")
                } else {
                    CardLine("No notes yet. Start one with New note, then dictate into it or type.")
                }
            } else {
                ForEach(store.notes) { note in
                    ScratchNoteRow(note: note) {
                        Task { await store.open(note) }
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
            BackLink(title: "Scratchpad") {
                Task { await store.close() }
            }
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 8) {
                    Text(store.saved?.title ?? "New note")
                        .titleText()
                        .lineLimit(2)
                    Text(status).metaText(Theme.inkSecondary)
                }
                Spacer(minLength: 16)
                ScratchNoteActions(
                    store: store,
                    share: shareAction,
                    showHistory: { showingHistory = true },
                    confirmDelete: { confirmingDelete = true })
            }
            .padding(.bottom, 20)
            ErrorNote(store.error)
            Card {
                ScratchNoteEditor(text: bodyText, focused: $editorFocused)
            }
            Text("While the cursor is in this note, dictation lands there.")
                .metaText()
                .padding(.top, 12)
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
                Button(saved.pinned ? "Unpin" : "Pin") {
                    Task { await store.togglePin() }
                }
                .buttonStyle(.compact)
                Button("Version history", action: showHistory)
                    .buttonStyle(.compact)
                if let share {
                    Button("Share link…", action: share)
                        .buttonStyle(.compact)
                }
                Button("Delete", action: confirmDelete)
                    .buttonStyle(QuietButton(color: Theme.live, compact: true))
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
            .padding(.horizontal, 15)
            .padding(.vertical, 16)
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
                    Text("Version history").headlineText()
                    Text("The last 50 saves of this note. Restoring one keeps the words it replaces as the version before it.")
                        .metaText()
                }
                Spacer(minLength: 16)
                Button("Done") { dismiss() }
                    .buttonStyle(.compact)
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
                    Text(when).bodyText(15)
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
