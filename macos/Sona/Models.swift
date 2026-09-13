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
