//! Behaviour tests for the six local learning loops.
//!
//! Every test here exercises a boundary the loops are defined by: what the
//! bounded corpus read is allowed to touch, what the evidence floors refuse,
//! what a human answer permanently silences, and what dies with a session.

use super::learning::{LearningInputs, DICTATION_CORPUS_BATCH};
use super::workflow_core_tests::{event, meeting, person, store as new_store, transcript};
use super::MeetingStore;
use crate::context::ContextPolicy;
use crate::managers::history::{CaptureStatus, DictationRunRow};
use crate::meeting::learning_types::{
    CaptureAdviceKind, LearningDecisionRequest, LearningDecisionStatus, LearningLoopKind,
    LearningSuggestion,
};
use crate::meeting::people_types::PersonId;
use crate::meeting::types::MeetingSessionId;
use crate::meeting::workflow_types::{WorkflowEventKind, WorkflowId};
use crate::modes::{
    CloudReceiptStatus, ModeReceipt, ModeSelectionSource, PromptPreset, RequestedEngine, Tone,
};
use crate::settings::ReplacementRule;
use rusqlite::params;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One day in milliseconds, for building corpora that span the day floors.
const DAY_MS: i64 = 86_400_000;
/// A fixed local noon, so a row's local day is unambiguous wherever this runs.
const NOON: i64 = 1_756_136_400_000;

pub(super) struct FakeInputs {
    runs: Vec<DictationRunRow>,
    rules: Vec<ReplacementRule>,
    vocabulary: Vec<String>,
    modes: Vec<(String, String)>,
    active_mode: Option<String>,
    /// The largest `limit` any corpus read asked for, so a test can prove the
    /// batch bound is applied at the boundary rather than after the read.
    largest_limit: AtomicUsize,
    /// How many rows the last read actually returned.
    last_returned: AtomicUsize,
}

impl FakeInputs {
    pub(super) fn empty() -> Self {
        Self {
            runs: Vec::new(),
            rules: Vec::new(),
            vocabulary: Vec::new(),
            modes: Vec::new(),
            active_mode: None,
            largest_limit: AtomicUsize::new(0),
            last_returned: AtomicUsize::new(0),
        }
    }

    fn with_runs(runs: Vec<DictationRunRow>) -> Self {
        Self {
            runs,
            ..Self::empty()
        }
    }

    fn with_mode(mut self, mode_id: &str, mode_name: &str) -> Self {
        self.modes
            .push((mode_id.to_string(), mode_name.to_string()));
        self
    }

    fn with_rule(mut self, spoken: &str, written: &str) -> Self {
        self.rules.push(ReplacementRule {
            spoken: spoken.to_string(),
            written: written.to_string(),
            enabled: true,
        });
        self
    }

    fn with_vocabulary(mut self, term: &str) -> Self {
        self.vocabulary.push(term.to_string());
        self
    }
}

impl LearningInputs for FakeInputs {
    fn dictation_runs_after(&self, after: i64, limit: usize) -> Vec<DictationRunRow> {
        self.largest_limit.fetch_max(limit, Ordering::Relaxed);
        let rows = self
            .runs
            .iter()
            .filter(|row| row.id > after)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        self.last_returned.store(rows.len(), Ordering::Relaxed);
        rows
    }

    fn replacement_rules(&self) -> Vec<ReplacementRule> {
        self.rules.clone()
    }

    fn known_vocabulary(&self) -> Vec<String> {
        self.vocabulary.clone()
    }

    fn mode_display_name(&self, mode_id: &str) -> Option<String> {
        self.modes
            .iter()
            .find(|(id, _)| id == mode_id)
            .map(|(_, name)| name.clone())
    }

    fn active_mode_id(&self) -> Option<String> {
        self.active_mode.clone()
    }
}

fn mode_receipt(mode_id: &str, source: ModeSelectionSource) -> ModeReceipt {
    ModeReceipt {
        run_id: 1,
        settings_revision: 1,
        mode_selection_source: source,
        mode_id: mode_id.to_string(),
        tone: Tone::Balanced,
        requested_context_policy: ContextPolicy::None,
        context_policy_ceiling: ContextPolicy::None,
        context_policy: ContextPolicy::None,
        prompt_preset: PromptPreset::MinimalistCleanup,
        post_process_requested: false,
        rewrite: crate::modes::RewriteOutcome::NotRequested,
        provider_id: None,
        model_id: None,
        engine_requested: RequestedEngine::Local,
        engine_used: Some(RequestedEngine::Local),
        cloud_fallback: false,
        cloud_status: CloudReceiptStatus::NotRequested,
        local_fallback_model_id: None,
        input_peak: None,
        input_rms: None,
        realtime_factor: None,
    }
}

