const SAMPLE_RATE: usize = 16_000;
const SEARCH_SAMPLES: usize = 2 * SAMPLE_RATE;
const ENERGY_FRAME_SAMPLES: u16 = 160;
const MIN_PAUSE_FRAMES: usize = 10;
const MIN_TAIL_SAMPLES: usize = SAMPLE_RATE / 2;
const QUIET_MEAN_SQUARE: f32 = 0.0001;

/// Borrow consecutive windows without padding, overlap, or dropped tail audio.
/// Models with no finite bound pass the full input length, preserving their
/// existing single-call path. Splitting changes neither the PCM nor its owner.
pub(super) fn transcribe_windows<E>(
    mut audio: &[f32],
    max_samples: usize,
    mut decode: impl FnMut(&[f32]) -> Result<String, E>,
) -> Result<String, E> {
    let end = window_end(audio, max_samples);
    let mut text = decode(&audio[..end])?;
    audio = &audio[end..];
    while !audio.is_empty() {
        let end = window_end(audio, max_samples);
        let next = decode(&audio[..end])?;
        if !text.is_empty()
            && !next.is_empty()
            && !text.ends_with(char::is_whitespace)
            && !next.starts_with(char::is_whitespace)
        {
            text.push(' ');
        }
        text.push_str(&next);
        audio = &audio[end..];
    }
    Ok(text)
}

/// Prefer the middle of a pause in the last two seconds before the model's
/// ceiling. Without a pause the hard ceiling wins. Keep a half-second tail
/// rather than creating a fragment too short for Moonshine's input contract.
fn window_end(audio: &[f32], max_samples: usize) -> usize {
    assert!(max_samples > 0, "an audio window must make progress");
    if audio.len() <= max_samples {
        return audio.len();
    }
    let minimum_tail = MIN_TAIL_SAMPLES.min(max_samples / 2);
    let ceiling = max_samples.min(audio.len() - minimum_tail);
    let search_start = ceiling.saturating_sub(SEARCH_SAMPLES).max(ceiling / 2);
    let mut boundary = ceiling;
    let mut quiet_frames = 0;
    let frame_samples = usize::from(ENERGY_FRAME_SAMPLES);
    for (index, frame) in audio[search_start..ceiling]
        .chunks_exact(frame_samples)
        .enumerate()
    {
        let energy = frame.iter().map(|sample| sample * sample).sum::<f32>();
        if energy <= QUIET_MEAN_SQUARE * f32::from(ENERGY_FRAME_SAMPLES) {
            quiet_frames += 1;
            if quiet_frames >= MIN_PAUSE_FRAMES {
                let frame_end = search_start + (index + 1) * frame_samples;
                boundary = frame_end - quiet_frames * frame_samples / 2;
            }
        } else {
            quiet_frames = 0;
        }
    }
    boundary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pause_before_the_halving_boundary_keeps_the_word_boundary_intact() {
        let mut audio = vec![0.5; 40 * SAMPLE_RATE];
        let pause_start = 18 * SAMPLE_RATE;
        let pause_end = pause_start + SAMPLE_RATE / 2;
        audio[pause_start..pause_end].fill(0.0);
        assert_eq!(
            window_end(&audio, audio.len() / 2),
            (pause_start + pause_end) / 2
        );
    }

    #[test]
    fn continuous_speech_never_exceeds_the_model_ceiling() {
        let audio = vec![0.5; 40 * SAMPLE_RATE];
        assert_eq!(window_end(&audio, 30 * SAMPLE_RATE), 30 * SAMPLE_RATE);
    }

    #[test]
    fn an_under_limit_buffer_is_not_split_or_scanned_for_silence() {
        let audio = vec![0.0; 30 * SAMPLE_RATE];
        assert_eq!(window_end(&audio, audio.len()), audio.len());
        assert_eq!(window_end(&audio, audio.len() + 1), audio.len());
    }

    #[test]
    fn a_short_tail_is_kept_without_exceeding_the_preceding_window() {
        let audio = vec![0.5; 64 * SAMPLE_RATE + 1];
        let end = window_end(&audio, 64 * SAMPLE_RATE);
        assert_eq!(audio.len() - end, MIN_TAIL_SAMPLES);
        assert!(end <= 64 * SAMPLE_RATE);
    }

    #[test]
    fn twenty_minutes_are_partitioned_without_lost_or_repeated_samples() {
        let mut audio = vec![0.25; 20 * 60 * SAMPLE_RATE];
        // Periodic pauses make the boundaries vary instead of testing only
        // exact divisibility by the limit.
        for second in (17..1200).step_by(17) {
            audio[second * SAMPLE_RATE..second * SAMPLE_RATE + SAMPLE_RATE / 4].fill(0.0);
        }
        let mut consumed = 0;
        let _: String = transcribe_windows(&audio, 30 * SAMPLE_RATE, |window| {
            assert!(!window.is_empty() && window.len() <= 30 * SAMPLE_RATE);
            assert_eq!(window.as_ptr(), audio[consumed..].as_ptr());
            assert_eq!(window, &audio[consumed..consumed + window.len()]);
            consumed += window.len();
            Ok::<String, std::convert::Infallible>(String::new())
        })
        .unwrap();
        assert_eq!(consumed, 20 * 60 * SAMPLE_RATE);
    }
}
