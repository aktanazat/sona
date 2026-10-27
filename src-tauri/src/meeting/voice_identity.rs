use super::diarization::{
    DiarizationEngineKind, DiarizationError, DiarizationSpeakerSpan, SpeakerEmbedding,
    SpeakerEmbeddingSession, NS_PER_SECOND, SAMPLE_RATE_HZ, SPEAKER_EMBEDDING_DIMENSIONS,
};
use super::types::{MeetingOrigin, SourceKind, SpeakerAssignmentKind, SpeakerId};
use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};

const VAD_FRAME_SAMPLES: usize = 480;
const VAD_FRAME_NS: u64 = 30_000_000;
const PRIMARY_GUARD_NS: u64 = 250_000_000;
const PRIMARY_CANDIDATE_SAMPLES: usize = 3 * SAMPLE_RATE_HZ;
const PRIMARY_CANDIDATE_NS: u64 = 3 * NS_PER_SECOND;
const PRIMARY_MIN_VOICED_FRAMES: u32 = 80;
const PRIMARY_MIN_CANDIDATES: u8 = 2;
const FALLBACK_WINDOW_SAMPLES: usize = 2 * SAMPLE_RATE_HZ;
const FALLBACK_WINDOW_NS: u64 = 2 * NS_PER_SECOND;
const FALLBACK_MIN_VOICED_FRAMES: u32 = 50;
const FALLBACK_MIN_CANDIDATES: u8 = 2;
const MAX_CANDIDATES_PER_SPEAKER: u8 = 3;
const MAX_TRACKED_CANDIDATE_SPEAKERS: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AudioSpan {
    start_offset_ns: u64,
    end_offset_ns: u64,
}

impl AudioSpan {
    pub(crate) fn new(start_offset_ns: u64, end_offset_ns: u64) -> Option<Self> {
        (start_offset_ns < end_offset_ns).then_some(Self {
            start_offset_ns,
            end_offset_ns,
        })
    }

    pub(crate) const fn start_offset_ns(self) -> u64 {
        self.start_offset_ns
    }

