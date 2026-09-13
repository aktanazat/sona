//! The summary's line-to-segment map, through the store it is written to: a
//! traced summary comes back with its anchors intact, and a revision written
//! before line provenance existed still reads back, with no map and no jumps.

use super::workflow_core_tests::{meeting, store};
use super::{ArtifactRevisionInput, MeetingStore};
use crate::meeting::types::{
    ArtifactCitation, CitedArtifactText, GeneratedMeetingArtifacts, MeetingArtifactState,
    MeetingSessionId, SummaryLineTrace, TranscriptRevisionId, TranscriptSegmentId,
};
use rusqlite::params;

const NOW: i64 = 1_700_000_000_000;
const GENERATION_KEY: &str = "trace-key";

/// A completed transcript revision for `session_id`, which an artifact
/// revision points at by foreign key.
fn transcript_revision(store: &MeetingStore, session_id: MeetingSessionId) -> TranscriptRevisionId {
    let transcript_revision_id = TranscriptRevisionId::new();
    store
        .connection()
        .expect("store connection")
        .execute(
            "INSERT INTO meeting_transcript_revisions (
                transcript_revision_id, session_id, engine_id, destination_json,
                source_set_json, language, state, created_at_utc_ms, completed_at_utc_ms
             ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'completed', ?3, ?3)",
            params![
                transcript_revision_id.uuid().to_string(),
                session_id.uuid().to_string(),
                NOW,
            ],
        )
        .expect("insert transcript revision");
    transcript_revision_id
}

fn cited(text: &str) -> CitedArtifactText {
    CitedArtifactText {
        text: text.to_string(),
        citations: Vec::new(),
    }
}

fn artifacts(
    summary: CitedArtifactText,
    summary_trace: Vec<SummaryLineTrace>,
) -> GeneratedMeetingArtifacts {
    GeneratedMeetingArtifacts {
        summary,
        summary_trace,
        outline: Vec::new(),
        decisions: Vec::new(),
        action_items: Vec::new(),
        key_questions: Vec::new(),
        risks: Vec::new(),
        follow_up_draft: cited("Thanks all."),
        ledger: None,
        ledger_failure: None,
    }
}

fn stored(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    content: &GeneratedMeetingArtifacts,
) -> GeneratedMeetingArtifacts {
    let transcript_revision_id = transcript_revision(store, session_id);
    store
        .store_artifact_revision(ArtifactRevisionInput {
            session_id,
            transcript_revision_id,
            input_revision: 0,
            template_id: "default",
            template_version: 5,
            generation_key: GENERATION_KEY,
            state: MeetingArtifactState::Current,
            content: Some(content),
            generated_at_utc_ms: NOW,
        })
        .expect("store artifact revision");
    store
        .artifact_by_generation_key(session_id, GENERATION_KEY)
        .expect("read artifact revision")
        .expect("the revision just written")
        .content
        .expect("current revision carries content")
}

#[test]
fn traced_summary_lines_survive_the_store() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Pricing review", NOW);
    let first = TranscriptSegmentId::new();
    let second = TranscriptSegmentId::new();
    let content = artifacts(
        cited("Pricing stayed open.\nDana took the comparison."),
        vec![
            SummaryLineTrace {
                line: 0,
                anchor: ArtifactCitation {
                    segment_id: first,
                    start_offset_ns: 12_000_000_000,
                    end_offset_ns: 14_000_000_000,
                },
            },
            SummaryLineTrace {
                line: 1,
                anchor: ArtifactCitation {
                    segment_id: second,
                    start_offset_ns: 30_000_000_000,
                    end_offset_ns: 31_500_000_000,
                },
            },
        ],
    );

    let read_back = stored(&store, session_id, &content);

    assert_eq!(read_back.summary_trace, content.summary_trace);
    // The map is only meaningful against the text it indexes, so the pair has
    // to come back together: one entry per line, in line order.
    let lines: Vec<&str> = read_back.summary.text.lines().collect();
    assert_eq!(lines.len(), read_back.summary_trace.len());
    for (ordinal, entry) in read_back.summary_trace.iter().enumerate() {
        assert_eq!(usize::try_from(entry.line).expect("line ordinal"), ordinal);
    }
    assert_eq!(read_back.summary_trace[1].anchor.segment_id, second);
}

#[test]
fn revision_written_before_line_provenance_reads_back_untraced() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Older meeting", NOW);
    // Exactly the artifact JSON this branch wrote before the map existed.
    let legacy = serde_json::json!({
        "summary": { "text": "Pricing stayed open.", "citations": [] },
        "outline": [],
        "decisions": [],
        "action_items": [],
        "key_questions": [],
        "risks": [],
        "follow_up_draft": { "text": "Thanks all.", "citations": [] }
    });
    let transcript_revision_id = transcript_revision(&store, session_id);
    store
        .connection()
        .expect("store connection")
        .execute(
            "INSERT INTO meeting_artifact_revisions (
                artifact_id, session_id, transcript_revision_id, input_revision, template_id,
                template_version, generation_key, state, content_json, generated_at_utc_ms
             ) VALUES (?1, ?2, ?3, 0, 'default', 4, ?4, 'current', ?5, ?6)",
            params![
                uuid::Uuid::new_v4().to_string(),
                session_id.uuid().to_string(),
                transcript_revision_id.uuid().to_string(),
                GENERATION_KEY,
                serde_json::to_string(&legacy).expect("encode legacy artifact"),
                NOW,
            ],
        )
        .expect("insert legacy artifact");

    let content = store
        .artifact_by_generation_key(session_id, GENERATION_KEY)
        .expect("read legacy artifact")
        .expect("the legacy revision")
        .content
        .expect("legacy revision carries content");

    assert_eq!(content.summary.text, "Pricing stayed open.");
    assert!(content.summary_trace.is_empty());
}
