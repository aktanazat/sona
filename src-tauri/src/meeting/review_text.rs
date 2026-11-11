//! A meeting's words as the review page reads them, for every page that
//! carries them out of Sona: the companion share document and the shared
//! meeting pages.

use std::collections::HashMap;

use super::types::{
    MeetingArtifactRevision, MeetingArtifactState, MeetingReviewSnapshot, SpeakerId,
};

/// The notes the review page reads: the newest current revision, else the
/// newest out-of-date one.
pub(crate) fn readable_artifact(
    artifacts: &[MeetingArtifactRevision],
) -> Option<&MeetingArtifactRevision> {
    let newest = |state: MeetingArtifactState| {
        artifacts
            .iter()
            .filter(|artifact| artifact.state == state && artifact.content.is_some())
            .max_by_key(|artifact| artifact.generated_at_utc_ms)
    };
    newest(MeetingArtifactState::Current).or_else(|| newest(MeetingArtifactState::OutOfDate))
}

/// Paragraphs are separated by blank lines; single line breaks stay inside
/// the paragraph.
pub(crate) fn paragraphs(text: &str) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut found = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    for line in normalized.split('\n').chain(std::iter::once("")) {
        if !line.trim().is_empty() {
            paragraph.push(line.trim_end());
            continue;
        }
        if paragraph.is_empty() {
            continue;
        }
        found.push(paragraph.join("\n").trim().to_owned());
        paragraph.clear();
    }
    found
}

/// Each speaker's name as the review page shows it.
pub(crate) fn speaker_names(review: &MeetingReviewSnapshot) -> HashMap<SpeakerId, &str> {
    review
        .speakers
        .iter()
        .map(|speaker| (speaker.speaker_id, speaker.display_name.as_str()))
        .collect()
}

pub(crate) struct TranscriptTurn {
    pub speaker_id: SpeakerId,
    pub start_offset_ns: u64,
    pub text: String,
}

/// Consecutive lines of one speaker read as one turn, as they do on the
/// review page, up to `max_turn_bytes` of text per turn.
pub(crate) fn transcript_turns(
    review: &MeetingReviewSnapshot,
    max_turn_bytes: usize,
) -> Vec<TranscriptTurn> {
    let mut turns: Vec<TranscriptTurn> = Vec::new();
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
            Some(turn)
                if turn.speaker_id == segment.assigned_speaker_id
                    && turn.text.len() + 1 + text.len() <= max_turn_bytes =>
            {
                turn.text.push(' ');
                turn.text.push_str(text);
            }
            _ => turns.push(TranscriptTurn {
                speaker_id: segment.assigned_speaker_id,
                start_offset_ns: segment.base.start_offset_ns,
                text: text.to_owned(),
            }),
        }
    }
    turns
}
