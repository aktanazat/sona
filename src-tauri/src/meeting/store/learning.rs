//! Storage and arithmetic shared by the six local learning loops.
//!
//! Every loop is the same machine with a different miner:
//!
//! 1. A signal reaches the workflow runner.
//! 2. The runner opens one `Immediate` transaction and calls the miner.
//! 3. The miner reads a **bounded** slice of local evidence, counts what it saw
//!    into [`learning_observations`](record_observation_in), and asks the floors
//!    whether anything has earned a sentence yet.
//! 4. Whatever clears the floors, and has not already been answered, becomes a
//!    pending suggestion — subject to the caps below.
//! 5. A person answers. The answer is remembered forever, per loop, per
//!    normalized candidate.
//!
//! Two rules hold everywhere and are the reason this module exists rather than
//! four copies of it:
//!
//! * **Rate limiting happens at generation, never at render.** A suggestion that
//!   should not be shown is never written, so no reader is protected by a
//!   filter someone else can forget to apply.
//! * **The runner holds a write transaction on the meeting database.** The
//!   dictation corpus lives in another one, so its page is read before that
//!   transaction opens ([`DictationCorpus`]) and every miner then filters it
//!   against a cursor row in this database.

mod advice;
mod corrections;
mod habits;
mod priming;
mod spoken;

use super::people::normalized;
use super::{MeetingStore, StoreError};
use crate::managers::history::DictationRunRow;
use crate::meeting::learning_types::{
    LearningDecisionRequest, LearningEvidence, LearningLoopKind, LearningSuggestion,
    LearningSuggestionEntry, LearningSuggestionsResult, SeriesPrimingBlob, MAX_SUGGESTION_EXAMPLES,
};
use crate::meeting::types::MeetingSessionId;
use crate::meeting::workflow_types::WorkflowId;
use crate::settings::ReplacementRule;
use chrono::{DateTime, Local};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{BTreeMap, HashSet};

pub(super) use advice::mine_capture_advice_in;
pub(super) use corrections::{mine_dictation_correction_in, mine_meeting_edits_in};
// Only the macOS destination observer narrows a pair before submitting it.
#[cfg(target_os = "macos")]
pub(crate) use corrections::rewrite_span;
pub(super) use habits::mine_mode_habits_in;
pub(super) use priming::{prime_series_in, series_priming_for_session_in};
pub(super) use spoken::mine_spoken_punctuation_in;

const SCHEMA_VERSION: u32 = 1;

/// How many dictation runs one mining pass reads. Sized to stay ahead of a
/// heavy dictation day — a few hundred captures — so the loops keep up with live
/// use while a backlog drains over a handful of days.
pub(super) const DICTATION_CORPUS_BATCH: usize = 500;

/// How many new suggestions one pass may add. The loops exist to notice things,
/// not to fill a queue: three at a time is a glance, ten is a chore.
pub(super) const MAX_NEW_SUGGESTIONS_PER_RUN: usize = 3;

/// How many pending suggestions one loop may hold, and how many all loops may
/// hold together. Both ceilings are enforced before a row is written.
pub(super) const MAX_PENDING_PER_LOOP: usize = 5;
pub(super) const MAX_PENDING_TOTAL: usize = 12;

/// How many distinct candidates one pass may count into the ledger. A miner
/// with a fixed candidate table cannot reach this; the correction loop, whose
/// candidates are whatever a human typed, can.
pub(super) const MAX_OBSERVED_CANDIDATES_PER_RUN: usize = 200;

/// How long an observation stays evidence. Old dictation habits are not current
/// dictation habits, and this is also what stops the ledger growing forever.
pub(super) const OBSERVATION_RETENTION_DAYS: i64 = 120;

