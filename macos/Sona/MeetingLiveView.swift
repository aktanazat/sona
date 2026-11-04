import SwiftUI

/// Capture, while it runs, and the small surfaces that lead into it: the
/// consent panel, a detection prompt, and the prep/wrap/recording rituals.
/// `MeetingLive.tsx`, `ConsentPanel.tsx` and `RitualCards.tsx`.

// MARK: - The live screen

/// The running call, with the words as they arrive, the listener's own notes
/// and help that reads the call without interrupting its recording.
struct MeetingLiveView: View {
    let store: MeetingLiveStore

    /// Where a stopped meeting is read. The live screen calls this only for a
    /// press that asks for the notes; a stop leaves `store.opened` set, which
    /// is the shell's cue.
    var onOpenReview: (MeetingSessionId) -> Void = { _ in }
    /// A recalled past moment, opened on its meeting's page while the
    /// recording keeps going.
    var onOpenRecalled: (MeetingRecallMatch) -> Void = { _ in }

    @State private var noteOpen = false
    /// Set by the Add press; the sheet closes when the core has kept the note
    /// and stays open, draft intact, when it refused.
    @State private var noteSaving = false
    @State private var discardOpen = false
    @State private var highlightedLine: String?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(AppModel.self) private var model

    private var session: MeetingSessionSnapshot? { store.live?.session }

    var body: some View {
        GeometryReader { geometry in
            ScrollViewReader { proxy in
                if let session {
                    if geometry.size.width >= 820 {
                        VStack(alignment: .leading, spacing: 0) {
                            captureHeader(session)
                            HStack(alignment: .top, spacing: 24) {
                                // A jump's highlight reaches 8pt past the words,
                                // and a scroll view clips at its own edge.
                                ScrollView {
                                    transcript
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                        .padding(.horizontal, 8)
                                }
                                .scrollIndicators(.never)
                                .padding(.horizontal, -8)
                                .frame(maxWidth: .infinity)
                                Rectangle()
                                    .fill(Theme.hairline)
                                    .frame(width: 1)
                                ScrollView {
                                    VStack(alignment: .leading, spacing: 0) {
                                        ownNotes
                                        recalled
                                        help(proxy)
                                    }
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                }
                                .scrollIndicators(.never)
                                .frame(width: 320)
                            }
                        }
                        .frame(maxWidth: Theme.contentMax, maxHeight: .infinity, alignment: .topLeading)
                        .padding(.horizontal, Theme.margin)
                        .padding(.top, 12)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                    } else {
                        Page {
                            captureHeader(session)
                            ownNotes
                            recalled
                            help(proxy)
                            transcript
                        }
                    }
                } else {
                    Page {
                        Text("This meeting is no longer recording.")
                            .bodyText(14, Theme.inkSecondary)
                    }
                }
            }
        }
        .sheet(isPresented: $noteOpen) { note }
        .onDisappear { Task { await store.flushLiveNotes() } }
        .task(id: highlightedLine) {
            guard highlightedLine != nil else { return }
            do { try await Task.sleep(for: .seconds(2)) } catch { return }
            highlightedLine = nil
        }
        .confirmationDialog(
            "Stop and discard this session?",
            isPresented: $discardOpen,
            titleVisibility: .visible
        ) {
            Button("Stop and discard", role: .destructive) { store.discard() }
            Button("Keep recording", role: .cancel) {}
        } message: {
            Text(
                "Audio, transcript, manual notes, generated notes, and saved answers "
                    + "will be deleted. This cannot be undone.")
        }
    }

    // MARK: The head of the page

    @ViewBuilder
    private func captureHeader(_ session: MeetingSessionSnapshot) -> some View {
        header(session)
        ErrorNote(store.error)
        warnings
        MeetingSnapshotStrip(sessionId: session.sessionId)
        CallNamesLiveControl(sessionId: session.sessionId)
        if let state = store.active, state.snapshot.sessionId == session.sessionId {
            MeetingDisclosureStatus(store: store, disclosure: state.disclosure)
        }
    }

