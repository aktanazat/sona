import AppKit
import Foundation
import Observation
import UniformTypeIdentifiers

// MARK: - What a surface shows

/// The one card the consent panel draws, in the order `ConsentPanel.tsx` picks
/// them: a capture that started itself beats everything, then the recording
/// this panel started, then an offer waiting for an answer, then prep, then
/// wrap. One card, because the panel is a small floating surface and two
/// stacked cards on it is how a person ends up answering the wrong one.
enum MeetingConsentCard: Identifiable {
    case recording(RitualEvent, RitualRecordingCard)
    case active(MeetingConsentPanelSessionState)
    case prompt(DetectionPromptEvent)
    case prep(RitualEvent, RitualPrepCard)
    case wrap(RitualEvent, RitualWrapCard)

    var id: String {
        switch self {
        case let .recording(event, _): "recording:\(event.ritualId)"
        case let .active(state): "active:\(state.snapshot.sessionId)"
        case let .prompt(prompt): "prompt:\(prompt.promptId)"
        case let .prep(event, _): "prep:\(event.ritualId)"
        case let .wrap(event, _): "wrap:\(event.ritualId)"
        }
    }
}

/// What a source row says about one lane, out of the two states the core
/// keeps. `MeetingStatus.tsx`: availability is whether the lane is allowed to
/// do anything, and only then is health what it is doing.
struct MeetingSourceReading {
    let name: String
    let required: Bool
    let word: String
    /// A blocked lane and a failed one are the two a person has to act on.
    let urgent: Bool
    /// The core counted interruptions on this track.
    let missingAudio: Bool

    init(_ source: MeetingSourceSnapshot, sealed: Bool) {
        name = source.sourceKind.label
        required = source.required
        missingAudio = source.gapCount > 0
        if source.availability != .available {
            word = source.availability.label
            urgent = true
            return
        }
        let recorded = sealed && (source.health == .healthy || source.health == .degraded)
        word = recorded ? "Recorded" : MeetingSourceReading.health(source.health)
        urgent = source.health == .failed
    }

    /// `SOURCE_STATE_KEYS`: the words describe a recording, not a subsystem.
    private static func health(_ health: MeetingSourceHealth) -> String {
        switch health {
        case .notStarted: "Ready"
        case .starting: "Starting"
        case .healthy, .degraded: "Recording"
        case .failed: "Failed"
        }
    }
}

/// The one line the live screen shows about what is being recorded, when there
/// is something to say. Nothing while the capture is whole.
struct MeetingLiveWarning {
    let text: String
    /// Storage is the failure; a partial capture is a caveat.
    let urgent: Bool
}

/// A capture's clock, as every surface counts it. The core reports how far
/// the capture has durably written, which trails the recording by however
/// full the current chunk is, so each read implies a start a little late,
/// and late by a different amount each time. The clock keeps one start
/// instead, and a read that trails leaves it alone: while the capture
/// records, the count only moves forward, and only for a read more than a
/// second ahead of it. A start, a pause, a resume or a stop is where the
/// count may meet the core's number again.
struct MeetingCaptureClock: Equatable {
    /// How far a read must be from the count before it moves the count.
    /// Under a second, the ticks stay on the seconds they were on.
    private static let drift: Int64 = 1_000_000_000

    /// While the capture records: the instant its offset read zero, as if
    /// it had run straight through. Nil while it stands still.
    let runningSince: Date?
    /// While it stands still: the offset it stands at.
    let standingNs: Int64

    /// The clock after `session` was read at `now`; `prior` is the same
    /// session's clock before the read.
    init(_ session: MeetingSessionSnapshot, readAt now: Date, after prior: MeetingCaptureClock?) {
        let reported = session.elapsedOffsetNs ?? 0
        let recording = session.phase == .capturingRecording
        if recording, let anchor = prior?.runningSince {
            let shown = anchor.distance(to: now).nanoseconds
            runningSince = reported - shown > Self.drift ? now.addingTimeInterval(-reported.seconds) : anchor
            standingNs = 0
            return
        }
        // A start, a pause, a resume, a stop, or another read while it
        // stands: the count carries on from what is showing, unless the
        // core's number is more than a second away from it.
        let shown = prior?.elapsedNs(at: now) ?? reported
        let count = abs(reported - shown) > Self.drift ? reported : shown
        runningSince = recording ? now.addingTimeInterval(-count.seconds) : nil
        standingNs = recording ? 0 : count
    }

    /// A recording a ritual card announced before any read of its session:
    /// counted from the wall-clock instant it started. A start that is
    /// missing or zero is not one to count from.
    init(startedAtUtcMs: Int64) {
        runningSince = startedAtUtcMs > 0 ? startedAtUtcMs.meetingDate : nil
        standingNs = 0
    }

    /// The count at `now`, in whole milliseconds, so a tick scheduled on
    /// `runningSince` plus n seconds reads n seconds and not a hair under.
    func elapsedNs(at now: Date) -> Int64 {
        guard let runningSince else { return standingNs }
        return max(0, Int64((runningSince.distance(to: now) * 1000).rounded())) * 1_000_000
    }
}

private extension TimeInterval {
    var nanoseconds: Int64 { Int64((self * 1_000_000_000).rounded()) }
}

private extension Int64 {
    var seconds: TimeInterval { TimeInterval(self) / 1_000_000_000 }
}

/// The fields of the app settings this slice reads. `settings-changed`
/// carries no useful payload, so the store re-reads `get_app_settings` and
/// decodes only these.
struct MeetingStartAppSettings: Decodable {
    /// The template the preview card names for a series. Absent means the app
    /// default, which the card reads as "App default".
    let meetingNotesTemplate: MeetingNotesTemplate?
}

// MARK: - The store

/// Starting a meeting, running it, and the two panels that lead to a start.
///
/// One store, because these surfaces are one act split across three places a
/// person can be standing: the detection panel that offers to record, the gate
/// a blocked start lands on, and the live screen. They share the session the
/// core is holding, and a second store would mean two readings of the same
/// revision.
///
/// The core is the only source of every phase word here. Nothing is optimistic:
/// a press sends a command, keeps the receipt, and re-reads the session, which
/// is what `useMeetingMutations` does and why a rejected start still leaves the
/// screen telling the truth.
@MainActor
@Observable
final class MeetingLiveStore {
    /// How long a burst of events is left to settle before one re-read.
    private static let eventSettle: Duration = .milliseconds(120)
    /// The window the upcoming list asks for, from `MeetingsUpcoming`.
    private static let upcomingDays = 7

