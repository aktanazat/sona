use anyhow::Result;
use hound::{WavReader, WavSpec, WavWriter};
use log::debug;
use rustfft::num_traits::ToPrimitive;
use std::path::Path;

pub(crate) fn wav_sample_to_i16(sample: f32) -> i16 {
    let scaled = sample * f32::from(i16::MAX);
    match scaled.to_i16() {
        Some(sample) => sample,
        None if scaled.is_nan() => 0,
        None if scaled.is_sign_negative() => i16::MIN,
        None => i16::MAX,
    }
}

/// Read a WAV file and return normalised f32 samples.
pub fn read_wav_samples<P: AsRef<Path>>(file_path: P) -> Result<Vec<f32>> {
    let reader = WavReader::open(file_path.as_ref())?;
    let samples = reader
        .into_samples::<i16>()
        .map(|sample| sample.map(|sample| f32::from(sample) / f32::from(i16::MAX)))
        .collect::<Result<Vec<f32>, _>>()?;
    Ok(samples)
}

/// Verify a WAV file by reading it back and checking the sample count.
pub fn verify_wav_file<P: AsRef<Path>>(file_path: P, expected_samples: usize) -> Result<()> {
    let reader = WavReader::open(file_path.as_ref())?;
    let actual_samples = usize::try_from(reader.len())
        .map_err(|_| anyhow::anyhow!("WAV sample count does not fit usize"))?;
    if actual_samples != expected_samples {
        anyhow::bail!(
            "WAV sample count mismatch: expected {}, got {}",
            expected_samples,
            actual_samples
        );
    }
    Ok(())
}

/// Save audio samples as a WAV file
pub fn save_wav_file<P: AsRef<Path>>(file_path: P, samples: &[f32]) -> Result<()> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = WavWriter::create(file_path.as_ref(), spec)?;

    for &sample in samples {
        writer.write_sample(wav_sample_to_i16(sample))?;
    }

    writer.finalize()?;
    debug!("Saved WAV file: {:?}", file_path.as_ref());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::wav_sample_to_i16;

    #[test]
    fn wav_sample_conversion_preserves_float_to_i16_semantics() {
        assert_eq!(wav_sample_to_i16(f32::NAN), 0);
        assert_eq!(wav_sample_to_i16(f32::NEG_INFINITY), i16::MIN);
        assert_eq!(wav_sample_to_i16(-2.0), i16::MIN);
        assert_eq!(wav_sample_to_i16(-1.0), -i16::MAX);
        assert_eq!(wav_sample_to_i16(-0.5), -16_383);
        assert_eq!(wav_sample_to_i16(0.5), 16_383);
        assert_eq!(wav_sample_to_i16(1.0), i16::MAX);
        assert_eq!(wav_sample_to_i16(2.0), i16::MAX);
        assert_eq!(wav_sample_to_i16(f32::INFINITY), i16::MAX);
    }
}
