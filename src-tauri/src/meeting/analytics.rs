//! Conversation metrics and keyword trackers over a diarized meeting
//! transcript, plus the user-owned notes layer that steers generated notes.
//!
//! Everything here is arithmetic and substring matching over data the meeting
//! pipeline already produced. No capture surface, no model, no network. The
//! store keeps a derived copy so a finished meeting can be read back without
//! re-deriving, but the transcript remains the only source of truth: every
//! value below can be recomputed from it at any time.

use super::types::{MeetingArtifactId, MeetingSessionId, SpeakerId, TranscriptSegmentId};
use serde::{Deserialize, Serialize};
use specta::Type;

/// A speaker holds the floor across consecutive utterances. A silence longer
/// than this ends the run even when nobody else speaks, so a long pause never
/// inflates a monologue into "still talking".
const MONOLOGUE_MAX_GAP_NS: u64 = 2_000_000_000;

/// One diarized utterance reduced to what the metrics need. Callers build
/// these from the effective transcript, which is where edits and speaker
/// reassignment have already been applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalyticsSegment {
    pub segment_id: TranscriptSegmentId,
    pub speaker_id: SpeakerId,
    pub start_offset_ns: u64,
    pub end_offset_ns: u64,
    pub text: String,
}

/// One speaker's airtime. `share_permille` is apportioned by largest remainder
/// so the displayed shares always add up to 100%.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SpeakerTalkShare {
    pub speaker_id: SpeakerId,
    pub speaking_ns: u64,
    pub share_permille: u32,
    pub turn_count: u32,
    pub longest_monologue_ns: u64,
}

/// Per-meeting conversation shape. Empty transcripts produce zeros and `None`
/// medians rather than invented values.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingTalkMetrics {
    pub segment_count: u32,
    pub turn_count: u32,
    /// Adjacent turns whose speaker differs: how often the floor changed hands.
    pub interaction_count: u32,
    pub total_speaking_ns: u64,
    pub speakers: Vec<SpeakerTalkShare>,
    pub longest_monologue_ns: u64,
    pub longest_monologue_speaker_id: Option<SpeakerId>,
    /// Median silence left between one speaker finishing and the next starting.
    /// `None` when the floor never changed hands.
    pub median_switch_gap_ms: Option<u64>,
}

/// A user-authored watch list. Patterns are literal phrases, matched
/// case-insensitively; they are never compiled as regular expressions, so a
/// stray bracket in a phrase can neither fail nor match something surprising.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct KeywordTracker {
    pub name: String,
    pub patterns: Vec<String>,
}

/// What one tracker found, with the segments that carry the evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct TrackerResult {
    pub name: String,
    pub hit_count: u32,
    pub segment_ids: Vec<TranscriptSegmentId>,
}

/// The derived, disposable part of a meeting's analytics: everything that can
/// be rebuilt from the transcript alone.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingAnalytics {
    pub talk: MeetingTalkMetrics,
    pub trackers: Vec<TrackerResult>,
}

/// Which shape the generated notes should take. The stored id is stable
/// because it is hashed into an artifact's generation key; `General` keeps the
/// original `meeting-review` id so pre-template artifacts stay addressable.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingNotesTemplate {
    #[default]
    General,
    OneOnOne,
    Interview,
    SalesCall,
    Standup,
}

impl MeetingNotesTemplate {
    pub const ALL: [Self; 5] = [
        Self::General,
        Self::OneOnOne,
        Self::Interview,
        Self::SalesCall,
        Self::Standup,
    ];