    // MARK: What detection saw

    private(set) var suggestions: [MeetingSuggestion] = []
    /// Skipping an offer hides the card here and nowhere else: the core keeps
    /// the offer, so a person who skips one and reopens the page sees it again.
    private(set) var skipped: Set<String> = []
    private(set) var detection: DetectionStatus?
    /// Offers still waiting for an answer, oldest first, one per subject.
    private(set) var prompts: [DetectionPromptEvent] = []
    private(set) var ritual: RitualEvent? {
        didSet { anchorClocks() }
    }
    /// The wrap card's Copy follow-up, once it has been pressed.
    private(set) var followUpCopied = false
    /// The follow-up is being drafted; the card's button says so and refuses
    /// a second press until the draft lands or fails.
    private(set) var followUpDrafting = false

    // MARK: The panel's own recording

    private(set) var active: MeetingConsentPanelSessionState? {
        didSet { anchorClocks(read: active?.snapshot) }
    }
    /// The two boxes an offer carries, kept for the one prompt they were
    /// ticked on. Part of the consent that prompt's Record expresses, so a
    /// choice made on one offer never answers for another.
    private var choices: PromptChoices?

    private struct PromptChoices {
        let promptId: String
        var alwaysRecordSeries: Bool
        var announceInChat: Bool
    }

    // MARK: Getting a meeting recording

    /// What the press asked for, kept so the gate can rebuild the same consent
    /// after a blocked attempt.
    private(set) var options: MeetingStartOptions?
    /// The preflight session the gate is standing on.
    private(set) var gate: MeetingSessionSnapshot?
    private(set) var starting = false
    private(set) var refreshing = false
    /// The gate's one checkbox: the person accepts a partial record.
    private(set) var acceptPartial = false
    /// Where the next meeting's notes would go, from the core's own choice.
    private(set) var textEngine: MeetingTextEngineChoice?
    private(set) var settings: MeetingStartAppSettings?

    /// The engine on this Mac, when the core says nothing will write a new
    /// meeting's notes: the server path is off or unpaired and this engine
    /// cannot answer. Nothing while the notes have somewhere to go; the series
    /// kept here are the settings page's own row.
    var engineWarning: MeetingLocalEngineStatus? {
        if case let .unavailable(engine) = textEngine { engine } else { nil }
    }

    // MARK: Capture, while it runs

    private(set) var live: MeetingReviewSnapshot? {
        didSet { anchorClocks(read: live?.session) }
    }
    /// The words recognized so far by the pass that runs during capture.
    /// Separate from `live.transcript`, which is the stored reading and is
    /// empty until the meeting stops.
    private(set) var provisional: [MeetingProvisionalSegment] = []
    /// The clock of each capture a surface shows, by session: the live
    /// screen's, the panel's, and a ritual card's, which are one session
    /// unless two captures overlap. Only `anchorClocks` writes it, on every
    /// read of `live` or `active`, so each surface counts the same seconds.
    private(set) var clocks: [MeetingSessionId: MeetingCaptureClock] = [:]
    /// The word for what is in flight, which disables the controls that would
    /// contradict it.
    private(set) var pending: String?
    private(set) var receipt: MeetingOperationReceipt?
    private(set) var noteBody = ""
    /// The session the integrator should open for reading: set when a stop
    /// commits, when an import lands, and when a recovery finalizes.
    private(set) var opened: MeetingSessionId?

    // MARK: The unfinished, the imported and the week ahead

    private(set) var recovery: [MeetingHistorySummary] = []
    private(set) var upcoming: UpcomingEvents?
    private(set) var upcomingLoading = false
    private(set) var importing = false
    /// The evening digest asked for Capture. A pulse: the integrator navigates
    /// and clears it.
    private(set) var captureRequested = false

    // MARK: What the store has to say

    /// The last thing the core refused, in the words the shell shows.
    private(set) var error: String?
    /// The last thing that worked, where React raises a toast.
    private(set) var notice: String?

    private let core: Core
    @ObservationIgnored private var suggestionTask: Task<Void, Never>?
    @ObservationIgnored private var liveTask: Task<Void, Never>?
    @ObservationIgnored private var expiryTask: Task<Void, Never>?

