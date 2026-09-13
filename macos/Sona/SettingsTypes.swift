import Foundation

// MARK: - Events this slice listens to

extension CoreEvent {
    /// Any settings write, from anywhere: this window, the tray, an agent
    /// proposal. The payload names one setting, but the shell re-reads the
    /// whole record rather than trust one field of it.
    static let settingsChanged = "settings-changed"
}

// MARK: - The settings record

/// One shortcut as the settings file stores it. `ShortcutBinding` in the
/// shared types carries only the chord; a settings row needs the name it
/// shows and the default it resets to.
struct BindingRecord: Decodable, Hashable, Identifiable {
    let id: String
    let name: String
    let description: String
    let defaultBinding: String
    let currentBinding: String

    var changed: Bool { currentBinding != defaultBinding }
}

/// The core's answer to `change_keyboard_implementation_setting`: switching
/// implementations drops chords the new one cannot express.
struct KeyboardChange: Decodable {
    let success: Bool
    let resetBindings: [String]
}

/// Which sound set the start and stop cues come from.
enum SoundTheme: String, CaseIterable, Hashable, Sendable {
    case marimba, pop, custom

    var label: String {
        switch self {
        case .marimba: "Marimba"
        case .pop: "Pop"
        case .custom: "Custom"
        }
    }
}

/// How finished English is spelled, whatever the speaker's accent.
enum DictationSpelling: String, CaseIterable, Hashable, Sendable {
    case asSpoken = "as_spoken"
    case british

    var label: String {
        switch self {
        case .asSpoken: "As spoken"
        case .british: "British"
        }
    }
}

/// Which screen edge the recording overlay and the idle pill sit on.
enum OverlayPosition: String, CaseIterable, Hashable, Sendable {
    case top, bottom

    var label: String {
        switch self {
        case .top: "Top"
        case .bottom: "Bottom"
        }
    }
}

/// How much the recording overlay shows.
enum OverlayStyle: String, CaseIterable, Hashable, Sendable {
    case none = "none"
    case minimal
    case live

    var label: String {
        switch self {
        case .none: "None"
        case .minimal: "Minimal"
        case .live: "Live text"
        }
    }

    var detail: String {
        switch self {
        case .none: "Nothing appears while you dictate."
        case .minimal: "A small indicator while you dictate."
        case .live: "The words as the model hears them."
        }
    }
}

/// How long a loaded model stays in memory after the last dictation.
enum DictationUnloadTimeout: String, CaseIterable, Hashable, Sendable {
    case never
    case immediately
    case sec15
    case min2
    case min5
    case min10
    case min15
    case hour1

    var label: String {
        switch self {
        case .never: "Never"
        case .immediately: "Immediately"
        case .sec15: "After 15 seconds (debug)"
        case .min2: "After 2 minutes"
        case .min5: "After 5 minutes"
        case .min10: "After 10 minutes"
        case .min15: "After 15 minutes"
        case .hour1: "After 1 hour"
        }
    }
}

/// Which keyboard layer registers the chords.
enum KeyboardImplementation: String, CaseIterable, Hashable, Sendable {
    case tauri
    case handyKeys = "handy_keys"

    /// The core's own names for the two layers, not prose: a bug report has
    /// to be able to quote which one was in force.
    var label: String {
        switch self {
        case .tauri: "Tauri Global Shortcut"
        case .handyKeys: "Native key listener"
        }
    }
}

/// Where transcribe.cpp runs.
enum AcceleratorTranscribe: String, CaseIterable, Hashable, Sendable {
    case auto, cpu, gpu

    var label: String {
        switch self {
        case .auto: "Automatic"
        case .cpu: "CPU"
        case .gpu: "GPU"
        }
    }
}

/// Which ONNX execution provider the smaller models use.
enum AcceleratorOrt: String, CaseIterable, Hashable, Sendable {
    case auto, cpu, cuda, directml, rocm

    var label: String {
        switch self {
        case .auto: "Automatic"
        case .cpu: "CPU"
        case .cuda: "CUDA"
        case .directml: "DirectML"
        case .rocm: "ROCm"
        }
    }
}

