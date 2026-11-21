//! Live words for a local model that cannot stream. Voice detection forwards
//! frames only while someone speaks, so a pause in their arrival closes a
//! segment: the segment is decoded once more and its words are kept, while the
//! open segment is redrafted each second. The preview never becomes the
//! dictation; the complete recording is decoded again when it ends.

use std::time::Duration;

/// No forwarded speech for this long closes the open segment. Voice
/// detection's own hangover already holds frames through short gaps.
pub(super) const SEGMENT_GAP: Duration = Duration::from_millis(400);
const DRAFT_STEP_SAMPLES: usize = 16_000;
/// Bounds the cost of a redraft while someone talks without pausing.
const SEGMENT_MAX_SAMPLES: usize = 12 * 16_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Due {
    /// Redraft the open segment; its words may still change.
    Draft,
    /// Decode the open segment for the last time and keep its words.
    Commit,
}

#[derive(Default)]
pub(super) struct ChunkedTranscript {
    committed: String,
    tentative: String,
    segment: Vec<f32>,
    drafted_samples: usize,
}

impl ChunkedTranscript {
    pub(super) fn push(&mut self, pcm: &[f32]) {
        self.segment.extend_from_slice(pcm);
    }

    /// The decode the open segment needs now. `paused` means no speech
    /// arrived for [`SEGMENT_GAP`].
    pub(super) fn due(&self, paused: bool) -> Option<Due> {
        if self.segment.is_empty() {
            None
        } else if paused || self.segment.len() >= SEGMENT_MAX_SAMPLES {
            Some(Due::Commit)
        } else if self.segment.len() - self.drafted_samples >= DRAFT_STEP_SAMPLES {
            Some(Due::Draft)
        } else {
            None
        }
    }

    pub(super) fn segment(&self) -> &[f32] {
        &self.segment
    }

    /// Take the decode of [`Self::segment`]. True when the words shown change.
    pub(super) fn apply(&mut self, due: Due, text: &str) -> bool {
        let text = text.trim();
        match due {
            Due::Draft => {
                self.drafted_samples = self.segment.len();
                if self.tentative == text {
                    return false;
                }
                text.clone_into(&mut self.tentative);
                true
            }
            Due::Commit => {
                if !text.is_empty() {
                    if !self.committed.is_empty() {
                        self.committed.push(' ');
                    }
                    self.committed.push_str(text);
                }
                self.segment.clear();
                self.drafted_samples = 0;
                let had_tentative = !self.tentative.is_empty();
                self.tentative.clear();
                had_tentative || !text.is_empty()
            }
        }
    }

    pub(super) fn committed(&self) -> &str {
        &self.committed
    }

    pub(super) fn tentative(&self) -> &str {
        &self.tentative
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: usize = 16_000;

    #[test]
    fn segments_accumulate_and_drafts_are_replaced_until_a_pause_keeps_them() {
        let mut transcript = ChunkedTranscript::default();
        transcript.push(&vec![0.1; SECOND / 2]);
        assert_eq!(
            transcript.due(false),
            None,
            "half a second is not worth a draft"
        );

        transcript.push(&vec![0.1; SECOND / 2]);
        assert_eq!(transcript.due(false), Some(Due::Draft));
        assert!(transcript.apply(Due::Draft, " hello wor "));
        assert_eq!(
            (transcript.committed(), transcript.tentative()),
            ("", "hello wor")
        );
        assert_eq!(transcript.due(false), None, "the draft covers what arrived");

        transcript.push(&vec![0.1; SECOND]);
        assert!(transcript.apply(Due::Draft, "hello world"));
        assert_eq!(transcript.due(true), Some(Due::Commit));
        assert!(transcript.apply(Due::Commit, "Hello world."));
        assert_eq!(
            (transcript.committed(), transcript.tentative()),
            ("Hello world.", "")
        );
        assert!(transcript.segment().is_empty());
        assert_eq!(
            transcript.due(true),
            None,
            "a pause with nothing open decodes nothing"
        );

        transcript.push(&vec![0.1; SECOND]);
        assert!(transcript.apply(Due::Commit, "How are you?"));
        assert_eq!(transcript.committed(), "Hello world. How are you?");
    }

    #[test]
    fn a_segment_without_words_adds_no_space() {
        let mut transcript = ChunkedTranscript::default();
        transcript.push(&vec![0.1; SECOND]);
        transcript.apply(Due::Commit, "One.");
        transcript.push(&vec![0.1; SECOND]);
        assert!(!transcript.apply(Due::Commit, "  "));
        transcript.push(&vec![0.1; SECOND]);
        transcript.apply(Due::Commit, "Two.");
        assert_eq!(transcript.committed(), "One. Two.");
    }

    #[test]
    fn talking_without_a_pause_keeps_a_bounded_segment() {
        let mut transcript = ChunkedTranscript::default();
        transcript.push(&vec![0.1; SEGMENT_MAX_SAMPLES]);
        assert_eq!(transcript.due(false), Some(Due::Commit));
    }
}
