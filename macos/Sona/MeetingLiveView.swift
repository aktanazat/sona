import SwiftUI

/// Capture, while it runs, and the small surfaces that lead into it: the
/// consent panel, a detection prompt, and the prep/wrap/recording rituals.
/// `MeetingLive.tsx`, `ConsentPanel.tsx` and `RitualCards.tsx`.

// MARK: - The live screen

/// The screen a running meeting owns: the title, the clock, Stop, and the
/// words as they arrive. Round 7 took the telemetry off this page — a signal
/// reading capture never publishes, and a durability lag nobody acts on — so
/// what is left is what a person watches.
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

    private var session: MeetingSessionSnapshot? { store.live?.session }

    var body: some View {
        Page {
            if let session {
                header(session)
                ErrorNote(store.error)
                warnings
                transcript
            } else {
                Text("This meeting is no longer recording.")
                    .bodyText(14, Theme.inkSecondary)
            }
        }
        .sheet(isPresented: $noteOpen) { note }
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
            // The core reports the offset once per read; the screen counts on
            // from it every second while the capture runs, and stands still
            // while it is paused.
            TimelineView(.periodic(from: .now, by: 1)) { context in
                HStack(spacing: 6) {
                    LiveDot(state: dot(session))
                    Text(store.elapsed(at: context.date))
                        .font(TypeScale.mono(13))
                        .foregroundStyle(Theme.ink)
                        .monospacedDigit()
                }
            }
            .accessibilityLabel("Elapsed")
            Button("Stop") { store.stop() }
                .buttonStyle(.primary)
                .disabled(!session.allows(.stop) || store.pending != nil)
            menu(session)
        }
        .padding(.bottom, 24)
    }

    /// Red while recording, a ring while paused, ink while the core is
    /// stopping or processing.
    private func dot(_ session: MeetingSessionSnapshot) -> CaptureState {
        switch session.phase {
        case .capturingRecording: .recording(since: Date())
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
                    ForEach(Array(store.provisional.enumerated()), id: \.offset) { _, segment in
                        line(at: segment.startOffsetNs, segment.text)
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
                        line(at: segment.base.startOffsetNs, segment.text)
                    }
                }
            }
        }
    }

    private func line(at offsetNs: Int64, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 20) {
            Text(offsetNs.meetingOffsetClock)
                .font(TypeScale.mono(12))
                .foregroundStyle(Theme.inkTertiary)
                .frame(width: 62, alignment: .trailing)
            Text(text).bodyText(14)
                .frame(maxWidth: .infinity, alignment: .leading)
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

// MARK: - The consent panel

/// The floating panel, with exactly one card on it. `ConsentPanel.tsx` decides
/// that in one order — a recording ritual, then the session it is recording,
/// then an offer, then prep, then wrap — and the store computes it, so this
/// view only draws what it is handed.
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
        .frame(width: 360)
        .padding(16)
    }
}

/// The offer itself: what would be recorded, the two standing answers, and the
/// two presses. `showIntroduction` is the first offer on this Mac, which is
/// the only time the panel explains itself.
struct DetectionPromptView: View {
    let store: MeetingLiveStore
    let prompt: DetectionPromptEvent

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 12) {
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
                VStack(alignment: .leading, spacing: 6) {
                    if prompt.prompt.isCalendar {
                        Toggle(
                            "Always record this meeting",
                            isOn: Binding(
                                get: { store.alwaysRecordSeries(prompt) },
                                set: { store.setAlwaysRecordSeries($0, for: prompt) }))
                    }
                    Toggle(
                        "Put a notice in the chat",
                        isOn: Binding(
                            get: { store.announceInChat(prompt) },
                            set: { store.setAnnounceInChat($0, for: prompt) }))
                    if store.announceInChat(prompt) {
                        Text("Typed into the meeting's chat box for you to send.")
                            .metaText(Theme.inkTertiary)
                            .padding(.leading, 20)
                    }
                }
                .toggleStyle(.checkbox)
                .font(TypeScale.body(13))
                HStack {
                    Spacer()
                    Button("Ignore") { store.answer(prompt, accepted: false) }
                        .buttonStyle(.secondary)
                        .disabled(store.pending != nil)
                    Button(store.pending ?? "Record") { store.record(prompt) }
                        .buttonStyle(.primary)
                        .disabled(store.pending != nil)
                }
            }
            .padding(16)
        }
    }
}