    /// The persisted artifact template id, also hashed into generation keys.
    pub const fn artifact_template_id(self) -> &'static str {
        match self {
            Self::General => "meeting-review",
            Self::OneOnOne => "meeting-one-on-one",
            Self::Interview => "meeting-interview",
            Self::SalesCall => "meeting-sales-call",
            Self::Standup => "meeting-standup",
        }
    }

    pub fn from_artifact_template_id(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|template| template.artifact_template_id() == value)
    }

    /// The template's name as a reader sees it, on an exported page.
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "Meeting",
            Self::OneOnOne => "One-to-one",
            Self::Interview => "Interview",
            Self::SalesCall => "Sales call",
            Self::Standup => "Standup",
        }
    }

    /// One sentence appended to the notes system prompt. It only asks for a
    /// different emphasis and section order; the output schema never changes.
    pub const fn steering(self) -> &'static str {
        match self {
            Self::General => {
                "Shape the notes as a general meeting record: what was discussed, what was decided, what happens next."
            }
            Self::OneOnOne => {
                "Shape the notes as a one-to-one record: personal updates, blockers raised, feedback given in both directions, and commitments each person made."
            }
            Self::Interview => {
                "Shape the notes as an interview record: the questions asked, the candidate's answers and concrete examples, and stated strengths and gaps. Do not score the candidate."
            }
            Self::SalesCall => {
                "Shape the notes as a sales-call record: the buyer's stated needs, objections and pricing pushback, competitors named, and agreed next steps with owners."
            }
            Self::Standup => {
                "Shape the notes as a standup record: per person, what moved, what is next, and what is blocked. Keep every entry to one line."
            }
        }
    }
}

/// The user's own layer on a meeting: rough notes typed while it runs and the
/// template those notes should be shaped into.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingUserNotes {
    pub session_id: MeetingSessionId,
    pub body: String,
    pub template: MeetingNotesTemplate,
    /// Bumped on every save; a save must supply the revision it is replacing.
    pub revision: u64,
    pub updated_at_utc_ms: i64,
}

impl MeetingUserNotes {
    /// A meeting with no saved notes still has a notes layer: an empty body at
    /// revision 0 under the supplied default template.
    pub fn empty(session_id: MeetingSessionId, template: MeetingNotesTemplate) -> Self {
        Self {
            session_id,
            body: String::new(),
            template,
            revision: 0,
            updated_at_utc_ms: 0,
        }
    }
}

/// Whether one extracted action item has been ticked off. State belongs to the
/// generated revision that produced the item, so regenerating notes starts a
/// fresh list rather than carrying ticks onto text nobody checked.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingActionItemState {
    pub artifact_id: MeetingArtifactId,
    pub action_index: u32,
    pub done: bool,
}

/// Everything the meeting review surface needs from this module in one read.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingAnalyticsSnapshot {
    pub session_id: MeetingSessionId,
    pub input_revision: u64,
    pub computed_at_utc_ms: i64,
    pub analytics: MeetingAnalytics,
    pub action_items: Vec<MeetingActionItemState>,
    pub notes: MeetingUserNotes,
}

/// Why a catch-up request produced what it did. `NoTranscriptYet` is the
/// answer while there is nothing to read: before the provisional pass over a
/// running capture has recognized any speech, and for a finished meeting whose
/// transcript is empty.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingCatchUpState {
    Ready,
    NoTranscriptYet,
    ModelUnavailable,
    Failed,
}

/// A short recap of the transcript captured so far.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingCatchUp {
    pub state: MeetingCatchUpState,
    pub bullets: Vec<String>,
    pub through_offset_ns: Option<u64>,
    pub segment_count: u32,
    /// Read from the provisional transcript of a capture that is still
    /// running, rather than from a stored transcript revision. The recap is as
    /// of `through_offset_ns` and the words behind it were recognized without
    /// the second pass — diarization, edits, speaker names — that a stored
    /// revision has had.
    pub provisional: bool,
}

impl MeetingCatchUp {
    pub fn empty(state: MeetingCatchUpState, segment_count: u32, provisional: bool) -> Self {
        Self {
            state,
            bullets: Vec::new(),
            through_offset_ns: None,
            segment_count,
            provisional,
        }
    }
}

/// One utterance recognized while the capture was still running. No segment
/// id, no speaker and no revision: those belong to the stored transcript the
/// post-stop pass writes, and this reading is thrown away when capture ends.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingProvisionalSegment {
    pub start_offset_ns: u64,
    pub end_offset_ns: u64,
    pub text: String,
}

