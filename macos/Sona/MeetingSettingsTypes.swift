import Foundation

/// The wire shapes behind the meeting settings: detection, retention, the
/// evening digest, keyword trackers, meeting intelligence, and what a recorded
/// series does once its notes are written.
///
/// Field names mirror `src-tauri/src` with snake_case turned into camelCase by
/// `Core.decoder`. Enum values are the wire strings verbatim. Everything a
/// command takes is written by hand here, because the fenced writes send
/// snake_case request bodies while the params around them are camelCase.

extension CoreEvent {
    /// Detection's whole state, emitted on change.
    static let meetingDetectionStatus = "detection-status"
    /// The settings file changed. Carries nothing useful: re-read the settings.
    static let meetingSettingsChanged = "settings-changed"
}

// MARK: - Detection

/// A macOS permission, as detection reports it. Calendar and notifications
/// share the vocabulary.
enum MeetingDetectionAccess: String, Decodable {
    case notDetermined = "not_determined"
    case authorized
    case denied
    case unavailable
}

/// Why detection is quiet. Silent detection is indistinguishable from broken
/// detection, so every reason says itself in a sentence.
enum MeetingDetectionSuppressReason: String {
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

    var sentence: String {
        switch self {
        case .detectionDisabled: "Detection is off."
        case .sonaHoldsInputDevice: "Sona is using the microphone, so nothing else can be identified right now."
        case .captureAlreadyActive: "A meeting is already being captured."
        case .noQualifyingSignal: "Nothing is using the microphone yet."
        case .attendeeFloorNotMet: "The next event has fewer than two attendees, so it reads as blocked time."
        case .unknownMicSource: "Something is using the microphone, but it is not a known meeting app."
        case .browserTitleUnreadable: "A browser is in front, but its tab title cannot be read."
        case .browserTitleNotMeeting: "A browser is in front and its tab is not a call."
        case .appPresentNotInUse: "A meeting app is open, but you have not switched to it since the microphone came on."
        case .sonaMicJustClosed: "The microphone Sona itself just used still reads as in use, so nothing else can be identified yet."
        }
    }
}

/// The call a hand-started capture adopted as its stop trigger.
struct MeetingDetectionAdoptedCall: Decodable, Equatable {
    let bundleId: String
    let displayName: String
}

/// Detection's whole policy. One value rather than six setters: the backend
/// refuses to represent a half-written state such as calendar-on-while-
/// detection-off, so every write sends the struct.
struct MeetingDetectionSettings: Codable, Equatable {
    var enabled: Bool
    var calendarEnabled: Bool
    var anyMicActivity: Bool
    /// Still on the wire and still round-tripped, but no row writes it: the
    /// consent slice took capture authority away from it.
    var autoStartOnOpenPane: Bool
    var meetingApps: [String]
    /// Bundle IDs that record without a prompt. Read for call apps only.
    var autoRecordApps: [String]
}

/// Everything the operator can see about what detection is doing.
struct MeetingDetectionStatus: Decodable {
    let settings: MeetingDetectionSettings
    let calendarAccess: MeetingDetectionAccess
    let notificationAccess: MeetingDetectionAccess
    let inputDeviceActive: Bool
    let sonaHoldsInputDevice: Bool
    let suppressReason: MeetingDetectionSuppressReason?
    let adoptedCall: MeetingDetectionAdoptedCall?
    /// Allowlisted bundle IDs whose application is running right now.
    let runningMeetingApps: [String]
    /// The Bluetooth false negative: a meeting app is frontmost and nothing
    /// reports holding the input device.
    let inputDeviceReportingSuspect: Bool

