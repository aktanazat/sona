import Foundation

enum PersonMemorySourceKind: String, Decodable {
    case conversation, transcript, note
    case meetingNote = "meeting_note"
    case noteVersion = "note_version"
}

struct PersonMemorySource: Decodable, Identifiable {
    let kind: PersonMemorySourceKind
    let id: String
    let title: String
    let link: String
    let atUtcMs: Int64
    let meetingId: String?
    let startOffsetNs: UInt64?
    let revision: String?
    let transcriptAvailable: Bool?
    let audioRetained: Bool?

    var at: Date { PeopleFormat.date(atUtcMs) }
    var timestamp: String? {
        guard let startOffsetNs else { return nil }
        let seconds = startOffsetNs / 1_000_000_000
        return String(format: "%llu:%02llu", seconds / 60, seconds % 60)
    }
}

struct PersonMemoryPassage: Decodable {
    let source: PersonMemorySource
    let speakerLabel: String
    let rawText: String
    let correctedText: String?
    let editRevision: UInt64?
}

struct PersonMemoryContribution: Decodable, Identifiable {
    let id: String
    let passage: PersonMemoryPassage
    let attribution: PersonContributionAttribution
    let before: PersonMemoryPassage?
    let after: PersonMemoryPassage?
}

struct PersonMemoryConversation: Decodable, Identifiable {
    let source: PersonMemorySource
    let linkSource: PersonLinkSource
    let confidence: PersonLinkConfidence
    let transcriptAvailable: Bool
    let attributedContributions: UInt64
    var id: String { source.id }
}

struct PersonMemoryNote: Decodable, Identifiable {
    let source: PersonMemorySource
    let body: String
    let includesAiSynthesis: Bool
    let directlyLinked: Bool
    let relatedSources: [PersonMemorySource]
    var id: String { source.id }
}

struct PersonMemoryCommitment: Decodable, Identifiable {
    enum Relation: String, Decodable {
        case explicitOwner = "explicit_owner"
        case attributedSpeech = "attributed_speech"
        case sharedConversation = "shared_conversation"
    }
    let id: String
    let interpretation: String
    let status: PersonLoopStatus
    let relation: Relation
    let evidence: [PersonMemoryPassage]
}

struct PersonMemoryThread: Decodable, Identifiable {
    enum Origin: String, Decodable {
        case explicitSubject = "explicit_subject"
        case aiTopic = "ai_topic"
    }
    let id: String
    let title: String
    let interpretation: String
    let origin: Origin
    let sources: [PersonMemorySource]
    let evidence: [PersonMemoryPassage]
}

struct PersonMemoryComparisonSide: Decodable {
    let source: PersonMemorySource
    let text: String
    let includesAiInterpretation: Bool
}

struct PersonMemoryChange: Decodable, Identifiable {
    enum Kind: String, Decodable {
        case noteEdited = "note_edited"
        case subjectEvidenceDiffers = "subject_evidence_differs"
    }
    let id: String
    let kind: Kind
    let title: String
    let earlier: PersonMemoryComparisonSide
    let later: PersonMemoryComparisonSide
}

struct PersonMemoryCatchUp: Decodable {
    let lastConversation: PersonMemorySource?
    let contributionIds: [String]
    let commitmentIds: [String]
    let noteIds: [String]
    let changeIds: [String]
}

struct PersonMemorySnapshot: Decodable {
    let schemaVersion: UInt32
    let personId: String
    let displayName: String
    let peopleRevision: UInt64
    let readAtUtcMs: Int64
    let sinceUtcMs: Int64?
    let conversations: [PersonMemoryConversation]
    let contributions: [PersonMemoryContribution]
    let notes: [PersonMemoryNote]
    let commitments: [PersonMemoryCommitment]
    let threads: [PersonMemoryThread]
    let changes: [PersonMemoryChange]
    let catchUp: PersonMemoryCatchUp
    let limitations: [String]
}

enum PersonHistoryFilter: String, CaseIterable, Identifiable {
    case all = "All"
    case conversations = "Conversations"
    case contributions = "Their words"
    case notes = "Notes"
    case commitments = "Commitments"
    var id: String { rawValue }
}

enum PersonHistoryEntry: Identifiable {
    case conversation(PersonMemoryConversation)
    case contribution(PersonMemoryContribution)
    case note(PersonMemoryNote)
    case commitment(PersonMemoryCommitment)

    var id: String {
        switch self {
        case let .conversation(row): "conversation:\(row.id)"
        case let .contribution(row): "contribution:\(row.id)"
        case let .note(row): "note:\(row.id)"
        case let .commitment(row): "commitment:\(row.id)"
        }
    }

    var source: PersonMemorySource? {
        switch self {
        case let .conversation(row): row.source
        case let .contribution(row): row.passage.source
        case let .note(row): row.source
        case let .commitment(row): row.evidence.first?.source
        }
    }

    var filter: PersonHistoryFilter {
        switch self {
        case .conversation: .conversations
        case .contribution: .contributions
        case .note: .notes
        case .commitment: .commitments
        }
    }
}