    @ViewBuilder
    private func header(_ session: MeetingSessionSnapshot) -> some View {
        HStack(alignment: .center, spacing: 12) {
            Text(session.title)
                .titleText()
                .lineLimit(1)
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 16)
            // The clock is the state while a capture is running, so the phase
            // word only appears when the state is not what the clock implies:
            // paused, stopping, or already processing.
            if session.phase != .capturingRecording {
                Text(session.phase.label).metaText(Theme.inkSecondary)
            }
            // The store keeps the capture's clock; this counts on from it
            // every second while the capture runs, and stands still while
            // it is paused.
            HStack(spacing: 8) {
                LiveDot(state: dot(session))
                if let clock = store.clocks[session.sessionId] {
                    MeetingClockText(clock: clock, size: 13, color: Theme.ink)
                }
            }
            ConversationBookmarkButton(
                isRecording: session.phase == .capturingRecording
                    && store.pending == nil && store.clocks[session.sessionId] != nil,
                saved: false,
                action: { store.bookmark() })
            ConversationTypedBookmarks(
                isRecording: session.phase == .capturingRecording
                    && store.pending == nil && store.clocks[session.sessionId] != nil,
                counts: store.bookmarkCounts?.sessionId == session.sessionId
                    ? store.bookmarkCounts?.counts : nil,
                action: { store.bookmark($0) })
            .task(id: session.sessionId) { await store.loadBookmarkCounts(session.sessionId) }
            if session.phase.isActive {
                Button("Stop") { store.stop() }
                    .buttonStyle(.primary)
                    .disabled(!session.allows(.stop) || store.pending != nil)
            } else {
                Button("Read meeting") { store.readStoppedMeeting() }
                    .buttonStyle(.primary)
                    .disabled(store.pending != nil)
            }
            menu(session)
        }
        .frame(minHeight: 30)
        .padding(.bottom, 20)
    }

    /// Red while recording, a ring while paused, ink while the core is
    /// stopping or processing.
    private func dot(_ session: MeetingSessionSnapshot) -> CaptureState {
        switch session.phase {
        case .capturingRecording: .recording(since: session.captureStart)
        case .capturingPaused: .idle
        default: .working(session.phase.label)
        }
    }

    @ViewBuilder
    private func menu(_ session: MeetingSessionSnapshot) -> some View {
        Menu {
            if session.phase == .capturingPaused {
                Button("Resume") { store.resume() }
                    .disabled(!session.allows(.resume) || store.pending != nil)
            } else {
                Button("Pause") { store.pause() }
                    .disabled(!session.allows(.pause) || store.pending != nil)
            }
            Button("Add a note") { noteOpen = true }
                .disabled(store.pending != nil)
            MeetingSnapshotMenuItem(sessionId: session.sessionId)
            Divider()
            Button("Discard", role: .destructive) { discardOpen = true }
                .disabled(!session.allows(.discard) || store.pending != nil)
        } label: {
            Image(systemName: "ellipsis")
        }
        .menuStyle(.button)
        .buttonStyle(.circle)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("More")
        .accessibilityLabel("More")
    }

    // MARK: What is worth saying about the capture

    @ViewBuilder
    private var warnings: some View {
        if !store.warnings.isEmpty {
            Card {
                ForEach(Array(store.warnings.enumerated()), id: \.offset) { _, warning in
                    CardRow {
                        HStack(alignment: .firstTextBaseline, spacing: 12) {
                            Image(systemName: "exclamationmark.triangle")
                                .font(.system(size: 13))
                                .foregroundStyle(warning.urgent ? Theme.live : Theme.inkSecondary)
                                .accessibilityHidden(true)
                            Text(warning.text)
                                .bodyText(14, warning.urgent ? Theme.live : Theme.ink)
                        }
                    }
                }
            }
            .padding(.bottom, 24)
        }
    }

    // MARK: The words

    @ViewBuilder
    private var transcript: some View {
        PageSection("Transcript") {
            if store.showsProvisional {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(store.provisional) { segment in
                        line(
                            at: segment.startOffsetNs, segment.text,
                            speaker: segment.speakerLabel, ink: ink(segment),
                            anchor: "live-\(segment.segmentId)")
                    }
                    Text("Provisional words and speaker labels")
                        .metaText(Theme.inkSecondary)
                        .padding(.top, 8)
                }
            } else if store.lines.isEmpty {
                Text("Words appear here as they are recognized.")
                    .bodyText(14, Theme.inkSecondary)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(store.lines, id: \.base.segmentId) { segment in
                        line(
                            at: segment.base.startOffsetNs, segment.text,
                            speaker: store.speakerLabel(segment), ink: ink(segment),
                            anchor: "stored-\(segment.base.segmentId)")
                    }
                }
            }
        }
    }

    /// The voice in its color with the clock across from it, then the words.
    private func line(
        at offsetNs: Int64, _ text: String, speaker: String, ink: Color, anchor: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text(speaker)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(ink)
                Spacer(minLength: 12)
                Text(offsetNs.meetingOffsetClock)
                    .font(TypeScale.body(12))
                    .monospacedDigit()
                    .foregroundStyle(Theme.inkTertiary)
            }
            Text(text).bodyText(14)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            highlightedLine == anchor ? Theme.accentSoft : .clear,
            in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .padding(.horizontal, -8)
        .id(anchor)
    }

    private func ink(_ segment: TranscriptEffectiveSegment) -> Color {
        switch segment.speakerAssignment {
        case .localSpeaker, .systemSpeaker:
            SpeakerInk.color(at: store.live?.speakers.firstIndex { $0.speakerId == segment.assignedSpeakerId })
        case .unknown, .overlap:
            SpeakerInk.color(at: nil)
        }
    }

    /// The running pass numbers its speakers per source, so the source is
    /// part of the color: the first voice on each source is a different one.
    private func ink(_ segment: MeetingProvisionalSegment) -> Color {
        guard case let .cluster(index) = segment.speaker,
              let source = MeetingSourceKind.allCases.firstIndex(of: segment.sourceKind)
        else { return SpeakerInk.color(at: nil) }
        return SpeakerInk.color(at: Int(index) * MeetingSourceKind.allCases.count + source)
    }

    private func jump(to offset: Int64, using proxy: ScrollViewProxy) {
        let anchor: String
        if store.showsProvisional {
            guard let segment = store.provisional.first(where: { $0.startOffsetNs == offset })
                ?? store.provisional.last(where: { $0.startOffsetNs <= offset }) else { return }
            anchor = "live-\(segment.segmentId)"
        } else {
            guard let segment = store.lines.first(where: { $0.base.startOffsetNs == offset })
                ?? store.lines.last(where: { $0.base.startOffsetNs <= offset }) else { return }
            anchor = "stored-\(segment.base.segmentId)"
        }
        highlightedLine = anchor
        withAnimation(reduceMotion ? nil : .easeOut(duration: 0.2)) {
            proxy.scrollTo(anchor, anchor: .center)
        }
    }

    // MARK: Your own notes

    private var ownNotes: some View {
        PageSection("Your own notes") {
            if store.notesLoading {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Opening your notes…").metaText()
                }
            } else if store.userNotes == nil {
                ErrorNote(store.notesError ?? "Your notes could not be opened.")
                Button("Try again") { store.reloadLiveNotes() }.buttonStyle(.secondary)
            } else {
                Text("These guide the notes Sona writes when the call ends.")
                    .metaText(Theme.inkSecondary)
                Card {
                    ZStack(alignment: .topLeading) {
                        if store.notesBody.isEmpty {
                            Text("Start typing…").bodyText(14, Theme.inkTertiary)
                                .padding(.horizontal, 5)
                                .padding(.top, 8)
                                .allowsHitTesting(false)
                        }
                        TextEditor(text: Binding(
                            get: { store.notesBody }, set: { store.typeLiveNotes($0) }))
                            .font(TypeScale.body(14))
                            .foregroundStyle(Theme.ink)
                            .scrollContentBackground(.hidden)
                            .frame(height: 180)
                            .disabled(store.pending != nil)
                            .accessibilityLabel("Your own notes")
                    }
                    .padding(12)
                }
                if let saved = store.liveNotesSavedLine {
                    Text(saved).metaText(Theme.inkTertiary)
                }
                if store.notesError != nil {
                    Text("Your notes have not saved. They are still here.")
                        .bodyText(13, Theme.inkSecondary)
                    ErrorNote(store.notesError)
                    HStack {
                        Button("Keep what I wrote") { store.keepLiveNotes() }
                            .buttonStyle(.secondary)
                            .disabled(store.notesState == .saving)
                        Button("Copy my notes") { store.copyLiveNotes() }.buttonStyle(.quiet)
                    }
                }
            }
        }
    }

    // MARK: Help without stopping the call

    private func help(_ proxy: ScrollViewProxy) -> some View {
        PageSection("During the call") {
            Card {
                ForEach(
                    Array([MeetingLiveHelpKind.recent, .questions, .say].enumerated()), id: \.offset
                ) { index, kind in
                    if index > 0 { Hairline() }
                    CardRow(action: { store.askLiveHelp(kind) }) {
                        Text(kind.label).bodyText(14)
                    }
                }
            }
            .disabled(store.helpLoading)
            HStack(spacing: 8) {
                InputField(
                    prompt: "Ask about this call",
                    text: Binding(get: { store.helpQuestion }, set: { store.setHelpQuestion($0) }))
                    .onSubmit { store.askLiveHelp(.ask) }
                    .accessibilityLabel("Ask about this call")
                Button("Ask") { store.askLiveHelp(.ask) }
                    .buttonStyle(.secondary)
                    .disabled(!store.canAskLiveHelp)
            }
            if store.helpQuestion.unicodeScalars.count > 500 {
                Text("Keep your question under 500 characters.").metaText(Theme.inkSecondary)
            }
            if let kind = store.helpKind {
                helpAnswer(kind, proxy: proxy)
            } else {
                Text("Ask about what is being said. Recording keeps going.")
                    .metaText(Theme.inkTertiary)
            }
        }
    }

    private func helpAnswer(_ kind: MeetingLiveHelpKind, proxy: ScrollViewProxy) -> some View {
        Card {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .firstTextBaseline) {
                    Text(kind.label).headlineText()
                    Spacer()
                    Button("Dismiss") { store.dismissLiveHelp() }.buttonStyle(.quiet)
                }
                if let question = store.helpQuestionShown {
                    Text(question).bodyText(13, Theme.inkSecondary)
                }
                if store.helpLoading {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("Reading the call…").metaText()
                    }
                } else if let error = store.helpError {
                    ErrorNote(error)
                    Button("Try again") { store.retryLiveHelp() }.buttonStyle(.compact)
                } else if let answer = store.liveHelp {
                    if let label = answer.state.label {
                        Text(label).bodyText(13, Theme.inkSecondary)
                        Button("Try again") { store.retryLiveHelp() }.buttonStyle(.compact)
                    } else {
                        ForEach(Array(answer.items.enumerated()), id: \.offset) { _, item in
                            HStack(alignment: .firstTextBaseline, spacing: 8) {
                                Text("•").bodyText(14, Theme.inkSecondary).accessibilityHidden(true)
                                Text(item.text).bodyText(14).textSelection(.enabled)
                                Spacer(minLength: 8)
                                Button(item.startOffsetNs.meetingOffsetClock) {
                                    jump(to: item.startOffsetNs, using: proxy)
                                }
                                .buttonStyle(.quiet)
                                .accessibilityLabel(
                                    "Jump to \(item.startOffsetNs.meetingOffsetClock) in the transcript")
                            }
                        }
                    }
                    if let from = answer.fromOffsetNs, let through = answer.throughOffsetNs {
                        Text("Read from \(from.meetingOffsetClock) to \(through.meetingOffsetClock).")
                            .metaText(Theme.inkTertiary)
                    }
                    if answer.provisional {
                        Text("Based on the words recognized so far.")
                            .metaText(Theme.inkTertiary)
                    }
                }
            }
            .padding(16)
        }
    }

    // MARK: What was said before

    /// Past lines the call's words were already said in, newest on top. A
    /// row opens its meeting at that moment; the recording keeps going.
    @ViewBuilder
    private var recalled: some View {
        if let recall = store.shownRecall, recall.unavailable || !recall.cards.isEmpty {
            PageSection("From past meetings") {
                if recall.unavailable {
                    Text("Past meetings can't be read right now.").metaText(Theme.inkTertiary)
                }
                if !recall.cards.isEmpty {
                    Card {
                        ForEach(recall.cards) { match in
                            CardRow(action: { onOpenRecalled(match) }) {
                                recalledLine(match)
                            } trailing: {
                                Button { store.dismissRecall(match) } label: {
                                    Image(systemName: "xmark")
                                        .font(.system(size: 10, weight: .semibold))
                                        .foregroundStyle(Theme.inkTertiary)
                                        .frame(width: 20, height: 20)
                                        .contentShape(Rectangle())
                                }
                                .buttonStyle(.plain)
                                .accessibilityLabel("Dismiss")
                            }
                            .transition(.move(edge: .top).combined(with: .opacity))
                        }
                    }
                }
            }
            .animation(Motion.settle(reduced: reduceMotion), value: recall.cards.map(\.id))
        }
    }

    private func recalledLine(_ match: MeetingRecallMatch) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(match.kind.label, systemImage: match.kind.symbol)
                .metaText(Theme.inkSecondary)
            Text(recalledQuote(match))
                .bodyText(14)
                .lineLimit(4)
                .fixedSize(horizontal: false, vertical: true)
            if let person = match.person {
                HStack(spacing: 6) {
                    PersonPortrait(
                        personId: person.personId, name: person.displayName, size: 18,
                        revision: model.people.photoRevision)
                    Text(person.displayName).metaText(Theme.inkSecondary).lineLimit(1)
                }
            }
            AppLabel(bundleId: match.appBundleId, size: 14) {
                HStack(spacing: 0) {
                    Text(match.meetingTitle).lineLimit(1)
                    Text(" · \(match.meetingStartedAtUtcMs.meetingDate.short)").fixedSize()
                }
                .metaText()
            }
        }
    }

    /// The past line in quotes, the words both meetings share marked where
    /// a word starts.
    private func recalledQuote(_ match: MeetingRecallMatch) -> AttributedString {
        var line = AttributedString("“\(match.text)”")
        var cursor = line.startIndex
        while cursor < line.endIndex,
              let found = line[cursor...].range(
                  of: match.term, options: [.caseInsensitive, .diacriticInsensitive]) {
            if line.characters[..<found.lowerBound].last.map({ !$0.isLetter && !$0.isNumber }) ?? true {
                line[found].backgroundColor = Theme.accentSoft
                line[found].foregroundColor = Theme.accent
            }
            cursor = found.upperBound
        }
        return line
    }

    // MARK: A note about this moment

    /// The note is anchored at the clock when it is saved, which is why
    /// nothing here asks for a time. The sheet stays until the core has kept
    /// the words; a refusal is read here, over the draft, not behind it.
    private var note: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Note for this moment").titleText().accessibilityAddTraits(.isHeader)
            InputField(
                prompt: "Write a note for this moment",
                text: Binding(get: { store.noteBody }, set: { store.setNote($0) }))
            ErrorNote(store.error)
            HStack {
                Spacer()
                Button("Cancel") { noteOpen = false }.buttonStyle(.secondary)
                Button(noteSaving ? "Adding…" : "Add a note") {
                    noteSaving = true
                    store.createNote()
                }
                .buttonStyle(.primary)
                .disabled(
                    store.noteBody.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || store.pending != nil)
            }
        }
        .padding(24)
        .frame(width: 420)
        .background(Theme.page)
        .onChange(of: store.pending) { _, pending in
            guard noteSaving, pending == nil else { return }
            noteSaving = false
            if store.noteBody.isEmpty { noteOpen = false }
        }
        .onDisappear { noteSaving = false }
    }
}