/// The words a running capture has recognized so far, in start order. Empty
/// for a meeting that is not capturing now: after the stop the stored
/// revision is the only transcript there is.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingProvisionalTranscript {
    pub session_id: MeetingSessionId,
    pub segments: Vec<MeetingProvisionalSegment>,
}

/// The most bullets a catch-up ever returns, and the number the prompt asks
/// for. Anything longer stops being a catch-up.
pub const CATCH_UP_MAX_BULLETS: usize = 6;

/// One continuous stretch of one speaker holding the floor, with the words
/// spoken in it counted. Every airtime number in the app and on an exported
/// ledger page is counted from this list, and it is built in exactly one
/// place — [`merge_turns`] — so the two can never disagree.
pub(crate) struct Turn {
    pub speaker_id: SpeakerId,
    pub start_offset_ns: u64,
    pub end_offset_ns: u64,
    pub word_count: u32,
}

/// Segments in start order. Both readers of the turn list need the same order,
/// so they sort once and share it.
fn in_start_order(segments: &[AnalyticsSegment]) -> Vec<&AnalyticsSegment> {
    let mut ordered: Vec<&AnalyticsSegment> = segments.iter().collect();
    ordered.sort_by_key(|segment| (segment.start_offset_ns, segment.end_offset_ns));
    ordered
}

/// Words in one utterance: whitespace-separated tokens, which is what `awk`
/// counts and what a reader would count by hand.
fn word_count(text: &str) -> u32 {
    u32::try_from(text.split_whitespace().count()).unwrap_or(u32::MAX)
}

/// Consecutive utterances by one speaker, merged into turns. A silence longer
/// than [`MONOLOGUE_MAX_GAP_NS`] ends the run even when nobody else speaks, so
/// a long pause never inflates a monologue into "still talking".
pub(crate) fn merge_turns(segments: &[AnalyticsSegment]) -> Vec<Turn> {
    merge_ordered_turns(&in_start_order(segments))
}

fn merge_ordered_turns(ordered: &[&AnalyticsSegment]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    for segment in ordered {
        let end_offset_ns = segment.end_offset_ns.max(segment.start_offset_ns);
        let words = word_count(&segment.text);
        let continues = turns.last().is_some_and(|turn| {
            turn.speaker_id == segment.speaker_id
                && segment.start_offset_ns
                    <= turn.end_offset_ns.saturating_add(MONOLOGUE_MAX_GAP_NS)
        });
        match turns.last_mut() {
            Some(turn) if continues => {
                turn.end_offset_ns = turn.end_offset_ns.max(end_offset_ns);
                turn.word_count = turn.word_count.saturating_add(words);
            }
            _ => turns.push(Turn {
                speaker_id: segment.speaker_id,
                start_offset_ns: segment.start_offset_ns,
                end_offset_ns,
                word_count: words,
            }),
        }
    }
    turns
}

