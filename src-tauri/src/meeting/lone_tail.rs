//! Whether a recording kept going after everyone but one voice had stopped
//! talking: the call that ended while Sona still listened, or the room the
//! others walked out of. It only ever offers; the lines leave the transcript
//! when the person cuts them, and come back when they put them back.

use super::types::{
    EffectiveTranscriptSegment, MeetingPhase, MeetingReviewSnapshot, SourceKind, SpeakerId,
    TranscriptSegmentId,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashSet;

/// A tail shorter than this is the goodbye, not a recording left running.
const MIN_TAIL_NS: u64 = 120_000_000_000;

/// `meeting_recording_tail`: the stretch at the end of a recording where
/// only one voice, heard on this Mac's microphone, is left.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingRecordingTail {
    /// Where the tail starts: the end of the last line anyone else said.
    pub from_offset_ns: u64,
    pub recording_end_offset_ns: u64,
    /// The voice still talking in the tail.
    pub remaining_speaker_id: SpeakerId,
    /// The last line someone else said, to hear them leave.
    pub last_other_segment_id: TranscriptSegmentId,
    /// The lines in the tail, oldest first, removed or not.
    pub segment_ids: Vec<TranscriptSegmentId>,
    /// Every line in the tail is already out of the transcript.
    pub cut: bool,
}

pub(crate) fn recording_tail(snapshot: &MeetingReviewSnapshot) -> Option<MeetingRecordingTail> {
    if snapshot.session.phase != MeetingPhase::ReviewReady {
        return None;
    }
    let microphone: HashSet<SpeakerId> = snapshot
        .speakers
        .iter()
        .filter(|speaker| speaker.source_kind == SourceKind::Microphone)
        .map(|speaker| speaker.speaker_id)
        .collect();
    let recording_end = snapshot
        .tracks
        .iter()
        .filter_map(|track| track.last_offset_ns)
        .max()
        .unwrap_or(0);
    tail_of(&snapshot.transcript, &microphone, recording_end)
}