// MARK: - The clock

/// A capture's clock, drawn. Running, it counts on once a second on a
/// schedule that starts at the clock's own start instant, so each tick lands
/// on a whole second of the meeting, and a redraw that hands it the same
/// clock keeps the same schedule. Standing, it is a still number.
struct MeetingClockText: View {
    let clock: MeetingCaptureClock
    var size: CGFloat = 12
    var color: Color = Theme.inkSecondary
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        if let since = clock.runningSince {
            TimelineView(.periodic(from: since, by: 1)) { context in
                face(clock.elapsedNs(at: context.date))
            }
        } else {
            face(clock.standingNs)
        }
    }

    /// The digits roll from one second to the next; with motion reduced,
    /// they change in place.
    private func face(_ elapsedNs: Int64) -> some View {
        let reading = elapsedNs.meetingOffsetClock
        return Text(reading)
            .font(TypeScale.mono(size))
            .foregroundStyle(color)
            .monospacedDigit()
            .contentTransition(reduceMotion ? .identity : .numericText())
            .animation(reduceMotion ? nil : .smooth(duration: 0.2), value: reading)
            .accessibilityLabel("Elapsed")
            .accessibilityValue(reading)
    }
}

private extension MeetingSessionSnapshot {
    /// When the capture started, as the live dot carries it: the same date
    /// on every redraw, so a redraw hands the dot the state it already has.
    var captureStart: Date { (startedAtUtcMs ?? 0).meetingDate }
}

