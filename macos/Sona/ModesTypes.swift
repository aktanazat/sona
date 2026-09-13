import Foundation

/// The mode shapes as the core sends them (`src-tauri/src/modes.rs`).
///
/// Results decode with `Core.decoder`, which turns `model_id` into `modelId`;
/// enum values are the wire strings verbatim. A mode goes back to the core
/// through `ModeDefinition.params`, which re-encodes the draft with the
/// opposite key strategy, because the request encoder converts nothing.

// MARK: - Closed choices

enum ModeTone: String, Codable, CaseIterable, Hashable, Identifiable {
    case casual
    case semiCasual = "semi_casual"
    case balanced
    case semiFormal = "semi_formal"
    case formal

    var id: String { rawValue }

    var label: String {
        switch self {
        case .casual: "Casual"
        case .semiCasual: "Semi-casual"
        case .balanced: "Balanced"
        case .semiFormal: "Semi-formal"
        case .formal: "Formal"
        }
    }
}

enum ModePromptPreset: String, Codable, CaseIterable, Hashable, Identifiable {
    case minimalistCleanup = "minimalist_cleanup"
    case applicationContext = "application_context"
    case email
    case meeting
    case notes
    case generic

    var id: String { rawValue }

    var label: String {
        switch self {
        case .minimalistCleanup: "Minimal cleanup"
        case .applicationContext: "Application context"
        case .email: "Email"
        case .meeting: "Meeting"
        case .notes: "Notes"
        case .generic: "General"
        }
    }
}

/// Least to most revealing: the order the privacy ceiling clamps against.
enum ModeContextPolicy: String, Codable, CaseIterable, Hashable, Identifiable {
    case none
    case target
    case targetAndSelection = "target_and_selection"
    case full

    var id: String { rawValue }

    var label: String {
        switch self {
        case .none: "Off"
        case .target: "App"
        case .targetAndSelection: "Selection"
        case .full: "Full"
        }
    }

    var rank: Int { Self.allCases.firstIndex(of: self) ?? 0 }

    /// True when this level asks for more than the ceiling permits.
    func isAbove(_ ceiling: ModeContextPolicy) -> Bool { rank > ceiling.rank }
}

enum ModeRequestedEngine: String, Codable, CaseIterable, Hashable, Identifiable {
    case local
    case deepgramNova3 = "deepgram_nova_3"
    case elevenLabsScribeV2 = "eleven_labs_scribe_v2"

    var id: String { rawValue }

    /// The one fact about a mode's configuration the list carries.
    var summary: String {
        switch self {
        case .local: "Local"
        case .deepgramNova3: "Deepgram"
        case .elevenLabsScribeV2: "ElevenLabs"
        }
    }

    var cloud: ModeCloudProvider? { ModeCloudProvider.all.first { $0.engine == self } }
}

/// A cloud speech provider: the engine value, the keyring account the native
/// secret commands use, and the name a reader sees.
struct ModeCloudProvider: Hashable, Identifiable {
    let engine: ModeRequestedEngine
    /// Consent uses the engine value; the secret store uses this account.
    let secretAccount: String
    let label: String

    var id: String { engine.rawValue }

    static let all: [ModeCloudProvider] = [
        ModeCloudProvider(engine: .deepgramNova3, secretAccount: "deepgram_nova3", label: "Deepgram Nova-3"),
        ModeCloudProvider(engine: .elevenLabsScribeV2, secretAccount: "elevenlabs_scribe_v2", label: "ElevenLabs Scribe v2"),
    ]

    /// The backend refuses audio transfer under an older acknowledgement.
    /// Keep equal to `settings::CLOUD_STT_CONSENT_VERSION`.
    static let consentVersion = 1
}

enum ModePasteMethod: String, Codable, CaseIterable, Hashable, Identifiable {
    case ctrlV = "ctrl_v"
    case direct
    case none
    case shiftInsert = "shift_insert"
    case ctrlShiftV = "ctrl_shift_v"
    case externalScript = "external_script"

    var id: String { rawValue }

