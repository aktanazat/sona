//! Prints of real voices, built the way a meeting builds them: Sortformer's
//! exclusive spans, the meeting voice detector, two-second diarization
//! windows and the primary collector's WeSpeaker candidates. Each print is
//! credited to the person whose turns its spans cover most, and the prints
//! are written to `prints.jsonl` beside the meetings.
//!
//! The directory holds `truth.json` and one 16 kHz mono WAV per meeting, as
//! `scripts/voice_probe_meetings.py` writes them from LibriSpeech dev-clean.
//!
//! SONA_VOICE_PROBE_DIR=<dir> SONA_SORTFORMER_MODEL=<gguf> \
//! SONA_WESPEAKER_MODEL=<onnx> <test binary> --ignored --exact \
//!     meeting::voice_identity::probe::voice_prints_probe --nocapture
//!
//! Then `same_voice_probe` runs the label sheet's grouping over those prints:
//!
//! SONA_VOICE_PROBE_DIR=<dir> <test binary> --ignored --exact \
//!     meeting::voice_identity::probe::same_voice_probe --nocapture

use super::*;
use crate::audio_toolkit::vad::open_detector;
use crate::meeting::diarization::{
    pair_same_voices, MeetingDiarizer, PreparedDiarizationModel, SpeakerEmbedding,
};
use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DIARIZATION_WINDOW_SAMPLES: usize = 2 * SAMPLE_RATE_HZ;
const DIARIZATION_MIN_VOICED_FRAMES: u32 = 10;
const MEETING_VAD_THRESHOLD: f32 = 0.55;
const MEETING_SILERO_FALLBACK_THRESHOLD: f32 = 0.5;

#[derive(Deserialize)]
struct Truth {
    genders: HashMap<String, String>,
    meetings: Vec<TruthMeeting>,
}

#[derive(Deserialize)]
struct TruthMeeting {
    id: String,
    speakers: Vec<String>,
    preset: String,
    turns: Vec<Turn>,
}

#[derive(Deserialize)]
struct Turn {
    speaker: String,
    start_s: f64,
    end_s: f64,
}

fn env_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} names the probe's input"))
}

fn voice_frames(samples: &[f32]) -> anyhow::Result<Vec<bool>> {
    let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/models");
    let mut detector = open_detector(
        &models.join("ten-vad.onnx"),
        MEETING_VAD_THRESHOLD,
        &models.join("silero_vad_v4.onnx"),
        MEETING_SILERO_FALLBACK_THRESHOLD,
    )?;
    samples
        .chunks_exact(VAD_FRAME_SAMPLES)
        .map(|frame| detector.is_voice(frame))
        .collect()
}

/// `DiarizationWindower` in `processing.rs`: a window opens on a voiced frame,
/// closes at two seconds, and is kept only with ten voiced frames.
fn observe_windows(collector: &mut VoiceIdentityCollector, samples: &[f32], voice: &[bool]) {
    let mut current: Vec<f32> = Vec::with_capacity(DIARIZATION_WINDOW_SAMPLES);
    let mut frames: Vec<bool> = Vec::new();
    let mut start: Option<u64> = None;
    let mut voiced = 0_u32;
    let mut finish = |current: &mut Vec<f32>,
                      frames: &mut Vec<bool>,
                      start: &mut Option<u64>,
                      voiced: &mut u32| {
        if let Some(offset) = start.take() {
            if *voiced >= DIARIZATION_MIN_VOICED_FRAMES {
                collector.observe_primary_window(current, offset, frames);
            }
        }
        current.clear();
        frames.clear();
        *voiced = 0;
    };
    let offsets = std::iter::successors(Some(0_u64), |offset| Some(offset + VAD_FRAME_NS));
    for ((frame, &is_voice), offset) in samples
        .chunks_exact(VAD_FRAME_SAMPLES)
        .zip(voice)
        .zip(offsets)
    {
        if start.is_none() {
            if !is_voice {
                continue;
            }
            start = Some(offset);
        }
        current.extend_from_slice(frame);
        frames.push(is_voice);
        if is_voice {
            voiced += 1;
        }
        if current.len() >= DIARIZATION_WINDOW_SAMPLES {
            finish(&mut current, &mut frames, &mut start, &mut voiced);
        }
    }
    finish(&mut current, &mut frames, &mut start, &mut voiced);
}