// MARK: - The consent panel

extension MeetingConsentCard {
    /// A recording docks against the right edge of the screen; an offer, a
    /// prep and a wrap float at the top right.
    var docksAtEdge: Bool {
        switch self {
        case .active, .recording: true
        case .prompt, .prep, .wrap: false
        }
    }
}

struct MeetingConsentPanelView: View {
    let store: MeetingLiveStore

    /// Reading a brief, notes or a meeting is another slice's page.
    var onOpenBrief: (String) -> Void = { _ in }
    var onOpenNotes: (MeetingSessionId) -> Void = { _ in }

    /// The follow-up text lives with the notes, which this slice does not
    /// read. Unwired, the copy button is not drawn at all.
    var followUp: ((MeetingSessionId) async throws -> String)?

    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// The window slides; a card changing for another only crossfades.
    var body: some View {
        ZStack(alignment: .trailing) {
            if let card = store.card {
                surface(card)
                    .id(card.id)
                    .transition(.opacity)
            }
        }
        .animation(Theme.Slide.animation(reduced: reduceMotion), value: store.card?.id)
    }

    @ViewBuilder private func surface(_ card: MeetingConsentCard) -> some View {
        switch card {
        case let .prompt(prompt):
            PanelCard(store: store) { DetectionPromptView(store: store, prompt: prompt) }
                .padding(8)
        case let .active(state):
            MeetingRecordingDock(store: store, recording: .panel(state))
        case let .recording(event, recording):
            MeetingRecordingDock(store: store, recording: .ritual(event, recording))
        case let .prep(event, prep):
            PanelCard(store: store) {
                RitualPrepView(store: store, event: event, card: prep, onOpenBrief: onOpenBrief)
            }
            .padding(8)
        case let .wrap(event, wrap):
            PanelCard(store: store) {
                RitualWrapView(
                    store: store, event: event, card: wrap,
                    onOpenNotes: onOpenNotes, followUp: followUp)
            }
            .padding(8)
        }
    }
}

/// The panel's surface: its width, its inset, and a corner concentric with
/// the corner of a button inside that inset.
private enum PanelSurface {
    static let width: CGFloat = 320
    static let inset: CGFloat = 12
    static let radius = Theme.radiusControl + inset
}

private extension View {
    /// The panel's fill, hairline and shadow. Docked, the right side runs
    /// flush into the screen's edge: only the left corners round, and the
    /// hairline runs on past the edge.
    func panelSurface(docked: Bool, radius: CGFloat = PanelSurface.radius) -> some View {
        let shape = UnevenRoundedRectangle(
            topLeadingRadius: radius,
            bottomLeadingRadius: radius,
            bottomTrailingRadius: docked ? 0 : radius,
            topTrailingRadius: docked ? 0 : radius,
            style: .continuous)
        return background(Theme.surface, in: shape)
            .overlay(shape.strokeBorder(Theme.border, lineWidth: 1).padding(.trailing, docked ? -1 : 0))
            .shadow(color: .black.opacity(0.12), radius: 6, y: 2)
            .contentShape(shape)
    }
}

