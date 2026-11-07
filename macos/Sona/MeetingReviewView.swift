import AppKit
import SwiftUI

/// One recorded meeting, read three ways: the words, what Sona made of them,
/// and the ledger a reader can check against quotes.
struct MeetingReviewView: View {
    let store: MeetingsStore
    let settings: MeetingSettingsStore
    var openPerson: (String) -> Void = { _ in }
    var openMeetingSettings: () -> Void = {}
    /// Opens the agent with a question about this meeting begun, for the
    /// person to finish and send.
    var askAgent: ((String) -> Void)?
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var confirmingDelete = false

    var body: some View {
        ScrollViewReader { proxy in
            Page(back: PageBack("Meetings") { store.closeReview() }) {
                if let snapshot = store.snapshot {
                    ConversationContextView(
                        sessionId: snapshot.session.sessionId, editable: store.editable,
                        partialRecording: snapshot.session.captureCompleteness == .partial)
                    ConversationPlaceCard(core: model.core, store: store,
                        sessionId: snapshot.session.sessionId, revision: snapshot.session.revision)
                    ConversationBookmarksView(core: model.core, store: store,
                        sessionId: snapshot.session.sessionId, revision: snapshot.session.revision)
                    HeardPlansCard(core: model.core, store: store,
                        sessionId: snapshot.session.sessionId, revision: snapshot.session.revision)
                    pane(snapshot)
                } else {
                    if let failure = store.reviewFailure {
                        unread(failure)
                    } else if store.reviewLoading {
                        Text("Reading the meeting…").bodyText(14, Theme.inkSecondary)
                    } else {
                        Text("That meeting is no longer here.").bodyText(14, Theme.inkSecondary)
                    }
                }
            } pinned: {
                if let snapshot = store.snapshot {
                    MeetingReviewHeader(
                        store: store, snapshot: snapshot, delete: { confirmingDelete = true },
                        openMeetingSettings: openMeetingSettings, askAgent: askAgent)
                    MeetingReviewTabs(store: store)
                }
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                VStack(spacing: 12) {
                    toast
                    ConversationAudioBar(audio: store.audio, floating: model.about.material)
                }
                .padding(.bottom, 20)
            }
            .sheet(isPresented: followUpPresented) { FollowUpSheet(store: store) }
            .sheet(isPresented: catchUpPresented) { CatchUpSheet(store: store) }
            .sheet(isPresented: Binding(get: { store.filing != nil }, set: { if !$0 { store.closeFiling() } })) {
                MeetingFilingSheet(store: store)
            }
            .relistenSheet(store.relisten)
            .alert("Delete this meeting?", isPresented: $confirmingDelete) {
                Button("Delete", role: .destructive) { store.deleteOpenMeeting() }
                Button("Keep it", role: .cancel) {}
            } message: {
                Text("It goes to the trash for 30 days, then Sona removes it for good.")
            }
            .onDisappear { store.audio.stop() }
            .onChange(of: store.snapshot?.session.sessionId) { _, _ in
                guard let noteId = store.focusedNoteId,
                      store.snapshot?.notes.contains(where: { $0.noteId == noteId }) == true else { return }
                withAnimation(reduceMotion ? nil : .easeOut(duration: 0.25)) {
                    proxy.scrollTo("meeting-note:\(noteId)", anchor: .center)
                }
            }
        }
    }

    /// What just worked floats at the foot of the window and leaves by
    /// itself, so the page under it never moves. Without motion it only
    /// fades.
    private var toast: some View {
        ZStack {
            if let notice = store.notice {
                MeetingReviewToast(store: store, notice: notice, material: model.about.material)
                    .transition(
                        reduceMotion
                            ? AnyTransition.opacity
                            : AnyTransition.opacity.combined(with: .move(edge: .bottom)))
            }
        }
        .frame(maxWidth: 560)
        .animation(.easeOut(duration: 0.2), value: store.notice)
    }

    @ViewBuilder
    private func pane(_ snapshot: MeetingReviewSnapshot) -> some View {
        switch store.tab {
        case .review:
            ConversationAfterCaptureView(store: store, snapshot: snapshot, core: model.core)
        case .transcript:
            TranscriptPane(store: store, snapshot: snapshot)
        case .insights:
            MeetingInsightsPane(
                store: store, settings: settings, snapshot: snapshot,
                openPerson: openPerson, openMeetingSettings: openMeetingSettings)
        case .ledger:
            LedgerPane(store: store, snapshot: snapshot, openPerson: openPerson)
        }
    }

    /// The first read failed: the reason, and the way to read again. The
    /// meeting is still there; only this read of it is not.
    private func unread(_ failure: String) -> some View {
        Card {
            CardRow {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Sona could not read this meeting.").bodyText()
                    Text(failure).metaText(Theme.inkSecondary)
                }
            } trailing: {
                Button("Try again") { store.retryReview() }
                    .buttonStyle(SecondaryButton(compact: true))
            }
        }
    }

    private var followUpPresented: Binding<Bool> {
        Binding(get: { store.followUpOpen }, set: { if !$0 { store.closeFollowUp() } })
    }

    private var catchUpPresented: Binding<Bool> {
        Binding(get: { store.catchUp != nil }, set: { if !$0 { store.dismissCatchUp() } })
    }
}

/// What just worked, said once: an export, a written ledger and the way to
/// find it. It stays while the pointer rests on it and leaves four seconds
/// after.
private struct MeetingReviewToast: View {
    let store: MeetingsStore
    let notice: String
    let material: AppearanceMaterial
    @State private var hovering = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 16) {
            Text(notice)
                .font(TypeScale.body(13))
                .foregroundStyle(Theme.ink)
                .lineLimit(2)
                .truncationMode(.middle)
            if store.savedLedgerPath != nil {
                Button("Show in Finder") {
                    store.openSavedLedger()
                    store.dismissNotice()
                }
                .buttonStyle(QuietButton(color: Theme.accent, compact: true))
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 12)
        .floatingControl(material)
        .onHover { hovering = $0 }
        .task(id: hovering ? nil : notice) {
            guard !hovering else { return }
            try? await Task.sleep(for: .seconds(4))
            guard !Task.isCancelled else { return }
            store.dismissNotice()
            store.dismissSavedLedger()
        }
    }
}

// MARK: - The head of the page

