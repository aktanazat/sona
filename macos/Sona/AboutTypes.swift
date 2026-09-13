import Foundation

/// The shapes behind the About page, the Debug page and the release-note
/// sheet. Wire types mirror the Rust structs with snake_case turned into
/// camelCase by `Core.decoder`; enum raw values are the exact wire strings.

// MARK: - Event names

extension CoreEvent {
    /// Carries no useful payload: every observer re-reads `get_app_settings`.
    static let aboutSettingsChanged = "settings-changed"
    /// Emitted when the core applies a theme, including a system change.
    static let aboutThemeChanged = "theme-changed"
}

// MARK: - Updates

/// `UpdateCheckStatus` (src-tauri/src/commands/updates.rs). A failed check is
/// a status, not a command error, so the surface can still print the version.
enum UpdateCheckStatus: String, Decodable {
    case upToDate = "up_to_date"
    case updateAvailable = "update_available"
    case checkFailed = "check_failed"
    /// Checks are off, so nothing was requested from GitHub.
    case disabled
}

/// The result of one manual update check.
struct UpdateCheckResult: Decodable {
    let currentVersion: String
    let latestVersion: String?
    let updateAvailable: Bool
    let url: String?
    let notesExcerpt: String?
    let publishedAtUtcMs: Int64?
    let status: UpdateCheckStatus
    let error: String?
}

// MARK: - Appearance

/// `Theme`: which appearance Sona asks for.
enum AppearanceTheme: String, CaseIterable, Hashable, Sendable {
    case system, light, dark

    var label: String {
        switch self {
        case .system: "System"
        case .light: "Light"
        case .dark: "Dark"
        }
    }
}

/// `AppearanceMaterial`: whether Sona's own surfaces are opaque.
enum AppearanceMaterial: String, CaseIterable, Hashable, Sendable {
    case solid, glass

    var label: String {
        switch self {
        case .solid: "Solid"
        case .glass: "Glass"
        }
    }
}

// MARK: - App language

/// One locale Sona ships strings for. The order is the priority order of
/// `src/i18n/languages.ts`, which is the order the picker shows.
struct AppLanguage: Identifiable, Hashable, Sendable {
    let code: String
    let name: String
    let nativeName: String

    var id: String { code }
    /// "简体中文 (Simplified Chinese)", as the old picker printed it.
    var label: String { "\(nativeName) (\(name))" }

    static let all: [AppLanguage] = [
        AppLanguage(code: "en", name: "English", nativeName: "English"),
        AppLanguage(code: "zh", name: "Simplified Chinese", nativeName: "简体中文"),
        AppLanguage(code: "zh-TW", name: "Traditional Chinese", nativeName: "繁體中文"),
        AppLanguage(code: "es", name: "Spanish", nativeName: "Español"),
        AppLanguage(code: "fr", name: "French", nativeName: "Français"),
        AppLanguage(code: "de", name: "German", nativeName: "Deutsch"),
        AppLanguage(code: "ja", name: "Japanese", nativeName: "日本語"),
        AppLanguage(code: "ko", name: "Korean", nativeName: "한국어"),
        AppLanguage(code: "vi", name: "Vietnamese", nativeName: "Tiếng Việt"),
        AppLanguage(code: "pl", name: "Polish", nativeName: "Polski"),
        AppLanguage(code: "it", name: "Italian", nativeName: "Italiano"),
        AppLanguage(code: "ru", name: "Russian", nativeName: "Русский"),
        AppLanguage(code: "uk", name: "Ukrainian", nativeName: "Українська"),
        AppLanguage(code: "pt", name: "Portuguese", nativeName: "Português"),
        AppLanguage(code: "cs", name: "Czech", nativeName: "Čeština"),
        AppLanguage(code: "tr", name: "Turkish", nativeName: "Türkçe"),
        AppLanguage(code: "ar", name: "Arabic", nativeName: "العربية"),
        AppLanguage(code: "he", name: "Hebrew", nativeName: "עברית"),
        AppLanguage(code: "sv", name: "Swedish", nativeName: "Svenska"),
        AppLanguage(code: "bg", name: "Bulgarian", nativeName: "Български"),
        AppLanguage(code: "nl", name: "Dutch", nativeName: "Nederlands"),
        AppLanguage(code: "ne", name: "Nepali", nativeName: "नेपाली"),
        AppLanguage(code: "hi", name: "Hindi", nativeName: "हिन्दी"),
        AppLanguage(code: "da", name: "Danish", nativeName: "Dansk"),
    ]