/// A card on the panel's surface, with the store's last error under it.
private struct PanelCard<Content: View>: View {
    let store: MeetingLiveStore
    var docked = false
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            content
            if let error = store.error {
                Text(error)
                    .bodyText(12, Theme.live)
                    .textSelection(.enabled)
            }
        }
        .padding(PanelSurface.inset)
        .frame(width: PanelSurface.width, alignment: .leading)
        .panelSurface(docked: docked)
    }
}

/// The recording a docked panel shows: one the panel started, or one Sona
/// started by itself.
private enum DockedRecording {
    case panel(MeetingConsentPanelSessionState)
    case ritual(RitualEvent, RitualRecordingCard)
}

/// A recording, docked flush against the right edge of the screen. At rest
/// it is a strip: the dot, the word and the clock, read upright. The pointer
/// on it pulls a drawer out with the title, the clock, a bookmark and Stop.
/// A click opens the whole card, which stays until it is collapsed, or
/// until the pointer leaves after a press in it.
private struct MeetingRecordingDock: View {
    let store: MeetingLiveStore
    let recording: DockedRecording
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var drawer = Drawer.shut
    /// Room for the drawer: made before the drawer comes out and given back
    /// once it is in, each in an update of its own. Inside the drawer's
    /// animation, SwiftUI would slide the strip across the room as it grows.
    @State private var wide = false
    /// The drawer that comes out once the room is made.
    @State private var pending: Drawer?
    @State private var shutting: Task<Void, Never>?
    @State private var narrowing: Task<Void, Never>?
    @State private var marked = false

    private enum Drawer: Equatable {
        case shut
        /// Pulled out while the pointer is on it.
        case peeking
        /// Opened by a click. Once a press lands in it, the pointer leaving
        /// shuts it.
        case open(pressed: Bool)
    }

    /// The strip and the drawer share a height, so the drawer covers the
    /// strip it comes out of and leaving one is leaving both.
    private static let height: CGFloat = 152
    private static let stripWidth: CGFloat = 32

    /// The strip is hidden, never removed: SwiftUI draws a view on its way
    /// out where it last sat, and the drawer comes out over it.
    var body: some View {
        ZStack(alignment: .trailing) {
            Color.clear
                .frame(width: wide ? PanelSurface.width : Self.stripWidth, height: Self.height)
            strip
                .opacity(drawer == .shut ? 1 : 0)
                .allowsHitTesting(drawer == .shut)
                .accessibilityHidden(drawer != .shut)
            switch drawer {
            case .shut:
                EmptyView()
            case .peeking:
                peek.transition(reduceMotion ? .opacity : .asymmetric(
                    insertion: .move(edge: .trailing),
                    removal: .move(edge: .trailing).combined(with: .opacity)))
            case .open:
                open.transition(.asymmetric(
                    insertion: .opacity,
                    removal: reduceMotion ? .opacity : .move(edge: .trailing)))
            }
        }
        .onGeometryChange(for: Bool.self) { $0.size.width > Self.stripWidth } action: { grown in
            guard grown, let pending else { return }
            self.pending = nil
            drawer = pending
        }
        .onPointer(inside: pointer, press: press)
        .animation(Theme.Slide.animation(reduced: reduceMotion), value: drawer)
        .padding([.vertical, .leading], 8)
    }