/// The session the panel is watching: the clock, and the two things a person
/// does from here — stop it, or stop it recording this series by itself.
struct MeetingConsentActiveCard: View {
    let store: MeetingLiveStore
    let state: MeetingConsentPanelSessionState

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 8) {
                    let paused = state.snapshot.phase == .capturingPaused
                    LiveDot(state: paused ? .idle : .recording(since: Date()))
                    Text(paused ? "Paused" : "Recording").metaText(paused ? Theme.inkSecondary : Theme.live)
                    Spacer()
                    Text((state.snapshot.elapsedOffsetNs ?? 0).meetingOffsetClock)
                        .font(TypeScale.mono(12))
                        .foregroundStyle(Theme.inkSecondary)
                }
                Text(state.snapshot.title).bodyText(14)
                if let line = state.disclosure.outcomeLine {
                    Text(line).bodyText(12, Theme.inkSecondary)
                }
                HStack {
                    if state.standingSeriesKey != nil {
                        Button("Forget this series") { store.forgetSeries() }
                            .buttonStyle(.quiet)
                            .disabled(store.pending != nil)
                    }
                    Spacer()
                    Button("Stop") { store.stopFromPanel() }
                        .buttonStyle(.primary)
                        .disabled(store.pending != nil)
                }
            }
            .padding(16)
        }
    }
}

// MARK: - The rituals

/// A running recording Sona started by itself, and the one answer that keeps
/// it from doing that again for this app.
struct RitualRecordingView: View {
    let store: MeetingLiveStore
    let event: RitualEvent
    let card: RitualRecordingCard

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 8) {
                    // The card is an event; the phase is the session's. Paused
                    // is the one state the card would otherwise misreport.
                    let paused = store.active?.snapshot.sessionId == card.sessionId
                        && store.active?.snapshot.phase == .capturingPaused
                    LiveDot(state: paused ? .idle : .recording(since: Date()))
                    Text(paused ? "Paused" : "Recording started")
                        .metaText(paused ? Theme.inkSecondary : Theme.live)
                    Spacer()
                    Text(card.startedAtUtcMs.meetingElapsed(since: Date()))
                        .font(TypeScale.mono(12))
                        .foregroundStyle(Theme.inkSecondary)
                }
                Text(card.appName).bodyText(14)
                HStack {
                    Button("Don't record this app automatically") {
                        store.respond(event, action: .recordingForgetApp)
                    }
                    .buttonStyle(.quiet)
                    .disabled(store.pending != nil)
                    Spacer()
                    Button("Stop") { store.respond(event, action: .recordingStop) }
                        .buttonStyle(.primary)
                        .disabled(store.pending != nil)
                }
            }
            .padding(16)
        }
    }
}

/// Before a meeting: what happened last time, what is still open, and who is
/// coming. `RitualCards.tsx` shows the first two loops and counts the rest.
struct RitualPrepView: View {
    let store: MeetingLiveStore
    let event: RitualEvent
    let card: RitualPrepCard
    var onOpenBrief: (String) -> Void = { _ in }

    private var minutes: Int {
        max(0, Int((card.startUtcMs.meetingDate.timeIntervalSinceNow / 60).rounded()))
    }

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 10) {
                Text("Prep").metaText(Theme.accent)
                Text("\(card.title) — in \(minutes) minutes").bodyText(14)
                if !card.headline.isEmpty {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Last time:").metaText()
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
                HStack {
                    Button("Open brief") {
                        store.respond(event, action: .prepOpenBrief)
                        onOpenBrief(card.lastMeetingId)
                    }
                    .buttonStyle(.secondary)
                    .disabled(store.pending != nil)
                    Spacer()
                    if card.canRecordWhenStarts {
                        Button("Record when it starts") {
                            store.respond(event, action: .prepRecordWhenStarts)
                        }
                        .buttonStyle(.primary)
                        .disabled(store.pending != nil)
                    }
                }
            }
            .padding(16)
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
        Card {
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
                    Button("Open notes") {
                        store.respond(event, action: .wrapOpenNotes)
                        onOpenNotes(card.sessionId)
                    }
                    .buttonStyle(.secondary)
                    .disabled(store.pending != nil)
                    if let followUp {
                        Button(followUpLabel) {
                            store.copyFollowUp(event, for: card.sessionId, draft: followUp)
                        }
                        .buttonStyle(.secondary)
                        .disabled(store.pending != nil || store.followUpDrafting)
                    }
                    Spacer()
                    Button("Done") { store.respond(event, action: .wrapDone) }
                        .buttonStyle(.primary)
                        .disabled(store.pending != nil)
                }
            }
            .padding(16)
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