/// The title a person can correct, the facts of the recording, one line for
/// whatever needs saying, and the menu of everything else this meeting
/// allows.
struct MeetingReviewHeader: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot
    let delete: () -> Void
    var openMeetingSettings: () -> Void = {}
    var askAgent: ((String) -> Void)?
    @State private var editing = false
    @State private var draft = ""
    /// The line that confirms a copy, cleared after a moment.
    @State private var copied: String?
    @State private var copyGeneration = 0

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .center, spacing: 12) {
                title
                Spacer(minLength: 12)
                actions
                    .fixedSize(horizontal: true, vertical: false)
            }
            .frame(height: 36)
            HStack(spacing: 6) {
                Text(facts).metaText(Theme.inkSecondary)
                if update == nil, let receipt = snapshot.currentArtifact?.receipt {
                    Text("·").metaText(Theme.inkTertiary)
                    Text(receipt.line).metaText(Theme.inkTertiary).help(receipt.line)
                }
            }
            .lineLimit(1)
            ZStack(alignment: .leading) {
                Text(" ").metaText().hidden()
                status
            }
            .lineLimit(1)
        }
        .padding(.bottom, 16)
    }

    @ViewBuilder
    private var title: some View {
        if editing {
            HStack(spacing: 8) {
                InputField(prompt: "Meeting title", text: $draft)
                    .frame(maxWidth: 420)
                    .onSubmit(commit)
                Button("Save", action: commit).buttonStyle(.compact)
                Button("Cancel") { editing = false }.buttonStyle(.quiet)
            }
        } else {
            Button { start() } label: {
                Text(snapshot.session.title)
                    .heroText()
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            .help(snapshot.session.title)
            .buttonStyle(.plain)
            .disabled(!store.editable)
            .accessibilityHint(store.editable ? "Edit the title" : "")
            .accessibilityAddTraits(.isHeader)
        }
    }

    /// What is under way, the one action worth a button, and the menu that
    /// holds the rest, the agent first.
    private var actions: some View {
        HStack(spacing: 8) {
            if let doing = store.pending ?? (store.catchingUp ? "Catching up" : nil) {
                Text("\(doing)…").metaText(Theme.inkSecondary)
            }
            if let copied {
                Text(copied).metaText(Theme.inkSecondary)
                    .task(id: copyGeneration) {
                        try? await Task.sleep(for: .seconds(2))
                        guard !Task.isCancelled else { return }
                        self.copied = nil
                    }
            }
            if store.hasLedger {
                Button("Follow up") { store.openFollowUp() }
                    .buttonStyle(.compact)
            }
            if store.canExport {
                MeetingSendToMenu(sessionId: snapshot.session.sessionId)
                    .menuStyle(.button)
                    .buttonStyle(.compact)
                    .fixedSize()
            }
            Menu {
                Button("Add to folder…") {
                    store.openFiling(snapshot.session.sessionId, title: snapshot.session.title)
                }
                Divider()
                if let askAgent {
                    Button("Ask about this meeting") { askAgent(question) }
                    Divider()
                }
                if store.editable {
                    Button("Rename", action: start)
                }
                if store.canRegenerate || store.transcriptDeleted {
                    Button("Write the notes again") { store.regenerate() }
                        .disabled(store.transcriptDeleted)
                        .help(store.transcriptDeleted ? MeetingCommandError.transcriptDeleted.label : "Write new notes")
                }
                Button("Catch me up") { store.runCatchUp() }
                    .disabled(store.catchingUp)
                Divider()
                Button("Copy notes") { copyNotes(asMarkdown: false) }
                    .disabled(!MeetingNotesCopy.hasNotes(snapshot, userNotes: store.notesBody))
                Button("Copy notes as Markdown") { copyNotes(asMarkdown: true) }
                    .disabled(!MeetingNotesCopy.hasNotes(snapshot, userNotes: store.notesBody))
                if store.canExport || store.hasLedger {
                    Divider()
                }
                if store.canExport {
                    Button("Export as Markdown") { store.exportOpenMeeting(.markdown) }
                    Button("Export as JSON") { store.exportOpenMeeting(.json) }
                }
                if store.hasLedger {
                    Button("Save the ledger as HTML") { store.exportOpenLedger() }
                }
                if let receipt = store.receiptLine {
                    Divider()
                    Text(receipt)
                }
                if store.canDelete {
                    Divider()
                    Button("Delete", role: .destructive, action: delete)
                }
            } label: {
                Image(systemName: "ellipsis")
            }
            .menuStyle(.button)
            .buttonStyle(.circle)
            .menuIndicator(.hidden)
            .fixedSize()
            .disabled(store.busy)
            .help("Meeting options")
            .accessibilityLabel("Meeting options")
        }
    }

    private func copyNotes(asMarkdown: Bool) {
        guard MeetingNotesCopy.copy(snapshot, userNotes: store.notesBody, asMarkdown: asMarkdown) else {
            return
        }
        copied = asMarkdown ? "Markdown copied" : "Notes copied"
        copyGeneration += 1
    }

    /// The start of a question for the agent: this meeting by name and by
    /// link, left open for the person to finish.
    private var question: String {
        "About “\(snapshot.session.title)” (sona://meeting/\(snapshot.session.sessionId)): "
    }

    /// At most one line under the facts: what the core refused, else a deleted
    /// transcript, else a remote run that has not stopped, else the run under
    /// way, else deleted audio, else what became of the notes and the voices.
    /// Each carries the one thing to do about it.
    @ViewBuilder
    private var status: some View {
        if let error = store.error {
            statusLine(AttributedString(error), color: Theme.live) {
                Button("Dismiss") { store.dismissError() }
                    .buttonStyle(QuietButton(compact: true))
            }
        } else if store.transcriptDeleted {
            statusLine(AttributedString(MeetingCommandError.transcriptDeleted.label + " Your notes are kept.")) {
                EmptyView()
            }
        } else if snapshot.remoteCancellationPending {
            statusLine(AttributedString("Sona asked the remote engine to stop, and it has not answered yet.")) {
                if store.canCancelRemote {
                    Button("Cancel the remote run") { store.cancelRemote() }
                        .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                        .disabled(store.busy)
                }
            }
        } else if let update {
            MeetingProcessingLine(update: update)
        } else if store.audioDeleted {
            statusLine(AttributedString("Audio deleted by your retention setting. Words and notes are kept.")) {
                EmptyView()
            }
        } else if let standing {
            statusLine(standing) {
                if processing.offersSettings {
                    Button("Open Settings", action: openMeetingSettings)
                        .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                }
                if processing.isFailed, store.canRegenerate {
                    Button("Try again") { store.regenerate() }
                        .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                        .disabled(store.busy)
                }
            }
        }
    }

    private func statusLine<Actions: View>(
        _ text: AttributedString, color: Color = Theme.inkSecondary, @ViewBuilder actions: () -> Actions
    ) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 14) {
            Text(text)
                .metaText(color)
                .textSelection(.enabled)
                .help(String(text.characters))
            actions()
        }
    }

    private var processing: MeetingProcessingStatus { snapshot.session.processingStatus }

    private var update: MeetingProcessingUpdate? {
        store.processingUpdate(snapshot.session.sessionId, phase: snapshot.session.phase)
    }

    /// "Notes failed: the model did not answer · Separating speakers
    /// failed · 12 lines have no speaker yet". Only the failure's own word
    /// takes live red.
    private var standing: AttributedString? {
        var parts: [AttributedString] = []
        if let notes { parts.append(notes) }
        if let voices { parts.append(AttributedString(voices)) }
        if let unattributed { parts.append(AttributedString(unattributed)) }
        guard var line = parts.first else { return nil }
        for part in parts.dropFirst() {
            line.append(AttributedString(" · "))
            line.append(part)
        }
        return line
    }

    /// What became of the notes, when they are not simply written.
    private var notes: AttributedString? {
        switch processing {
        case .succeeded:
            return nil
        case .pending, .running:
            return AttributedString("\(processing.label)…")
        case .cancelled:
            return AttributedString(processing.label)
        case let .failed(reason, cause):
            var line = AttributedString("Notes failed")
            line.foregroundColor = Theme.live
            line.append(AttributedString(": \(Self.why(reason, cause))"))
            return line
        }
    }

    /// The rest of "Notes failed: …".
    private static func why(_ reason: MeetingProcessingFailure, _ cause: MeetingEngineFailureCause?) -> String {
        switch reason {
        case .engineFailure: cause?.label ?? "the engine failed"
        case .localModelUnavailable: "the chosen engine was not ready"
        case .remoteUnavailable: "your server was not reachable"
        case .cancelled: "the run was cancelled"
        case .interrupted: "Sona closed before they were written"
        case .transcriptDeleted: "the transcript was deleted by your retention settings"
        }
    }

    /// Where telling the voices apart stands, when it is not simply done.
    private var voices: String? {
        let separation = snapshot.diarization.status
        switch separation {
        case .notRequested, .succeeded: return nil
        case .downloading, .running: return "\(separation.label)…"
        case .modelUnavailable, .failed: return separation.label
        }
    }

    /// The lines Sona heard but could not put a voice to.
    private var unattributed: String? {
        let count = snapshot.transcript.filter { $0.speakerAssignment == .unknown && !$0.removed }.count
        guard count > 0 else { return nil }
        return count == 1 ? "1 line has no speaker yet" : "\(count) lines have no speaker yet"
    }

    private var facts: String {
        let started = snapshot.session.startedAtUtcMs?.meetingDate
        var parts: [String] = [started.map { "\($0.short) at \($0.time)" } ?? "Never started"]
        if let elapsed = snapshot.session.elapsedOffsetNs {
            parts.append((TimeInterval(elapsed) / 1_000_000_000).spoken)
        }
        return parts.joined(separator: " · ")
    }

    private func start() {
        draft = snapshot.session.title
        editing = true
    }

    /// Blank or unchanged is a person changing their mind, not a write.
    private func commit() {
        let next = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        editing = false
        guard !next.isEmpty, next != snapshot.session.title else { return }
        store.setTitle(next)
    }
}

/// The readings as one segmented control, and the transcript's search at the
/// end of the row. The transcript's controls hold their place on every tab,
/// so the row is one size wherever it is and switching never moves it.
struct MeetingReviewTabs: View {
    let store: MeetingsStore

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 12) {
                navigation
                Spacer(minLength: 0)
                transcriptControls
            }
            .frame(height: 32)
            VStack(alignment: .leading, spacing: 12) {
                navigation
                transcriptControls
            }
        }
        .padding(.bottom, 12)
    }

    private var navigation: some View {
        Picker("Meeting view", selection: tab) {
            ForEach(MeetingReviewTab.allCases) { tab in
                Text(tab.title).tag(tab)
            }
        }
        .labelsHidden()
        .pickerStyle(.segmented)
        .fixedSize()
    }

    @ViewBuilder
    private var transcriptControls: some View {
        if !store.transcriptDeleted {
            let shown = store.tab == .transcript
            HStack(spacing: 12) {
                Picker("Wording", selection: Binding(
                    get: { store.originalWording }, set: { store.chooseOriginalWording($0) })) {
                    Text("Edited").tag(false)
                    Text("Original").tag(true)
                }
                .pickerStyle(.segmented)
                .fixedSize()
                TranscriptSearchField(store: store)
            }
            .frame(height: 32)
            .opacity(shown ? 1 : 0)
            .disabled(!shown)
            .accessibilityHidden(!shown)
        }
    }

    private var tab: Binding<MeetingReviewTab> {
        Binding(get: { store.tab }, set: { store.choose(tab: $0) })
    }
}

