import Foundation

/// The wire types the start, live, consent-panel, detection and upcoming
/// surfaces need and `MeetingTypes.swift` does not carry. Mirrored from
/// `src/bindings.ts` (generated from `src-tauri/src/meeting/types.rs`,
/// `meeting/detection.rs`, `meeting/digest.rs` and the command modules).
///
/// Field names are the Rust names with snake_case turned into camelCase by
/// `Core.decoder`; enum values are the wire strings verbatim. Detection's own
/// payloads are already camelCase on the wire, which that conversion leaves
/// alone.
///
/// Every name here is prefixed for this slice — `MeetingLive`, `MeetingStart`,
/// `MeetingConsent`, `MeetingPreflight`, `MeetingRecovery`, `MeetingImport`,
/// `MeetingSuggestion`, `MeetingSource`, `Detection`, `Ritual`, `Upcoming` —
/// so a wire type whose Rust name carries no such prefix is renamed rather
/// than dropped: `MeetingStopSurface` is `MeetingLiveStopSurface`,
/// `MeetingLocalEngineStatus` is `MeetingLiveLocalEngineStatus`,
/// `CalendarEventSummary` is `DetectionCalendarEvent`, `MeetingRitual*` are
/// `Ritual*`, `MeetingUpcoming*` are `Upcoming*`.

// MARK: - Events

extension CoreEvent {
    /// `meeting:suggestion-changed`: the offers a running meeting app raised
    /// changed. Payload is `MeetingEventPayload`.
    static let meetingSuggestionChanged = "meeting:suggestion-changed"
    /// `detection-prompt`: one offer to record, with the answer attached.
    static let detectionPrompt = "detection-prompt"
    /// `detection-prompt-retracted`: that offer is no longer live.
    static let detectionPromptRetracted = "detection-prompt-retracted"
    /// `meeting-ritual`: a prep, wrap or recording card.
    static let meetingRitual = "meeting-ritual"
    static let meetingRitualRetracted = "meeting-ritual-retracted"
    /// `detection-status`: the whole detection read, pushed on every change.
    static let detectionStatus = "detection-status"
    /// `settings-changed`: the app settings were written. The payload says
    /// nothing useful, so a reader re-reads `get_app_settings`.
    static let meetingLiveSettingsChanged = "settings-changed"
    /// `sona:capture-requested`: the evening digest asking for Capture. The
    /// payload is `null` — the event is the whole message.
    static let sonaCaptureRequested = "sona:capture-requested"
}

// MARK: - Consent

/// The consent policy every acknowledgement on this machine is stamped with,
/// from `MEETING_CONSENT_POLICY_VERSION` in `MeetingStartGate.tsx`. One
/// spelling, because a standing series grant cites the same version a
/// per-attempt receipt does.
enum MeetingConsentPolicy {
    static let version = 1
}

/// `MeetingConsentInput`, in the wire shape the core persists per attempt.
///
/// The press on the labelled Start button under the assurance line is the
/// acknowledgement these flags record, so a surface that builds one without
/// that sentence on screen would make the row assert something nobody could
/// have made.
struct MeetingConsentInput {
    let microphoneAcknowledged: Bool
    let systemAudioAcknowledged: Bool
    let knownMissingSourcesAcknowledged: [MeetingSourceKind]
    let degradedStartPolicy: MeetingDegradedStartPolicy
    /// Remote processing is not offered in this build, so every meeting is
    /// transcribed on this Mac and the acknowledgement is always `null`.
    let destination: JSONValue = .object(["kind": .string("local")])

    /// `consentFor(options, acceptedMissingSources, acceptPartial)`.
    init(sources: [MeetingSourceKind], acceptedMissingSources: [MeetingSourceKind],
         acceptPartial: Bool, degradedStartPolicy: MeetingDegradedStartPolicy) {
        microphoneAcknowledged = sources.contains(.microphone)
        systemAudioAcknowledged = sources.contains(.systemAudio)
        knownMissingSourcesAcknowledged = acceptedMissingSources
        self.degradedStartPolicy = acceptPartial ? .continueAndMarkPartial : degradedStartPolicy
    }