fn run(id: i64, day_offset: i64, text: &str) -> DictationRunRow {
    DictationRunRow {
        id,
        completed_at_ms: NOON + day_offset * DAY_MS,
        delivered_text: text.to_string(),
        mode: mode_receipt("message", ModeSelectionSource::ActiveMode),
        capture_status: Some(CaptureStatus::Complete),
        is_retry: false,
    }
}

fn sweep(store: &MeetingStore, inputs: &FakeInputs, key: &str) {
    store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::DictationCorpusSwept,
                serde_json::json!({"local_day": key}),
                key,
            ),
            inputs,
        )
        .unwrap();
}

fn suggestions(store: &MeetingStore, inputs: &FakeInputs) -> Vec<LearningSuggestion> {
    store
        .learning_suggestions(inputs)
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.suggestion)
        .collect()
}

// ---------------------------------------------------------------- corpus bounds

/// The corpus read is bounded and the cursor advances past what it read, so a
/// second pass over the same history sees only what is new.
#[test]
fn a_mining_pass_reads_a_bounded_batch_and_never_rereads_it() {
    let (_directory, store) = new_store();
    let corpus = (1..=DICTATION_CORPUS_BATCH as i64 + 25)
        .map(|id| run(id, 0, "nothing interesting here"))
        .collect::<Vec<_>>();
    let inputs = FakeInputs::with_runs(corpus);

    sweep(&store, &inputs, "sweep-day-1");
    assert_eq!(
        inputs.largest_limit.load(Ordering::Relaxed),
        DICTATION_CORPUS_BATCH,
        "the batch bound is asked for at the corpus boundary"
    );
    assert_eq!(
        inputs.last_returned.load(Ordering::Relaxed),
        DICTATION_CORPUS_BATCH,
        "a pass never reads more than one batch"
    );

    sweep(&store, &inputs, "sweep-day-2");
    assert_eq!(
        inputs.last_returned.load(Ordering::Relaxed),
        25,
        "the second pass sees only what the cursor had not reached"
    );

    sweep(&store, &inputs, "sweep-day-3");
    assert_eq!(
        inputs.last_returned.load(Ordering::Relaxed),
        0,
        "a drained corpus reads nothing"
    );
}

/// A duplicate day bucket does not run the miner again, which is the whole point
/// of a coarse dedupe key on the history edge.
#[test]
fn a_repeated_day_bucket_does_not_mine_again() {
    let (_directory, store) = new_store();
    let inputs = FakeInputs::with_runs(vec![run(1, 0, "hello"), run(2, 0, "hello")]);

    sweep(&store, &inputs, "sweep-same-day");
    let first = inputs.last_returned.load(Ordering::Relaxed);
    assert_eq!(first, 2);

    // Re-dispatching the same dedupe key records a skip, never a second pass.
    store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::DictationCorpusSwept,
                serde_json::json!({}),
                "sweep-same-day",
            ),
            &inputs,
        )
        .unwrap();
    assert_eq!(
        inputs.last_returned.load(Ordering::Relaxed),
        first,
        "the miner did not run a second time for the same day"
    );
}

// --------------------------------------------------------------- evidence floors

fn domain_corpus(days: i64, per_day: usize) -> Vec<DictationRunRow> {
    let mut runs = Vec::new();
    let mut id = 0;
    for day in 0..days {
        for _ in 0..per_day {
            id += 1;
            runs.push(run(
                id,
                day,
                "send it to aktan at example dot org before friday",
            ));
        }
    }
    runs
}