/// "Search this meeting", small enough to share the row with the tabs. ⌘F
/// or the magnifier puts the cursor in it, escape empties it, and while a
/// query is typed it counts what the core found.
private struct TranscriptSearchField: View {
    let store: MeetingsStore
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: 6) {
            Button { focused = true } label: {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 13, weight: .regular))
                    .foregroundStyle(Theme.inkTertiary)
            }
            .buttonStyle(.plain)
            .keyboardShortcut("f", modifiers: .command)
            .accessibilityLabel("Search this meeting")
            TextField(
                "Search this meeting", text: query,
                prompt: Text("Search this meeting").foregroundStyle(Theme.inkTertiary)
            )
            .textFieldStyle(.plain)
            .font(TypeScale.body(13))
            .foregroundStyle(Theme.ink)
            .focused($focused)
            .onExitCommand {
                if store.transcriptQuery.isEmpty { focused = false } else { store.searchTranscript("") }
            }
            if !store.transcriptQuery.isEmpty {
                if let hits = store.searchHits {
                    Text("\(hits.count)")
                        .font(TypeScale.body(12))
                        .monospacedDigit()
                        .foregroundStyle(Theme.inkTertiary)
                        .accessibilityLabel(hits.count == 1 ? "1 match" : "\(hits.count) matches")
                }
                Button { store.searchTranscript("") } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.inkTertiary)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Clear the search")
            }
        }
        .padding(.horizontal, 10)
        .frame(width: 216, height: 30)
        .background(Theme.surface, in: Capsule())
        .overlay(Capsule().strokeBorder(focused ? Theme.accent : Theme.border, lineWidth: 1))
    }

    private var query: Binding<String> {
        Binding(get: { store.transcriptQuery }, set: { store.searchTranscript($0) })
    }
}

// MARK: - The words

/// The transcript: who spoke, what they said, what Sona missed, and the way
/// to correct any of it. A voice's name holds what can be done about it.
struct TranscriptPane: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var editingSegment: TranscriptSegmentId?
    @State private var renaming: MeetingSpeaker?
    @State private var name = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            if !store.transcriptDeleted && !store.elsewhereHits.isEmpty {
                elsewhere
            }
            if let speaker = store.transcriptSpeaker {
                HStack(spacing: 10) {
                    Text("Only \(store.speakerName(speaker))").metaText(Theme.inkSecondary)
                    Button("Show everyone") { store.showOnly(speaker: nil) }
                        .buttonStyle(QuietButton(color: Theme.accent))
                }
            }
            turns
            if !store.transcriptDeleted && !store.gapRuns.isEmpty {
                gaps
            }
        }
        .alert("Rename this voice", isPresented: renamingPresented) {
            TextField("Name", text: $name)
            Button("Save") {
                let next = name.trimmingCharacters(in: .whitespacesAndNewlines)
                if let speaker = renaming, !next.isEmpty, next != speaker.displayName {
                    store.renameSpeaker(speaker.speakerId, to: next)
                }
                renaming = nil
            }
            Button("Cancel", role: .cancel) { renaming = nil }
        }
    }

    /// What the search found outside the words: the title, a note.
    private var elsewhere: some View {
        Card {
            ForEach(store.elsewhereHits) { hit in
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(hit.kind == .title ? "In the title" : "In a note").metaText()
                        Text(hit.excerpt).bodyText(14)
                    }
                }
            }
        }
    }

    private var turns: some View {
        ScrollViewReader { proxy in
            let turns = store.turns
            LazyVStack(alignment: .leading, spacing: 16) {
                if turns.isEmpty {
                    Text(emptyLine).bodyText(14, Theme.inkSecondary)
                } else {
                    ForEach(turns) { turn in
                        TranscriptTurnRow(
                            store: store,
                            turn: turn,
                            speakers: snapshot.speakers,
                            editing: $editingSegment,
                            rename: { speaker in
                                name = speaker.displayName
                                renaming = speaker
                            }
                        )
                        .id(turn.segments.first?.base.segmentId ?? turn.id)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .onChange(of: store.jump, initial: true) { _, jump in
                guard let jump, store.canJumpTo(jump.segmentId),
                      let turn = turns.first(where: { row in
                          row.segments.contains { $0.base.segmentId == jump.segmentId }
                      }) else { return }
                withAnimation(reduceMotion ? nil : .default) {
                    proxy.scrollTo(turn.id, anchor: .center)
                }
                store.clearJump()
            }
        }
    }

    private var gaps: some View {
        PageSection("What Sona missed") {
            Card {
                ForEach(store.gapRuns) { run in
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("\(run.range) — \(run.reason.label)").bodyText(14)
                            Text(detail(run)).metaText()
                        }
                    }
                }
            }
        }
    }

    private var emptyLine: String {
        if store.transcriptDeleted {
            return MeetingCommandError.transcriptDeleted.label + " Audio playback and rewriting are unavailable."
        }
        return store.transcriptQuery.isEmpty
            ? "Nothing was transcribed for this meeting."
            : "Nothing said matches “\(store.transcriptQuery)”."
    }

    private func detail(_ run: MeetingGapRun) -> String {
        var parts: [String] = []
        if run.count > 1 { parts.append("\(run.count) gaps") }
        if let missing = run.missing { parts.append("\(missing) missing") }
        if let frames = run.droppedFrames, frames > 0 { parts.append("\(frames) frames dropped") }
        return parts.isEmpty ? "One gap" : parts.joined(separator: " · ")
    }

    private var renamingPresented: Binding<Bool> {
        Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })
    }
}