    var label: String {
        switch self {
        case .ctrlV: "Paste"
        case .direct: "Type directly"
        case .none: "Do not deliver"
        case .shiftInsert: "Shift+Insert"
        case .ctrlShiftV: "Paste without formatting"
        case .externalScript: "External script"
        }
    }
}

enum ModeClipboardHandling: String, Codable, CaseIterable, Hashable, Identifiable {
    case copyToClipboard = "copy_to_clipboard"
    case dontModify = "dont_modify"

    var id: String { rawValue }

    var label: String {
        switch self {
        case .copyToClipboard: "Copy to clipboard"
        case .dontModify: "Leave unchanged"
        }
    }
}

enum ModeAutoSubmitKey: String, Codable, CaseIterable, Hashable, Identifiable {
    case enter
    case ctrlEnter = "ctrl_enter"
    case cmdEnter = "cmd_enter"

    var id: String { rawValue }

    var label: String {
        switch self {
        case .enter: "Enter"
        case .ctrlEnter: "Ctrl+Enter"
        case .cmdEnter: "Cmd+Enter"
        }
    }
}

/// Linux typing backends. The editor never showed a control for this; the
/// value round-trips untouched so saving a mode on this Mac cannot erase it.
enum ModeTypingTool: String, Codable, CaseIterable, Hashable {
    case auto
    case wtype
    case kwtype
    case dotool
    case ydotool
    case xdotool
}

enum ModeWebsiteHostMatch: String, Codable, CaseIterable, Hashable, Identifiable {
    case exact
    case suffix

    var id: String { rawValue }

    var label: String {
        switch self {
        case .exact: "Exact host"
        case .suffix: "Host and subdomains"
        }
    }
}

// MARK: - The mode itself

struct ModeVocabularyEntry: Codable, Hashable {
    var spoken: String
    var written: String

    init(spoken: String = "", written: String = "") {
        self.spoken = spoken
        self.written = written
    }
}

struct ModeAsr: Codable, Hashable {
    /// Empty inherits the globally selected model when the plan is built.
    var modelId: String
    var language: String
    var translateToEnglish: Bool
    var customWords: [ModeVocabularyEntry]
    var fillerWordRemovalEnabled: Bool
    var customFillerWords: [String]?
    var literalPunctuation: Bool
    var vadEnabled: Bool
    var requestedEngine: ModeRequestedEngine
    var localFallbackEnabled: Bool
    /// Nil means the mode's own local model.
    var localFallbackModelId: String?
    var cloudKeyterms: [String]
    var cloudTimestamps: Bool

    private enum CodingKeys: String, CodingKey {
        case modelId, language, translateToEnglish, customWords, fillerWordRemovalEnabled
        case customFillerWords, literalPunctuation, vadEnabled, requestedEngine
        case localFallbackEnabled, localFallbackModelId, cloudKeyterms, cloudTimestamps
    }

    /// A mode written before a field existed omits it; the defaults are the
    /// ones `modes.rs` declares, so an old mode reads the same either side.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        modelId = try container.decode(String.self, forKey: .modelId)
        language = try container.decode(String.self, forKey: .language)
        translateToEnglish = try container.decode(Bool.self, forKey: .translateToEnglish)
        customWords = try container.decode([ModeVocabularyEntry].self, forKey: .customWords)
        fillerWordRemovalEnabled = try container.decode(Bool.self, forKey: .fillerWordRemovalEnabled)
        customFillerWords = try container.decodeIfPresent([String].self, forKey: .customFillerWords)
        literalPunctuation = try container.decodeIfPresent(Bool.self, forKey: .literalPunctuation) ?? false
        vadEnabled = try container.decode(Bool.self, forKey: .vadEnabled)
        requestedEngine = try container.decodeIfPresent(ModeRequestedEngine.self, forKey: .requestedEngine) ?? .local
        localFallbackEnabled = try container.decodeIfPresent(Bool.self, forKey: .localFallbackEnabled) ?? true
        localFallbackModelId = try container.decodeIfPresent(String.self, forKey: .localFallbackModelId)
        cloudKeyterms = try container.decodeIfPresent([String].self, forKey: .cloudKeyterms) ?? []
        cloudTimestamps = try container.decodeIfPresent(Bool.self, forKey: .cloudTimestamps) ?? true
    }
}