#[test]
fn loop_one_needs_repeated_evidence_across_days_before_it_suggests() {
    let (_directory, store) = new_store();

    // Four occurrences, one day: the occurrence floor is met, the day floor is
    // not.
    let one_day = FakeInputs::with_runs(domain_corpus(1, 4));
    sweep(&store, &one_day, "floor-one-day");
    assert!(
        suggestions(&store, &one_day).is_empty(),
        "one day of evidence is not a habit"
    );

    // Two days, four occurrences: both floors met.
    let (_directory, store) = new_store();
    let two_days = FakeInputs::with_runs(domain_corpus(2, 2));
    sweep(&store, &two_days, "floor-two-days");
    assert_eq!(
        suggestions(&store, &two_days),
        vec![LearningSuggestion::SpokenPunctuation {
            spoken: "dot org".to_string(),
            written: ".org".to_string(),
        }]
    );
}

/// A phrase the user's own rules already claim is never suggested, enabled or
/// not: a disabled rule is still the user's answer about that phrase.
#[test]
fn loop_one_never_suggests_a_phrase_a_rule_already_claims() {
    let (_directory, store) = new_store();
    let inputs = FakeInputs::with_runs(domain_corpus(2, 2)).with_rule("dot org", ".org");
    sweep(&store, &inputs, "floor-covered");
    assert!(suggestions(&store, &inputs).is_empty());
}

/// The polka-dot hazard end to end: a corpus where `dot` is mostly the English
/// word never earns a `dot`-headed rule, however many domains it also contains.
#[test]
fn loop_one_refuses_a_phrase_whose_corpus_reading_is_mostly_prose() {
    let (_directory, store) = new_store();
    let mut runs = domain_corpus(2, 2);
    // Sentence-initial `dot org` is counted but never live, so the precision
    // ratio falls below the floor.
    for day in 0..2 {
        for _ in 0..10 {
            let id = runs.len() as i64 + 1;
            runs.push(run(id, day, "Dot org was the example we used"));
        }
    }
    let inputs = FakeInputs::with_runs(runs);
    sweep(&store, &inputs, "floor-precision");
    assert!(
        suggestions(&store, &inputs).is_empty(),
        "a mostly-prose reading is below the precision floor"
    );
}

#[test]
fn loop_five_needs_five_shortcut_runs_across_three_days() {
    let (_directory, store) = new_store();
    let shortcut = |id: i64, day: i64| {
        let mut row = run(id, day, "text");
        row.mode = mode_receipt("email", ModeSelectionSource::ExplicitModeShortcut);
        row
    };

    // Five runs, two days: the day floor refuses.
    let two_days = FakeInputs::with_runs(vec![
        shortcut(1, 0),
        shortcut(2, 0),
        shortcut(3, 0),
        shortcut(4, 1),
        shortcut(5, 1),
    ])
    .with_mode("email", "Email");
    sweep(&store, &two_days, "habit-two-days");
    assert!(suggestions(&store, &two_days).is_empty());

    // Five runs, three days: both floors met.
    let (_directory, store) = new_store();
    let three_days = FakeInputs::with_runs(vec![
        shortcut(1, 0),
        shortcut(2, 0),
        shortcut(3, 1),
        shortcut(4, 1),
        shortcut(5, 2),
    ])
    .with_mode("email", "Email");
    sweep(&store, &three_days, "habit-three-days");
    assert_eq!(
        suggestions(&store, &three_days),
        vec![LearningSuggestion::ModeHabit {
            mode_id: "email".to_string(),
            mode_name: "Email".to_string(),
        }]
    );
}

/// Loop 5 counts human decisions. A run that only inherited a plan — a retry —
/// or one a rule selected is not a shortcut the user reached for.
#[test]
fn loop_five_counts_only_explicit_shortcuts() {
    let (_directory, store) = new_store();
    let mut runs = Vec::new();
    for (index, day) in [(1_i64, 0_i64), (2, 0), (3, 1), (4, 1), (5, 2)] {
        let mut row = run(index, day, "text");
        row.mode = mode_receipt("email", ModeSelectionSource::AppActivationRule);
        runs.push(row);
    }
    for (index, day) in [(6_i64, 0_i64), (7, 0), (8, 1), (9, 1), (10, 2)] {
        let mut row = run(index, day, "text");
        row.mode = mode_receipt("email", ModeSelectionSource::ExplicitModeShortcut);
        row.is_retry = true;
        runs.push(row);
    }
    let inputs = FakeInputs::with_runs(runs).with_mode("email", "Email");
    sweep(&store, &inputs, "habit-not-explicit");
    assert!(suggestions(&store, &inputs).is_empty());
}

