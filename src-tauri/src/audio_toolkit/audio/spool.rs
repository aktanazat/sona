//! The on-disk copy of a dictation's whole capture, written while it records.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use hound::{SampleFormat, WavSpec, WavWriter};

use super::utils::wav_sample_to_i16;
use crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE;

const SPEC: WavSpec = WavSpec {
    channels: 1,
    sample_rate: WHISPER_SAMPLE_RATE,
    bits_per_sample: 16,
    sample_format: SampleFormat::Int,
};

/// What hound writes ahead of the samples for [`SPEC`]: the RIFF header, a
/// 16-byte `fmt ` chunk, and the `data` chunk's id and length.
const HEADER_LEN: usize = 44;
const HEADER_BYTES: u64 = 44;
const BYTES_PER_SAMPLE: u64 = 2;
/// The RIFF length counts everything after its own 8-byte preamble.
const RIFF_LEN_BEFORE_DATA: u32 = 36;
const RIFF_LEN_OFFSET: u64 = 4;
const DATA_LEN_OFFSET: u64 = 40;

/// About one second of 16 kHz audio between header updates, so a crash loses
/// at most that much.
const CHECKPOINT_SAMPLES: usize = 16_000;

/// Streams one dictation's 16 kHz samples to a WAV file as they arrive.
pub(crate) struct SpoolWriter {
    writer: WavWriter<BufWriter<File>>,
    path: PathBuf,
    since_checkpoint: usize,
    failed: bool,
}

impl SpoolWriter {
    pub(crate) fn create(path: &Path) -> Result<Self, hound::Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self {
            writer: WavWriter::create(path, SPEC)?,
            path: path.to_path_buf(),
            since_checkpoint: 0,
            failed: false,
        })
    }

    /// Append samples. A write that fails stops the spool for the rest of the
    /// capture; the recording itself goes on.
    pub(crate) fn write(&mut self, samples: &[f32]) {
        if self.failed {
            return;
        }
        let written = samples
            .iter()
            .try_for_each(|&sample| self.writer.write_sample(wav_sample_to_i16(sample)))
            .and_then(|()| {
                self.since_checkpoint += samples.len();
                if self.since_checkpoint < CHECKPOINT_SAMPLES {
                    return Ok(());
                }
                self.since_checkpoint = 0;
                self.writer.flush()
            });
        if let Err(error) = written {
            self.failed = true;
            log::error!(
                "Dictation spool {} stopped after {} samples: {error}",
                self.path.display(),
                self.writer.len()
            );
        }
    }

    pub(crate) fn finish(self) {
        let Self {
            writer,
            path,
            failed,
            ..
        } = self;
        if let Err(error) = writer.finalize() {
            if !failed {
                log::error!("Dictation spool {} did not close: {error}", path.display());
            }
        }
    }
}

/// Make a spool's header agree with the whole samples on disk, dropping a torn
/// trailing byte, and return how many samples it holds. A file too short for
/// a header holds none. A header that is not a spool's is left untouched.
pub fn repair_spool(path: &Path) -> io::Result<u64> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let length = file.metadata()?.len();
    if length < HEADER_BYTES {
        return Ok(0);
    }
    let mut header = [0u8; HEADER_LEN];
    file.read_exact(&mut header)?;
    if !is_spool_header(&header) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a 16 kHz mono 16-bit dictation spool",
        ));
    }

    let data_bytes = (length - HEADER_BYTES) / BYTES_PER_SAMPLE * BYTES_PER_SAMPLE;
    let data_len = u32::try_from(data_bytes)
        .ok()
        .filter(|len| len.checked_add(RIFF_LEN_BEFORE_DATA).is_some())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "spool exceeds a WAV file"))?;
    if length != HEADER_BYTES + data_bytes {
        file.set_len(HEADER_BYTES + data_bytes)?;
    }
    file.seek(SeekFrom::Start(RIFF_LEN_OFFSET))?;
    file.write_all(&(data_len + RIFF_LEN_BEFORE_DATA).to_le_bytes())?;
    file.seek(SeekFrom::Start(DATA_LEN_OFFSET))?;
    file.write_all(&data_len.to_le_bytes())?;
    Ok(data_bytes / BYTES_PER_SAMPLE)
}

fn is_spool_header(header: &[u8; HEADER_LEN]) -> bool {
    let u16_at = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
    let u32_at = |at: usize| {
        u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
    };
    header[0..4] == *b"RIFF"
        && header[8..16] == *b"WAVEfmt "
        && u32_at(16) == 16
        && u16_at(20) == 1
        && u16_at(22) == SPEC.channels
        && u32_at(24) == SPEC.sample_rate
        && u16_at(34) == SPEC.bits_per_sample
        && header[36..40] == *b"data"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A crash leaves the header at its last checkpoint and loses the write
    /// buffer, sometimes mid-sample. The repaired file must play every whole
    /// sample that reached the disk, not stop where the stale header said.
    #[test]
    fn repair_gives_a_crashed_spool_every_whole_sample_on_disk() {
        let directory = tempfile::tempdir().expect("spool directory");
        let path = directory.path().join("sona-1767225600-42.wav");
        let audio: Vec<f32> = (0..48_123_u16)
            .map(|index| (f32::from(index) * 0.031).sin() * 0.4)
            .collect();
        let mut writer = SpoolWriter::create(&path).expect("create spool");
        for frame in audio.chunks(480) {
            writer.write(frame);
        }
        std::mem::forget(writer);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open crashed spool")
            .write_all(&[0x7f])
            .expect("append a torn byte");
        let on_disk =
            (fs::metadata(&path).expect("spool metadata").len() - HEADER_BYTES) / BYTES_PER_SAMPLE;
        let stale = hound::WavReader::open(&path)
            .expect("crashed spool opens")
            .len();
        assert!(u64::from(stale) < on_disk, "the crash left a stale header");

        let samples = repair_spool(&path).expect("repair spool");

        assert_eq!(samples, on_disk);
        let repaired = crate::audio_toolkit::read_wav_samples(&path).expect("read repaired spool");
        assert_eq!(u64::try_from(repaired.len()), Ok(samples));
        let worst = repaired
            .iter()
            .zip(&audio)
            .map(|(repaired, original)| (repaired - original).abs())
            .fold(0.0_f32, f32::max);
        assert!(worst < 1.0e-3, "repaired samples drifted by {worst}");
    }
}