/// Everything the loops need that the meeting store does not own: the dictation
/// corpus, and the live settings a suggestion has to be checked against.
///
/// This is the whole boundary. The store reads no settings and opens no second
/// database on its own, so there is exactly one place to look for what the
/// loops can see.
pub(crate) trait LearningInputs: Send + Sync {
    /// One bounded page of dictation runs newer than `after`, oldest first.
    /// An empty vector is both "nothing new" and "history is locked": neither
    /// is an error a mining pass should fail on.
    fn dictation_runs_after(&self, after: i64, limit: usize) -> Vec<DictationRunRow>;
    /// The user's replacement rules, enabled or not. Loop 1 must never suggest
    /// a phrase a rule already claims, including a rule the user disabled.
    fn replacement_rules(&self) -> Vec<ReplacementRule>;
    /// Spoken and written forms of every vocabulary entry, global and per-mode.
    fn known_vocabulary(&self) -> Vec<String>;
    /// The mode's name as the user sees it now, or `None` when it is gone.
    fn mode_display_name(&self, mode_id: &str) -> Option<String>;
    /// The mode a run would use without a shortcut or an activation rule.
    fn active_mode_id(&self) -> Option<String>;
}

/// One candidate a miner is proposing, before caps and decision memory apply.
pub(super) struct MinedCandidate {
    pub key: String,
    pub suggestion: LearningSuggestion,
    pub evidence: LearningEvidence,
}

/// Totals for one candidate, folded across every day still in the ledger.
#[derive(Default)]
pub(super) struct ObservationTotals {
    pub occurrences: u64,
    pub sample_size: u64,
    pub distinct_days: u64,
    pub display_text: String,
    pub examples: Vec<String>,
}

impl ObservationTotals {
    /// The measured share, in parts per thousand, or `None` when nothing was
    /// sampled. A ratio over zero samples is not zero; it is unknown.
    pub(super) fn share_permille(&self) -> Option<u32> {
        (self.sample_size > 0).then(|| {
            u32::try_from(self.occurrences.saturating_mul(1_000) / self.sample_size)
                .unwrap_or(u32::MAX)
        })
    }

    pub(super) fn evidence(&self) -> LearningEvidence {
        LearningEvidence {
            occurrences: self.occurrences,
            distinct_days: self.distinct_days,
            examples: self.examples.clone(),
        }
    }
}

