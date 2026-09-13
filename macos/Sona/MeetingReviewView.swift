import SwiftUI

/// One recorded meeting, read three ways: the words, what Sona made of them,
/// and the ledger a reader can check against quotes.
struct MeetingReviewView: View {
    let store: MeetingsStore
    var openPerson: (String) -> Void = { _ in }
    @State private var confirmingDelete = false

    var body: some View {
        Page {
            BackLink(title: "Meetings") { store.closeReview() }
                .padding(.bottom, 20)
            if let snapshot = store.snapshot {
                MeetingReviewHeader(store: store, snapshot: snapshot, delete: { confirmingDelete = true })
                MeetingsNoticeBand(store: store)
                MeetingReviewTabs(store: store)
                pane(snapshot)
            } else {
                Text(store.reviewLoading ? "Reading the meeting…" : "That meeting is no longer here.")
                    .bodyText(15, Theme.inkSecondary)
            }
        }
        .sheet(isPresented: followUpPresented) { FollowUpSheet(store: store) }
        .sheet(isPresented: catchUpPresented) { CatchUpSheet(store: store) }
        .alert("Delete this meeting?", isPresented: $confirmingDelete) {
            Button("Delete", role: .destructive) { store.deleteOpenMeeting() }
            Button("Keep it", role: .cancel) {}
        } message: {
            Text("It goes to the trash for a week, then Sona removes it for good.")
        }
    }

    @ViewBuilder
    private func pane(_ snapshot: MeetingReviewSnapshot) -> some View {
        switch store.tab {
        case .transcript:
            TranscriptPane(store: store, snapshot: snapshot)
        case .insights:
            MeetingInsightsPane(store: store, snapshot: snapshot, openPerson: openPerson)
        case .ledger:
            LedgerPane(store: store, snapshot: snapshot, openPerson: openPerson)
        }
    }

    private var followUpPresented: Binding<Bool> {
        Binding(get: { store.followUpOpen }, set: { if !$0 { store.closeFollowUp() } })
    }

    private var catchUpPresented: Binding<Bool> {
        Binding(get: { store.catchUp != nil }, set: { if !$0 { store.dismissCatchUp() } })
    }
}

// MARK: - The head of the page

