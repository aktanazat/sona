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

    @State private var noteOpen = false
    /// Set by the Add press; the sheet closes when the core has kept the note
    /// and stays open, draft intact, when it refused.
    @State private var noteSaving = false
    @State private var discardOpen = false
    @State private var highlightedLine: String?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var session: MeetingSessionSnapshot? { store.live?.session }

    var body: some View {
        GeometryReader { geometry in
            ScrollViewReader { proxy in
                if let session {
                    if geometry.size.width >= 820 {
                        VStack(alignment: .leading, spacing: 0) {
                            captureHeader(session)
                            HStack(alignment: .top, spacing: 24) {
                                ScrollView { transcript }
                                    .scrollIndicators(.never)
                                    .frame(maxWidth: .infinity)
                                ScrollView {
                                    ownNotes
                                    help(proxy)
                                }
                                .scrollIndicators(.never)
                                .frame(width: 320)
                            }
                        }
                        .frame(maxWidth: Theme.contentMax, maxHeight: .infinity, alignment: .topLeading)
                        .padding(.horizontal, Theme.margin)
                        .padding(.top, 40)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                    } else {
                        Page {
                            captureHeader(session)
                            ownNotes
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
        HStack(alignment: .firstTextBaseline, spacing: 16) {
            Text(session.title).titleText().lineLimit(1)
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
            HStack(spacing: 6) {
                LiveDot(state: dot(session))
                if let clock = store.clocks[session.sessionId] {
                    MeetingClockText(clock: clock, size: 13, color: Theme.ink)
                }
            }
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
        .padding(.bottom, 24)
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
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: 28, height: 28)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("More")
    }

    // MARK: What is worth saying about the capture

    @ViewBuilder
    private var warnings: some View {
        if !store.warnings.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(store.warnings.enumerated()), id: \.offset) { _, warning in
                    Text(warning.text)
                        .bodyText(14, warning.urgent ? Theme.live : Theme.inkSecondary)
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
                    ForEach(Array(store.provisional.enumerated()), id: \.offset) { index, segment in
                        line(at: segment.startOffsetNs, segment.text, anchor: "live-\(index)")
                    }
                    Text("Provisional. The final transcript is written when the meeting ends.")
                        .metaText(Theme.inkTertiary)
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
                            anchor: "stored-\(segment.base.segmentId)")
                    }
                }
            }
        }
    }

    private func line(at offsetNs: Int64, _ text: String, anchor: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 20) {
            Text(offsetNs.meetingOffsetClock)
                .font(TypeScale.mono(12))
                .foregroundStyle(Theme.inkTertiary)
                .frame(width: 62, alignment: .trailing)
            Text(text).bodyText(14)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.vertical, 4)
        .background(highlightedLine == anchor ? Theme.accentSoft : Color.clear)
        .id(anchor)
    }

    private func jump(to offset: Int64, using proxy: ScrollViewProxy) {
        let anchor: String
        if store.showsProvisional {
            guard let index = store.provisional.firstIndex(where: { $0.startOffsetNs == offset })
                ?? store.provisional.lastIndex(where: { $0.startOffsetNs <= offset }) else { return }
            anchor = "live-\(index)"
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
                    Text(kind.label).bodyText(14)
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
                    Button("Try again") { store.retryLiveHelp() }.buttonStyle(.secondary)
                } else if let answer = store.liveHelp {
                    if let label = answer.state.label {
                        Text(label).bodyText(13, Theme.inkSecondary)
                        Button("Try again") { store.retryLiveHelp() }.buttonStyle(.secondary)
                    } else {
                        ForEach(Array(answer.items.enumerated()), id: \.offset) { _, item in
                            VStack(alignment: .leading, spacing: 4) {
                                Text(item.text).bodyText(14).textSelection(.enabled)
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
            .padding(14)
        }
    }

    // MARK: A note about this moment

    /// The note is anchored at the clock when it is saved, which is why
    /// nothing here asks for a time. The sheet stays until the core has kept
    /// the words; a refusal is read here, over the draft, not behind it.
    private var note: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Note for this moment").headlineText()
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
        .padding(28)
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

/// The floating panel, which is the card: one surface at the panel radius,
/// 340 wide as the Tauri window was, with exactly one card's rows on it.
/// `ConsentPanel.tsx` decides which in one order — a recording ritual, then
/// the session it is recording, then an offer, then prep, then wrap — and
/// the store computes it, so this view only draws what it is handed.
struct MeetingConsentPanelView: View {
    let store: MeetingLiveStore

    /// Reading a brief, notes or a meeting is another slice's page.
    var onOpenBrief: (String) -> Void = { _ in }
    var onOpenNotes: (MeetingSessionId) -> Void = { _ in }

    /// The follow-up text lives with the notes, which this slice does not
    /// read. Unwired, the copy button is not drawn at all.
    var followUp: ((MeetingSessionId) async throws -> String)?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            switch store.card {
            case let .prompt(prompt):
                DetectionPromptView(store: store, prompt: prompt)
            case let .active(state):
                MeetingConsentActiveCard(store: store, state: state)
            case let .recording(event, card):
                RitualRecordingView(store: store, event: event, card: card)
            case let .prep(event, card):
                RitualPrepView(store: store, event: event, card: card, onOpenBrief: onOpenBrief)
            case let .wrap(event, card):
                RitualWrapView(
                    store: store, event: event, card: card,
                    onOpenNotes: onOpenNotes, followUp: followUp)
            case .none:
                EmptyView()
            }
            ErrorNote(store.error)
        }
        .padding(16)
        .frame(width: 340)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusPanel))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusPanel).strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// The offer itself: what would be recorded, the two standing answers, and the
