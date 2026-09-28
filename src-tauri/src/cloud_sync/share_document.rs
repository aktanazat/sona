//! The document a browser share link carries.
//!
//! A link used to carry a flat Markdown page of the transcript and the notes
//! typed during the meeting, and nothing Sona wrote: no summary, no decisions
//! or action items, not the notes the person typed beside them. It now
//! carries this document, which the companion's viewer
//! (`cloudflare/sona-companion/public/viewer.js`, `parseShareDocument`)
//! renders section by section through text nodes only. The viewer refuses
//! anything outside the shape and bounds below, so each bound has a twin
//! there. Bounds here count UTF-8 bytes and the viewer counts UTF-16 units; a
//! string is never fewer bytes than units, so what passes here passes there.

use std::collections::HashMap;

use serde::Serialize;

use crate::meeting::types::{
    CitedArtifactText, GeneratedMeetingArtifacts, MeetingArtifactRevision, MeetingArtifactState,
    MeetingReviewSnapshot, SpeakerId,
};

use super::types::CloudBrowserShareInclude;

pub(super) const SHARE_DOCUMENT_KIND: &str = "notes_document";
pub(super) const SHARE_DOCUMENT_SOURCE_FORMAT: &str = "sona-share-document-v1";
const SHARE_DOCUMENT_VERSION: u32 = 1;
/// The viewer's `MAX_TITLE_CHARS`; the manifest title is the same string.
const MAX_TITLE_BYTES: usize = 240;
/// The viewer's `MAX_RENDER_BLOCKS`, counted across every section.
const MAX_BLOCKS: usize = 10_000;
/// The viewer's `MAX_RENDER_LINE_CHARS`, for text and meta alike.
const MAX_BLOCK_TEXT_BYTES: usize = 256 * 1024;
/// Half the viewer's `MAX_VIEWER_PLAINTEXT`.
const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
const FALLBACK_TITLE: &str = "Sona shared notes";
const UNKNOWN_SPEAKER: &str = "Unknown speaker";

/// The meeting does not fit what the viewer renders.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct ShareDocumentTooLarge;

#[derive(Serialize)]
struct ShareDocument<'a> {
    version: u32,
    title: &'a str,
    include: CloudBrowserShareInclude,
    notes_out_of_date: bool,
    sections: Vec<Section>,
}

#[derive(Serialize)]
struct Section {
    heading: &'static str,
    blocks: Vec<Block>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    Paragraph {
        text: String,
    },
    Item {
        text: String,
        meta: Option<String>,
        time: Option<String>,
    },
}

/// A meeting as far as `include` lets it be seen, and the title its manifest
/// carries. Sections with nothing in them are left out, so a meeting with no
/// notes yet is an empty, valid document the viewer shows as such.
pub(super) fn meeting_document(
    review: &MeetingReviewSnapshot,
    user_notes: &str,
    include: CloudBrowserShareInclude,
) -> Result<(String, Vec<u8>), ShareDocumentTooLarge> {
    let title = bounded_title(&review.session.title);
    let artifact = readable_artifact(&review.artifacts);
    let mut sections = Vec::new();
    if let Some(content) = artifact.and_then(|artifact| artifact.content.as_ref()) {
        generated_sections(content, &mut sections);
    }
    push_section(&mut sections, "Your own notes", paragraphs(user_notes));
    if include == CloudBrowserShareInclude::Everything {
        let moments = review
            .notes
            .iter()
            .flat_map(|note| items(&note.body, None, note.start_offset_ns.map(clock)))
            .collect();
        push_section(&mut sections, "Notes in the moment", moments);
    }
    if include != CloudBrowserShareInclude::Notes {
        push_section(&mut sections, "Transcript", transcript(review));
    }
    encode_document(
        title,
        include,
        artifact.is_some_and(|artifact| artifact.state != MeetingArtifactState::Current),
        sections,
    )
}

pub(super) fn scratch_document(
    title: &str,
    body: &str,
) -> Result<(String, Vec<u8>), ShareDocumentTooLarge> {
    let mut sections = Vec::new();
    push_section(&mut sections, "Note", paragraphs(body));
    encode_document(
        bounded_title(title),
        CloudBrowserShareInclude::Notes,
        false,
        sections,
    )
}

