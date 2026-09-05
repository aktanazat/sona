//! Semantic recall for meetings: the dictation index, widened.
//!
//! The model, the similarity floor and the vector encoding all come from
//! `managers::history::semantic` — the same static embedding table, the same
//! measured floor, the same little-endian lanes. Nothing about the maths is
//! new here. What is new is the corpus: a meeting's summary and its transcript,
//! which FTS5 reaches only by literal word (and the summary not at all, because
//! generated notes were never written into `meeting_search_documents`).
//!
//! # Where the vectors live
//!
//! In the meeting database, beside the words they were derived from. Putting
//! them in `history.db` next to the dictation index would have been fewer
//! tables and one more copy of transcript text living outside the retention
//! sweep, the encryption key, and the cascade that deletes a meeting — a
//! meeting the user deleted would have kept its sentences in another file. The
//! model stays where it is; only the storage follows the corpus.
//!
//! # When they are built
//!
//! At artifact completion, by [`index_after_artifact`] — the moment the words a
//! reader will search for become final. Meetings that finished before this
//! index existed are picked up a couple at a time by [`top_up_index`] on
//! search, which terminates because every pass writes an index-state row, even
//! for a meeting with nothing embeddable in it. Correctness never depends on
//! the push: the state row records *what* was indexed, so a missed
//! notification costs latency, not accuracy.

use crate::audio_toolkit::spoken_edits::SENTENCE_END_MARKS;
use crate::managers::history::semantic::{
    cosine_of_vectors, cosine_similarity, encode_vector, SemanticModel, SIMILARITY_FLOOR,
};
use crate::managers::history::HistoryManager;
use crate::meeting::store::query_plane::MeetingQueryCandidate;
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::MeetingSessionId;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// Characters of transcript per chunk.
///
/// A static embedding mean-pools its tokens, so the unit matters twice over: a
/// whole meeting in one vector averages every subject into none of them, and a
/// single transcript segment is often five words of back-channel. A few
/// sentences is the granularity a person actually remembers and asks about.
const TARGET_CHUNK_CHARS: usize = 480;

/// Index one session's text, replacing whatever was there.
///
/// Returns whether the session had anything to index — `false` means it has no
/// current transcript revision or has been deleted, which is not a failure.
pub(crate) fn index_session(
    store: &MeetingStore,
    model: &SemanticModel,
    session_id: MeetingSessionId,
) -> Result<bool, StoreError> {
    let Some(inputs) = store.semantic_index_inputs(session_id)? else {
        return Ok(false);
    };
    let mut chunks = Vec::new();
    // Summaries first, each on its own: the headline and the notes summary are
    // already one distilled thought apiece, and merging them with transcript
    // text would dilute both.
    for text in inputs
        .summaries
        .iter()
        .cloned()
        .chain(chunk_transcript(&inputs.transcript))
    {
        if let Some(vector) = model.encode(&text) {
            chunks.push((text, encode_vector(&vector)));
        }
    }
    // Written even when `chunks` is empty. That row is what stops the backfill
    // from selecting a wordless meeting on every search for the rest of time.
    store.replace_semantic_chunks(
        session_id,
        &inputs.key,
        model.revision(),
        utc_now_ms(),
        &chunks,
    )?;
    Ok(true)
}

/// The artifact-completion hook.
///
/// Called with the store already in hand, on the processing job's own thread:
/// embedding a meeting is tokenize-and-look-up, not inference, and this is
/// already the background pass that just spent minutes transcribing. Failure is
/// logged and dropped — an unindexed meeting is still fully searchable by word,
/// and no receipt may wait on a cache.
pub fn index_after_artifact(
    app: Option<&AppHandle>,
    store: &MeetingStore,
    session_id: MeetingSessionId,
) {
    let Some(app) = app else {
        return;
    };
    let Some(history) = app.try_state::<Arc<HistoryManager>>() else {
        return;
    };
    // No fetch is started here. Whether this machine downloads the recall model
    // is dictation search's decision to make, and finishing a meeting is not
    // consent to a network round trip. `top_up_index` collects this session
    // once the model does arrive.
    let Some(model) = history.semantic_model() else {
        return;
    };
    match index_session(store, &model, session_id) {
        Ok(_) => {}
        Err(error) => log::warn!("Meeting semantic index skipped {session_id:?}: {error:?}"),
    }
}