    /// The shipped locale a stored code means, the way `getSupportedLanguage`
    /// resolved it: an exact match first, then the bare language, with
    /// Traditional Chinese recovered from a script or region subtag and
    /// Cantonese reading as Chinese.
    static func supported(_ code: String?) -> AppLanguage? {
        guard let code, !code.isEmpty else { return nil }
        let normalized = code.lowercased().replacingOccurrences(of: "_", with: "-")
        let subtags = normalized.split(separator: "-").map(String.init)
        let language = subtags.first ?? normalized
        if let exact = all.first(where: { $0.code.lowercased() == normalized }) {
            return exact
        }
        let isHant = subtags.contains("hant")
        let isHans = subtags.contains("hans")
        let traditionalRegion = subtags.contains { ["tw", "hk", "mo"].contains($0) }
        var fallback = language
        if language == "zh", isHant || (!isHans && traditionalRegion) {
            fallback = "zh-tw"
        } else if language == "yue" {
            fallback = isHans ? "zh" : "zh-tw"
        }
        return all.first { $0.code.lowercased() == fallback }
    }
}

// MARK: - Logging

/// `LogLevel`: the file log's verbosity, and the tags printed in a log line.
/// The names are the log format's own tokens, so they are not prose.
enum LogLevel: String, CaseIterable, Hashable, Sendable {
    case trace, debug, info, warn, error

    /// "Error", "Warn": the label beside a line and in the picker.
    var label: String {
        switch self {
        case .trace: "Trace"
        case .debug: "Debug"
        case .info: "Info"
        case .warn: "Warn"
        case .error: "Error"
        }
    }

    /// Rising order, so a filter can keep everything at or above a level.
    var severity: Int {
        switch self {
        case .trace: 0
        case .debug: 1
        case .info: 2
        case .warn: 3
        case .error: 4
        }
    }

    /// The token tauri-plugin-log prints between brackets.
    init?(logTag: String) {
        switch logTag.uppercased() {
        case "TRACE": self = .trace
        case "DEBUG": self = .debug
        case "INFO": self = .info
        case "WARN", "WARNING": self = .warn
        case "ERROR": self = .error
        default: return nil
        }
    }
}

/// One line of `sona.log`, as the live viewer shows it. A line the format
/// does not explain keeps its whole text as the message and no level.
struct LogLine: Identifiable, Hashable, Sendable {
    let id: Int
    let time: String
    let level: LogLevel?
    let target: String?
    let message: String

    /// The fixed-width tag column: a known level, or "Log".
    var tag: String { level?.label ?? "Log" }

    /// A line as the clipboard gets it: "21:35:10 Debug message".
    var transcript: String { "\(time) \(tag) \(message)" }
}

// MARK: - Keyboard diagnostic

/// `KeyboardDiagnosticReport`: counts only, never which keys were pressed.
struct KeyboardDiagnosticReport: Decodable, Sendable {
    let secureInputEnabled: Bool
    let culpritPid: Int?
    let culpritName: String?
    let keyDown: Int
    let keyUp: Int
    let flagsChanged: Int
    let mouse: Int
    let durationMs: Int
}

/// What the counts mean. Blocked and suspicious both say "your keys are not
/// reaching Sona", which is why anyone runs this, so they read as faults; a
/// working keyboard is the unremarkable answer and stays quiet.
struct KeyboardDiagnosticVerdict {
    enum Tone { case muted, warning, danger }

    let text: String
    let tone: Tone

    init(_ report: KeyboardDiagnosticReport) {
        if report.secureInputEnabled, report.keyDown == 0 {
            text = "Secure Input is on and no key reached Sona. Close the app holding it, then try again."
            tone = .danger
        } else if !report.secureInputEnabled, report.keyDown == 0, report.flagsChanged > 0 {
            text = "Modifier keys arrived but no key presses did. Another app is likely swallowing them."
            tone = .danger
        } else if report.keyDown == 0, report.flagsChanged == 0, report.mouse == 0 {
            text = "Nothing arrived at all. Press a few keys while the diagnostic runs."
            tone = .warning
        } else {
            text = "Keys reach Sona."
            tone = .muted
        }
    }

    /// "enabled — held by Safari (pid 412)", or just "disabled".
    static func secureInputLine(_ report: KeyboardDiagnosticReport) -> String {
        guard report.secureInputEnabled else { return "disabled" }
        guard let name = report.culpritName, let pid = report.culpritPid else {
            return "enabled — held by an app macOS did not name"
        }
        return "enabled — held by \(name) (pid \(pid))"
    }
}

// MARK: - Word correction

/// The selected model, as far as the word-correction row needs to know it.
struct WordCorrectionModel: Decodable, Sendable {
    let id: String
    let name: String
}