/// Airtime, turn-taking and monologue length over a diarized transcript.
///
/// Segments are read in start order. A segment whose end precedes its start
/// counts as zero-length, and overlapping segments contribute their own
/// duration each, because two people talking at once is two people talking:
/// shares are of total speech, not of wall-clock time.
pub fn talk_metrics(segments: &[AnalyticsSegment]) -> MeetingTalkMetrics {
    let ordered = in_start_order(segments);

    let mut shares: Vec<SpeakerTalkShare> = Vec::new();
    let mut total_speaking_ns = 0_u64;

    for segment in &ordered {
        let end_offset_ns = segment.end_offset_ns.max(segment.start_offset_ns);
        let duration_ns = end_offset_ns - segment.start_offset_ns;
        total_speaking_ns = total_speaking_ns.saturating_add(duration_ns);

        match shares
            .iter_mut()
            .find(|share| share.speaker_id == segment.speaker_id)
        {
            Some(share) => share.speaking_ns = share.speaking_ns.saturating_add(duration_ns),
            None => shares.push(SpeakerTalkShare {
                speaker_id: segment.speaker_id,
                speaking_ns: duration_ns,
                share_permille: 0,
                turn_count: 0,
                longest_monologue_ns: 0,
            }),
        }
    }

    let turns = merge_ordered_turns(&ordered);

    let mut longest_monologue_ns = 0_u64;
    let mut longest_monologue_speaker_id = None;
    for turn in &turns {
        let length_ns = turn.end_offset_ns - turn.start_offset_ns;
        if let Some(share) = shares
            .iter_mut()
            .find(|share| share.speaker_id == turn.speaker_id)
        {
            share.turn_count = share.turn_count.saturating_add(1);
            share.longest_monologue_ns = share.longest_monologue_ns.max(length_ns);
        }
        if length_ns > longest_monologue_ns {
            longest_monologue_ns = length_ns;
            longest_monologue_speaker_id = Some(turn.speaker_id);
        }
    }

    let mut switch_gaps_ms: Vec<u64> = Vec::new();
    for pair in turns.windows(2) {
        let [previous, next] = pair else { continue };
        if previous.speaker_id == next.speaker_id {
            continue;
        }
        let gap_ns = next.start_offset_ns.saturating_sub(previous.end_offset_ns);
        switch_gaps_ms.push(gap_ns / 1_000_000);
    }

    apportion_permille(&mut shares, total_speaking_ns);
    shares.sort_by(|left, right| {
        right
            .speaking_ns
            .cmp(&left.speaking_ns)
            .then_with(|| left.speaker_id.uuid().cmp(&right.speaker_id.uuid()))
    });

    MeetingTalkMetrics {
        segment_count: u32::try_from(segments.len()).unwrap_or(u32::MAX),
        turn_count: u32::try_from(turns.len()).unwrap_or(u32::MAX),
        interaction_count: u32::try_from(switch_gaps_ms.len()).unwrap_or(u32::MAX),
        total_speaking_ns,
        speakers: shares,
        longest_monologue_ns,
        longest_monologue_speaker_id,
        median_switch_gap_ms: median(&mut switch_gaps_ms),
    }
}

/// Scan the transcript for every tracker. A tracker with no usable pattern
/// still reports, with zero hits, so the user can see it is watching nothing.
pub fn tracker_results(
    trackers: &[KeywordTracker],
    segments: &[AnalyticsSegment],
) -> Vec<TrackerResult> {
    if trackers.is_empty() {
        return Vec::new();
    }
    let folded: Vec<(TranscriptSegmentId, String)> = segments
        .iter()
        .map(|segment| (segment.segment_id, segment.text.to_lowercase()))
        .collect();

    trackers
        .iter()
        .map(|tracker| {
            let patterns: Vec<String> = tracker
                .patterns
                .iter()
                .map(|pattern| pattern.trim().to_lowercase())
                .filter(|pattern| !pattern.is_empty())
                .collect();
            let mut hit_count = 0_u32;
            let mut segment_ids = Vec::new();
            for (segment_id, text) in &folded {
                let hits: u32 = patterns
                    .iter()
                    .map(|pattern| count_occurrences(text, pattern))
                    .sum();
                if hits > 0 {
                    hit_count = hit_count.saturating_add(hits);
                    segment_ids.push(*segment_id);
                }
            }
            TrackerResult {
                name: tracker.name.clone(),
                hit_count,
                segment_ids,
            }
        })
        .collect()
}

/// Non-overlapping literal occurrences of `needle` in `haystack`. Both are
/// expected to be already case-folded by the caller.
fn count_occurrences(haystack: &str, needle: &str) -> u32 {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0_u32;
    let mut rest = haystack;
    while let Some(index) = rest.find(needle) {
        count = count.saturating_add(1);
        rest = &rest[index + needle.len()..];
    }
    count
}