/// One GPU the core found, as the accelerator picker names it.
struct AcceleratorDevice: Decodable, Hashable, Identifiable {
    let id: String
    let name: String
    let totalVramMb: UInt64?

    /// "Apple M4 Pro (16.0 GB)", as the picker spells it.
    var label: String {
        guard let totalVramMb else { return name }
        let vram = totalVramMb >= 1024
            ? String(format: "%.1f GB", Double(totalVramMb) / 1024)
            : "\(totalVramMb) MB"
        return "\(name) (\(vram))"
    }
}

/// Everything `get_available_accelerators` reports.
struct AcceleratorOptions: Decodable {
    let transcribe: [String]
    let ort: [String]
    let gpuDevices: [AcceleratorDevice]

    static let empty = AcceleratorOptions(transcribe: [], ort: [], gpuDevices: [])
}

/// One row of the transcribe.cpp menu: a backend, or one GPU of it.
struct AcceleratorChoice: Hashable {
    let accelerator: AcceleratorTranscribe
    /// The `transcribe_gpu_device` this row selects. `nil` leaves the core
    /// to pick, which is what every non-GPU row does.
    let device: String?
    let label: String
}

/// How keystrokes are delivered on Linux. macOS always uses `auto`, so the
/// row only appears when the core reports more than one tool.
enum TypingTool: String, CaseIterable, Hashable, Sendable {
    case auto, wtype, kwtype, dotool, ydotool, xdotool

    var label: String { self == .auto ? "Automatic" : rawValue }
}

/// One input or output device the core can use.
struct AudioDevice: Decodable, Hashable, Identifiable {
    let index: String
    let name: String
    let isDefault: Bool

    var id: String { index }
}

/// Which custom sound files exist on disk, for the custom theme.
struct SoundCustomFiles: Decodable {
    let start: Bool
    let stop: Bool

    static let none = SoundCustomFiles(start: false, stop: false)
}

/// Only the parts of a model record this slice asks about: which languages
/// it recognizes, and whether it can detect or translate.
struct SettingsModelCapability: Decodable {
    let id: String
    let supportedLanguages: [String]?
    let supportsLanguageDetection: Bool?
    let supportsTranslation: Bool?
}

/// The settings file, as the fields this shell reads them. Every field is
/// optional on the wire, so each one decodes to the value the core itself
/// defaults to rather than failing the whole record: a settings file
/// written by a newer build must still open here, and a row must never
/// claim "off" for something that ships on.
struct AppSettings: Decodable {
    var bindings: [String: BindingRecord] = [:]
    var pushToTalk = true
    var audioFeedback = false
    var audioFeedbackVolume: Double = 1
    var soundTheme: SoundTheme = .marimba
    var startHidden = false
    var autostartEnabled = false
    var selectedModel = ""
    var alwaysOnMicrophone = false
    var selectedMicrophone: String?
    var selectedChannel: Int?
    var clamshellMicrophone: String?
    var selectedOutputDevice: String?
    var translateToEnglish = false
    var selectedLanguage = "auto"
    var englishSpelling: DictationSpelling = .asSpoken
    var overlayPosition: OverlayPosition = .bottom
    var overlayStyle: OverlayStyle = .live
    var modelUnloadTimeout: DictationUnloadTimeout = .min5
    var muteWhileRecording = false
    var appendTrailingSpace = false
    var experimentalEnabled = false
    var lazyStreamClose = false
    var keyboardImplementation: KeyboardImplementation = .handyKeys
    var showTrayIcon = true
    var typingTool: TypingTool = .auto
    var externalScriptPath: String?
    var fillerWordRemovalEnabled = true
    var vadEnabled = true
    var commandModeEnabled = true
    var transcribeAccelerator: AcceleratorTranscribe = .auto
    var ortAccelerator: AcceleratorOrt = .auto
    var transcribeGpuDevice: String?
    var hudPillEnabled = false
    var hudPillPosition: OverlayPosition = .bottom
    /// Written on the debug page, read here: it is what puts the fifteen
    /// second unload on the timeout menu.
    var debugMode = false