fn overlap_s(span: &DiarizationSpeakerSpan, turn: &Turn) -> f64 {
    let seconds = |ns| Duration::from_nanos(ns).as_secs_f64();
    let start = seconds(span.start_offset_ns).max(turn.start_s);
    let end = seconds(span.end_offset_ns).min(turn.end_s);
    (end - start).max(0.0)
}

#[ignore = "builds prints from the meetings SONA_VOICE_PROBE_DIR names with the pinned models"]
#[test]
fn voice_prints_probe() {
    let directory = env_path("SONA_VOICE_PROBE_DIR");
    let sortformer = env_path("SONA_SORTFORMER_MODEL");
    let wespeaker = env_path("SONA_WESPEAKER_MODEL");
    let truth: Truth = serde_json::from_slice(
        &std::fs::read(directory.join("truth.json")).expect("truth.json is readable"),
    )
    .expect("truth.json is the generator's");
    let mut out = std::fs::File::create(directory.join("prints.jsonl")).expect("prints.jsonl");
    let diarizer = MeetingDiarizer::without_download();
    let prepared = PreparedDiarizationModel {
        path: sortformer,
        engine: DiarizationEngineKind::Sortformer,
    };
    let started = std::time::Instant::now();
    let (mut people, mut clusters, mut prints) = (0, 0, 0);
    for meeting in &truth.meetings {
        let samples =
            crate::audio_toolkit::read_wav_samples(directory.join(format!("{}.wav", meeting.id)))
                .expect("the meeting WAV is readable");
        let mut session = diarizer.open(&prepared).expect("Sortformer opens");
        session
            .push_priming_audio(&samples, 0)
            .expect("the meeting fits one pass");
        session.prime().expect("Sortformer scores the meeting");
        let spans = session.exclusive_spans().to_vec();
        let voice = voice_frames(&samples).expect("the meeting voice detector reads the meeting");
        let mut collector = VoiceIdentityCollector::primary(
            &spans,
            Box::new(SpeakerEmbeddingSession::open(&wespeaker).expect("WeSpeaker opens")),
        );
        observe_windows(&mut collector, &samples, &voice);
        let candidates = collector.into_candidates();
        let found: std::collections::BTreeSet<u32> =
            spans.iter().map(|span| span.cluster).collect();
        people += meeting.speakers.len();
        clusters += found.len();
        for candidate in candidates {
            let mut shares: BTreeMap<&str, f64> = BTreeMap::new();
            for span in spans
                .iter()
                .filter(|span| span.cluster == candidate.cluster)
            {
                for turn in &meeting.turns {
                    *shares.entry(turn.speaker.as_str()).or_default() += overlap_s(span, turn);
                }
            }
            let total: f64 = shares.values().sum();
            let Some((speaker, share)) = shares
                .iter()
                .max_by(|left, right| left.1.total_cmp(right.1))
                .map(|(speaker, share)| (speaker.to_string(), *share))
            else {
                continue;
            };
            prints += 1;
            let line = serde_json::json!({
                "meeting": meeting.id,
                "preset": meeting.preset,
                "cluster": candidate.cluster,
                "speaker": speaker,
                "gender": truth.genders.get(&speaker),
                "purity": if total > 0.0 { share / total } else { 0.0 },
                "exclusive_s": total,
                "embedding": candidate.embedding.as_slice(),
            });
            writeln!(out, "{line}").expect("prints.jsonl is writable");
        }
        println!(
            "{} {}: {} people, {} clusters, {} spans",
            meeting.id,
            meeting.preset,
            meeting.speakers.len(),
            found.len(),
            spans.len()
        );
    }
    println!(
        "{} meetings, {people} people, {clusters} clusters, {prints} prints in {:.0?}",
        truth.meetings.len(),
        started.elapsed()
    );
}