/// two presses. `showIntroduction` is the first offer on this Mac, which is
/// the only time the panel explains itself.
struct DetectionPromptView: View {
    let store: MeetingLiveStore
    let prompt: DetectionPromptEvent

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(prompt.prompt.consentTitle).headlineText()
            Text(
                prompt.showIntroduction
                    ? "Sona records on this Mac and keeps you in control."
                    : "Audio stays on this Mac. Nothing joins the call."
            )
            .bodyText(13, Theme.inkSecondary)
            if let brief = store.seriesBrief {
                Text(brief).bodyText(13, Theme.inkSecondary)
            }
            VStack(alignment: .leading, spacing: 4) {
                if prompt.prompt.isCalendar {
                    Toggle(
                        "Always record this meeting",
                        isOn: Binding(
                            get: { store.alwaysRecordSeries(prompt) },
                            set: { store.setAlwaysRecordSeries($0, for: prompt) }))
                }
                Toggle(
                    "Post a recording notice",
                    isOn: Binding(
                        get: { store.announceInChat(prompt) },
                        set: { store.setAnnounceInChat($0, for: prompt) }))
                .disabled(store.settings?.meetingDisclosureEnabled != true)
                if store.announceInChat(prompt) {
                    Text("Sent only to an identified empty meeting chat. Otherwise, copy it yourself.")
                        .metaText(Theme.inkTertiary)
                        .padding(.leading, 20)
                }
            }
            .toggleStyle(.checkbox)
            .font(TypeScale.body(13))
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
}

/// The session the panel is watching, in one row: the dot, the word, the
/// clock, and Stop. The dot carries the colour; the word beside it is plain
/// ink. The title sits under the row, and so does the chat notice's outcome
/// when the person asked for one, because it asks them to send it. Telling
/// this series to stop recording itself waits behind the `…`.
struct MeetingConsentActiveCard: View {
    let store: MeetingLiveStore
    let state: MeetingConsentPanelSessionState

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
            }
            Text(state.snapshot.title)
                .font(TypeScale.label(13))
                .foregroundStyle(Theme.inkSecondary)
                .lineLimit(1)
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
            }
            Text(card.appName)
                .font(TypeScale.label(13))
                .foregroundStyle(Theme.inkSecondary)
                .lineLimit(1)
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
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("More")
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

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Prep").metaText(Theme.accent)
            Text("\(card.title) — in \(minutes) \(minutes == 1 ? "minute" : "minutes")").bodyText(14)
            if !card.headline.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    Text(card.lastMeetingId == nil ? "Context:" : "Last time:").metaText()
                    Text(card.headline).bodyText(13, Theme.inkSecondary)
                }
            }
            if card.mineOpenLoopCount > 0 {
                VStack(alignment: .leading, spacing: 2) {
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
                VStack(alignment: .leading, spacing: 2) {
                    Text("Participants:").metaText()
                    ForEach(Array(card.participants.enumerated()), id: \.offset) { _, person in
                        Text(person.line).bodyText(13, Theme.inkSecondary)
                    }
                }
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
    }

    @ViewBuilder private var briefSection: some View {
        switch preview {
        case .loading:
            Text("Preparing the brief…").metaText()
        case let .ready(points) where points.isEmpty:
            Text("The brief has nothing to add yet.").metaText()
        case let .ready(points):
            VStack(alignment: .leading, spacing: 2) {
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
        VStack(alignment: .leading, spacing: 10) {
            Text("Wrap").metaText(Theme.accent)
            Text("\(card.title) — saved").bodyText(14)
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
