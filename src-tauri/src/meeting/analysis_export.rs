//! Readable copies of one retained meeting for offline analysis: each lane's
//! audio as the processing pipeline hears it, the meeting's JSON export, and
//! who the voice code thinks every speaker is.

use super::export::{self, ExportDocument};
use super::processing::{record_starts_new_span, LaneResampler};
use super::store::{DurableTrackRecord, MeetingStore, StoreError};
use super::template_types::NotesTemplateChoice;
use super::types::{
    MeetingExportFormat, MeetingSessionId, MeetingTrackSnapshot, SourceEpoch, SourceKind,
};
use crate::audio_toolkit::audio::wav_sample_to_i16;
use hound::{SampleFormat, WavSpec, WavWriter};
use serde::Serialize;
use std::fs::{self, File};
use std::io::{BufWriter, Seek, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

const PIPELINE_RATE_HZ: u32 = 16_000;

#[derive(Debug)]
pub(crate) enum AnalysisExportError {
    Store(StoreError),
    Write(PathBuf),
}

impl From<StoreError> for AnalysisExportError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExportedMeeting {
    pub meeting_id: String,
    pub title: String,
    pub directory: PathBuf,
    /// False when retention deleted the recording; the words are still here.
    pub audio_retained: bool,
    pub lanes: Vec<ExportedLane>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExportedLane {
    pub track_id: String,
    pub source_kind: SourceKind,
    /// 16 kHz mono 16-bit PCM, the pipeline's input.
    pub file: String,
    /// The captured samples at their own rate and channels, written only
    /// when the format never changed during the track.
    pub raw_file: Option<String>,
    pub records: u64,
    pub duration_ms: u64,
    /// Silence put in where the track had no audio.
    pub silence_ms: u64,
    /// Audio dropped where a span started before the previous one ended.
    pub overlap_dropped_ms: u64,
}

/// Write `<root>/<meeting id>/`: `meeting.json`, `speakers.json` and one WAV
/// per track. A file is renamed into place only once it is complete.
pub(crate) fn export_meeting(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    root: &Path,
    notes_template: NotesTemplateChoice,
) -> Result<ExportedMeeting, AnalysisExportError> {
    let review = store.review_snapshot(session_id)?;
    let user_notes = store.user_notes(session_id, notes_template)?;
    let directory = root.join(session_id.uuid().to_string());
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .map_err(|_| AnalysisExportError::Write(directory.clone()))?;

    let meeting_json = export::render(
        MeetingExportFormat::Json,
        &ExportDocument {
            review: &review,
            user_notes: &user_notes.body,
        },
    )
    .map_err(|_| AnalysisExportError::Write(directory.join("meeting.json")))?;
    write_file(&directory.join("meeting.json"), &meeting_json)?;

    let speakers = store.speaker_analysis(session_id)?;
    let speakers_json = serde_json::to_vec_pretty(&speakers)
        .map_err(|_| AnalysisExportError::Write(directory.join("speakers.json")))?;
    write_file(&directory.join("speakers.json"), &speakers_json)?;

    let audio_retained = review.session.audio_purged_at_utc_ms.is_none();
    let mut lanes = Vec::new();
    if audio_retained {
        for track in review
            .tracks
            .iter()
            .filter(|track| track.durable_record_count > 0)
        {
            let stem = lane_stem(track, &review.tracks);
            lanes.push(export_lane(store, session_id, track, &directory, &stem)?);
        }
    }
    Ok(ExportedMeeting {
        meeting_id: session_id.uuid().to_string(),
        title: review.session.title,
        directory,
        audio_retained,
        lanes,
    })
}

fn write_file(path: &Path, contents: &[u8]) -> Result<(), AnalysisExportError> {
    export::write_atomic(path, contents).map_err(|_| AnalysisExportError::Write(path.to_owned()))
}

/// `microphone` or `system_audio`; the track id joins the name only when a
/// lane has more than one track, as a restarted capture leaves.
fn lane_stem(track: &MeetingTrackSnapshot, tracks: &[MeetingTrackSnapshot]) -> String {
    let lane = track.source_kind.as_str();
    let shared = tracks
        .iter()
        .filter(|other| other.source_kind == track.source_kind && other.durable_record_count > 0)
        .count()
        > 1;
    if shared {
        format!("{lane}-{}", track.track_id.uuid())
    } else {
        lane.to_owned()
    }
}

fn export_lane(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    track: &MeetingTrackSnapshot,
    directory: &Path,
    stem: &str,
) -> Result<ExportedLane, AnalysisExportError> {
    let file = format!("{stem}.wav");
    let raw_file = format!("{stem}.raw.wav");
    let path = directory.join(&file);
    let raw_path = directory.join(&raw_file);
    let partial = directory.join(format!(".{file}.partial"));
    let raw_partial = directory.join(format!(".{raw_file}.partial"));
    let write_error = |path: &Path| AnalysisExportError::Write(path.to_owned());

    let mut lane = LaneWriter::new(
        WavWriter::create(&partial, pipeline_spec()).map_err(|_| write_error(&path))?,
    );
    let mut raw: Option<LaneWriter<BufWriter<File>>> = None;
    let mut raw_format_changed = false;
    let mut records = 0_u64;
    let mut previous = SpanCursor::default();
    let mut resampler = LaneResampler::default();
    let visited = store.visit_durable_track_records(session_id, track.track_id, None, |record| {
        records += 1;
        let span_start_ns = previous
            .starts_new_span(&record)
            .then_some(record.start_offset_ns);
        previous.advance(&record)?;
        lane.place(span_start_ns, resampler.resample(&record)?)
            .map_err(|_| StoreError::Io)?;
        if raw_format_changed {
            return Ok(());
        }
        let spec = raw_spec(&record);
        let raw = match raw.as_mut() {
            Some(raw) if raw.spec() != spec => {
                raw_format_changed = true;
                return Ok(());
            }
            Some(raw) => raw,
            None => raw.insert(LaneWriter::new(
                WavWriter::create(&raw_partial, spec).map_err(|_| StoreError::Io)?,
            )),
        };
        raw.place(span_start_ns, &record.samples)
            .map_err(|_| StoreError::Io)
    });
    let summary = lane.finish();
    let raw_kept = !raw_format_changed && raw.is_some_and(|raw| raw.finish().is_ok());
    let finished = visited.map_err(AnalysisExportError::from).and_then(|()| {
        let summary = summary.map_err(|_| write_error(&path))?;
        fs::rename(&partial, &path).map_err(|_| write_error(&path))?;
        if raw_kept {
            fs::rename(&raw_partial, &raw_path).map_err(|_| write_error(&raw_path))?;
        }
        Ok(summary)
    });
    let _ = fs::remove_file(&partial);
    let _ = fs::remove_file(&raw_partial);
    let summary = finished?;
    Ok(ExportedLane {
        track_id: track.track_id.uuid().to_string(),
        source_kind: track.source_kind,
        file,
        raw_file: raw_kept.then_some(raw_file),
        records,
        duration_ms: frames_ms(summary.frames, PIPELINE_RATE_HZ),
        silence_ms: frames_ms(summary.silence_frames, PIPELINE_RATE_HZ),
        overlap_dropped_ms: frames_ms(summary.dropped_frames, PIPELINE_RATE_HZ),
    })
}

/// The previous record, as `record_starts_new_span` compares the next one
/// against it.
#[derive(Default)]
struct SpanCursor {
    end_offset_ns: Option<u64>,
    epoch: Option<SourceEpoch>,
    sequence: Option<u64>,
}

impl SpanCursor {
    fn starts_new_span(&self, record: &DurableTrackRecord) -> bool {
        self.sequence.is_none()
            || record_starts_new_span(self.end_offset_ns, self.epoch, self.sequence, record)
    }

    fn advance(&mut self, record: &DurableTrackRecord) -> Result<(), StoreError> {
        self.end_offset_ns = Some(
            record
                .start_offset_ns
                .checked_add(record.duration_ns)
                .ok_or(StoreError::Corrupt)?,
        );
        self.epoch = Some(record.source_epoch);
        self.sequence = Some(record.sequence);
        Ok(())
    }
}

const fn pipeline_spec() -> WavSpec {
    WavSpec {
        channels: 1,
        sample_rate: PIPELINE_RATE_HZ,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    }
}

const fn raw_spec(record: &DurableTrackRecord) -> WavSpec {
    WavSpec {
        channels: record.format.channels,
        sample_rate: record.format.sample_rate_hz,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    }
}

fn frames_ms(frames: u64, rate_hz: u32) -> u64 {
    frames.saturating_mul(1_000) / u64::from(rate_hz)
}

#[derive(Debug, Eq, PartialEq)]
struct LaneSummary {
    frames: u64,
    silence_frames: u64,
    dropped_frames: u64,
}

/// One lane laid out on the meeting's clock: a span starts at its first
/// record's offset, with silence before it, and the records inside a span
/// follow each other sample for sample, as the pipeline reads them.
struct LaneWriter<W: Write + Seek> {
    writer: WavWriter<W>,
    summary: LaneSummary,
    overlap_frames: u64,
}

impl<W: Write + Seek> LaneWriter<W> {
    fn new(writer: WavWriter<W>) -> Self {
        Self {
            writer,
            summary: LaneSummary {
                frames: 0,
                silence_frames: 0,
                dropped_frames: 0,
            },
            overlap_frames: 0,
        }
    }

    fn spec(&self) -> WavSpec {
        self.writer.spec()
    }

    /// `span_start_ns` is set when this record starts a span; `samples` are
    /// interleaved at the writer's rate and channel count.
    fn place(&mut self, span_start_ns: Option<u64>, samples: &[f32]) -> hound::Result<()> {
        let spec = self.writer.spec();
        let channels = usize::from(spec.channels);
        if let Some(start_ns) = span_start_ns {
            let target =
                u64::try_from(u128::from(start_ns) * u128::from(spec.sample_rate) / 1_000_000_000)
                    .map_err(|_| hound::Error::TooWide)?;
            if target > self.summary.frames {
                let silence = target - self.summary.frames;
                for _ in 0..silence.saturating_mul(u64::from(spec.channels)) {
                    self.write_sample(0.0)?;
                }
                self.summary.silence_frames += silence;
                self.summary.frames = target;
                self.overlap_frames = 0;
            } else {
                self.overlap_frames = self.summary.frames - target;
            }
        }
        let skip_frames = usize::try_from(self.overlap_frames)
            .unwrap_or(usize::MAX)
            .min(samples.len() / channels);
        let dropped_frames = frame_count(skip_frames)?;
        self.overlap_frames -= dropped_frames;
        self.summary.dropped_frames += dropped_frames;
        let kept = &samples[skip_frames * channels..];
        for sample in kept {
            self.write_sample(*sample)?;
        }
        self.summary.frames += frame_count(kept.len() / channels)?;
        Ok(())
    }

    fn write_sample(&mut self, sample: f32) -> hound::Result<()> {
        if self.writer.spec().sample_format == SampleFormat::Float {
            self.writer.write_sample(sample)
        } else {
            self.writer.write_sample(wav_sample_to_i16(sample))
        }
    }

    fn finish(self) -> hound::Result<LaneSummary> {
        self.writer.finalize()?;
        Ok(self.summary)
    }
}

fn frame_count(frames: usize) -> hound::Result<u64> {
    u64::try_from(frames).map_err(|_| hound::Error::TooWide)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn placed(spans: &[(Option<u64>, Vec<f32>)]) -> (Vec<f32>, LaneSummary) {
        let spec = WavSpec {
            channels: 1,
            sample_rate: 1_000,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut bytes = Cursor::new(Vec::new());
        let mut lane = LaneWriter::new(WavWriter::new(&mut bytes, spec).unwrap());
        for (start_ns, samples) in spans {
            lane.place(*start_ns, samples).unwrap();
        }
        let summary = lane.finish().unwrap();
        bytes.set_position(0);
        let samples = hound::WavReader::new(bytes)
            .unwrap()
            .into_samples()
            .collect::<Result<_, _>>()
            .unwrap();
        (samples, summary)
    }

    #[test]
    fn a_span_that_starts_inside_the_last_drops_its_overlap_and_keeps_the_clock() {
        let (samples, summary) = placed(&[
            (Some(2_000_000), vec![1.0; 4]),
            (None, vec![2.0; 2]),
            (Some(6_000_000), vec![3.0; 3]),
            (Some(12_000_000), vec![4.0; 1]),
        ]);

        assert_eq!(
            samples,
            [0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 3.0, 0.0, 0.0, 0.0, 4.0]
        );
        assert_eq!(
            summary,
            LaneSummary {
                frames: 13,
                silence_frames: 5,
                dropped_frames: 2,
            }
        );
    }

    fn overlap_across_records(spec: WavSpec) -> (Vec<u8>, LaneSummary) {
        let second = usize::try_from(spec.sample_rate).unwrap() * usize::from(spec.channels);
        let mut bytes = Cursor::new(Vec::new());
        let mut lane = LaneWriter::new(WavWriter::new(&mut bytes, spec).unwrap());
        for (start, samples) in [
            (Some(0), vec![0.25; second * 10]),
            (Some(8_000_000_000), vec![0.5; second]),
            (None, vec![0.75; second]),
            (None, vec![-0.5; second]),
            (Some(12_000_000_000), vec![0.5; second]),
        ] {
            lane.place(start, &samples).unwrap();
        }
        let summary = lane.finish().unwrap();
        (bytes.into_inner(), summary)
    }

    #[test]
    fn pipeline_wav_keeps_the_clock_when_overlap_spans_several_records() {
        let (bytes, summary) = overlap_across_records(pipeline_spec());
        let samples = hound::WavReader::new(Cursor::new(bytes))
            .unwrap()
            .into_samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(samples.len(), 16_000 * 13);
        assert!(samples[..16_000 * 10].iter().all(|sample| *sample == 8_191));
        assert!(samples[16_000 * 10..16_000 * 11]
            .iter()
            .all(|sample| *sample == -16_383));
        assert!(samples[16_000 * 11..16_000 * 12]
            .iter()
            .all(|sample| *sample == 0));
        assert!(samples[16_000 * 12..]
            .iter()
            .all(|sample| *sample == 16_383));
        assert_eq!(summary.dropped_frames, 16_000 * 2);
        assert_eq!(summary.silence_frames, 16_000);
    }

    #[test]
    fn raw_stereo_wav_keeps_the_clock_when_overlap_spans_several_records() {
        let spec = WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let (bytes, summary) = overlap_across_records(spec);
        let samples = hound::WavReader::new(Cursor::new(bytes))
            .unwrap()
            .into_samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let second = 48_000 * 2;

        assert_eq!(samples.len(), second * 13);
        assert!(samples[..second * 10].iter().all(|sample| *sample == 0.25));
        assert!(samples[second * 10..second * 11]
            .iter()
            .all(|sample| *sample == -0.5));
        assert!(samples[second * 11..second * 12]
            .iter()
            .all(|sample| *sample == 0.0));
        assert!(samples[second * 12..].iter().all(|sample| *sample == 0.5));
        assert_eq!(summary.dropped_frames, 48_000 * 2);
        assert_eq!(summary.silence_frames, 48_000);
    }
}
