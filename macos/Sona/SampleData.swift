import Foundation

/// What the microphone is doing. `working` names what the core is doing to
/// the words after the microphone closed: "transcribing" or "polishing".
enum CaptureState: Equatable {
    case idle
    case recording(since: Date)
    case working(String)
}

/// One dictation from the core's history.
struct Transcription: Identifiable, Hashable {
    let id: Int64
    let date: Date
    let title: String
    /// The words after the mode shaped them, or as heard when no mode ran.
    let text: String
    /// The words as heard.
    let rawText: String
    var saved: Bool

    init(_ entry: HistoryEntry) {
        id = entry.id
        date = Date(timeIntervalSince1970: TimeInterval(entry.timestamp))
        title = entry.title
        text = entry.postProcessedText ?? entry.transcriptionText
        rawText = entry.transcriptionText
        saved = entry.saved
    }

    var words: Int { text.split(separator: " ").count }
}

struct ActionItem: Identifiable, Hashable {
    let id: Int
    let owner: String
    let text: String
    let due: String
}

struct Meeting: Identifiable, Hashable {
    let id: Int
    let title: String
    let date: Date
    let duration: TimeInterval
    let app: String
    let people: [String]
    let summary: String
    let decisions: [String]
    let actions: [ActionItem]
    let loops: [String]
}

struct Person: Identifiable, Hashable {
    let id: Int
    let name: String
    let organization: String
    let role: String
    let lastMet: Date
    let meetings: Int
    let summary: String
    let context: [String]
}

/// One speech model the core knows about, on disk or in its catalog.
struct Model: Identifiable, Hashable {
    enum Status: Hashable {
        case active
        case downloaded
        case downloading(fraction: Double, downloaded: String)
        case available
    }

    let id: String
    let name: String
    /// What the catalog says, then the size: "Fast, accurate live preview · 620 MB".
    let meta: String
    var status: Status

    init(_ info: ModelInfo, current: String) {
        id = info.id
        name = info.name
        let size = Self.bytes(info.sizeMb * 1_000_000)
        meta = info.description.isEmpty ? size : "\(info.description) · \(size)"
        if info.isDownloading {
            let fraction = info.sizeMb == 0 ? 0 : Double(info.partialSize) / Double(info.sizeMb * 1_000_000)
            status = .downloading(fraction: min(fraction, 1), downloaded: "\(Self.bytes(info.partialSize)) of \(size)")
        } else if info.isDownloaded {
            status = info.id == current ? .active : .downloaded
        } else {
            status = .available
        }
    }

    static let byteFormatter: ByteCountFormatter = {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        return formatter
    }()

    static func bytes(_ count: UInt64) -> String {
        byteFormatter.string(fromByteCount: Int64(count))
    }
}

struct Provider: Identifiable, Hashable {
    let id: Int
    let name: String
    let detail: String
    let connected: Bool
}

struct Mode: Identifiable, Hashable {
    let id: Int
    let name: String
    let description: String
    let apps: String
    let shortcut: String
    let prompt: String
    let active: Bool
}

/// A decision Sona could not make alone.
struct Decision: Identifiable, Hashable {
    enum Kind: Hashable {
        case link
        case loops
        case vocabulary
        case recovery
    }

    let id: Int
    let kind: Kind
    let text: String
    let accept: String
    let decline: String
}

struct SavedPrompt: Identifiable, Hashable {
    let id: Int
    let name: String
    let text: String
    let runs: Int
}

struct Workflow: Identifiable, Hashable {
    let id: Int
    let name: String
    let trigger: String
    let steps: Int
    let enabled: Bool
}

struct Agent: Identifiable, Hashable {
    let id: Int
    let name: String
    let transport: String
    let lastSeen: String
}

struct VocabularyEntry: Identifiable, Hashable {
    let id: Int
    let word: String
    let heardAs: String
    let uses: Int
}

struct PaletteCommand: Identifiable, Hashable {
    let id: Int
    let title: String
    let detail: String
    let shortcut: String
}

struct ChatTurn: Identifiable, Hashable {
    let id: Int
    let fromUser: Bool
    let text: String
}