/// The title a person can correct, the facts of the recording, what the last
/// write did, and the menu of everything this meeting allows.
struct MeetingReviewHeader: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot
    let delete: () -> Void
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
            if let unattributed {
                Text(unattributed).metaText()
            }
            if snapshot.session.processingStatus != .succeeded {
                Text(snapshot.session.processingStatus.label)
                    .bodyText(14, snapshot.session.processingStatus.isFailed ? Theme.live : Theme.accent)
            }
            if let receipt = store.receiptLine {
                Text(receipt).metaText()
            }
            if let pending = store.pending {
                Text("\(pending)…").metaText(Theme.accent)
            }
            if snapshot.remoteCancellationPending {
                remoteCancellation
            }
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
            Text(snapshot.session.title)
                .titleText()
                .onTapGesture { if store.editable { start() } }
        }
    }

    private var actions: some View {
        HStack(spacing: 10) {
            if store.hasLedger {
                Button("Follow up") { store.openFollowUp() }
                    .buttonStyle(SecondaryButton(compact: true))
            }
            Menu {
                if store.editable {
                    Button("Rename", action: start)
                }
                if store.canRegenerate {
                    Button("Write the notes again") { store.regenerate() }
                }
                if store.canExport {
                    Divider()
                    Button("Export as Markdown") { store.exportOpenMeeting(.markdown) }
                    Button("Export as JSON") { store.exportOpenMeeting(.json) }
                }
                if store.hasLedger {
                    Button("Save the ledger as HTML") { store.exportOpenLedger() }
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

    private var remoteCancellation: some View {
        HStack(spacing: 14) {
            Text("Sona asked the remote engine to stop, and it has not answered yet.")
                .bodyText(14, Theme.accent)
            if store.canCancelRemote {
                Button("Cancel the remote run") { store.cancelRemote() }
                    .buttonStyle(SecondaryButton(compact: true))
                    .disabled(store.busy)
            }
        }
        .padding(.top, 4)
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

    /// The lines Sona heard but could not put a voice to.
    private var unattributed: String? {
        let count = snapshot.transcript.filter { $0.speakerAssignment == .unknown && !$0.removed }.count
        guard count > 0 else { return nil }
        return count == 1
            ? "1 line has no speaker yet."
            : "\(count) lines have no speaker yet."
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

/// The three readings, with the one in front of the reader underlined.
struct MeetingReviewTabs: View {
    let store: MeetingsStore

    var body: some View {
        HStack(spacing: 24) {
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
        }
        .overlay(alignment: .bottom) { Hairline() }
        .padding(.bottom, 24)
    }
}

// MARK: - The words

/// The transcript: who spoke, what they said, what Sona missed, and the way
/// to correct any of it.
struct TranscriptPane: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot
    @State private var editingSegment: TranscriptSegmentId?

    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            SpeakerRoster(store: store, snapshot: snapshot)
            search
            turns
            if !store.gapRuns.isEmpty {
                gaps
            }
        }
    }

    private var search: some View {
        VStack(alignment: .leading, spacing: 12) {
            SearchField(prompt: "Search this meeting", text: query)
            if let hits = store.searchHits {
                Text(hits.isEmpty ? "Nothing said matches that." : "\(hits.count) matches")
                    .metaText()
            }
            if !store.elsewhereHits.isEmpty {
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
        }
    }

    private var turns: some View {
        ScrollViewReader { proxy in
            PageSection(store.speakerNames.isEmpty ? "Transcript" : "Transcript · \(store.speakerNames.count) voices") {
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
                                editing: $editingSegment
                            )
                            .id(turn.segments.first?.base.segmentId ?? turn.id)
                        }
                    }
                }
            }
            .onChange(of: store.jump) { _, jump in
                guard let jump else { return }
                withAnimation { proxy.scrollTo(jump.segmentId, anchor: .center) }
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

    private var query: Binding<String> {
        Binding(get: { store.transcriptQuery }, set: { store.searchTranscript($0) })
    }
}

/// The roster: one chip a voice, each renameable, each mergeable into another.
struct SpeakerRoster: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot
    @State private var renaming: MeetingSpeaker?
    @State private var name = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(snapshot.diarization.status.label).metaText()
            if !snapshot.speakers.isEmpty {
                HStack(spacing: 10) {
                    ForEach(snapshot.speakers) { speaker in
                        chip(speaker)
                    }
                    Spacer(minLength: 0)
                }
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

    private func chip(_ speaker: MeetingSpeaker) -> some View {
        Menu {
            if store.editable {
                Button("Rename") {
                    name = speaker.displayName
                    renaming = speaker
                }
                ForEach(others(speaker)) { other in
                    Button("Same person as \(other.displayName)") {
                        store.mergeSpeaker(speaker.speakerId, into: other.speakerId)
                    }
                }
            } else {
                Text("This meeting cannot be edited.")
            }
        } label: {
            HStack(spacing: 6) {
                Text(speaker.displayName).font(TypeScale.label(13)).foregroundStyle(Theme.accent)
                Text(speaker.sourceKind.label).font(TypeScale.body(11)).foregroundStyle(Theme.inkTertiary)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(Theme.accentSoft, in: Capsule())
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
    }

    private func others(_ speaker: MeetingSpeaker) -> [MeetingSpeaker] {
        snapshot.speakers.filter { $0.speakerId != speaker.speakerId }
    }

    private var renamingPresented: Binding<Bool> {
        Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })
    }
}

/// One voice, one stretch: the clock, the name, and the words. A press on a
/// line opens it for correction.
struct TranscriptTurnRow: View {
    let store: MeetingsStore
    let turn: TranscriptTurn
    @Binding var editing: TranscriptSegmentId?

    var body: some View {
        CardRow {
            HStack(alignment: .top, spacing: 16) {
                Text(turn.time)
                    .font(TypeScale.mono(12))
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(width: 62, alignment: .leading)
                VStack(alignment: .leading, spacing: 6) {
                    Text(store.speakerName(turn.speakerId))
                        .font(TypeScale.label(13))
                        .foregroundStyle(Theme.accent)
                    ForEach(turn.segments) { segment in
                        line(segment)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func line(_ segment: TranscriptEffectiveSegment) -> some View {
        if editing == segment.base.segmentId {
            SegmentEditor(store: store, segment: segment) { editing = nil }
                .id(segment.base.segmentId)
        } else {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(TranscriptHighlight.mark(segment.text, query: store.transcriptQuery))
                    .bodyText(15, segment.removed ? Theme.inkDisabled : Theme.ink)
                    .strikethrough(segment.removed, color: Theme.inkDisabled)
                if segment.edited {
                    Text("corrected").font(TypeScale.body(11)).foregroundStyle(Theme.inkTertiary)
                }
            }
            .id(segment.base.segmentId)
            .contentShape(Rectangle())
            .onTapGesture { if store.editable { editing = segment.base.segmentId } }
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
    let snapshot: MeetingReviewSnapshot
    var openPerson: (String) -> Void = { _ in }

    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            MeetingPeopleBand(store: store, openPerson: openPerson)
            MeetingAnalyticsStrip(store: store)
            ArtifactPane(store: store, snapshot: snapshot)
            MeetingQuestionsCard(store: store, snapshot: snapshot)
            MeetingNotesCard(store: store, snapshot: snapshot)
            MeetingUserNotesCard(store: store)
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
        let before = max(0, row.meetingsTogether - 1)
        if before > 0 {
            parts.append(before == 1 ? "1 meeting before this" : "\(before) meetings before this")
        }
        if let last = row.lastPriorMeeting {
            parts.append("last on \(last.date.short): \(last.title)")
        }
        return parts.joined(separator: " · ")
    }
}

/// Who talked, for how long, and how the room handed over.
struct MeetingAnalyticsStrip: View {
    let store: MeetingsStore

    var body: some View {
        if let talk = store.talk {
            PageSection("How it went") {
                Card {
                    CardRow {
                        HStack(alignment: .top, spacing: 40) {
                            Stat(label: "Talk", value: store.talkLeaders.isEmpty ? "—" : store.talkLeaders)
                            Stat(label: "Longest run", value: talk.longestMonologueNs.meetingTalkDuration)
                            Stat(label: "Patience", value: store.patience)
                            Stat(label: "Handovers", value: "\(talk.interactionCount) / \(talk.turnCount) turns")
                        }
                    }
                    ForEach(talk.speakers) { share in
                        CardRow {
                            VStack(alignment: .leading, spacing: 6) {
                                HStack(spacing: 10) {
                                    Text(store.speakerName(share.speakerId)).bodyText(14)
                                    Text(share.sharePermille.meetingTalkShare).metaText(Theme.accent)
                                    Text(share.speakingNs.meetingTalkDuration).metaText()
                                    Text("\(share.turnCount) turns").metaText()
                                }
                                Meter(fraction: Double(share.sharePermille) / 1000)
                                    .frame(height: 6)
                            }
                        }
                    }
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
            }
        }
    }
}

/// Everything one generation of the model wrote, or the reason nothing was.
struct ArtifactPane: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot

    var body: some View {
        if let artifact = snapshot.currentArtifact, let content = artifact.content {
            VStack(alignment: .leading, spacing: 28) {
                summary(content, artifact: artifact)
                if !content.outline.isEmpty { outline(content) }
                if !content.decisions.isEmpty { decisions(content) }
                if !content.actionItems.isEmpty { actionItems(content, artifact: artifact) }
                if !content.keyQuestions.isEmpty { cited("Questions worth answering", content.keyQuestions) }
                if !content.risks.isEmpty { cited("Risks", content.risks) }
                followUp(content)
            }
        } else {
            ArtifactFailureCard(store: store, snapshot: snapshot)
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

/// Nothing was written, and the one line that says why.
struct ArtifactFailureCard: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot

    var body: some View {
        PageSection("Notes") {
            Card {
                CardRow {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(snapshot.session.processingStatus.label).bodyText()
                        Text(snapshot.session.processingStatus.explanation).metaText(Theme.inkSecondary)
                        if snapshot.session.processingStatus.offersSettings {
                            Text("Install a local model, or point Sona at a remote engine, in Settings.")
                                .metaText(Theme.accent)
                        }
                    }
                } trailing: {
                    HStack(spacing: 12) {
                        if snapshot.session.processingStatus.offersRetry, store.canRegenerate {
                            Button("Try again") { store.regenerate() }
                                .buttonStyle(SecondaryButton(compact: true))
                                .disabled(store.busy)
                        }
                        Button("Refresh") { Task { await store.refreshSnapshot() } }
                            .buttonStyle(QuietButton())
                    }
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

/// The pane a person writes in: a template, whatever they type, saved as they
/// go, and the way to put it in front of the model.
struct MeetingUserNotesCard: View {
    let store: MeetingsStore

    var body: some View {
        PageSection("Your own notes") {
            Card {
                ChoiceRow(
                    title: "Template",
                    detail: store.notesSavedLine,
                    choices: MeetingNotesTemplate.allCases,
                    label: { $0.label },
                    selection: template
                )
                CardRow {
                    TextEditor(text: notesText)
                        .font(TypeScale.body())
                        .foregroundStyle(Theme.ink)
                        .scrollContentBackground(.hidden)
                        .frame(minHeight: 160)
                        .onDisappear { store.flushNotes() }
                }
                if store.notesState == .conflict {
                    CardRow {
                        Text("Sona could not save that. The notes on disk may have changed; reopen the meeting to see them.")
                            .bodyText(14, Theme.live)
                    }
                }
                ActionRow(
                    title: "Write the notes again with mine",
                    detail: "Sona puts what you typed in front of the model and regenerates.",
                    button: "Re-enhance",
                    busy: store.enhancing
                ) {
                    store.reenhance()
                }
                ActionRow(
                    title: "Catch me up",
                    detail: "A handful of lines on what has been said so far.",
                    button: "Catch up",
                    busy: store.catchingUp
                ) {
                    store.runCatchUp()
                }
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

// MARK: - The ledger

/// The reading a person can check: what each thread came to, the quote behind
/// it, what is still open, who owes what, and where Sona stopped trusting it.
struct LedgerPane: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot
    var openPerson: (String) -> Void = { _ in }

    var body: some View {
        if let ledger = snapshot.currentLedger {
            VStack(alignment: .leading, spacing: 28) {
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
                        Text("No ledger was written for this meeting.").bodyText(15, Theme.inkSecondary)
                    }
                }
            }
        }
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