    var json: JSONValue {
        .object([
            "policy_version": .number(Double(MeetingConsentPolicy.version)),
            "microphone_acknowledged": .bool(microphoneAcknowledged),
            "system_audio_acknowledged": .bool(systemAudioAcknowledged),
            "known_missing_sources_acknowledged": .array(
                knownMissingSourcesAcknowledged.map { .string($0.rawValue) }),
            "degraded_start_policy": .string(degradedStartPolicy.rawValue),
            "destination": destination,
            "remote_acknowledgement": .null,
        ])
    }
}

/// `MeetingSessionDisclosure`: what this recording's one announcement into the
/// meeting's own chat is doing.
enum MeetingConsentDisclosure: Decodable {
    case notAsked
    /// Asked for and not posted yet. `notetaker` is the name the room is told
    /// the notes are for.
    case pending(notetaker: String)
    /// Posted, or refused: the receipt says which.
    case attempted(receipt: MeetingConsentDeliveryReceipt)

    private enum Key: String, CodingKey { case kind, notetaker, receipt }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "not_asked": self = .notAsked
        case "pending": self = .pending(notetaker: try container.decode(String.self, forKey: .notetaker))
        case "attempted":
            self = .attempted(receipt: try container.decode(MeetingConsentDeliveryReceipt.self, forKey: .receipt))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown disclosure \(kind)")
        }
    }

    /// The room was not told, and the target is what refused it.
    var refused: Bool {
        if case let .attempted(receipt) = self { receipt.outcome == .definitelyNotDispatched } else { false }
    }

    var notetaker: String? {
        if case let .pending(notetaker) = self { notetaker } else { nil }
    }
}

/// `DeliveryReceipt`, as a disclosure carries it.
struct MeetingConsentDeliveryReceipt: Decodable {
    let method: MeetingConsentDeliveryMethod
    let outcome: MeetingConsentDeliveryOutcome
    let dispatchedAtMs: Int64
}

enum MeetingConsentDeliveryMethod: String, Decodable {
    case none
    case accessibilityInsertion = "accessibility_insertion"
    case clipboardPaste = "clipboard_paste"
    case directTyping = "direct_typing"
    case externalScript = "external_script"
}

enum MeetingConsentDeliveryOutcome: String, Decodable {
    case delivered
    case definitelyNotDispatched = "definitely_not_dispatched"
    case dispatchedButUnconfirmed = "dispatched_but_unconfirmed"
    case dispatchedUnderSecureInput = "dispatched_under_secure_input"
}

/// `MeetingConsentPanelSessionState`: the capture the panel is showing, the
/// standing series grant behind it when there is one, and its disclosure.
struct MeetingConsentPanelSessionState: Decodable {
    let snapshot: MeetingSessionSnapshot
    let standingSeriesKey: String?
    let disclosure: MeetingConsentDisclosure
}

// MARK: - Suggestions

/// `MeetingSuggestion`: an offer raised by a running meeting application. The
/// payload is content-free by design — provider, bundle id, evidence flags,
/// two instants — so the card it draws is short.
struct MeetingSuggestion: Decodable, Identifiable {
    let offerId: String
    let provider: MeetingProvider
    let appBundleId: String
    let evidenceFlags: MeetingSuggestionEvidence
    let observedAtNs: Int64
    let expiresAtNs: Int64

    var id: String { offerId }
    /// `meetings.detected.mayBeActive`.
    var title: String { "A meeting may be active in \(provider.label)." }
}

/// `MeetingEvidenceFlags`: which signals identified the call.
struct MeetingSuggestionEvidence: Decodable {
    let appOnly: Bool
    let axTitle: Bool
    let axHost: Bool
    let axUnavailable: Bool
}

// MARK: - Starting

/// What a press on Start asks for, gathered by the surface that offered it.
/// Travels with the request so the gate can rebuild the same consent after a
/// blocked attempt.
struct MeetingStartOptions {
    var title: String
    var origin: MeetingOrigin
    var suggestionId: String?
    var calendarEventKey: String?
    var sources: [MeetingSourceKind]
    var degradedStartPolicy: MeetingDegradedStartPolicy = .abortIfRequiredSourceFails
    /// The facts to preview above the gate, when the start came from a meeting
    /// Sona had already identified.
    var preview: MeetingStartFacts?

    func consent(acceptedMissingSources: [MeetingSourceKind], acceptPartial: Bool) -> MeetingConsentInput {
        MeetingConsentInput(
            sources: sources, acceptedMissingSources: acceptedMissingSources,
            acceptPartial: acceptPartial, degradedStartPolicy: degradedStartPolicy)
    }
}