fn tail_of(
    transcript: &[EffectiveTranscriptSegment],
    microphone: &HashSet<SpeakerId>,
    recording_end_ns: u64,
) -> Option<MeetingRecordingTail> {
    let last = transcript
        .iter()
        .max_by_key(|line| (line.base.end_offset_ns, line.base.start_offset_ns))?;
    let remaining = last.assigned_speaker_id;
    if !microphone.contains(&remaining) {
        return None;
    }
    let last_other = transcript
        .iter()
        .filter(|line| line.assigned_speaker_id != remaining)
        .max_by_key(|line| (line.base.end_offset_ns, line.base.start_offset_ns))?;
    let from = last_other.base.end_offset_ns;
    let recording_end = recording_end_ns.max(last.base.end_offset_ns);
    if recording_end.saturating_sub(from) < MIN_TAIL_NS {
        return None;
    }
    let mut tail: Vec<&EffectiveTranscriptSegment> = transcript
        .iter()
        .filter(|line| line.base.start_offset_ns >= from)
        .collect();
    if tail.is_empty() {
        return None;
    }
    tail.sort_by_key(|line| (line.base.start_offset_ns, line.base.ordinal));
    Some(MeetingRecordingTail {
        from_offset_ns: from,
        recording_end_offset_ns: recording_end,
        remaining_speaker_id: remaining,
        last_other_segment_id: last_other.base.segment_id,
        cut: tail.iter().all(|line| line.removed),
        segment_ids: tail.iter().map(|line| line.base.segment_id).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::types::{
        SourceTrackId, SpeakerAssignmentKind, TranscriptRevisionId, TranscriptSegment,
    };

    const SECOND: u64 = 1_000_000_000;

    fn line(speaker: SpeakerId, start_s: u64, end_s: u64) -> EffectiveTranscriptSegment {
        EffectiveTranscriptSegment {
            base: TranscriptSegment {
                segment_id: TranscriptSegmentId::new(),
                transcript_revision_id: TranscriptRevisionId::new(),
                track_id: SourceTrackId::new(),
                ordinal: start_s,
                start_offset_ns: start_s * SECOND,
                end_offset_ns: end_s * SECOND,
                speaker_id: speaker,
                text: "words".into(),
                confidence_milli: None,
            },
            replacement_text: None,
            removed: false,
            edit_revision: None,
            assigned_speaker_id: speaker,
            speaker_assignment: SpeakerAssignmentKind::LocalSpeaker,
        }
    }

    #[test]
    fn a_call_that_ended_while_the_microphone_kept_talking_offers_its_tail() {
        let me = SpeakerId::new();
        let them = SpeakerId::new();
        let transcript = vec![
            line(me, 0, 10),
            line(them, 10, 600),
            line(me, 600, 610),
            line(them, 612, 620),
            line(me, 700, 720),
            line(me, 900, 950),
        ];
        let tail = tail_of(&transcript, &HashSet::from([me]), 1000 * SECOND).expect("tail");
        assert_eq!(tail.from_offset_ns, 620 * SECOND);
        assert_eq!(tail.recording_end_offset_ns, 1000 * SECOND);
        assert_eq!(tail.last_other_segment_id, transcript[3].base.segment_id);
        assert_eq!(
            tail.segment_ids,
            vec![transcript[4].base.segment_id, transcript[5].base.segment_id]
        );
        assert!(!tail.cut);
    }

    #[test]
    fn a_short_goodbye_and_a_far_end_still_talking_offer_nothing() {
        let me = SpeakerId::new();
        let them = SpeakerId::new();
        let goodbye = vec![line(them, 0, 600), line(me, 610, 650)];
        assert_eq!(tail_of(&goodbye, &HashSet::from([me]), 700 * SECOND), None);
        let far_end_last = vec![line(me, 0, 10), line(them, 10, 900)];
        assert_eq!(
            tail_of(&far_end_last, &HashSet::from([me]), 1200 * SECOND),
            None
        );
        let alone = vec![line(me, 0, 10), line(me, 500, 900)];
        assert_eq!(tail_of(&alone, &HashSet::from([me]), 1200 * SECOND), None);
    }

    #[test]
    fn a_cut_tail_stays_found_so_it_can_be_put_back() {
        let me = SpeakerId::new();
        let them = SpeakerId::new();
        let mut transcript = vec![line(them, 0, 300), line(me, 400, 420), line(me, 500, 520)];
        transcript[1].removed = true;
        let partly = tail_of(&transcript, &HashSet::from([me]), 600 * SECOND).expect("tail");
        assert!(!partly.cut);
        transcript[2].removed = true;
        let cut = tail_of(&transcript, &HashSet::from([me]), 600 * SECOND).expect("tail");
        assert!(cut.cut);
        assert_eq!(cut.segment_ids.len(), 2);
    }

    #[test]
    fn cutting_the_tail_and_putting_it_back_keeps_each_lines_words() {
        use crate::meeting::store::{workflow_core_tests, SegmentEdit, StoreMutation};
        use crate::meeting::types::{
            MeetingCommandKind, MeetingOperationId, OperationActor, OperationResult,
        };

        let (_directory, store) = workflow_core_tests::store();
        let session_id = workflow_core_tests::reviewable_meeting(&store, "Call", 1);
        workflow_core_tests::transcript_segments(
            &store,
            session_id,
            &["Thanks, everyone", "Note to self", "Book the room"],
        );
        let mutation = |expected_revision| StoreMutation {
            operation_id: MeetingOperationId::new(),
            actor: OperationActor::User,
            requested_at_utc_ms: 2,
            session_id,
            expected_revision,
            command: MeetingCommandKind::SegmentEdit,
        };
        let lines = |store: &crate::meeting::store::MeetingStore| {
            store
                .review_snapshot(session_id)
                .unwrap()
                .transcript
                .into_iter()
                .map(|line| {
                    let text = line.replacement_text.unwrap_or(line.base.text);
                    (text, line.removed)
                })
                .collect::<Vec<_>>()
        };
        let tail: Vec<_> = store.review_snapshot(session_id).unwrap().transcript[1..]
            .iter()
            .map(|line| line.base.segment_id)
            .collect();
        store
            .edit_segment(SegmentEdit {
                mutation: mutation(0),
                segment_id: tail[0],
                replacement_text: "Note to self: call Dana".to_string(),
                removed: false,
            })
            .unwrap();

        let cut = store
            .set_segments_removed(mutation(1), &tail, true)
            .unwrap();
        assert_eq!(cut.result, OperationResult::Committed);
        assert_eq!(
            lines(&store),
            [
                ("Thanks, everyone".to_string(), false),
                ("Note to self: call Dana".to_string(), true),
                ("Book the room".to_string(), true),
            ]
        );

        let back = store
            .set_segments_removed(mutation(2), &tail, false)
            .unwrap();
        assert_eq!(back.result, OperationResult::Committed);
        assert_eq!(
            lines(&store),
            [
                ("Thanks, everyone".to_string(), false),
                ("Note to self: call Dana".to_string(), false),
                ("Book the room".to_string(), false),
            ]
        );
    }
}
