import Foundation

// MARK: - Credential store

/// Which credential namespace a key belongs to. The core keeps speech keys and
/// language-model keys apart, so one account name can hold both.
enum SecretKind: String, Sendable {
    case llm
    case stt
}

/// What the credential store will say about a key, which is never the key.
struct SecretState: Decodable, Equatable, Sendable {
    let configured: Bool
    /// When the provider last accepted the key. Only its presence is read: a
    /// key is verified or it is not.
    let lastVerifiedAt: Double?
    /// The last refusal the store recorded against this account.
    let lastErrorKind: SecretFault?

    var verified: Bool { configured && lastVerifiedAt != nil }
}

/// A refusal from the credential store, the verifier, or the provider.
///
/// Three of the core's error enums land here and the app answers each with the
/// same sentence: the store's own errors, the speech verifier's errors, and the
/// `unknown_provider` a cloud command returns. Keeping the raw value means a
/// case added to the core still shows something true instead of failing the
/// whole decode.
struct SecretFault: Decodable, Equatable, Sendable {
    let raw: String

    init(_ raw: String) {
        self.raw = raw
    }

    init(from decoder: any Decoder) throws {
        raw = try decoder.singleValueContainer().decode(String.self)
    }

    /// The store could not be asked at all: a transport fault, not an answer.
    static let backend = SecretFault("backend")

    var sentence: String {
        switch raw {
        case "not_found": "No API key is saved for this provider."
        case "unavailable": "The system credential store is unavailable."
        case "locked": "The system credential store is locked."
        case "corrupt": "The saved key cannot be read safely."
        case "invalid": "The system credential store did not accept the key."
        case "busy": "The system credential store is busy. Try again."
        case "not_configured": "Save an API key before verifying it."
        case "consent_required": "Acknowledge the provider transfer before verifying."
        case "authentication": "The provider rejected this API key."
        case "quota": "The provider rejected verification because the account is over quota."
        case "network": "The provider could not be reached for verification."
        case "protocol": "The provider returned an unexpected verification response."
        case "unknown_provider": "This cloud provider is not available in this build."
        default: "The system credential store could not complete that action."
        }
    }
}

/// A refusal from one of the two consent commands. Both name a destination the
/// core would not record, so the raw value is kept and read per surface.
struct ProviderConsentFault: Decodable, Equatable, Sendable {
    let raw: String

    init(_ raw: String) {
        self.raw = raw
    }

    init(from decoder: any Decoder) throws {
        raw = try decoder.singleValueContainer().decode(String.self)
    }

    /// The address is the only refusal worth naming: everything else means the
    /// acknowledgement did not reach the disk, which reads the same.
    var postProcessSentence: String {
        raw == "invalid_destination"
            ? "This provider destination is invalid or local-only, so no remote consent can be recorded."
            : "Sona could not save this acknowledgement."
    }

    var cloudSentence: String {
        raw == "unknown_provider"
            ? "This cloud provider is not available in this build."
            : "The acknowledgement could not be saved."
    }
}

// MARK: - Post-processing providers

/// One entry in the core's post-processing provider catalog.
struct Provider: Decodable, Equatable, Identifiable, Sendable {
    let id: String
    let label: String
    let baseUrl: String
    let allowBaseUrlEdit: Bool?
    let supportsStructuredOutput: Bool?

    /// Only the custom entry lets its address be typed; the rest are pinned by
    /// the core so an acknowledgement cannot be pointed somewhere else.
    var editableBaseUrl: Bool { allowBaseUrlEdit ?? false }
}

enum ProviderCatalog {
    /// On-device, so no key, no address, and no transfer acknowledgement.
    static let appleIntelligenceId = "apple_intelligence"
    /// The one provider whose address the reader owns.
    static let customId = "custom"
    /// What the picker shows before the core has answered.
    static let fallbackId = "openai"
}

/// Where a provider's text would go, as an acknowledgement names it.
///
/// The comparison against the stored consent has to survive a trailing slash, a
/// stray query, and credentials in the address, so the destination is rebuilt
/// from its parts rather than compared as typed.
enum ProviderEndpoint: Equatable, Sendable {
    /// Apple Intelligence, or a loopback address: nothing leaves this Mac.
    case local
    /// Not an https address, so no acknowledgement can name it.
    case invalid
    case remote(String)