#[derive(Deserialize)]
struct Print {
    meeting: String,
    speaker: String,
    embedding: Vec<f32>,
}

/// Every meeting's prints against every other meeting's, through
/// `pair_same_voices`. A pair of two people is a false join; a person in both
/// meetings left unpaired is a miss. A sheet is one waiting voice and every
/// voice paired with it, and it is wrong when any of them is somebody else.
#[ignore = "reads the prints voice_prints_probe wrote to SONA_VOICE_PROBE_DIR"]
#[test]
fn same_voice_probe() {
    let directory = env_path("SONA_VOICE_PROBE_DIR");
    let prints = std::fs::read_to_string(directory.join("prints.jsonl"))
        .expect("prints.jsonl is readable")
        .lines()
        .map(|line| serde_json::from_str::<Print>(line).expect("a print line"))
        .collect::<Vec<_>>();
    let voices = prints
        .iter()
        .map(|print| {
            SpeakerEmbedding::from_normalized_slice(&print.embedding).expect("a normalized print")
        })
        .collect::<Vec<_>>();
    let mut meetings: Vec<Vec<usize>> = Vec::new();
    for (index, print) in prints.iter().enumerate() {
        match meetings.last_mut() {
            Some(meeting) if prints[meeting[0]].meeting == print.meeting => meeting.push(index),
            _ => meetings.push(vec![index]),
        }
    }
    let (mut same, mut found, mut different, mut joined) = (0, 0, 0, 0);
    let (mut sheets, mut wrong_sheets, mut members, mut wrong_members) = (0, 0, 0, 0);
    for (here_index, here) in meetings.iter().enumerate() {
        let here_voices = here
            .iter()
            .map(|&print| (print, &voices[print]))
            .collect::<Vec<_>>();
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); prints.len()];
        for (there_index, there) in meetings.iter().enumerate() {
            if there_index == here_index {
                continue;
            }
            let there_voices = there
                .iter()
                .map(|&print| (print, &voices[print]))
                .collect::<Vec<_>>();
            let pairs = pair_same_voices(&here_voices, &there_voices);
            for &(waiting, member) in &pairs {
                groups[waiting].push(member);
            }
            if there_index < here_index {
                continue;
            }
            for &left in here {
                for &right in there {
                    let paired = pairs.contains(&(left, right));
                    if prints[left].speaker == prints[right].speaker {
                        same += 1;
                        found += usize::from(paired);
                    } else {
                        different += 1;
                        joined += usize::from(paired);
                    }
                }
            }
        }
        for &waiting in here {
            let group = &groups[waiting];
            if group.is_empty() {
                continue;
            }
            let wrong = group
                .iter()
                .filter(|&&member| prints[member].speaker != prints[waiting].speaker)
                .count();
            sheets += 1;
            wrong_sheets += usize::from(wrong > 0);
            members += group.len();
            wrong_members += wrong;
        }
    }
    let rate = |part: usize, whole: usize| {
        let count = |n: usize| f64::from(u32::try_from(n).expect("a probe count fits u32"));
        count(part) / count(whole.max(1))
    };
    println!(
        "{}: {} prints in {} meetings",
        directory.display(),
        prints.len(),
        meetings.len()
    );
    println!(
        "pairs: false joins {joined}/{different} = {:.5}, misses {}/{same} = {:.3}",
        rate(joined, different),
        same - found,
        rate(same - found, same)
    );
    println!(
        "sheets: {sheets} shown, {wrong_sheets} with somebody else = {:.4}; \
         members {members}, {wrong_members} somebody else = {:.4}",
        rate(wrong_sheets, sheets),
        rate(wrong_members, members)
    );
}