    private enum Key: String, CodingKey {
        case settings, calendarAccess, notificationAccess, inputDeviceActive
        case sonaHoldsInputDevice, suppressReason, adoptedCall
        case runningMeetingApps, inputDeviceReportingSuspect
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        settings = try container.decode(MeetingDetectionSettings.self, forKey: .settings)
        calendarAccess = try container.decode(MeetingDetectionAccess.self, forKey: .calendarAccess)
        notificationAccess = try container.decode(MeetingDetectionAccess.self, forKey: .notificationAccess)
        inputDeviceActive = try container.decode(Bool.self, forKey: .inputDeviceActive)
        sonaHoldsInputDevice = try container.decode(Bool.self, forKey: .sonaHoldsInputDevice)
        // A reason this build does not know costs one line of the status
        // section, never the whole read.
        suppressReason = try container.decodeIfPresent(String.self, forKey: .suppressReason)
            .flatMap(MeetingDetectionSuppressReason.init(rawValue:))
        adoptedCall = try container.decodeIfPresent(MeetingDetectionAdoptedCall.self, forKey: .adoptedCall)
        runningMeetingApps = try container.decodeIfPresent([String].self, forKey: .runningMeetingApps) ?? []
        inputDeviceReportingSuspect = try container.decode(Bool.self, forKey: .inputDeviceReportingSuspect)
    }
}

// MARK: - The apps detection knows by name

/// One product the picker offers as a name rather than an identifier.
struct MeetingAppsEntry: Identifiable {
    let id: String
    let name: String
    /// What ticking the box stores: what the backend seeds itself with.
    let writes: [String]
    /// Every identifier that reads as this product, which is wider than
    /// `writes` wherever the activation observer knows further ones.
    let matches: [String]
    /// True for the apps a standing auto-record grant is read for.
    let isCall: Bool
}

/// The six products Sona recognises, and the rules the list obeys.
enum MeetingAppsCatalog {
    static let known: [MeetingAppsEntry] = [
        MeetingAppsEntry(id: "zoom", name: "Zoom", writes: ["us.zoom.xos"], matches: ["us.zoom.xos"], isCall: false),
        MeetingAppsEntry(
            id: "teams", name: "Microsoft Teams",
            writes: ["com.microsoft.teams2", "com.microsoft.teams"],
            matches: ["com.microsoft.teams2", "com.microsoft.teams"], isCall: false),
        MeetingAppsEntry(
            id: "webex", name: "Webex",
            writes: ["com.webex.meetingmanager"],
            matches: ["com.webex.meetingmanager", "com.cisco.webex", "com.cisco.webexmeetingsapp"], isCall: false),
        MeetingAppsEntry(
            id: "facetime", name: "FaceTime", writes: ["com.apple.facetime"],
            matches: ["com.apple.facetime"], isCall: true),
        MeetingAppsEntry(
            id: "phone", name: "Phone", writes: ["com.apple.mobilephone"],
            matches: ["com.apple.mobilephone"], isCall: true),
        MeetingAppsEntry(
            id: "slack", name: "Slack", writes: ["com.tinyspeck.slackmacgap"],
            matches: ["com.tinyspeck.slackmacgap"], isCall: false),
    ]

    /// Every identifier that already has a name, so the rest are the entries
    /// that keep their own row.
    static let namedBundleIds: Set<String> = Set(known.flatMap(\.matches))

    /// The format the backend normalises to: trimmed, lowercased, one
    /// identifier per entry. Validating it here is what lets the add sheet
    /// refuse a typo instead of storing an inert entry.
    static func isWellFormed(_ bundleId: String) -> Bool {
        let parts = bundleId.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count >= 2 else { return false }
        for part in parts {
            guard let first = part.first, first.isASCII, first.isLetter || first.isNumber else { return false }
            for character in part where !(character.isASCII && (character.isLetter || character.isNumber || character == "-")) {
                return false
            }
            for character in part where character.isUppercase {
                return false
            }
        }
        return true
    }
}

// MARK: - Retention

/// How long a meeting is kept. A different object from the dictation
/// recordings Essentials governs.
enum MeetingRetentionPolicy: Codable, Equatable, Hashable {
    case forever
    case deleteAfter(days: Int)

    static let dayChoices = [7, 30, 90]

    private enum Key: String, CodingKey { case kind, days }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "forever": self = .forever
        case "delete_after_days": self = .deleteAfter(days: try container.decode(Int.self, forKey: .days))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown retention policy \(kind)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .forever:
            try container.encode("forever", forKey: .kind)
        case let .deleteAfter(days):
            try container.encode("delete_after_days", forKey: .kind)
            try container.encode(days, forKey: .days)
        }
    }

    var label: String {
        switch self {
        case .forever: "Keep until I delete it"
        case let .deleteAfter(days): "Delete after \(days) days"
        }
    }
}

struct MeetingRetentionSnapshot: Decodable {
    let policy: MeetingRetentionPolicy
    let revision: Int
}