/// One voice, one stretch: the name in the voice's color with the clock
/// across from it, then the words. The name opens what can be done about the
/// voice; a press on a line opens it for correction.
struct TranscriptTurnRow: View {
    let store: MeetingsStore
    let turn: TranscriptTurn
    let speakers: [MeetingSpeaker]
    @Binding var editing: TranscriptSegmentId?
    let rename: (MeetingSpeaker) -> Void
    @Environment(AppModel.self) private var model
    @State private var savingPassage: TranscriptEffectiveSegment?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                voice
                Spacer(minLength: 12)
                if let segment = turn.segments.first, store.canPlay(segment.base.segmentId) {
                    Button { store.playContribution(segment.base.segmentId) } label: {
                        Label(turn.time, systemImage: "play.fill")
                            .font(TypeScale.body(12)).monospacedDigit()
                    }
                    .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                    .accessibilityLabel("Listen from \(turn.time)")
                } else {
                    Text(turn.time).font(TypeScale.body(12)).monospacedDigit()
                        .foregroundStyle(Theme.inkTertiary)
                }
            }
            ForEach(turn.segments) { segment in
                line(segment)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .sheet(item: $savingPassage) { segment in
            if let sessionId = store.openSessionId {
                ScratchPassageSheet(store: model.scratchpad, meetingId: sessionId,
                    segmentId: segment.base.segmentId, excerpt: store.displayedText(segment))
            }
        }
    }

    /// The name, and behind it the two corrections a voice takes: a new
    /// name, or the same person as another voice.
    @ViewBuilder
    private var voice: some View {
        let label = Text(store.speakerName(turn.speakerId))
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(SpeakerInk.color(at: speakers.firstIndex { $0.speakerId == turn.speakerId }))
        if let speaker = speakers.first(where: { $0.speakerId == turn.speakerId }) {
            Menu {
                if store.editable {
                    Button("Rename…") { rename(speaker) }
                    ForEach(speakers.filter { $0.speakerId != speaker.speakerId }) { other in
                        Button("Same person as \(other.displayName)") {
                            store.mergeSpeaker(speaker.speakerId, into: other.speakerId)
                        }
                    }
                } else {
                    Text("This meeting cannot be edited.")
                }
            } label: {
                label
            }
            .menuStyle(.button)
            .buttonStyle(.plain)
            .menuIndicator(.hidden)
            .fixedSize()
            .accessibilityHint(store.editable ? "Rename this voice or merge it with another" : "")
        } else {
            label
        }
    }

    @ViewBuilder
    private func line(_ segment: TranscriptEffectiveSegment) -> some View {
        if editing == segment.base.segmentId {
            SegmentEditor(store: store, segment: segment) { editing = nil }
                .id(segment.base.segmentId)
        } else {
            Button {
                editing = segment.base.segmentId
            } label: {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(TranscriptHighlight.mark(store.displayedText(segment), query: store.transcriptQuery))
                        .bodyText(14, segment.removed ? Theme.inkDisabled : Theme.ink)
                        .strikethrough(segment.removed, color: Theme.inkDisabled)
                    if segment.edited && !store.originalWording {
                        Text("corrected").font(TypeScale.body(11)).foregroundStyle(Theme.inkTertiary)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(!store.editable)
            .accessibilityHint(store.editable ? "Correct these words" : "")
            .id(segment.base.segmentId)
            .contextMenu {
                if store.canPlay(segment.base.segmentId) {
                    Button("Listen with context") { store.playContribution(segment.base.segmentId) }
                }
                if !segment.removed && !store.transcriptDeleted {
                    Button("Save passage…") { savingPassage = segment }
                    Button("Copy source link") {
                        let milliseconds = max(0, segment.base.startOffsetNs) / 1_000_000
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(
                            "sona://meeting/\(store.openSessionId ?? "")?atMs=\(milliseconds)&segmentId=\(segment.base.segmentId)", forType: .string)
                    }
                }
            }
            if store.relisten.fix?.segmentId == segment.base.segmentId {
                RelistenPrompt(store: store.relisten)
            }
        }
    }
}

/// A voice's color in a transcript, by its place among the meeting's
/// speakers, so a name keeps one color from line to line. A line no voice is
/// sure of stays gray.
enum SpeakerInk {
    private static let palette: [Color] = [
        NSColor.systemBlue, .systemOrange, .systemGreen, .systemPink, .systemTeal, .systemYellow,
    ].map { hue in
        Color(nsColor: NSColor(name: nil) { appearance in
            guard appearance.bestMatch(from: [.aqua, .darkAqua]) != .darkAqua else { return hue }
            // The system hues fall under 2:1 contrast on the light page; at
            // half brightness every one clears 4.5:1.
            var shade = hue
            appearance.performAsCurrentDrawingAppearance {
                shade = hue.blended(withFraction: 0.5, of: .black) ?? hue
            }
            return shade
        })
    }

    static func color(at index: Int?) -> Color {
        guard let index else { return Theme.inkSecondary }
        return palette[index % palette.count]
    }
}

/// The query, marked in the words it matched.
enum TranscriptHighlight {
    static func mark(_ text: String, query: String) -> AttributedString {
        var result = AttributedString(text)
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return result }
        var cursor = result.startIndex
        while cursor < result.endIndex,
              let found = result[cursor...].range(of: needle, options: [.caseInsensitive]) {
            result[found].backgroundColor = Theme.accentSoft
            result[found].foregroundColor = Theme.accent
            cursor = found.upperBound
        }
        return result
    }
}

/// One line, open for correction: the words as they now read, a commit, and
/// the way to take the line out of the record.
struct SegmentEditor: View {
    let store: MeetingsStore
    let segment: TranscriptEffectiveSegment
    let close: () -> Void
    @State private var text = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            InputField(prompt: "What was said", text: $text)
                .onSubmit(commit)
            HStack(spacing: 12) {
                Button("Save", action: commit)
                    .buttonStyle(SecondaryButton(compact: true))
                    .disabled(store.busy)
                Button("Cancel", action: close).buttonStyle(QuietButton())
                Spacer(minLength: 12)
                if !segment.removed {
                    Button("Remove this line") {
                        store.editSegment(segment, text: segment.text, removed: true)
                        close()
                    }
                    .buttonStyle(QuietButton(color: Theme.live))
                    .disabled(store.busy)
                }
            }
        }
        .onAppear { text = segment.text }
    }

    private func commit() {
        let next = text.trimmingCharacters(in: .whitespacesAndNewlines)
        close()
        guard !next.isEmpty, next != segment.text else { return }
        store.editSegment(segment, text: next)
    }
}

// MARK: - What Sona made of it

/// What the model wrote first, then the people, the numbers, and the notes.
struct MeetingInsightsPane: View {
    let store: MeetingsStore
    let settings: MeetingSettingsStore
    let snapshot: MeetingReviewSnapshot
    var openPerson: (String) -> Void = { _ in }
    var openMeetingSettings: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            ArtifactPane(store: store, snapshot: snapshot, templates: settings.templates)
            MeetingPeopleBand(store: store, openPerson: openPerson)
            DisclosureGroup("Call participant names") {
                CallNamesReviewSection(store: store, snapshot: snapshot)
                    .padding(.top, 12)
            }
            .font(TypeScale.body(14))
            .foregroundStyle(Theme.inkSecondary)
            MeetingAnalyticsStrip(store: store)
            MeetingQuestionsCard(store: store, snapshot: snapshot)
            MeetingNotesCard(store: store, snapshot: snapshot)
            MeetingSnapshotsSection(sessionId: snapshot.session.sessionId)
            MeetingUserNotesCard(store: store, templates: settings.templates)
            MeetingSeriesSection(
                store: settings, sessionId: snapshot.session.sessionId,
                openMeetingSettings: openMeetingSettings)
        }
    }
}

/// "You have met Ada 4 times before": the band React shows above the numbers.
struct MeetingPeopleBand: View {
    let store: MeetingsStore
    var openPerson: (String) -> Void = { _ in }

    var body: some View {
        if !store.previouslyTogether.isEmpty {
            PageSection("Previously together") {
                Card {
                    ForEach(store.previouslyTogether) { row in
                        CardRow(action: { openPerson(row.personId) }) {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(row.displayName).bodyText()
                                Text(line(row)).metaText()
                                if let loop = row.topOpenLoop {
                                    Text("Still open: \(loop.text)").metaText(Theme.inkSecondary)
                                }
                            }
                        } trailing: {
                            Image(systemName: "chevron.right")
                                .font(.system(size: 11, weight: .semibold))
                                .foregroundStyle(Theme.inkTertiary)
                        }
                    }
                }
            }
        }
    }

    private func line(_ row: MeetingPersonContextRow) -> String {
        var parts: [String] = []
        let before = row.priorMeetings
        if before > 0 {
            parts.append(before == 1 ? "1 meeting before this" : "\(before) meetings before this")
        }
        if let last = row.lastPriorMeeting {
            parts.append("last on \(last.date.short): \(last.title)")
        }
        return parts.joined(separator: " · ")
    }
}

/// Who talked and how the room handed over: one bar of everyone's share of
/// the talk, where a voice's name narrows the transcript to its words, a
/// timeline of when each voice spoke, and the numbers this meeting can say
/// something with.
struct MeetingAnalyticsStrip: View {
    let store: MeetingsStore