    private var strip: some View {
        VStack(spacing: 10) {
            LiveDot(state: dot)
            UprightText(word)
                .font(TypeScale.label(12))
                .foregroundStyle(paused ? Theme.inkSecondary : Theme.ink)
            if let clock {
                UprightText { MeetingClockText(clock: clock, size: 11) }
            }
            if silent != nil {
                Image(systemName: "speaker.slash")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Theme.ink)
                    .accessibilityLabel("No sound from the call")
            }
        }
        .frame(width: Self.stripWidth, height: Self.height)
        .panelSurface(docked: true, radius: Self.stripWidth / 2)
        .onTapGesture { pull(.open(pressed: false)) }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityAction { pull(.open(pressed: false)) }
    }

    private var peek: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                LiveDot(state: dot)
                Text(word)
                    .font(TypeScale.label(13))
                    .foregroundStyle(paused ? Theme.inkSecondary : Theme.ink)
                Spacer(minLength: 8)
                if let clock {
                    MeetingClockText(clock: clock)
                }
            }
            if let error = store.error {
                Text(error).bodyText(12, Theme.live).lineLimit(3)
            } else if let silent {
                Text("No sound from the call for \(silent.clock) while you talk.")
                    .bodyText(12, Theme.ink)
                    .lineLimit(3)
            } else {
                AppLabel(bundleId: appBundleId, size: 14, announcesApp: session != nil) {
                    Text(title)
                        .font(TypeScale.label(13))
                        .foregroundStyle(Theme.inkSecondary)
                        .lineLimit(2)
                }
            }
            Spacer(minLength: 0)
            HStack(spacing: 8) {
                Button(action: mark) {
                    Image(systemName: marked ? "bookmark.fill" : "bookmark")
                }
                .buttonStyle(.circle)
                .disabled(!canBookmark)
                .help("Bookmark this moment")
                .accessibilityLabel(marked ? "Bookmarked" : "Bookmark")
                Spacer(minLength: 8)
                Button("Stop", action: stop)
                    .buttonStyle(.compact)
                    .disabled(store.pending != nil)
            }
        }
        .padding(PanelSurface.inset)
        .frame(width: PanelSurface.width, height: Self.height, alignment: .topLeading)
        .panelSurface(docked: true)
        .onTapGesture { pull(.open(pressed: false)) }
    }

    private var open: some View {
        PanelCard(store: store, docked: true) {
            switch recording {
            case let .panel(state):
                MeetingConsentActiveCard(store: store, state: state, collapse: shut)
            case let .ritual(event, card):
                RitualRecordingView(store: store, event: event, card: card, collapse: shut)
            }
        }
    }

    private func pointer(_ inside: Bool) {
        shutting?.cancel()
        shutting = nil
        if inside {
            if drawer == .shut { pull(.peeking) }
        } else if drawer == .peeking || drawer == .open(pressed: true) {
            shutting = Task {
                try? await Task.sleep(for: .milliseconds(400))
                if !Task.isCancelled { shut() }
            }
        }
    }

    private func press() {
        if drawer == .open(pressed: false) { drawer = .open(pressed: true) }
    }

    private func pull(_ next: Drawer) {
        narrowing?.cancel()
        narrowing = nil
        if wide {
            drawer = next
        } else {
            pending = next
            wide = true
        }
    }

    private func shut() {
        shutting?.cancel()
        shutting = nil
        pending = nil
        drawer = .shut
        narrowing?.cancel()
        narrowing = Task {
            try? await Task.sleep(for: .seconds(reduceMotion ? Theme.Slide.fade : Theme.Slide.duration))
            if !Task.isCancelled { wide = false }
        }
    }

    private func mark() {
        Task {
            guard await store.bookmarkFromPanel() else { return }
            marked = true
            try? await Task.sleep(for: .seconds(1.5))
            marked = false
        }
    }

    private func stop() {
        switch recording {
        case .panel: store.stopFromPanel()
        case let .ritual(event, _): store.respond(event, action: .recordingStop)
        }
    }

    private var sessionId: MeetingSessionId {
        switch recording {
        case let .panel(state): state.snapshot.sessionId
        case let .ritual(_, card): card.sessionId
        }
    }

    /// The session as the panel reads it, while it is this recording.
    private var session: MeetingSessionSnapshot? {
        guard let snapshot = store.active?.snapshot, snapshot.sessionId == sessionId else { return nil }
        return snapshot
    }

    private var paused: Bool { session?.phase == .capturingPaused }
    private var word: String { paused ? "Paused" : "Recording" }
    private var dot: CaptureState { paused ? .idle : .recording(since: since) }
    private var clock: MeetingCaptureClock? { store.clocks[sessionId] }
    private var canBookmark: Bool { store.pending == nil && session?.phase == .capturingRecording }
    private var silent: MeetingCallSilent? {
        store.callSilent.flatMap { $0.sessionId == sessionId ? $0 : nil }
    }

    private var title: String {
        switch recording {
        case let .panel(state): state.snapshot.title
        case let .ritual(_, card): session?.title ?? card.appName
        }
    }

    /// The app an automatic recording started for. The panel's own capture
    /// carries no app.
    private var appBundleId: String? {
        switch recording {
        case .panel: nil
        case let .ritual(_, card): card.bundleId
        }
    }

    private var since: Date {
        switch recording {
        case let .panel(state): state.snapshot.captureStart
        case let .ritual(_, card): card.startedAtUtcMs.meetingDate
        }
    }
}

/// The offer: what would be recorded, the standing answers that go with
/// the press, and the two presses. The notice box is drawn only while the
/// recording notice is on in Settings, because only then can it be ticked;
/// its line says where the notice goes, which is what decides the tick.
struct DetectionPromptView: View {
    let store: MeetingLiveStore
    let prompt: DetectionPromptEvent

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                AppLabel(bundleId: prompt.prompt.bundleId, size: 18, announcesApp: prompt.prompt.appName == nil) {
                    Text(prompt.prompt.consentTitle)
                        .headlineText()
                        .lineLimit(3)
                }
                Text(
                    prompt.showIntroduction
                        ? "Sona records on this Mac and keeps you in control."
                        : "Audio stays on this Mac. Nothing joins the call."
                )
                .metaText(Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
                if let brief = store.seriesBrief {
                    Text(brief).bodyText(13, Theme.inkSecondary).lineLimit(2)
                }
            }
            choices
            HStack(spacing: 8) {
                Spacer()
                Button("Ignore") { store.answer(prompt, accepted: false) }
                    .buttonStyle(QuietButton(compact: true))
                    .disabled(store.pending != nil)
                Button(store.pending ?? "Record") { store.record(prompt) }
                    .buttonStyle(PrimaryButton(compact: true))
                    .disabled(store.pending != nil)
            }
        }
    }

    /// The standing answers. Only a calendar offer can record its series
    /// from now on.
    @ViewBuilder private var choices: some View {
        let noticeEnabled = store.settings?.meetingDisclosureEnabled == true
        if prompt.prompt.isCalendar || noticeEnabled {
            VStack(alignment: .leading, spacing: 4) {
                if prompt.prompt.isCalendar {
                    Toggle(
                        "Always record this meeting",
                        isOn: Binding(
                            get: { store.alwaysRecordSeries(prompt) },
                            set: { store.setAlwaysRecordSeries($0, for: prompt) }))
                }
                if noticeEnabled {
                    Toggle(
                        "Post a recording notice",
                        isOn: Binding(
                            get: { store.announceInChat(prompt) },
                            set: { store.setAnnounceInChat($0, for: prompt) }))
                    if store.announceInChat(prompt) {
                        Text("Sent only to an identified empty meeting chat. Otherwise, copy it yourself.")
                            .metaText()
                            .padding(.leading, 20)
                    }
                }
            }
            .toggleStyle(.checkbox)
            .font(TypeScale.body(13))
        }
    }
}

/// The session the panel is watching, in one row: the dot, the word, the
/// clock, Stop, and the `›` that tucks the card back into its strip. The
/// dot carries the colour; the word beside it is plain ink. The title sits
/// under the row, and so does the chat notice's outcome when the person
/// asked for one, because it asks them to send it. Telling this series to
/// stop recording itself waits behind the `…`.
struct MeetingConsentActiveCard: View {
    let store: MeetingLiveStore
    let state: MeetingConsentPanelSessionState
    let collapse: () -> Void

