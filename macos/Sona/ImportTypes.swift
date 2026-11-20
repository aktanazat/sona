import Foundation

/// A dictation app Sona brings history and vocabulary in from, named as the
/// core names it.
enum DictationApp: String, Decodable, Hashable {
    case wisprFlow = "wispr_flow"
    case aquaVoice = "aqua_voice"
    case typeless
    case willowVoice = "willow_voice"

    var name: String {
        switch self {
        case .wisprFlow: "Wispr Flow"
        case .aquaVoice: "Aqua Voice"
        case .typeless: "Typeless"
        case .willowVoice: "Willow Voice"
        }
    }
}

/// One row of the Import screen's other-apps lists: what it reads, and where
/// the person finds it.
enum ImportSource: String, CaseIterable, Identifiable, Hashable {
    case wisprFlowDictations
    case aquaVoice
    case typeless
    case willowVoice
    case granola
    case wisprFlowMeetings
    case otter

    enum Kind {
        case dictation(DictationApp)
        /// The core's name for the meeting app.
        case meetings(String)
    }

    static let dictationApps: [ImportSource] = [.wisprFlowDictations, .aquaVoice, .typeless, .willowVoice]
    static let meetingApps: [ImportSource] = [.granola, .wisprFlowMeetings, .otter]

    var id: String { rawValue }

    var kind: Kind {
        switch self {
        case .wisprFlowDictations: .dictation(.wisprFlow)
        case .aquaVoice: .dictation(.aquaVoice)
        case .typeless: .dictation(.typeless)
        case .willowVoice: .dictation(.willowVoice)
        case .granola: .meetings("granola")
        case .wisprFlowMeetings: .meetings("wispr_flow")
        case .otter: .meetings("otter")
        }
    }

    var name: String {
        switch self {
        case .wisprFlowDictations, .wisprFlowMeetings: "Wispr Flow"
        case .aquaVoice: "Aqua Voice"
        case .typeless: "Typeless"
        case .willowVoice: "Willow Voice"
        case .granola: "Granola"
        case .otter: "Otter"
        }
    }

    /// What the import brings in. The picker opens in the app's own folder,
    /// so the hint never has to say where it is.
    var hint: String {
        switch self {
        case .wisprFlowDictations, .aquaVoice: "History and dictionary"
        case .typeless: "History"
        case .willowVoice: "Transcripts and dictionary"
        case .granola: "CSV export from Settings › Profile"
        case .wisprFlowMeetings: "Notetaker meetings"
        case .otter: "TXT, DOCX or SRT export, with speaker names"
        }
    }

    /// Where the app keeps its data on this Mac, for the apps that keep it
    /// here. Granola and Otter exports land wherever they were saved.
    var dataFolder: URL? {
        let support = FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/Application Support", directoryHint: .isDirectory)
        return switch self {
        case .wisprFlowDictations, .wisprFlowMeetings:
            support.appending(path: "Wispr Flow", directoryHint: .isDirectory)
        case .aquaVoice: support.appending(path: "Aqua Voice", directoryHint: .isDirectory)
        case .typeless: support.appending(path: "Typeless", directoryHint: .isDirectory)
        case .willowVoice: support.appending(path: "com.seewillow.WillowMac", directoryHint: .isDirectory)
        case .granola, .otter: nil
        }
    }
}

/// What one import did, in the words the row shows.
struct ImportOutcome: Equatable {
    let sentence: String
    let failed: Bool
}

/// A dictation import waiting on the person's agreement to lose older
/// dictations.
struct PendingRemovals: Equatable {
    let source: ImportSource
    let path: String
    let removals: Int
}

struct DictationImportResult: Decodable {
    let dictationsImported: Int
    /// Already imported by an earlier run.
    let dictationsSkipped: Int
    let wordsAdded: Int
    /// Already in the vocabulary, or a kind of entry Sona does not keep.
    let wordsSkipped: Int

    var sentence: String {
        let imported = "Imported \(ImportCount.dictations(dictationsImported)) and \(LibraryCount.words(wordsAdded))"
        let skippedParts = [
            dictationsSkipped > 0 ? ImportCount.dictations(dictationsSkipped) : nil,
            wordsSkipped > 0 ? LibraryCount.words(wordsSkipped) : nil,
        ].compactMap { $0 }
        guard !skippedParts.isEmpty else { return imported }
        let skipped = "Already here or not supported: " + skippedParts.joined(separator: " and ")
        return dictationsImported + wordsAdded == 0 ? skipped : imported + ". " + skipped
    }
}

struct MeetingAppImportResult: Decodable {
    let imported: Int
    let alreadyImported: Int
    let unreadable: Int

    var sentence: String {
        var skipped: [String] = []
        if alreadyImported > 0 { skipped.append("\(alreadyImported.formatted()) already imported") }
        if unreadable > 0 { skipped.append("\(unreadable.formatted()) Sona couldn't read") }
        let imported = "Imported \(ImportCount.meetings(imported))"
        return skipped.isEmpty ? imported : imported + ". Skipped " + skipped.joined(separator: " and ")
    }
}

enum ImportCount {
    static func dictations(_ count: Int) -> String {
        "\(count.formatted()) \(count == 1 ? "dictation" : "dictations")"
    }

    static func meetings(_ count: Int) -> String {
        "\(count.formatted()) \(count == 1 ? "meeting" : "meetings")"
    }
}
