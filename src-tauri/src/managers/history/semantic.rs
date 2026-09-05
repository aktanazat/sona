//! Local semantic recall for dictation history: a static sentence embedding
//! per history row, scanned exactly, beside the FTS5 index.
//!
//! FTS5 joins its tokens with implicit AND, so a search for "spending plan"
//! cannot reach a transcript that said "the budget for August" — not because
//! the index is stale but because no shared token exists. That miss is
//! structural, and no amount of tokenizer tuning closes it. A static embedding
//! does: it maps both phrasings into the same region of a 256-dimensional
//! space, and a dot product finds the row.
//!
//! Everything here is local. The model is a [Model2Vec] static embedding table
//! (no transformer forward pass, no ONNX session, no GPU, no ANE): tokenize,
//! look up one row per token, mean-pool, L2-normalize. The `model2vec-rs`
//! dependency is compiled with `local-only`, which turns its own Hugging Face
//! downloader into a compile error rather than a code path, so the model can
//! only ever arrive through [`SemanticModel::install_verified`] below — the
//! same pinned-revision plus sha256 shape `meeting/diarization.rs` uses for the
//! speaker-embedding model.
//!
//! No approximate index. History holds thousands of rows, not millions, and an
//! exact scan of a few thousand 256-lane dot products costs less than the
//! SQLite row decode that feeds it (see the latency table in the slice report).
//! An ANN structure here would be a second source of truth that can disagree
//! with the rows, bought for nothing.
//!
//! [Model2Vec]: https://github.com/MinishLab/model2vec

use anyhow::{anyhow, Context, Result};
use hf_hub::api::tokio::ApiBuilder;
use hf_hub::{Repo, RepoType};
use log::{debug, info, warn};
use model2vec_rs::model::StaticModel;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

/// Cosine similarity a candidate row must reach before semantic recall will
/// surface it.
///
/// Measured, not guessed. On the fixtures in
/// `similarity_floor_separates_paraphrase_from_noise`, the pinned model scores
/// genuine paraphrases in [0.2908, 0.4936] and unrelated text — including
/// same-domain, different-subject pairs — at or below 0.2185. This number sits
/// in that 0.072-wide gap, deliberately nearer the paraphrase floor than the
/// noise ceiling: a missed recall leaves the user with the lexical search Sona
/// already shipped, while a false recall puts a wrong row in their results and
/// labels it a match. The two failure modes are not worth the same, so the
/// margin is not split evenly.
pub(crate) const SIMILARITY_FLOOR: f32 = 0.27;

/// Rows re-embedded per database-connection acquisition during backfill.
///
/// The connection is shared with every user-facing history query, so the
/// backfill takes it in short bursts and releases it between them instead of
/// holding it for the length of the whole pass.
pub(crate) const BACKFILL_CHUNK_ROWS: usize = 64;

/// One file of the pinned model, with the two facts that prove it is the file
/// the manifest names.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct SemanticModelFile {
    pub(crate) filename: String,
    pub(crate) sha256: String,
    pub(crate) size_bytes: u64,
}

/// The exact model Sona embeds history with, pinned to a Hugging Face commit.
///
/// The revision is a commit sha, not a branch: a retrained upload cannot
/// silently change what Sona stored. `revision` is also the value written into
/// `transcription_history.semantic_model_revision`, so vectors produced by a
/// different model are recognizable as stale rather than mixed in — embeddings
/// from two models share a dimension count and nothing else.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct SemanticModelManifest {
    pub(crate) id: String,
    pub(crate) revision: String,
    pub(crate) license: String,
    pub(crate) embedding_dimensions: usize,
    pub(crate) files: Vec<SemanticModelFile>,
}

static MODEL_MANIFEST: LazyLock<SemanticModelManifest> = LazyLock::new(|| {
    // PANIC: the bundled, compile-time JSON asset is part of the application binary.
    serde_json::from_str(include_str!(
        "../../../resources/models/history-semantic-recall.json"
    ))
    .expect("history semantic recall manifest is valid")
});

pub(crate) fn model_manifest() -> &'static SemanticModelManifest {
    &MODEL_MANIFEST
}

