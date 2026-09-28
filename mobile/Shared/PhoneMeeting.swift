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
    }
    struct Text: Codable, Equatable { var text: String }
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
    }
    struct Artifact: Codable, Equatable {
        var state: String
        var generated_at_utc_ms: Int64
        var content: Content?
    }
    struct Segment: Codable, Equatable {
        var segment_id: String
        var transcript_revision_id: String
        var start_offset_ns: UInt64
        var speaker_id: String
        var base_text: String
    }
    struct Edit: Codable, Equatable {
        var segment_id: String
        var edit_sequence: UInt64
        var replacement_text: String
        var removed: Bool
    }
    struct Speaker: Codable, Equatable { var speaker_id: String; var display_name: String }
    struct Assignment: Codable, Equatable {
        var generation_id: String
        var segment_id: String
        var speaker_id: String
    }
    struct OwnNotes: Codable, Equatable { var body: String }
    var format_version: Int
    var session: Session
    var artifact_revisions: [Artifact]
    var transcript_segments: [Segment]
    var segment_edits: [Edit]
    var speakers: [Speaker]
    var diarization_assignments: [Assignment]
    var user_notes: OwnNotes?
    var id: String { session.session_id }
    var date: Date { Date(timeIntervalSince1970: Double(session.created_at_utc_ms) / 1000) }

    var content: Content? {
        artifact_revisions.filter { $0.state == "current" }
            .max { $0.generated_at_utc_ms < $1.generated_at_utc_ms }?.content
    }

    var transcript: String {
        let edits = Dictionary(grouping: segment_edits, by: \.segment_id)
            .compactMapValues { $0.max { $0.edit_sequence < $1.edit_sequence } }
        let names = Dictionary(speakers.map { ($0.speaker_id, $0.display_name) }, uniquingKeysWith: { _, last in last })
        let assignments = Dictionary(diarization_assignments.filter {
            $0.generation_id == session.current_diarization_generation_id
        }.map { ($0.segment_id, $0.speaker_id) }, uniquingKeysWith: { _, last in last })
        return transcript_segments.filter { $0.transcript_revision_id == session.current_transcript_revision_id }
            .sorted { $0.start_offset_ns < $1.start_offset_ns }
            .compactMap { segment -> String? in
                let edit = edits[segment.segment_id]
                guard edit?.removed != true else { return nil }
                let seconds = segment.start_offset_ns / 1_000_000_000
                let time = String(format: "%02llu:%02llu", seconds / 60, seconds % 60)
                let name = names[assignments[segment.segment_id] ?? segment.speaker_id] ?? NSLocalizedString("meeting.speaker", comment: "")
                return "\(time)  \(name): \(edit?.replacement_text ?? segment.base_text)"
            }.joined(separator: "\n\n")
    }

    var notes: String {
        guard let content else { return "" }
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
}

/// Digest and length limits are checked before the phone decodes a bundle or writing profile.
struct PhoneObjectManifest: Decodable {
    var version: Int
    var source_format: String
    var chunk_count: Int
    var plaintext_bytes: Int
    var plaintext_sha256: String
}
