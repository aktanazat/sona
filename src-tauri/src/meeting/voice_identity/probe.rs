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
