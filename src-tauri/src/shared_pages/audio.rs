//! The meeting's recording as one AAC file, mixed the way review playback
//! mixes it, with silence where nothing was captured so transcript times
//! still land on the right second.

use std::fs::{self, File};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine;

use super::content::PageAudio;
use super::{Code, SharedPagesError};
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::{MeetingReviewSnapshot, MeetingSessionId};

const AFCONVERT: &str = "/usr/bin/afconvert";
const SAMPLE_RATE: u32 = 16_000;
const NS_PER_SECOND: u64 = 1_000_000_000;
const GAP_NS: u64 = 45 * NS_PER_SECOND;
pub(crate) const PART_BYTES: usize = 24 * 1024 * 1024;
const MAX_PARTS: usize = 64;
pub(crate) const MIME: &str = "audio/mp4";

/// Writes the meeting's recording to `dest` as AAC in an MPEG-4 container.
pub(crate) fn export(
    store: &MeetingStore,
    review: &MeetingReviewSnapshot,
    dest: &Path,
) -> Result<(), SharedPagesError> {
    let session_id = review.session.session_id;
    let recorded = review
        .tracks
        .iter()
        .filter(|track| track.durable_record_count > 0);
    let first = recorded
        .clone()
        .filter_map(|track| track.first_offset_ns)
        .min();
    let latest = recorded.filter_map(|track| track.last_offset_ns).max();
    let (Some(first), Some(latest)) = (first, latest) else {
        return Err(unavailable());
    };
    let wav = dest.with_extension("export.wav");
    let written = write_wav(store, session_id, first, latest, &wav);
    let converted = written.and_then(|()| convert(&wav, dest));
    let _ = fs::remove_file(&wav);
    converted
}

fn write_wav(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    first: u64,
    latest: u64,
    path: &Path,
) -> Result<(), SharedPagesError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|_| failed())?;
    silence(&mut writer, first)?;
    let mut start = first;
    while start < latest {
        match store.conversation_audio_clip(session_id, None, Some(start), None) {
            Ok(clip) => {
                let wav = base64::engine::general_purpose::STANDARD
                    .decode(clip.wav_base64)
                    .map_err(|_| unavailable())?;
                let mut reader =
                    hound::WavReader::new(Cursor::new(wav)).map_err(|_| unavailable())?;
                for sample in reader.samples::<i16>() {
                    writer
                        .write_sample(sample.map_err(|_| unavailable())?)
                        .map_err(|_| failed())?;
                }
                match clip.next_offset_ns {
                    Some(next) if next > start => start = next,
                    _ => break,
                }
            }
            Err(StoreError::NotFound) => {
                let span = GAP_NS.min(latest - start);
                silence(&mut writer, span)?;
                start += span;
            }
            Err(_) => return Err(unavailable()),
        }
    }
    writer.finalize().map_err(|_| failed())
}

fn silence(
    writer: &mut hound::WavWriter<std::io::BufWriter<File>>,
    ns: u64,
) -> Result<(), SharedPagesError> {
    for _ in 0..ns * u64::from(SAMPLE_RATE) / NS_PER_SECOND {
        writer.write_sample(0_i16).map_err(|_| failed())?;
    }
    Ok(())
}

fn convert(wav: &Path, dest: &Path) -> Result<(), SharedPagesError> {
    let partial = dest.with_extension("partial.m4a");
    let status = Command::new(AFCONVERT)
        .args(["-f", "m4af", "-d", "aac", "-b", "48000", "-c", "1"])
        .arg(wav)
        .arg(&partial)
        .status()
        .map_err(|_| unavailable())?;
    if !status.success() {
        let _ = fs::remove_file(&partial);
        return Err(unavailable());
    }
    fs::rename(&partial, dest).map_err(|_| failed())
}

/// The page's description of the recording at `path`.
pub(crate) fn describe(
    path: &Path,
    fallback_duration_s: Option<u64>,
) -> Result<PageAudio, SharedPagesError> {
    let bytes = fs::metadata(path).map_err(|_| unavailable())?.len();
    if bytes == 0 {
        return Err(unavailable());
    }
    let parts = usize::try_from(bytes).map_or(usize::MAX, |bytes| bytes.div_ceil(PART_BYTES));
    if parts > MAX_PARTS {
        return Err(SharedPagesError::new(
            Code::TooLarge,
            "The recording is larger than 64 parts of 24 MiB.",
        ));
    }
    Ok(PageAudio {
        r#type: MIME.to_owned(),
        parts: u32::try_from(parts).unwrap_or(u32::MAX),
        bytes,
        duration_s: duration_s(path).or(fallback_duration_s).unwrap_or(0),
    })
}

/// The recording's bytes, which the site serves in `PART_BYTES` parts, and
/// the page's description of them.
pub(crate) fn read(
    path: &Path,
    fallback_duration_s: Option<u64>,
) -> Result<(PageAudio, Vec<u8>), SharedPagesError> {
    let audio = describe(path, fallback_duration_s)?;
    let bytes = fs::read(path).map_err(|_| unavailable())?;
    if u64::try_from(bytes.len()) != Ok(audio.bytes) {
        return Err(unavailable());
    }
    Ok((audio, bytes))
}

fn duration_s(path: &Path) -> Option<u64> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = File::open(path).ok()?;
    let source = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension("m4a");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let track = probed.format.default_track()?;
    let frames = track.codec_params.n_frames?;
    let rate = u64::from(track.codec_params.sample_rate?);
    (rate > 0).then(|| (frames + rate / 2) / rate)
}

/// Where a draft keeps its exported recording.
pub(crate) fn draft_audio_path(draft_dir: &Path) -> PathBuf {
    draft_dir.join("audio.m4a")
}

fn unavailable() -> SharedPagesError {
    SharedPagesError::new(
        Code::AudioUnavailable,
        "The recording is not available to share.",
    )
}

fn failed() -> SharedPagesError {
    SharedPagesError::new(Code::Failed, "The recording could not be written.")
}