    init(core: Core) {
        self.core = core

        core.observe(CoreEvent.meetingSuggestionChanged) { [weak self] _ in
            self?.reloadSuggestionsSoon()
        }
        core.observe(CoreEvent.detectionStatus) { [weak self] line in
            guard let self, let status: DetectionStatus = try? Core.payload(line) else { return }
            self.receive(status)
        }
        core.observe(CoreEvent.detectionPrompt) { [weak self] line in
            guard let self, let prompt: DetectionPromptEvent = try? Core.payload(line) else { return }
            self.receive(prompt)
        }
        core.observe(CoreEvent.detectionPromptRetracted) { [weak self] line in
            guard let self,
                  let retracted: DetectionPromptRetractedEvent = try? Core.payload(line) else { return }
            self.prompts.removeAll { $0.promptId == retracted.promptId }
        }
        core.observe(CoreEvent.meetingRitual) { [weak self] line in
            guard let self, let event: RitualEvent = try? Core.payload(line) else { return }
            self.receive(event)
        }
        core.observe(CoreEvent.meetingRitualRetracted) { [weak self] line in
            guard let self, let retracted: RitualRetractedEvent = try? Core.payload(line) else { return }
            if self.ritual?.ritualId == retracted.ritualId { self.clearRitual() }
        }
        // A phase, a source's health or a word of transcript changed. The
        // screen a person is looking at is the session, so re-read it.
        for name in [
            CoreEvent.meetingSessionChanged,
            CoreEvent.meetingTranscriptChanged,
            CoreEvent.meetingSourceHealthChanged,
        ] {
            core.observe(name) { [weak self] line in
                self?.sessionChanged(line)
            }
        }
        core.observe(CoreEvent.meetingRemoved) { [weak self] line in
            guard let self else { return }
            let payload: MeetingEventPayload? = try? Core.payload(line)
            guard let removed = payload?.sessionId else { return }
            if self.live?.session.sessionId == removed { self.closeLive() }
            if self.gate?.sessionId == removed { self.closeGate() }
            self.recovery.removeAll { $0.sessionId == removed }
        }
        // A start the detection panel made that did not reach capture: the
        // core sends the preflight here, and the gate is where a person
        // refreshes a blocked source or records without it.
        core.observe(CoreEvent.meetingNavigationRequested) { [weak self] line in
            guard let self, let payload: MeetingNavigationPayload = try? Core.payload(line),
                  payload.destination == .preflight, let sessionId = payload.sessionId else { return }
            Task { await self.openGate(sessionId) }
        }
        // The choice reads the server switch, the pairing and the engine here,
        // all of which a settings write can move.
        core.observe(CoreEvent.meetingLiveSettingsChanged) { [weak self] _ in
            Task {
                await self?.loadSettings()
                await self?.loadEngine()
            }
        }
        core.observe(CoreEvent.sonaCaptureRequested) { [weak self] _ in
            self?.captureRequested = true
        }
        // The Apple Intelligence switch is in System Settings, so the answer
        // to "can notes be written here" changes while this app is in the
        // back. Coming to the front is the moment to ask again.
        NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification, object: nil, queue: nil
        ) { [weak self] _ in
            Task { await self?.loadEngine() }
        }
    }

    /// The first read: what detection sees, the offers standing, the meetings
    /// left unfinished, whether anything here can write notes, and the panel's
    /// own recording if one is running.
    func start() async {
        await loadSettings()
        await loadDetection()
        await loadSuggestions()
        await loadRecovery()
        await loadEngine()
        await loadActive()
    }

    // MARK: - Reading the core

    private func loadSettings() async {
        do {
            settings = try await core.request("get_app_settings")
        } catch {
            // The preview card's Notes row is the only reader; without the
            // settings it names the app default, which is what absent means.
            settings = nil
        }
    }

    private func loadDetection() async {
        do {
            receive(try await core.request("detection_status_get") as DetectionStatus)
        } catch {
            self.error = reason(error)
        }
    }

    /// The offers standing, then a re-read timed for the first one to lapse.
    /// The core purges an offer only when something reads the list, and its
    /// expiry sends no event, so the card would otherwise outlive the offer.
    private func loadSuggestions() async {
        do {
            suggestions = try await core.request("meeting_suggestions_list")
        } catch {
            self.error = reason(error)
        }
        expiryTask?.cancel()
        guard let next = suggestions.map(\.expiresAtNs).min() else { return }
        let wait = next - MeetingLiveStore.hostNowNs()
        expiryTask = Task { [weak self] in
            if wait > 0 { try? await Task.sleep(for: .nanoseconds(wait)) }
            guard !Task.isCancelled else { return }
            await self?.loadSuggestions()
        }
    }

    /// The clock the core stamps offers with: mach absolute time in
    /// nanoseconds, which `meeting/clock.rs` reads through Core Audio and this
    /// side reads through `CLOCK_UPTIME_RAW`. The same counter, so an expiry
    /// compares directly.
    private static func hostNowNs() -> Int64 {
        Int64(clock_gettime_nsec_np(CLOCK_UPTIME_RAW))
    }

    private func loadRecovery() async {
        do {
            recovery = try await core.request("meeting_recovery_list")
        } catch {
            self.error = reason(error)
        }
    }

    /// Each read stamps itself; only the newest read's answer is kept. A
    /// settings write can start a read while an earlier one is still waiting
    /// on a slow model endpoint, and that earlier answer describes the engine
    /// the write just moved away from.
    @ObservationIgnored private var engineRead = 0

    private func loadEngine() async {
        engineRead += 1
        let read = engineRead
        let choice: MeetingTextEngineChoice? = try? await core.request("meeting_text_engine_for_next_meeting")
        guard read == engineRead else { return }
        textEngine = choice
    }

    /// System Settings → Apple Intelligence & Siri, where the switch and the
    /// model download are.
    func openAppleIntelligenceSettings() {
        NSWorkspace.shared.open(MeetingAppleIntelligenceBlocker.systemSettingsPane)
    }

    private func loadActive() async {
        do {
            let state: MeetingConsentPanelSessionState? =
                try await core.request("meeting_consent_panel_active_state")
            active = state
            await announceIfPending()
            await fitDisclosure()
        } catch {
            self.error = reason(error)
        }
    }

    /// The week ahead. Read on demand: it costs an EventKit query, and the
    /// section that shows it is not always on screen.
    func loadUpcoming() async {
        upcomingLoading = true
        defer { upcomingLoading = false }
        do {
            upcoming = try await core.request(
                "meeting_upcoming_events", ["days": JSONValue.number(Double(MeetingLiveStore.upcomingDays))])
        } catch {
            self.error = reason(error)
        }
    }

    func refreshUpcoming() {
        Task { await loadUpcoming() }
    }

    private func reloadSuggestionsSoon() {
        suggestionTask?.cancel()
        suggestionTask = Task { [weak self] in
            try? await Task.sleep(for: MeetingLiveStore.eventSettle)
            guard !Task.isCancelled else { return }
            await self?.loadSuggestions()
        }
    }

    /// One session event, coalesced. A session a surface is standing on is
    /// re-read at its revision. A session nobody here is watching still
    /// matters twice over: a standing series can start recording by itself,
    /// and the panel is the only Stop for it; and the background pass can
    /// finish or open a recovery, which moves the unfinished list. Neither
    /// costs more than one read after the burst settles.
    private func sessionChanged(_ line: Data) {
        let payload: MeetingEventPayload? = try? Core.payload(line)
        let touched = payload?.sessionId
        let watching = live?.session.sessionId ?? gate?.sessionId
        let watched = touched == nil || touched == watching
        liveTask?.cancel()
        liveTask = Task { [weak self] in
            try? await Task.sleep(for: MeetingLiveStore.eventSettle)
            guard !Task.isCancelled, let self else { return }
            if watched { await self.refreshSession() }
            await self.loadActive()
            if !watched { await self.loadRecovery() }
        }
    }

    /// `meeting_get` on whichever session a surface is standing on.
    private func refreshSession() async {
        if let sessionId = live?.session.sessionId {
            await adoptLive(sessionId, whileShown: true)
            return
        }
        guard let sessionId = gate?.sessionId, let snapshot = await read(sessionId) else { return }
        gate = snapshot.session
    }

    /// The live screen's one read: the session, and the words its running
    /// capture has recognized. A read that fails leaves the last snapshot
    /// standing — the recording is still running, and a screen with no Stop
    /// on it would be the worse answer — and says so in the error line.
    /// A refresh reads `whileShown`: a stop or a discard that closed the
    /// screen while the read was out must not have it reopened by the read.
    private func adoptLive(_ sessionId: MeetingSessionId, whileShown: Bool = false) async {
        guard let snapshot = await read(sessionId) else { return }
        if whileShown, live?.session.sessionId != sessionId { return }
        live = snapshot
        provisional = snapshot.session.phase.isActive ? await readProvisional(sessionId) : []
    }

    private func read(_ sessionId: MeetingSessionId) async -> MeetingReviewSnapshot? {
        do {
            return try await core.request("meeting_get", MeetingRequest.session(sessionId))
        } catch {
            self.error = reason(error)
            return nil
        }
    }

    private func readProvisional(_ sessionId: MeetingSessionId) async -> [MeetingProvisionalSegment] {
        let transcript: MeetingProvisionalTranscript? = try? await core.request(
            "meeting_live_transcript", MeetingRequest.session(sessionId))
        return transcript?.segments ?? provisional
    }

    /// The gate, for a preflight the core created on this screen's behalf.
    /// The consent the gate rebuilds is read off the session: what it was
    /// asked to record, and the title it was given.
    private func openGate(_ sessionId: MeetingSessionId) async {
        guard let snapshot = await read(sessionId), snapshot.session.phase == .preflight else { return }
        options = MeetingStartOptions(
            title: snapshot.session.title, origin: .suggestion, suggestionId: nil,
            calendarEventKey: nil, sources: snapshot.session.sources.map(\.sourceKind),
            preview: nil)
        acceptPartial = false
        gate = snapshot.session
    }

    // MARK: - Detection

    private func receive(_ status: DetectionStatus) {
        detection = status
        guard !status.inputDeviceActive else { return }
        // An offer to record is live only while something holds the microphone.
        // A calendar event is raised a minute before anyone opens one, and a
        // call on a Bluetooth headset may never raise the device at all, so
        // those two outlive it and the core retracts them by their own bound.
        prompts.removeAll { !$0.prompt.survivesIdleMic }
    }

    private func receive(_ prompt: DetectionPromptEvent) {
        let subject = prompt.prompt.subject(promptId: prompt.promptId)
        prompts.removeAll { $0.prompt.subject(promptId: $0.promptId) == subject }
        prompts.append(prompt)
        if prompt.delivery == .panel {
            Task { try? await core.request(
                "detection_prompt_panel_ack", MeetingLiveRequest.prompt(prompt.promptId)) }
        }
    }

    /// One offer, answered. Cleared locally first: a card must stop offering a
    /// decision that has been made, and the core drops an unknown prompt id.
    func answer(_ prompt: DetectionPromptEvent, accepted: Bool) {
        prompts.removeAll { $0.promptId == prompt.promptId }
        Task {
            do {
                try await core.request(
                    "detection_prompt_respond",
                    MeetingLiveRequest.promptRespond(prompt.promptId, accepted: accepted))
            } catch {
                self.error = reason(error)
            }
        }
    }

    // MARK: - Rituals

    private func receive(_ event: RitualEvent) {
        ritual = event
        followUpCopied = false
        if event.delivery == .panel {
            Task { try? await core.request(
                "meeting_ritual_panel_ack", MeetingLiveRequest.ritual(event.ritualId)) }
        }
    }

    private func clearRitual() {
        ritual = nil
        followUpCopied = false
    }

    /// A press on a ritual card. Every action but the follow-up copy closes the
    /// card: copying is the one a person does before pressing Done.
    func respond(_ event: RitualEvent, action: RitualAction) {
        Task {
            do {
                let accepted: Bool = try await core.request(
                    "meeting_ritual_respond",
                    MeetingLiveRequest.ritualRespond(event.ritualId, action: action))
                if accepted, action != .wrapFollowUpCopied, ritual?.ritualId == event.ritualId {
                    clearRitual()
                }
                if action == .recordingStop || action == .recordingForgetApp {
                    await loadActive()
                }
            } catch {
                self.error = reason(error)
            }
        }
    }

    /// Copy follow-up: the text is the review slice's draft, so the draft is
    /// wired by whoever has it. A draft that fails is said on the card in the
    /// core's words; one that lands is copied, and the press is reported to
    /// the core, which is what makes the card say "Copied".
    func copyFollowUp(
        _ event: RitualEvent, for sessionId: MeetingSessionId,
        draft: @escaping (MeetingSessionId) async throws -> String
    ) {
        guard !followUpDrafting else { return }
        followUpDrafting = true
        Task {
            defer { followUpDrafting = false }
            do {
                let text = try await draft(sessionId)
                copy(text)
                followUpCopied = true
                error = nil
                respond(event, action: .wrapFollowUpCopied)
            } catch {
                self.error = reason(error)
            }
        }
    }

    // MARK: - The panel's recording

    /// The room is told once, and only when the core asked for it.
    private func announceIfPending() async {
        guard let state = active, let notetaker = state.disclosure.notetaker else { return }
        do {
            try await core.request(
                "meeting_announce_disclosure",
                MeetingLiveRequest.announceDisclosure(
                    state.snapshot.sessionId,
                    line: "Sona is taking notes for \(notetaker). Say so if you'd rather it didn't."))
            let refreshed: MeetingConsentPanelSessionState? =
                try await core.request("meeting_consent_panel_active_state")
            active = refreshed
        } catch {
            // A room that would not take the line is a sentence on the card,
            // not a failure of the recording.
            self.error = reason(error)
        }
    }

    /// The panel is a fixed-height window: the core is told whether the card
    /// carries the refusal line, because that line changes its height.
    private func fitDisclosure() async {
        guard let state = active else { return }
        try? await core.request(
            "meeting_consent_panel_fit_disclosure",
            ["note": JSONValue.bool(state.disclosure.outcomeLine != nil)])
    }

    /// The box "Always record this meeting": only a calendar offer has one.
    func alwaysRecordSeries(_ prompt: DetectionPromptEvent) -> Bool {
        prompt.prompt.isCalendar && choices(for: prompt).alwaysRecordSeries
    }

    /// The box for the chat notice, starting from what the series remembers.
    func announceInChat(_ prompt: DetectionPromptEvent) -> Bool {
        choices(for: prompt).announceInChat
    }

    func setAlwaysRecordSeries(_ on: Bool, for prompt: DetectionPromptEvent) {
        var current = choices(for: prompt)
        current.alwaysRecordSeries = on
        choices = current
    }

    func setAnnounceInChat(_ on: Bool, for prompt: DetectionPromptEvent) {
        var current = choices(for: prompt)
        current.announceInChat = on
        choices = current
    }

    private func choices(for prompt: DetectionPromptEvent) -> PromptChoices {
        if let choices, choices.promptId == prompt.promptId { return choices }
        return PromptChoices(
            promptId: prompt.promptId, alwaysRecordSeries: false, announceInChat: prompt.announceInChat)
    }

    /// Record, from the panel. The consent is the press on this card: both
    /// sources acknowledged, nothing missing accepted, and the standing grant
    /// only when the box for it was ticked. A series grant is a calendar
    /// thing, so an app offer never sends one.
    func record(_ prompt: DetectionPromptEvent) {
        Task {
            await act("Starting") {
                let consent = MeetingConsentInput(
                    sources: MeetingSourceKind.allCases, acceptedMissingSources: [],
                    acceptPartial: false, degradedStartPolicy: .abortIfRequiredSourceFails)
                let result: MeetingMutationResult = try await self.core.request(
                    "meeting_consent_panel_start",
                    MeetingLiveRequest.consentPanelStart(
                        promptId: prompt.promptId, consent: consent,
                        alwaysRecordSeries: self.alwaysRecordSeries(prompt),
                        announceInChat: self.announceInChat(prompt)))
                guard self.receive(result.receipt) else { return }
                self.prompts.removeAll { $0.promptId == prompt.promptId }
                self.choices = nil
                if result.snapshot.phase == .capturingRecording {
                    await self.loadActive()
                }
            }
        }
    }

    /// This series records itself; forget that. The core answers with whether
    /// a grant was there to forget.
    func forgetSeries() {
        guard let sessionId = active?.snapshot.sessionId else { return }
        Task {
            await act("Forgetting the series") {
                let forgotten: Bool = try await self.core.request(
                    "meeting_consent_panel_forget_series", MeetingRequest.session(sessionId))
                if forgotten { self.notice = "This series will not record itself again." }
                await self.loadActive()
            }
        }
    }

    /// Stop, from the panel. The core records which Stop was pressed. A
    /// refusal re-reads the panel's session, so the next press carries the
    /// revision the core has now rather than the one a health change moved.
    func stopFromPanel() {
        guard let session = active?.snapshot else { return }
        Task {
            await act("Stopping") {
                let result: MeetingMutationResult = try await self.core.request(
                    "meeting_stop",
                    MeetingLiveRequest.stop(
                        session.sessionId, revision: session.revision, surface: .consentPanel))
                guard self.receive(result.receipt) else {
                    await self.loadActive()
                    return
                }
                self.active = nil
                if self.live?.session.sessionId == session.sessionId { self.closeLive() }
                self.opened = session.sessionId
            }
        }
    }

    // MARK: - Starting

    /// Every start goes through here: a preflight row, then the gate. The
    /// consent the core persists says the press happened below the sentence
    /// naming what is recorded, so the gate is shown for a healthy start too;
    /// with every source available it is one screen and one press.
    func offer(_ options: MeetingStartOptions) {
        guard !options.sources.isEmpty else { return }
        self.options = options
        acceptPartial = false
        Task {
            await act("Starting") {
                let created: MeetingMutationResult = try await self.core.request(
                    "meeting_preflight_create", MeetingLiveRequest.preflightCreate(options))
                guard self.receive(created.receipt) else {
                    // An offer the core would not take is over: it lapsed, or
                    // the app closed. The list is re-read so the card goes with it.
                    if options.suggestionId != nil { await self.loadSuggestions() }
                    return
                }
                self.gate = created.snapshot
            }
        }
    }

    /// Record, from the gate. The press is the acknowledgement, and the flags
    /// it sends are the ones the sentence above the button claimed.
    func record() {
        guard let session = gate, let options, session.allows(.start),
              session.phase == .preflight, !gateBlocked || canStartPartial else { return }
        let accepted = acceptPartial ? blockedSources.map(\.sourceKind) : []
        Task {
            await act("Starting") {
                await self.capture(
                    session,
                    consent: options.consent(
                        acceptedMissingSources: accepted, acceptPartial: self.acceptPartial))
            }
        }
    }

    /// `meeting_start`. A start that reached capture is the live screen; a
    /// refusal, or a committed consent whose sources all failed to open, leaves
    /// the gate standing on a freshly read session.
    private func capture(_ session: MeetingSessionSnapshot, consent: MeetingConsentInput) async {
        starting = true
        defer { starting = false }
        do {
            let result: MeetingMutationResult = try await core.request(
                "meeting_start",
                MeetingLiveRequest.start(
                    session.sessionId, revision: session.revision, consent: consent))
            guard receive(result.receipt) else {
                gate = await read(session.sessionId)?.session ?? result.snapshot
                return
            }
            // The core keeps the consent it was given and then opens the
            // sources. When none of them starts, the phase rolls back to
            // preflight under a committed receipt: nothing is recording, and
            // the gate is where a person tries again.
            guard result.snapshot.phase.isActive else {
                gate = await read(session.sessionId)?.session ?? result.snapshot
                error = "Nothing is recording: no source could be started. Check the sources and try again."
                return
            }
            gate = nil
            provisional = []
            await adoptLive(session.sessionId)
            if live == nil {
                // The capture is running either way. The screen stands on the
                // session the start returned until the next read lands.
                live = MeetingReviewSnapshot(session: result.snapshot)
            }
        } catch {
            self.error = reason(error)
            if let failure = (error as? CoreError)?.remote(as: MeetingCommandError.self) {
                await recover(from: failure, session: session)
            } else {
                gate = await read(session.sessionId)?.session
            }
        }
    }

    /// Check again whether the blocked source came back.
    func refresh() {
        guard let session = gate, session.phase == .preflight,
              session.allows(.refreshPreflight), !refreshing, !starting else { return }
        Task {
            refreshing = true
            defer { refreshing = false }
            do {
                let result: MeetingMutationResult = try await core.request(
                    "meeting_preflight_refresh",
                    MeetingRequest.wrap(
                        MeetingRequest.mutation(session.sessionId, revision: session.revision)))
                if receive(result.receipt) {
                    gate = await read(session.sessionId)?.session ?? result.snapshot
                }
            } catch {
                self.error = reason(error)
            }
        }
    }

    /// Leaving the gate cancels the preflight row, so an abandoned start does
    /// not sit in history as a meeting that never recorded.
    func cancel() {
        guard let session = gate else {
            closeGate()
            return
        }
        guard session.phase == .preflight, session.allows(.cancelPreflight) else {
            closeGate()
            return
        }
        Task {
            await act("Cancelling") {
                let receipt: MeetingOperationReceipt = try await self.core.request(
                    "meeting_preflight_cancel",
                    MeetingRequest.wrap(
                        MeetingRequest.mutation(session.sessionId, revision: session.revision)))
                guard self.receive(receipt) else {
                    self.gate = await self.read(session.sessionId)?.session
                    return
                }
                self.closeGate()
                await self.loadRecovery()
            }
        }
    }

    func setAcceptPartial(_ accepted: Bool) {
        acceptPartial = accepted
    }

    private func closeGate() {
        gate = nil
        options = nil
        acceptPartial = false
    }

    /// Offers, countdowns and calendar rows all reach the same start.
    func start(_ suggestion: MeetingSuggestion) {
        offer(MeetingStartOptions(
            title: MeetingStartOptions.defaultTitle, origin: .suggestion,
            suggestionId: suggestion.offerId,
            calendarEventKey: nil, sources: MeetingSourceKind.allCases,
            preview: MeetingStartFacts(suggestion: suggestion)))
    }

    func start(_ event: DetectionCalendarEvent) {
        let facts = MeetingStartFacts(event: event)
        offer(MeetingStartOptions(
            title: facts.heading, origin: .manual, suggestionId: nil,
            calendarEventKey: event.eventKey, sources: MeetingSourceKind.allCases,
            preview: facts))
    }

    func start(_ row: UpcomingRow) {
        let facts = MeetingStartFacts(row: row)
        offer(MeetingStartOptions(
            title: facts.heading, origin: .manual, suggestionId: nil,
            calendarEventKey: row.eventKey, sources: MeetingSourceKind.allCases,
            preview: facts))
    }

    /// The plain Record press, with no meeting identified behind it.
    func startManual() {
        offer(MeetingStartOptions(
            title: MeetingStartOptions.defaultTitle, origin: .manual, suggestionId: nil,
            calendarEventKey: nil,
            sources: MeetingSourceKind.allCases, preview: nil))
    }

    func skip(_ suggestion: MeetingSuggestion) {
        skipped.insert(suggestion.offerId)
    }

    // MARK: - Capture, while it runs

    func open(_ snapshot: MeetingReviewSnapshot) {
        live = snapshot
        provisional = []
        Task { await adoptLive(snapshot.session.sessionId) }
    }

    /// The live screen, read from the session the core says is capturing.
    func open(_ sessionId: MeetingSessionId) {
        Task { await adoptLive(sessionId) }
    }

    func closeLive() {
        live = nil
        provisional = []
        noteBody = ""
    }

    func pause() {
        mutate("Pausing", .pause) { "meeting_pause" }
    }

    func resume() {
        mutate("Resuming", .resume) { "meeting_resume" }
    }

    /// Stop. The surface is recorded, because a stop from the live screen and
    /// a stop from the panel are different acts. A committed stop hands the
    /// window to the review page, where the notes arrive: the live screen has
    /// nothing left to say once the capture has ended.
    func stop() {
        guard let session = live?.session, session.allows(.stop) || pending != nil else { return }
        Task {
            await act("Stopping") {
                let result: MeetingMutationResult = try await self.core.request(
                    "meeting_stop",
                    MeetingLiveRequest.stop(
                        session.sessionId, revision: session.revision, surface: .meetingLive))
                guard await self.settle(result, session) else { return }
                self.closeLive()
                self.opened = session.sessionId
            }
        }
    }

    /// Stop and throw it away. Everything the meeting wrote goes with it.
    func discard() {
        guard let session = live?.session else { return }
        Task {
            await act("Discarding") {
                let result: MeetingRemovalResult = try await self.core.request(
                    "meeting_discard",
                    MeetingRequest.wrap(
                        MeetingRequest.mutation(session.sessionId, revision: session.revision)))
                guard self.receive(result.receipt) else {
                    await self.adoptLive(session.sessionId)
                    return
                }
                self.closeLive()
                self.notice = "Session discarded"
                await self.loadRecovery()
            }
        }
    }

    func setNote(_ text: String) {
        noteBody = text
    }

    /// A note against this moment of the meeting. The moment is the count
    /// the capture's clock shows now, not the core's last number; a note
    /// written two minutes after the last read is a note about now, not then.
    /// The draft stays in the sheet until the core has kept it, so a refusal
    /// hands the words back rather than losing them.
    func createNote() {
        guard let session = live?.session else { return }
        let body = noteBody.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !body.isEmpty else { return }
        let moment = clocks[session.sessionId]?.elapsedNs(at: Date()) ?? 0
        Task {
            await act("Saving the note") {
                let result: MeetingMutationResult = try await self.core.request(
                    "meeting_note_create",
                    MeetingRequest.noteCreate(
                        session.sessionId, revision: session.revision,
                        startOffsetNs: moment, body: body))
                guard await self.settle(result, session) else { return }
                self.noteBody = ""
            }
        }
    }

    private func mutate(_ word: String, _ action: MeetingAllowedAction, method: () -> String) {
        guard let session = live?.session, session.allows(action) else { return }
        let method = method()
        Task {
            await act(word) {
                let result: MeetingMutationResult = try await self.core.request(
                    method,
                    MeetingRequest.wrap(
                        MeetingRequest.mutation(session.sessionId, revision: session.revision)))
                await self.settle(result, session)
            }
        }
    }

    /// Every live mutation ends here: the receipt is read, and the session is
    /// re-read whichever way it went. A refusal is most often a stale
    /// revision — a source's health moved the session under the screen — and
    /// the re-read is what makes the next press carry the revision the core
    /// has now.
    @discardableResult
    private func settle(_ result: MeetingMutationResult, _ session: MeetingSessionSnapshot) async -> Bool {
        let committed = receive(result.receipt)
        await adoptLive(session.sessionId)
        return committed
    }

    // MARK: - The unfinished

    /// Keep what was recorded before the interruption: the core seals the
    /// partial capture and processes it.
    func finalizeRecovery(_ entry: MeetingHistorySummary) {
        Task {
            guard let snapshot = await read(entry.sessionId) else { return }
            await act("Finishing") {
                let result: MeetingMutationResult = try await self.core.request(
                    "meeting_recovery_finalize",
                    MeetingRequest.wrap(
                        MeetingRequest.mutation(
                            snapshot.session.sessionId, revision: snapshot.session.revision)))
                guard self.receive(result.receipt) else { return }
                self.opened = entry.sessionId
                await self.loadRecovery()
            }
        }
    }

    /// Throw the interrupted meeting away instead.
    func discardRecovery(_ entry: MeetingHistorySummary) {
        Task {
            guard let snapshot = await read(entry.sessionId) else { return }
            await act("Discarding") {
                let result: MeetingRemovalResult = try await self.core.request(
                    "meeting_discard",
                    MeetingRequest.wrap(
                        MeetingRequest.mutation(
                            snapshot.session.sessionId, revision: snapshot.session.revision)))
                guard self.receive(result.receipt) else { return }
                self.notice = "Session discarded"
                await self.loadRecovery()
            }
        }
    }

    // MARK: - Importing

    /// Recordings and transcript exports, as meetings. The picker is the
    /// shell's own: the core takes a path, and choosing the file is this side's
    /// job.
    func importMeeting() {
        guard !importing else { return }
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.allowedContentTypes = MeetingImport.contentTypes
        panel.prompt = "Import"
        panel.message = "Choose a recording or a transcript export."
        guard panel.runModal() == .OK, let url = panel.url else { return }
        importMeeting(at: url)
    }

    /// The same import, for a file dropped on the window.
    func importMeeting(at url: URL) {
        Task {
            importing = true
            defer { importing = false }
            let path = url.path
            let transcript = MeetingImport.isTranscript(path)
            do {
                let snapshot: MeetingSessionSnapshot = transcript
                    ? try await core.request(
                        "meeting_import_transcript", ["path": JSONValue.string(path)])
                    : try await core.request(
                        "meeting_import_recording", MeetingLiveRequest.importRecording(path: path))
                opened = snapshot.sessionId
                notice = "Saved locally."
                await loadRecovery()
            } catch {
                self.error = reason(error)
            }
        }
    }

    // MARK: - What the surfaces read

    /// Offers still worth showing: the core's list, less the ones skipped here.
    var offered: [MeetingSuggestion] {
        suggestions.filter { !skipped.contains($0.offerId) }
    }

    var countdown: DetectionCountdown? { detection?.countdown }

    /// The one card the panel draws, in `ConsentPanel.tsx` order.
    var card: MeetingConsentCard? {
        if let ritual, case let .recording(recording) = ritual.ritual {
            return .recording(ritual, recording)
        }
        if let active { return .active(active) }
        if let prompt = prompts.last { return .prompt(prompt) }
        guard let ritual else { return nil }
        switch ritual.ritual {
        case let .prep(card): return .prep(ritual, card)
        case let .wrap(card): return .wrap(ritual, card)
        case .recording: return nil
        }
    }

    /// `briefing` in ConsentPanel.tsx: the relationship line under a calendar
    /// offer, and only when the countdown is about that same event.
    var seriesBrief: String? {
        guard let prompt = prompts.last, let eventKey = prompt.prompt.eventKey,
              let countdown, countdown.event.eventKey == eventKey,
              let person = countdown.briefing.first else { return nil }
        let loops = countdown.briefing.reduce(0) { $0 + $1.openLoops.count }
        let nth = person.meetingsCount + 1
        let loopWord = loops == 1 ? "1 open loop" : "\(loops) open loops"
        return "Meeting \(nth) with \(person.displayName) · \(loopWord)"
    }

    /// The gate's title: a blocked source, or a phase that will not start, is
    /// the screen saying so.
    var gateBlocked: Bool { !blockedSources.isEmpty }

    var blockedSources: [MeetingSourceSnapshot] {
        (gate?.sources ?? []).filter { $0.required && $0.availability != .available }
    }

    /// Whether a partial recording has anything to record: a start with every
    /// source blocked would seal an empty meeting, so the gate refuses it.
    var canStartPartial: Bool {
        (gate?.sources ?? []).contains { $0.availability == .available }
    }

    var canStart: Bool {
        guard let gate else { return false }
        return gate.phase == .preflight && gate.allows(.start)
    }

    var canRefresh: Bool {
        guard let gate else { return false }
        return gate.phase == .preflight && gate.allows(.refreshPreflight)
    }

    /// The words this meeting will be read with: what the preview card names
    /// for the series, or the app default.
    var notesTemplate: MeetingNotesTemplate? { settings?.meetingNotesTemplate }

    /// The transcript as it reads: an editor's removal is honoured even live.
    var lines: [TranscriptEffectiveSegment] {
        (live?.transcript ?? []).filter { !$0.removed }
    }

    /// Whether the screen is reading the running pass rather than the stored
    /// transcript: the stored one is empty until the meeting stops.
    var showsProvisional: Bool {
        lines.isEmpty && !provisional.isEmpty
    }

    /// A read of `session` landed, or a surface let a session go: carry the
    /// read session's clock on, start one for a recording a ritual card
    /// announced before any read of it, and drop the clocks nothing shows.
    /// Written only when a clock moved, so a read that leaves every count
    /// where it was redraws nothing.
    private func anchorClocks(read session: MeetingSessionSnapshot? = nil) {
        var next = clocks
        if let session {
            next[session.sessionId] = MeetingCaptureClock(
                session, readAt: Date(), after: clocks[session.sessionId])
        }
        var shown = [live?.session.sessionId, active?.snapshot.sessionId]
        if let ritual, case let .recording(card) = ritual.ritual {
            shown.append(card.sessionId)
            if next[card.sessionId] == nil {
                next[card.sessionId] = MeetingCaptureClock(startedAtUtcMs: card.startedAtUtcMs)
            }
        }
        next = next.filter { shown.contains($0.key) }
        if next != clocks { clocks = next }
    }

    /// The two lines the live screen has to say, in the order it says them:
    /// storage that stopped accepting writes, then audio that is missing.
    var warnings: [MeetingLiveWarning] {
        guard let session = live?.session else { return [] }
        var lines: [MeetingLiveWarning] = []
        if session.storage != .available {
            lines.append(MeetingLiveWarning(text: "Storage needs attention.", urgent: true))
        }
        if let audio = audioWarning(session) {
            lines.append(audio)
        }
        return lines
    }

    /// The audio line, if there is one. A microphone-only meeting the person
    /// chose is said as a fact, not a fault; a lane that was asked for and is
    /// not working is the fault, and the line names every lane in that state
    /// rather than assuming the microphone is fine; anything else missing
    /// reads off the core's completeness.
    private func audioWarning(_ session: MeetingSessionSnapshot) -> MeetingLiveWarning? {
        let microphone = session.sources.first { $0.sourceKind == .microphone }
        guard let lane = session.sources.first(where: { $0.sourceKind == .systemAudio }) else {
            return microphone != nil
                ? MeetingLiveWarning(
                    text: "Recording the microphone only. The other side of the call is not captured.",
                    urgent: false)
                : nil
        }
        let systemDown = MeetingLiveStore.down(lane)
        let microphoneDown = microphone.map(MeetingLiveStore.down) ?? true
        if systemDown && microphoneDown {
            return MeetingLiveWarning(
                text: session.phase == .capturingPaused
                    ? "Nothing resumed: neither source could be reopened. Stop, or try again."
                    : "Nothing is being recorded: both sources have failed. Stop, or check the sources.",
                urgent: true)
        }
        if systemDown {
            return MeetingLiveWarning(
                text: "Recording the microphone only. System audio is unavailable, "
                    + "so this meeting will be partial.",
                urgent: false)
        }
        if microphoneDown {
            return MeetingLiveWarning(
                text: "Recording system audio only. The microphone is unavailable, "
                    + "so this meeting will be partial.",
                urgent: false)
        }
        if session.captureCompleteness == .partial || lane.health == .degraded
            || microphone?.health == .degraded
        {
            return MeetingLiveWarning(
                text: "Some audio is missing. Check the gaps before relying on the generated notes.",
                urgent: false)
        }
        return nil
    }

    /// A lane that is not delivering audio at all. Gaps are a different
    /// state: the lane is recording, with holes.
    private static func down(_ lane: MeetingSourceSnapshot) -> Bool {
        lane.availability != .available || lane.health == .failed
    }

    /// One word per source, as the gate's list and the live screen read it.
    func readings(_ session: MeetingSessionSnapshot) -> [MeetingSourceReading] {
        let sealed = !session.phase.isActive && session.phase != .preflight
        return session.sources.map { MeetingSourceReading($0, sealed: sealed) }
    }

    var upcomingDays: [UpcomingDay] { UpcomingDay.group(upcoming?.rows ?? []) }

    /// An empty week under `authorized` is a free week; under anything else it
    /// is a missing grant, which is a different sentence.
    var upcomingAccess: DetectionCalendarAccess { upcoming?.access ?? .notDetermined }

    // MARK: - Clearing what a surface consumed

    func clearOpened() {
        opened = nil
    }

    func clearCaptureRequest() {
        captureRequested = false
    }

    func dismissError() {
        error = nil
    }

    func dismissNotice() {
        notice = nil
    }

    // MARK: - Answers from the core

    /// One command, with the word for it while it runs.
    private func act(_ word: String, _ work: () async throws -> Void) async {
        pending = word
        defer { pending = nil }
        do {
            try await work()
        } catch {
            self.error = reason(error)
        }
    }

    /// `receiveReceipt`: a duplicate is worth saying, a refusal is named here
    /// rather than by the caller, and the caller only decides whether to go on.
    @discardableResult
    private func receive(_ receipt: MeetingOperationReceipt) -> Bool {
        self.receipt = receipt
        if receipt.reasonCodes.contains(.duplicateOperation) {
            notice = "This action was already recorded. Sona refreshed the existing result."
        }
        if receipt.result == .committed {
            error = nil
            return true
        }
        error = receipt.refusal
        return false
    }

    /// The typed refusals this slice can do something about: a stale consent
    /// or revision means the screen is reading an old session, and a meeting
    /// that needs recovery is not a meeting this screen can start.
    private func recover(from failure: MeetingCommandError, session: MeetingSessionSnapshot) async {
        switch failure {
        case .consentRequired, .consentStale, .staleRevision, .invalidTransition,
             .sourceUnavailable, .noSourceStarted, .storageUnavailable:
            gate = await read(session.sessionId)?.session
        case .captureLeaseBusy:
            // Another recording holds the microphone: this one never existed,
            // and the gate has nothing left to offer.
            closeGate()
            await loadActive()
        case .recoveryRequired:
            closeGate()
            await loadRecovery()
        default:
            gate = await read(session.sessionId)?.session
        }
    }

    /// The core's own word for a failure, in the sentence the shell shows.
    private func reason(_ error: Error) -> String {
        guard let failure = error as? CoreError else { return error.localizedDescription }
        return failure.remote(as: MeetingCommandError.self)?.label ?? failure.localizedDescription
    }

    private func copy(_ text: String) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
    }
}

