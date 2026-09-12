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

/// Fixtures for the design pass. Meetings, people, modes, prompts, workflows,
/// agents, vocabulary, the palette and the chat still render from here; the
/// core does not carry them over the socket yet. Dictations, models, stats
/// and the microphone state come from the core.
enum SampleData {
    static let calendar = Calendar.current
    static let now = calendar.date(from: DateComponents(year: 2026, month: 9, day: 12, hour: 14, minute: 20))!

    static func day(_ daysAgo: Int, _ hour: Int, _ minute: Int = 0) -> Date {
        let base = calendar.date(byAdding: .day, value: -daysAgo, to: now)!
        return calendar.date(bySettingHour: hour, minute: minute, second: 0, of: base)!
    }

    static let meetings: [Meeting] = [
        Meeting(id: 1, title: "Retention proposal review", date: day(0, 10, 0), duration: 47 * 60, app: "Zoom",
                people: ["Dana Whitfield", "Priya Raman", "You"],
                summary: "Dana walked through the ninety-day retention default. Priya asked for a per-workspace override; the group agreed to ship the default first and revisit overrides after two customers ask. Nothing blocks Friday.",
                decisions: ["Ninety days is the default retention window.", "Overrides wait for customer demand."],
                actions: [ActionItem(id: 1, owner: "You", text: "Send Dana the two open questions", due: "Fri"),
                          ActionItem(id: 2, owner: "Priya", text: "Draft the consent panel copy", due: "Mon")],
                loops: ["Who owns the migration note for 1.0 users?"]),
        Meeting(id: 2, title: "Standup", date: day(1, 9, 30), duration: 14 * 60, app: "Google Meet",
                people: ["Priya Raman", "Marco Bell", "You"],
                summary: "Short. Marco reproduced the Zoom preview false positive. Priya has the panel copy in review.",
                decisions: [], actions: [ActionItem(id: 3, owner: "Marco", text: "Fix preview-window detection", due: "Wed")],
                loops: ["Should paused meetings show in Recent?", "Rename 'Modes' before launch?", "Watch app battery numbers"]),
        Meeting(id: 3, title: "Alder Systems intro", date: day(2, 15, 0), duration: 31 * 60, app: "Zoom",
                people: ["Tomás Alder", "You"],
                summary: "Tomás runs a forty-person support team and wants transcripts that never leave their machines. Local-first was the whole pitch; he asked twice whether anything phones home.",
                decisions: ["Send the local-first architecture note."],
                actions: [ActionItem(id: 4, owner: "You", text: "Send architecture note", due: "Thu")], loops: []),
        Meeting(id: 4, title: "Design critique", date: day(3, 13, 0), duration: 52 * 60, app: "FaceTime",
                people: ["Lena Okafor", "You"], summary: "Lena cut the sidebar to six items and asked for one action per screen.",
                decisions: ["One headline, one action, per screen."], actions: [], loops: []),
        Meeting(id: 5, title: "1:1 with Marco", date: day(4, 11, 0), duration: 28 * 60, app: "Zoom",
                people: ["Marco Bell", "You"], summary: "Marco wants to own detection end to end.",
                decisions: [], actions: [], loops: []),
        Meeting(id: 6, title: "Whisper large v3 eval", date: day(5, 16, 0), duration: 39 * 60, app: "Google Meet",
                people: ["Priya Raman", "You"], summary: "Word error rate fell from 9.1 to 6.4 on the meeting set.",
                decisions: ["Large v3 becomes the default model."], actions: [], loops: []),
        Meeting(id: 7, title: "Onboarding walkthrough", date: day(5, 10, 0), duration: 22 * 60, app: "Zoom",
                people: ["Dana Whitfield", "You"], summary: "Three permission prompts is two too many.",
                decisions: [], actions: [], loops: []),
        Meeting(id: 8, title: "Pricing", date: day(6, 14, 30), duration: 44 * 60, app: "Google Meet",
                people: ["Lena Okafor", "Tomás Alder", "You"], summary: "Per-seat, annual only, no free tier.",
                decisions: ["Annual only."], actions: [], loops: []),
        Meeting(id: 9, title: "Roadmap", date: day(6, 9, 0), duration: 58 * 60, app: "Zoom",
                people: ["Dana Whitfield", "Priya Raman", "Marco Bell", "You"], summary: "Q4 is the native app.",
                decisions: ["Q4 is the native app."], actions: [], loops: []),
    ]