struct MeetingRetentionMutation: Decodable {
    let receipt: MeetingSettingsReceipt
    let snapshot: MeetingRetentionSnapshot
}

// MARK: - Receipts and errors

/// Where a fenced write got to. `rejected` means somebody else moved the
/// revision and nothing was written.
enum MeetingSettingsOutcome: String, Decodable {
    case committed, rejected, failed
}

/// The part of an operation receipt a settings row acts on.
struct MeetingSettingsReceipt: Decodable {
    let result: MeetingSettingsOutcome
}

/// The command errors these surfaces can meet, as sentences.
enum MeetingSettingsCommandError: String, Decodable {
    case staleRevision = "stale_revision"
    case invalidRequest = "invalid_request"
    case notFound = "not_found"
    case storageUnavailable = "storage_unavailable"
    case deletionInProgress = "deletion_in_progress"
    case localModelUnavailable = "local_model_unavailable"
    case remoteUnavailable = "remote_unavailable"

    var sentence: String {
        switch self {
        case .staleRevision: "This meeting changed in another window. Sona reloaded it."
        case .invalidRequest: "The meeting request was invalid."
        case .notFound: "This meeting is no longer available."
        case .storageUnavailable: "Encrypted meeting storage is unavailable."
        case .deletionInProgress: "Deletion is already in progress. The meeting remains inaccessible."
        case .localModelUnavailable: "A local model is unavailable for this meeting."
        case .remoteUnavailable: "The selected remote destination is unavailable."
        }
    }
}

// MARK: - Keyword trackers

/// A watch list for words that matter. Every finished transcript is scanned
/// for them on this Mac; the hits show on the meeting's Insights tab.
struct MeetingTracker: Codable, Equatable, Identifiable {
    var name: String
    var patterns: [String]

    /// Local only: the rows are edited in place and saved as one list.
    var id: String { name }

    /// Patterns are edited as one comma-separated line, which is how people
    /// list phrases. Commas inside a phrase are not supported.
    var line: String {
        get { patterns.joined(separator: ", ") }
        set { patterns = newValue.split(separator: ",", omittingEmptySubsequences: false).map(String.init) }
    }
}

// MARK: - Series preferences

/// The shape of notes a series asks for. `nil` hands it back to the app default.
enum MeetingSeriesTemplate: String, Codable, CaseIterable, Identifiable {
    case general
    case oneOnOne = "one_on_one"
    case interview
    case salesCall = "sales_call"
    case standup

    var id: String { rawValue }

    var label: String {
        switch self {
        case .general: "General"
        case .oneOnOne: "One on one"
        case .interview: "Interview"
        case .salesCall: "Sales call"
        case .standup: "Standup"
        }
    }
}

/// Everything one series has decided, and the fence its next write carries.
struct MeetingSeriesPreferences: Decodable {
    /// `nil` when the meeting belongs to no series.
    let seriesKey: String?
    let template: MeetingSeriesTemplate?
    let digestIncluded: Bool
    let alwaysRecord: Bool
    let remoteIntelligenceOptOut: Bool
    let announceInChat: Bool
    let revision: Int
}

struct MeetingSeriesMutation: Decodable {
    let receipt: MeetingSettingsReceipt
    let preferences: MeetingSeriesPreferences
}

// MARK: - Meeting intelligence

/// One series the operator can keep on this Mac.
struct MeetingRemoteSeriesRow: Decodable, Identifiable {
    let seriesKey: String
    let title: String
    let lastMetAtUtcMs: Int64
    let meetings: Int
    let remoteIntelligenceOptOut: Bool

    var id: String { seriesKey }
}

struct MeetingRemoteRoster: Decodable {
    let rows: [MeetingRemoteSeriesRow]
    /// The one fence every switch on these rows writes with.
    let revision: Int
}

/// Where a meeting's summaries, ledgers, recaps and answers get written.
enum MeetingRemoteEngine: Equatable {
    case appleIntelligence
    case localEndpoint(baseUrl: String, model: String, contextWindowTokens: Int?)

    var isEndpoint: Bool {
        if case .localEndpoint = self { return true }
        return false
    }
}