/// `MeetingPreviewFacts`: the one shape a meeting takes before it is recorded.
/// Pure data — a row exists only where the core supplied its value, so an
/// event with no attendee list has no participants row rather than an empty
/// one.
struct MeetingStartFacts: Identifiable {
    enum Origin { case calendar, app }

    let id: String
    let title: String
    let origin: Origin
    var startUtcMs: Int64?
    var endUtcMs: Int64?
    var calendarName: String?
    var appName: String?
    var attendeeCount: Int?
    var participants: [MeetingStartParticipant] = []
    var description: String?
    var url: String?

    /// The header never renders blank: name the origin honestly instead.
    var heading: String {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty { return trimmed }
        if let appName, !appName.isEmpty { return appName }
        return origin == .calendar ? "Calendar event" : "Microphone in use"
    }

    var durationSeconds: TimeInterval? {
        guard let startUtcMs, let endUtcMs else { return nil }
        return max(0, TimeInterval(endUtcMs - startUtcMs) / 1000)
    }

    /// `eventFacts`: everything the calendar left empty stays nil.
    init(event: DetectionCalendarEvent) {
        id = event.eventKey
        title = event.title
        origin = .calendar
        startUtcMs = event.startUtcMs
        endUtcMs = event.endUtcMs
        calendarName = event.calendarName
        appName = nil
        attendeeCount = event.attendeeCount
        participants = (event.attendees ?? []).map {
            MeetingStartParticipant(name: $0.name, status: $0.status, isSelf: $0.isSelf)
        }
        description = event.notes
        url = event.url
    }

    /// `suggestionFacts`: the app and nothing else.
    init(suggestion: MeetingSuggestion) {
        id = suggestion.offerId
        title = suggestion.title
        origin = .app
        appName = suggestion.provider.label
    }

    /// A prompt still waiting: its own title, and the app when it names one.
    init(prompt: DetectionPromptEvent) {
        id = prompt.promptId
        title = prompt.title
        origin = prompt.prompt.isCalendar ? .calendar : .app
        appName = prompt.prompt.appName
    }

    /// An upcoming calendar row, as the week-ahead list reads it.
    init(row: UpcomingRow) {
        id = row.eventKey
        title = row.title
        origin = .calendar
        startUtcMs = row.startUtcMs
        endUtcMs = row.endUtcMs
        calendarName = row.calendarName
        attendeeCount = row.attendeeCount
        participants = row.attendees.map {
            MeetingStartParticipant(name: $0.name, status: $0.status, isSelf: $0.isSelf)
        }
        url = row.joinUrl
    }
}

struct MeetingStartParticipant {
    let name: String
    let status: DetectionParticipation
    let isSelf: Bool
}

// MARK: - Requests

/// The params for every command this slice sends, in the exact wire keys. The
/// command arguments are camelCase (`sessionId`, `promptId`, `days`) and the
/// request structs inside them keep serde's snake_case.
enum MeetingLiveRequest {
    /// `MeetingPreflightCreateRequest`. `expected_revision` is 0: the session
    /// does not exist yet.
    static func preflightCreate(_ options: MeetingStartOptions) -> [String: JSONValue] {
        MeetingRequest.wrap([
            "operation_id": .string(MeetingRequest.operationId()),
            "expected_revision": .number(0),
            "title": .string(options.title.trimmingCharacters(in: .whitespacesAndNewlines)),
            "origin": .string(options.origin.rawValue),
            "suggestion_id": options.suggestionId.map { JSONValue.string($0) } ?? .null,
            "calendar_event_key": options.calendarEventKey.map { JSONValue.string($0) } ?? .null,
            "requested_sources": .array(options.sources.map { .string($0.rawValue) }),
            "required_sources": .array(options.sources.map { .string($0.rawValue) }),
            "accepted_known_missing_sources": .array([]),
            "degraded_start_policy": .string(options.degradedStartPolicy.rawValue),
            "destination": .object(["kind": .string("local")]),
            "remote_acknowledgement": .null,
            "microphone_device_uid": .null,
            "frozen_system_audio_application_bundle_ids": .array([]),
        ])
    }

    /// `MeetingStartRequest`: the mutation plus the consent the press expressed.
    static func start(_ sessionId: MeetingSessionId, revision: Int,
                      consent: MeetingConsentInput) -> [String: JSONValue] {
        var request = MeetingRequest.mutation(sessionId, revision: revision)
        request["consent"] = consent.json
        return MeetingRequest.wrap(request)
    }