struct ModeLlm: Codable, Hashable {
    var enabled: Bool
    /// Nil inherits the app-wide post-processing provider.
    var providerId: String?
    /// Inert while the provider is inherited.
    var modelId: String
    var spokenInstructions: Bool

    private enum CodingKeys: String, CodingKey {
        case enabled, providerId, modelId, spokenInstructions
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decode(Bool.self, forKey: .enabled)
        providerId = try container.decodeIfPresent(String.self, forKey: .providerId)
        modelId = try container.decode(String.self, forKey: .modelId)
        spokenInstructions = try container.decodeIfPresent(Bool.self, forKey: .spokenInstructions) ?? false
    }
}

struct ModePrompt: Codable, Hashable {
    var preset: ModePromptPreset
    var sourcePromptId: String?
    var customPrompt: String?
}

struct ModeDelivery: Codable, Hashable {
    var pasteMethod: ModePasteMethod
    var clipboardHandling: ModeClipboardHandling
    var autoSubmit: Bool
    var autoSubmitKey: ModeAutoSubmitKey
    var appendTrailingSpace: Bool
    var pasteDelayMs: Int
    var pasteDelayAfterMs: Int
    var reliablePaste: Bool
    var typingTool: ModeTypingTool
    var externalScriptPath: String?
}

/// One chord as `AppSettings.bindings` holds it.
struct ModeShortcut: Decodable, Hashable {
    let id: String
    let name: String
    let description: String
    let defaultBinding: String
    let currentBinding: String

    var changed: Bool { currentBinding != defaultBinding }
}

/// A mode's two chords: start a dictation in it, or switch to it.
struct ModeShortcutPair: Decodable, Hashable {
    let transcribe: ModeShortcut
    let switchTo: ModeShortcut

    private enum CodingKeys: String, CodingKey {
        case transcribe
        case switchTo = "switch"
    }
}

/// What the core stores for a mode. This is the shape `upsert_mode` takes.
struct ModeDefinition: Codable, Hashable {
    var id: String
    var name: String
    var tone: ModeTone
    var contextPolicy: ModeContextPolicy
    var asr: ModeAsr
    var llm: ModeLlm
    var prompt: ModePrompt
    var delivery: ModeDelivery

    init(_ mode: Mode) {
        id = mode.id
        name = mode.name
        tone = mode.tone
        contextPolicy = mode.contextPolicy
        asr = mode.asr
        llm = mode.llm
        prompt = mode.prompt
        delivery = mode.delivery
    }

    /// The draft as request params. The request encoder converts no keys, so
    /// the snake_case names the core reads are produced here and handed on as
    /// already-formed JSON.
    func params() throws -> JSONValue {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        return try JSONDecoder().decode(JSONValue.self, from: encoder.encode(self))
    }
}

/// A mode joined with its current chords: what `get_modes` returns.
struct Mode: Decodable, Identifiable, Hashable {
    let id: String
    let name: String
    let tone: ModeTone
    let contextPolicy: ModeContextPolicy
    let asr: ModeAsr
    let llm: ModeLlm
    let prompt: ModePrompt
    let delivery: ModeDelivery
    let shortcuts: ModeShortcutPair

    /// The mode Sona keeps at all times; the only one that cannot be deleted.
    static let defaultId = "message"

    /// The four modes Sona ships with. They are presets, not fixtures: every
    /// one can be renamed, reordered and (except the default) deleted.
    static let presetIds: Set<String> = [defaultId, "email", "meeting", "notes"]

    var isPreset: Bool { Self.presetIds.contains(id) }

    /// The default mode dictates on the app-wide `transcribe` chord; every
    /// other chord is derived from the mode's ID.
    static func bindingId(_ modeId: String, switching: Bool) -> String {
        if !switching, modeId == defaultId {
            return "transcribe"
        }
        return "mode/\(modeId)/\(switching ? "switch" : "transcribe")"
    }
}