/// Loop 6 needs enough runs on a route, and another route to compare against,
/// before it claims one is worse.
#[test]
fn loop_six_needs_a_comparison_before_it_advises() {
    let cloud_run = |id: i64, day: i64, retry: bool| {
        let mut row = run(id, day, "text");
        row.mode.engine_used = Some(RequestedEngine::DeepgramNova3);
        row.is_retry = retry;
        row
    };

    // One route only: nothing to compare against.
    let (_directory, store) = new_store();
    let single = FakeInputs::with_runs(
        (1..=40)
            .map(|id| cloud_run(id, id % 3, id % 2 == 0))
            .collect(),
    );
    sweep(&store, &single, "advice-single-route");
    assert!(suggestions(&store, &single).is_empty());

    // Two routes, one retried far more often.
    let (_directory, store) = new_store();
    let mut runs = (1..=40)
        .map(|id| cloud_run(id, id % 3, id % 2 == 0))
        .collect::<Vec<_>>();
    runs.extend((41..=80).map(|id| run(id, id % 3, "text")));
    let compared = FakeInputs::with_runs(runs);
    sweep(&store, &compared, "advice-two-routes");
    let advice = suggestions(&store, &compared);
    assert!(
        advice.iter().any(|suggestion| matches!(
            suggestion,
            LearningSuggestion::CaptureAdvice {
                advice: CaptureAdviceKind::RetryRate,
                ..
            }
        )),
        "a route retried far more often earns advice: {advice:?}"
    );
}

// ------------------------------------------------------------ decision memory

/// One dismissal owner, and it is absolute: a dismissed candidate never comes
/// back, and neither does an accepted one.
#[test]
fn an_answered_candidate_is_never_suggested_again() {
    for status in [
        LearningDecisionStatus::Dismissed,
        LearningDecisionStatus::Accepted,
    ] {
        let (_directory, store) = new_store();
        let inputs = FakeInputs::with_runs(domain_corpus(2, 2));
        sweep(&store, &inputs, "memory-first");
        let entry = store
            .learning_suggestions(&inputs)
            .unwrap()
            .entries
            .into_iter()
            .next()
            .expect("a suggestion to answer");

        let remaining = store
            .decide_learning_suggestion(
                &LearningDecisionRequest {
                    loop_kind: entry.loop_kind,
                    candidate_key: entry.candidate_key.clone(),
                    status,
                    display_text: "dot org".to_string(),
                },
                NOON,
            )
            .unwrap();
        assert!(remaining.entries.is_empty(), "the answered row is retired");

        // More evidence, another pass: the answer holds.
        let more = FakeInputs::with_runs(domain_corpus(4, 4));
        sweep(&store, &more, "memory-second");
        assert!(
            suggestions(&store, &more).is_empty(),
            "{status:?} did not survive a later mining pass"
        );
    }
}

/// The vocabulary-term loop shares the same memory even though its candidates
/// are computed live rather than stored. This is the cutover the localStorage
/// dismissal helpers used to own.
#[test]
fn a_dismissed_vocabulary_term_is_excluded_by_the_store_not_the_client() {
    let (_directory, store) = new_store();
    let meeting_one = meeting(&store, "North Star review", 1_000);
    let meeting_two = meeting(&store, "North Star review", 2_000);
    for session in [meeting_one, meeting_two] {
        transcript(&store, session, "North Star is the plan. North Star again.");
    }
    let inputs = FakeInputs::empty();
    store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::MeetingFinalized,
                serde_json::json!({
                    "session_id": meeting_two.uuid().to_string(),
                    "known_vocabulary": [],
                }),
                "vocab-term-finalized",
            ),
            &inputs,
        )
        .unwrap();
    let before = store.vocabulary_candidates(&[]).unwrap().entries;
    let candidate = before
        .first()
        .expect("a repeated term to dismiss")
        .text
        .clone();

    store
        .decide_learning_suggestion(
            &LearningDecisionRequest {
                loop_kind: LearningLoopKind::VocabularyTerm,
                candidate_key: candidate.clone(),
                status: LearningDecisionStatus::Dismissed,
                display_text: candidate.clone(),
            },
            NOON,
        )
        .unwrap();

    let after = store.vocabulary_candidates(&[]).unwrap().entries;
    assert!(
        !after.iter().any(|entry| entry.text == candidate),
        "the store still offers a term the user dismissed"
    );
}