fn encode_document(
    title: String,
    include: CloudBrowserShareInclude,
    notes_out_of_date: bool,
    sections: Vec<Section>,
) -> Result<(String, Vec<u8>), ShareDocumentTooLarge> {
    let blocks = sections
        .iter()
        .map(|section| section.blocks.len())
        .sum::<usize>();
    if blocks > MAX_BLOCKS {
        return Err(ShareDocumentTooLarge);
    }
    let bytes = serde_json::to_vec(&ShareDocument {
        version: SHARE_DOCUMENT_VERSION,
        title: &title,
        include,
        notes_out_of_date,
        sections,
    })
    .map_err(|_| ShareDocumentTooLarge)?;
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(ShareDocumentTooLarge);
    }
    Ok((title, bytes))
}

/// The notes the review page reads: the newest current revision, else the
/// newest out-of-date one, which the document then flags.
fn readable_artifact(artifacts: &[MeetingArtifactRevision]) -> Option<&MeetingArtifactRevision> {
    let newest = |state: MeetingArtifactState| {
        artifacts
            .iter()
            .filter(|artifact| artifact.state == state && artifact.content.is_some())
            .max_by_key(|artifact| artifact.generated_at_utc_ms)
    };
    newest(MeetingArtifactState::Current).or_else(|| newest(MeetingArtifactState::OutOfDate))
}

/// The generated notes, in the order and under the labels the review page
/// uses.
fn generated_sections(content: &GeneratedMeetingArtifacts, sections: &mut Vec<Section>) {
    push_section(sections, "Summary", paragraphs(&content.summary.text));
    let covered = content
        .outline
        .iter()
        .flat_map(|topic| {
            let detail = topic.detail.as_ref().and_then(|detail| meta(&detail.text));
            items(&topic.title.text, detail, None)
        })
        .collect();
    push_section(sections, "What was covered", covered);
    push_section(sections, "Decisions", cited_items(&content.decisions));
    let actions = content
        .action_items
        .iter()
        .flat_map(|item| {
            let owner = [item.owner_text.as_deref(), item.due_text.as_deref()]
                .into_iter()
                .flatten()
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            items(&item.text.text, meta(&owner), None)
        })
        .collect();
    push_section(sections, "Action items", actions);
    push_section(
        sections,
        "Questions worth answering",
        cited_items(&content.key_questions),
    );
    push_section(sections, "Risks", cited_items(&content.risks));
    push_section(
        sections,
        "Follow up",
        paragraphs(&content.follow_up_draft.text),
    );
}

/// Consecutive lines of one speaker read as one turn, as they do on the
/// review page, which keeps a long meeting inside the viewer's block count.
fn transcript(review: &MeetingReviewSnapshot) -> Vec<Block> {
    let speakers = review
        .speakers
        .iter()
        .map(|speaker| (speaker.speaker_id, speaker.display_name.as_str()))
        .collect::<HashMap<_, _>>();
    let mut turns: Vec<(SpeakerId, u64, String)> = Vec::new();
    for segment in review.transcript.iter().filter(|segment| !segment.removed) {
        let text = segment
            .replacement_text
            .as_deref()
            .unwrap_or(&segment.base.text)
            .trim();
        if text.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some((speaker, _, turn))
                if *speaker == segment.assigned_speaker_id
                    && turn.len() + 1 + text.len() <= MAX_BLOCK_TEXT_BYTES =>
            {
                turn.push(' ');
                turn.push_str(text);
            }
            _ => turns.push((
                segment.assigned_speaker_id,
                segment.base.start_offset_ns,
                text.to_owned(),
            )),
        }
    }
    turns
        .into_iter()
        .flat_map(|(speaker, start, text)| {
            let name = speakers
                .get(&speaker)
                .and_then(|name| meta(name))
                .unwrap_or_else(|| UNKNOWN_SPEAKER.to_owned());
            items(&text, Some(name), Some(clock(start)))
        })
        .collect()
}

fn push_section(sections: &mut Vec<Section>, heading: &'static str, blocks: Vec<Block>) {
    if !blocks.is_empty() {
        sections.push(Section { heading, blocks });
    }
}

fn cited_items(values: &[CitedArtifactText]) -> Vec<Block> {
    values
        .iter()
        .flat_map(|value| items(&value.text, None, None))
        .collect()
}

/// One entry, split into as many blocks as its text needs; the meta and the
/// time go on the first.
fn items(text: &str, meta: Option<String>, time: Option<String>) -> Vec<Block> {
    let mut blocks = Vec::new();
    for piece in pieces(text) {
        let first = blocks.is_empty();
        blocks.push(Block::Item {
            text: piece.to_owned(),
            meta: if first { meta.clone() } else { None },
            time: if first { time.clone() } else { None },
        });
    }
    blocks
}