impl MeetingStore {
    /// Every pending suggestion, newest first, with stale ones dropped.
    ///
    /// Staleness is checked here rather than prevented at generation because the
    /// world moves after a suggestion is written: the user can write the rule by
    /// hand, or rename the mode, and neither is something the miner can foresee.
    pub(crate) fn learning_suggestions(
        &self,
        inputs: &dyn LearningInputs,
    ) -> Result<LearningSuggestionsResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let revision = learning_revision_in(&transaction)?;
        let mut entries = pending_suggestions_in(&transaction)?;
        let covered = covered_replacement_keys(&inputs.replacement_rules());
        entries.retain(|entry| match &entry.suggestion {
            LearningSuggestion::SpokenPunctuation { spoken, .. } => {
                !covered.contains(&normalized(spoken))
            }
            LearningSuggestion::ModeHabit { mode_id, .. } => {
                inputs.mode_display_name(mode_id).is_some()
                    && inputs.active_mode_id().as_deref() != Some(mode_id.as_str())
            }
            LearningSuggestion::VocabularyCorrection { spoken, .. } => {
                !known_vocabulary_keys(inputs).contains(&normalized(spoken))
            }
            LearningSuggestion::CaptureAdvice { .. } => true,
        });
        // A renamed mode keeps its suggestion; only its label moves.
        for entry in &mut entries {
            if let LearningSuggestion::ModeHabit { mode_id, mode_name } = &mut entry.suggestion {
                if let Some(current) = inputs.mode_display_name(mode_id) {
                    *mode_name = current;
                }
            }
        }
        transaction.commit()?;
        Ok(LearningSuggestionsResult {
            schema_version: SCHEMA_VERSION,
            revision,
            entries,
        })
    }

    /// The suggestion behind one candidate key, for a caller about to act on it.
    pub(crate) fn learning_suggestion(
        &self,
        loop_kind: LearningLoopKind,
        candidate_key: &str,
    ) -> Result<Option<LearningSuggestion>, StoreError> {
        let connection = self.connection()?;
        let json: Option<String> = connection
            .query_row(
                "SELECT suggestion_json FROM learning_suggestions
                  WHERE loop_kind = ?1 AND candidate_key = ?2",
                params![loop_kind.as_str(), normalized(candidate_key)],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(|_| StoreError::Corrupt))
            .transpose()
    }

    /// Records a human answer and retires the pending row it answered.
    ///
    /// The answer is the durable half. Whatever the caller wrote into settings
    /// on the way here is already done and is not undone by a failure below;
    /// the miners exclude anything settings already covers, so the worst case is
    /// that a candidate is offered once more and answers itself.
    pub(crate) fn decide_learning_suggestion(
        &self,
        request: &LearningDecisionRequest,
        decided_at_utc_ms: i64,
    ) -> Result<LearningSuggestionsResult, StoreError> {
        let candidate_key = normalized(&request.candidate_key);
        if candidate_key.is_empty() {
            return Err(StoreError::Invalid);
        }
        let display_text = request.display_text.trim();
        if display_text.is_empty() {
            return Err(StoreError::Invalid);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO learning_decisions (
                loop_kind, candidate_key, status, display_text, decided_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(loop_kind, candidate_key) DO UPDATE SET
                status = excluded.status,
                display_text = excluded.display_text,
                decided_at_utc_ms = excluded.decided_at_utc_ms",
            params![
                request.loop_kind.as_str(),
                &candidate_key,
                request.status.as_str(),
                display_text,
                decided_at_utc_ms
            ],
        )?;
        transaction.execute(
            "DELETE FROM learning_suggestions WHERE loop_kind = ?1 AND candidate_key = ?2",
            params![request.loop_kind.as_str(), &candidate_key],
        )?;
        bump_learning_revision_in(&transaction)?;
        let revision = learning_revision_in(&transaction)?;
        let entries = pending_suggestions_in(&transaction)?;
        transaction.commit()?;
        Ok(LearningSuggestionsResult {
            schema_version: SCHEMA_VERSION,
            revision,
            entries,
        })
    }

    /// The priming blob attached to one session, for the transcription run that
    /// owns it. `None` for every session that is not in a consented series.
    pub(crate) fn series_priming(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Option<SeriesPrimingBlob>, StoreError> {
        let connection = self.connection()?;
        series_priming_for_session_in(&connection, session_id)
    }
}

/// The decision memory, as the miners consult it: every candidate this loop has
/// already been answered about, accepted or dismissed alike.
///
/// Accepted and dismissed are the same answer to the only question a miner asks
/// — "is this still worth suggesting?" — which is why one set serves both.
pub(super) fn decided_keys_in(
    connection: &Connection,
    loop_kind: LearningLoopKind,
) -> Result<HashSet<String>, StoreError> {
    let mut statement =
        connection.prepare("SELECT candidate_key FROM learning_decisions WHERE loop_kind = ?1")?;
    let keys = statement
        .query_map([loop_kind.as_str()], |row| row.get::<_, String>(0))?
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(keys)
}

/// The display forms of everything the user accepted in these loops, oldest
/// decision first. Loop 4 primes a session with exactly this.
pub(super) fn accepted_display_texts_in(
    connection: &Connection,
    loop_kinds: &[LearningLoopKind],
    limit: usize,
) -> Result<Vec<String>, StoreError> {
    let mut texts = Vec::new();
    for loop_kind in loop_kinds {
        let mut statement = connection.prepare(
            "SELECT display_text FROM learning_decisions
              WHERE loop_kind = ?1 AND status = 'accepted'
              ORDER BY decided_at_utc_ms DESC, candidate_key",
        )?;
        let rows = statement
            .query_map([loop_kind.as_str()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        texts.extend(rows);
    }
    texts.sort();
    texts.dedup();
    texts.truncate(limit);
    Ok(texts)
}

/// A verbatim excerpt kept as evidence, and the meeting it came from.
///
/// The two travel together because they have to: the excerpt is the user's own
/// words, and `session_id` is the only thing that makes it die when they delete
/// the meeting they said them in. `None` is the dictation-sourced case, whose
/// lifetime is [`OBSERVATION_RETENTION_DAYS`] rather than a session's.
pub(super) struct ObservationExample<'a> {
    pub context: &'a str,
    pub session_id: Option<MeetingSessionId>,
}

/// Counts one candidate's evidence for one local day.
///
/// `occurrences` and `sample_size` accumulate: a pass counts only what its own
/// corpus slice contained, and the cursor guarantees no slice is counted twice.
#[allow(clippy::too_many_arguments)]
pub(super) fn record_observation_in(
    connection: &Connection,
    loop_kind: LearningLoopKind,
    candidate_key: &str,
    local_day: &str,
    occurrences: u64,
    sample_size: u64,
    display_text: &str,
    example: Option<ObservationExample<'_>>,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO learning_observations (
            loop_kind, candidate_key, local_day, occurrences, sample_size,
            display_text, example_context, source_session_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(loop_kind, candidate_key, local_day) DO UPDATE SET
            occurrences = occurrences + excluded.occurrences,
            sample_size = sample_size + excluded.sample_size,
            display_text = excluded.display_text,
            example_context = COALESCE(example_context, excluded.example_context),
            -- The session follows whichever writer supplied the surviving
            -- excerpt, so the row can never outlive the words it holds.
            source_session_id = CASE
                WHEN example_context IS NULL THEN excluded.source_session_id
                ELSE source_session_id
            END",
        params![
            loop_kind.as_str(),
            candidate_key,
            local_day,
            i64::try_from(occurrences).map_err(|_| StoreError::Invalid)?,
            i64::try_from(sample_size).map_err(|_| StoreError::Invalid)?,
            display_text,
            example.as_ref().map(|example| example.context),
            example
                .as_ref()
                .and_then(|example| example.session_id)
                .map(|session_id| session_id.uuid().to_string())
        ],
    )?;
    Ok(())
}

/// Every candidate this loop has evidence for, folded across days.
pub(super) fn observation_totals_in(
    connection: &Connection,
    loop_kind: LearningLoopKind,
) -> Result<BTreeMap<String, ObservationTotals>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT candidate_key, occurrences, sample_size, display_text, example_context
           FROM learning_observations
          WHERE loop_kind = ?1
          ORDER BY candidate_key, local_day",
    )?;
    let rows = statement
        .query_map([loop_kind.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut totals = BTreeMap::<String, ObservationTotals>::new();
    for (key, occurrences, sample_size, display_text, example) in rows {
        let entry = totals.entry(key).or_default();
        entry.occurrences = entry
            .occurrences
            .saturating_add(u64::try_from(occurrences).map_err(|_| StoreError::Corrupt)?);
        entry.sample_size = entry
            .sample_size
            .saturating_add(u64::try_from(sample_size).map_err(|_| StoreError::Corrupt)?);
        entry.distinct_days = entry.distinct_days.saturating_add(1);
        entry.display_text = display_text;
        if let Some(example) = example {
            if entry.examples.len() < MAX_SUGGESTION_EXAMPLES && !entry.examples.contains(&example)
            {
                entry.examples.push(example);
            }
        }
    }
    Ok(totals)
}

/// Drops evidence older than [`OBSERVATION_RETENTION_DAYS`].
///
/// A pending suggestion holding a second copy of that evidence goes with it:
/// the `learning_suggestions_need_evidence` trigger owns that rule, so it holds
/// here and for a meeting deletion cascading through `source_session_id`
/// alike.
pub(super) fn prune_observations_in(
    connection: &Connection,
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    let horizon = local_day(now_utc_ms - OBSERVATION_RETENTION_DAYS * 86_400_000);
    connection.execute(
        "DELETE FROM learning_observations WHERE local_day < ?1",
        [horizon],
    )?;
    Ok(())
}

/// Writes what a miner proposed, applying decision memory and every cap.
///
/// Returns the keys actually written. Candidates are offered in the order the
/// miner ranked them, so a cap truncates the weakest evidence rather than an
/// arbitrary slice, and a caller that has follow-up bookkeeping does it for
/// exactly the rows a reader will see.
pub(super) fn insert_suggestions_in(
    connection: &Connection,
    loop_kind: LearningLoopKind,
    candidates: Vec<MinedCandidate>,
    now_utc_ms: i64,
) -> Result<Vec<String>, StoreError> {
    let decided = decided_keys_in(connection, loop_kind)?;
    let mut pending_in_loop = pending_count_in(connection, Some(loop_kind))?;
    let mut pending_total = pending_count_in(connection, None)?;
    let mut added = Vec::new();

    for candidate in candidates {
        if added.len() >= MAX_NEW_SUGGESTIONS_PER_RUN
            || pending_in_loop >= MAX_PENDING_PER_LOOP
            || pending_total >= MAX_PENDING_TOTAL
        {
            break;
        }
        if candidate.key.is_empty() || decided.contains(&candidate.key) {
            continue;
        }
        let inserted = connection.execute(
            "INSERT OR IGNORE INTO learning_suggestions (
                loop_kind, candidate_key, suggestion_json, evidence_json, generated_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                loop_kind.as_str(),
                &candidate.key,
                serde_json::to_string(&candidate.suggestion).map_err(|_| StoreError::Corrupt)?,
                serde_json::to_string(&candidate.evidence).map_err(|_| StoreError::Corrupt)?,
                now_utc_ms
            ],
        )? != 0;
        if inserted {
            added.push(candidate.key);
            pending_in_loop += 1;
            pending_total += 1;
        }
    }
    if !added.is_empty() {
        bump_learning_revision_in(connection)?;
    }
    Ok(added)
}