#[derive(Deserialize)]
struct LaneSpan {
    id: String,
    start_s: f64,
    end_s: f64,
}

const LANE_SPAN_MIN_VOICED_FRAMES: usize = 50;
const LANE_SPAN_CHUNK_FRAMES: usize = PRIMARY_CANDIDATE_SAMPLES / VAD_FRAME_SAMPLES;
const LANE_SPAN_MAX_CHUNKS: usize = 3;

fn frame_index(seconds: f64) -> usize {
    let nanos = Duration::try_from_secs_f64(seconds.max(0.0)).map_or(u128::MAX, |at| at.as_nanos());
    usize::try_from(nanos / u128::from(VAD_FRAME_NS)).unwrap_or(usize::MAX)
}

fn frame_seconds(frames: usize) -> f64 {
    let frames = u64::try_from(frames).unwrap_or(u64::MAX);
    Duration::from_nanos(VAD_FRAME_NS.saturating_mul(frames)).as_secs_f64()
}

fn sample_seconds(samples: usize) -> f64 {
    let samples = u64::try_from(samples).unwrap_or(u64::MAX);
    // PANIC: SAMPLE_RATE_HZ is the constant 16 000, which fits u64.
    let rate = u64::try_from(SAMPLE_RATE_HZ).expect("the sample rate fits u64");
    Duration::from_nanos(samples.saturating_mul(NS_PER_SECOND) / rate).as_secs_f64()
}

/// One print for an arbitrary stretch of a lane, from its voiced frames only:
/// up to three three-second pieces of speech, as the collector's candidates
/// are, averaged into a unit vector. `None` under 1.5 s of speech.
fn span_print(
    embedder: &mut SpeakerEmbeddingSession,
    samples: &[f32],
    voice: &[bool],
    span: &LaneSpan,
) -> (f64, Option<Vec<f32>>) {
    let last = voice.len();
    let (first, end) = (
        frame_index(span.start_s).min(last),
        frame_index(span.end_s).min(last),
    );
    let voiced: Vec<usize> = (first..end).filter(|&frame| voice[frame]).collect();
    let voiced_s = frame_seconds(voiced.len());
    if voiced.len() < LANE_SPAN_MIN_VOICED_FRAMES {
        return (voiced_s, None);
    }
    let mut sum = [0.0_f32; SPEAKER_EMBEDDING_DIMENSIONS];
    let mut pieces = 0;
    for chunk in voiced
        .chunks(LANE_SPAN_CHUNK_FRAMES)
        .filter(|chunk| chunk.len() >= LANE_SPAN_MIN_VOICED_FRAMES)
        .take(LANE_SPAN_MAX_CHUNKS)
    {
        let audio: Vec<f32> = chunk
            .iter()
            .flat_map(|&frame| &samples[frame * VAD_FRAME_SAMPLES..(frame + 1) * VAD_FRAME_SAMPLES])
            .copied()
            .collect();
        let Ok(embedding) = embedder.embed(&audio) else {
            continue;
        };
        for (total, value) in sum.iter_mut().zip(embedding.as_slice()) {
            *total += value;
        }
        pieces += 1;
    }
    if pieces == 0 {
        return (voiced_s, None);
    }
    (
        voiced_s,
        unit_mean(sum).map(|print| print.as_slice().to_vec()),
    )
}

fn voiced_runs(voice: &[bool]) -> Vec<[f64; 2]> {
    let mut runs = Vec::new();
    let mut start = None;
    for (frame, &is_voice) in voice.iter().chain(std::iter::once(&false)).enumerate() {
        match (is_voice, start) {
            (true, None) => start = Some(frame),
            (false, Some(first)) => {
                runs.push([frame_seconds(first), frame_seconds(frame)]);
                start = None;
            }
            _ => {}
        }
    }
    runs
}