/// Paragraphs are separated by blank lines; single line breaks stay inside
/// the paragraph, and the viewer keeps them.
fn paragraphs(text: &str) -> Vec<Block> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    for line in normalized.split('\n').chain(std::iter::once("")) {
        if !line.trim().is_empty() {
            paragraph.push(line.trim_end());
            continue;
        }
        if paragraph.is_empty() {
            continue;
        }
        let joined = paragraph.join("\n");
        blocks.extend(pieces(&joined).map(|piece| Block::Paragraph {
            text: piece.to_owned(),
        }));
        paragraph.clear();
    }
    blocks
}

/// `text` trimmed and cut into pieces the viewer accepts, at whitespace where
/// there is some.
fn pieces(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text.trim();
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut cut = rest.len().min(MAX_BLOCK_TEXT_BYTES);
        if cut < rest.len() {
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            if let Some(space) = rest[..cut]
                .rfind(char::is_whitespace)
                .filter(|space| *space > 0)
            {
                cut = space;
            }
        }
        let (piece, tail) = rest.split_at(cut);
        rest = tail.trim_start();
        Some(piece.trim_end())
    })
}

fn meta(value: &str) -> Option<String> {
    let value = truncate(value.trim(), MAX_BLOCK_TEXT_BYTES).trim_end();
    (!value.is_empty()).then(|| value.to_owned())
}

fn bounded_title(raw: &str) -> String {
    let single_line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = truncate(&single_line, MAX_TITLE_BYTES).trim_end();
    if title.is_empty() {
        FALLBACK_TITLE.to_owned()
    } else {
        title.to_owned()
    }
}

fn truncate(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut cut = max_bytes;
    while !value.is_char_boundary(cut) {
        cut -= 1;
    }
    &value[..cut]
}

