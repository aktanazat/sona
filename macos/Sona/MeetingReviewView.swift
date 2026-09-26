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
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var confirmingDelete = false

    var body: some View {
        Page {
            BackLink(title: "Meetings") { store.closeReview() }
                .padding(.bottom, 20)
            if let snapshot = store.snapshot {
                MeetingReviewHeader(
                    store: store, snapshot: snapshot, delete: { confirmingDelete = true },
                    openMeetingSettings: openMeetingSettings, askAgent: askAgent)
                MeetingReviewTabs(store: store)
                pane(snapshot)
            } else if let failure = store.reviewFailure {
                unread(failure)
            } else if store.reviewLoading {
                Text("Reading the meeting…").bodyText(15, Theme.inkSecondary)
            } else {
                Text("That meeting is no longer here.").bodyText(15, Theme.inkSecondary)
            }
        }
        .overlay(alignment: .bottom) { toast }
        .sheet(isPresented: followUpPresented) { FollowUpSheet(store: store) }
        .sheet(isPresented: catchUpPresented) { CatchUpSheet(store: store) }
        .alert("Delete this meeting?", isPresented: $confirmingDelete) {
            Button("Delete", role: .destructive) { store.deleteOpenMeeting() }
            Button("Keep it", role: .cancel) {}
        } message: {
            Text("It goes to the trash for a week, then Sona removes it for good.")
        }
    }

    /// What just worked floats at the foot of the window and leaves by
    /// itself, so the page under it never moves. Without motion it only
    /// fades.
    private var toast: some View {
        ZStack {
            if let notice = store.notice {
                MeetingReviewToast(store: store, notice: notice)
                    .transition(
                        reduceMotion
                            ? AnyTransition.opacity
                            : AnyTransition.opacity.combined(with: .move(edge: .bottom)))
            }
        }
        .frame(maxWidth: 560)
        .padding(.bottom, 24)
        .animation(.easeOut(duration: 0.2), value: store.notice)
    }

    @ViewBuilder
    private func pane(_ snapshot: MeetingReviewSnapshot) -> some View {
        switch store.tab {
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
    @State private var hovering = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 14) {
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
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
        .shadow(color: .black.opacity(0.12), radius: 16, y: 6)
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

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 16) {
                title
                Spacer(minLength: 16)
                actions
            }
            Text(facts).metaText(Theme.inkSecondary)
            status
        }
        .padding(.bottom, 24)
    }

    @ViewBuilder
    private var title: some View {
        if editing {
            HStack(spacing: 10) {
                InputField(prompt: "Meeting title", text: $draft)
                    .frame(maxWidth: 420)
                    .onSubmit(commit)
                Button("Save", action: commit).buttonStyle(SecondaryButton(compact: true))
                Button("Cancel") { editing = false }.buttonStyle(QuietButton())
            }
        } else {
            Button { start() } label: {
                Text(snapshot.session.title).titleText()
            }
            .buttonStyle(.plain)
            .disabled(!store.editable)
            .accessibilityHint(store.editable ? "Edit the title" : "")
        }
    }

    /// What is under way, the one action worth a button, and the menu that
    /// holds the rest, the agent first.
    private var actions: some View {
        HStack(spacing: 10) {
            if let doing = store.pending ?? (store.catchingUp ? "Catching up" : nil) {
                Text("\(doing)…").metaText(Theme.accent)
            }
            if store.hasLedger {
                Button("Follow up") { store.openFollowUp() }
                    .buttonStyle(SecondaryButton(compact: true))
            }
            Menu {
                if let askAgent {
                    Button("Ask about this meeting") { askAgent(question) }
                    Divider()
                }
                if store.editable {
                    Button("Rename", action: start)
                }
                if store.canRegenerate {
                    Button("Write the notes again") { store.regenerate() }
                }
                Button("Catch me up") { store.runCatchUp() }
                    .disabled(store.catchingUp)
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
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.inkSecondary)
                    .frame(width: 30, height: 26)
                    .contentShape(Rectangle())
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .disabled(store.busy)
        }
    }

    /// The start of a question for the agent: this meeting by name and by
    /// link, left open for the person to finish.
    private var question: String {
        "About “\(snapshot.session.title)” (sona://meeting/\(snapshot.session.sessionId)): "
    }

    /// At most one line under the facts: what the core refused, else a remote
    /// run that has not stopped, else what became of the notes and the
    /// voices. Each carries the one thing to do about it.
    @ViewBuilder
    private var status: some View {
        if let error = store.error {
            statusLine(AttributedString(error), color: Theme.live) {
                Button("Dismiss") { store.dismissError() }
                    .buttonStyle(QuietButton(compact: true))
            }
        } else if snapshot.remoteCancellationPending {
            statusLine(AttributedString("Sona asked the remote engine to stop, and it has not answered yet.")) {
                if store.canCancelRemote {
                    Button("Cancel the remote run") { store.cancelRemote() }
                        .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                        .disabled(store.busy)
                }
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
            actions()
        }
    }

    private var processing: MeetingProcessingStatus { snapshot.session.processingStatus }

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

    /// "12 Mar at 09:30 · 48m · Microphone + System audio", and the gaps when
    /// the recording has them.
    private var facts: String {
        let started = snapshot.session.startedAtUtcMs?.meetingDate
        var parts: [String] = [started.map { "\($0.short) at \($0.time)" } ?? "Never started"]
        if let elapsed = snapshot.session.elapsedOffsetNs {
            parts.append((TimeInterval(elapsed) / 1_000_000_000).spoken)
        }
        let sources = snapshot.session.sources.map(\.sourceKind.label)
        if !sources.isEmpty {
            parts.append(sources.joined(separator: " + "))
        }
        if snapshot.session.captureCompleteness == .partial {
            parts.append("Partial recording")
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

/// The three readings, with the one in front of the reader underlined, and
/// the transcript's search at the end of the row. The row keeps one height
/// on every tab, so switching never moves the page.
struct MeetingReviewTabs: View {
    let store: MeetingsStore

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 24) {
            ForEach(MeetingReviewTab.allCases) { tab in
                Button {
                    store.choose(tab: tab)
                } label: {
                    VStack(spacing: 8) {
                        Text(tab.title)
                            .font(TypeScale.label())
                            .foregroundStyle(store.tab == tab ? Theme.ink : Theme.inkSecondary)
                        Rectangle()
                            .fill(store.tab == tab ? Theme.accent : .clear)
                            .frame(height: 2)
                    }
                    .fixedSize()
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            Spacer(minLength: 0)
            if store.tab == .transcript {
                TranscriptSearchField(store: store)
            }
        }
        .frame(height: 36, alignment: .bottom)
        .overlay(alignment: .bottom) { Hairline() }
        .padding(.bottom, 24)
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
                    .font(.system(size: 12, weight: .medium))
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
        .frame(width: 240, height: 28)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
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
            if !store.elsewhereHits.isEmpty {
                elsewhere
            }
            turns
            if !store.gapRuns.isEmpty {
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
            Card {
                if store.turns.isEmpty {
                    CardRow {
                        Text(emptyLine).bodyText(14, Theme.inkSecondary)
                    }
                } else {
                    ForEach(store.turns) { turn in
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
            .onChange(of: store.jump) { _, jump in
                guard let jump else { return }
                withAnimation(reduceMotion ? nil : .default) {
                    proxy.scrollTo(jump.segmentId, anchor: .center)
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
        store.transcriptQuery.isEmpty
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

/// One voice, one stretch: the clock, the name, and the words. The name opens
/// what can be done about the voice; a press on a line opens it for
/// correction.
struct TranscriptTurnRow: View {
    let store: MeetingsStore
    let turn: TranscriptTurn
    let speakers: [MeetingSpeaker]
    @Binding var editing: TranscriptSegmentId?
    let rename: (MeetingSpeaker) -> Void

    var body: some View {
        CardRow {
            HStack(alignment: .top, spacing: 16) {
                Text(turn.time)
                    .font(TypeScale.mono(12))
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(width: 62, alignment: .leading)
                VStack(alignment: .leading, spacing: 6) {
                    voice
                    ForEach(turn.segments) { segment in
                        line(segment)
                    }
                }
            }
        }
    }

    /// The name, and behind it the two corrections a voice takes: a new
    /// name, or the same person as another voice.
    @ViewBuilder
    private var voice: some View {
        let label = Text(store.speakerName(turn.speakerId))
            .font(TypeScale.label(13))
            .foregroundStyle(Theme.accent)
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
            .menuStyle(.borderlessButton)
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
                    Text(TranscriptHighlight.mark(segment.text, query: store.transcriptQuery))
                        .bodyText(15, segment.removed ? Theme.inkDisabled : Theme.ink)
                        .strikethrough(segment.removed, color: Theme.inkDisabled)
                    if segment.edited {
                        Text("corrected").font(TypeScale.body(11)).foregroundStyle(Theme.inkTertiary)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(!store.editable)
            .accessibilityHint(store.editable ? "Correct these words" : "")
            .id(segment.base.segmentId)
        }
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
                        store.editSegment(segment.base.segmentId, text: segment.text, removed: true)
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
        store.editSegment(segment.base.segmentId, text: next)
    }
}

// MARK: - What Sona made of it

/// The people, the numbers, the notes, and whatever the model wrote.
struct MeetingInsightsPane: View {
    let store: MeetingsStore
    let settings: MeetingSettingsStore
    let snapshot: MeetingReviewSnapshot
    var openPerson: (String) -> Void = { _ in }
    var openMeetingSettings: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            MeetingPeopleBand(store: store, openPerson: openPerson)
            MeetingAnalyticsStrip(store: store)
            ArtifactPane(store: store, snapshot: snapshot)
            MeetingQuestionsCard(store: store, snapshot: snapshot)
            MeetingNotesCard(store: store, snapshot: snapshot)
            MeetingUserNotesCard(store: store)
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
                                    Text("Still open: \(loop.text)").metaText(Theme.accent)
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

/// Who talked, for how long, and how the room handed over: the numbers this
/// meeting can say something with, in one strip, then a thin bar a voice.
struct MeetingAnalyticsStrip: View {
    let store: MeetingsStore

    var body: some View {
        if let talk = store.talk {
            Card {
                VStack(alignment: .leading, spacing: 14) {
                    HStack(alignment: .firstTextBaseline, spacing: 32) {
                        figure(talk.longestMonologueNs.meetingTalkDuration, "Longest run")
                        if let patience = store.patience {
                            figure(patience, "Patience")
                        }
                        if let handovers = store.handovers {
                            figure("\(handovers)", "Handovers in \(talk.turnCount) turns")
                        }
                    }
                    let shares = store.talkShares
                    if !shares.isEmpty {
                        VStack(alignment: .leading, spacing: 8) {
                            ForEach(shares) { share in
                                bar(share)
                            }
                        }
                    }
                }
                .padding(.horizontal, 20)
                .padding(.vertical, 16)
                .frame(maxWidth: .infinity, alignment: .leading)
                .overlay(alignment: .bottom) { Hairline() }
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
                        }
                    }
                }
            }
            .padding(.bottom, 32)
        }
    }

    /// A number over its name: 17 points, never wrapped.
    private func figure(_ value: String, _ label: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(value)
                .font(TypeScale.headline)
                .monospacedDigit()
                .foregroundStyle(Theme.ink)
            Text(label).metaText()
        }
        .lineLimit(1)
        .fixedSize()
    }

    /// One voice on one line: the name, a thin bar of its share, the share,
    /// and the time it held.
    private func bar(_ share: SpeakerTalkShare) -> some View {
        HStack(spacing: 12) {
            Text(store.speakerName(share.speakerId))
                .font(TypeScale.body(13))
                .foregroundStyle(Theme.inkSecondary)
                .lineLimit(1)
                .frame(width: 120, alignment: .leading)
            Meter(fraction: Double(share.sharePermille) / 1000)
                .frame(maxWidth: 360)
            Text(share.sharePermille.meetingTalkShare)
                .font(TypeScale.label(13))
                .monospacedDigit()
                .foregroundStyle(Theme.ink)
                .frame(width: 40, alignment: .trailing)
            Text(share.speakingNs.meetingTalkDuration)
                .font(TypeScale.body(13))
                .monospacedDigit()
                .foregroundStyle(Theme.inkTertiary)
                .frame(width: 56, alignment: .trailing)
        }
        .help(share.turnCount == 1 ? "1 turn" : "\(share.turnCount) turns")
        .accessibilityElement(children: .combine)
    }
}

/// Everything one generation of the model wrote. When nothing was, the
/// status line under the title already says why and offers the retry.
struct ArtifactPane: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot

    var body: some View {
        if let artifact = snapshot.readableArtifact, let content = artifact.content {
            VStack(alignment: .leading, spacing: 28) {
                if artifact.state == .outOfDate {
                    StaleNotesBand(store: store, what: "These notes")
                }
                summary(content, artifact: artifact)
                if !content.outline.isEmpty { outline(content) }
                if !content.decisions.isEmpty { decisions(content) }
                if !content.actionItems.isEmpty { actionItems(content, artifact: artifact) }
                if !content.keyQuestions.isEmpty { cited("Questions worth answering", content.keyQuestions) }
                if !content.risks.isEmpty { cited("Risks", content.risks) }
                followUp(content)
            }
        }
    }

    private func summary(_ content: MeetingGeneratedArtifacts, artifact: MeetingArtifactRevision) -> some View {
        PageSection("Summary") {
            Card {
                if let lines = content.summary.tracedLines(content.summaryTrace) {
                    ForEach(lines) { line in
                        CardRow(action: line.segmentId.map { id in { store.jumpTo(id) } }) {
                            HStack(alignment: .firstTextBaseline, spacing: 12) {
                                if let offset = line.startOffsetNs {
                                    Text(offset.meetingOffsetClock)
                                        .font(TypeScale.mono(12))
                                        .foregroundStyle(Theme.inkTertiary)
                                        .frame(width: 52, alignment: .leading)
                                }
                                Text(line.text).bodyText()
                            }
                        }
                    }
                } else {
                    CardRow {
                        Text(content.summary.text).bodyText()
                    }
                }
                CardRow {
                    Text("Written \(artifact.generatedAtUtcMs.meetingDate.short) at \(artifact.generatedAtUtcMs.meetingDate.time) · \(artifact.templateId) v\(artifact.templateVersion)")
                        .metaText()
                } trailing: {
                    if store.canRegenerate {
                        Button("Write it again") { store.regenerate() }
                            .buttonStyle(QuietButton())
                            .disabled(store.busy)
                    }
                }
            }
        }
    }

    private func outline(_ content: MeetingGeneratedArtifacts) -> some View {
        PageSection("What was covered") {
            Card {
                ForEach(Array(content.outline.enumerated()), id: \.offset) { _, topic in
                    CardRow(action: jump(topic.title)) {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(topic.title.text).bodyText()
                            if let detail = topic.detail {
                                Text(detail.text).metaText(Theme.inkSecondary)
                            }
                        }
                    }
                }
            }
        }
    }

    private func decisions(_ content: MeetingGeneratedArtifacts) -> some View {
        cited("Decisions", content.decisions)
    }

    private func actionItems(
        _ content: MeetingGeneratedArtifacts, artifact: MeetingArtifactRevision
    ) -> some View {
        PageSection("Action items") {
            Card {
                ForEach(Array(content.actionItems.enumerated()), id: \.offset) { index, item in
                    CardRow {
                        HStack(alignment: .firstTextBaseline, spacing: 12) {
                            Button {
                                store.toggleActionItem(
                                    artifact.artifactId, index,
                                    done: !store.isDone(artifact.artifactId, index))
                            } label: {
                                Image(systemName: store.isDone(artifact.artifactId, index)
                                    ? "checkmark.square.fill" : "square")
                                    .font(.system(size: 14, weight: .medium))
                                    .foregroundStyle(store.isDone(artifact.artifactId, index)
                                        ? Theme.accent : Theme.inkTertiary)
                            }
                            .buttonStyle(.plain)
                            VStack(alignment: .leading, spacing: 4) {
                                Text(item.text.text)
                                    .bodyText(15, store.isDone(artifact.artifactId, index)
                                        ? Theme.inkTertiary : Theme.ink)
                                    .strikethrough(store.isDone(artifact.artifactId, index),
                                                   color: Theme.inkTertiary)
                                if let owner = ownerLine(item) {
                                    Text(owner).metaText()
                                }
                            }
                        }
                    } trailing: {
                        if let citation = item.text.citations.first {
                            Button(citation.startOffsetNs.meetingOffsetClock) {
                                store.jumpTo(citation.segmentId)
                            }
                            .buttonStyle(QuietButton())
                        }
                    }
                }
            }
        }
    }

    private func cited(_ label: String, _ lines: [ArtifactCitedText]) -> some View {
        PageSection(label) {
            Card {
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    CardRow(action: jump(line)) {
                        Text(line.text).bodyText()
                    } trailing: {
                        if let citation = line.citations.first {
                            Text(citation.startOffsetNs.meetingOffsetClock)
                                .font(TypeScale.mono(12))
                                .foregroundStyle(Theme.inkTertiary)
                        }
                    }
                }
            }
        }
    }

    private func followUp(_ content: MeetingGeneratedArtifacts) -> some View {
        PageSection("Follow up") {
            Card {
                CardRow {
                    Text(content.followUpDraft.text.isEmpty
                        ? "The model wrote no follow-up for this meeting."
                        : content.followUpDraft.text)
                        .bodyText()
                } trailing: {
                    if store.hasLedger {
                        Button("Draft it") { store.openFollowUp() }
                            .buttonStyle(SecondaryButton(compact: true))
                    }
                }
            }
        }
    }

    private func ownerLine(_ item: MeetingActionItem) -> String? {
        let parts = [item.ownerText, item.dueText].compactMap { $0 }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    private func jump(_ line: ArtifactCitedText) -> (() -> Void)? {
        guard let citation = line.citations.first else { return nil }
        return { store.jumpTo(citation.segmentId) }
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
                                    Text(answer.state.label).metaText(Theme.accent)
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
                    row(note)
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
                        .font(TypeScale.mono(12))
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
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text("Your own notes").sectionLabel()
            if let saved = store.notesSavedLine {
                Text(saved).metaText()
            }
            Spacer(minLength: 12)
            Menu {
                Picker("Template", selection: template) {
                    ForEach(MeetingNotesTemplate.allCases, id: \.self) { choice in
                        Text(choice.label).tag(choice)
                    }
                }
                .pickerStyle(.inline)
            } label: {
                Text("\(template.wrappedValue.label) template")
                    .font(TypeScale.label(13))
                    .foregroundStyle(Theme.inkSecondary)
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .disabled(store.userNotes == nil)
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

    private var template: Binding<MeetingNotesTemplate> {
        Binding(get: { store.userNotes?.template ?? .general }, set: { store.choose(template: $0) })
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
            VStack(alignment: .leading, spacing: 28) {
                if readable.stale {
                    StaleNotesBand(store: store, what: "This ledger")
                }
                headline(ledger)
                if !ledger.threads.isEmpty { threads(ledger) }
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
                            Text(missing).bodyText(15, Theme.inkSecondary)
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

    private func headline(_ ledger: MeetingLedger) -> some View {
        Card {
            CardRow {
                Text(ledger.headline).font(TypeScale.headline).foregroundStyle(Theme.ink)
            }
        }
    }

    private func threads(_ ledger: MeetingLedger) -> some View {
        PageSection("Threads") {
            Card {
                ForEach(Array(ledger.threads.enumerated()), id: \.offset) { _, thread in
                    LedgerThreadRow(store: store, thread: thread)
                }
            }
        }
    }

    private func stances(_ ledger: MeetingLedger) -> some View {
        PageSection("Where people stood") {
            Card {
                ForEach(Array(ledger.stances.enumerated()), id: \.offset) { _, stance in
                    CardRow(action: stance.citations.first.map { c in { store.jumpTo(c.segmentId) } }) {
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
                            .font(TypeScale.mono(12))
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
                .font(TypeScale.mono(12))
                .foregroundStyle(Theme.inkTertiary)
        }
    }

    private var attribution: String {
        let who = thread.receipt.speaker ?? "Unattributed"
        let citations = thread.receipt.citations.count
        return citations > 1 ? "\(who) · \(citations) citations" : who
    }

    private var color: Color {
        switch thread.state.outcome {
        case .landed: Theme.accent
        case .open: Theme.ink
        case .dropped: Theme.inkTertiary
        }
    }

    private var jump: (() -> Void)? {
        guard let citation = thread.receipt.citations.first else { return nil }
        return { store.jumpTo(citation.segmentId) }
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
                    Image(systemName: row.status == .done ? "checkmark.square.fill" : "square")
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(row.status == .done ? Theme.accent : Theme.inkTertiary)
                }
                .buttonStyle(.plain)
                .disabled(store.loopsBusy)
                VStack(alignment: .leading, spacing: 4) {
                    Text(row.text)
                        .bodyText(15, row.status == .open ? Theme.ink : Theme.inkTertiary)
                        .strikethrough(row.status == .done, color: Theme.inkTertiary)
                    HStack(spacing: 10) {
                        Text(row.status.label).metaText(row.status == .open ? Theme.accent : Theme.inkTertiary)
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
                }
            }
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
                    Text("Follow up").font(TypeScale.headline).foregroundStyle(Theme.ink)
                    if let draft = store.followUp {
                        Text(draft.source.label).metaText(Theme.inkSecondary)
                    }
                }
                Spacer(minLength: 20)
                Button("Close") { store.closeFollowUp() }.buttonStyle(.secondary)
            }
            .padding(24)
            Hairline()
            ScrollView {
                Text(store.followUp?.body ?? (store.drafting ? "Writing the draft…" : "No draft."))
                    .bodyText()
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(24)
            }
            Hairline()
            HStack(spacing: 12) {
                Spacer(minLength: 0)
                Button("Copy") { store.copyFollowUp() }
                    .buttonStyle(.secondary)
                    .disabled(store.followUp == nil)
                Button("Open in Mail") { store.mailFollowUp() }
                    .buttonStyle(.primary)
                    .disabled(store.followUp == nil)
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
                    Text("Caught up").font(TypeScale.headline).foregroundStyle(Theme.ink)
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
                                Text("·").bodyText(15, Theme.accent)
                                Text(bullet).bodyText()
                            }
                        }
                    } else if let catchUp = store.catchUp {
                        Text(catchUp.state.label).bodyText(15, Theme.inkSecondary)
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