/// This loop's corpus cursor: the highest dictation run id it has read.
pub(super) fn corpus_cursor_in(
    connection: &Connection,
    loop_kind: LearningLoopKind,
) -> Result<i64, StoreError> {
    connection
        .query_row(
            "SELECT last_run_receipt_id FROM learning_cursors WHERE loop_kind = ?1",
            [loop_kind.as_str()],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

/// Advances the cursor, never backwards. Two passes racing on the same slice
/// both count nothing the other did not, because each filters its slice against
/// the cursor it reads inside its own transaction.
pub(super) fn advance_corpus_cursor_in(
    connection: &Connection,
    loop_kind: LearningLoopKind,
    last_run_receipt_id: i64,
) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE learning_cursors
            SET last_run_receipt_id = MAX(last_run_receipt_id, ?2)
          WHERE loop_kind = ?1",
        params![loop_kind.as_str(), last_run_receipt_id],
    )?;
    Ok(())
}

/// One page of the dictation corpus, read before the runner takes the meeting
/// store's write lock.
///
/// The corpus lives in another database behind another lock. Reading it from
/// inside the runner's transaction is what would make a mining pass hold this
/// database's only connection across an unlock wait that runs to seconds, so
/// the page is resolved first and every miner then filters it against local
/// rows only.
#[derive(Default)]
pub(crate) struct DictationCorpus {
    /// Oldest first, bounded to [`DICTATION_CORPUS_BATCH`].
    rows: Vec<DictationRunRow>,
}