    private enum Key: String, CodingKey {
        case bindings, pushToTalk, audioFeedback, audioFeedbackVolume, soundTheme
        case startHidden, autostartEnabled, selectedModel, alwaysOnMicrophone
        case selectedMicrophone, selectedChannel, clamshellMicrophone, selectedOutputDevice
        case translateToEnglish, selectedLanguage, englishSpelling, overlayPosition
        case overlayStyle, modelUnloadTimeout, muteWhileRecording, appendTrailingSpace
        case experimentalEnabled, lazyStreamClose, keyboardImplementation, showTrayIcon
        case typingTool, externalScriptPath, fillerWordRemovalEnabled, vadEnabled
        case commandModeEnabled, transcribeAccelerator, ortAccelerator, transcribeGpuDevice
        case hudPillEnabled, hudPillPosition, debugMode
    }

    init() {}

    init(from decoder: Decoder) throws {
        let box = try decoder.container(keyedBy: Key.self)
        /// A field the core never wrote, or wrote as a value this build has
        /// never seen, keeps the default rather than failing the record.
        func value<T: Decodable>(_ key: Key, _ fallback: T) -> T {
            (try? box.decodeIfPresent(T.self, forKey: key)) ?? fallback
        }
        func choice<T: RawRepresentable>(_ key: Key, _ fallback: T) -> T where T.RawValue == String {
            guard let raw = try? box.decodeIfPresent(String.self, forKey: key),
                  let known = T(rawValue: raw) else { return fallback }
            return known
        }
        bindings = value(.bindings, bindings)
        pushToTalk = value(.pushToTalk, pushToTalk)
        audioFeedback = value(.audioFeedback, audioFeedback)
        audioFeedbackVolume = value(.audioFeedbackVolume, audioFeedbackVolume)
        soundTheme = choice(.soundTheme, soundTheme)
        startHidden = value(.startHidden, startHidden)
        autostartEnabled = value(.autostartEnabled, autostartEnabled)
        selectedModel = value(.selectedModel, selectedModel)
        alwaysOnMicrophone = value(.alwaysOnMicrophone, alwaysOnMicrophone)
        selectedMicrophone = try? box.decodeIfPresent(String.self, forKey: .selectedMicrophone)
        selectedChannel = try? box.decodeIfPresent(Int.self, forKey: .selectedChannel)
        clamshellMicrophone = try? box.decodeIfPresent(String.self, forKey: .clamshellMicrophone)
        selectedOutputDevice = try? box.decodeIfPresent(String.self, forKey: .selectedOutputDevice)
        translateToEnglish = value(.translateToEnglish, translateToEnglish)
        selectedLanguage = value(.selectedLanguage, selectedLanguage)
        englishSpelling = choice(.englishSpelling, englishSpelling)
        overlayPosition = choice(.overlayPosition, overlayPosition)
        overlayStyle = choice(.overlayStyle, overlayStyle)
        modelUnloadTimeout = choice(.modelUnloadTimeout, modelUnloadTimeout)
        muteWhileRecording = value(.muteWhileRecording, muteWhileRecording)
        appendTrailingSpace = value(.appendTrailingSpace, appendTrailingSpace)
        experimentalEnabled = value(.experimentalEnabled, experimentalEnabled)
        lazyStreamClose = value(.lazyStreamClose, lazyStreamClose)
        keyboardImplementation = choice(.keyboardImplementation, keyboardImplementation)
        showTrayIcon = value(.showTrayIcon, showTrayIcon)
        typingTool = choice(.typingTool, typingTool)
        externalScriptPath = try? box.decodeIfPresent(String.self, forKey: .externalScriptPath)
        fillerWordRemovalEnabled = value(.fillerWordRemovalEnabled, fillerWordRemovalEnabled)
        vadEnabled = value(.vadEnabled, vadEnabled)
        commandModeEnabled = value(.commandModeEnabled, commandModeEnabled)
        transcribeAccelerator = choice(.transcribeAccelerator, transcribeAccelerator)
        ortAccelerator = choice(.ortAccelerator, ortAccelerator)
        transcribeGpuDevice = try? box.decodeIfPresent(String.self, forKey: .transcribeGpuDevice)
        hudPillEnabled = value(.hudPillEnabled, hudPillEnabled)
        hudPillPosition = choice(.hudPillPosition, hudPillPosition)
        debugMode = value(.debugMode, debugMode)
    }