/// Build the index for up to `limit` meetings that need it. Best effort by
/// design: this runs inside a search, and a search must answer.
pub(crate) fn top_up_index(store: &MeetingStore, model: &SemanticModel, limit: usize) {
    let targets = match store.semantic_index_targets(model.revision(), limit) {
        Ok(targets) => targets,
        Err(error) => {
            log::warn!("Meeting semantic backfill could not list targets: {error:?}");
            return;
        }
    };
    for session_id in targets {
        if let Err(error) = index_session(store, model, session_id) {
            log::warn!("Meeting semantic backfill skipped {session_id:?}: {error:?}");
        }
    }
}

/// Meetings recalled by meaning rather than by word.
///
/// One row per meeting — its best-scoring chunk — so a meeting that circles a
/// subject for an hour does not crowd out every other answer. Rows below the
/// floor are not weak matches, they are absent: the floor is what keeps
/// "everything is a little bit similar to everything" out of a search box.
///
/// The chunk is what scores; the sentence inside it is what the reader is
/// shown. A chunk is a few sentences wide because that is the unit that embeds
/// to a subject, and quoting all of it hands an answering model 480 characters
/// of which one clause is the reason the row is there. So the winning chunk is
/// re-scored sentence by sentence, against the same query vector, and the
/// nearest sentence becomes the snippet.
pub(crate) fn meeting_matches(
    store: &MeetingStore,
    model: &SemanticModel,
    query: &str,
    before_utc_ms: Option<i64>,
    limit: usize,
) -> Result<Vec<MeetingQueryCandidate>, StoreError> {
    let Some(vector) = model.encode(query) else {
        return Ok(Vec::new());
    };
    let rows = store.query_semantic_chunk_vectors(model.revision(), before_utc_ms)?;
    let mut best: HashMap<MeetingSessionId, (f32, i64, i64)> = HashMap::new();
    for row in rows {
        let Some(score) = cosine_similarity(&row.embedding, &vector) else {
            continue;
        };
        if score < SIMILARITY_FLOOR {
            continue;
        }
        let entry = best
            .entry(row.session_id)
            .or_insert((score, row.chunk_id, row.when_utc_ms));
        if score > entry.0 {
            *entry = (score, row.chunk_id, row.when_utc_ms);
        }
    }
    let mut ranked = best.into_iter().collect::<Vec<_>>();
    // Recency picks which matches fit on the page, because recency is the page
    // order; the floor above is what decided they belong on it at all.
    ranked.sort_by(|(left_id, left), (right_id, right)| {
        right
            .2
            .cmp(&left.2)
            .then_with(|| left_id.uuid().cmp(&right_id.uuid()))
    });
    ranked.truncate(limit);
    let chunk_ids = ranked
        .iter()
        .map(|(_, (_, chunk_id, _))| *chunk_id)
        .collect::<Vec<_>>();
    let mut candidates = store.query_meetings_by_chunk(&chunk_ids)?;
    for candidate in &mut candidates {
        // Bounded by construction: only the chunks that reached the page are
        // split, and a chunk is at most a handful of sentences.
        let sentence = best_sentence(&candidate.snippet, &vector, |text| model.encode(text))
            .map(str::to_owned);
        if let Some(sentence) = sentence {
            candidate.snippet = sentence;
        }
    }
    Ok(candidates)
}

/// Consecutive transcript segments joined up to [`TARGET_CHUNK_CHARS`].
///
/// Segments are never split: a chunk overshoots rather than cutting a sentence
/// in half, because half a sentence embeds to half a meaning.
fn chunk_transcript(segments: &[String]) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for segment in segments {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        if !current.is_empty() {
            if current.chars().count() >= TARGET_CHUNK_CHARS {
                chunks.push(std::mem::take(&mut current));
            } else {
                current.push(' ');
            }
        }
        current.push_str(segment);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// The sentence of `chunk` nearest to `query`, or `None` when `chunk` is one
/// sentence and is therefore already the text that matched.
///
/// No floor applies here. The chunk cleared it, and this is not a second
/// membership decision: it picks which words of an answer that already belongs
/// on the page are the ones to show. A sentence the model cannot embed — no
/// token it knows — loses to any sentence it can, and a chunk of nothing but
/// such sentences keeps the chunk.
///
/// `embed` is the model the chunk was embedded with, taken as a function so
/// the choice can be tested against a vocabulary a test writes rather than a
/// 28.8MB download.
fn best_sentence<'chunk>(
    chunk: &'chunk str,
    query: &[f32],
    embed: impl Fn(&str) -> Option<Vec<f32>>,
) -> Option<&'chunk str> {
    let sentences = sentences(chunk);
    if sentences.len() < 2 {
        return None;
    }
    let mut best: Option<(f32, &str)> = None;
    for sentence in sentences {
        let Some(score) = embed(sentence)
            .as_deref()
            .and_then(|vector| cosine_of_vectors(vector, query))
        else {
            continue;
        };
        if best.is_none_or(|(highest, _)| score > highest) {
            best = Some((score, sentence));
        }
    }
    best.map(|(_, sentence)| sentence)
}

