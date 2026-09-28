/// Quiet dictation uses 12 dB of software gain. Limiting happens before both
/// speech detection and storage; the native meeting packet is never modified.
pub(crate) fn amplify_sample(sample: f32) -> f32 {
    if sample.is_nan() {
        0.0
    } else {
        (sample * 4.0).clamp(-1.0, 1.0)
    }
}

pub(super) fn detection_threshold(normal: f32, quiet_speech: bool) -> f32 {
    if quiet_speech {
        normal * 0.6
    } else {
        normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_speech_is_amplified_without_changing_polarity() {
        assert_eq!(amplify_sample(0.03125), 0.125);
        assert_eq!(amplify_sample(-0.03125), -0.125);
        assert_eq!(amplify_sample(0.0), 0.0);
    }

    #[test]
    fn loud_input_is_limited_instead_of_wrapping() {
        assert_eq!(amplify_sample(0.25), 1.0);
        assert_eq!(amplify_sample(0.75), 1.0);
        assert_eq!(amplify_sample(-0.75), -1.0);
        assert_eq!(amplify_sample(f32::MAX), 1.0);
        assert_eq!(amplify_sample(f32::MIN), -1.0);
    }

    #[test]
    fn invalid_device_samples_cannot_escape_the_limiter() {
        assert_eq!(amplify_sample(f32::INFINITY), 1.0);
        assert_eq!(amplify_sample(f32::NEG_INFINITY), -1.0);
        assert_eq!(amplify_sample(f32::NAN), 0.0);
    }

    #[test]
    fn quiet_threshold_relaxes_both_engines_and_restores_their_own_defaults() {
        assert!((detection_threshold(0.55, true) - 0.33).abs() < f32::EPSILON);
        assert!((detection_threshold(0.3, true) - 0.18).abs() < f32::EPSILON);
        assert_eq!(detection_threshold(0.55, false), 0.55);
        assert_eq!(detection_threshold(0.3, false), 0.3);
    }
}
