import SwiftUI

/// The scratchpad: dictations and drafts kept as notes. The list is the page;
/// one note opens in its place, with its words, its pin, its history and the
/// way back.
struct ScratchpadView: View {
    let store: ScratchpadStore
    /// An action another part of the app offers for the open note, such as
    /// sharing it. Nothing shows until one is passed.
    var share: ((ScratchNote) -> Void)? = nil
    var openSource: ((String) -> Void)? = nil
    @State private var showingSynthesis = false
    @State private var showingSubjects = false

    var body: some View {
        Group {
            if store.editing {
                ScratchNoteEditorPage(
                    store: store, share: share, openSource: openSource,
                    showSynthesis: { showingSynthesis = true },
                    showSubjects: { showingSubjects = true })
            } else {
                ScratchNoteListPage(store: store, showSynthesis: { showingSynthesis = true },
                    showSubjects: { showingSubjects = true })
            }
        }
        .task { await store.start() }
        .sheet(isPresented: $showingSynthesis) {
            ScratchSourcePicker(store: store, synthesis: true)
        }
        .sheet(isPresented: $showingSubjects) {
            ScratchSubjectsSheet(store: store)
        }
        .sheet(item: Binding(get: { store.viewedVersion }, set: { _ in store.closeVersion() })) { version in
            ScratchReadOnlyVersion(version: version, close: { store.closeVersion() })
        }
        .sheet(item: Binding(get: { store.openedSubject }, set: { _ in store.closeSubject() })) { page in
            ScratchSubjectsSheet(store: store, initialPage: page)
        }
    }
}

// MARK: - The list

private struct ScratchNoteListPage: View {
    let store: ScratchpadStore
    let showSynthesis: () -> Void
    let showSubjects: () -> Void