// -------------------------------------------------------------- loop 2 evidence

/// Retry lineage is never vocabulary evidence. A corpus full of model-versus-
/// model differences produces no correction suggestion, because loop 2 does not
/// read the corpus at all.
#[test]
fn retry_lineage_is_never_vocabulary_evidence() {
    let (_directory, store) = new_store();
    let mut runs = Vec::new();
    for day in 0..4 {
        let id = day * 2 + 1;
        runs.push(run(id, day, "we shipped handy on friday"));
        let mut retried = run(id + 1, day, "we shipped Sona on friday");
        retried.is_retry = true;
        runs.push(retried);
    }
    let inputs = FakeInputs::with_runs(runs);
    sweep(&store, &inputs, "retry-lineage");
    assert!(
        suggestions(&store, &inputs)
            .iter()
            .all(|suggestion| !matches!(
                suggestion,
                LearningSuggestion::VocabularyCorrection { .. }
            )),
        "a model-versus-model diff became vocabulary evidence"
    );
}

/// A human rewrite recurring across days becomes a suggestion; the same rewrite
/// on one day does not.
#[test]
fn loop_two_learns_a_human_rewrite_that_recurs_across_days() {
    let (_directory, store) = new_store();
    let inputs = FakeInputs::empty();
    for (index, day) in [(0_i64, 0_i64), (1, 0), (2, 0)] {
        correction(&store, &inputs, "handy", "Sona", day, index);
    }
    assert!(
        suggestions(&store, &inputs).is_empty(),
        "three corrections on one day are one decision repeated"
    );

    correction(&store, &inputs, "handy", "Sona", 1, 3);
    correction(&store, &inputs, "handy", "Sona", 2, 4);
    assert_eq!(
        suggestions(&store, &inputs),
        vec![LearningSuggestion::VocabularyCorrection {
            spoken: "handy".to_string(),
            written: "Sona".to_string(),
        }]
    );
}

/// A dictation correction arrives as the whole passage the human re-read. The
/// word they changed is the evidence; the sentence around it is not.
#[test]
fn loop_two_learns_the_word_a_dictation_correction_changed() {
    let (_directory, store) = new_store();
    let inputs = FakeInputs::empty();
    for (index, day) in [(0_i64, 0_i64), (1, 1), (2, 2)] {
        correction(
            &store,
            &inputs,
            "please open the handy project",
            "please open the Sona project",
            day,
            index,
        );
    }
    assert_eq!(
        suggestions(&store, &inputs),
        vec![LearningSuggestion::VocabularyCorrection {
            spoken: "handy".to_string(),
            written: "Sona".to_string(),
        }]
    );
}

/// A rewrite already in the user's vocabulary is not news.
#[test]
fn loop_two_skips_a_rewrite_the_vocabulary_already_covers() {
    let (_directory, store) = new_store();
    let inputs = FakeInputs::empty().with_vocabulary("handy");
    for (index, day) in [(0_i64, 0_i64), (1, 1), (2, 2)] {
        correction(&store, &inputs, "handy", "Sona", day, index);
    }
    assert!(suggestions(&store, &inputs).is_empty());
}

// ---------------------------------------------------------------------- loop 4

/// The priming blob is attached to one session and dies with it. There is no
/// second copy to clean up, which is what makes Forget-this-series complete.
#[test]
fn the_series_priming_blob_dies_with_its_session() {
    let (_directory, store) = new_store();
    let session_id = meeting(&store, "Weekly sync", 1_000);
    standing_consent(&store, session_id, "series-weekly");
    let confirmed = meeting(&store, "Weekly sync", 500);
    calendar_facts(&store, confirmed, "series-weekly");
    let person_id = person(&store, "Aktan Azat", &[], &[]);
    confirm_link(&store, confirmed, person_id);

    let inputs = FakeInputs::empty();
    store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::MeetingStarted,
                serde_json::json!({"session_id": session_id.uuid().to_string()}),
                "priming-started",
            ),
            &inputs,
        )
        .unwrap();

    assert_eq!(
        priming_rows(&store, session_id),
        1,
        "a session in a consented series is primed"
    );

    store
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM meeting_sessions WHERE id = ?1",
            params![session_id.uuid().to_string()],
        )
        .unwrap();
    assert_eq!(
        priming_rows(&store, session_id),
        0,
        "the blob outlived the session it belonged to"
    );
}

