//! How one batch decode went, for the headless `--diagnose` report. It holds
//! counts, never transcript words, so the report can be shared as it is.

use super::RecoveredSpan;
use clap::ValueEnum;
use std::fmt;
use transcribe_cpp::{Model, Transcript};

/// Whisper's temperature fallback samples with a fresh random seed on every
/// decode unless given one. Every decode of a diagnosis shares this seed, so
/// the no-speech measurement differs from the decode it measures only where
/// the check acted, and a diagnosis repeats exactly.
pub(super) const SAMPLING_SEED: u32 = 1;

/// One of Sona's own text filters on the dictation path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum TextFilter {
    /// A decode that only repeats its custom-words prompt counts as nothing heard.
    PromptEcho,
    /// An unprompted decode made only of custom words counts as nothing heard.
    VocabularyOnly,
    /// Filler removal, punctuation, spelling, replacements, vocabulary
    /// correction, spoken edits, snippets and emoji.
    PostProcessing,
}

/// Parts of the app's decode a headless check switches off. The default
/// switches nothing off, which is the app's own decode.
#[derive(Clone, Debug, Default)]
pub struct DecodeSwitches {
    pub without_prompt: bool,
    pub without_no_speech_check: bool,
    pub without_filters: Vec<TextFilter>,
}

impl DecodeSwitches {
    pub(super) fn keeps(&self, filter: TextFilter) -> bool {
        !self.without_filters.contains(&filter)
    }
}

/// What one filter did to the text it was given.
#[derive(Clone, Copy, Debug)]
pub enum FilterOutcome {
    NotApplied(&'static str),
    SwitchedOff,
    Ran { before: usize, after: usize },
}

impl FilterOutcome {
    /// A filter that keeps the whole text or removes all of it.
    pub(super) fn whole(chars: usize, removed: bool) -> Self {
        Self::Ran {
            before: chars,
            after: if removed { 0 } else { chars },
        }
    }
}

impl Default for FilterOutcome {
    fn default() -> Self {
        Self::NotApplied("not reached")
    }
}

impl fmt::Display for FilterOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NotApplied(reason) => write!(f, "not applied ({reason})"),
            Self::SwitchedOff => f.write_str("switched off"),
            Self::Ran { before, after } if after > before => {
                write!(f, "added {} characters to {before}", after - before)
            }
            Self::Ran { before, after } => {
                write!(f, "removed {} of {before} characters", before - after)
            }
        }
    }
}

/// One segment the engine reported, with times in the whole clip.
#[derive(Debug)]
pub struct SegmentStats {
    pub start_ms: i64,
    pub end_ms: i64,
    /// The engine's token rows for the segment, or when it reports none, the
    /// segment text encoded again by the model's tokenizer.
    pub tokens: Option<usize>,
    pub tokens_reencoded: bool,
    pub chars: usize,
    /// Mean natural log of the token probabilities the engine reported.
    pub avg_logprob: Option<f64>,
}

impl SegmentStats {
    fn new(model: &Model, transcript: &Transcript, index: usize, offset_ms: i64) -> Self {
        let segment = &transcript.segments[index];
        let rows = transcript
            .tokens
            .iter()
            .filter(|token| usize::try_from(token.seg_index).is_ok_and(|seg| seg == index));
        let row_count = rows.clone().count();
        let logprobs = rows
            .filter(|token| token.p > 0.0 && token.p <= 1.0)
            .map(|token| token.p.ln());
        let (sum, count) = logprobs.fold((0.0_f64, 0_u32), |(sum, count), logprob| {
            (sum + f64::from(logprob), count + 1)
        });
        let tokens_reencoded = row_count == 0;
        let tokens = if tokens_reencoded {
            model.tokenize(&segment.text).ok().map(|ids| ids.len())
        } else {
            Some(row_count)
        };
        Self {
            start_ms: offset_ms + segment.t0_ms,
            end_ms: offset_ms + segment.t1_ms,
            tokens,
            tokens_reencoded,
            chars: segment.text.chars().count(),
            avg_logprob: (count > 0).then(|| sum / f64::from(count)),
        }
    }
}

/// A stretch of a window that heard nothing and was decoded again.
#[derive(Debug)]
pub struct RecoveredStats {
    pub start_ms: i64,
    pub end_ms: i64,
    pub chars: usize,
}

/// The successful native runs of a diagnosed decode, in audio order.
#[derive(Debug, Default)]
pub(super) struct NativeRuns {
    runs: usize,
    decoded_samples: usize,
    segments: Vec<SegmentStats>,
    recovered: Vec<RecoveredStats>,
}

impl NativeRuns {
    pub(super) fn record(
        &mut self,
        model: &Model,
        samples: usize,
        transcript: &Transcript,
        recovered: &[RecoveredSpan],
    ) {
        let offset_ms = i64::try_from(self.decoded_samples / 16).unwrap_or(i64::MAX);
        self.segments.extend(
            (0..transcript.segments.len())
                .map(|index| SegmentStats::new(model, transcript, index, offset_ms)),
        );
        self.recovered
            .extend(recovered.iter().map(|recovered| RecoveredStats {
                start_ms: offset_ms + recovered.span.start,
                end_ms: offset_ms + recovered.span.end,
                chars: recovered.text.chars().count(),
            }));
        self.runs += 1;
        self.decoded_samples += samples;
    }