    var body: some View {
        if let talk = store.talk {
            Card {
                VStack(alignment: .leading, spacing: 14) {
                    let slices = TalkSlice.fold(store.talkShares)
                    if !slices.isEmpty {
                        TalkShareBar(slices: slices, color: color)
                        ChatFlowRow(spacing: 14) {
                            ForEach(slices) { legend($0) }
                        }
                        TalkTimeline(store: store, talk: talk, slices: slices, color: color)
                    }
                    HStack(alignment: .top, spacing: 16) {
                        Stat(label: "Turns", value: "\(talk.turnCount)")
                        if let handovers = store.handovers {
                            Stat(label: "Handovers", value: "\(handovers)")
                        }
                        longestRun(talk)
                        if let patience = store.patience {
                            Stat(label: "Patience", value: patience)
                        }
                    }
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
                .overlay(alignment: .bottom) { Hairline().padding(.horizontal, 16) }
                ForEach(store.trackers) { tracker in
                    CardRow {
                        HStack(spacing: 10) {
                            Text(tracker.name).bodyText(14)
                            Text(tracker.hitCount == 1 ? "1 mention" : "\(tracker.hitCount) mentions")
                                .metaText()
                        }
                    } trailing: {
                        if let first = tracker.segmentIds.first {
                            Button("Show first") { store.jumpTo(first) }
                                .buttonStyle(QuietButton(color: Theme.accent))
                                .disabled(!store.canJumpTo(first))
                        }
                    }
                }
            }
            .padding(.bottom, 28)
        }
    }

    private func color(_ slice: TalkSlice) -> Color {
        switch slice {
        case let .voice(share):
            SpeakerInk.color(at: store.snapshot?.speakers.firstIndex { $0.speakerId == share.speakerId })
        case .others:
            Theme.inkTertiary
        }
    }

    /// A voice's name and share, a press that reads only its words. The
    /// folded voices have no one transcript to narrow to.
    @ViewBuilder
    private func legend(_ slice: TalkSlice) -> some View {
        let label = HStack(spacing: 6) {
            Circle().fill(color(slice)).frame(width: 7, height: 7)
            Text(slice.name(store)).font(TypeScale.body(13)).foregroundStyle(Theme.inkSecondary).lineLimit(1)
            Text(slice.permille.meetingTalkShare)
                .font(TypeScale.label(13))
                .monospacedDigit()
                .foregroundStyle(Theme.ink)
        }
        switch slice {
        case let .voice(share):
            Button { store.showOnly(speaker: share.speakerId) } label: { label }
                .buttonStyle(.plain)
                .help("\(share.speakingNs.meetingTalkDuration) in \(share.turnCount == 1 ? "1 turn" : "\(share.turnCount) turns")")
                .accessibilityElement(children: .combine)
                .accessibilityHint("Show only these words in the transcript")
        case .others:
            label.accessibilityElement(children: .combine)
        }
    }

    /// The longest run and who held it, a press that plays it from its start.
    @ViewBuilder
    private func longestRun(_ talk: MeetingTalkMetrics) -> some View {
        let who = talk.longestMonologueSpeakerId.map { store.speakerName($0) }
        let stat = Stat(label: who.map { "Longest run · \($0)" } ?? "Longest run", value: talk.longestMonologueNs.meetingTalkDuration)
        if let start = talk.longestMonologueStartNs, let speaker = talk.longestMonologueSpeakerId,
           let segmentId = store.segmentStarting(at: start, speaker: speaker), store.canPlay(segmentId) {
            Button {
                store.jumpTo(segmentId)
                store.playContribution(segmentId)
            } label: { stat }
                .buttonStyle(.plain)
                .help("Play it from the start")
        } else {
            stat
        }
    }
}

/// One part of the talk bar: a voice, or the voices too small to draw.
private enum TalkSlice: Identifiable {
    case voice(SpeakerTalkShare)
    case others(permille: Int)

    /// A voice under one part in fifty would draw thinner than the gap
    /// beside it, so those fold into one part at the end.
    private static let foldPermille = 20

    /// The shares come apportioned to add up to 1000; folding only sums them.
    static func fold(_ shares: [SpeakerTalkShare]) -> [TalkSlice] {
        let voices = shares.filter { $0.sharePermille >= foldPermille }.map(TalkSlice.voice)
        let folded = shares.filter { $0.sharePermille < foldPermille }.reduce(0) { $0 + $1.sharePermille }
        return folded > 0 ? voices + [.others(permille: folded)] : voices
    }

    var id: String {
        switch self {
        case let .voice(share): share.speakerId
        case .others: "others"
        }
    }

    var permille: Int {
        switch self {
        case let .voice(share): share.sharePermille
        case let .others(permille): permille
        }
    }

    @MainActor
    func name(_ store: MeetingsStore) -> String {
        switch self {
        case let .voice(share): store.speakerName(share.speakerId)
        case .others: "Others"
        }
    }
}

/// Every voice's share of the talk, end to end in one bar.
private struct TalkShareBar: View {
    let slices: [TalkSlice]
    let color: (TalkSlice) -> Color

    private static let gap: CGFloat = 2

    var body: some View {
        GeometryReader { proxy in
            let room = proxy.size.width - Self.gap * CGFloat(max(slices.count - 1, 0))
            HStack(spacing: Self.gap) {
                ForEach(slices) { slice in
                    Capsule()
                        .fill(color(slice))
                        .frame(width: max(room * CGFloat(slice.permille) / 1000, 0))
                }
            }
        }
        .frame(height: 8)
        .accessibilityHidden(true)
    }
}

/// The transcript laid out for the timeline: each line in the lane of its
/// part of the talk bar, and the turns a press can open. A voice the bar
/// folds into Others shares the Others lane; a line from no voice on the
/// bar is left out, as the bar leaves it out.
private struct TalkRibbon {
    struct Mark {
        let lane: Int
        let startNs: Int64
        let endNs: Int64
    }

    struct Turn: Identifiable {
        let lane: Int
        let speakerId: SpeakerId
        let startNs: Int64
        var endNs: Int64
        let segmentId: TranscriptSegmentId

        var id: TranscriptSegmentId { segmentId }
    }

    let lanes: [SpeakerId: Int]
    let marks: [Mark]
    let turns: [Turn]
    /// Where the last line that still reads ends: the right edge.
    let endNs: Int64

    init(transcript: [TranscriptEffectiveSegment], shares: [SpeakerTalkShare], slices: [TalkSlice]) {
        var lanes: [SpeakerId: Int] = [:]
        var othersLane: Int?
        for (index, slice) in slices.enumerated() {
            switch slice {
            case let .voice(share): lanes[share.speakerId] = index
            case .others: othersLane = index
            }
        }
        if let othersLane {
            for share in shares where lanes[share.speakerId] == nil {
                lanes[share.speakerId] = othersLane
            }
        }
        let spoken = transcript
            .filter { !$0.removed }
            .sorted { $0.base.startOffsetNs < $1.base.startOffsetNs }
        var marks: [Mark] = []
        var turns: [Turn] = []
        var lastSpeaker: SpeakerId?
        for segment in spoken {
            let speaker = segment.assignedSpeakerId
            defer { lastSpeaker = speaker }
            guard let lane = lanes[speaker] else { continue }
            let start = max(0, segment.base.startOffsetNs)
            let end = max(start, segment.base.endOffsetNs)
            marks.append(Mark(lane: lane, startNs: start, endNs: end))
            if lastSpeaker == speaker, let last = turns.indices.last {
                turns[last].endNs = max(turns[last].endNs, end)
            } else {
                turns.append(Turn(lane: lane, speakerId: speaker, startNs: start, endNs: end,
                                  segmentId: segment.base.segmentId))
            }
        }
        self.lanes = lanes
        self.marks = marks
        self.turns = turns
        endNs = spoken.map { $0.base.endOffsetNs }.max() ?? 0
    }
}

/// When each voice spoke, where the bar above says how much: one lane per
/// part of the bar, the meeting left to right from its start to its last
/// word, and the longest run outlined. A press opens that turn in the
/// transcript.
private struct TalkTimeline: View {
    let store: MeetingsStore
    let talk: MeetingTalkMetrics
    let slices: [TalkSlice]
    let color: (TalkSlice) -> Color

    private static let laneHeight: CGFloat = 7
    private static let laneGap: CGFloat = 3
    private static let axisGap: CGFloat = 5
    private static let tick: CGFloat = 3
    private static let narrowestBar: CGFloat = 1.5
    private static let narrowestPress: CGFloat = 6