/// Largest-remainder apportionment, so the shares a person reads add to 100%
/// instead of to 99.8% after truncation.
fn apportion_permille(shares: &mut [SpeakerTalkShare], total_speaking_ns: u64) {
    if total_speaking_ns == 0 {
        return;
    }
    let total = u128::from(total_speaking_ns);
    let mut remainders: Vec<(u128, usize)> = Vec::with_capacity(shares.len());
    let mut assigned = 0_u32;
    for (index, share) in shares.iter_mut().enumerate() {
        let scaled = u128::from(share.speaking_ns) * 1_000;
        let quota = u32::try_from(scaled / total).unwrap_or(1_000);
        share.share_permille = quota;
        assigned = assigned.saturating_add(quota);
        remainders.push((scaled % total, index));
    }
    remainders.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let mut leftover = 1_000_u32.saturating_sub(assigned);
    for (_, index) in remainders {
        if leftover == 0 {
            break;
        }
        shares[index].share_permille += 1;
        leftover -= 1;
    }
}

/// Median of an unsorted sample; the mean of the two middles when even.
fn median(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        return Some(values[middle]);
    }
    Some((values[middle - 1] + values[middle]) / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND_NS: u64 = 1_000_000_000;

    fn segment(
        speaker_id: SpeakerId,
        start_seconds: u64,
        end_seconds: u64,
        text: &str,
    ) -> AnalyticsSegment {
        AnalyticsSegment {
            segment_id: TranscriptSegmentId::new(),
            speaker_id,
            start_offset_ns: start_seconds * SECOND_NS,
            end_offset_ns: end_seconds * SECOND_NS,
            text: text.to_string(),
        }
    }

    fn share_for(metrics: &MeetingTalkMetrics, speaker_id: SpeakerId) -> SpeakerTalkShare {
        *metrics
            .speakers
            .iter()
            .find(|share| share.speaker_id == speaker_id)
            .expect("speaker present in metrics")
    }

    #[test]
    fn empty_transcript_reports_no_conversation() {
        let metrics = talk_metrics(&[]);

        assert_eq!(metrics.segment_count, 0);
        assert_eq!(metrics.turn_count, 0);
        assert_eq!(metrics.interaction_count, 0);
        assert_eq!(metrics.total_speaking_ns, 0);
        assert!(metrics.speakers.is_empty());
        assert_eq!(metrics.longest_monologue_ns, 0);
        assert_eq!(metrics.longest_monologue_speaker_id, None);
        assert_eq!(metrics.median_switch_gap_ms, None);
    }

    #[test]
    fn single_speaker_holds_the_whole_share_and_never_interacts() {
        let speaker = SpeakerId::new();
        let metrics = talk_metrics(&[
            segment(speaker, 0, 10, "opening"),
            segment(speaker, 11, 20, "still going"),
        ]);

        assert_eq!(metrics.speakers.len(), 1);
        assert_eq!(metrics.speakers[0].share_permille, 1_000);
        assert_eq!(metrics.total_speaking_ns, 19 * SECOND_NS);
        assert_eq!(metrics.interaction_count, 0);
        assert_eq!(metrics.median_switch_gap_ms, None);
        // One second of silence keeps the floor, so both segments are one run.
        assert_eq!(metrics.turn_count, 1);
        assert_eq!(metrics.longest_monologue_ns, 20 * SECOND_NS);
        assert_eq!(metrics.longest_monologue_speaker_id, Some(speaker));
    }

    #[test]
    fn a_long_silence_ends_a_monologue_even_without_another_speaker() {
        let speaker = SpeakerId::new();
        let metrics = talk_metrics(&[
            segment(speaker, 0, 10, "first"),
            segment(speaker, 60, 65, "much later"),
        ]);

        assert_eq!(metrics.turn_count, 2);
        assert_eq!(metrics.interaction_count, 0);
        assert_eq!(metrics.longest_monologue_ns, 10 * SECOND_NS);
    }

    #[test]
    fn two_speakers_split_airtime_and_report_switch_gaps() {
        let rep = SpeakerId::new();
        let buyer = SpeakerId::new();
        let metrics = talk_metrics(&[
            segment(rep, 0, 40, "pitch"),
            segment(buyer, 41, 100, "the long answer"),
            segment(rep, 103, 120, "follow up"),
        ]);

        assert_eq!(metrics.turn_count, 3);
        assert_eq!(metrics.interaction_count, 2);
        assert_eq!(metrics.total_speaking_ns, 116 * SECOND_NS);
        assert_eq!(share_for(&metrics, rep).speaking_ns, 57 * SECOND_NS);
        assert_eq!(share_for(&metrics, buyer).speaking_ns, 59 * SECOND_NS);
        // 57/116 and 59/116 truncate to 491 and 508; the remainder is apportioned.
        assert_eq!(
            share_for(&metrics, rep).share_permille + share_for(&metrics, buyer).share_permille,
            1_000
        );
        assert_eq!(metrics.longest_monologue_ns, 59 * SECOND_NS);
        assert_eq!(metrics.longest_monologue_speaker_id, Some(buyer));
        // Gaps of 1s and 3s.
        assert_eq!(metrics.median_switch_gap_ms, Some(2_000));
    }

    #[test]
    fn median_switch_gap_takes_the_middle_of_an_odd_sample() {
        let first = SpeakerId::new();
        let second = SpeakerId::new();
        let metrics = talk_metrics(&[
            segment(first, 0, 10, "a"),
            segment(second, 20, 25, "b"),
            segment(first, 40, 45, "c"),
            segment(second, 46, 50, "d"),
        ]);

        // Switch gaps: 10s, 15s, 1s.
        assert_eq!(metrics.interaction_count, 3);
        assert_eq!(metrics.median_switch_gap_ms, Some(10_000));
    }

    #[test]
    fn overlapping_speech_clamps_to_a_zero_gap_and_keeps_both_speakers_airtime() {
        let first = SpeakerId::new();
        let second = SpeakerId::new();
        let metrics = talk_metrics(&[
            segment(first, 0, 30, "talking over"),
            segment(second, 20, 40, "interrupting"),
        ]);

        assert_eq!(metrics.total_speaking_ns, 50 * SECOND_NS);
        assert_eq!(metrics.interaction_count, 1);
        assert_eq!(metrics.median_switch_gap_ms, Some(0));
        assert_eq!(share_for(&metrics, first).speaking_ns, 30 * SECOND_NS);
        assert_eq!(share_for(&metrics, second).speaking_ns, 20 * SECOND_NS);
    }

    #[test]
    fn a_segment_ending_before_it_starts_contributes_nothing() {
        let speaker = SpeakerId::new();
        let mut inverted = segment(speaker, 10, 10, "corrupt");
        inverted.end_offset_ns = 5 * SECOND_NS;
        let metrics = talk_metrics(&[inverted, segment(speaker, 20, 30, "real")]);

        assert_eq!(metrics.total_speaking_ns, 10 * SECOND_NS);
        assert_eq!(metrics.longest_monologue_ns, 10 * SECOND_NS);
    }

    #[test]
    fn unordered_input_is_read_in_time_order() {
        let first = SpeakerId::new();
        let second = SpeakerId::new();
        let ordered = talk_metrics(&[
            segment(first, 0, 10, "a"),
            segment(second, 12, 20, "b"),
            segment(first, 25, 30, "c"),
        ]);
        let shuffled = talk_metrics(&[
            segment(first, 25, 30, "c"),
            segment(first, 0, 10, "a"),
            segment(second, 12, 20, "b"),
        ]);

        assert_eq!(ordered.turn_count, shuffled.turn_count);
        assert_eq!(ordered.interaction_count, shuffled.interaction_count);
        assert_eq!(ordered.median_switch_gap_ms, shuffled.median_switch_gap_ms);
        assert_eq!(ordered.longest_monologue_ns, shuffled.longest_monologue_ns);
    }

    #[test]
    fn zero_length_speech_leaves_shares_unapportioned() {
        let speaker = SpeakerId::new();
        let metrics = talk_metrics(&[segment(speaker, 5, 5, "silent")]);

        assert_eq!(metrics.total_speaking_ns, 0);
        assert_eq!(metrics.speakers.len(), 1);
        assert_eq!(metrics.speakers[0].share_permille, 0);
        assert_eq!(metrics.longest_monologue_ns, 0);
    }

    #[test]
    fn three_way_shares_still_add_to_one_thousand() {
        let first = SpeakerId::new();
        let second = SpeakerId::new();
        let third = SpeakerId::new();
        let metrics = talk_metrics(&[
            segment(first, 0, 10, "a"),
            segment(second, 30, 40, "b"),
            segment(third, 60, 70, "c"),
        ]);

        let total: u32 = metrics
            .speakers
            .iter()
            .map(|share| share.share_permille)
            .sum();
        assert_eq!(total, 1_000);
    }

    #[test]
    fn trackers_match_literally_and_case_insensitively() {
        let speaker = SpeakerId::new();
        let segments = vec![
            segment(speaker, 0, 5, "Can you do better on the Best Price?"),
            segment(speaker, 6, 10, "No discount this quarter."),
            segment(speaker, 11, 15, "Nothing relevant here."),
        ];
        let trackers = vec![KeywordTracker {
            name: "Pricing".to_string(),
            patterns: vec!["best price".to_string(), "DISCOUNT".to_string()],
        }];

        let results = tracker_results(&trackers, &segments);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].hit_count, 2);
        assert_eq!(results[0].segment_ids.len(), 2);
        assert_eq!(results[0].segment_ids[0], segments[0].segment_id);
        assert_eq!(results[0].segment_ids[1], segments[1].segment_id);
    }

    #[test]
    fn tracker_patterns_are_never_regular_expressions() {
        let speaker = SpeakerId::new();
        let segments = vec![
            segment(speaker, 0, 5, "the price is 10% (final)"),
            segment(speaker, 6, 10, "anything at all"),
        ];
        let trackers = vec![KeywordTracker {
            name: "Literal".to_string(),
            patterns: vec!["(final)".to_string(), ".*".to_string()],
        }];

        let results = tracker_results(&trackers, &segments);

        assert_eq!(results[0].hit_count, 1);
        assert_eq!(results[0].segment_ids, vec![segments[0].segment_id]);
    }

    #[test]
    fn repeated_phrases_count_once_per_non_overlapping_occurrence() {
        let speaker = SpeakerId::new();
        let segments = vec![segment(speaker, 0, 5, "aaaa")];
        let trackers = vec![KeywordTracker {
            name: "Repeat".to_string(),
            patterns: vec!["aa".to_string()],
        }];

        assert_eq!(tracker_results(&trackers, &segments)[0].hit_count, 2);
    }

    #[test]
    fn a_tracker_with_only_blank_patterns_reports_zero_hits() {
        let speaker = SpeakerId::new();
        let segments = vec![segment(speaker, 0, 5, "anything")];
        let trackers = vec![KeywordTracker {
            name: "Blank".to_string(),
            patterns: vec![String::new(), "   ".to_string()],
        }];

        let results = tracker_results(&trackers, &segments);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].hit_count, 0);
        assert!(results[0].segment_ids.is_empty());
    }

    #[test]
    fn trackers_over_an_empty_transcript_report_nothing_found() {
        let trackers = vec![KeywordTracker {
            name: "Pricing".to_string(),
            patterns: vec!["discount".to_string()],
        }];

        let results = tracker_results(&trackers, &[]);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].hit_count, 0);
    }

    #[test]
    fn template_ids_round_trip_and_general_keeps_the_original_id() {
        assert_eq!(
            MeetingNotesTemplate::General.artifact_template_id(),
            "meeting-review"
        );
        for template in MeetingNotesTemplate::ALL {
            assert_eq!(
                MeetingNotesTemplate::from_artifact_template_id(template.artifact_template_id()),
                Some(template)
            );
        }
        assert_eq!(
            MeetingNotesTemplate::from_artifact_template_id("nope"),
            None
        );
    }
}