    var address: String? {
        if case let .remote(value) = self { value } else { nil }
    }

    static func of(_ provider: Provider) -> ProviderEndpoint {
        let trimmed = provider.baseUrl.trimmingCharacters(in: .whitespacesAndNewlines)
        guard var parts = URLComponents(string: trimmed),
              let scheme = parts.scheme?.lowercased(),
              let host = parts.host?.lowercased(),
              !host.isEmpty
        else {
            return .invalid
        }

        let loopback = host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "[::1]"
        if provider.id == ProviderCatalog.appleIntelligenceId || loopback {
            return .local
        }
        guard scheme == "https" else { return .invalid }

        parts.scheme = scheme
        parts.host = host
        parts.user = nil
        parts.password = nil
        parts.query = nil
        parts.fragment = nil
        if parts.port == 443 { parts.port = nil }
        while parts.path.hasSuffix("/") { parts.path.removeLast() }
        guard let address = parts.string else { return .invalid }
        return .remote(address)
    }
}

/// The acknowledgement the core stored for one provider.
struct ProviderConsent: Decodable, Equatable, Sendable {
    /// Optional throughout: a field this build does not recognise must not
    /// blank the settings, and an acknowledgement that cannot be read is
    /// treated as one that was never given.
    let consentVersion: Int?
    let endpoint: String?
    let origin: String?
    let textTransferConsent: Bool?

    /// Granted, and granted for this exact address.
    func grants(_ address: String) -> Bool {
        endpoint == address && textTransferConsent == true
    }
}

/// One model the provider published.
struct PostProcessModel: Decodable, Equatable, Sendable {
    let id: String
    let provenance: String
}

/// How the last model lookup ended.
enum PostProcessDiscovery: String, Decodable, Equatable, Sendable {
    case ready
    case requiresConsent = "requires_consent"
    case missingCredential = "missing_credential"
    case credentialUnavailable = "credential_unavailable"
    case credentialLocked = "credential_locked"
    case credentialCorrupt = "credential_corrupt"
    case credentialBusy = "credential_busy"
    case invalidDestination = "invalid_destination"
    case unsupported
    case unauthorized
    case forbidden
    case rateLimited = "rate_limited"
    case unreachable
    case invalidResponse = "invalid_response"

    /// A state the core adds later is still a failed lookup, not a crash.
    init(from decoder: any Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = PostProcessDiscovery(rawValue: raw) ?? .invalidResponse
    }

    /// What the reader has to do about it. A finished load is not news.
    var sentence: String? {
        switch self {
        case .ready: nil
        case .requiresConsent: "Allow remote text transfer before loading models."
        case .missingCredential: "Add an API key before loading models."
        case .credentialUnavailable, .credentialCorrupt, .credentialBusy: "The API key is unavailable."
        case .credentialLocked: "Unlock the API key before loading models."
        case .invalidDestination:
            "This provider destination is invalid or local-only, so no remote consent can be recorded."
        case .unsupported:
            "This provider does not publish a compatible model list. Enter a model ID manually."
        case .unauthorized: "The provider rejected the API key."
        case .forbidden: "The provider does not allow this model list."
        case .rateLimited: "The provider is rate limiting model discovery."
        case .unreachable: "Sona could not reach this provider."
        case .invalidResponse: "The provider returned an invalid model list."
        }
    }
}

/// What one lookup returned.
struct PostProcessCatalog: Decodable, Equatable, Sendable {
    let providerId: String
    let models: [PostProcessModel]
    let discovery: PostProcessDiscovery
    let allowsManualModelId: Bool

    /// A lookup that never produced a list. The manual-entry answer is carried
    /// over from the last real catalog so the field does not lock up.
    static func failed(
        providerId: String,
        discovery: PostProcessDiscovery,
        allowsManualModelId: Bool
    ) -> PostProcessCatalog {
        PostProcessCatalog(
            providerId: providerId,
            models: [],
            discovery: discovery,
            allowsManualModelId: allowsManualModelId
        )
    }