extension MeetingRemoteEngine: Codable {
    private enum Key: String, CodingKey { case kind, baseUrl, model, contextWindowTokens }
    /// The wire spells these snake_case; the decoder converts on the way in,
    /// so only the encode side names them.
    private enum WireKey: String, CodingKey {
        case kind
        case baseUrl = "base_url"
        case model
        case contextWindowTokens = "context_window_tokens"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "apple_intelligence":
            self = .appleIntelligence
        case "local_endpoint":
            self = .localEndpoint(
                baseUrl: try container.decodeIfPresent(String.self, forKey: .baseUrl) ?? "",
                model: try container.decodeIfPresent(String.self, forKey: .model) ?? "",
                contextWindowTokens: try container.decodeIfPresent(Int.self, forKey: .contextWindowTokens))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown meeting engine \(kind)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: WireKey.self)
        switch self {
        case .appleIntelligence:
            try container.encode("apple_intelligence", forKey: .kind)
        case let .localEndpoint(baseUrl, model, contextWindowTokens):
            try container.encode("local_endpoint", forKey: .kind)
            try container.encode(baseUrl, forKey: .baseUrl)
            try container.encode(model, forKey: .model)
            // Explicitly null: the endpoint has no context window until one is
            // typed, and the field is how that is said.
            try container.encode(contextWindowTokens, forKey: .contextWindowTokens)
        }
    }
}

/// What the chosen engine answers right now.
enum MeetingRemoteEngineStatus: Decodable {
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
                modelCount: try container.decodeIfPresent(Int.self, forKey: .modelCount) ?? 0,
                error: try container.decodeIfPresent(String.self, forKey: .error))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown engine status \(kind)")
        }
    }

    var sentence: String {
        switch self {
        case let .appleIntelligence(available):
            available
                ? "Apple Intelligence is available."
                : "Apple Intelligence is not available on this Mac."
        case let .localEndpoint(reachable, modelCount, error):
            if let error {
                MeetingRemoteEngineStatus.endpointFailure(error)
            } else if reachable {
                "Local endpoint is reachable with \(modelCount) model\(modelCount == 1 ? "" : "s")."
            } else {
                "Local endpoint is unreachable."
            }
        }
    }

    /// True while the reader has something to fix.
    var isWarning: Bool {
        switch self {
        case let .appleIntelligence(available): !available
        case let .localEndpoint(reachable, _, error): !reachable || error != nil
        }
    }

    private static func endpointFailure(_ error: String) -> String {
        switch error {
        case "context_window_not_configured": "Local endpoint is reachable, but its context window is not configured."
        case "invalid_endpoint": "The local endpoint address is invalid."
        case "invalid_response": "The local endpoint returned an invalid response."
        case "unreachable": "Local endpoint is unreachable."
        default: "Local endpoint status is unknown."
        }
    }
}

// MARK: - Automations

/// What a recorded series does once its notes are written.
enum MeetingAutomationKind: String, Decodable, CaseIterable, Identifiable {
    case reminders
    case shortcut
    case webhook
    case runPrompt = "run_prompt"

    var id: String { rawValue }

    var label: String {
        switch self {
        case .reminders: "Add my open commitments to Reminders"
        case .shortcut: "Run a Shortcut"
        case .webhook: "Send to a webhook"
        case .runPrompt: "Run a saved prompt"
        }
    }

    var hint: String {
        switch self {
        case .reminders: "Writes what you still owe into a list called Sona. Nothing is ever read back out of it."
        case .shortcut: "Runs the Shortcut by name with the meeting's export on its input. Thirty seconds, then it is stopped."
        case .webhook: "Sends the meeting export as JSON. Use only your own machine or tailnet, as with the server."
        case .runPrompt: "Asks one of your saved prompts about the meeting and keeps the answer with it."
        }
    }

    /// What the row points at, or nil for the one kind with nothing to name.
    var placeholder: String? {
        switch self {
        case .reminders: nil
        case .shortcut: "Shortcut name"
        case .webhook: "http://100.x.y.z:8650/hook"
        case .runPrompt: "Pick a prompt"
        }
    }

    /// The line a row shows while the switch cannot be turned on yet.
    var blockedNote: String? {
        switch self {
        case .reminders: nil
        case .shortcut: "Add a name first"
        case .webhook: "Add an address first"
        case .runPrompt: "Pick a prompt first"
        }
    }
}