/// One sentence per element, terminators kept, in the order they were said.
///
/// A mark closes a sentence only when whitespace or the end of the chunk
/// follows it. So a decimal keeps its dot, a run of marks (`Wait?!`) closes
/// once at the end of the run, and a line break — a mark that is whitespace
/// itself — closes on its own.
///
/// An abbreviation before a space (`e.g. this`) does split, and the fragment
/// it leaves is not a sentence. Nothing reads it as one: the fragment competes
/// for the snippet on its own embedding, and three characters of `e.g.` score
/// below whichever sentence carries the subject.
fn sentences(chunk: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut characters = chunk.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        if !SENTENCE_END_MARKS.contains(&character) {
            continue;
        }
        let closes = character.is_whitespace()
            || characters
                .peek()
                .is_none_or(|(_, next)| next.is_whitespace());
        if !closes {
            continue;
        }
        let end = index + character.len_utf8();
        let sentence = chunk[start..end].trim();
        if !sentence.is_empty() {
            sentences.push(sentence);
        }
        start = end;
    }
    let tail = chunk[start..].trim();
    if !tail.is_empty() {
        sentences.push(tail);
    }
    sentences
}

fn utc_now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_chunks_join_segments_without_splitting_one() {
        let segments = vec![
            "a".repeat(TARGET_CHUNK_CHARS - 10),
            "the pricing tier question came back".to_string(),
            "b".repeat(TARGET_CHUNK_CHARS),
        ];

        let chunks = chunk_transcript(&segments);

        assert_eq!(chunks.len(), 2, "{chunks:?}");
        assert!(
            chunks[0].ends_with("the pricing tier question came back"),
            "a segment that overshoots the target still lands whole"
        );
        assert_eq!(chunks[1], "b".repeat(TARGET_CHUNK_CHARS));
    }

    #[test]
    fn empty_and_blank_segments_produce_no_chunks() {
        assert!(chunk_transcript(&[]).is_empty());
        assert!(chunk_transcript(&["   ".to_string(), "\n".to_string()]).is_empty());
    }

    /// A stand-in for the static embedding table: one lane per word of a
    /// vocabulary this test writes, mean-pooled and normalised the way
    /// [`SemanticModel::encode`] normalises, and `None` for text holding no
    /// word it knows — the same answer the real encoder gives text of no known
    /// tokens.
    ///
    /// It reproduces the one property the sentence pick rests on: text about
    /// the query's words scores above text about other words. The real table
    /// is a 28.8MB download, and a test that runs only where somebody fetched
    /// it reports nothing about this machine.
    fn embed(text: &str) -> Option<Vec<f32>> {
        const VOCABULARY: [&str; 6] = ["beef", "supplier", "price", "deck", "friday", "tier"];

        let mut vector = vec![0.0_f32; VOCABULARY.len()];
        for word in text.split_whitespace() {
            let word = word.trim_matches(|character: char| !character.is_alphanumeric());
            if let Some(lane) = VOCABULARY
                .iter()
                .position(|known| known.eq_ignore_ascii_case(word))
            {
                vector[lane] += 1.0;
            }
        }
        let length = vector.iter().map(|lane| lane * lane).sum::<f32>().sqrt();
        (length > 0.0).then(|| vector.iter().map(|lane| lane / length).collect())
    }

    /// The defect this exists to close: a reader asked about beef and was
    /// handed 227 characters that never mention it.
    #[test]
    fn the_snippet_is_the_sentence_that_matched_not_the_chunk_around_it() {
        let query = embed("beef").expect("the query names a known word");

        let sentence = best_sentence(
            "The deck goes out on Friday. Our beef supplier raised the price again.",
            &query,
            embed,
        );

        assert_eq!(sentence, Some("Our beef supplier raised the price again."));
    }

    #[test]
    fn a_one_sentence_chunk_is_already_the_text_that_matched() {
        let query = embed("beef").expect("the query names a known word");

        assert_eq!(
            best_sentence("Our beef supplier raised the price.", &query, embed),
            None,
            "there is nothing to narrow, so the caller keeps the chunk it has"
        );
    }

    #[test]
    fn a_chunk_the_model_knows_no_word_of_keeps_its_own_text() {
        let query = embed("beef").expect("the query names a known word");

        assert_eq!(
            best_sentence("Zzz qqq. Xyzzy plugh.", &query, embed),
            None,
            "no sentence could be scored, so none of them may be called the match"
        );
    }

    #[test]
    fn a_mark_closes_a_sentence_only_when_space_or_nothing_follows_it() {
        assert_eq!(
            sentences("The tier is 3.5x cheaper. Wait?! Yes.\nSend the deck"),
            [
                "The tier is 3.5x cheaper.",
                "Wait?!",
                "Yes.",
                "Send the deck",
            ],
            "a decimal keeps its dot, a run of marks closes once, a line break \
             closes on its own, and an unterminated tail is still a sentence"
        );
    }

    /// The fixture model directory, the same one
    /// `managers::history::semantic`'s own tests use: `SONA_SEMANTIC_MODEL_DIR`
    /// or the conventional unpacked copy. Never the operator's application
    /// support directory — a test whose subject is whether *this* machine
    /// happens to have downloaded 28.8MB is a test that reports nothing.
    fn fixture_model_directory() -> Option<std::path::PathBuf> {
        let directory = std::path::PathBuf::from(
            std::env::var("SONA_SEMANTIC_MODEL_DIR")
                .unwrap_or_else(|_| "/tmp/potion8m".to_string()),
        );
        directory
            .join("model.safetensors")
            .is_file()
            .then_some(directory)
    }

    /// Every search asks [`HistoryManager::semantic_model`] for the model, and
    /// a pack runs a search behind a card, so the cost of loading it has to be
    /// paid once for the process and never again. Loading it per call would put
    /// a sha256 over thirty megabytes and a safetensors parse in front of every
    /// question a reader types.
    ///
    /// Residency is asserted by identity, not by duration:
    /// [`SemanticModelSlot::model`] memoises into a `Mutex<Option<Arc<_>>>`, so
    /// the claim is that two calls hand back the same allocation, and
    /// [`Arc::ptr_eq`] states exactly that. A wall-clock ratio would state it
    /// only on an idle machine, and would pass or fail on this one depending on
    /// how many other builds are running.
    ///
    /// Both halves assert. With a fixture on disk the memoised `Arc` is
    /// checked; without one the documented absent case is checked instead — an
    /// absent directory stays unloaded and is retried, which is what lets a
    /// finished background fetch take effect with no signalling. Neither branch
    /// is a no-op, so this never silently passes.
    #[test]
    fn the_recall_model_is_loaded_once_per_process_not_once_per_search() {
        use crate::managers::history::semantic::SemanticModelSlot;

        match fixture_model_directory() {
            Some(directory) => {
                let slot = SemanticModelSlot::without_fetch(directory.clone());
                let first = slot
                    .model()
                    .unwrap_or_else(|| panic!("the fixture at {} must load", directory.display()));
                let second = slot.model().expect("a memoised model stays loaded");
                assert!(
                    Arc::ptr_eq(&first, &second),
                    "a second search re-loaded the model instead of reusing the one in the slot"
                );
                assert_eq!(
                    Arc::strong_count(&first),
                    3,
                    "the slot and both handles are the only owners, so the load happened once"
                );
            }
            None => {
                let directory = std::env::temp_dir().join("sona-query-semantic-absent-model");
                let _ = std::fs::remove_dir_all(&directory);
                let slot = SemanticModelSlot::without_fetch(directory);
                assert!(
                    slot.model().is_none() && slot.model().is_none(),
                    "an absent model directory loads nothing and is retried, not cached as broken"
                );
            }
        }
    }
}