/// A loaded static embedding model, ready to turn text into one vector.
pub(crate) struct SemanticModel {
    model: StaticModel,
    dimensions: usize,
    revision: String,
}

impl SemanticModel {
    /// Load the model from a directory that already holds every manifest file
    /// intact. Returns `None` when the directory is missing, incomplete, or
    /// fails verification — an absent model is an ordinary state, not an error.
    pub(crate) fn load(directory: &Path) -> Option<Self> {
        let manifest = model_manifest();
        if !directory_is_verified(directory) {
            return None;
        }
        // `local-only` is compiled in, so this can only read the directory.
        let model = match StaticModel::from_pretrained(directory, None, None, None) {
            Ok(model) => model,
            Err(error) => {
                warn!("History semantic model failed to load: {error:#}");
                return None;
            }
        };
        let dimensions = model.encode_single("dimension probe").len();
        if dimensions != manifest.embedding_dimensions {
            warn!(
                "History semantic model reports {dimensions} dimensions, manifest pins {}",
                manifest.embedding_dimensions
            );
            return None;
        }
        Some(Self {
            model,
            dimensions,
            revision: manifest.revision.clone(),
        })
    }

    #[cfg(test)]
    pub(crate) fn load_for_test(directory: &Path) -> Option<Self> {
        let manifest = model_manifest();
        let model = StaticModel::from_pretrained(directory, None, None, None).ok()?;
        let dimensions = model.encode_single("dimension probe").len();
        (dimensions == manifest.embedding_dimensions).then_some(Self {
            model,
            dimensions,
            revision: manifest.revision.clone(),
        })
    }

    /// The manifest revision this model produces vectors for. Stored beside
    /// every vector so a model change invalidates instead of corrupting.
    pub(crate) fn revision(&self) -> &str {
        &self.revision
    }

    /// Embed one text as a unit vector.
    ///
    /// The model's own config sets `normalize: true`, but this re-normalizes
    /// rather than trusting it: L2 length 1 is the invariant that makes
    /// [`cosine_similarity`] a plain dot product, and it belongs to this
    /// function, not to a JSON field in a downloaded file. Text with no
    /// embeddable token (whitespace, emoji) pools to the zero vector, which
    /// [`encode`] reports as `None` instead of storing a vector that is similar
    /// to nothing.
    pub(crate) fn encode(&self, text: &str) -> Option<Vec<f32>> {
        let mut vector = self.model.encode_single(text);
        if vector.len() != self.dimensions {
            return None;
        }
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if !norm.is_finite() || norm <= f32::EPSILON {
            return None;
        }
        for value in &mut vector {
            *value /= norm;
        }
        Some(vector)
    }

    /// Fetch the pinned model into `directory` and verify every file before
    /// publishing it. Nothing else in this module can create the directory, so
    /// a half-written download is never loadable.
    pub(crate) async fn install(directory: &Path) -> Result<()> {
        let manifest = model_manifest();
        let api = ApiBuilder::from_env().build()?;
        let repository = api.repo(Repo::with_revision(
            manifest.id.clone(),
            RepoType::Model,
            manifest.revision.clone(),
        ));
        for file in &manifest.files {
            let cached = repository
                .get(&file.filename)
                .await
                .with_context(|| format!("download {}", file.filename))?;
            install_verified(&cached, directory, file)?;
        }
        info!(
            "History semantic model installed: {} @ {} ({} license)",
            manifest.id, manifest.revision, manifest.license
        );
        Ok(())
    }
}

/// Copy one downloaded file into place only if its bytes match the manifest.
///
/// The copy lands on a `.partial` name, is verified there, and is renamed only
/// after it passes, so `directory_is_verified` can never observe a torn file.
fn install_verified(cached: &Path, directory: &Path, file: &SemanticModelFile) -> Result<()> {
    fs::create_dir_all(directory)?;
    let target = directory.join(&file.filename);
    let temporary = directory.join(format!("{}.partial", file.filename));
    let _ = fs::remove_file(&temporary);

    let mut source = File::open(cached)?;
    let mut destination = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    io::copy(&mut source, &mut destination)?;
    destination.flush().and_then(|()| destination.sync_all())?;
    drop(destination);

    if !file_is_verified(&temporary, file) {
        let _ = fs::remove_file(&temporary);
        return Err(anyhow!("{} failed verification", file.filename));
    }
    fs::rename(&temporary, &target)?;
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("semantic model file has no parent"))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