    var body: some View {
        let paused = state.snapshot.phase == .capturingPaused
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                LiveDot(state: paused ? .idle : .recording(since: state.snapshot.captureStart))
                Text(paused ? "Paused" : "Recording")
                    .font(TypeScale.label(13))
                    .foregroundStyle(paused ? Theme.inkSecondary : Theme.ink)
                if let clock = store.clocks[state.snapshot.sessionId] {
                    MeetingClockText(clock: clock)
                }
                Spacer(minLength: 8)
                Button("Stop") { store.stopFromPanel() }
                    .buttonStyle(.compact)
                    .disabled(store.pending != nil)
                if state.standingSeriesKey != nil {
                    PanelMore {
                        Button("Forget this series") { store.forgetSeries() }
                            .disabled(store.pending != nil)
                    }
                }
                PanelCollapse(action: collapse)
            }
            Text(state.snapshot.title)
                .font(TypeScale.label(13))
                .foregroundStyle(Theme.inkSecondary)
                .lineLimit(2)
            MeetingDisclosureStatus(store: store, disclosure: state.disclosure)
        }
    }
}

// MARK: - The rituals

/// A running recording Sona started by itself, in the same row as the
/// panel's own, with the app it is recording under it. The one answer that
/// keeps it from doing that again for this app waits behind the `…`.
struct RitualRecordingView: View {
    let store: MeetingLiveStore
    let event: RitualEvent
    let card: RitualRecordingCard
    let collapse: () -> Void

    var body: some View {
        // The card is an event; the phase is the session's. Paused is the
        // one state the card would otherwise misreport.
        let paused = store.active?.snapshot.sessionId == card.sessionId
            && store.active?.snapshot.phase == .capturingPaused
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                LiveDot(state: paused ? .idle : .recording(since: card.startedAtUtcMs.meetingDate))
                Text(paused ? "Paused" : "Recording started")
                    .font(TypeScale.label(13))
                    .foregroundStyle(paused ? Theme.inkSecondary : Theme.ink)
                if let clock = store.clocks[card.sessionId] {
                    MeetingClockText(clock: clock)
                }
                Spacer(minLength: 8)
                Button("Stop") { store.respond(event, action: .recordingStop) }
                    .buttonStyle(.compact)
                    .disabled(store.pending != nil)
                PanelMore {
                    Button("Don't record this app automatically") {
                        store.respond(event, action: .recordingForgetApp)
                    }
                    .disabled(store.pending != nil)
                }
                PanelCollapse(action: collapse)
            }
            AppLabel(bundleId: card.bundleId, size: 14) {
                Text(card.appName)
                    .font(TypeScale.label(13))
                    .foregroundStyle(Theme.inkSecondary)
                    .lineLimit(1)
            }
            if let active = store.active, active.snapshot.sessionId == card.sessionId {
                MeetingDisclosureStatus(store: store, disclosure: active.disclosure)
            }
        }
    }
}

/// The `…` on a recording card: the answer the card keeps but rarely needs,
/// out of the row, so the row is the recording and its Stop.
private struct PanelMore<Items: View>: View {
    @ViewBuilder let items: Items

    var body: some View {
        Menu {
            items
        } label: {
            Image(systemName: "ellipsis")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: 26, height: 30)
        }
        .menuStyle(.borderlessButton)
        .tint(Theme.inkSecondary)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("More")
    }
}

/// The `›` on a docked card: back into the strip at the screen's edge.
private struct PanelCollapse: View {
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: "chevron.right")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: 22, height: 30)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Collapse")
        .accessibilityLabel("Collapse")
    }
}

/// Before a meeting: what happened last time, what is still open, and who is
/// coming. `RitualCards.tsx` shows the first two loops and counts the rest.
struct RitualPrepView: View {
    let store: MeetingLiveStore
    let event: RitualEvent
    let card: RitualPrepCard
    var onOpenBrief: (String) -> Void = { _ in }
    @Environment(AppModel.self) private var model
    @State private var preview = BriefPreview.loading
    /// Your last talks with the first participant whose recorded moments
    /// have audio to play.
    @State private var lastTime: LastTime?

    private struct LastTime {
        let name: String
        let reel: ConversationReel
    }

    /// What the card shows of the brief. Briefs are prepared ahead of time,
    /// so most cards read a saved one.
    private enum BriefPreview {
        case loading
        case ready([MeetingBriefPoint])
        case failed(String)
    }

    private var minutes: Int {
        max(0, Int((card.startUtcMs.meetingDate.timeIntervalSinceNow / 60).rounded()))
    }