/// The review page's clock: `M:SS`, or `H:MM:SS` past the first hour.
fn clock(offset_ns: u64) -> String {
    let total = offset_ns / 1_000_000_000;
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::types::{
        AllowedMeetingAction, CaptureCompleteness, DiarizationStatus, EffectiveTranscriptSegment,
        ManualNote, ManualNoteId, MeetingActionItem, MeetingArtifactId, MeetingDiarizationSnapshot,
        MeetingOutlineTopic, MeetingPhase, MeetingSessionId, MeetingSessionSnapshot,
        MeetingSpeaker, ProcessingFailure, ProcessingStatus, SourceKind, SourceTrackId,
        SpeakerAssignmentKind, StorageAvailability, TranscriptRevisionId, TranscriptSegment,
        TranscriptSegmentId,
    };

    const SECOND: u64 = 1_000_000_000;

    fn cited(text: &str) -> CitedArtifactText {
        CitedArtifactText {
            text: text.to_owned(),
            citations: Vec::new(),
        }
    }

    fn revision(
        session_id: MeetingSessionId,
        state: MeetingArtifactState,
        summary: &str,
    ) -> MeetingArtifactRevision {
        MeetingArtifactRevision {
            artifact_id: MeetingArtifactId::new(),
            session_id,
            transcript_revision_id: TranscriptRevisionId::new(),
            input_revision: 1,
            template_id: "general".to_owned(),
            template_version: 1,
            generation_key: "key".to_owned(),
            state,
            generated_at_utc_ms: 10,
            content: Some(GeneratedMeetingArtifacts {
                summary: cited(summary),
                summary_trace: Vec::new(),
                outline: vec![MeetingOutlineTopic {
                    title: cited("Launch date"),
                    detail: Some(cited("Moved to May")),
                }],
                decisions: vec![cited("Ship the redesign in May.")],
                action_items: vec![MeetingActionItem {
                    text: cited("Draft the launch note"),
                    owner_text: Some("Priya".to_owned()),
                    due_text: Some("Friday".to_owned()),
                }],
                key_questions: Vec::new(),
                risks: vec![cited("The venue may fall through.")],
                follow_up_draft: cited(""),
                ledger: None,
                ledger_failure: None,
            }),
        }
    }

    fn segment(
        speaker: SpeakerId,
        start_s: u64,
        text: &str,
        replacement: Option<&str>,
        removed: bool,
    ) -> EffectiveTranscriptSegment {
        EffectiveTranscriptSegment {
            base: TranscriptSegment {
                segment_id: TranscriptSegmentId::new(),
                transcript_revision_id: TranscriptRevisionId::new(),
                track_id: SourceTrackId::new(),
                ordinal: start_s,
                start_offset_ns: start_s * SECOND,
                end_offset_ns: (start_s + 1) * SECOND,
                speaker_id: speaker,
                text: text.to_owned(),
                confidence_milli: None,
            },
            replacement_text: replacement.map(str::to_owned),
            removed,
            edit_revision: None,
            assigned_speaker_id: speaker,
            speaker_assignment: SpeakerAssignmentKind::LocalSpeaker,
        }
    }

    fn speaker(session_id: MeetingSessionId, id: SpeakerId, name: &str) -> MeetingSpeaker {
        MeetingSpeaker {
            speaker_id: id,
            session_id,
            source_kind: SourceKind::Microphone,
            display_name: name.to_owned(),
            revision: 1,
        }
    }

    fn review(title: &str) -> MeetingReviewSnapshot {
        let session_id = MeetingSessionId::new();
        MeetingReviewSnapshot {
            session: MeetingSessionSnapshot {
                session_id,
                phase: MeetingPhase::ReviewReady,
                revision: 4,
                title: title.to_owned(),
                started_at_utc_ms: None,
                elapsed_offset_ns: None,
                sources: Vec::new(),
                open_capture_window_started_at_ns: None,
                capture_completeness: CaptureCompleteness::Partial,
                storage: StorageAvailability::Available,
                processing_status: ProcessingStatus::Failed {
                    reason: ProcessingFailure::LocalModelUnavailable,
                    cause: None,
                },
                preflight_local_processing: None,
                retention_deadline_utc_ms: None,
                transcript_purged_at_utc_ms: None,
                allowed_actions: vec![AllowedMeetingAction::Export],
            },
            tracks: Vec::new(),
            gaps: Vec::new(),
            speakers: Vec::new(),
            transcript: Vec::new(),
            notes: Vec::new(),
            snapshots: Vec::new(),
            artifacts: Vec::new(),
            questions: Vec::new(),
            diarization: MeetingDiarizationSnapshot {
                status: DiarizationStatus::ModelUnavailable,
                model_id: "local-speaker-diarization".to_owned(),
                model_version: "unavailable".to_owned(),
                generation_id: None,
                assigned_segment_count: 0,
            },
            can_export: true,
            remote_cancellation_pending: false,
        }
    }

    /// A meeting with every part filled: generated notes, a note typed in the
    /// moment, and a transcript with a merged turn, an edited line and a
    /// removed one.
    fn full_review() -> MeetingReviewSnapshot {
        let mut review = review("Design\nsync ");
        let session_id = review.session.session_id;
        let (priya, aktan) = (SpeakerId::new(), SpeakerId::new());
        review.speakers = vec![
            speaker(session_id, priya, "Priya"),
            speaker(session_id, aktan, "Aktan"),
        ];
        review.transcript = vec![
            segment(priya, 0, "We ship in May.", None, false),
            segment(
                priya,
                5,
                "The venue is bookd.",
                Some("The venue is booked."),
                false,
            ),
            segment(aktan, 40, "Off the record.", None, true),
            segment(aktan, 65, "Who writes the note?", None, false),
        ];
        review.notes = vec![ManualNote {
            note_id: ManualNoteId::new(),
            session_id,
            start_offset_ns: Some(30 * SECOND),
            end_offset_ns: None,
            body: " Ask about budget ".to_owned(),
            revision: 1,
            created_at_utc_ms: 1,
            updated_at_utc_ms: 1,
        }];
        review.artifacts = vec![revision(
            session_id,
            MeetingArtifactState::Current,
            "We agreed on the May launch.",
        )];
        review
    }

    const USER_NOTES: &str = "Ask Priya about the venue.\r\n\r\nBring the deck.\n";
    const NOTES: &str = concat!(
        r#"{"heading":"Summary","blocks":[{"type":"paragraph","text":"We agreed on the May launch."}]},"#,
        r#"{"heading":"What was covered","blocks":[{"type":"item","text":"Launch date","meta":"Moved to May","time":null}]},"#,
        r#"{"heading":"Decisions","blocks":[{"type":"item","text":"Ship the redesign in May.","meta":null,"time":null}]},"#,
        r#"{"heading":"Action items","blocks":[{"type":"item","text":"Draft the launch note","meta":"Priya · Friday","time":null}]},"#,
        r#"{"heading":"Risks","blocks":[{"type":"item","text":"The venue may fall through.","meta":null,"time":null}]},"#,
        r#"{"heading":"Your own notes","blocks":[{"type":"paragraph","text":"Ask Priya about the venue."},{"type":"paragraph","text":"Bring the deck."}]}"#,
    );
    const MOMENTS: &str = r#"{"heading":"Notes in the moment","blocks":[{"type":"item","text":"Ask about budget","meta":null,"time":"0:30"}]}"#;
    const TRANSCRIPT: &str = concat!(
        r#"{"heading":"Transcript","blocks":["#,
        r#"{"type":"item","text":"We ship in May. The venue is booked.","meta":"Priya","time":"0:00"},"#,
        r#"{"type":"item","text":"Who writes the note?","meta":"Aktan","time":"1:05"}]}"#,
    );

    fn golden(include: CloudBrowserShareInclude, name: &str, sections: &[&str]) {
        let (title, bytes) =
            meeting_document(&full_review(), USER_NOTES, include).expect("document");
        assert_eq!(title, "Design sync");
        assert_eq!(
            String::from_utf8(bytes).expect("utf-8"),
            format!(
                r#"{{"version":1,"title":"Design sync","include":"{name}","notes_out_of_date":false,"sections":[{}]}}"#,
                sections.join(",")
            )
        );
    }

    #[test]
    fn notes_only_carries_the_generated_notes_and_the_typed_notes() {
        golden(CloudBrowserShareInclude::Notes, "notes", &[NOTES]);
    }

    #[test]
    fn notes_and_transcript_adds_the_transcript_without_removed_lines() {
        golden(
            CloudBrowserShareInclude::NotesAndTranscript,
            "notes_and_transcript",
            &[NOTES, TRANSCRIPT],
        );
    }

    #[test]
    fn everything_adds_the_notes_taken_in_the_moment() {
        golden(
            CloudBrowserShareInclude::Everything,
            "everything",
            &[NOTES, MOMENTS, TRANSCRIPT],
        );
    }

    /// Nothing written yet is a valid, empty document, not a failure; notes
    /// read from an out-of-date revision say so.
    #[test]
    fn empty_meeting_is_valid_and_stale_notes_are_flagged() {
        let (title, bytes) =
            meeting_document(&review(" \n "), "", CloudBrowserShareInclude::Everything)
                .expect("empty document");
        assert_eq!(title, FALLBACK_TITLE);
        assert_eq!(
            String::from_utf8(bytes).expect("utf-8"),
            r#"{"version":1,"title":"Sona shared notes","include":"everything","notes_out_of_date":false,"sections":[]}"#
        );

        let mut stale = review("Design sync");
        stale.artifacts = vec![revision(
            stale.session.session_id,
            MeetingArtifactState::OutOfDate,
            "An earlier reading.",
        )];
        let (_, bytes) =
            meeting_document(&stale, "", CloudBrowserShareInclude::Notes).expect("document");
        let document: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(document["notes_out_of_date"], true);
        assert_eq!(
            document["sections"][0]["blocks"][0]["text"],
            "An earlier reading."
        );
    }

    #[test]
    fn overlong_text_splits_under_the_viewer_line_bound() {
        let word = "word ";
        let text = word.repeat(MAX_BLOCK_TEXT_BYTES / word.len() + 10);
        let blocks = paragraphs(&text);
        assert_eq!(blocks.len(), 2);
        for block in &blocks {
            let Block::Paragraph { text } = block else {
                panic!("paragraph expected");
            };
            assert!(text.len() <= MAX_BLOCK_TEXT_BYTES);
            assert!(text.starts_with("word") && text.ends_with("word"));
        }
    }

    #[test]
    fn scratch_note_preserves_paragraphs_without_interpreting_markup() {
        let (title, bytes) =
            scratch_document("  Draft\nnote ", "First line\nnext line\n\n<img src=x>")
                .expect("note document");
        assert_eq!(title, "Draft note");
        assert_eq!(
            String::from_utf8(bytes).expect("utf-8"),
            r#"{"version":1,"title":"Draft note","include":"notes","notes_out_of_date":false,"sections":[{"heading":"Note","blocks":[{"type":"paragraph","text":"First line\nnext line"},{"type":"paragraph","text":"<img src=x>"}]}]}"#
        );
    }
}