/// A revoked series primes nothing, so Forget-this-series leaves no residue in
/// later meetings either.
#[test]
fn a_revoked_series_primes_nothing() {
    let (_directory, store) = new_store();
    let session_id = meeting(&store, "Weekly sync", 1_000);
    standing_consent(&store, session_id, "series-weekly");
    store.revoke_series_consent("series-weekly", 2_000).unwrap();

    let inputs = FakeInputs::empty();
    store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::MeetingStarted,
                serde_json::json!({"session_id": session_id.uuid().to_string()}),
                "priming-revoked",
            ),
            &inputs,
        )
        .unwrap();
    assert_eq!(priming_rows(&store, session_id), 0);
}

/// A meeting's own words are evidence only while the meeting exists.
///
/// `example_context` is up to 120 characters of a transcript the user typed
/// into, and a pending suggestion keeps a second copy of it in `evidence_json`.
/// Deleting the meeting has to take both.
#[test]
fn a_deleted_meetings_words_leave_both_the_ledger_and_the_feed() {
    let (_directory, store) = new_store();
    let inputs = FakeInputs::empty();
    let mut sessions = Vec::new();
    // Three days of the same rewrite clears the occurrence and day floors, so
    // the excerpt reaches a pending suggestion rather than only the ledger.
    for day in 0..3 {
        let session_id = meeting(&store, "North Star review", 1_000 + day);
        transcript(&store, session_id, "we shipped handy on friday");
        segment_edit(
            &store,
            session_id,
            "we shipped Sona on friday",
            NOON + day * DAY_MS,
        );
        finalize(&store, &inputs, session_id, day);
        sessions.push(session_id);
    }

    assert!(
        suggestions(&store, &inputs).contains(&LearningSuggestion::VocabularyCorrection {
            spoken: "handy".to_string(),
            written: "Sona".to_string(),
        }),
        "three days of the same meeting rewrite did not reach the feed"
    );
    assert!(
        rendered_examples(&store)
            .iter()
            .any(|example| example.contains("we shipped Sona on friday")),
        "the card is not rendering the meeting's own sentence"
    );

    for session_id in sessions {
        store
            .connection()
            .unwrap()
            .execute(
                "DELETE FROM meeting_sessions WHERE id = ?1",
                params![session_id.uuid().to_string()],
            )
            .unwrap();
    }

    assert_eq!(
        observation_examples(&store),
        Vec::<String>::new(),
        "a deleted meeting's excerpt survived in the ledger"
    );
    assert_eq!(
        rendered_examples(&store),
        Vec::<String>::new(),
        "a deleted meeting's excerpt survived in a pending suggestion's evidence"
    );
}

/// Dictation-sourced evidence answers to the retention horizon, not to any
/// meeting, so nothing about deleting meetings may touch it.
#[test]
fn dictation_sourced_evidence_belongs_to_no_meeting() {
    let (_directory, store) = new_store();
    let session_id = meeting(&store, "Unrelated", 1_000);
    let inputs = FakeInputs::with_runs(domain_corpus(2, 2));
    sweep(&store, &inputs, "dictation-owned");
    let before = observation_examples(&store);
    assert!(!before.is_empty(), "the sweep recorded no excerpt");

    store
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM meeting_sessions WHERE id = ?1",
            params![session_id.uuid().to_string()],
        )
        .unwrap();

    assert_eq!(
        observation_examples(&store),
        before,
        "deleting a meeting took evidence that came from dictation"
    );
}