    private var reelKey: String { "prep:\(card.eventKey)" }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("\(card.title) — in \(minutes) \(minutes == 1 ? "minute" : "minutes")")
                    .headlineText()
                Spacer(minLength: 8)
                Text("Prep").metaText()
            }
            if !card.headline.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    Text(card.lastMeetingId == nil ? "Context:" : "Last time:").metaText()
                    Text(card.headline).bodyText(13, Theme.inkSecondary)
                }
            }
            if card.mineOpenLoopCount > 0 {
                VStack(alignment: .leading, spacing: 4) {
                    Text("My open loops (\(card.mineOpenLoopCount))").metaText()
                    ForEach(Array(card.mineOpenLoops.prefix(2).enumerated()), id: \.offset) {
                        _, loop in
                        Text(loop).bodyText(13, Theme.inkSecondary)
                    }
                }
            }
            if card.waitingOnCount > 0 {
                Text("Waiting on (\(card.waitingOnCount))").metaText()
            }
            if !card.participants.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Participants:").metaText()
                    ForEach(Array(card.participants.enumerated()), id: \.offset) { _, person in
                        Text(person.line).bodyText(13, Theme.inkSecondary)
                    }
                }
            }
            if let lastTime {
                lastTimeLine(lastTime)
            }
            briefSection
            HStack(spacing: 8) {
                Spacer()
                Button("Open brief") {
                    store.respond(event, action: .prepOpenBrief)
                    onOpenBrief(card.eventKey)
                }
                .buttonStyle(QuietButton(compact: true))
                .disabled(store.pending != nil)
                if card.canRecordWhenStarts {
                    Button("Record when it starts") {
                        store.respond(event, action: .prepRecordWhenStarts)
                    }
                    .buttonStyle(PrimaryButton(compact: true))
                    .disabled(store.pending != nil)
                } else {
                    Button("Record") {
                        store.respond(event, action: .prepRecord)
                    }
                    .buttonStyle(PrimaryButton(compact: true))
                    .disabled(store.pending != nil)
                }
            }
        }
        .task(id: card.eventKey) {
            preview = .loading
            do {
                let brief = try await model.prep.preview(card.eventKey)
                preview = .ready(brief.content.highlights + brief.content.agenda)
            } catch {
                preview = .failed(error.localizedDescription)
            }
        }
        .task(id: card.eventKey) {
            lastTime = nil
            for participant in card.participants {
                guard let personId = participant.personId,
                      let reel: ConversationReel = try? await model.core.request(
                          "conversation_reel_for_person", ["personId": personId]),
                      reel.moments.contains(where: \.said.audioRetained) else { continue }
                guard !Task.isCancelled else { return }
                lastTime = LastTime(name: participant.name, reel: reel)
                return
            }
        }
        .onDisappear {
            if model.meetings.audio.reelKey == reelKey { model.meetings.audio.stop() }
        }
    }

    /// "Your talks with Ada · 0:48": one press plays the moments.
    private func lastTimeLine(_ lastTime: LastTime) -> some View {
        let audio = model.meetings.audio
        let sounding = audio.reelKey == reelKey
        let total = lastTime.reel.moments.reduce(UInt64(0)) { $0 + $1.playableNs }
        return VStack(alignment: .leading, spacing: 2) {
            Button {
                if sounding {
                    audio.stop()
                } else {
                    audio.playReel(lastTime.reel.moments.map(\.clip), key: reelKey)
                }
            } label: {
                HStack(spacing: 6) {
                    Text("Your talks with \(lastTime.name) · \(Int64(clamping: total).meetingOffsetClock)")
                    Image(systemName: sounding ? "stop.fill" : "play.fill")
                        .font(.system(size: 10))
                        .accessibilityHidden(true)
                }
            }
            .buttonStyle(QuietButton(color: Theme.accent, compact: true))
            .disabled(audio.heldForRecording)
            .accessibilityValue(sounding ? "Playing" : "")
            if audio.heldForRecording {
                Text(ConversationAudioPlayback.heldNotice).metaText()
            } else if audio.failedReelKey == reelKey, let error = audio.error {
                Text(error).metaText(error == ConversationAudioPlayback.heldNotice ? Theme.inkTertiary : Theme.live)
            }
        }
    }

    @ViewBuilder private var briefSection: some View {
        switch preview {
        case .loading:
            Text("Preparing the brief…").metaText()
        case let .ready(points) where points.isEmpty:
            Text("The brief has nothing to add yet.").metaText()
        case let .ready(points):
            VStack(alignment: .leading, spacing: 4) {
                Text("From the brief:").metaText()
                ForEach(Array(points.prefix(2).enumerated()), id: \.offset) { _, point in
                    Text(point.text).bodyText(13, Theme.inkSecondary).lineLimit(2)
                }
            }
        case let .failed(message):
            Text(message).bodyText(13, Theme.live).lineLimit(2)
        }
    }
}

/// After a meeting: the one line it came to, what it left open, and the two
/// things a person does with that.
struct RitualWrapView: View {
    let store: MeetingLiveStore
    let event: RitualEvent
    let card: RitualWrapCard
    var onOpenNotes: (MeetingSessionId) -> Void = { _ in }
    var followUp: ((MeetingSessionId) async throws -> String)?

    /// What the copy button says: the draft in flight, the copy that landed,
    /// or the offer.
    private var followUpLabel: String {
        if store.followUpDrafting { return "Drafting…" }
        return store.followUpCopied ? "Copied" : "Copy follow-up"
    }

    /// The counts, in the order `RitualCards.tsx` reads them, and only the
    /// ones that are not zero.
    private var delta: String {
        var parts: [String] = []
        if card.followUpCount > 0 {
            parts.append(
                card.followUpCount == 1 ? "1 follow-up" : "\(card.followUpCount) follow-ups")
        }
        if card.waitingOnCount > 0 {
            if card.waitingOnNames.count == 1 {
                parts.append("\(card.waitingOnCount) waiting on \(card.waitingOnNames[0])")
            } else {
                parts.append("Waiting on (\(card.waitingOnCount))")
            }
        }
        if let speakers = card.unresolvedSpeakerCount, speakers > 0 {
            parts.append(
                speakers == 1 ? "1 speaker to label" : "\(speakers) speakers to label")
        }
        return parts.joined(separator: " · ")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("\(card.title) — saved").headlineText()
                Spacer(minLength: 8)
                Text("Wrap").metaText()
            }
            if !card.headline.isEmpty {
                Text(card.headline).bodyText(13, Theme.inkSecondary)
            }
            if !delta.isEmpty {
                Text(delta).metaText()
            }
            HStack(spacing: 8) {
                Spacer()
                Button("Open notes") {
                    store.respond(event, action: .wrapOpenNotes)
                    onOpenNotes(card.sessionId)
                }
                .buttonStyle(QuietButton(compact: true))
                .disabled(store.pending != nil)
                if let followUp {
                    Button(followUpLabel) {
                        store.copyFollowUp(event, for: card.sessionId, draft: followUp)
                    }
                    .buttonStyle(.compact)
                    .disabled(store.pending != nil || store.followUpDrafting)
                }
                Button("Done") { store.respond(event, action: .wrapDone) }
                    .buttonStyle(PrimaryButton(compact: true))
                    .disabled(store.pending != nil)
            }
        }
    }
}

// MARK: - A ritual on its own

/// The rituals as the shell presents them when there is no panel: one card,
/// the one the store picked.
struct RitualView: View {
    let store: MeetingLiveStore
    var onOpenBrief: (String) -> Void = { _ in }
    var onOpenNotes: (MeetingSessionId) -> Void = { _ in }
    var followUp: ((MeetingSessionId) async throws -> String)?

    var body: some View {
        MeetingConsentPanelView(
            store: store, onOpenBrief: onOpenBrief, onOpenNotes: onOpenNotes,
            followUp: followUp)
    }
}