struct ModeActivationRule: Decodable, Hashable {
    let appId: String
    let modeId: String
}

struct ModeWebsiteRule: Decodable, Hashable {
    let host: String
    let matchKind: ModeWebsiteHostMatch
    let modeId: String
}

/// Every mode, which one is active, and the revision every mutation quotes.
struct ModesSnapshot: Decodable {
    let modes: [Mode]
    let activeModeId: String
    let revision: UInt64
    let modeActivationRules: [ModeActivationRule]
    let modeWebsiteActivationRules: [ModeWebsiteRule]
}

// MARK: - Refusals

/// Why the core refused a mode mutation. Tagged by `kind`, and three of the
/// kinds carry data.
enum ModeMutationError: Error, Decodable, Equatable {
    case staleRevision(expected: UInt64, actual: UInt64)
    case invalidModeId
    case emptyName
    case cannotDeleteDefault
    case unknownMode(String)
    case duplicateModeId(String)
    case invalidReorder
    case invalidAppIdentity
    case frontmostApplicationUnavailable
    case invalidWebsiteHost
    case websiteActivationConsentRequired
    case frontmostWebsiteUnavailable
    case websiteActivationSecureField
    case notPersisted
    /// A kind this build does not know, kept so it can still be read out.
    case other(String)

    private enum Key: String, CodingKey { case kind, expectedRevision, actualRevision, modeId }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "stale_revision":
            self = .staleRevision(
                expected: try container.decode(UInt64.self, forKey: .expectedRevision),
                actual: try container.decode(UInt64.self, forKey: .actualRevision))
        case "invalid_mode_id": self = .invalidModeId
        case "empty_name": self = .emptyName
        case "cannot_delete_default": self = .cannotDeleteDefault
        case "unknown_mode": self = .unknownMode(try container.decode(String.self, forKey: .modeId))
        case "duplicate_mode_id": self = .duplicateModeId(try container.decode(String.self, forKey: .modeId))
        case "invalid_reorder": self = .invalidReorder
        case "invalid_app_identity": self = .invalidAppIdentity
        case "frontmost_application_unavailable": self = .frontmostApplicationUnavailable
        case "invalid_website_host": self = .invalidWebsiteHost
        case "website_activation_consent_required": self = .websiteActivationConsentRequired
        case "frontmost_website_unavailable": self = .frontmostWebsiteUnavailable
        case "website_activation_secure_field": self = .websiteActivationSecureField
        case "not_persisted": self = .notPersisted
        default: self = .other(kind)
        }
    }

    /// One sentence per refusal, as the web app words them.
    var message: String {
        switch self {
        case .staleRevision:
            "Settings changed elsewhere. Review the latest mode and save again."
        case .invalidModeId:
            "This mode ID is invalid."
        case .emptyName:
            "A mode name is required."
        case .cannotDeleteDefault:
            "The default mode cannot be deleted."
        case .unknownMode:
            "That mode no longer exists."
        case .duplicateModeId:
            "A mode with this ID already exists."
        case .invalidReorder:
            "The mode order could not be saved."
        case .invalidAppIdentity:
            "That application could not be identified."
        case .frontmostApplicationUnavailable:
            "No application in front could be captured."
        case .invalidWebsiteHost:
            "This website host is invalid."
        case .websiteActivationConsentRequired:
            "Enable Browser URLs in Privacy before adding a website rule."
        case .frontmostWebsiteUnavailable:
            "No browser website could be captured."
        case .websiteActivationSecureField:
            "Website rules cannot be captured from a secure field."
        case .notPersisted:
            "That change was not saved to disk. Try again."
        case let .other(kind):
            "The core refused that change: \(kind)."
        }
    }
}

// MARK: - What the app-wide settings say about a mode