/// Placeholder collections for screens the core does not carry over the socket yet.
/// User-content collections stay empty so a clean install never invents local data.
enum SampleData {

    static let meetings: [Meeting] = []

    static let people: [Person] = []

    static let providers: [Provider] = [
        Provider(id: 1, name: "Apple Intelligence", detail: "On this Mac. Used for summaries and modes.", connected: true),
        Provider(id: 2, name: "Local endpoint", detail: "http://127.0.0.1:11434 · llama 3.3 70b", connected: true),
        Provider(id: 3, name: "Anthropic", detail: "Key in the keychain. Off unless a mode asks for it.", connected: false),
    ]

    static let modes: [Mode] = [
        Mode(id: 1, name: "Note", description: "Clean punctuation, keep your words.", apps: "Everywhere", shortcut: "⌥ Space",
             prompt: "Fix punctuation and casing. Remove filler. Do not change words.", active: true),
        Mode(id: 2, name: "Message", description: "Short, lower pressure, no sign-off.", apps: "Slack, Messages", shortcut: "⌥ M",
             prompt: "Rewrite as a short chat message. No greeting, no sign-off.", active: false),
        Mode(id: 3, name: "Email", description: "Greeting, paragraphs, a close.", apps: "Mail", shortcut: "⌥ E",
             prompt: "Rewrite as an email with a greeting, short paragraphs and a close.", active: false),
        Mode(id: 4, name: "Code", description: "Comments and commit messages.", apps: "Xcode, Zed, Terminal", shortcut: "⌥ C",
             prompt: "Rewrite as a code comment or commit message. Imperative, under 72 columns.", active: false),
        Mode(id: 5, name: "Verbatim", description: "Exactly what you said.", apps: "Everywhere", shortcut: "⌥ V",
             prompt: "", active: false),
        Mode(id: 6, name: "Meeting", description: "Runs on its own while a call is on.", apps: "Zoom, Meet, FaceTime", shortcut: "Automatic",
             prompt: "Summarise, list decisions, list action items with owners.", active: false),
    ]

    static let decisions: [Decision] = []

    static let prompts: [SavedPrompt] = []

    static let workflows: [Workflow] = []

    static let agents: [Agent] = []

    static let vocabulary: [VocabularyEntry] = []

    static let commands: [PaletteCommand] = [
        PaletteCommand(id: 1, title: "Start recording", detail: "Note mode", shortcut: "⌥ Space"),
        PaletteCommand(id: 2, title: "New meeting", detail: "Record the current call", shortcut: "⌥ ⇧ M"),
        PaletteCommand(id: 5, title: "Switch mode to Email", detail: "Mode", shortcut: "⌥ E"),
        PaletteCommand(id: 6, title: "Open settings", detail: "", shortcut: "⌘ ,"),
    ]

    static let chat: [ChatTurn] = []
}

extension TimeInterval {
    /// "3:12" or "1:04:07": a clock that fits in a row.
    var clock: String {
        let total = Int(self)
        let hours = total / 3600
        let minutes = (total % 3600) / 60
        let seconds = total % 60
        return hours > 0
            ? String(format: "%d:%02d:%02d", hours, minutes, seconds)
            : String(format: "%d:%02d", minutes, seconds)
    }

    /// "47 min" or "1 h 12 m": a length in words.
    var spoken: String {
        let minutes = Int(self) / 60
        return minutes >= 60 ? "\(minutes / 60) h \(minutes % 60) m" : "\(minutes) min"
    }
}

extension Date {
    static let timeFormat: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm"
        return formatter
    }()

    static let dayFormat: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "EEEE d MMMM"
        return formatter
    }()

    static let shortFormat: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "d MMM"
        return formatter
    }()

    var time: String { Date.timeFormat.string(from: self) }
    var dayName: String { Date.dayFormat.string(from: self) }
    var short: String { Date.shortFormat.string(from: self) }

    /// "Today", "Yesterday", or the day name.
    var relativeDay: String {
        let calendar = Calendar.current
        let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: self), to: calendar.startOfDay(for: .now)).day ?? 0
        switch days {
        case 0: return "Today"
        case 1: return "Yesterday"
        default: return dayName
        }
    }
}