    var body: some View {
        let ribbon = TalkRibbon(
            transcript: store.snapshot?.transcript ?? [], shares: store.talkShares, slices: slices)
        if ribbon.marks.count >= 2, ribbon.endNs > 0 {
            VStack(alignment: .leading, spacing: 4) {
                GeometryReader { proxy in
                    plot(ribbon, size: proxy.size)
                }
                .frame(height: Self.lanesHeight(slices.count) + Self.axisGap + Self.tick + 1)
                ZStack {
                    Text((ribbon.endNs / 2).meetingOffsetClock)
                    HStack {
                        Text(Int64(0).meetingOffsetClock)
                        Spacer(minLength: 8)
                        Text(ribbon.endNs.meetingOffsetClock)
                    }
                }
                .metaText()
                .monospacedDigit()
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Who spoke when")
            .accessibilityValue(summary(ribbon))
        }
    }

    private func plot(_ ribbon: TalkRibbon, size: CGSize) -> some View {
        let scale = size.width / CGFloat(ribbon.endNs)
        let inks = slices.map(color)
        let axis = Self.lanesHeight(slices.count) + Self.axisGap + 0.5
        let longest = longestRun(ribbon, scale: scale)
        return ZStack(alignment: .topLeading) {
            Canvas { context, canvas in
                for mark in ribbon.marks {
                    let rect = Self.bar(mark.lane, from: mark.startNs, to: mark.endNs, scale: scale,
                                        narrowest: Self.narrowestBar)
                    context.fill(Path(roundedRect: rect, cornerRadius: min(2, rect.width / 2)),
                                 with: .color(inks[mark.lane]))
                }
                if let longest {
                    context.stroke(Path(roundedRect: longest.insetBy(dx: -2, dy: -2), cornerRadius: 3.5),
                                   with: .color(Theme.inkSecondary), lineWidth: 1)
                }
                var rule = Path()
                rule.move(to: CGPoint(x: 0, y: axis))
                rule.addLine(to: CGPoint(x: canvas.width, y: axis))
                for x in [0.5, canvas.width / 2, canvas.width - 0.5] {
                    rule.move(to: CGPoint(x: x, y: axis))
                    rule.addLine(to: CGPoint(x: x, y: axis + Self.tick))
                }
                context.stroke(rule, with: .color(Theme.border), lineWidth: 1)
            }
            ForEach(ribbon.turns) { turn in
                press(turn, scale: scale, width: size.width)
            }
        }
        .frame(width: size.width, height: size.height, alignment: .topLeading)
    }

    /// A turn's press: its lane's height and gap, never narrower than a
    /// pointer can land on, held inside the plot.
    private func press(_ turn: TalkRibbon.Turn, scale: CGFloat, width: CGFloat) -> some View {
        let drawn = Self.bar(turn.lane, from: turn.startNs, to: turn.endNs, scale: scale, narrowest: 0)
        let wide = max(drawn.width, Self.narrowestPress)
        let x = min(max(drawn.midX - wide / 2, 0), max(width - wide, 0))
        return Color.clear
            .frame(width: wide, height: Self.laneHeight + Self.laneGap)
            .contentShape(Rectangle())
            .onTapGesture { store.jumpTo(turn.segmentId) }
            .help("\(store.speakerName(turn.speakerId)) · \(turn.startNs.meetingOffsetClock) – \(turn.endNs.meetingOffsetClock)")
            .offset(x: x, y: drawn.minY - Self.laneGap / 2)
    }

    private func longestRun(_ ribbon: TalkRibbon, scale: CGFloat) -> CGRect? {
        guard let start = talk.longestMonologueStartNs, let speaker = talk.longestMonologueSpeakerId,
              talk.longestMonologueNs > 0, let lane = ribbon.lanes[speaker] else { return nil }
        return Self.bar(lane, from: start, to: start + talk.longestMonologueNs, scale: scale,
                        narrowest: Self.narrowestBar)
    }

    /// Each lane as a screen reader hears it: who, how many turns the core
    /// counted, and when they first and last spoke.
    private func summary(_ ribbon: TalkRibbon) -> String {
        slices.enumerated().map { lane, slice -> String in
            let count = store.talkShares
                .filter { ribbon.lanes[$0.speakerId] == lane }
                .reduce(0) { $0 + $1.turnCount }
            var line = "\(slice.name(store)), \(count == 1 ? "1 turn" : "\(count) turns")"
            let turns = ribbon.turns.filter { $0.lane == lane }
            if let first = turns.first, let last = turns.last {
                line += first.id == last.id
                    ? ", at \(first.startNs.meetingOffsetClock)"
                    : ", first at \(first.startNs.meetingOffsetClock), last at \(last.startNs.meetingOffsetClock)"
            }
            return line
        }
        .joined(separator: ". ")
    }

    private static func lanesHeight(_ count: Int) -> CGFloat {
        CGFloat(count) * laneHeight + CGFloat(max(count - 1, 0)) * laneGap
    }

    private static func bar(_ lane: Int, from start: Int64, to end: Int64, scale: CGFloat,
                            narrowest: CGFloat) -> CGRect {
        CGRect(x: CGFloat(start) * scale, y: CGFloat(lane) * (laneHeight + laneGap),
               width: max(CGFloat(end - start) * scale, narrowest), height: laneHeight)
    }
}

/// The words changed after this was written: a corrected line, a renamed
/// speaker. What is on screen still reads; it just reads the earlier
/// transcript, and one click writes it again.
struct StaleNotesBand: View {
    let store: MeetingsStore
    let what: String

    var body: some View {
        Card {
            CardRow {
                VStack(alignment: .leading, spacing: 4) {
                    Text("\(what) were written before the transcript changed.").bodyText()
                    Text("They still read the earlier words.").metaText(Theme.inkSecondary)
                }
            } trailing: {
                if store.canRegenerate {
                    Button("Write again") { store.regenerate() }
                        .buttonStyle(SecondaryButton(compact: true))
                        .disabled(store.busy)
                }
            }
        }
    }
}

/// The questions asked of this meeting, and what the evidence supported.
struct MeetingQuestionsCard: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot

    var body: some View {
        if !snapshot.questions.isEmpty {
            PageSection("Asked of this meeting") {
                Card {
                    ForEach(snapshot.questions) { answer in
                        CardRow {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(answer.question ?? "A question with no text").bodyText()
                                if let text = answer.answer {
                                    Text(text).bodyText(14, Theme.inkSecondary)
                                }
                                HStack(spacing: 10) {
                                    Text(answer.state.label).metaText(Theme.inkSecondary)
                                    if answer.provisional {
                                        Text("provisional").metaText()
                                    }
                                    if let through = answer.throughOffsetNs {
                                        Text("as of \(through.meetingOffsetClock)").metaText()
                                    }
                                }
                            }
                        } trailing: {
                            if answer.state != .forgotten, store.editable {
                                Button("Forget") { store.forgetQuestion(answer.questionId) }
                                    .buttonStyle(QuietButton())
                                    .disabled(store.busy)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The notes a person typed against a moment, and the composer for one more.
struct MeetingNotesCard: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot
    @State private var editing: MeetingNoteId?
    @State private var draft = ""

    var body: some View {
        PageSection("Notes in the moment") {
            Card {
                ForEach(snapshot.notes) { note in
                    row(note).id("meeting-note:\(note.noteId)")
                }
                CardRow {
                    InputField(prompt: "Add a note", text: newNote)
                        .onSubmit { store.createNote() }
                } trailing: {
                    Button("Add") { store.createNote() }
                        .buttonStyle(SecondaryButton(compact: true))
                        .disabled(store.busy || store.newNote.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
        }
    }

    @ViewBuilder
    private func row(_ note: MeetingManualNote) -> some View {
        if editing == note.noteId {
            CardRow {
                VStack(alignment: .leading, spacing: 10) {
                    InputField(prompt: "The note", text: $draft)
                        .onSubmit { commit(note) }
                    HStack(spacing: 12) {
                        Button("Save") { commit(note) }
                            .buttonStyle(SecondaryButton(compact: true))
                            .disabled(store.busy)
                        Button("Cancel") { editing = nil }.buttonStyle(QuietButton())
                    }
                }
            }
        } else {
            CardRow(action: store.editable ? { start(note) } : nil) {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    Text(note.startOffsetNs?.meetingOffsetClock ?? note.createdAtUtcMs.meetingDate.time)
                        .font(TypeScale.body(12))
                        .monospacedDigit()
                        .foregroundStyle(Theme.inkTertiary)
                        .frame(width: 52, alignment: .leading)
                    Text(note.body).bodyText()
                }
            } trailing: {
                if store.editable {
                    Button("Delete") { store.deleteNote(note) }
                        .buttonStyle(QuietButton(color: Theme.live))
                        .disabled(store.busy)
                }
            }
        }
    }

    private func start(_ note: MeetingManualNote) {
        draft = note.body
        editing = note.noteId
    }

    private func commit(_ note: MeetingManualNote) {
        let next = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        editing = nil
        guard !next.isEmpty, next != note.body else { return }
        store.updateNote(note, body: next)
    }

    private var newNote: Binding<String> {
        Binding(get: { store.newNote }, set: { store.setNewNote($0) })
    }
}

/// The page a person writes in: a line with how it is kept, its template and
/// the way to put it in front of the model, then an editor that grows with
/// what is typed.
struct MeetingUserNotesCard: View {
    let store: MeetingsStore
    /// The person's own templates, for the picker to name beside the
    /// built-ins. A list that could not be read costs only the custom names.
    let templates: MeetingTemplatesStore

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            Card {
                MeetingNotesEditor(text: notesText)
                    .onDisappear { store.flushNotes() }
                if store.notesState == .conflict {
                    CardRow {
                        Text("Sona could not save that. The notes on disk may have changed; reopen the meeting to see them.")
                            .bodyText(14, Theme.live)
                    }
                }
            }
        }
        .padding(.bottom, 32)
        .task { await templates.loadIfNeeded() }
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text("Your own notes").sectionLabel()
                .accessibilityAddTraits(.isHeader)
            if let saved = store.notesSavedLine {
                Text(saved).metaText()
            }
            Spacer(minLength: 12)
            MeetingTemplateMenu(
                builtIns: MeetingNotesTemplate.allCases,
                builtInLabel: { $0.label },
                templates: templates,
                selection: template
            ) {
                HStack(spacing: 4) {
                    Text(templateLabel)
                    Image(systemName: "chevron.up.chevron.down")
                        .font(.system(size: 10, weight: .semibold))
                        .accessibilityHidden(true)
                }
                .font(TypeScale.label(13))
                .foregroundStyle(Theme.inkSecondary)
                .contentShape(Rectangle())
            }
            .menuStyle(.button)
            .buttonStyle(.plain)
            .menuIndicator(.hidden)
            .fixedSize()
            .disabled(store.userNotes == nil || store.enhancing || store.busy)
            if store.canRegenerate, !store.notesBody.isEmpty {
                Button("Rewrite with my notes") { store.reenhance() }
                    .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                    .disabled(store.enhancing || store.busy)
            }
        }
    }

    private var notesText: Binding<String> {
        Binding(get: { store.notesBody }, set: { store.typeNotes($0) })
    }

    /// The meeting's choice: its custom template when it has one, else the
    /// built-in. Never nil on the way out: the menu offers no "app default".
    private var template: Binding<MeetingTemplateChoice<MeetingNotesTemplate>?> {
        Binding(
            get: {
                MeetingTemplateChoice(
                    builtIn: store.userNotes?.template ?? .general,
                    customTemplateId: store.userNotes?.customTemplateId)
            },
            set: { choice in
                if let choice { store.choose(template: choice) }
            })
    }

    /// "General meeting template", or the name of the person's own template
    /// the meeting chose; never its raw id.
    private var templateLabel: String {
        switch template.wrappedValue {
        case let .custom(templateId)?: templates.name(templateId) ?? "Custom template"
        default: "\((store.userNotes?.template ?? .general).label) template"
        }
    }
}

/// Notes that grow with what is typed: three lines when empty, a line more
/// for every line written, so the page scrolls and the editor never does.
private struct MeetingNotesEditor: View {
    @Binding var text: String

    var body: some View {
        // The editor cannot size itself to its words; a hidden copy of them
        // can. The copy wraps a little narrower, so it is never the shorter.
        Text(text + " ")
            .font(TypeScale.body())
            .padding(.horizontal, 8)
            .frame(maxWidth: .infinity, minHeight: 54, alignment: .topLeading)
            .padding(.bottom, 8)
            .hidden()
            .overlay(alignment: .topLeading) {
                ZStack(alignment: .topLeading) {
                    if text.isEmpty {
                        Text("Start typing…")
                            .font(TypeScale.body())
                            .foregroundStyle(Theme.inkTertiary)
                            .padding(.horizontal, 5)
                    }
                    TextEditor(text: $text)
                        .font(TypeScale.body())
                        .foregroundStyle(Theme.ink)
                        .scrollContentBackground(.hidden)
                        .scrollDisabled(true)
                }
            }
            .padding(.horizontal, 15)
            .padding(.vertical, 16)
    }
}

// MARK: - The ledger

/// The reading a person can check: what each thread came to, the quote behind
/// it, what is still open, who owes what, and where Sona stopped trusting it.
struct LedgerPane: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot
    var openPerson: (String) -> Void = { _ in }

    var body: some View {
        if let readable = snapshot.readableLedger {
            let ledger = readable.ledger
            let counts = LedgerOutcomeCounts(ledger)
            // A ledger written again may have nothing under the pick; it then reads as no pick.
            let picked = store.ledgerOutcome.flatMap { counts.count($0) > 0 ? $0 : nil }
            VStack(alignment: .leading, spacing: 28) {
                if readable.stale {
                    StaleNotesBand(store: store, what: "This ledger")
                }
                headline(ledger, counts: counts, picked: picked)
                if !ledger.threads.isEmpty { threads(ledger, picked: picked) }
                LoopList(store: store, kind: .loop, openPerson: openPerson)
                LoopList(store: store, kind: .commitment, openPerson: openPerson)
                if !ledger.stances.isEmpty { stances(ledger) }
                LedgerTrustCard(ledger: ledger)
            }
        } else {
            PageSection("Ledger") {
                Card {
                    CardRow {
                        VStack(alignment: .leading, spacing: 6) {
                            Text(missing).bodyText(14, Theme.inkSecondary)
                            if snapshot.ledgerFailure != nil {
                                Text("The notes above were written; only this second pass was not.")
                                    .metaText(Theme.inkSecondary)
                            }
                        }
                    } trailing: {
                        if snapshot.ledgerFailure != nil, store.canRegenerate {
                            Button("Write again") { store.regenerate() }
                                .buttonStyle(SecondaryButton(compact: true))
                                .disabled(store.busy)
                        }
                    }
                }
            }
        }
    }

    /// Why there is no ledger: the cause the core kept, or the plain fact
    /// when the notes are older than that record or were never written.
    private var missing: String {
        if let cause = snapshot.ledgerFailure {
            return "No ledger was written because \(cause.label)."
        }
        if snapshot.session.processingStatus.isPending {
            return "The ledger is written after the notes. It lands here."
        }
        return "No ledger was written for this meeting."
    }

    private func headline(_ ledger: MeetingLedger, counts: LedgerOutcomeCounts, picked: LedgerOutcome?) -> some View {
        Card {
            CardRow {
                VStack(alignment: .leading, spacing: 10) {
                    Text(ledger.headline).font(TypeScale.headline).foregroundStyle(Theme.ink)
                    if counts.total > 0 {
                        HStack(alignment: .center, spacing: 10) {
                            OutcomeBar(counts: counts, selected: picked) { store.pick(outcome: $0) }
                            if let choices = counts.choices {
                                Text(choices).metaText()
                            }
                        }
                    }
                }
            }
        }
    }

    /// Every thread, or only the substantive ones in the picked part of the
    /// bar, which are the ones it counts.
    private func threads(_ ledger: MeetingLedger, picked: LedgerOutcome?) -> some View {
        let shown = ledger.threads.filter { thread in
            picked.map { thread.substantive && thread.state.outcome == $0 } ?? true
        }
        return PageSection(picked.map { "Threads · \($0.label)" } ?? "Threads") {
            Card {
                ForEach(Array(shown.enumerated()), id: \.offset) { _, thread in
                    LedgerThreadRow(store: store, thread: thread)
                }
            }
        }
    }

    private func stances(_ ledger: MeetingLedger) -> some View {
        PageSection("Where people stood") {
            Card {
                ForEach(Array(ledger.stances.enumerated()), id: \.offset) { _, stance in
                    CardRow(action: stance.citations.first.flatMap { store.citationAction($0.segmentId) }) {
                        VStack(alignment: .leading, spacing: 4) {
                            HStack(spacing: 8) {
                                Text(stance.from).bodyText(14)
                                if let to = stance.to {
                                    Image(systemName: "arrow.right")
                                        .font(.system(size: 10, weight: .semibold))
                                        .foregroundStyle(Theme.inkTertiary)
                                    Text(to).bodyText(14)
                                }
                            }
                            Text(stance.what).bodyText()
                            if let note = stance.note {
                                Text(note).metaText()
                            }
                        }
                    } trailing: {
                        Text(stance.atMs.meetingMsClock)
                            .font(TypeScale.body(12))
                            .monospacedDigit()
                            .foregroundStyle(Theme.inkTertiary)
                    }
                }
            }
        }
    }
}

/// One thread: its topic, the word it came to, and the quote that word rests
/// on.
struct LedgerThreadRow: View {
    let store: MeetingsStore
    let thread: LedgerThread

    var body: some View {
        CardRow(action: jump) {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 10) {
                    Text(thread.topic).bodyText()
                    Text(thread.state.label)
                        .font(TypeScale.label(12))
                        .foregroundStyle(color)
                    if let owner = thread.owner {
                        Text(owner).metaText()
                    }
                    if !thread.substantive {
                        Text("aside").metaText()
                    }
                }
                Text("“\(thread.receipt.quote)”").bodyText(14, Theme.inkSecondary)
                Text(attribution).metaText()
            }
        } trailing: {
            Text(thread.receipt.tMs.meetingMsClock)
                .font(TypeScale.body(12))
                .monospacedDigit()
                .foregroundStyle(Theme.inkTertiary)
        }
    }

    private var attribution: String {
        let who = thread.receipt.speaker ?? store.citedSpeakers(thread.receipt.citations) ?? "Unattributed"
        let citations = thread.receipt.citations.count
        return citations > 1 ? "\(who) · \(citations) citations" : who
    }

    private var color: Color {
        switch thread.state.outcome {
        case .landed, .open: Theme.ink
        case .dropped: Theme.inkTertiary
        }
    }

    private var jump: (() -> Void)? {
        thread.receipt.citations.first.flatMap { store.citationAction($0.segmentId) }
    }
}

/// Open loops, or commitments: the rows a person actually works through.
struct LoopList: View {
    let store: MeetingsStore
    let kind: MeetingLoopKind
    var openPerson: (String) -> Void = { _ in }

    var body: some View {
        let rows = store.loopRows(kind)
        if !rows.isEmpty {
            PageSection(kind == .loop ? "Still open" : "Who owes what") {
                Card {
                    ForEach(rows) { row in
                        LoopRowView(store: store, row: row, openPerson: openPerson)
                    }
                }
            }
        }
    }
}

/// One loop: done or not, who owns it, and what it was read from.
struct LoopRowView: View {
    let store: MeetingsStore
    let row: MeetingLoopRow
    var openPerson: (String) -> Void = { _ in }

    var body: some View {
        CardRow {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Button {
                    store.change(row, row.status == .open ? .resolve(dropped: false) : .reopen)
                } label: {
                    Image(systemName: row.status == .done ? "checkmark.circle.fill" : "circle")
                        .font(.system(size: 15))
                        .foregroundStyle(row.status == .done ? Theme.accent : Theme.inkTertiary)
                }
                .buttonStyle(.plain)
                .disabled(store.loopsBusy)
                VStack(alignment: .leading, spacing: 4) {
                    Text(row.text)
                        .bodyText(14, row.status == .open ? Theme.ink : Theme.inkTertiary)
                        .strikethrough(row.status == .done, color: Theme.inkTertiary)
                    HStack(spacing: 10) {
                        Text(row.status.label).metaText(row.status == .open ? Theme.ink : Theme.inkTertiary)
                        Text(row.direction.label).metaText()
                        if let owner = row.owner {
                            Text(owner).metaText()
                        }
                        if row.resolvedBy == .external {
                            Text("closed elsewhere").metaText()
                        }
                        if let carried = row.carriedSinceAtUtcMs {
                            Text("carried since \(carried.meetingDate.short)").metaText()
                        }
                    }
                    if let quote = row.quote {
                        Text("“\(quote)”").metaText(Theme.inkSecondary)
                    }
                    if let instead = row.instead, !instead.isEmpty {
                        Text("What happened instead: \(instead)").metaText()
                    }
                    if store.audio.failedReelKey == receiptsKey, let error = store.audio.error {
                        Text(error).metaText(error == ConversationAudioPlayback.heldNotice ? Theme.inkSecondary : Theme.live)
                    }
                }
            }
        } trailing: {
            HStack(spacing: 12) {
                owner
                if row.status == .open {
                    Button("Drop") { store.change(row, .resolve(dropped: true)) }
                        .buttonStyle(QuietButton())
                        .disabled(store.loopsBusy)
                } else {
                    Button("Reopen") { store.change(row, .reopen) }
                        .buttonStyle(QuietButton())
                        .disabled(store.loopsBusy)
                }
                if let citation = row.citations.first {
                    Button(citation.startOffsetNs.meetingOffsetClock) { store.jumpTo(citation.segmentId) }
                        .buttonStyle(QuietButton(color: Theme.accent))
                        .disabled(!store.canJumpTo(citation.segmentId))
                }
                if hasReceipts {
                    receipts
                }
            }
        }
    }

    private var receiptsKey: String { "loop:\(row.loopId)" }

    /// A promise worth hearing through: it was carried between meetings or
    /// read from more than one line.
    private var hasReceipts: Bool {
        row.carriedSinceAtUtcMs != nil || row.carriedIntoLoopId != nil || row.citations.count > 1
    }

    /// "Play receipts": the promise and every later mention of it, oldest
    /// first, across the meetings it was carried through.
    @ViewBuilder
    private var receipts: some View {
        let audio = store.audio
        if audio.reelKey == receiptsKey {
            if let index = audio.reelIndex {
                Text("\(index + 1) of \(audio.reelClips.count)").metaText()
            }
            Button("Stop") { audio.stop() }
                .buttonStyle(QuietButton(color: Theme.accent))
        } else {
            Button("Play receipts") {
                audio.playReel(reading: "conversation_reel_for_loop", ["loopId": .string(row.loopId)], key: receiptsKey)
            }
            .buttonStyle(QuietButton(color: Theme.accent))
            .disabled(audio.heldForRecording)
            .help(audio.heldForRecording ? ConversationAudioPlayback.heldNotice : "")
        }
    }

    private var owner: some View {
        Menu {
            Button("Nobody") { store.change(row, .assign(personId: nil)) }
            ForEach(store.people) { person in
                Button(person.displayName) { store.change(row, .assign(personId: person.personId)) }
            }
            if let personId = row.ownerPersonId {
                Divider()
                Button("Open this person") { openPerson(personId) }
            }
        } label: {
            Text(row.ownerDisplayName ?? "Assign")
                .font(TypeScale.label(12))
                .foregroundStyle(Theme.inkSecondary)
        }
        .menuStyle(.borderlessButton)
        .tint(Theme.inkSecondary)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(store.loopsBusy)
    }
}

/// How far the ledger can be trusted: every quote found, or the count that
/// was not.
struct LedgerTrustCard: View {
    let ledger: MeetingLedger