/// Whether a model's decoder is handed the vocabulary as its prompt, which is
/// what makes a fuzzy pass afterwards a second guess at words the decoder was
/// already told about (`managers/transcription.rs` gates that on the whisper
/// architecture). Read off the id and the name, because the shell never
/// receives the architecture; an unrecognised file reads as false, which
/// leaves the control live rather than dimming it on a guess.
enum WordCorrectionFamily {
    private static let promptedNeedles = ["whisper", "breeze"]
    /// Families tested before the prompted ones, so "Breeze" inside another
    /// product name cannot claim a model that is not a Whisper fine-tune.
    private static let otherNeedles = [
        "moonshine", "parakeet", "nemotron", "canary", "cohere", "voxtral",
        "qwen3-asr", "qwen3_asr", "qwen3", "fun-asr", "funasr", "gigaam",
        "giga-am", "granite", "sensevoice", "sense-voice", "medasr", "med-asr",
        "moss",
    ]

    static func takesVocabularyAsPrompt(id: String, name: String) -> Bool {
        let haystack = "\(id) \(name)".lowercased()
        if otherNeedles.contains(where: haystack.contains) { return false }
        return promptedNeedles.contains(where: haystack.contains)
    }
}

// MARK: - Settings slices

/// The part of `AppSettings` the About page reads. Every field is optional on
/// the wire, and an unknown enum value reads as absent rather than failing the
/// whole decode.
struct AboutSettings: Decodable, Sendable {
    let appLanguage: String?
    let theme: AppearanceTheme?
    let appearanceMaterial: AppearanceMaterial?
    let updateCheckEnabled: Bool?
    let showWhatsNewOnUpdate: Bool?
    let whatsNewLastSeenVersion: String?

    private enum Key: String, CodingKey {
        case appLanguage, theme, appearanceMaterial, updateCheckEnabled
        case showWhatsNewOnUpdate, whatsNewLastSeenVersion
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        appLanguage = try container.decodeIfPresent(String.self, forKey: .appLanguage)
        theme = AppearanceTheme(
            rawValue: try container.decodeIfPresent(String.self, forKey: .theme) ?? ""
        )
        appearanceMaterial = AppearanceMaterial(
            rawValue: try container.decodeIfPresent(String.self, forKey: .appearanceMaterial) ?? ""
        )
        updateCheckEnabled = try container.decodeIfPresent(Bool.self, forKey: .updateCheckEnabled)
        showWhatsNewOnUpdate = try container.decodeIfPresent(Bool.self, forKey: .showWhatsNewOnUpdate)
        whatsNewLastSeenVersion = try container.decodeIfPresent(
            String.self, forKey: .whatsNewLastSeenVersion
        )
    }
}

/// The part of `AppSettings` the Debug page reads.
struct DebugSettingsSnapshot: Decodable, Sendable {
    let debugMode: Bool?
    let logLevel: LogLevel?
    let wordCorrectionThreshold: Double?
    let extraRecordingBufferMs: Int?
    let selectedModel: String?

    private enum Key: String, CodingKey {
        case debugMode, logLevel, wordCorrectionThreshold, extraRecordingBufferMs, selectedModel
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        debugMode = try container.decodeIfPresent(Bool.self, forKey: .debugMode)
        logLevel = LogLevel(
            rawValue: try container.decodeIfPresent(String.self, forKey: .logLevel) ?? ""
        )
        wordCorrectionThreshold = try container.decodeIfPresent(
            Double.self, forKey: .wordCorrectionThreshold
        )
        extraRecordingBufferMs = try container.decodeIfPresent(
            Int.self, forKey: .extraRecordingBufferMs
        )
        selectedModel = try container.decodeIfPresent(String.self, forKey: .selectedModel)
    }
}

// MARK: - Versions

/// A release as three numbers. A key that is not a semver triple is not a
/// version this app compares: a typo drops the entry instead of reaching a
/// reader, exactly as `parseVersion` did.
struct VersionNumber: Comparable, CustomStringConvertible, Sendable {
    let major: Int
    let minor: Int
    let patch: Int

    init?(_ text: String) {
        var digits = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if digits.first == "v" || digits.first == "V" {
            digits.removeFirst()
        }
        // The triple, ignoring any pre-release or build suffix.
        let parts = digits.split(separator: ".", maxSplits: 2, omittingEmptySubsequences: false)
        guard parts.count == 3 else { return nil }
        let leadingNumber = { (field: Substring) -> Int? in
            let value = field.prefix { $0.isNumber }
            return value.isEmpty ? nil : Int(value)
        }
        guard let major = leadingNumber(parts[0]),
              let minor = leadingNumber(parts[1]),
              let patch = leadingNumber(parts[2])
        else { return nil }
        self.major = major
        self.minor = minor
        self.patch = patch
    }

    var description: String { "\(major).\(minor).\(patch)" }

