import Foundation

/// The phone reads the portable, non-audio meeting bundle without changing its wire format.
struct PhoneMeeting: Codable, Equatable, Identifiable {
    static let sourceFormat = "sona-meeting-bundle-json-v1"
    static let phoneSourceFormat = "sona-phone-meeting-v1"
    struct Session: Codable, Equatable {
        var session_id: String
        var title: String
        var created_at_utc_ms: Int64
        var current_transcript_revision_id: String?
        var current_diarization_generation_id: String?
        var started_at_utc_ms: Int64?
        var ended_at_utc_ms: Int64?
        var origin: String?
    }
    struct Citation: Codable, Equatable, Hashable {
        var segment_id: String
        var start_offset_ns: UInt64
        var end_offset_ns: UInt64
    }
    struct Text: Codable, Equatable {
        var text: String
        var citations: [Citation]?
    }
    struct SummaryTrace: Codable, Equatable {
        var line: Int
        var anchor: Citation
    }
    struct Topic: Codable, Equatable { var title: Text; var detail: Text? }
    struct Action: Codable, Equatable { var text: Text; var owner_text: String?; var due_text: String? }
    struct Content: Codable, Equatable {
        var summary: Text
        var outline: [Topic]
        var decisions: [Text]
        var action_items: [Action]
        var key_questions: [Text]
        var risks: [Text]
        var follow_up_draft: Text
        var summary_trace: [SummaryTrace]?
    }
    struct Artifact: Codable, Equatable {
        var state: String
        var generated_at_utc_ms: Int64
        var content: Content?
        var transcript_revision_id: String?
    }
    struct Segment: Codable, Equatable {
        var segment_id: String
        var transcript_revision_id: String
        var start_offset_ns: UInt64
        var speaker_id: String
        var base_text: String
        var learned_text: String?
        var end_offset_ns: UInt64?
        var ordinal: UInt64?
        var track_id: String?
    }
    struct Edit: Codable, Equatable {
        var segment_id: String
        var edit_sequence: UInt64
        var replacement_text: String
        var removed: Bool
    }
    struct Speaker: Codable, Equatable {
        var speaker_id: String
        var display_name: String
        var source_kind: String?
        var merged_into_speaker_id: String?
    }
    struct Assignment: Codable, Equatable {
        var generation_id: String
        var segment_id: String
        var speaker_id: String
        var assignment_kind: String?
    }
    struct OwnNotes: Codable, Equatable { var body: String }
    struct ManualNote: Codable, Equatable, Identifiable {
        var note_id: String
        var body: String
        var start_offset_ns: UInt64?
        var end_offset_ns: UInt64?
        var created_at_utc_ms: Int64
        var id: String { note_id }
    }
    struct AnswerCitation: Codable, Equatable, Hashable {
        var kind: String
        var session_id: String
        var entity_id: String
        var start_offset_ns: UInt64?
        var end_offset_ns: UInt64?
    }
    struct Answer: Codable, Equatable, Identifiable {
        var question_id: String
        var question: String?
        var state: String
        var answer: String?
        var citations: [AnswerCitation]
        var provisional: Bool?
        var id: String { question_id }
    }
    struct Location: Codable, Equatable {
        var name: String
        var latitude: Double
        var longitude: Double
        var hasValidCoordinates: Bool {
            latitude.isFinite && longitude.isFinite &&
                (-90...90).contains(latitude) && (-180...180).contains(longitude)
        }
        var mapsURL: URL? {
            guard hasValidCoordinates else { return nil }
            var components = URLComponents()
            components.scheme = "maps"
            components.host = ""
            components.queryItems = [
                URLQueryItem(name: "ll", value: "\(latitude),\(longitude)"),
                URLQueryItem(name: "q", value: name),
            ]
            return components.url
        }
    }
    struct ConversationContext: Codable, Equatable {
        var session_id: String
        var revision: UInt64
        var kind: String
        var origin: String
        var application_bundle_ids: [String]
        var captured_microphone: Bool
        var captured_system_audio: Bool
        var calendar_name: String?
        var calendar_location: String?
        var location: Location?
    }
    struct SourceTrack: Codable, Equatable {
        var track_id: String
        var source_kind: String
        var first_offset_ns: UInt64?
        var last_offset_ns: UInt64?
    }
    struct Turn: Equatable, Identifiable {
        let segment: Segment
        let speakerName: String
        let edit: Edit?
        var id: String { segment.segment_id }
        var originalText: String { segment.base_text }
        var correctedText: String { edit?.replacement_text ?? segment.learned_text ?? originalText }
        var isRemoved: Bool { edit?.removed == true }
        var isEdited: Bool { edit != nil }
        var timestamp: String { PhoneMeeting.timestamp(segment.start_offset_ns) }
    }
    var format_version: Int
    var session: Session
    var artifact_revisions: [Artifact]
    var transcript_segments: [Segment]
    var segment_edits: [Edit]
    var speakers: [Speaker]
    var diarization_assignments: [Assignment]
    var user_notes: OwnNotes?
    var conversation_context: ConversationContext?
    var transcript_purged_at_utc_ms: Int64?
    var source_tracks: [SourceTrack]?
    var manual_notes: [ManualNote]?
    var questions: [Answer]?
    var id: String { session.session_id }
    var date: Date { Date(timeIntervalSince1970: Double(session.created_at_utc_ms) / 1000) }