    /// `meeting_stop` takes the mutation and the surface the press came from.
    static func stop(_ sessionId: MeetingSessionId, revision: Int,
                     surface: MeetingLiveStopSurface) -> [String: JSONValue] {
        [
            "request": .object(MeetingRequest.mutation(sessionId, revision: revision)),
            "surface": .string(surface.rawValue),
        ]
    }

    /// `MeetingConsentPanelStartRequest`.
    static func consentPanelStart(promptId: String, consent: MeetingConsentInput,
                                  alwaysRecordSeries: Bool, announceInChat: Bool) -> [String: JSONValue] {
        MeetingRequest.wrap([
            "prompt_id": .string(promptId),
            "operation_id": .string(MeetingRequest.operationId()),
            "consent": consent.json,
            "always_record_series": .bool(alwaysRecordSeries),
            "announce_in_chat": .bool(announceInChat),
        ])
    }

    /// `ImportRecordingRequest`. A recording carries no title and no recorded
    /// instant of its own: the core reads both off the file.
    static func importRecording(path: String) -> [String: JSONValue] {
        MeetingRequest.wrap([
            "path": .string(path),
            "title": .null,
            "recorded_at_utc_ms": .null,
            "origin": .object(["kind": .string("local_file")]),
        ])
    }

    static func announceDisclosure(_ sessionId: MeetingSessionId, line: String) -> [String: JSONValue] {
        ["sessionId": .string(sessionId), "line": .string(line)]
    }

    static func prompt(_ promptId: String) -> [String: JSONValue] {
        ["promptId": .string(promptId)]
    }

    static func promptRespond(_ promptId: String, accepted: Bool) -> [String: JSONValue] {
        ["promptId": .string(promptId), "accepted": .bool(accepted)]
    }

    static func ritual(_ ritualId: String) -> [String: JSONValue] {
        ["ritualId": .string(ritualId)]
    }

    static func ritualRespond(_ ritualId: String, action: RitualAction) -> [String: JSONValue] {
        ["ritualId": .string(ritualId), "action": .string(action.rawValue)]
    }
}

/// `MeetingStopSurface`: which Stop control was pressed. The core records it,
/// because a stop from the panel and a stop from the live screen are different
/// acts.
enum MeetingLiveStopSurface: String {
    case meetingLive = "meeting_live"
    case consentPanel = "consent_panel"
}

/// `MeetingLocalEngineStatus`: whether anything on this Mac can turn the words
/// into notes.
enum MeetingLiveLocalEngineStatus: Decodable {
    case appleIntelligence(available: Bool)
    case localEndpoint(reachable: Bool, modelCount: Int, error: String?)

    private enum Key: String, CodingKey { case kind, available, reachable, modelCount, error }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "apple_intelligence":
            self = .appleIntelligence(available: try container.decode(Bool.self, forKey: .available))
        case "local_endpoint":
            self = .localEndpoint(
                reachable: try container.decode(Bool.self, forKey: .reachable),
                modelCount: try container.decode(Int.self, forKey: .modelCount),
                error: try container.decodeIfPresent(String.self, forKey: .error))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown local engine \(kind)")
        }
    }

    /// Whether notes can be written here at all.
    var available: Bool {
        switch self {
        case let .appleIntelligence(available): available
        case let .localEndpoint(reachable, modelCount, _): reachable && modelCount > 0
        }
    }

    /// The line the start surface shows, and nothing while processing is fine.
    var warning: String? {
        switch self {
        case let .appleIntelligence(available):
            available ? nil : "Apple Intelligence is unavailable, so no notes will be written for this meeting."
        case let .localEndpoint(reachable, modelCount, error):
            if !reachable {
                error.map { "The local model endpoint is unreachable: \($0)" }
                    ?? "The local model endpoint is unreachable, so no notes will be written."
            } else if modelCount == 0 {
                "The local model endpoint has no model loaded, so no notes will be written."
            } else {
                nil
            }
        }
    }
}

// MARK: - Detection

/// `CalendarAccess`, which the upcoming section reads as well: an empty week
/// under `authorized` is a free week, and an empty week under anything else is
/// a missing grant.
enum DetectionCalendarAccess: String, Decodable {
    case notDetermined = "not_determined"
    case authorized
    case denied
    case unavailable
}