    /// The binding rows in a stable order: the ones the settings pages name
    /// first, then anything else the core carries, alphabetically.
    var orderedBindings: [BindingRecord] {
        let front = ["transcribe", "cancel", "command", "transcribe_with_post_process"]
        return bindings.values.sorted { left, right in
            let leftRank = front.firstIndex(of: left.id) ?? front.count
            let rightRank = front.firstIndex(of: right.id) ?? front.count
            if leftRank != rightRank { return leftRank < rightRank }
            return left.id < right.id
        }
    }
}

// MARK: - Languages

/// One row of the language list.
struct LanguageOption: Hashable, Identifiable {
    let code: String
    let name: String

    var id: String { code }
}

/// The recognition languages, and the matching the picker does against a
/// model's own list. Mirrors `src/lib/constants/languages.ts`.
enum LanguageCatalog {
    static let chineseCode = "zh"

    static let all: [LanguageOption] = [
        ("auto", "Auto detect"), ("en", "English"), ("zh", "Chinese"),
        ("zh-Hans", "Chinese (Simplified)"), ("zh-Hant", "Chinese (Traditional)"),
        ("yue", "Cantonese"), ("de", "German"), ("es", "Spanish"), ("ru", "Russian"),
        ("ko", "Korean"), ("fr", "French"), ("ja", "Japanese"), ("pt", "Portuguese"),
        ("tr", "Turkish"), ("pl", "Polish"), ("ca", "Catalan"), ("nl", "Dutch"),
        ("ar", "Arabic"), ("sv", "Swedish"), ("it", "Italian"), ("id", "Indonesian"),
        ("hi", "Hindi"), ("fi", "Finnish"), ("vi", "Vietnamese"), ("he", "Hebrew"),
        ("uk", "Ukrainian"), ("el", "Greek"), ("ms", "Malay"), ("cs", "Czech"),
        ("ro", "Romanian"), ("da", "Danish"), ("hu", "Hungarian"), ("ta", "Tamil"),
        ("no", "Norwegian"), ("th", "Thai"), ("ur", "Urdu"), ("hr", "Croatian"),
        ("bg", "Bulgarian"), ("lt", "Lithuanian"), ("la", "Latin"), ("mi", "Maori"),
        ("ml", "Malayalam"), ("cy", "Welsh"), ("sk", "Slovak"), ("te", "Telugu"),
        ("fa", "Persian"), ("lv", "Latvian"), ("bn", "Bengali"), ("sr", "Serbian"),
        ("az", "Azerbaijani"), ("sl", "Slovenian"), ("kn", "Kannada"), ("et", "Estonian"),
        ("mk", "Macedonian"), ("br", "Breton"), ("eu", "Basque"), ("is", "Icelandic"),
        ("hy", "Armenian"), ("ne", "Nepali"), ("mn", "Mongolian"), ("bs", "Bosnian"),
        ("kk", "Kazakh"), ("sq", "Albanian"), ("sw", "Swahili"), ("gl", "Galician"),
        ("mr", "Marathi"), ("pa", "Punjabi"), ("si", "Sinhala"), ("km", "Khmer"),
        ("sn", "Shona"), ("yo", "Yoruba"), ("so", "Somali"), ("af", "Afrikaans"),
        ("oc", "Occitan"), ("ka", "Georgian"), ("be", "Belarusian"), ("tg", "Tajik"),
        ("sd", "Sindhi"), ("gu", "Gujarati"), ("am", "Amharic"), ("yi", "Yiddish"),
        ("lo", "Lao"), ("uz", "Uzbek"), ("fo", "Faroese"), ("ht", "Haitian Creole"),
        ("ps", "Pashto"), ("tk", "Turkmen"), ("nn", "Nynorsk"), ("mt", "Maltese"),
        ("sa", "Sanskrit"), ("lb", "Luxembourgish"), ("my", "Myanmar"), ("bo", "Tibetan"),
        ("tl", "Tagalog"), ("mg", "Malagasy"), ("as", "Assamese"), ("tt", "Tatar"),
        ("haw", "Hawaiian"), ("ln", "Lingala"), ("ha", "Hausa"), ("ba", "Bashkir"),
        ("jw", "Javanese"), ("su", "Sundanese"),
    ].map(LanguageOption.init(code:name:))