/// One kind on one series, as it is stored.
struct MeetingAutomationRule: Decodable {
    /// Nil for a kind this build does not know.
    let kind: MeetingAutomationKind?
    let enabled: Bool
    let target: String?

    private enum Key: String, CodingKey { case kind, enabled, target }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        kind = MeetingAutomationKind(rawValue: try container.decode(String.self, forKey: .kind))
        enabled = try container.decode(Bool.self, forKey: .enabled)
        target = try container.decodeIfPresent(String.self, forKey: .target)
    }
}

/// One series the settings surface lists.
struct MeetingAutomationSeries: Decodable, Identifiable {
    let seriesKey: String
    let title: String
    let lastMetAtUtcMs: Int64
    let meetingCount: Int
    let automations: [MeetingAutomationRule]

    var id: String { seriesKey }

    func rule(_ kind: MeetingAutomationKind) -> MeetingAutomationRule? {
        automations.first { $0.kind == kind }
    }

    func isEnabled(_ kind: MeetingAutomationKind) -> Bool {
        rule(kind)?.enabled ?? false
    }

    func target(_ kind: MeetingAutomationKind) -> String {
        rule(kind)?.target ?? ""
    }
}

/// Every series, and the fence its writes carry. One counter for the table.
struct MeetingAutomationRoster: Decodable {
    let series: [MeetingAutomationSeries]
    let revision: Int
}

struct MeetingAutomationSnapshot: Decodable {
    let seriesKey: String?
    let automations: [MeetingAutomationRule]
    let revision: Int
}

struct MeetingAutomationMutation: Decodable {
    let receipt: MeetingSettingsReceipt
    let snapshot: MeetingAutomationSnapshot
}

struct MeetingAutomationEnableResult: Decodable {
    let mutation: MeetingAutomationMutation
    /// What macOS answers about Reminders, which only a `reminders` write asks.
    let remindersAccess: MeetingDetectionAccess
}

/// Where one attempt got to. `started` means the attempt never reported back.
enum MeetingAutomationRunState: String, Decodable {
    case started, committed, failed
}

/// The receipt for one attempt, kept forever, one row per artifact revision
/// per kind.
struct MeetingAutomationRun: Decodable, Identifiable {
    let artifactId: String
    let sessionId: String
    let seriesKey: String
    let kind: MeetingAutomationKind?
    let state: MeetingAutomationRunState
    let failure: String?
    /// One short line naming what happened. Never the payload, never a URL.
    let detail: String?
    let effects: Int
    let startedAtUtcMs: Int64
    let finishedAtUtcMs: Int64?

    var id: String { artifactId + "\u{0}" + (kind?.rawValue ?? "unknown") }

    private enum Key: String, CodingKey {
        case artifactId, sessionId, seriesKey, kind, state, failure, detail
        case effects, startedAtUtcMs, finishedAtUtcMs
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        artifactId = try container.decode(String.self, forKey: .artifactId)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        seriesKey = try container.decode(String.self, forKey: .seriesKey)
        kind = MeetingAutomationKind(rawValue: try container.decode(String.self, forKey: .kind))
        state = try container.decode(MeetingAutomationRunState.self, forKey: .state)
        failure = try container.decodeIfPresent(String.self, forKey: .failure)
        detail = try container.decodeIfPresent(String.self, forKey: .detail)
        effects = try container.decodeIfPresent(Int.self, forKey: .effects) ?? 0
        startedAtUtcMs = try container.decode(Int64.self, forKey: .startedAtUtcMs)
        finishedAtUtcMs = try container.decodeIfPresent(Int64.self, forKey: .finishedAtUtcMs)
    }
}

// MARK: - Saved prompts, as an automation row needs them

/// One saved prompt, as the `run_prompt` picker needs it: a name for an id.
/// The prompts surface itself owns writing them.
struct MeetingPromptOption: Decodable, Identifiable, Hashable {
    let promptId: String
    let name: String

    var id: String { promptId }
}

/// The prompts on this machine, as the picker reads them.
struct MeetingPromptList: Decodable {
    let prompts: [MeetingPromptOption]
    let revision: Int
}

// MARK: - The settings subset these rows read