    /// One result belongs to one provider at one address: changing either asks
    /// a different server a different question.
    static func scope(providerId: String, baseUrl: String) -> String {
        providerId + "\u{0}" + baseUrl.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// A provider result plus the last list that actually loaded.
///
/// The provider list is transient and the saved selection is not, so a failed
/// refresh must not empty the field a reader is looking at.
struct PostProcessCatalogEntry: Equatable, Sendable {
    let catalog: PostProcessCatalog
    let cachedModels: [PostProcessModel]
}

/// Where one row in the model menu came from.
enum PostProcessModelSource: Equatable, Sendable {
    case provider
    case cached
    case saved

    var label: String {
        switch self {
        case .provider: "Provider"
        case .cached: "Cached"
        case .saved: "Saved"
        }
    }
}

struct PostProcessModelChoice: Identifiable, Equatable, Sendable {
    let id: String
    let source: PostProcessModelSource
}

/// One rewrite instruction in the library.
struct PostProcessPrompt: Decodable, Equatable, Identifiable, Sendable {
    let id: String
    let name: String
    let prompt: String
}

// MARK: - Cloud transcription

enum CloudSttProvider: String, CaseIterable, Identifiable, Sendable {
    case deepgramNova3 = "deepgram_nova_3"
    case elevenLabsScribeV2 = "eleven_labs_scribe_v2"

    var id: String { rawValue }

    /// The credential-store account, which is not the name the commands use.
    var accountId: String {
        switch self {
        case .deepgramNova3: "deepgram_nova3"
        case .elevenLabsScribeV2: "elevenlabs_scribe_v2"
        }
    }

    var label: String {
        switch self {
        case .deepgramNova3: "Deepgram Nova-3"
        case .elevenLabsScribeV2: "ElevenLabs Scribe v2"
        }
    }
}

enum CloudSttConsent {
    /// The core checks this before it lets audio leave the device.
    static let version = 1
}

/// One cloud route's stored acknowledgements and key state.
struct CloudSttProviderSettings: Decodable, Equatable, Sendable {
    /// The wire name, kept raw: a route this build does not know about must not
    /// fail the whole settings read.
    let provider: String
    let consentVersion: Int?
    let audioTransferConsent: Bool?
    let privacyConsent: Bool?
    let localFallbackConsent: Bool?
    let secretState: SecretState?

    var known: CloudSttProvider? { CloudSttProvider(rawValue: provider) }

    /// Every acknowledgement, at the version this build was written against.
    var hasCurrentConsent: Bool {
        consentVersion == CloudSttConsent.version
            && audioTransferConsent == true
            && privacyConsent == true
            && localFallbackConsent == true
    }
}

// MARK: - The settings this screen reads

/// The slice of the core's settings this screen shows. Every field is optional:
/// the core owns the shape, and a field this build does not know about, or one
/// it has not written yet, must not blank the screen.
struct ProvidersSettings: Decodable, Equatable, Sendable {
    let postProcessEnabled: Bool?
    let postProcessProviderId: String?
    let postProcessProviders: [Provider]?
    let postProcessSecretStates: [String: SecretState]?
    let postProcessProviderConsents: [String: ProviderConsent]?
    let postProcessModels: [String: String]?
    let postProcessPrompts: [PostProcessPrompt]?
    let postProcessSelectedPromptId: String?
    let cloudSttProviders: [CloudSttProviderSettings]?

    var enabled: Bool { postProcessEnabled ?? false }
    var providers: [Provider] { postProcessProviders ?? [] }
    var selectedProviderId: String { postProcessProviderId ?? ProviderCatalog.fallbackId }
    var selectedProvider: Provider? { providers.first { $0.id == selectedProviderId } }
    var prompts: [PostProcessPrompt] { postProcessPrompts ?? [] }
    var cloudProviders: [CloudSttProviderSettings] { cloudSttProviders ?? [] }

    func secretState(for providerId: String) -> SecretState? {
        postProcessSecretStates?[providerId]
    }

    func consent(for providerId: String) -> ProviderConsent? {
        postProcessProviderConsents?[providerId]
    }

    func model(for providerId: String) -> String {
        postProcessModels?[providerId] ?? ""
    }

    func cloud(_ provider: CloudSttProvider) -> CloudSttProviderSettings? {
        cloudProviders.first { $0.provider == provider.rawValue }
    }

    func hasCurrentCloudConsent(_ provider: CloudSttProvider) -> Bool {
        cloud(provider)?.hasCurrentConsent ?? false
    }
}