    var body: some View {
        PageSection("How much to trust this") {
            Card {
                CardRow {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(measured).bodyText()
                        ForEach(Array(ledger.caveats.enumerated()), id: \.offset) { _, caveat in
                            Text(caveat).metaText(Theme.inkSecondary)
                        }
                    }
                }
            }
        }
    }

    private var measured: String {
        switch ledger.receipts {
        case .verified:
            "Every line here was found in the transcript."
        case let .degraded(threads, commitments):
            "Sona dropped \(threads) threads and \(commitments) commitments whose quotes it could not find."
        }
    }
}

// MARK: - Sending it on

/// The follow-up: the words, the clipboard, and the mail client.
struct FollowUpSheet: View {
    let store: MeetingsStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Follow up").titleText().accessibilityAddTraits(.isHeader)
                    if let draft = store.followUp {
                        Text(draft.source.label).metaText(Theme.inkSecondary)
                        Text(draft.mailContextStatus).metaText(Theme.inkSecondary)
                    }
                }
                Spacer(minLength: 20)
                Button("Close") { store.closeFollowUp() }.buttonStyle(.secondary)
            }
            .padding(24)
            Hairline()
            ErrorNote(store.error).padding(.horizontal, 24)
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    if store.drafting { ProgressView("Writing the draft…") }
                    Text(store.followUp.map { $0.body.isEmpty ? "No follow-up is needed for this meeting." : $0.body }
                        ?? (store.drafting ? "" : "No draft."))
                        .bodyText()
                        .textSelection(.enabled)
                    Text("Sona opens a draft. Only you can send it.").metaText(Theme.inkSecondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(24)
            }
            Hairline()
            HStack(spacing: 12) {
                Spacer(minLength: 0)
                Button("Copy") { store.copyFollowUp() }
                    .buttonStyle(.secondary)
                    .disabled(store.followUp?.body.isEmpty != false || store.pending != nil)
                Button(store.pending == "Opening Mail" ? "Opening…" : "Open in Mail") { store.mailFollowUp() }
                    .buttonStyle(.primary)
                    .disabled(store.followUp?.body.isEmpty != false || store.pending != nil)
            }
            .padding(24)
        }
        .frame(width: 560, height: 480)
        .background(Theme.page)
    }
}

/// What has been said so far, in the lines the core wrote.
struct CatchUpSheet: View {
    let store: MeetingsStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Caught up").titleText().accessibilityAddTraits(.isHeader)
                    if let catchUp = store.catchUp {
                        Text(line(catchUp)).metaText(Theme.inkSecondary)
                    }
                }
                Spacer(minLength: 20)
                Button("Close") { store.dismissCatchUp() }.buttonStyle(.secondary)
            }
            .padding(24)
            Hairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    if let catchUp = store.catchUp, !catchUp.bullets.isEmpty {
                        ForEach(Array(catchUp.bullets.enumerated()), id: \.offset) { _, bullet in
                            HStack(alignment: .firstTextBaseline, spacing: 10) {
                                Text("•").bodyText(14, Theme.inkSecondary)
                                Text(bullet).bodyText()
                            }
                        }
                    } else if let catchUp = store.catchUp {
                        Text(catchUp.state.label).bodyText(14, Theme.inkSecondary)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(24)
            }
        }
        .frame(width: 520, height: 400)
        .background(Theme.page)
    }

    /// "As of 12:04, from 142 lines · provisional".
    private func line(_ catchUp: MeetingCatchUp) -> String {
        var parts: [String] = []
        if let through = catchUp.throughOffsetNs {
            parts.append("As of \(through.meetingOffsetClock)")
        }
        parts.append(catchUp.segmentCount == 1 ? "from 1 line" : "from \(catchUp.segmentCount) lines")
        if catchUp.provisional {
            parts.append("provisional")
        }
        return parts.joined(separator: " · ")
    }
}