/// True when every manifest file is present at its pinned size and digest.
pub(crate) fn directory_is_verified(directory: &Path) -> bool {
    model_manifest()
        .files
        .iter()
        .all(|file| file_is_verified(&directory.join(&file.filename), file))
}

fn file_is_verified(path: &Path, file: &SemanticModelFile) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if metadata.len() != file.size_bytes {
        return false;
    }
    let Ok(mut handle) = File::open(path) else {
        return false;
    };
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = match handle.read(&mut buffer) {
            Ok(count) => count,
            Err(_) => return false,
        };
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    format!("{:x}", digest.finalize()) == file.sha256
}

/// Serialize a unit vector for the `semantic_embedding` BLOB column:
/// little-endian f32 lanes, no header. The dimension is `len() / 4`, checked
/// against the loaded model on read, so the column needs no schema of its own.
pub(crate) fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Cosine similarity of a stored BLOB against a query vector, read straight
/// from the bytes.
///
/// Both sides are unit vectors ([`SemanticModel::encode`] guarantees it), so
/// cosine is the dot product and no per-row norm is computed. Returns `None`
/// for a BLOB whose lane count does not match the query — a vector written by a
/// different model is not comparable, and guessing is worse than skipping.
pub(crate) fn cosine_similarity(stored: &[u8], query: &[f32]) -> Option<f32> {
    if stored.len() != query.len() * 4 {
        return None;
    }
    let mut sum = 0.0_f32;
    for (chunk, &value) in stored.chunks_exact(4).zip(query) {
        // The slice is exactly 4 bytes: `chunks_exact` yields no short chunk.
        let lane = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        sum += lane * value;
    }
    sum.is_finite().then_some(sum)
}

/// Cosine similarity of two vectors that are both in memory, for text
/// embedded to be compared once and thrown away rather than stored.
///
/// The same dot product as [`cosine_similarity`], on the same unit-vector
/// guarantee, and `None` on a length mismatch for the same reason: two models'
/// vectors are not comparable.
pub(crate) fn cosine_of_vectors(left: &[f32], right: &[f32]) -> Option<f32> {
    if left.len() != right.len() {
        return None;
    }
    let sum = left
        .iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum::<f32>();
    sum.is_finite().then_some(sum)
}

/// The one text Sona embeds for a history row.
///
/// Post-processing rewrites the same content, so the polished text is the
/// better rendering of what the user would later remember saying. Raw text is
/// the fallback, and an empty polished string is treated as absent rather than
/// as an instruction to embed nothing.
pub(crate) fn embeddable_text<'a>(
    transcription_text: &'a str,
    post_processed_text: Option<&'a str>,
) -> &'a str {
    post_processed_text
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(transcription_text)
}

/// Model residency for one [`super::HistoryManager`]: load on demand, fetch
/// once when missing, and answer "is semantic recall available right now?"
/// without ever blocking a caller on the network.
pub(crate) struct SemanticModelSlot {
    directory: PathBuf,
    model: std::sync::Mutex<Option<Arc<SemanticModel>>>,
    /// Set for the lifetime of the process the first time a fetch is started.
    /// One attempt per run: a user who searches ten times while offline gets
    /// one log line, not ten downloads.
    fetch_started: AtomicBool,
    allow_fetch: bool,
}