    pub(crate) const fn end_offset_ns(self) -> u64 {
        self.end_offset_ns
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VoiceEvidenceSpan {
    pub(crate) speaker_id: SpeakerId,
    pub(crate) start_offset_ns: u64,
    pub(crate) end_offset_ns: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AutoIdentityMode {
    EmbedSortformerCandidates,
    ReuseWeSpeakerWindows,
}

pub(crate) const fn identity_source_allowed(
    origin: MeetingOrigin,
    source_kind: SourceKind,
) -> bool {
    !matches!(origin, MeetingOrigin::Import) && matches!(source_kind, SourceKind::SystemAudio)
}

pub(crate) fn automatic_identity_mode(
    origin: MeetingOrigin,
    source_kind: SourceKind,
    has_compatible_profiles: bool,
    engine: DiarizationEngineKind,
) -> Option<AutoIdentityMode> {
    if !identity_source_allowed(origin, source_kind) || !has_compatible_profiles {
        return None;
    }
    Some(match engine {
        DiarizationEngineKind::Sortformer => AutoIdentityMode::EmbedSortformerCandidates,
        DiarizationEngineKind::WeSpeaker => AutoIdentityMode::ReuseWeSpeakerWindows,
    })
}

pub(crate) fn map_sortformer_evidence(
    exclusive_spans: &[DiarizationSpeakerSpan],
    cluster_speakers: &HashMap<u32, SpeakerId>,
    contiguous_audio: &[AudioSpan],
) -> Vec<VoiceEvidenceSpan> {
    let mut evidence = Vec::new();
    for span in exclusive_spans {
        let Some(&speaker_id) = cluster_speakers.get(&span.cluster) else {
            continue;
        };
        for audio in contiguous_audio {
            let start_offset_ns = max(span.start_offset_ns, audio.start_offset_ns());
            let end_offset_ns = min(span.end_offset_ns, audio.end_offset_ns());
            if start_offset_ns < end_offset_ns {
                evidence.push(VoiceEvidenceSpan {
                    speaker_id,
                    start_offset_ns,
                    end_offset_ns,
                });
            }
        }
    }
    coalesce_evidence(evidence)
}

pub(crate) fn coalesce_evidence(mut evidence: Vec<VoiceEvidenceSpan>) -> Vec<VoiceEvidenceSpan> {
    evidence.sort_unstable_by_key(|span| (span.start_offset_ns, span.end_offset_ns));
    let mut coalesced: Vec<VoiceEvidenceSpan> = Vec::with_capacity(evidence.len());
    for span in evidence {
        match coalesced.last_mut() {
            Some(previous)
                if previous.speaker_id == span.speaker_id
                    && previous.end_offset_ns == span.start_offset_ns =>
            {
                previous.end_offset_ns = span.end_offset_ns;
            }
            _ => coalesced.push(span),
        }
    }
    coalesced
}

pub(crate) fn fallback_evidence_span(
    speaker_id: SpeakerId,
    assignment: SpeakerAssignmentKind,
    start_offset_ns: u64,
    end_offset_ns: u64,
    sample_count: usize,
    voiced_frames: u32,
) -> Option<VoiceEvidenceSpan> {
    fallback_window_is_eligible(
        assignment,
        start_offset_ns,
        end_offset_ns,
        sample_count,
        voiced_frames,
    )
    .then_some(VoiceEvidenceSpan {
        speaker_id,
        start_offset_ns,
        end_offset_ns,
    })
}

pub(crate) struct VoiceMatchCandidate {
    pub(crate) cluster: u32,
    pub(crate) embedding: SpeakerEmbedding,
}

/// The one capability the primary collector needs from a model: one normalized
/// vector for the samples it hands over. Naming it keeps the collector off the
/// streaming diarization session, whose online clustering can answer `Overlap`
/// for audio Sortformer already certified as a single speaker, and lets a test
/// drive the collector without an ONNX asset on disk.
pub(crate) trait SpeakerEmbedder {
    fn embed_candidate(&mut self, samples: &[f32]) -> Result<SpeakerEmbedding, DiarizationError>;
}

impl SpeakerEmbedder for SpeakerEmbeddingSession {
    fn embed_candidate(&mut self, samples: &[f32]) -> Result<SpeakerEmbedding, DiarizationError> {
        self.embed(samples)
    }
}

pub(crate) enum VoiceIdentityCollector {
    Primary(PrimaryCollector),
    Fallback(FallbackCollector),
}

impl VoiceIdentityCollector {
    pub(crate) fn primary(
        exclusive_spans: &[DiarizationSpeakerSpan],
        embedder: Box<dyn SpeakerEmbedder>,
    ) -> Self {
        Self::Primary(PrimaryCollector::new(exclusive_spans, embedder))
    }

    pub(crate) fn fallback() -> Self {
        Self::Fallback(FallbackCollector::default())
    }

    pub(crate) fn observe_primary_window(
        &mut self,
        samples: &[f32],
        start_offset_ns: u64,
        voice_frames: &[bool],
    ) {
        let Self::Primary(collector) = self else {
            return;
        };
        if !samples.len().is_multiple_of(VAD_FRAME_SAMPLES)
            || samples.len() / VAD_FRAME_SAMPLES != voice_frames.len()
        {
            return;
        }
        for (index, (frame, voice)) in samples
            .chunks_exact(VAD_FRAME_SAMPLES)
            .zip(voice_frames.iter().copied())
            .enumerate()
        {
            let Ok(index) = u64::try_from(index) else {
                return;
            };
            collector.observe_frame(
                voice,
                frame,
                start_offset_ns.saturating_add(index.saturating_mul(VAD_FRAME_NS)),
            );
        }
    }

    pub(crate) fn observe_fallback_window(
        &mut self,
        assignment: SpeakerAssignmentKind,
        cluster: Option<u32>,
        start_offset_ns: u64,
        end_offset_ns: u64,
        sample_count: usize,
        voiced_frames: u32,
        embedding: Option<SpeakerEmbedding>,
    ) {
        if let Self::Fallback(collector) = self {
            collector.observe_window(
                assignment,
                cluster,
                start_offset_ns,
                end_offset_ns,
                sample_count,
                voiced_frames,
                embedding,
            );
        }
    }

    pub(crate) fn reset(&mut self) {
        match self {
            Self::Primary(collector) => collector.reset(),
            Self::Fallback(collector) => collector.reset(),
        }
    }

    pub(crate) fn into_candidates(self) -> Vec<VoiceMatchCandidate> {
        match self {
            Self::Primary(collector) => collector.into_candidates(),
            Self::Fallback(collector) => collector.into_candidates(),
        }
    }
}

pub(crate) struct PrimaryCollector {
    exclusive_spans: Vec<DiarizationSpeakerSpan>,
    next_span: usize,
    current: Option<PrimaryCandidate>,
    samples: Vec<f32>,
    accumulators: BTreeMap<u32, EmbeddingAccumulator>,
    embedder: Box<dyn SpeakerEmbedder>,
    model_failed: bool,
}

impl PrimaryCollector {
    fn new(exclusive_spans: &[DiarizationSpeakerSpan], embedder: Box<dyn SpeakerEmbedder>) -> Self {
        Self {
            exclusive_spans: exclusive_spans.to_vec(),
            next_span: 0,
            current: None,
            samples: Vec::with_capacity(PRIMARY_CANDIDATE_SAMPLES),
            accumulators: BTreeMap::new(),
            embedder,
            model_failed: false,
        }
    }

    fn observe_frame(&mut self, voice: bool, frame: &[f32], start_offset_ns: u64) {
        if self.model_failed || frame.len() != VAD_FRAME_SAMPLES {
            return;
        }
        let end_offset_ns = start_offset_ns.saturating_add(VAD_FRAME_NS);
        let Some((span_index, cluster)) = self.span_for_frame(start_offset_ns, end_offset_ns)
        else {
            self.discard_current();
            return;
        };
        if self.current.as_ref().is_some_and(|candidate| {
            candidate.span_index != span_index || candidate.end_offset_ns != start_offset_ns
        }) {
            self.discard_current();
        }
        if self.current.is_none() {
            let Some(accumulator) = bounded_accumulator(&mut self.accumulators, cluster) else {
                return;
            };
            if !accumulator.start_attempt() {
                return;
            }
            self.current = Some(PrimaryCandidate {
                span_index,
                cluster,
                end_offset_ns: start_offset_ns,
                voiced_frames: 0,
            });
        }
        let Some(candidate) = self.current.as_mut() else {
            return;
        };
        candidate.end_offset_ns = end_offset_ns;
        if voice {
            candidate.voiced_frames = candidate.voiced_frames.saturating_add(1);
        }
        self.samples.extend_from_slice(frame);
        if self.samples.len() == PRIMARY_CANDIDATE_SAMPLES {
            self.complete_current();
        }
    }

    fn span_for_frame(&mut self, start_offset_ns: u64, end_offset_ns: u64) -> Option<(usize, u32)> {
        while let Some(span) = self.exclusive_spans.get(self.next_span) {
            if span.end_offset_ns <= start_offset_ns {
                self.next_span += 1;
                continue;
            }
            let guarded_start = span.start_offset_ns.checked_add(PRIMARY_GUARD_NS)?;
            let guarded_end = span.end_offset_ns.checked_sub(PRIMARY_GUARD_NS)?;
            if guarded_end.saturating_sub(guarded_start) < PRIMARY_CANDIDATE_NS {
                self.next_span += 1;
                continue;
            }
            if start_offset_ns >= guarded_end {
                self.next_span += 1;
                continue;
            }
            if start_offset_ns < guarded_start || end_offset_ns > guarded_end {
                return None;
            }
            return Some((self.next_span, span.cluster));
        }
        None
    }

    fn complete_current(&mut self) {
        let Some(candidate) = self.current.take() else {
            return;
        };
        let embedding =
            if primary_candidate_is_eligible(self.samples.len(), candidate.voiced_frames) {
                match self.embedder.embed_candidate(&self.samples) {
                    Ok(embedding) => Some(embedding),
                    Err(_) => {
                        self.model_failed = true;
                        self.accumulators.clear();
                        None
                    }
                }
            } else {
                None
            };
        if let Some(embedding) = embedding {
            if let Some(accumulator) =
                bounded_accumulator(&mut self.accumulators, candidate.cluster)
            {
                let _ = accumulator.accept(embedding, PRIMARY_CANDIDATE_NS);
            }
        }
        self.samples.clear();
    }

    fn discard_current(&mut self) {
        self.current = None;
        self.samples.clear();
    }

    fn reset(&mut self) {
        self.discard_current();
        self.accumulators.clear();
    }

    fn into_candidates(self) -> Vec<VoiceMatchCandidate> {
        self.accumulators
            .into_iter()
            .filter_map(|(cluster, accumulator)| {
                accumulator
                    .into_embedding(PRIMARY_MIN_CANDIDATES, PRIMARY_CANDIDATE_NS * 2)
                    .map(|embedding| VoiceMatchCandidate { cluster, embedding })
            })
            .collect()
    }
}

struct PrimaryCandidate {
    span_index: usize,
    cluster: u32,
    end_offset_ns: u64,
    voiced_frames: u32,
}

#[derive(Default)]
pub(crate) struct FallbackCollector {
    accumulators: BTreeMap<u32, EmbeddingAccumulator>,
    pending: Option<PendingFallbackWindow>,
}

impl FallbackCollector {
    #[allow(clippy::too_many_arguments)]
    fn observe_window(
        &mut self,
        assignment: SpeakerAssignmentKind,
        cluster: Option<u32>,
        start_offset_ns: u64,
        end_offset_ns: u64,
        sample_count: usize,
        voiced_frames: u32,
        embedding: Option<SpeakerEmbedding>,
    ) {
        let Some(cluster) = cluster else {
            self.pending = None;
            return;
        };
        let Some(embedding) = embedding else {
            self.pending = None;
            return;
        };
        if !fallback_window_is_eligible(
            assignment,
            start_offset_ns,
            end_offset_ns,
            sample_count,
            voiced_frames,
        ) {
            self.pending = None;
            return;
        }
        // The pending window is taken only when this window continues it. A
        // window that does not replaces it, which is what a gap inside one
        // cluster means for a pair that has to be contiguous.
        let Some(previous) = self.pending.take_if(|pending| {
            pending.cluster == cluster && pending.end_offset_ns == start_offset_ns
        }) else {
            self.pending = Some(PendingFallbackWindow {
                cluster,
                end_offset_ns,
                embedding,
                accepted: false,
            });
            return;
        };
        let Some(accumulator) = bounded_accumulator(&mut self.accumulators, cluster) else {
            self.pending = None;
            return;
        };
        if !previous.accepted {
            // Whether that earlier window was accepted is not tracked past
            // here: only the newest window can extend the next pair.
            let _ = accumulator.accept(previous.embedding, FALLBACK_WINDOW_NS);
        }
        let accepted = accumulator.accept(embedding.clone(), FALLBACK_WINDOW_NS);
        self.pending = Some(PendingFallbackWindow {
            cluster,
            end_offset_ns,
            embedding,
            accepted,
        });
    }

    fn reset(&mut self) {
        self.accumulators.clear();
        self.pending = None;
    }

    fn into_candidates(self) -> Vec<VoiceMatchCandidate> {
        self.accumulators
            .into_iter()
            .filter_map(|(cluster, accumulator)| {
                accumulator
                    .into_embedding(FALLBACK_MIN_CANDIDATES, FALLBACK_WINDOW_NS * 2)
                    .map(|embedding| VoiceMatchCandidate { cluster, embedding })
            })
            .collect()
    }
}

struct PendingFallbackWindow {
    cluster: u32,
    end_offset_ns: u64,
    embedding: SpeakerEmbedding,
    accepted: bool,
}

fn bounded_accumulator(
    accumulators: &mut BTreeMap<u32, EmbeddingAccumulator>,
    cluster: u32,
) -> Option<&mut EmbeddingAccumulator> {
    if !accumulators.contains_key(&cluster) && accumulators.len() >= MAX_TRACKED_CANDIDATE_SPEAKERS
    {
        return None;
    }
    Some(accumulators.entry(cluster).or_default())
}

#[derive(Clone)]
struct EmbeddingAccumulator {
    /// Bounded by `PrimaryCollector` alone. `FallbackCollector` never opens an
    /// attempt, so its accumulators are bounded by `accepted`.
    attempts: u8,
    accepted: u8,
    duration_ns: u64,
    sum: [f32; SPEAKER_EMBEDDING_DIMENSIONS],
}

impl Default for EmbeddingAccumulator {
    fn default() -> Self {
        Self {
            attempts: 0,
            accepted: 0,
            duration_ns: 0,
            sum: [0.0; SPEAKER_EMBEDDING_DIMENSIONS],
        }
    }
}

impl EmbeddingAccumulator {
    fn start_attempt(&mut self) -> bool {
        if self.attempts >= MAX_CANDIDATES_PER_SPEAKER {
            return false;
        }
        self.attempts += 1;
        true
    }

    fn accept(&mut self, embedding: SpeakerEmbedding, duration_ns: u64) -> bool {
        if self.accepted >= MAX_CANDIDATES_PER_SPEAKER {
            return false;
        }
        for (sum, value) in self.sum.iter_mut().zip(embedding.as_slice()) {
            *sum += value;
        }
        self.accepted += 1;
        self.duration_ns = self.duration_ns.saturating_add(duration_ns);
        true
    }

    fn into_embedding(
        self,
        minimum_candidates: u8,
        minimum_duration_ns: u64,
    ) -> Option<SpeakerEmbedding> {
        if self.accepted < minimum_candidates || self.duration_ns < minimum_duration_ns {
            return None;
        }
        let norm = self
            .sum
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        if !norm.is_finite() || norm <= f32::EPSILON {
            return None;
        }
        let mut normalized = self.sum;
        for value in &mut normalized {
            *value /= norm;
        }
        SpeakerEmbedding::from_normalized_slice(&normalized)
    }
}

fn primary_candidate_is_eligible(sample_count: usize, voiced_frames: u32) -> bool {
    sample_count == PRIMARY_CANDIDATE_SAMPLES && voiced_frames >= PRIMARY_MIN_VOICED_FRAMES
}

fn fallback_window_is_eligible(
    assignment: SpeakerAssignmentKind,
    start_offset_ns: u64,
    end_offset_ns: u64,
    sample_count: usize,
    voiced_frames: u32,
) -> bool {
    assignment == SpeakerAssignmentKind::SystemSpeaker
        && sample_count >= FALLBACK_WINDOW_SAMPLES
        && end_offset_ns.saturating_sub(start_offset_ns) >= FALLBACK_WINDOW_NS
        && voiced_frames >= FALLBACK_MIN_VOICED_FRAMES
}

#[cfg(test)]
mod tests {
    use super::super::diarization::match_speaker_profile;
    use super::*;
    use std::sync::{Arc, Mutex};

    fn span(cluster: u32, start_offset_ns: u64, end_offset_ns: u64) -> DiarizationSpeakerSpan {
        DiarizationSpeakerSpan {
            cluster,
            start_offset_ns,
            end_offset_ns,
        }
    }

    fn embedding(cosine: f32) -> SpeakerEmbedding {
        let mut values = [0.0; SPEAKER_EMBEDDING_DIMENSIONS];
        values[0] = cosine;
        values[1] = (1.0 - cosine * cosine).sqrt();
        SpeakerEmbedding::from_normalized_slice(&values).expect("unit vector")
    }

    #[test]
    fn sortformer_evidence_subtracts_gaps_maps_speakers_and_coalesces_only_neighbors() {
        let first = SpeakerId::new();
        let second = SpeakerId::new();
        let clusters = HashMap::from([(1, first), (2, second)]);
        let evidence = map_sortformer_evidence(
            &[
                span(1, 0, 4 * NS_PER_SECOND),
                span(1, 6 * NS_PER_SECOND, 12 * NS_PER_SECOND),
                span(9, 12 * NS_PER_SECOND, 14 * NS_PER_SECOND),
                span(2, 14 * NS_PER_SECOND, 16 * NS_PER_SECOND),
            ],
            &clusters,
            &[
                AudioSpan::new(0, 4 * NS_PER_SECOND).expect("first contiguous span"),
                AudioSpan::new(6 * NS_PER_SECOND, 16 * NS_PER_SECOND)
                    .expect("second contiguous span"),
            ],
        );

        assert_eq!(
            evidence,
            vec![
                VoiceEvidenceSpan {
                    speaker_id: first,
                    start_offset_ns: 0,
                    end_offset_ns: 4 * NS_PER_SECOND,
                },
                VoiceEvidenceSpan {
                    speaker_id: first,
                    start_offset_ns: 6 * NS_PER_SECOND,
                    end_offset_ns: 12 * NS_PER_SECOND,
                },
                VoiceEvidenceSpan {
                    speaker_id: second,
                    start_offset_ns: 14 * NS_PER_SECOND,
                    end_offset_ns: 16 * NS_PER_SECOND,
                },
            ]
        );
    }

    #[test]
    fn automatic_matching_never_opens_a_collector_without_compatible_system_audio_profiles() {
        assert_eq!(
            automatic_identity_mode(
                MeetingOrigin::Manual,
                SourceKind::SystemAudio,
                false,
                DiarizationEngineKind::Sortformer,
            ),
            None
        );
        assert_eq!(
            automatic_identity_mode(
                MeetingOrigin::Import,
                SourceKind::Microphone,
                true,
                DiarizationEngineKind::WeSpeaker,
            ),
            None
        );
        assert_eq!(
            automatic_identity_mode(
                MeetingOrigin::Manual,
                SourceKind::Microphone,
                true,
                DiarizationEngineKind::WeSpeaker,
            ),
            None
        );
    }

    #[test]
    fn fallback_candidates_need_two_contiguous_qualified_windows_and_cap_at_three() {
        let mut collector = VoiceIdentityCollector::fallback();
        let base = 8 * NS_PER_SECOND;
        for index in 0..4 {
            let start_offset_ns = base + index * FALLBACK_WINDOW_NS;
            collector.observe_fallback_window(
                SpeakerAssignmentKind::SystemSpeaker,
                Some(7),
                start_offset_ns,
                start_offset_ns + FALLBACK_WINDOW_NS,
                FALLBACK_WINDOW_SAMPLES,
                FALLBACK_MIN_VOICED_FRAMES,
                Some(embedding(1.0)),
            );
        }
        collector.observe_fallback_window(
            SpeakerAssignmentKind::SystemSpeaker,
            Some(7),
            base + 4 * FALLBACK_WINDOW_NS,
            base + 5 * FALLBACK_WINDOW_NS,
            FALLBACK_WINDOW_SAMPLES,
            FALLBACK_MIN_VOICED_FRAMES - 1,
            Some(embedding(1.0)),
        );

        let candidates = collector.into_candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].cluster, 7);
    }

    #[test]
    fn fallback_gap_reset_drops_pre_gap_embeddings() {
        let mut collector = VoiceIdentityCollector::fallback();
        for index in 0..2 {
            let start_offset_ns = index * FALLBACK_WINDOW_NS;
            collector.observe_fallback_window(
                SpeakerAssignmentKind::SystemSpeaker,
                Some(3),
                start_offset_ns,
                start_offset_ns + FALLBACK_WINDOW_NS,
                FALLBACK_WINDOW_SAMPLES,
                FALLBACK_MIN_VOICED_FRAMES,
                Some(embedding(1.0)),
            );
        }
        collector.reset();
        collector.observe_fallback_window(
            SpeakerAssignmentKind::SystemSpeaker,
            Some(3),
            20 * NS_PER_SECOND,
            20 * NS_PER_SECOND + FALLBACK_WINDOW_NS,
            FALLBACK_WINDOW_SAMPLES,
            FALLBACK_MIN_VOICED_FRAMES,
            Some(embedding(1.0)),
        );

        assert!(collector.into_candidates().is_empty());
    }

    #[test]
    fn primary_and_fallback_quality_floors_are_exact() {
        assert!(primary_candidate_is_eligible(
            PRIMARY_CANDIDATE_SAMPLES,
            PRIMARY_MIN_VOICED_FRAMES
        ));
        assert!(!primary_candidate_is_eligible(
            PRIMARY_CANDIDATE_SAMPLES,
            PRIMARY_MIN_VOICED_FRAMES - 1
        ));
        assert!(fallback_window_is_eligible(
            SpeakerAssignmentKind::SystemSpeaker,
            0,
            FALLBACK_WINDOW_NS,
            FALLBACK_WINDOW_SAMPLES,
            FALLBACK_MIN_VOICED_FRAMES,
        ));
        assert!(!fallback_window_is_eligible(
            SpeakerAssignmentKind::SystemSpeaker,
            0,
            FALLBACK_WINDOW_NS,
            FALLBACK_WINDOW_SAMPLES,
            FALLBACK_MIN_VOICED_FRAMES - 1,
        ));
    }

    #[test]
    fn matcher_accepts_the_exact_runner_up_margin() {
        let candidate = embedding(1.0);
        let best = embedding(0.85);
        let margin_boundary = embedding(0.77);
        assert_eq!(
            match_speaker_profile(&candidate, [(0, &best), (1, &margin_boundary)]),
            Some(0)
        );
    }

    struct RecordingEmbedder {
        sample_counts: Arc<Mutex<Vec<usize>>>,
    }

    impl SpeakerEmbedder for RecordingEmbedder {
        fn embed_candidate(
            &mut self,
            samples: &[f32],
        ) -> Result<SpeakerEmbedding, DiarizationError> {
            self.sample_counts
                .lock()
                .expect("sample counts")
                .push(samples.len());
            Ok(embedding(1.0))
        }
    }

    /// Both candidates cut from one exclusive span reach the accumulator. The
    /// collector used to embed through the streaming diarization session, whose
    /// clustering answers `Overlap` when a second candidate sits near two of
    /// its own centroids, which dropped the candidate and spent its attempt.
    #[test]
    fn primary_candidates_are_embedded_without_a_clustering_verdict() {
        let sample_counts = Arc::new(Mutex::new(Vec::new()));
        let mut collector = VoiceIdentityCollector::primary(
            &[span(5, 0, 7 * NS_PER_SECOND)],
            Box::new(RecordingEmbedder {
                sample_counts: Arc::clone(&sample_counts),
            }),
        );
        let samples = vec![0.5; PRIMARY_CANDIDATE_SAMPLES];
        let voice_frames = vec![true; PRIMARY_CANDIDATE_SAMPLES / VAD_FRAME_SAMPLES];
        for index in 0..2 {
            collector.observe_primary_window(
                &samples,
                PRIMARY_GUARD_NS + index * PRIMARY_CANDIDATE_NS,
                &voice_frames,
            );
        }

        assert_eq!(
            *sample_counts.lock().expect("sample counts"),
            vec![PRIMARY_CANDIDATE_SAMPLES; 2]
        );
        let candidates = collector.into_candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].cluster, 5);
    }
}