// MARK: - Reading a prompt

extension DetectionPromptKind {
    /// `promptSubject`: what an offer is about, as opposed to which delivery of
    /// it this is. The core mints a fresh prompt id on every raise and re-arms
    /// an app's claim whenever the input device goes idle, so one app across
    /// three microphone episodes is three ids for one subject.
    func subject(promptId: String) -> String {
        switch self {
        case let .calendarEvent(eventKey, _): "event:\(eventKey)"
        case let .appMeeting(bundleId, _), let .appHuddle(bundleId, _),
             let .browserCall(bundleId, _), let .appCall(bundleId, _): "app:\(bundleId)"
        case .unknownMicSource: "mic"
        }
    }

    /// The two kinds that outlive an idle microphone: a calendar event is
    /// raised before anyone opens one, and a call on a Bluetooth headset may
    /// never raise the device at all.
    var survivesIdleMic: Bool {
        switch self {
        case .calendarEvent, .appCall: true
        case .appMeeting, .appHuddle, .browserCall, .unknownMicSource: false
        }
    }
}

// MARK: - Importing a file

/// Which owner a chosen file goes to. A transcript export is read as it
/// stands; everything else is decoded and transcribed.
enum MeetingImport {
    static let transcriptExtensions = ["txt", "srt", "json", "md"]
    static let mediaExtensions = ["wav", "mp3", "m4a", "aac", "flac", "ogg", "mov", "mp4", "m4v"]
    static var extensions: [String] { mediaExtensions + transcriptExtensions }

    /// The picker's filter. An extension the system has no type for is left
    /// out rather than guessed at: the core reads the file either way, and a
    /// picker that offers a type macOS cannot name would offer nothing.
    static var contentTypes: [UTType] {
        extensions.compactMap { UTType(filenameExtension: $0) }
    }

    static func isTranscript(_ path: String) -> Bool {
        transcriptExtensions.contains(URL(fileURLWithPath: path).pathExtension.lowercased())
    }
}

// MARK: - The title a press with nothing named carries

extension MeetingStartOptions {
    /// `meetings.setup.defaultTitle`. A meeting nobody titled is what it is:
    /// notes kept on this Mac.
    static let defaultTitle = "Local notes"
}