/// One lane WAV of a meeting analysis export through the meeting's own voice
/// path: Sortformer's exclusive spans, the meeting voice detector, two-second
/// windows and the primary collector's prints per cluster. With
/// `SONA_LANE_PROBE_SPANS` (a JSON list of `{id, start_s, end_s}`) it also
/// prints each listed stretch from its voiced frames.
///
/// SONA_LANE_PROBE_WAV=<wav> SONA_LANE_PROBE_OUT=<json> \
/// SONA_SORTFORMER_MODEL=<gguf> SONA_WESPEAKER_MODEL=<onnx> <test binary> \
///     --ignored --exact meeting::voice_identity::probe::lane_voices_probe
#[ignore = "reads the lane WAV SONA_LANE_PROBE_WAV names with the pinned models"]
#[test]
fn lane_voices_probe() {
    let wav = env_path("SONA_LANE_PROBE_WAV");
    let out = env_path("SONA_LANE_PROBE_OUT");
    let sortformer = env_path("SONA_SORTFORMER_MODEL");
    let wespeaker = env_path("SONA_WESPEAKER_MODEL");
    let spans: Vec<LaneSpan> =
        std::env::var_os("SONA_LANE_PROBE_SPANS").map_or_else(Vec::new, |path| {
            serde_json::from_slice(&std::fs::read(path).expect("the spans file is readable"))
                .expect("the spans file is a list of {id, start_s, end_s}")
        });
    let started = std::time::Instant::now();
    let samples = crate::audio_toolkit::read_wav_samples(&wav).expect("the lane WAV is readable");
    let voice = voice_frames(&samples).expect("the meeting voice detector reads the lane");
    let diarizer = MeetingDiarizer::without_download();
    let prepared = PreparedDiarizationModel {
        path: sortformer,
        engine: DiarizationEngineKind::Sortformer,
    };
    let mut session = diarizer.open(&prepared).expect("Sortformer opens");
    let scored = session
        .push_priming_audio(&samples, 0)
        .and_then(|()| session.prime());
    let diarization_error = scored.err().map(|error| format!("{error:?}"));
    let exclusive = session.exclusive_spans().to_vec();
    let mut collector = VoiceIdentityCollector::primary(
        &exclusive,
        Box::new(SpeakerEmbeddingSession::open(&wespeaker).expect("WeSpeaker opens")),
    );
    observe_windows(&mut collector, &samples, &voice);
    let prints: Vec<serde_json::Value> = collector
        .into_candidates()
        .into_iter()
        .map(|candidate| {
            serde_json::json!({
                "cluster": candidate.cluster,
                "embedding": candidate.embedding.as_slice(),
            })
        })
        .collect();
    let mut embedder = SpeakerEmbeddingSession::open(&wespeaker).expect("WeSpeaker opens");
    let span_prints: Vec<serde_json::Value> = spans
        .iter()
        .map(|span| {
            let (voiced_s, embedding) = span_print(&mut embedder, &samples, &voice, span);
            serde_json::json!({ "id": span.id, "voiced_s": voiced_s, "embedding": embedding })
        })
        .collect();
    let seconds = |ns: u64| Duration::from_nanos(ns).as_secs_f64();
    let report = serde_json::json!({
        "wav": wav,
        "duration_s": sample_seconds(samples.len()),
        "diarization_error": diarization_error,
        "voiced": voiced_runs(&voice),
        "exclusive_spans": exclusive
            .iter()
            .map(|span| serde_json::json!({
                "cluster": span.cluster,
                "start_s": seconds(span.start_offset_ns),
                "end_s": seconds(span.end_offset_ns),
            }))
            .collect::<Vec<_>>(),
        "prints": prints,
        "span_prints": span_prints,
    });
    std::fs::write(
        &out,
        serde_json::to_vec(&report).expect("the report serializes"),
    )
    .expect("the probe output is writable");
    println!(
        "{}: {:.0} s, {} exclusive spans, {} prints, {} span prints in {:.0?}",
        wav.display(),
        sample_seconds(samples.len()),
        exclusive.len(),
        prints.len(),
        span_prints.len(),
        started.elapsed()
    );
}