/// The part of `AppSettings` a mode is read against. Every field is optional
/// on the wire, so every field has the answer the core would resolve to.
struct ModeAppSettings: Decodable {
    let selectedModel: String
    let postProcessProviderId: String
    let postProcessProviders: [ModePostProcessProvider]
    let postProcessModels: [String: String]
    let contextPolicyCeiling: ModeContextPolicy
    let contextUrlCaptureEnabled: Bool
    let cloudSttProviders: [ModeCloudConsent]

    private enum Key: String, CodingKey {
        case selectedModel, postProcessProviderId, postProcessProviders, postProcessModels
        case contextPolicyCeiling, contextUrlCaptureEnabled, cloudSttProviders
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        selectedModel = try container.decodeIfPresent(String.self, forKey: .selectedModel) ?? ""
        postProcessProviderId = try container.decodeIfPresent(String.self, forKey: .postProcessProviderId) ?? ""
        postProcessProviders = try container.decodeIfPresent([ModePostProcessProvider].self, forKey: .postProcessProviders) ?? []
        postProcessModels = try container.decodeIfPresent([String: String].self, forKey: .postProcessModels) ?? [:]
        contextPolicyCeiling = try container.decodeIfPresent(ModeContextPolicy.self, forKey: .contextPolicyCeiling) ?? .none
        contextUrlCaptureEnabled = try container.decodeIfPresent(Bool.self, forKey: .contextUrlCaptureEnabled) ?? false
        cloudSttProviders = try container.decodeIfPresent([ModeCloudConsent].self, forKey: .cloudSttProviders) ?? []
    }

    /// Whether audio may leave the device for this provider: a current
    /// acknowledgement of all three terms, nothing older.
    func hasCurrentConsent(_ provider: ModeCloudProvider) -> Bool {
        guard let state = cloudSttProviders.first(where: { $0.provider == provider.engine }) else {
            return false
        }
        return state.consentVersion == ModeCloudProvider.consentVersion
            && state.audioTransferConsent
            && state.privacyConsent
            && state.localFallbackConsent
    }
}

struct ModePostProcessProvider: Decodable, Hashable {
    let id: String
    let label: String
    let baseUrl: String
}

struct ModeCloudConsent: Decodable {
    let provider: ModeRequestedEngine
    let consentVersion: Int
    let audioTransferConsent: Bool
    let privacyConsent: Bool
    let localFallbackConsent: Bool

    private enum Key: String, CodingKey {
        case provider, consentVersion, audioTransferConsent, privacyConsent, localFallbackConsent
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        provider = try container.decode(ModeRequestedEngine.self, forKey: .provider)
        consentVersion = try container.decodeIfPresent(Int.self, forKey: .consentVersion) ?? 0
        audioTransferConsent = try container.decodeIfPresent(Bool.self, forKey: .audioTransferConsent) ?? false
        privacyConsent = try container.decodeIfPresent(Bool.self, forKey: .privacyConsent) ?? false
        localFallbackConsent = try container.decodeIfPresent(Bool.self, forKey: .localFallbackConsent) ?? false
    }
}

/// Whether a provider has a key in the system credential store.
struct ModeSecretState: Decodable {
    let configured: Bool
}

/// The rewrite models a provider publishes, plus why the list is what it is.
struct ModeLlmCatalog: Decodable {
    let providerId: String
    let models: [ModeLlmCatalogOption]
    let discovery: String
    let allowsManualModelId: Bool

    /// The one sentence worth printing beside the field, or nothing when the
    /// list simply loaded.
    var status: String? {
        switch discovery {
        case "ready": nil
        case "requires_consent": "Allow remote text transfer before loading models."
        case "missing_credential": "Add an API key before loading models."
        case "credential_unavailable", "credential_corrupt", "credential_busy": "The API key is unavailable."
        case "credential_locked": "Unlock the API key before loading models."
        case "invalid_destination": "This provider destination is invalid or local-only, so no remote consent can be recorded."
        case "unsupported": "This provider does not publish a compatible model list. Enter a model ID manually."
        case "unauthorized": "The provider rejected the API key."
        case "forbidden": "The provider does not allow this model list."
        case "rate_limited": "The provider is rate limiting model discovery."
        case "unreachable": "Sona could not reach this provider."
        case "invalid_response": "The provider returned an invalid model list."
        default: "The model list could not be read."
        }
    }
}