/// The two sets the Settings list and `set_workflow_enabled` are built on have
/// to partition the enum, and every id in it has to be one the schema accepts:
/// `matching_enabled_workflows_in` tolerates a missing settings row, so an id
/// the `CHECK` list rejects is a workflow that silently never runs.
#[test]
fn the_configurable_and_permanent_sets_partition_every_workflow() {
    for workflow_id in WorkflowId::ALL {
        assert_ne!(
            WorkflowId::CONFIGURABLE.contains(&workflow_id),
            WorkflowId::PERMANENT.contains(&workflow_id),
            "{workflow_id:?} is in both sets or in neither"
        );
    }
    assert_eq!(
        WorkflowId::CONFIGURABLE.len() + WorkflowId::PERMANENT.len(),
        WorkflowId::ALL.len(),
        "one of the two sets names a workflow that is not in ALL"
    );

    let (_directory, store) = new_store();
    let connection = store.connection().unwrap();
    let seeded = connection
        .prepare("SELECT workflow_id FROM workflow_settings ORDER BY workflow_id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut expected = WorkflowId::ALL
        .iter()
        .map(|workflow_id| workflow_id.as_str().to_string())
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(
        seeded, expected,
        "the migration's allowed-value list and WorkflowId::ALL disagree"
    );
}

/// Both names a transcription route answers to are persisted, and they are not
/// the same string for Deepgram.
///
/// `as_str` is the subject key of `learning_advice_baselines`, so drifting it
/// re-keys every baseline and re-advises a subject the user dismissed. The
/// serde value is what the settings UI writes, so renaming that breaks reading
/// a stored mode. Deepgram's two spellings diverged on purpose when the UI
/// adopted `deepgram_nova_3`; pinning both sides here is what keeps either one
/// from being "fixed" into the other.
#[test]
fn each_engine_pins_its_ledger_key_and_its_wire_value() {
    let named = [
        RequestedEngine::Local,
        RequestedEngine::DeepgramNova3,
        RequestedEngine::ElevenLabsScribeV2,
    ]
    .map(|engine| {
        let wire = serde_json::to_value(engine)
            .expect("an engine serializes")
            .as_str()
            .expect("an engine serializes to a string")
            .to_string();
        (engine.as_str(), wire)
    });

    assert_eq!(
        named,
        [
            ("local", "local".to_string()),
            ("deepgram_nova3", "deepgram_nova_3".to_string()),
            ("eleven_labs_scribe_v2", "eleven_labs_scribe_v2".to_string()),
        ]
    );
}

// ------------------------------------------------------------- forward compat

/// A workflow event kind this build does not know is a row a newer build wrote.
/// Skipping it keeps the reconciliation queue moving; failing on it would stall
/// every other pending event on the machine.
#[test]
fn an_unknown_event_kind_does_not_stall_the_pending_scan() {
    let (_directory, store) = new_store();
    let inputs = FakeInputs::empty();
    let dispatch = store
        .record_workflow_event(event(
            WorkflowEventKind::MeetingStarted,
            serde_json::json!({"session_id": MeetingSessionId::new().uuid().to_string()}),
            "pending-known",
        ))
        .unwrap();
    // A kind from the future. `ignore_check_constraints` is how a test writes
    // the row a newer build's own widened CHECK would have accepted.
    store
        .connection()
        .unwrap()
        .execute_batch(
            "PRAGMA ignore_check_constraints = ON;
             INSERT INTO workflow_events (
                id, kind, payload_json, occurred_at_utc_ms, source, dedupe_key
             ) VALUES ('11111111-1111-4111-8111-111111111111', 'loop_seven',
                       '{}', 1, 'test', 'from-the-future');
             PRAGMA ignore_check_constraints = OFF;",
        )
        .unwrap();

    let pending = store.pending_workflow_event_ids().unwrap();
    assert!(
        pending.contains(&dispatch.event_id),
        "the known pending event was lost behind an unreadable one"
    );

    // And the known event still runs.
    let receipts = store
        .run_workflow_event(dispatch.event_id, false, &inputs)
        .unwrap();
    assert!(receipts
        .iter()
        .any(|receipt| receipt.workflow_id == WorkflowId::PersonLinking));
}

// ------------------------------------------------------------------- fixtures

/// A human review edit on this meeting's one transcript segment.
fn segment_edit(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    replacement_text: &str,
    operator_at_utc_ms: i64,
) {
    let connection = store.connection().unwrap();
    let segment_id: String = connection
        .query_row(
            "SELECT s.segment_id
               FROM meeting_transcript_revisions r
               JOIN meeting_transcript_segments s
                 ON s.transcript_revision_id = r.transcript_revision_id
              WHERE r.session_id = ?1",
            params![session_id.uuid().to_string()],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meeting_segment_edits (
                segment_id, edit_sequence, replacement_text, removed, operator_at_utc_ms
             ) VALUES (?1, 1, ?2, 0, ?3)",
            params![segment_id, replacement_text, operator_at_utc_ms],
        )
        .unwrap();
}

fn finalize(store: &MeetingStore, inputs: &FakeInputs, session_id: MeetingSessionId, nonce: i64) {
    store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::MeetingFinalized,
                serde_json::json!({
                    "session_id": session_id.uuid().to_string(),
                    "known_vocabulary": [],
                }),
                &format!("finalized:{nonce}"),
            ),
            inputs,
        )
        .unwrap();
}