impl DictationCorpus {
    /// Reads one bounded page newer than `floor`.
    ///
    /// `floor` is the lowest cursor any miner on this event will page against,
    /// read from the meeting database *before* this call. A cursor only ever
    /// advances, so the floor can be behind the cursor that finally filters but
    /// never ahead of it — which is what makes this page a superset of what any
    /// miner is entitled to count, and why the filtering still belongs inside
    /// the transaction.
    pub(super) fn read(inputs: &dyn LearningInputs, floor: i64) -> Self {
        let mut rows = inputs.dictation_runs_after(floor, DICTATION_CORPUS_BATCH);
        rows.retain(|row| row.id > floor);
        rows.truncate(DICTATION_CORPUS_BATCH);
        Self { rows }
    }
}

/// The lowest corpus cursor among `workflows`, or `None` when none of them
/// reads the corpus — which is every event but the daily sweep.
pub(super) fn cursor_floor_in(
    connection: &Connection,
    workflows: &[WorkflowId],
) -> Result<Option<i64>, StoreError> {
    let mut floor = None;
    for loop_kind in workflows.iter().copied().filter_map(corpus_loop_kind) {
        let cursor = corpus_cursor_in(connection, loop_kind)?;
        floor = Some(floor.map_or(cursor, |floor: i64| floor.min(cursor)));
    }
    Ok(floor)
}