    pub(super) fn report_into(self, diagnosis: &mut DecodeDiagnosis) {
        diagnosis.windows = Some(self.runs);
        diagnosis.segments = Some(self.segments);
        diagnosis.recovered_spans = self.recovered;
    }
}

/// How one batch decode went. `None` counts are ones the engine does not report.
#[derive(Debug, Default)]
pub struct DecodeDiagnosis {
    pub switches: DecodeSwitches,
    pub model: String,
    pub engine: String,
    pub prompt_words: Option<usize>,
    pub language_hint: Option<String>,
    pub detected_language: Option<String>,
    pub windows: Option<usize>,
    pub segments: Option<Vec<SegmentStats>>,
    pub engine_chars: usize,
    /// `Ran` holds the characters of a second decode with the check off as
    /// `before`, and of the decode with it on as `after`.
    pub no_speech_check: FilterOutcome,
    /// The fallback sampling seed every whisper decode here shared.
    pub sampling_seed: Option<u32>,
    /// Stretches of a window that heard nothing in audio known to hold
    /// speech, decoded again without the prompt.
    pub recovered_spans: Vec<RecoveredStats>,
    pub prompt_echo: FilterOutcome,
    pub vocabulary_only: FilterOutcome,
    pub post_processing: FilterOutcome,
    pub result_chars: usize,
}

pub(super) fn seconds(ms: i64) -> f64 {
    f64::from(i32::try_from(ms).unwrap_or(i32::MAX)) / 1000.0
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

impl fmt::Display for DecodeDiagnosis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "model: {}", self.model)?;
        writeln!(f, "engine: {}", self.engine)?;
        let mut off: Vec<String> = Vec::new();
        if self.switches.without_prompt {
            off.push("prompt".to_owned());
        }
        if self.switches.without_no_speech_check {
            off.push("no-speech check".to_owned());
        }
        off.extend(
            self.switches
                .without_filters
                .iter()
                .filter_map(|filter| filter.to_possible_value())
                .map(|value| format!("{} filter", value.get_name())),
        );
        if off.is_empty() {
            writeln!(f, "switched off: nothing (the app's decode)")?;
        } else {
            writeln!(f, "switched off: {}", off.join(", "))?;
        }
        match self.prompt_words {
            Some(words) => writeln!(f, "prompt: used, {words} word{}", plural(words))?,
            None => writeln!(f, "prompt: none")?,
        }
        match (&self.language_hint, &self.detected_language) {
            (Some(hint), _) => writeln!(f, "language: {hint}, given to the engine")?,
            (None, Some(detected)) => writeln!(f, "language: {detected}, detected by the engine")?,
            (None, None) => writeln!(f, "language: left to the engine, none reported")?,
        }
        match self.windows {
            Some(windows) => writeln!(f, "windows: {windows}")?,
            None => writeln!(f, "windows: not reported by this engine")?,
        }
        match &self.segments {
            Some(segments) => {
                writeln!(f, "segments: {}", segments.len())?;
                for (index, segment) in segments.iter().enumerate() {
                    write!(
                        f,
                        "  {}. {:.2}-{:.2}s  ",
                        index + 1,
                        seconds(segment.start_ms),
                        seconds(segment.end_ms)
                    )?;
                    match (segment.tokens, segment.tokens_reencoded) {
                        (Some(tokens), false) => write!(f, "tokens {tokens}")?,
                        (Some(tokens), true) => write!(f, "tokens {tokens} (re-encoded)")?,
                        (None, _) => f.write_str("tokens n/a")?,
                    }
                    write!(f, "  chars {}  avg logprob ", segment.chars)?;
                    match segment.avg_logprob {
                        Some(logprob) => writeln!(f, "{logprob:.3}")?,
                        None => writeln!(f, "n/a (no token probabilities from this engine)")?,
                    }
                }
            }
            None => writeln!(f, "segments: not reported by this engine")?,
        }
        if let Some(seed) = self.sampling_seed {
            writeln!(
                f,
                "fallback sampling: seed {seed} in every decode here (the app draws a new seed each time)"
            )?;
        }
        writeln!(f, "engine output: {} characters", self.engine_chars)?;
        writeln!(f, "filters:")?;
        match self.no_speech_check {
            FilterOutcome::Ran { before, after } if before == after => writeln!(
                f,
                "  no-speech check (engine): removed nothing (both decodes {after} characters; measured by decoding again)"
            )?,
            FilterOutcome::Ran { before, after } if before > after => writeln!(
                f,
                "  no-speech check (engine): removed {} characters ({before} with the check off, {after} with it on; measured by decoding again)",
                before - after
            )?,
            FilterOutcome::Ran { before, after } => writeln!(
                f,
                "  no-speech check (engine): changed the decode, which came out shorter without it ({before} characters with the check off, {after} with it on; measured by decoding again)"
            )?,
            other => writeln!(f, "  no-speech check (engine): {other}")?,
        }
        writeln!(f, "  prompt echo: {}", self.prompt_echo)?;
        for span in &self.recovered_spans {
            writeln!(
                f,
                "  {:.2}-{:.2}s heard nothing though the audio holds speech, so it was decoded again without the prompt: {} characters",
                seconds(span.start_ms),
                seconds(span.end_ms),
                span.chars
            )?;
        }
        writeln!(f, "  vocabulary-only: {}", self.vocabulary_only)?;
        writeln!(f, "  post-processing: {}", self.post_processing)?;
        writeln!(f, "result: {} characters", self.result_chars)
    }
}