    private var searching: Bool {
        !store.activeQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        @Bindable var store = store
        Page {
            PageTitle(
                "Notes",
                subtitle: ""
            ) {
                HStack(spacing: 8) {
                    Button("Subjects", action: showSubjects).buttonStyle(.quiet)
                    Button("Review connections", action: showSynthesis)
                        .buttonStyle(.compact)
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
                Card {
                    ForEach(store.notes) { note in
                        ScratchNoteRow(note: note) {
                            Task { await store.open(note) }
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
    let openSource: ((String) -> Void)?
    let showSynthesis: () -> Void
    let showSubjects: () -> Void

    @FocusState private var editorFocused: Bool
    @State private var confirmingDelete = false
    @State private var showingHistory = false
    @State private var showingSources = false
    @State private var captured: ScratchSourceEvidence?
    @State private var sourceError: String?
    @State private var selectedPhoneThought: PhoneThoughtReference?

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
                ? "Not saved"
                : "Blank changes are not saved"
        case .clean:
            if let saved = store.saved {
                "Saved \(FeedClock.ago(saved.updatedAtMs))"
            } else {
                "Not saved"
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
                BackLink(title: "Notes") {
                    Task { await store.close() }
                }
                Spacer(minLength: 12)
                Button("Subjects", action: showSubjects).buttonStyle(.quiet)
                Button("Review connections", action: showSynthesis)
                    .buttonStyle(.compact)
                Button {
                    showingSources = true
                } label: {
                    Image(systemName: "link")
                }
                .buttonStyle(.circle)
                .help("Link a source")
                .accessibilityLabel("Link a source")
                .disabled(store.body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
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
                Text(store.memory?.interpretation == true ? "AI interpretation" : (store.memory?.passages.isEmpty == false ? "Passage note" : "Explicit note")).metaText()
            }
            .padding(.top, 8)
            .padding(.bottom, 20)
            ErrorNote(store.error)
            ErrorNote(sourceError)
            ScratchNoteEditor(text: bodyText, focused: $editorFocused)
            .padding(.bottom, 24)
            connections
        }
        .onAppear { editorFocused = true }
        .onChange(of: store.evidenceRevision) { _, _ in
            captured = nil
            sourceError = nil
        }
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
        .sheet(isPresented: $showingSources) {
            ScratchSourcePicker(store: store, synthesis: false)
        }
        .sheet(item: $captured) { evidence in
            VStack(alignment: .leading, spacing: 16) {
                HStack {
                    Text(evidence.source.title).titleText().lineLimit(2)
                    Spacer()
                    Button("Open original") {
                        captured = nil
                        openOriginal(evidence.source)
                    }.buttonStyle(.compact)
                    Button("Done") { captured = nil }.buttonStyle(.quiet)
                }
                Text("Captured source").metaText()
                ScrollView {
                    Text(evidence.text).bodyText().textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .padding(24)
            .frame(width: 560, height: 520)
            .background(Theme.page)
        }
        .sheet(item: $selectedPhoneThought) { reference in
            PhoneThoughtSourceSheet(reference: reference)
        }
    }

    @ViewBuilder private var connections: some View {
        if store.memoryLoading {
            Text("Reading sources…").metaText()
        }
        if let memory = store.memory {
            if !memory.passages.isEmpty {
                PageSection("Saved passage") {
                    Card {
                        ForEach(memory.passages) { passage in
                            CardRow {
                                Text(passage.wording == "raw" ? "Direct quote" : "Corrected wording").metaText()
                                if let source = memory.sources.first(where: { $0.kind == "meeting" && $0.id == passage.meetingId }) {
                                    Button(source.title) { navigate(source) }.buttonStyle(.quiet)
                                }
                            } trailing: {
                                Button("Open moment") { openSource?(passage.link) }.buttonStyle(.compact)
                            }
                        }
                    }
                }
            }
            if !memory.phoneSources.isEmpty {
                PageSection("Original iPhone thoughts") {
                    Card {
                        ForEach(memory.phoneSources) { reference in
                            CardRow {
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(reference.origin.capitalized + " thought").bodyText(13)
                                    Text(reference.sourceDeleted
                                        ? "Original removed on iPhone · note kept"
                                        : "Captured \(FeedClock.ago(reference.capturedAtUtcMs))")
                                        .metaText()
                                }
                            } trailing: {
                                if !reference.sourceDeleted {
                                    Button("Open original") { selectedPhoneThought = reference }
                                        .buttonStyle(.compact)
                                }
                            }
                        }
                    }
                }
            }
            if !memory.sources.isEmpty {
                PageSection("Sources") {
                    Card {
                        ForEach(memory.sources, id: \.key) { source in
                            CardRow {
                                Button(source.title) { navigate(source) }
                                    .buttonStyle(.quiet)
                                    .disabled(!source.available)
                                    .lineLimit(2)
                            } trailing: {
                                HStack(spacing: 12) {
                                    Text(source.kind == "meeting" ? "Conversation" : source.kind.capitalized)
                                        .metaText()
                                    Button {
                                        Task { await store.unlink(source) }
                                    } label: {
                                        Image(systemName: "xmark")
                                    }
                                    .buttonStyle(.circle)
                                    .help("Remove link")
                                    .accessibilityLabel("Remove link to \(source.title)")
                                }
                            }
                        }
                    }
                }
            }
            if !memory.relatedNotes.isEmpty {
                PageSection("Related notes") {
                    Card {
                        ForEach(memory.relatedNotes) { note in
                            CardRow(action: { Task { await store.open(note) } }) {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(note.title).bodyText().lineLimit(2)
                                    if let connection = memory.connections.first(where: { $0.noteId == note.id }) {
                                        Text(connection.reasons.joined(separator: " · ")).metaText()
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    private func navigate(_ source: ScratchSource) {
        Task {
            guard await store.flush(), !store.hasPendingSave else { return }
            do {
                if let evidence = try await store.capturedSource(source) {
                    captured = evidence
                } else {
                    openOriginal(source)
                }
                sourceError = nil
            } catch {
                sourceError = error.localizedDescription
            }
        }
    }

    private func openOriginal(_ source: ScratchSource) {
        if source.kind == "note" {
            Task { await store.open(id: source.id) }
        } else {
            openSource?(source.link)
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
                        Text("Note")
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

private struct ScratchSourcePicker: View {
    let store: ScratchpadStore
    let synthesis: Bool
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var period = "selection"
    @State private var periodSources: [ScratchSourceCandidate] = []
    @State private var hasMore = false
    @State private var results: [ScratchSourceCandidate] = []
    @State private var chosen: [String: ScratchSourceCandidate] = [:]
    @State private var reading = false
    @State private var busy = false
    @State private var error: String?
    @State private var draft: ScratchSynthesisDraft?

    private var candidates: [ScratchSourceCandidate] {
        let rows = !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? results : (period == "selection" ? store.sourceCandidates : periodSources)
        return rows.filter {
            synthesis ? $0.kind != "person" : !($0.kind == "note" && $0.id == store.openId)
        }
    }

    var body: some View {
        Group {
            if let draft {
                ScratchSynthesisReview(store: store, initial: draft,
                    reject: { self.draft = nil }, accepted: { dismiss() })
            } else {
                selection
            }
        }
        .padding(24)
        .frame(minWidth: 560, idealWidth: 660, minHeight: 520, idealHeight: 680)
        .background(Theme.page)
        .interactiveDismissDisabled(busy)
        .onChange(of: store.evidenceRevision) { _, _ in
            if draft != nil {
                draft = nil
                error = "A source changed. Prepare the note again."
            }
        }
        .onAppear {
            if synthesis, let note = store.saved {
                let source = ScratchSourceCandidate(note: note)
                chosen[source.key] = source
            }
        }
        .task(id: period) {
            guard synthesis, period != "selection" else { return }
            reading = true
            do {
                let page = try await store.reviewCandidates(period: period)
                guard !Task.isCancelled else { return }
                periodSources = page.sources.map(ScratchSourceCandidate.init(source:))
                hasMore = page.hasMore
                error = page.conversationsUnavailable ? "Conversations are unavailable. Try again." : nil
            } catch {
                guard !Task.isCancelled else { return }
                self.error = error.localizedDescription
                periodSources = []
            }
            reading = false
        }
        .task(id: query) {
            let searched = query.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !searched.isEmpty else { reading = false; results = []; return }
            reading = true
            do {
                try await Task.sleep(for: .milliseconds(200))
                let found = try await store.searchSources(searched)
                guard !Task.isCancelled else { return }
                results = found
                error = nil
                reading = false
            } catch is CancellationError {
            } catch {
                guard !Task.isCancelled else { return }
                self.error = error.localizedDescription
                reading = false
            }
        }
    }

    private var selection: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text(synthesis ? "Connections review" : "Link a source").titleText()
                Spacer()
                Button("Cancel") { dismiss() }.buttonStyle(.quiet).disabled(busy)
                if synthesis {
                    Button("Write note") { prepare(manual: true) }.buttonStyle(.compact)
                        .disabled(busy || !(2...12).contains(chosen.count))
                    Button(busy ? "Preparing…" : "Review with AI") { prepare(manual: false) }
                        .buttonStyle(.primary).disabled(busy || !(2...12).contains(chosen.count))
                }
            }
            if synthesis {
                Picker("Review period", selection: $period) {
                    Text("Selected records").tag("selection")
                    Text("Today").tag("day")
                    Text("This week").tag("week")
                }.pickerStyle(.segmented).disabled(busy)
            }
            SearchField(prompt: synthesis ? "Search notes and conversations" : "Search sources", text: $query)
                .disabled(busy)
            ErrorNote(error)
            if synthesis {
                HStack {
                    Text("\(chosen.count) of 2–12 selected").metaText()
                    if !chosen.isEmpty {
                        Button("Clear") { chosen.removeAll() }.buttonStyle(.quiet).disabled(busy)
                    }
                    if hasMore && period != "selection" { Text("More records available in search").metaText() }
                }
                if !chosen.isEmpty {
                    ScrollView(.horizontal) {
                        HStack {
                            ForEach(chosen.values.sorted { $0.key < $1.key }, id: \.key) { source in
                                Button { chosen.removeValue(forKey: source.key) } label: {
                                    Label(source.title, systemImage: "xmark").lineLimit(1)
                                }.buttonStyle(.compact).disabled(busy)
                            }
                        }
                    }
                }
            }
            ScrollView {
                Card {
                    if reading { CardLine("Reading sources…") }
                    else if candidates.isEmpty { CardLine("No matching sources.") }
                    else {
                        ForEach(candidates, id: \.key) { candidate in
                            CardRow(action: { select(candidate) }) {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(candidate.title).bodyText().lineLimit(2)
                                    if let connection = store.memory?.connections.first(where: { $0.noteId == candidate.id && candidate.kind == "note" }) {
                                        Text(connection.reasons.joined(separator: " · ")).metaText()
                                    }
                                }
                            } trailing: {
                                Text(candidate.kind == "meeting" ? "Conversation" : candidate.kind.capitalized).metaText()
                                if chosen[candidate.key] != nil {
                                    Image(systemName: "checkmark").foregroundStyle(Theme.accent)
                                        .accessibilityLabel("Selected")
                                }
                            }.disabled(busy)
                        }
                    }
                }
            }
        }
    }

    private func prepare(manual: Bool) {
        Task {
            busy = true
            defer { busy = false }
            let targets = chosen.values.sorted { $0.key < $1.key }.map(\.target)
            do {
                if manual { draft = try await store.manualReview(targets) }
                else { draft = try await store.prepareSynthesis(targets) }
                error = nil
            } catch { self.error = error.localizedDescription }
        }
    }

    private func select(_ candidate: ScratchSourceCandidate) {
        if synthesis {
            if chosen.removeValue(forKey: candidate.key) == nil { chosen[candidate.key] = candidate }
        } else {
            Task {
                busy = true
                if await store.link(candidate.target) { dismiss() } else { error = store.error }
                busy = false
            }
        }
    }
}

private struct ScratchSynthesisReview: View {
    let store: ScratchpadStore
    let initial: ScratchSynthesisDraft
    let reject: () -> Void
    let accepted: () -> Void
    @State private var bodyText = ""
    @State private var sourceIndex = 0
    @State private var differences = false
    @State private var saving = false
    @State private var error: String?

    private var selected: ScratchReviewSource? {
        initial.sources.indices.contains(sourceIndex) ? initial.sources[sourceIndex] : nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text(initial.authorship == "interpretation" ? "Review interpretation" : "New cited note").titleText()
                Spacer()
                Button("Reject", action: reject).buttonStyle(.quiet).disabled(saving)
                Button(saving ? "Saving…" : "Accept as new note") {
                    Task {
                        saving = true
                        let draft = ScratchSynthesisDraft(body: bodyText, sources: initial.sources, authorship: initial.authorship)
                        if await store.acceptSynthesis(draft) { accepted() } else { error = store.error }
                        saving = false
                    }
                }.buttonStyle(.primary)
                    .disabled(saving || bodyText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            ErrorNote(error)
            Text(initial.authorship == "interpretation" ? "AI interpretation" : "Explicit note").metaText()
            TextEditor(text: $bodyText).font(TypeScale.body()).foregroundStyle(Theme.ink)
                .scrollContentBackground(.hidden).frame(minHeight: 180)
                .accessibilityLabel("New note")
            Hairline()
            HStack {
                Picker("Original source", selection: $sourceIndex) {
                    ForEach(Array(initial.sources.enumerated()), id: \.offset) { index, item in
                        Text("[\(index + 1)] \(item.source.title)").tag(index)
                    }
                }
                Toggle("Differences", isOn: $differences).toggleStyle(.checkbox)
            }
            if let selected {
                Text(selected.source.kind == "meeting" ? "Transcript quotes and explicit meeting notes" : "Saved note at review").metaText()
                ScrollView {
                    if differences {
                        ScratchTextDifferences(original: selected.text, proposed: bodyText)
                    } else {
                        Text(selected.text).bodyText().textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }.frame(minHeight: 180)
            }
        }
        .onAppear { bodyText = initial.body }
    }
}

private struct ScratchTextDifferences: View {
    let original: String
    let proposed: String

    private var changes: [CollectionDifference<String>.Change] {
        let before = original.components(separatedBy: .newlines)
        let after = proposed.components(separatedBy: .newlines)
        return Array(after.difference(from: before))
    }

    var body: some View {
        LazyVStack(alignment: .leading, spacing: 8) {
            if changes.isEmpty { Text("No text differences.").metaText() }
            ForEach(Array(changes.enumerated()), id: \.offset) { _, change in
                switch change {
                case let .remove(_, text, _):
                    Text("− " + text).bodyText(14, Theme.live).strikethrough().textSelection(.enabled)
                case let .insert(_, text, _):
                    Text("+ " + text).bodyText(14, Theme.accent).textSelection(.enabled)
                }
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct ScratchPassageSheet: View {
    let store: ScratchpadStore
    let meetingId: String
    let segmentId: String
    let excerpt: String
    var personId: String? = nil
    @Environment(\.dismiss) private var dismiss
    @State private var note = ""
    @State private var personQuery = ""
    @State private var quote = ""
    @State private var people: [ScratchSourceCandidate] = []
    @State private var selectedPerson: ScratchSourceCandidate?
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("Save passage").titleText()
                Spacer()
                Button("Cancel") { dismiss() }.buttonStyle(.quiet).disabled(busy)
                Button(busy ? "Saving…" : "Save passage") {
                    Task {
                        busy = true
                        if await store.capturePassage(meetingId: meetingId, segmentId: segmentId,
                            excerpt: quote, personId: selectedPerson?.id ?? personId, body: note) {
                            dismiss()
                        } else { error = store.error }
                        busy = false
                    }
                }.buttonStyle(.primary).disabled(busy || quote.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            ErrorNote(error)
            Text("Selected passage").metaText()
            TextEditor(text: $quote).font(TypeScale.body()).scrollContentBackground(.hidden)
                .accessibilityLabel("Selected passage").frame(maxHeight: 160)
            Text("Note (optional)").metaText()
            TextEditor(text: $note).font(TypeScale.body()).scrollContentBackground(.hidden)
                .accessibilityLabel("Note about this passage").frame(minHeight: 150)
            if let selectedPerson {
                HStack {
                    Text(selectedPerson.title).bodyText()
                    Button("Remove person") { self.selectedPerson = nil; personQuery = "" }.buttonStyle(.quiet)
                }
            } else if personId != nil {
                Text("Person linked").metaText()
            } else {
                SearchField(prompt: "Link a person", text: $personQuery)
                ForEach(people, id: \.key) { person in
                    Button(person.title) { selectedPerson = person }.buttonStyle(.quiet)
                }
            }
        }
        .padding(24).frame(width: 560, height: 540).background(Theme.page)
        .interactiveDismissDisabled(busy)
        .onAppear { quote = excerpt }
        .task(id: personQuery) {
            guard !personQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { people = []; return }
            do {
                try await Task.sleep(for: .milliseconds(200))
                let found = try await store.searchSources(personQuery)
                guard !Task.isCancelled else { return }
                people = found.filter { $0.kind == "person" }
            } catch is CancellationError {
            } catch { self.error = error.localizedDescription }
        }
    }
}

private struct ScratchReadOnlyVersion: View {
    let version: ScratchNoteVersion
    let close: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("Saved version").titleText()
                Spacer()
                Button("Return to current", action: close).buttonStyle(.compact)
            }
            Text(Date(timeIntervalSince1970: Double(version.savedAtMs) / 1000).formatted(date: .abbreviated, time: .standard)).metaText()
            Text(version.kind == .synthesis ? "AI interpretation" : "Saved note").metaText()
            ScrollView {
                Text(version.body).bodyText().textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }.padding(24).frame(width: 600, height: 540).background(Theme.page)
    }
}

private struct ScratchSubjectsSheet: View {
    let store: ScratchpadStore
    var initialPage: ScratchSubjectPage? = nil
    @Environment(\.dismiss) private var dismiss
    @State private var subjects: [ScratchSubject] = []
    @State private var page: ScratchSubjectPage?
    @State private var title = ""
    @State private var error: String?
    @State private var busy = false
    @State private var confirmingDelete = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                if page != nil {
                    Button("Subjects") { page = nil }.buttonStyle(.quiet)
                }
                Text(page?.subject.title ?? "Subjects").titleText()
                Spacer()
                if page != nil {
                    Button("Delete subject") { confirmingDelete = true }.buttonStyle(.quiet)
                }
                Button("Done") { dismiss() }.buttonStyle(.compact)
            }
            ErrorNote(error)
            if let page {
                if let saved = store.saved {
                    HStack {
                        Button("Add open note") { update(page.subject.id, noteId: saved.id, action: "add") }.buttonStyle(.compact)
                        Button("Use open note as current") { update(page.subject.id, noteId: saved.id, action: "set_current") }.buttonStyle(.compact)
                    }
                }
                ScrollView {
                    VStack(alignment: .leading, spacing: 20) {
                        Text("Current understanding").sectionLabel()
                        if let current = page.current {
                            Button(current.title) { open(current) }.buttonStyle(.quiet)
                            Text(current.body).bodyText().textSelection(.enabled)
                        } else { Text("No current note.").metaText() }
                        Text("History").sectionLabel()
                        Card {
                            if page.history.isEmpty { CardLine("No linked notes.") }
                            ForEach(page.history) { note in
                                CardRow {
                                    VStack(alignment: .leading, spacing: 4) {
                                        Button(note.title) { open(note) }.buttonStyle(.quiet)
                                        Text(FeedClock.ago(note.updatedAtMs)).metaText()
                                    }
                                } trailing: {
                                    Button("Set current") { update(page.subject.id, noteId: note.id, action: "set_current") }
                                        .buttonStyle(.quiet).disabled(note.id == page.subject.currentNoteId)
                                    Button("Remove") { update(page.subject.id, noteId: note.id, action: "remove") }.buttonStyle(.quiet)
                                }
                            }
                        }
                    }
                }
            } else {
                HStack {
                    InputField(prompt: "Subject name", text: $title)
                    Button("Create") {
                        Task {
                            busy = true
                            defer { busy = false }
                            do {
                                let subject = try await store.createSubject(title: title)
                                title = ""
                                subjects = try await store.subjects()
                                page = try await store.subject(id: subject.id)
                                error = nil
                            } catch { self.error = error.localizedDescription }
                        }
                    }.buttonStyle(.compact).disabled(title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
                ScrollView {
                    Card {
                        if subjects.isEmpty { CardLine("No subjects yet.") }
                        ForEach(subjects) { subject in
                            CardRow(action: {
                                Task {
                                    do { page = try await store.subject(id: subject.id); error = nil }
                                    catch { self.error = error.localizedDescription }
                                }
                            }) { Text(subject.title).bodyText() }
                        }
                    }
                }
            }
        }
        .padding(24).frame(width: 640, height: 620).background(Theme.page)
        .disabled(busy)
        .task {
            if page == nil { page = initialPage }
            do { subjects = try await store.subjects() }
            catch { self.error = error.localizedDescription }
        }
        .onChange(of: store.notes) { _, _ in
            guard let id = page?.subject.id else { return }
            Task {
                do { page = try await store.subject(id: id) }
                catch { self.error = error.localizedDescription }
            }
        }
        .alert("Delete this subject?", isPresented: $confirmingDelete) {
            Button("Delete", role: .destructive) {
                guard let page else { return }
                Task {
                    do {
                        try await store.deleteSubject(id: page.subject.id)
                        self.page = nil
                        subjects = try await store.subjects()
                        error = nil
                    } catch { self.error = error.localizedDescription }
                }
            }
            Button("Keep", role: .cancel) {}
        } message: { Text("Linked notes are kept.") }
    }

    private func update(_ id: String, noteId: String, action: String) {
        Task {
            busy = true
            defer { busy = false }
            do { page = try await store.subjectNote(id: id, noteId: noteId, action: action); error = nil }
            catch { self.error = error.localizedDescription }
        }
    }

    private func open(_ note: ScratchNote) {
        Task {
            await store.open(note)
            if store.openId == note.id { dismiss() }
        }
    }
}
