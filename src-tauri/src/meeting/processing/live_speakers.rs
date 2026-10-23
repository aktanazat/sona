use crate::meeting::analytics::MeetingProvisionalSpeaker;
use crate::meeting::diarization::{
    DiarizationError, DiarizedWindow, MeetingDiarizationSession, MeetingDiarizer, SAMPLE_RATE_HZ,
};
use crate::meeting::types::SpeakerAssignmentKind;
use std::ops::Range;
use std::path::Path;

// Live ASR already supplies longer utterances. Three seconds retained returning
// voices across new words in the real-model control; two seconds fragmented them.
const LIVE_SPEAKER_WINDOW_SAMPLES: usize = 3 * SAMPLE_RATE_HZ;

#[derive(Default)]
pub(super) struct LiveSpeakers {
    session: Option<LiveSpeakerSession>,
    spans: Vec<LiveSpeakerSpan>,
}

enum LiveSpeakerSession {
    Local(MeetingDiarizationSession),
    #[cfg(test)]
    Replay(tests::ReplayDiarizer),
}

pub(super) struct LiveSpeakerSpan {
    pub(super) samples: Range<usize>,
    pub(super) speaker: MeetingProvisionalSpeaker,
}

impl LiveSpeakers {
    pub(super) fn prepare(&mut self, diarizer: &MeetingDiarizer, model_directory: &Path) {
        if self.session.is_none() {
            self.session = diarizer
                .wespeaker_fallback(model_directory)
                .and_then(|prepared| diarizer.open(&prepared).ok())
                .map(LiveSpeakerSession::Local);
        }
    }

    pub(super) fn spans(&mut self, samples: &[f32], start_offset_ns: u64) -> &[LiveSpeakerSpan] {
        self.spans.clear();
        if samples.is_empty() {
            return &self.spans;
        }
        let Some(session) = self.session.as_mut() else {
            self.spans.push(LiveSpeakerSpan {
                samples: 0..samples.len(),
                speaker: MeetingProvisionalSpeaker::Unavailable,
            });
            return &self.spans;
        };
        fill_speaker_spans(
            samples,
            start_offset_ns,
            &mut self.spans,
            |audio, start, end| match session {
                LiveSpeakerSession::Local(session) => session
                    .diarize_window(audio, start, end)
                    .map(|result| result.window),
                #[cfg(test)]
                LiveSpeakerSession::Replay(session) => session.diarize(audio, start, end),
            },
        );
        &self.spans
    }
}

pub(super) fn sample_offset_ns(start_offset_ns: u64, samples: usize) -> u64 {
    start_offset_ns.saturating_add(
        u64::try_from(samples)
            .unwrap_or(u64::MAX)
            .saturating_mul(62_500),
    )
}