/// The part of `get_app_settings` the meeting settings show. Every field is
/// serde-defaulted on the Rust side, so every one is optional here.
struct MeetingSettingsSnapshot: Decodable {
    var digestEnabled = false
    var digestMinuteOfDay = MeetingDigestClock.defaultMinuteOfDay
    var remoteIntelligenceEnabled = false
    var localEngine: MeetingRemoteEngine?
    var agentPanelEnabled = false
    var agentPanelPaired = false
    var relayUrl: String?
    var relayKeyId: String?
    var relayPublicKey: String?

    /// The same four fields the backend's own readiness check reads: a relay
    /// is reachable only when the panel is on, a pairing was saved, and the
    /// pinned key and its URL are both stored.
    var isRelayPaired: Bool {
        agentPanelEnabled && agentPanelPaired && relayUrl != nil && relayKeyId != nil && relayPublicKey != nil
    }

    private enum Key: String, CodingKey {
        case digestEnabled = "meetingDigestEnabled"
        case digestMinuteOfDay = "meetingDigestMinuteOfDay"
        case remoteIntelligenceEnabled = "meetingRemoteIntelligenceEnabled"
        case localEngine = "meetingLocalEngine"
        case agentPanelEnabled = "agentPanelEnabled"
        case agentPanelPaired = "agentPanelPaired"
        case relayUrl = "agentPanelRelayUrl"
        case relayKeyId = "agentPanelRelayKeyId"
        case relayPublicKey = "agentPanelRelayPublicKey"
    }

    /// The unread state: what the rows show before the first read lands, and
    /// every value in it is the core's own default.
    init() {}

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        digestEnabled = try container.decodeIfPresent(Bool.self, forKey: .digestEnabled) ?? false
        digestMinuteOfDay = try container.decodeIfPresent(Int.self, forKey: .digestMinuteOfDay)
            ?? MeetingDigestClock.defaultMinuteOfDay
        remoteIntelligenceEnabled = try container.decodeIfPresent(Bool.self, forKey: .remoteIntelligenceEnabled) ?? false
        localEngine = try container.decodeIfPresent(MeetingRemoteEngine.self, forKey: .localEngine)
        agentPanelEnabled = try container.decodeIfPresent(Bool.self, forKey: .agentPanelEnabled) ?? false
        agentPanelPaired = try container.decodeIfPresent(Bool.self, forKey: .agentPanelPaired) ?? false
        relayUrl = try container.decodeIfPresent(String.self, forKey: .relayUrl)
        relayKeyId = try container.decodeIfPresent(String.self, forKey: .relayKeyId)
        relayPublicKey = try container.decodeIfPresent(String.self, forKey: .relayPublicKey)
    }
}

// MARK: - Small conversions the rows need

/// The digest is stored as minutes past local midnight, so it has no clock
/// format to parse. This is the only place that conversion lives.
enum MeetingDigestClock {
    static let defaultMinuteOfDay = 18 * 60

    /// Minutes past midnight as the "HH:MM" a field shows.
    static func text(_ minuteOfDay: Int) -> String {
        let clamped = min(max(minuteOfDay, 0), 24 * 60 - 1)
        return String(format: "%02d:%02d", clamped / 60, clamped % 60)
    }

    /// "HH:MM" back to minutes, or nil for the half-typed values a field
    /// reports while somebody is still typing into it.
    static func minuteOfDay(_ text: String) -> Int? {
        let parts = text.split(separator: ":", omittingEmptySubsequences: false)
        guard parts.count == 2,
              let hours = Int(parts[0]), let minutes = Int(parts[1]),
              parts[0].count == 2, parts[1].count == 2,
              hours <= 23, minutes <= 59
        else { return nil }
        return hours * 60 + minutes
    }
}

/// The shared spellings these surfaces print.
enum MeetingSettingsFormat {
    /// "Zoom, Webex, Slack +2": the first three names, then how many rows are
    /// not in that list. `total` covers rows with no name to print.
    static func names(_ names: [String], total: Int? = nil) -> String {
        let count = total ?? names.count
        let shown = names.prefix(3)
        let overflow = count - shown.count
        return shown.joined(separator: ", ") + (overflow > 0 ? " +\(overflow)" : "")
    }

    /// "Last met 3 Mar" from a UTC millisecond stamp.
    static func day(_ utcMs: Int64) -> String {
        Date(timeIntervalSince1970: Double(utcMs) / 1000).short
    }
}