    static let people: [Person] = [
        Person(id: 1, name: "Dana Whitfield", organization: "Halden", role: "Head of Product", lastMet: day(0, 10), meetings: 11,
               summary: "Dana runs product at Halden. Direct, prepared, sends the doc the night before.",
               context: ["You owe her two questions on retention by Friday.", "She asked about a migration note for 1.0 users; nobody owns it yet.", "Last three meetings ran under time."]),
        Person(id: 2, name: "Priya Raman", organization: "Halden", role: "Design", lastMet: day(1, 9, 30), meetings: 14,
               summary: "Priya owns the consent panel and most of the copy.",
               context: ["Consent panel copy due Monday.", "Prefers Meet over Zoom."]),
        Person(id: 3, name: "Marco Bell", organization: "Halden", role: "Engineer", lastMet: day(1, 9, 30), meetings: 9,
               summary: "Marco owns meeting detection.",
               context: ["Fixing the Zoom preview false positive by Wednesday."]),
        Person(id: 4, name: "Tomás Alder", organization: "Alder Systems", role: "Founder", lastMet: day(2, 15), meetings: 2,
               summary: "Runs a forty-person support team. Cares about data never leaving the machine.",
               context: ["Send the local-first architecture note.", "Second meeting; first was pricing."]),
        Person(id: 5, name: "Lena Okafor", organization: "Independent", role: "Design advisor", lastMet: day(3, 13), meetings: 5,
               summary: "Lena critiques the app every other week.",
               context: ["Asked for one action per screen."]),
        Person(id: 6, name: "Sam Ortiz", organization: "Northwind", role: "Support lead", lastMet: day(12, 15), meetings: 3,
               summary: "Pilot customer.", context: ["Pilot ends this month."]),
        Person(id: 7, name: "Ines Ferreira", organization: "Northwind", role: "IT", lastMet: day(12, 15), meetings: 1,
               summary: "Reviews anything that touches the network.", context: []),
        Person(id: 8, name: "Yuki Tanaka", organization: "Independent", role: "Contractor", lastMet: day(20, 11), meetings: 2,
               summary: "Built the first watch prototype.", context: []),
    ]

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

    static let decisions: [Decision] = [
        Decision(id: 1, kind: .link, text: "Is “D. Whitfield” in Tuesday's invite the same person as Dana Whitfield?", accept: "Same person", decline: "Different"),
        Decision(id: 2, kind: .loops, text: "Three open loops from Tuesday's standup are a week old.", accept: "Review", decline: "Close all"),
        Decision(id: 3, kind: .vocabulary, text: "You said “Kubernetes” four times and Sona heard “cube and eighties”.", accept: "Add word", decline: "Ignore"),
        Decision(id: 4, kind: .recovery, text: "A meeting from Monday stopped before it was saved. Twenty-one minutes are recoverable.", accept: "Recover", decline: "Discard"),
    ]

    static let prompts: [SavedPrompt] = [
        SavedPrompt(id: 1, name: "Decisions only", text: "List every decision made, one per line, with who made it.", runs: 23),
        SavedPrompt(id: 2, name: "What did I promise", text: "List everything I committed to, with the deadline if one was said.", runs: 41),
        SavedPrompt(id: 3, name: "Customer note", text: "Write a three-sentence note for the CRM.", runs: 8),
    ]

    static let workflows: [Workflow] = [
        Workflow(id: 1, name: "After every meeting", trigger: "Meeting ends", steps: 3, enabled: true),
        Workflow(id: 2, name: "Customer calls to CRM", trigger: "Meeting with a person from a customer org ends", steps: 2, enabled: true),
        Workflow(id: 3, name: "Weekly digest", trigger: "Friday 17:00", steps: 1, enabled: false),
    ]

    static let agents: [Agent] = [
        Agent(id: 1, name: "Claude Code", transport: "Local socket", lastSeen: "2 min ago"),
        Agent(id: 2, name: "Codex", transport: "Local socket", lastSeen: "yesterday"),
    ]

    static let vocabulary: [VocabularyEntry] = [
        VocabularyEntry(id: 1, word: "Halden", heardAs: "hold in", uses: 31),
        VocabularyEntry(id: 2, word: "Parakeet", heardAs: "pair a kit", uses: 12),
        VocabularyEntry(id: 3, word: "diarization", heardAs: "diary station", uses: 9),
        VocabularyEntry(id: 4, word: "Okafor", heardAs: "oak afore", uses: 7),
    ]

    static let commands: [PaletteCommand] = [
        PaletteCommand(id: 1, title: "Start recording", detail: "Note mode", shortcut: "⌥ Space"),
        PaletteCommand(id: 2, title: "New meeting", detail: "Record the current call", shortcut: "⌥ ⇧ M"),
        PaletteCommand(id: 3, title: "Retention proposal review", detail: "Meeting · today 10:00", shortcut: ""),
        PaletteCommand(id: 4, title: "Dana Whitfield", detail: "Person · Halden", shortcut: ""),
        PaletteCommand(id: 5, title: "Switch mode to Email", detail: "Mode", shortcut: "⌥ E"),
        PaletteCommand(id: 6, title: "Open settings", detail: "", shortcut: "⌘ ,"),
    ]

    static let chat: [ChatTurn] = [
        ChatTurn(id: 1, fromUser: true, text: "What did I promise Dana?"),
        ChatTurn(id: 2, fromUser: false, text: "Two things. You said you would send her the two open questions on the retention window before Friday, and that the migration note for 1.0 users would have an owner by Monday. Nobody was named for the second one."),
        ChatTurn(id: 3, fromUser: true, text: "Draft the questions."),
        ChatTurn(id: 4, fromUser: false, text: "1. Does the ninety-day window count from capture or from last access?\n2. When a workspace is deleted, is its history purged on the same schedule or immediately?"),
    ]
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