    enum CodingKeys: String, CodingKey {
        case format_version, session, artifact_revisions, transcript_segments, segment_edits
        case speakers, diarization_assignments, user_notes, conversation_context
        case transcript_purged_at_utc_ms, source_tracks, questions
        case manual_notes = "notes"
    }

    var content: Content? {
        artifact_revisions.filter { $0.state == "current" }
            .max { $0.generated_at_utc_ms < $1.generated_at_utc_ms }?.content
    }

    var savedArtifact: Artifact? {
        let current = artifact_revisions.filter { $0.state == "current" && $0.content != nil }
            .max { $0.generated_at_utc_ms < $1.generated_at_utc_ms }
        return current ?? artifact_revisions.filter { $0.state == "out_of_date" && $0.content != nil }
            .max { $0.generated_at_utc_ms < $1.generated_at_utc_ms }
    }

    var turns: [Turn] { turns(revisionID: session.current_transcript_revision_id) }

    func turns(revisionID: String?) -> [Turn] {
        guard transcript_purged_at_utc_ms == nil, let revisionID else { return [] }
        let edits = Dictionary(grouping: segment_edits, by: \.segment_id)
            .compactMapValues { $0.max { $0.edit_sequence < $1.edit_sequence } }
        let names = Dictionary(speakers.map { ($0.speaker_id, $0) }, uniquingKeysWith: { _, last in last })
        let assignments = Dictionary(diarization_assignments.filter {
            $0.generation_id == session.current_diarization_generation_id
        }.map { ($0.segment_id, $0) }, uniquingKeysWith: { _, last in last })
        return transcript_segments.filter { $0.transcript_revision_id == revisionID }
            .sorted {
                if $0.start_offset_ns != $1.start_offset_ns { return $0.start_offset_ns < $1.start_offset_ns }
                if $0.ordinal != $1.ordinal { return ($0.ordinal ?? 0) < ($1.ordinal ?? 0) }
                return $0.segment_id < $1.segment_id
            }
            .map { segment in
                let assignment = assignments[segment.segment_id]
                var speakerID = assignment?.speaker_id ?? segment.speaker_id
                var seen: Set<String> = []
                while let next = names[speakerID]?.merged_into_speaker_id, seen.insert(speakerID).inserted {
                    speakerID = next
                }
                let speaker = names[speakerID]
                let name: String
                switch assignment?.assignment_kind {
                case "unknown":
                    name = String(localized: "conversation.unknownSpeaker", defaultValue: "Unknown speaker")
                case "overlap":
                    name = String(localized: "conversation.overlap", defaultValue: "Overlapping voices")
                default:
                    let saved = speaker?.display_name.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                    name = saved.isEmpty
                        ? String(localized: "conversation.unknownSpeaker", defaultValue: "Unknown speaker") : saved
                }
                return Turn(segment: segment, speakerName: name, edit: edits[segment.segment_id])
            }
    }

    var transcript: String {
        turns.filter { !$0.isRemoved }.map { "\($0.timestamp)  \($0.speakerName): \($0.correctedText)" }
            .joined(separator: "\n\n")
    }

    var originalTranscript: String {
        turns.map { "\($0.timestamp)  \($0.speakerName): \($0.originalText)" }.joined(separator: "\n\n")
    }

    var notes: String {
        content.map(Self.noteText) ?? ""
    }

    var savedNotes: String {
        guard let artifact = savedArtifact, let content = artifact.content else { return "" }
        let text = Self.noteText(content)
        guard artifact.state != "current" else { return text }
        return String(localized: "conversation.outOfDate", defaultValue: "Out of date") + "\n" + text
    }

    private static func noteText(_ content: Content) -> String {
        var sections = [content.summary.text]
        func append(_ key: String, _ rows: [String]) {
            let rows = rows.filter { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
            if !rows.isEmpty { sections.append(NSLocalizedString(key, comment: "") + "\n" + rows.joined(separator: "\n")) }
        }
        append("meeting.topics", content.outline.map { [$0.title.text, $0.detail?.text].compactMap { $0 }.joined(separator: "\n") })
        append("meeting.decisions", content.decisions.map(\.text))
        append("meeting.actions", content.action_items.map { [$0.text.text, $0.owner_text, $0.due_text].compactMap { $0 }.joined(separator: " — ") })
        append("meeting.questions", content.key_questions.map(\.text))
        append("meeting.risks", content.risks.map(\.text))
        append("meeting.followUp", [content.follow_up_draft.text])
        return sections.filter { !$0.isEmpty }.joined(separator: "\n\n")
    }

    static func timestamp(_ nanoseconds: UInt64) -> String {
        let seconds = nanoseconds / 1_000_000_000
        if seconds >= 3_600 {
            return String(format: "%llu:%02llu:%02llu", seconds / 3_600, (seconds / 60) % 60, seconds % 60)
        }
        return String(format: "%02llu:%02llu", seconds / 60, seconds % 60)
    }
}

/// Digest and length limits are checked before the phone decodes a bundle or writing profile.
struct PhoneObjectManifest: Decodable {
    var version: Int
    var source_format: String
    var chunk_count: Int
    var plaintext_bytes: Int
    var plaintext_sha256: String
}