enum DetectionNotificationAccess: String, Decodable {
    case notDetermined = "not_determined"
    case authorized
    case denied
    case unavailable
}

enum DetectionParticipation: String, Decodable {
    case unknown
    case pending
    case accepted
    case declined
    case tentative

    /// `meetings.preview.participation.*`.
    var label: String {
        switch self {
        case .unknown: "No answer recorded"
        case .pending: "No reply yet"
        case .accepted: "Accepted"
        case .declined: "Declined"
        case .tentative: "Maybe"
        }
    }
}

/// `CalendarAttendee`.
struct DetectionCalendarAttendee: Decodable {
    let name: String
    let status: DetectionParticipation
    let email: String?
    let isSelf: Bool
}

/// `CalendarEventSummary`: one occurrence, as detection read it from EventKit.
struct DetectionCalendarEvent: Decodable {
    let eventKey: String
    let seriesKey: String?
    let title: String
    /// Including the organizer, and including participants EventKit refused to
    /// name. Zero means the event carries no attendee list at all.
    let attendeeCount: Int
    let startUtcMs: Int64
    let endUtcMs: Int64
    let attendees: [DetectionCalendarAttendee]?
    let notes: String?
    let calendarName: String?
    let url: String?
}

/// `SuppressReason`: why detection is quiet, when it is.
enum DetectionSuppressReason: String, Decodable {
    case detectionDisabled = "detection_disabled"
    case sonaHoldsInputDevice = "sona_holds_input_device"
    case captureAlreadyActive = "capture_already_active"
    case noQualifyingSignal = "no_qualifying_signal"
    case attendeeFloorNotMet = "attendee_floor_not_met"
    case unknownMicSource = "unknown_mic_source"
    case browserTitleUnreadable = "browser_title_unreadable"
    case browserTitleNotMeeting = "browser_title_not_meeting"
    case appPresentNotInUse = "app_present_not_in_use"
    case sonaMicJustClosed = "sona_mic_just_closed"

    /// `meetings.detection.why.*`.
    var label: String {
        switch self {
        case .detectionDisabled: "Detection is off."
        case .sonaHoldsInputDevice:
            "Sona is using the microphone, so nothing else can be identified right now."
        case .captureAlreadyActive: "A meeting is already being captured."
        case .noQualifyingSignal: "Nothing is using the microphone yet."
        case .attendeeFloorNotMet:
            "The next event has fewer than two attendees, so it reads as blocked time."
        case .unknownMicSource:
            "Something is using the microphone, but it is not a known meeting app."
        case .browserTitleUnreadable:
            "A browser is in front, but its tab title cannot be read."
        case .browserTitleNotMeeting: "A browser is in front and its tab is not a call."
        case .appPresentNotInUse:
            "A meeting app is open, but you have not switched to it since the microphone came on."
        case .sonaMicJustClosed:
            "The microphone Sona itself just used still reads as in use, so nothing else can be identified yet."
        }
    }
}

/// `DetectionSettings`. This slice reads them; the detection settings section
/// owns writing them.
struct DetectionSettings: Decodable {
    let enabled: Bool
    let calendarEnabled: Bool
    let anyMicActivity: Bool
    let autoStartOnOpenPane: Bool
    let meetingApps: [String]
    /// Bundle IDs that record without a prompt.
    let autoRecordApps: [String]
}

/// `PersonBriefingLastMeeting`.
struct DetectionBriefingLastMeeting: Decodable {
    let id: MeetingSessionId
    let title: String
    let atUtcMs: Int64
    let headline: String?
}

/// `PersonBriefingRow`: the deterministic relationship context a countdown
/// carries. Only the fields the countdown card reads are decoded.
struct DetectionBriefingRow: Decodable, Identifiable {
    let personId: MeetingPersonId
    let displayName: String
    let meetingsCount: Int
    let last: DetectionBriefingLastMeeting?
    let openLoops: [DetectionBriefingLoop]

    var id: MeetingPersonId { personId }
}

/// `PersonOpenLoop`, as a briefing row carries it.
struct DetectionBriefingLoop: Decodable, Identifiable {
    let loopId: MeetingLoopId
    let meetingId: MeetingSessionId
    /// The words as the ledger recorded them.
    let text: String

    var id: MeetingLoopId { loopId }
}