/// Every excerpt the ledger is holding.
fn observation_examples(store: &MeetingStore) -> Vec<String> {
    store
        .connection()
        .unwrap()
        .prepare(
            "SELECT example_context FROM learning_observations
              WHERE example_context IS NOT NULL
              ORDER BY example_context",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// Every excerpt a pending suggestion would render on its card — the second
/// copy, in `evidence_json`.
fn rendered_examples(store: &MeetingStore) -> Vec<String> {
    let mut examples = store
        .learning_suggestions(&FakeInputs::empty())
        .unwrap()
        .entries
        .into_iter()
        .flat_map(|entry| entry.evidence.examples)
        .collect::<Vec<_>>();
    examples.sort();
    examples
}

fn correction(
    store: &MeetingStore,
    inputs: &FakeInputs,
    spoken: &str,
    written: &str,
    day: i64,
    nonce: i64,
) {
    let mut new_event = event(
        WorkflowEventKind::DictationCorrectionRecorded,
        serde_json::json!({"spoken": spoken, "written": written}),
        &format!("correction:{spoken}->{written}:{day}:{nonce}"),
    );
    new_event.occurred_at_utc_ms = NOON + day * DAY_MS;
    store
        .record_and_run_workflow_event(new_event, inputs)
        .unwrap();
}

fn standing_consent(store: &MeetingStore, session_id: MeetingSessionId, series_key: &str) {
    store
        .grant_series_consent(
            series_key,
            1,
            &[crate::meeting::types::SourceKind::Microphone],
            1_000,
        )
        .unwrap();
    let acknowledgement = serde_json::json!({
        "consent_id": uuid::Uuid::new_v4().to_string(),
        "session_id": session_id.uuid().to_string(),
        "attempt_number": 1,
        "preflight_revision": 0,
        "policy_version": 1,
        "acknowledged_at_utc_ms": 1_000,
        "provenance": {
            "kind": "standing_series",
            "series_key": series_key,
            "granted_at_utc_ms": 1_000,
        },
        "microphone_acknowledged": true,
        "system_audio_acknowledged": false,
        "known_missing_sources_acknowledged": [],
        "degraded_start_policy": "abort_if_required_source_fails",
        "destination": {"kind": "local"},
        "remote_acknowledgement": null,
    });
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_consents (
                consent_id, session_id, attempt_number, preflight_revision,
                policy_version, acknowledgement_json, acknowledged_at_utc_ms
             ) VALUES (?1, ?2, 1, 0, 1, ?3, 1000)",
            params![
                uuid::Uuid::new_v4().to_string(),
                session_id.uuid().to_string(),
                acknowledgement.to_string()
            ],
        )
        .unwrap();
}

fn calendar_facts(store: &MeetingStore, session_id: MeetingSessionId, series_key: &str) {
    let event_json = serde_json::json!({
        "eventKey": format!("{series_key}-occurrence"),
        "seriesKey": series_key,
    });
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_calendar_facts(session_id, event_key, event_json)
             VALUES (?1, ?2, ?3)",
            params![
                session_id.uuid().to_string(),
                format!("{series_key}-occurrence"),
                event_json.to_string()
            ],
        )
        .unwrap();
}

fn confirm_link(store: &MeetingStore, session_id: MeetingSessionId, person_id: PersonId) {
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_person_links (
                meeting_id, person_id, source, confidence, created_at_utc_ms
             ) VALUES (?1, ?2, 'calendar', 'confirmed', 1)",
            params![session_id.uuid().to_string(), person_id.uuid().to_string()],
        )
        .unwrap();
}

fn priming_rows(store: &MeetingStore, session_id: MeetingSessionId) -> i64 {
    store
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM meeting_series_priming WHERE session_id = ?1",
            params![session_id.uuid().to_string()],
            |row| row.get(0),
        )
        .unwrap()
}