    static func < (lhs: VersionNumber, rhs: VersionNumber) -> Bool {
        (lhs.major, lhs.minor, lhs.patch) < (rhs.major, rhs.minor, rhs.patch)
    }
}

// MARK: - Release notes

/// One bundled release note: the version it describes and its Markdown.
struct ReleaseNotesNote: Identifiable, Hashable, Sendable {
    let version: String
    let markdown: String

    var id: String { version }
}

/// The bundled notes, newest first.
///
/// The React app kept these in `src/content/release-notes/index.ts` as a map
/// of Markdown strings, because its bundler could not import loose `.md`
/// files. They are the same strings here: content that ships with the build,
/// not something the core can be asked for.
enum ReleaseNotesCatalog {
    static let all: [ReleaseNotesNote] = [
        ReleaseNotesNote(version: "1.1.0", markdown: sona110),
        ReleaseNotesNote(version: "1.0.0", markdown: sona100),
    ]
    .filter { VersionNumber($0.version) != nil }
    .sorted { left, right in
        guard let a = VersionNumber(left.version), let b = VersionNumber(right.version) else {
            return false
        }
        return a > b
    }

    /// The newest bundled note, whatever version is running. What the Debug
    /// preview opens.
    static var latest: ReleaseNotesNote? { all.first }

    /// The highest note newer than what was last seen and not newer than the
    /// running build.
    static func noteToShow(currentVersion: String, lastSeenVersion: String) -> ReleaseNotesNote? {
        guard let current = VersionNumber(currentVersion) else { return nil }
        let lastSeen = VersionNumber(lastSeenVersion)
        return all.first { note in
            guard let version = VersionNumber(note.version) else { return false }
            if version > current { return false }
            if let lastSeen, version <= lastSeen { return false }
            return true
        }
    }

    private static let sona110 = """
    # Sona 1.1.0

    Meetings learn to listen sooner and to act. Catch-up and questions answer while a meeting is still running, from a provisional transcript Sona keeps in memory until the real one is written after the stop. FaceTime, Phone, and meeting-app calls can start a recording on their own once you grant each app in Settings, with a card to stop or revoke. A recording or a Granola, Otter, or Circleback export can be imported as a meeting.

    Detection prompts on participation, not presence: a meeting app that is open but untouched since the microphone came on stays silent and says so on the status line. A call in Chrome, Safari, or Arc is read from the tab title on every tick with Accessibility alone, so a Meet you open after switching to the browser is still caught. A declined invitation never prompts, a meeting under way beats the next block on the calendar, and a capture stops only on its own evidence: its own event, its own microphone lane, a sleep that happened during it.

    A finished meeting opens on its ledger: every thread, where it landed, and the verbatim receipt behind each claim, checked against the transcript before Sona shows it. The consent panel, the Prep and Wrap cards, and the recording pill share the app's type and tokens, enter and leave with one short motion, and work from the keyboard.

    Saved prompts ask a question of a meeting, a person, or a series and can return JSON checked against a schema; three editable defaults ship. Follow-ups open in Mail, reminders carry the due day the notes named, the consent panel can announce the recording in the meeting's chat, and a deleted meeting stays restorable for thirty days.

    The chat can now look things up. When you allow it to send matching quotes to your server, Sona searches your recordings, reads a meeting or a transcript, looks up a person, checks open loops and the calendar, and counts words and activity, at most three lookups per question, each shown as a step. The per-meeting Questions tab is gone; ask the chat instead. The chat can also offer changes: close a commitment, give it an owner, set a series template, add a vocabulary term, rename a speaker. Nothing applies without a press, and every change carries a receipt and an undo. The `sona` CLI and MCP server gain `--upcoming` and a consent-gated `--loop-resolve`. People pages gain organizations and a short relationship summary.

    Dictation gains five spoken edits and a "Sona," cue for spoken instructions. Context capture works on Windows and Linux. Codex, Grok, and OMP hook events are typed, and their permission requests can be answered from Sona where the tool allows a reply.

    A companion iPhone and Apple Watch recorder lives in `mobile/`; it pairs through the encrypted vault and hands in-person recordings to the Mac.

    Upgraders keep their existing meeting-app list, so FaceTime and Phone detection is off until you tick them in Settings. The chat relay moves to turn version 2; pair the updated relay with this build.
    """

    private static let sona100 = """
    # Sona 1.0.0

    Sona introduces a new application identity, package name, data directory, log name, and agent-hook sidecar.

    On the first launch, Sona can move settings, history, recordings, models, and configured provider keys from the Legacy app. Close the Legacy app before migration, then remove it using the normal uninstall flow for your platform. Sona needs new Microphone and Accessibility permissions because it is a new application identity.

    Environment variables now use the `SONA_` prefix. The release does not add automatic updates, notarization, or distribution signing.
    """
}