/// `DetectionCountdown`: the event about to start, and what it is about.
struct DetectionCountdown: Decodable {
    let event: DetectionCalendarEvent
    let secondsToStart: Int
    let briefing: [DetectionBriefingRow]
}

/// `AdoptedCall`: the call a hand-started capture took as its stop trigger.
struct DetectionAdoptedCall: Decodable {
    let bundleId: String
    let displayName: String
}

/// `DetectionStatus`: the whole detection read. Already camelCase on the wire.
struct DetectionStatus: Decodable {
    let eventSchemaVersion: Int
    let settings: DetectionSettings
    let calendarAccess: DetectionCalendarAccess
    let notificationAccess: DetectionNotificationAccess
    /// Some process holds the default input device.
    let inputDeviceActive: Bool
    /// That process is Sona itself.
    let sonaHoldsInputDevice: Bool
    let suppressReason: DetectionSuppressReason?
    let countdown: DetectionCountdown?
    let adoptedCall: DetectionAdoptedCall?
    /// Allowlisted bundle IDs running right now. Empty is a legitimate answer.
    let runningMeetingApps: [String]
    /// The Bluetooth-microphone false negative: nothing reports holding the
    /// input device while a meeting app is frontmost.
    let inputDeviceReportingSuspect: Bool
}

/// `DetectionPromptDelivery`: which surface owns this delivery. Only
/// `inAppOnly` needs the main window to say anything.
enum DetectionPromptDelivery: String, Decodable {
    case panel
    case notification
    case inAppOnly = "in_app_only"
}

/// `DetectionPromptKind`: what was detected, and the identity it named.
enum DetectionPromptKind: Decodable {
    case calendarEvent(eventKey: String, eventTitle: String)
    case appMeeting(bundleId: String, appName: String)
    case appHuddle(bundleId: String, appName: String)
    case browserCall(bundleId: String, appName: String)
    case appCall(bundleId: String, appName: String)
    case unknownMicSource

    private enum Key: String, CodingKey { case kind, eventKey, eventTitle, bundleId, appName }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        func app() throws -> (String, String) {
            (try container.decode(String.self, forKey: .bundleId),
             try container.decode(String.self, forKey: .appName))
        }
        switch kind {
        case "CalendarEvent":
            self = .calendarEvent(
                eventKey: try container.decode(String.self, forKey: .eventKey),
                eventTitle: try container.decode(String.self, forKey: .eventTitle))
        case "AppMeeting": let (bundle, name) = try app(); self = .appMeeting(bundleId: bundle, appName: name)
        case "AppHuddle": let (bundle, name) = try app(); self = .appHuddle(bundleId: bundle, appName: name)
        case "BrowserCall": let (bundle, name) = try app(); self = .browserCall(bundleId: bundle, appName: name)
        case "AppCall": let (bundle, name) = try app(); self = .appCall(bundleId: bundle, appName: name)
        case "UnknownMicSource": self = .unknownMicSource
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown prompt \(kind)")
        }
    }

    var isCalendar: Bool { if case .calendarEvent = self { true } else { false } }

    var eventKey: String? {
        if case let .calendarEvent(eventKey, _) = self { eventKey } else { nil }
    }

    /// The calendar event's own title, untouched by the prompt copy.
    var eventTitle: String? {
        if case let .calendarEvent(_, eventTitle) = self { eventTitle } else { nil }
    }

    /// The application this prompt names, for prompts that name one. A
    /// calendar prompt names an event instead, and an unknown microphone
    /// source names nothing at all.
    var appName: String? {
        switch self {
        case let .appMeeting(_, name), let .appHuddle(_, name),
             let .browserCall(_, name), let .appCall(_, name):
            name.trimmingCharacters(in: .whitespaces).isEmpty ? nil : name
        case .calendarEvent, .unknownMicSource:
            nil
        }
    }

    /// `promptTitle` in DetectionListeners.tsx: total by construction, because
    /// the card this titles carries a Record button and an untitled offer to
    /// record is not an offer.
    var title: String {
        switch self {
        case let .calendarEvent(_, eventTitle):
            let title = eventTitle.trimmingCharacters(in: .whitespaces)
            return title.isEmpty ? "Calendar event starting" : "\(title) starting"
        case .appMeeting:
            return appName.map { "\($0) meeting detected" } ?? "Meeting detected"
        case .appCall:
            return appName.map { "\($0) call detected" } ?? "Meeting detected"
        case .appHuddle:
            return appName.map { "\($0) huddle detected" } ?? "Meeting detected"
        case .browserCall:
            return appName.map { "Call detected in \($0)" } ?? "Meeting detected"
        case .unknownMicSource:
            return "Microphone activity detected"
        }
    }

    /// The consent panel's own heading, from `consentPanel.*Title`.
    var consentTitle: String {
        switch self {
        case let .calendarEvent(_, eventTitle):
            let title = eventTitle.trimmingCharacters(in: .whitespaces)
            return title.isEmpty ? "Record this meeting?" : "Record \(title)?"
        case .appCall:
            return appName.map { "Record this \($0) call?" } ?? "Record this meeting?"
        case .appMeeting, .appHuddle, .browserCall:
            return appName.map { "Record this \($0) meeting?" } ?? "Record this meeting?"
        case .unknownMicSource:
            return "Record this meeting?"
        }
    }
}