fn fill_speaker_spans(
    samples: &[f32],
    start_offset_ns: u64,
    spans: &mut Vec<LiveSpeakerSpan>,
    mut diarize: impl FnMut(&[f32], u64, u64) -> Result<DiarizedWindow, DiarizationError>,
) {
    for start in (0..samples.len()).step_by(LIVE_SPEAKER_WINDOW_SAMPLES) {
        let end = samples.len().min(start + LIVE_SPEAKER_WINDOW_SAMPLES);
        let audio = &samples[start..end];
        let speaker = if audio.len() < LIVE_SPEAKER_WINDOW_SAMPLES {
            MeetingProvisionalSpeaker::Unknown
        } else {
            match diarize(
                audio,
                sample_offset_ns(start_offset_ns, start),
                sample_offset_ns(start_offset_ns, end),
            ) {
                Ok(window) => match (window.assignment, window.cluster) {
                    (
                        SpeakerAssignmentKind::SystemSpeaker | SpeakerAssignmentKind::LocalSpeaker,
                        Some(index),
                    ) => MeetingProvisionalSpeaker::Cluster { index },
                    (SpeakerAssignmentKind::Overlap, _) => MeetingProvisionalSpeaker::Uncertain,
                    _ => MeetingProvisionalSpeaker::Unknown,
                },
                Err(DiarizationError::InvalidAudio) => MeetingProvisionalSpeaker::Unknown,
                Err(_) => MeetingProvisionalSpeaker::Unavailable,
            }
        };
        if let Some(previous) = spans.last_mut() {
            if previous.speaker == speaker {
                previous.samples.end = end;
                continue;
            }
        }
        spans.push(LiveSpeakerSpan {
            samples: start..end,
            speaker,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speaker_spans(
        samples: &[f32],
        start_offset_ns: u64,
        diarize: impl FnMut(&[f32], u64, u64) -> Result<DiarizedWindow, DiarizationError>,
    ) -> Vec<LiveSpeakerSpan> {
        let mut spans = Vec::new();
        fill_speaker_spans(samples, start_offset_ns, &mut spans, diarize);
        spans
    }

    #[derive(Default)]
    pub(super) struct ReplayDiarizer {
        voices: Vec<f32>,
    }

    impl ReplayDiarizer {
        pub(super) fn diarize(
            &mut self,
            samples: &[f32],
            start: u64,
            end: u64,
        ) -> Result<DiarizedWindow, DiarizationError> {
            let voice = *samples.first().ok_or(DiarizationError::InvalidAudio)?;
            let index = match self.voices.iter().position(|known| *known == voice) {
                Some(index) => index,
                None => {
                    self.voices.push(voice);
                    self.voices.len() - 1
                }
            };
            Ok(window(
                start,
                end,
                Some(u32::try_from(index).map_err(|_| DiarizationError::InferenceFailed)?),
                SpeakerAssignmentKind::SystemSpeaker,
            ))
        }
    }

    fn window(
        start_offset_ns: u64,
        end_offset_ns: u64,
        cluster: Option<u32>,
        assignment: SpeakerAssignmentKind,
    ) -> DiarizedWindow {
        DiarizedWindow {
            start_offset_ns,
            end_offset_ns,
            cluster,
            assignment,
        }
    }

    #[test]
    fn adjacent_windows_preserve_distinct_remote_clusters_without_copying_audio() {
        let audio: Vec<f32> = (0..LIVE_SPEAKER_WINDOW_SAMPLES * 3)
            .map(|index| {
                if index < LIVE_SPEAKER_WINDOW_SAMPLES {
                    0.1
                } else {
                    0.2
                }
            })
            .collect();
        let spans = speaker_spans(&audio, 5_000_000_000, |samples, start, end| {
            let cluster = u32::from(samples[0] > 0.15);
            let offset = usize::try_from((start - 5_000_000_000) / 62_500)
                .expect("the test audio offset fits usize");
            assert!(std::ptr::eq(samples.as_ptr(), audio[offset..].as_ptr()));
            Ok(window(
                start,
                end,
                Some(cluster),
                SpeakerAssignmentKind::SystemSpeaker,
            ))
        });
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].samples, 0..LIVE_SPEAKER_WINDOW_SAMPLES);
        assert_eq!(
            spans[0].speaker,
            MeetingProvisionalSpeaker::Cluster { index: 0 }
        );
        assert_eq!(spans[1].samples, LIVE_SPEAKER_WINDOW_SAMPLES..audio.len());
        assert_eq!(
            spans[1].speaker,
            MeetingProvisionalSpeaker::Cluster { index: 1 }
        );
    }

    #[test]
    fn embedding_ties_are_uncertain_not_a_claim_of_simultaneous_speech() {
        let audio = vec![0.1; LIVE_SPEAKER_WINDOW_SAMPLES];
        let spans = speaker_spans(&audio, 0, |_, start, end| {
            Ok(window(start, end, None, SpeakerAssignmentKind::Overlap))
        });
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].speaker, MeetingProvisionalSpeaker::Uncertain);
    }

    #[test]
    fn short_speech_does_not_infer_or_borrow_a_previous_speakers_label() {
        for sample_count in [
            SAMPLE_RATE_HZ - 1,
            2 * SAMPLE_RATE_HZ,
            LIVE_SPEAKER_WINDOW_SAMPLES - 1,
        ] {
            let audio = vec![0.1; sample_count];
            let spans = speaker_spans(&audio, 0, |_, _, _| {
                panic!("short speech must not create or update a speaker group")
            });
            assert_eq!(spans.len(), 1);
            assert_eq!(spans[0].samples, 0..sample_count);
            assert_eq!(spans[0].speaker, MeetingProvisionalSpeaker::Unknown);
        }
    }

    #[test]
    fn enough_context_diarizes_once_and_leaves_a_short_tail_unknown() {
        let audio = vec![0.1; LIVE_SPEAKER_WINDOW_SAMPLES + 2 * SAMPLE_RATE_HZ];
        let mut calls = 0;
        let spans = speaker_spans(&audio, 7_000_000_000, |samples, start, end| {
            calls += 1;
            assert_eq!(samples.len(), LIVE_SPEAKER_WINDOW_SAMPLES);
            assert!(std::ptr::eq(samples.as_ptr(), audio.as_ptr()));
            assert_eq!(start, 7_000_000_000);
            assert_eq!(end, 10_000_000_000);
            Ok(window(
                start,
                end,
                Some(0),
                SpeakerAssignmentKind::SystemSpeaker,
            ))
        });
        assert_eq!(calls, 1);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].samples, 0..LIVE_SPEAKER_WINDOW_SAMPLES);
        assert_eq!(
            spans[0].speaker,
            MeetingProvisionalSpeaker::Cluster { index: 0 }
        );
        assert_eq!(spans[1].samples, LIVE_SPEAKER_WINDOW_SAMPLES..audio.len());
        assert_eq!(spans[1].speaker, MeetingProvisionalSpeaker::Unknown);
    }

    #[test]
    fn absent_local_model_does_not_invent_a_speaker() {
        let mut speakers = LiveSpeakers::default();
        let spans = speakers.spans(&vec![0.1; LIVE_SPEAKER_WINDOW_SAMPLES], 0);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].speaker, MeetingProvisionalSpeaker::Unavailable);
    }

    #[test]
    fn failed_inference_does_not_invent_a_speaker() {
        let spans = speaker_spans(&vec![0.1; LIVE_SPEAKER_WINDOW_SAMPLES], 0, |_, _, _| {
            Err(DiarizationError::InferenceFailed)
        });
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].speaker, MeetingProvisionalSpeaker::Unavailable);
    }

    #[test]
    fn a_window_without_a_cluster_remains_unknown() {
        let spans = speaker_spans(
            &vec![0.1; LIVE_SPEAKER_WINDOW_SAMPLES],
            0,
            |_, start, end| {
                Ok(window(
                    start,
                    end,
                    None,
                    SpeakerAssignmentKind::SystemSpeaker,
                ))
            },
        );
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].speaker, MeetingProvisionalSpeaker::Unknown);
    }

    #[test]
    fn refreshed_mic_and_system_snapshots_keep_track_scoped_clusters_and_row_ids() {
        use super::super::{LiveSegment, LiveTranscript};
        use crate::meeting::types::{MeetingSessionId, SourceKind, SourceTrackId};

        let session_id = MeetingSessionId::new();
        let system = SourceTrackId::new();
        let microphone = SourceTrackId::new();
        let live = LiveTranscript::new();
        let append = |track_id, source_kind, index, start_offset_ns, text: &str| {
            live.append(LiveSegment {
                track_id,
                source_kind,
                speaker: MeetingProvisionalSpeaker::Cluster { index },
                start_offset_ns,
                end_offset_ns: start_offset_ns + 2_000_000_000,
                text: text.to_string(),
            });
        };

        append(system, SourceKind::SystemAudio, 0, 0, "first remote voice");
        append(
            system,
            SourceKind::SystemAudio,
            1,
            2_000_000_000,
            "second remote voice",
        );
        let first = live.snapshot(session_id);
        append(
            microphone,
            SourceKind::Microphone,
            0,
            1_000_000_000,
            "microphone voice",
        );
        let refreshed = live.snapshot(session_id);

        assert_eq!(refreshed.segments.len(), 3);
        assert_eq!(refreshed.segments[0], first.segments[0]);
        assert_eq!(refreshed.segments[2], first.segments[1]);
        assert_eq!(refreshed.segments[1].segment_id, 2);
        assert_eq!(refreshed.segments[1].track_id, microphone);
        assert_eq!(refreshed.segments[1].source_kind, SourceKind::Microphone);
        assert_eq!(
            refreshed.segments[1].speaker,
            MeetingProvisionalSpeaker::Cluster { index: 0 }
        );

        append(
            system,
            SourceKind::SystemAudio,
            1,
            5_000_000_000,
            "second remote voice returns",
        );
        let later = live.snapshot(session_id);
        assert_eq!(later.segments[3].track_id, system);
        assert_eq!(later.segments[3].source_kind, SourceKind::SystemAudio);
        assert_eq!(later.segments[3].speaker, first.segments[1].speaker);
        assert_eq!(later.segments[3].segment_id, 3);
        assert_eq!(later.segments[0], first.segments[0]);
        assert_eq!(later.segments[2], first.segments[1]);
    }

    #[test]
    fn returning_remote_speakers_keep_their_labels_when_the_next_pass_starts_with_the_other_voice()
    {
        let directory = tempfile::tempdir().expect("the test owns its model directory");
        let diarizer = MeetingDiarizer::without_download();
        let mut speakers = LiveSpeakers {
            session: Some(LiveSpeakerSession::Replay(ReplayDiarizer::default())),
            spans: Vec::new(),
        };
        let short_audio = vec![0.3; 2 * SAMPLE_RATE_HZ];
        let short = speakers.spans(&short_audio, 0);
        assert_eq!(short.len(), 1);
        assert_eq!(short[0].speaker, MeetingProvisionalSpeaker::Unknown);
        let mut first_audio = vec![0.1; LIVE_SPEAKER_WINDOW_SAMPLES];
        first_audio.extend(vec![0.2; LIVE_SPEAKER_WINDOW_SAMPLES]);
        speakers.prepare(&diarizer, directory.path());
        let first = speakers.spans(&first_audio, 0);
        assert_eq!(first.len(), 2);
        let first_speakers = [first[0].speaker, first[1].speaker];

        let short = speakers.spans(&short_audio, 6_000_000_000);
        assert_eq!(short.len(), 1);
        assert_eq!(short[0].speaker, MeetingProvisionalSpeaker::Unknown);
        let mut next_audio = vec![0.2; LIVE_SPEAKER_WINDOW_SAMPLES];
        next_audio.extend(vec![0.1; LIVE_SPEAKER_WINDOW_SAMPLES]);
        speakers.prepare(&diarizer, directory.path());
        let next = speakers.spans(&next_audio, 8_000_000_000);

        assert_eq!(next.len(), 2);
        assert_eq!(
            first_speakers[0],
            MeetingProvisionalSpeaker::Cluster { index: 0 }
        );
        assert_eq!(
            first_speakers[1],
            MeetingProvisionalSpeaker::Cluster { index: 1 }
        );
        assert_eq!(next[0].speaker, first_speakers[1]);
        assert_eq!(next[1].speaker, first_speakers[0]);
    }
}