/// The loop whose corpus cursor a workflow pages against.
///
/// Exhaustive on purpose: a new workflow has to answer this question before it
/// compiles, because a corpus-reading loop the runner forgot here would read a
/// page that starts after its own cursor and silently skip evidence.
const fn corpus_loop_kind(workflow_id: WorkflowId) -> Option<LearningLoopKind> {
    match workflow_id {
        WorkflowId::SpokenPunctuation => Some(LearningLoopKind::SpokenPunctuation),
        WorkflowId::ModeHabits => Some(LearningLoopKind::ModeHabit),
        WorkflowId::CaptureAdvisor => Some(LearningLoopKind::CaptureAdvice),
        WorkflowId::PersonLinking
        | WorkflowId::PreMeetingBriefing
        | WorkflowId::Continuity
        | WorkflowId::VocabularyMining
        | WorkflowId::DocumentLinking
        | WorkflowId::CorrectionLearning
        | WorkflowId::MeetingActivity
        | WorkflowId::SeriesPriming
        | WorkflowId::DailyDigest => None,
    }
}

/// This loop's slice of an already-read page, filtered against the cursor the
/// caller's transaction can see.
pub(super) fn corpus_slice_in<'a>(
    connection: &Connection,
    loop_kind: LearningLoopKind,
    corpus: &'a DictationCorpus,
) -> Result<Vec<&'a DictationRunRow>, StoreError> {
    let cursor = corpus_cursor_in(connection, loop_kind)?;
    Ok(corpus
        .rows
        .iter()
        .filter(|row| row.id > cursor)
        .take(DICTATION_CORPUS_BATCH)
        .collect())
}

pub(super) fn learning_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    let revision: i64 = connection.query_row(
        "SELECT learning_revision FROM workflow_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(revision).map_err(|_| StoreError::Corrupt)
}

pub(super) fn bump_learning_revision_in(connection: &Connection) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE workflow_state SET learning_revision = learning_revision + 1 WHERE singleton = 1",
        [],
    )?;
    Ok(())
}

/// The local calendar day an instant falls in, as `YYYY-MM-DD`.
///
/// Local rather than UTC because the evidence floors are about a person's days.
pub(super) fn local_day(utc_ms: i64) -> String {
    DateTime::from_timestamp_millis(utc_ms)
        .map(|instant| instant.with_timezone(&Local).format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

fn pending_count_in(
    connection: &Connection,
    loop_kind: Option<LearningLoopKind>,
) -> Result<usize, StoreError> {
    let count: i64 = match loop_kind {
        Some(loop_kind) => connection.query_row(
            "SELECT COUNT(*) FROM learning_suggestions WHERE loop_kind = ?1",
            [loop_kind.as_str()],
            |row| row.get(0),
        )?,
        None => connection.query_row("SELECT COUNT(*) FROM learning_suggestions", [], |row| {
            row.get(0)
        })?,
    };
    usize::try_from(count).map_err(|_| StoreError::Corrupt)
}

fn pending_suggestions_in(
    connection: &Connection,
) -> Result<Vec<LearningSuggestionEntry>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT loop_kind, candidate_key, suggestion_json, evidence_json, generated_at_utc_ms
           FROM learning_suggestions
          ORDER BY generated_at_utc_ms DESC, loop_kind, candidate_key",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |(loop_kind, candidate_key, suggestion, evidence, generated_at_utc_ms)| {
                Ok(LearningSuggestionEntry {
                    loop_kind: LearningLoopKind::from_str(&loop_kind).ok_or(StoreError::Corrupt)?,
                    candidate_key,
                    suggestion: serde_json::from_str(&suggestion)
                        .map_err(|_| StoreError::Corrupt)?,
                    evidence: serde_json::from_str(&evidence).map_err(|_| StoreError::Corrupt)?,
                    generated_at_utc_ms,
                })
            },
        )
        .collect()
}

/// Normalized spoken forms of every replacement rule the user has, enabled or
/// not. A disabled rule is still the user's answer about that phrase.
pub(super) fn covered_replacement_keys(rules: &[ReplacementRule]) -> HashSet<String> {
    rules
        .iter()
        .map(|rule| normalized(&rule.spoken))
        .filter(|key| !key.is_empty())
        .collect()
}

pub(super) fn known_vocabulary_keys(inputs: &dyn LearningInputs) -> HashSet<String> {
    inputs
        .known_vocabulary()
        .iter()
        .map(|term| normalized(term))
        .filter(|key| !key.is_empty())
        .collect()
}