/// `DetectionPromptEvent`: one offer to record.
struct DetectionPromptEvent: Decodable, Identifiable {
    let eventSchemaVersion: Int
    /// Opaque handle. Echo it back through `detection_prompt_respond`.
    let promptId: String
    let prompt: DetectionPromptKind
    /// The English copy, exactly as the notification shows it.
    let notificationTitle: String
    let delivery: DetectionPromptDelivery
    /// Rendered only after the panel acknowledged its first delivery.
    let showIntroduction: Bool
    /// What this prompt's series remembers about announcing itself.
    let announceInChat: Bool

    var id: String { promptId }
    var title: String { prompt.title }

    /// `startOptions(prompt)` in ConsentPanel.tsx.
    var startOptions: MeetingStartOptions {
        MeetingStartOptions(
            title: prompt.eventTitle ?? notificationTitle,
            origin: .suggestion,
            suggestionId: nil,
            calendarEventKey: prompt.eventKey,
            sources: MeetingSourceKind.allCases,
            preview: MeetingStartFacts(prompt: self))
    }
}

enum DetectionPromptRetractionReason: String, Decodable {
    case triggerAppQuit = "trigger_app_quit"
    case eventEnded = "event_ended"
    case micEpisodeEnded = "mic_episode_ended"
    case callEnded = "call_ended"
    case resolved
}

struct DetectionPromptRetractedEvent: Decodable {
    let eventSchemaVersion: Int
    let promptId: String
    let reason: DetectionPromptRetractionReason
}

// MARK: - Rituals

/// `MeetingRitualAction`: what a press on a ritual card asks for.
enum RitualAction: String {
    case prepRecordWhenStarts = "prep_record_when_starts"
    case prepOpenBrief = "prep_open_brief"
    case prepDismiss = "prep_dismiss"
    case wrapOpenNotes = "wrap_open_notes"
    case wrapFollowUpCopied = "wrap_follow_up_copied"
    case wrapDone = "wrap_done"
    case recordingStop = "recording_stop"
    /// Stop, and take this application off the auto-record list.
    case recordingForgetApp = "recording_forget_app"
}

/// `MeetingPrepParticipant`.
struct RitualParticipant: Decodable {
    let name: String
    let meetingsCount: Int
    let organization: String?

    /// "Aktan · 6 meetings · 99 Point".
    var line: String {
        [name, meetingsCount == 1 ? "1 meeting" : "\(meetingsCount) meetings", organization]
            .compactMap { $0 }
            .filter { !$0.isEmpty }
            .joined(separator: " · ")
    }
}

/// `MeetingPrepCard`: what is about to start, and what was left open last time.
struct RitualPrepCard: Decodable {
    let eventKey: String
    let seriesKey: String
    let title: String
    let startUtcMs: Int64
    let lastMeetingId: MeetingSessionId
    let headline: String
    let mineOpenLoops: [String]
    let mineOpenLoopCount: Int
    let waitingOnCount: Int
    let participants: [RitualParticipant]
    let canRecordWhenStarts: Bool
}

/// `MeetingWrapCard`: the meeting that just saved, and what it left behind.
struct RitualWrapCard: Decodable {
    let sessionId: MeetingSessionId
    let title: String
    let headline: String
    let unresolvedSpeakerCount: Int?
    let followUpCount: Int
    let waitingOnCount: Int
    let waitingOnNames: [String]
}