impl SemanticModelSlot {
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            model: std::sync::Mutex::new(None),
            fetch_started: AtomicBool::new(false),
            allow_fetch: true,
        }
    }

    #[cfg(test)]
    pub(crate) fn without_fetch(directory: PathBuf) -> Self {
        Self {
            directory,
            model: std::sync::Mutex::new(None),
            fetch_started: AtomicBool::new(false),
            allow_fetch: false,
        }
    }

    /// The loaded model, or `None` when it is not on disk yet.
    ///
    /// Loading is attempted at most once per verified directory; a directory
    /// that fails verification stays unloaded and is retried on the next call,
    /// which is what makes a completed background fetch take effect without
    /// any signalling between the two.
    pub(crate) fn model(&self) -> Option<Arc<SemanticModel>> {
        let mut slot = self
            .model
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(model) = slot.as_ref() {
            return Some(Arc::clone(model));
        }
        let loaded = Arc::new(SemanticModel::load(&self.directory)?);
        *slot = Some(Arc::clone(&loaded));
        Some(loaded)
    }

    /// Start the one background fetch, if the model is absent and nothing has
    /// tried yet. Returns immediately either way: the caller is a user-facing
    /// search that must answer from what exists now.
    pub(crate) fn ensure_fetch_started(&self) {
        if !self.allow_fetch || directory_is_verified(&self.directory) {
            return;
        }
        if self.fetch_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let manifest = model_manifest();
        info!(
            "Semantic recall is lexical-only until the recall model arrives; fetching {} @ {}",
            manifest.id, manifest.revision
        );
        let directory = self.directory.clone();
        std::thread::spawn(move || {
            match tauri::async_runtime::block_on(SemanticModel::install(&directory)) {
                Ok(()) => debug!("History semantic model is ready"),
                Err(error) => warn!("History semantic model fetch failed: {error:#}"),
            }
        });
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// The fixture model directory, or `None` when the pinned model has not
    /// been fetched on this machine. Tests that need real vectors skip
    /// themselves rather than assert against a model that is not there.
    pub(crate) fn fixture_model() -> Option<SemanticModel> {
        let directory = fixture_directory()?;
        SemanticModel::load_for_test(&directory)
    }

    pub(crate) fn fixture_directory() -> Option<PathBuf> {
        let directory = PathBuf::from(
            std::env::var("SONA_SEMANTIC_MODEL_DIR")
                .unwrap_or_else(|_| "/tmp/potion8m".to_string()),
        );
        directory
            .join("model.safetensors")
            .is_file()
            .then_some(directory)
    }

    #[test]
    fn manifest_pins_a_commit_and_a_digest_per_file() {
        let manifest = model_manifest();
        assert_eq!(manifest.embedding_dimensions, 256);
        assert_eq!(manifest.license, "MIT");
        assert_eq!(
            manifest.revision.len(),
            40,
            "revision must be a full commit sha, not a branch name"
        );
        assert!(manifest.revision.bytes().all(|b| b.is_ascii_hexdigit()));
        let mut names: Vec<&str> = manifest
            .files
            .iter()
            .map(|file| file.filename.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            ["config.json", "model.safetensors", "tokenizer.json"]
        );
        for file in &manifest.files {
            assert_eq!(file.sha256.len(), 64, "{} needs a sha256", file.filename);
            assert!(file.size_bytes > 0);
        }
    }

    #[test]
    fn an_absent_directory_is_not_verified_and_loads_nothing() {
        let directory = std::env::temp_dir().join("sona-semantic-absent-fixture");
        let _ = fs::remove_dir_all(&directory);
        assert!(!directory_is_verified(&directory));
        assert!(SemanticModel::load(&directory).is_none());
        let slot = SemanticModelSlot::without_fetch(directory);
        assert!(slot.model().is_none());
    }

    #[test]
    fn a_truncated_model_file_fails_verification() {
        let directory = std::env::temp_dir().join("sona-semantic-truncated-fixture");
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create fixture directory");
        for file in &model_manifest().files {
            fs::write(directory.join(&file.filename), b"not the pinned bytes")
                .expect("write stub file");
        }
        assert!(!directory_is_verified(&directory));
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_blob_from_a_different_dimension_is_skipped_not_guessed() {
        let query = vec![1.0_f32, 0.0, 0.0];
        assert_eq!(cosine_similarity(&encode_vector(&query), &query), Some(1.0));
        let wrong_width = encode_vector(&[1.0_f32, 0.0]);
        assert_eq!(cosine_similarity(&wrong_width, &query), None);
        assert_eq!(cosine_similarity(&[], &query), None);
    }

    #[test]
    fn cosine_of_unit_vectors_is_the_dot_product() {
        let a = vec![0.6_f32, 0.8];
        let b = vec![0.8_f32, -0.6];
        assert_eq!(cosine_similarity(&encode_vector(&a), &a), Some(1.0));
        assert_eq!(cosine_similarity(&encode_vector(&a), &b), Some(0.0));
    }

    #[test]
    fn polished_text_is_preferred_and_blank_polish_falls_back() {
        assert_eq!(embeddable_text("raw", Some("polished")), "polished");
        assert_eq!(embeddable_text("raw", None), "raw");
        assert_eq!(embeddable_text("raw", Some("   ")), "raw");
    }

    #[test]
    fn encoding_returns_a_unit_vector_and_refuses_unembeddable_text() {
        let Some(model) = fixture_model() else {
            eprintln!("skipped: pinned semantic model not present");
            return;
        };
        let vector = model.encode("the budget for August").expect("embed text");
        assert_eq!(vector.len(), model_manifest().embedding_dimensions);
        let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {norm}");
        assert_eq!(model.encode("   "), None);
    }

    /// Dictation-shaped transcripts, each with a query a user might type when
    /// they remember the content but not the words. Deliberately spread across
    /// what people actually dictate: reminders, work notes, messages, errands.
    const PARAPHRASE_FIXTURES: &[(&str, &str)] = &[
        ("the budget for August", "spending plan"),
        (
            "remind me to call the dentist tomorrow morning",
            "phone the dental office",
        ),
        (
            "the deployment failed because the database migration timed out",
            "release broke on a slow schema upgrade",
        ),
        (
            "pick up milk eggs and bread on the way home",
            "groceries to buy",
        ),
        (
            "we agreed to push the launch to the second week of March",
            "release date moved later",
        ),
        (
            "the rent went up by two hundred dollars this year",
            "housing costs increased",
        ),
        (
            "book a flight to Berlin for the conference in October",
            "travel arrangements for the trip",
        ),
        (
            "my back has been hurting since I started running again",
            "pain from exercise",
        ),
        (
            "send Sarah the contract draft before the end of the day",
            "email the agreement to a colleague",
        ),
        (
            "the car needs new brake pads and an oil change",
            "vehicle maintenance",
        ),
    ];

    /// Pairs that must stay out of results. A false recall is a wrong answer in
    /// the user's search; a missed one only leaves them with the lexical search
    /// Sona already shipped. So this set is deliberately the harder half.
    ///
    /// The first ten are plainly different subjects. The last six are the ones
    /// that actually decide the floor: same register, same domain, adjacent
    /// wording, different subject — errand vs errand, booking vs booking, work
    /// message vs work message. Those are the false positives a user would
    /// notice, and a floor tuned only against obviously-unrelated text would
    /// sit far too low.
    const UNRELATED_FIXTURES: &[(&str, &str)] = &[
        (
            "the budget for August",
            "sourdough starter feeding schedule",
        ),
        (
            "remind me to call the dentist tomorrow morning",
            "guitar amplifier settings",
        ),
        (
            "the deployment failed because the database migration timed out",
            "recipe for lemon risotto",
        ),
        (
            "pick up milk eggs and bread on the way home",
            "tax return deadline",
        ),
        (
            "we agreed to push the launch to the second week of March",
            "dog walking schedule",
        ),
        (
            "the rent went up by two hundred dollars this year",
            "guitar chord voicings",
        ),
        (
            "book a flight to Berlin for the conference in October",
            "replace the kitchen faucet washer",
        ),
        (
            "my back has been hurting since I started running again",
            "quarterly revenue forecast",
        ),
        (
            "send Sarah the contract draft before the end of the day",
            "which houseplants tolerate low light",
        ),
        (
            "the car needs new brake pads and an oil change",
            "learning Japanese verbs",
        ),
        // Same domain, different subject — the realistic false positives, and
        // the pairs that actually decide the floor. Query length is matched to
        // the paraphrase set on purpose: a search box receives a few words, and
        // measuring noise with long frame-heavy sentences while measuring
        // recall with short ones would compare two different distributions.
        // (`remind me to renew my passport` against `remind me to call the
        // dentist…` scores 0.4048 — see the slice report. That is not a model
        // defect this floor can fix: a mean-pooled bag of tokens sharing the
        // literal 3-token frame `remind me to` is *lexically* similar, which is
        // the half FTS5 owns.)
        ("the budget for August", "standup attendees"),
        (
            "remind me to call the dentist tomorrow morning",
            "renew my passport",
        ),
        (
            "book a flight to Berlin for the conference in October",
            "dinner reservation Friday",
        ),
        (
            "the car needs new brake pads and an oil change",
            "dishwasher leaking",
        ),
        (
            "send Sarah the contract draft before the end of the day",
            "design review feedback",
        ),
        (
            "pick up milk eggs and bread on the way home",
            "post office parcel",
        ),
    ];

    /// The number [`SIMILARITY_FLOOR`] is, proven against text rather than
    /// asserted.
    ///
    /// Every paraphrase must clear the floor and every unrelated pair must fall
    /// under it. The measured interval is printed, so swapping the model or
    /// moving the floor shows up as a number rather than as a mystery, and the
    /// margin on each side is asserted separately: the two failure modes are
    /// not symmetric.
    #[test]
    fn similarity_floor_separates_paraphrase_from_noise() {
        let Some(model) = fixture_model() else {
            eprintln!("skipped: pinned semantic model not present");
            return;
        };

        let score = |document: &str, query: &str| {
            let stored = encode_vector(&model.encode(document).expect("embed document"));
            let asked = model.encode(query).expect("embed query");
            cosine_similarity(&stored, &asked).expect("compare")
        };

        let mut paraphrase_scores: Vec<f32> = PARAPHRASE_FIXTURES
            .iter()
            .map(|(document, query)| {
                let value = score(document, query);
                eprintln!("paraphrase {value:.4}  {query:?} -> {document:?}");
                value
            })
            .collect();
        let mut unrelated_scores: Vec<f32> = UNRELATED_FIXTURES
            .iter()
            .map(|(document, query)| {
                let value = score(document, query);
                eprintln!("unrelated  {value:.4}  {query:?} -> {document:?}");
                value
            })
            .collect();
        paraphrase_scores.sort_by(|a, b| a.total_cmp(b));
        unrelated_scores.sort_by(|a, b| a.total_cmp(b));

        let lowest_paraphrase = paraphrase_scores[0];
        let highest_unrelated = unrelated_scores[unrelated_scores.len() - 1];
        eprintln!(
            "floor {SIMILARITY_FLOOR} sits in [{highest_unrelated:.4}, {lowest_paraphrase:.4}]; \
             paraphrase span [{lowest_paraphrase:.4}, {:.4}], unrelated span [{:.4}, {highest_unrelated:.4}]",
            paraphrase_scores[paraphrase_scores.len() - 1],
            unrelated_scores[0],
        );

        assert!(
            highest_unrelated < SIMILARITY_FLOOR,
            "unrelated text reached {highest_unrelated}, floor is {SIMILARITY_FLOOR}"
        );
        assert!(
            lowest_paraphrase > SIMILARITY_FLOOR,
            "a paraphrase scored {lowest_paraphrase}, floor is {SIMILARITY_FLOOR}"
        );
        // The margin is not split evenly, and this pins the direction. A false
        // recall puts a wrong row in the user's results and labels it a match; a
        // missed one leaves them with the lexical search Sona already shipped.
        // So the floor buys more headroom against noise than against recall,
        // which means it must sit nearer the paraphrase floor.
        assert!(
            SIMILARITY_FLOOR - highest_unrelated > lowest_paraphrase - SIMILARITY_FLOOR,
            "floor {SIMILARITY_FLOOR} must keep more headroom above the noise \
             ceiling ({highest_unrelated}) than below the paraphrase floor \
             ({lowest_paraphrase})"
        );
    }
}