    /// The bare `zh` is not offered: the two script variants recognize the
    /// same speech and only they say which script you get back. It stays in
    /// `all` because auto-detect can still resolve to it.
    static let selectable: [LanguageOption] = all.filter { $0.code != chineseCode }

    private static let names: [String: String] = Dictionary(
        all.map { ($0.code, $0.name) }, uniquingKeysWith: { first, _ in first }
    )

    /// The name a picker shows. A code this build has no name for shows as
    /// the code: the model reported it, so it is real.
    static func name(_ code: String) -> String { names[code] ?? code }

    /// The base code Sona matches on: "en-US" and "zh-Hant" both collapse.
    static func base(_ code: String) -> String {
        guard let dash = code.firstIndex(of: "-") else { return code }
        return String(code[code.startIndex ..< dash])
    }

    static func supports(_ supported: [String], _ code: String) -> Bool {
        let wanted = base(code)
        return supported.contains { base($0) == wanted }
    }

    /// The language actually in force, resolved the way
    /// `effective_language` in the core resolves it.
    static func effective(intent: String, supported: [String], detects: Bool) -> String {
        if supported.isEmpty { return intent }
        if intent != "auto", supports(supported, intent) { return intent }
        if detects { return "auto" }
        if supports(supported, "en") { return "en" }
        return base(supported[0])
    }

    /// The languages worth offering for a model: everything it recognizes,
    /// with auto only when it can detect.
    static func available(supported: [String], detects: Bool) -> [LanguageOption] {
        guard !supported.isEmpty else { return selectable }
        return selectable.filter { $0.code == "auto" ? detects : supports(supported, $0.code) }
    }
}

// MARK: - Copy

/// The names the settings pages give shortcuts. The core carries a name for
/// every binding; these are the four the pages title differently.
enum BindingCopy {
    /// The four the pages name themselves. Anything else the core adds
    /// arrives with its own name and description, and shows both.
    static func title(_ record: BindingRecord) -> String {
        switch record.id {
        case "transcribe": "Transcribe shortcut"
        case "cancel": "Cancel shortcut"
        case "command": "Command shortcut"
        case "transcribe_with_post_process": "Post-processing shortcut"
        default: record.name
        }
    }

    /// Only the transcribe row carries a hint: that a shortcut can be held
    /// as well as tapped is the one thing about it nothing else shows.
    static func detail(_ record: BindingRecord) -> String? {
        switch record.id {
        case "transcribe": "Tap to toggle, hold to talk. Works with any shortcut."
        case "cancel", "command", "transcribe_with_post_process": nil
        default: record.description.isEmpty ? nil : record.description
        }
    }

    /// A chord as the key caps show it. The core qualifies a modifier with
    /// the side it was pressed on; both sides carry the same engraving, so
    /// the qualifier is dropped rather than printed on the cap.
    static func chord(_ binding: String) -> String {
        binding
            .split(separator: "+")
            .map { part -> String in
                let key = part.trimmingCharacters(in: .whitespaces)
                for side in ["_left", "_right"] where key.hasSuffix(side) {
                    return String(key.dropLast(side.count))
                }
                return key
            }
            .joined(separator: "+")
    }
}