/// `MeetingRecordingCard`: an auto-started capture, while it runs.
struct RitualRecordingCard: Decodable {
    let sessionId: MeetingSessionId
    let bundleId: String
    let appName: String
    let startedAtUtcMs: Int64
}

/// `MeetingRitual`.
enum RitualKind: Decodable {
    case prep(card: RitualPrepCard)
    case wrap(card: RitualWrapCard)
    case recording(card: RitualRecordingCard)

    private enum Key: String, CodingKey { case kind, card }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "prep": self = .prep(card: try container.decode(RitualPrepCard.self, forKey: .card))
        case "wrap": self = .wrap(card: try container.decode(RitualWrapCard.self, forKey: .card))
        case "recording":
            self = .recording(card: try container.decode(RitualRecordingCard.self, forKey: .card))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown ritual \(kind)")
        }
    }

    var isRecording: Bool { if case .recording = self { true } else { false } }

    var recordingSessionId: MeetingSessionId? {
        if case let .recording(card) = self { card.sessionId } else { nil }
    }
}

/// `MeetingRitualEvent`.
struct RitualEvent: Decodable, Identifiable {
    let eventSchemaVersion: Int
    let ritualId: String
    let ritual: RitualKind
    let notificationTitle: String
    let delivery: DetectionPromptDelivery

    var id: String { ritualId }
}

struct RitualRetractedEvent: Decodable {
    let eventSchemaVersion: Int
    let ritualId: String
}

// MARK: - Upcoming

/// `MeetingUpcomingSeries`: the three decisions a repeating meeting has made.
struct UpcomingSeries: Decodable {
    let seriesKey: String
    /// A live standing grant covers this series, so its occurrences record
    /// themselves.
    let alwaysRecord: Bool
    let template: MeetingNotesTemplate?
    let digestIncluded: Bool
}

/// `MeetingUpcomingAttendee`.
struct UpcomingAttendee: Decodable {
    let name: String
    let status: DetectionParticipation
    /// True for the calendar account's own entry.
    let isSelf: Bool
    let personId: MeetingPersonId?
}

/// `MeetingUpcomingRow`: one occurrence in the week ahead.
struct UpcomingRow: Decodable, Identifiable {
    /// What `meeting_preflight_create` accepts to start this specific event.
    let eventKey: String
    let title: String
    let startUtcMs: Int64
    let endUtcMs: Int64
    let attendees: [UpcomingAttendee]
    /// Including the ones EventKit refused to name, so it can exceed
    /// `attendees.count`.
    let attendeeCount: Int
    let calendarName: String?
    /// For a scheduled call, the join link.
    let joinUrl: String?
    /// Present exactly when the event repeats.
    let series: UpcomingSeries?

    var id: String { eventKey }
    var start: Date { startUtcMs.meetingDate }
}

/// `MeetingUpcomingEvents`.
struct UpcomingEvents: Decodable {
    let access: DetectionCalendarAccess
    let windowStartUtcMs: Int64
    let windowEndUtcMs: Int64
    let rows: [UpcomingRow]
    /// The fence every series control writes with. One number for the pane.
    let seriesRevision: Int
}

/// The week ahead, bucketed by local day the way meeting history is.
struct UpcomingDay: Identifiable {
    let startOfDay: Date
    let rows: [UpcomingRow]

    var id: TimeInterval { startOfDay.timeIntervalSince1970 }
    var heading: String { startOfDay.relativeDay }

    /// `groupByLocalDay`: rows in the order the core sent them, split where
    /// the local day changes.
    static func group(_ rows: [UpcomingRow], calendar: Calendar = .current) -> [UpcomingDay] {
        var days: [UpcomingDay] = []
        var current: [UpcomingRow] = []
        var day: Date?
        for row in rows {
            let start = calendar.startOfDay(for: row.start)
            if let day, day != start {
                days.append(UpcomingDay(startOfDay: day, rows: current))
                current = []
            }
            day = start
            current.append(row)
        }
        if let day { days.append(UpcomingDay(startOfDay: day, rows: current)) }
        return days
    }
}

// MARK: - Reading a clock

extension Int64 {
    /// `elapsedLabel`: how long a capture has been running. A start that is
    /// missing, zero or in the future is not a start to count from.
    func meetingElapsed(since now: Date) -> String {
        guard self > 0 else { return TimeInterval(0).clock }
        return Swift.max(0, now.timeIntervalSince(meetingDate)).clock
    }
}