struct ModeLlmCatalogOption: Decodable, Hashable {
    let id: String
}

/// Which provider and model this draft's rewrite will actually use.
///
/// The mirror of `ModeLlmSettings::destination`, and the only copy of that
/// rule on this side: the editor renders an unsaved draft, so the core cannot
/// answer for a provider the user picked two keystrokes ago. One fallback and
/// nothing else, precisely so the two copies cannot drift apart.
struct ModeLlmDestination: Equatable {
    let providerId: String
    let modelId: String
    /// The mode named no provider of its own and took the app-wide one.
    let inherited: Bool

    init(_ llm: ModeLlm, _ settings: ModeAppSettings?) {
        if let override = llm.providerId {
            providerId = override
            modelId = llm.modelId
            inherited = false
            return
        }
        let global = settings?.postProcessProviderId ?? ""
        providerId = global
        modelId = settings?.postProcessModels[global] ?? ""
        inherited = true
    }
}

// MARK: - Pure list rules

/// The order the list would have after nudging one mode by one position.
///
/// The list reorders two ways — dragging a row, or the move up/down items —
/// and the core takes only a full ordered ID list. The drag already produces
/// one; this is what the keyboard path produces, so both commit through the
/// same command with the same shape. Returns the input unchanged when the
/// move would leave the list, which is what a disabled menu item means.
enum ModesOrder {
    static func withMove(_ ids: [String], _ modeId: String, by direction: Int) -> [String] {
        guard let from = ids.firstIndex(of: modeId) else { return ids }
        let to = from + direction
        guard to >= 0, to < ids.count else { return ids }
        var next = ids
        next.swapAt(from, to)
        return next
    }

    /// The order after a drag drops `modeId` onto the row at `index`.
    static func withMove(_ ids: [String], _ modeId: String, to index: Int) -> [String] {
        guard let from = ids.firstIndex(of: modeId), from != index, index >= 0, index < ids.count else {
            return ids
        }
        var next = ids
        next.remove(at: from)
        next.insert(modeId, at: min(index, next.count))
        return next
    }
}

/// Every revisioned action a row offers, resolved against the row's position
/// and the mutation in flight. Rendering it is the menu's only job, which is
/// what makes "the default mode cannot be deleted" and "the top row cannot
/// move up" true without opening a menu.
///
/// The active mode has nothing to activate, so that entry drops out entirely
/// rather than sitting in the menu greyed.
struct ModeRowAction: Identifiable {
    enum Kind: String { case activate, duplicate, moveUp, moveDown, delete }

    let kind: Kind
    let label: String
    let disabled: Bool
    let destructive: Bool

    var id: String { kind.rawValue }

    static func all(for mode: Mode, index: Int, count: Int, isActive: Bool, busy: Bool) -> [ModeRowAction] {
        let isDefault = mode.id == Mode.defaultId
        var actions: [ModeRowAction] = []
        if !isActive {
            actions.append(ModeRowAction(kind: .activate, label: "Activate", disabled: busy, destructive: false))
        }
        actions.append(ModeRowAction(kind: .duplicate, label: "Duplicate", disabled: busy, destructive: false))
        actions.append(ModeRowAction(kind: .moveUp, label: "Move up", disabled: busy || index == 0, destructive: false))
        actions.append(ModeRowAction(
            kind: .moveDown, label: "Move down", disabled: busy || index == count - 1, destructive: false))
        actions.append(ModeRowAction(
            kind: .delete,
            label: isDefault ? "The default mode cannot be deleted." : "Delete",
            disabled: busy || isDefault,
            destructive: true))
        return actions
    }
}

/// One app or website rule, as the editor lists them together: both are the
/// same promise, "when I am here, use this mode".
struct ModeActivationItem: Identifiable {
    let id: String
    /// The stored match key: a bundle identity or a host.
    let target: String
    /// Scope or qualifier under the target, when the target alone is not it.
    let detail: String?
}
