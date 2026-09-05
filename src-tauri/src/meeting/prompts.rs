//! Saved prompts at the manager boundary: keep one, forget one, or ask one.
//!
//! Four of the five methods are thin, like `series.rs` beside them: the store
//! owns the rows, the fence and the receipt, and this layer only maps store
//! errors onto the command error the webview understands.
//!
//! The fifth is [`MeetingSessionManager::run_saved_prompt`], and it is where
//! the evidence for a prompt is decided. Three nouns, two shapes:
//!
//! * A meeting is read whole — its transcript, its manual notes and the rough
//!   notes the reader typed — which is the notes pass's evidence.
//! * A person and a series are read through the meetings behind them, searched
//!   with the prompt as the query, which is the question pass's evidence.
//!
//! Which meetings those are is a store query rather than a corpus search, so a
//! prompt about a person reads meetings that person was actually confirmed in
//! and a prompt about a series reads that series' own meetings. The engine is
//! chosen once, by `text_generator_for_session`, about the newest of them —
//! D14's boundary is not widened here, and a prompt that reads several meetings
//! is written wherever that meeting's series says it may be.
//!
//! Nothing retries. A run that produced no answer is stored saying so.

use super::processing::{MeetingProcessingService, PromptEvidenceScope, PromptGenerationRequest};
use super::prompt_types::{
    PromptRun, PromptTargetRef, SavedPrompt, SavedPromptDeleteRequest, SavedPromptList,
    SavedPromptMutationResult, SavedPromptRunRequest, SavedPromptSaveRequest,
};
use super::session::MeetingSessionManager;
use super::store::prompts::PROMPT_SESSION_LIMIT;
use super::store::MeetingStore;
use super::types::{MeetingCommandError, MeetingSessionId, PromptRunId, SavedPromptId};
use super::workflow_engine::{map_store_error, now_utc_ms};

impl MeetingSessionManager {
    pub async fn saved_prompts(&self) -> Result<SavedPromptList, MeetingCommandError> {
        self.store().await?.saved_prompts().map_err(map_store_error)
    }

    pub async fn save_saved_prompt(
        &self,
        request: SavedPromptSaveRequest,
    ) -> Result<SavedPromptMutationResult, MeetingCommandError> {
        self.store()
            .await?
            .save_saved_prompt(&request, now_utc_ms())
            .map_err(map_store_error)
    }

    pub async fn delete_saved_prompt(
        &self,
        request: SavedPromptDeleteRequest,
    ) -> Result<SavedPromptMutationResult, MeetingCommandError> {
        self.store()
            .await?
            .delete_saved_prompt(&request, now_utc_ms())
            .map_err(map_store_error)
    }

    /// Every answer one noun has been given, newest first.
    pub async fn prompt_runs(
        &self,
        target: PromptTargetRef,
    ) -> Result<Vec<PromptRun>, MeetingCommandError> {
        self.store()
            .await?
            .prompt_runs(&target)
            .map_err(map_store_error)
    }

    /// Ask one saved prompt about one noun, and keep the answer.
    ///
    /// A noun with no meetings behind it is [`MeetingCommandError::NotFound`]
    /// rather than a stored failure: there was nothing to ask about, so there
    /// is no result worth keeping beside the ones that were actually attempted.
    pub async fn run_saved_prompt(
        &self,
        request: SavedPromptRunRequest,
    ) -> Result<PromptRun, MeetingCommandError> {
        let store = self.store().await?;
        let prompt = store
            .saved_prompt(request.prompt_id)
            .map_err(map_store_error)?
            .ok_or(MeetingCommandError::NotFound)?;
        let scope = evidence_scope(&store, &request.target)?;
        // Held before the scope is spent: this is the meeting whose deletion
        // takes the run with it, and it is the same meeting the engine choice
        // below is made about.
        let anchor = scope.anchor();
        let run = self.processing().run_saved_prompt(
            &store,
            PromptGenerationRequest {
                run_id: PromptRunId::new(),
                prompt,
                target: request.target,
                produced_at_utc_ms: now_utc_ms(),
                scope,
            },
        );
        store
            .record_prompt_run(&run, anchor)
            .map_err(map_store_error)?;
        Ok(run)
    }
}

/// Which meetings this noun's prompt reads.
fn evidence_scope(
    store: &MeetingStore,
    target: &PromptTargetRef,
) -> Result<PromptEvidenceScope, MeetingCommandError> {
    match target {
        PromptTargetRef::Meeting { session_id } => {
            // Read the session rather than trusting the id: a prompt run
            // against a meeting that has been deleted must not write a row
            // anchored to a session that is no longer there.
            store
                .session_snapshot(*session_id)
                .map_err(map_store_error)?;
            Ok(PromptEvidenceScope::Meeting(*session_id))
        }
        PromptTargetRef::Person { person_id } => sessions(
            store
                .person_session_ids(*person_id, PROMPT_SESSION_LIMIT)
                .map_err(map_store_error)?,
        ),
        PromptTargetRef::Series { series_key } => sessions(
            store
                .series_session_ids(series_key, PROMPT_SESSION_LIMIT)
                .map_err(map_store_error)?,
        ),
    }
}

fn sessions(
    session_ids: Vec<MeetingSessionId>,
) -> Result<PromptEvidenceScope, MeetingCommandError> {
    if session_ids.is_empty() {
        return Err(MeetingCommandError::NotFound);
    }
    Ok(PromptEvidenceScope::Search(session_ids))
}

/// Run one saved prompt against the meeting that just finished, for D22's
/// `run_prompt` automation.
///
/// `None` is the prompt having been deleted since the automation was
/// configured, which is a configuration the executor cannot run rather than a
/// generation that failed — the caller records the difference.
///
/// The automation always asks about the meeting, whatever noun the prompt says
/// it is about: an after-meeting pass has one meeting and no person, and
/// running a person prompt against nobody would be a second, quieter way for a
/// prompt to mean something else.
pub(crate) fn run_prompt_for_meeting(
    store: &MeetingStore,
    processing: &MeetingProcessingService,
    prompt_id: SavedPromptId,
    session_id: MeetingSessionId,
    produced_at_utc_ms: i64,
) -> Option<PromptRun> {
    let prompt: SavedPrompt = store.saved_prompt(prompt_id).ok().flatten()?;
    let run = processing.run_saved_prompt(
        store,
        PromptGenerationRequest {
            run_id: PromptRunId::new(),
            prompt,
            target: PromptTargetRef::Meeting { session_id },
            scope: PromptEvidenceScope::Meeting(session_id),
            produced_at_utc_ms,
        },
    );
    store.record_prompt_run(&run, session_id).ok()?;
    Some(run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::automation_types::{
        MeetingAutomationKind, MeetingAutomationRunState, MeetingSeriesAutomationSetRequest,
    };
    use crate::meeting::automations::{AutomationEffects, EffectOutcome, ReminderItem};
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::processing::{
        MeetingTextGenerationError, MeetingTextGenerator, ReplyShape,
    };
    use crate::meeting::prompt_types::{
        PromptOutput, PromptRunFailure, PromptRunResult, PromptTarget,
    };
    use crate::meeting::store::workflow_core_tests::{meeting, reviewable_meeting, store};
    use crate::meeting::store::{ArtifactRevisionInput, TranscriptRevisionInput};
    use crate::meeting::types::{
        CitedArtifactText, GeneratedMeetingArtifacts, ManualNote, ManualNoteId,
        MeetingArtifactState, MeetingOperationId, OperationResult, ProcessingDestination,
        SourceKind,
    };
    use std::sync::Arc;

    const NOW: i64 = 1_700_000_000_000;
    const SERIES_KEY: &str = "weekly-pricing";
    const SCHEMA: &str = r#"{"type":"object","required":["decisions"],"properties":{"decisions":{"type":"array","items":{"type":"string"}}}}"#;

    /// An engine that answers whatever the test decided, and is always there.
    struct StubGenerator(Result<String, MeetingTextGenerationError>);

    impl MeetingTextGenerator for StubGenerator {
        fn is_available(&self) -> bool {
            true
        }

        fn model_id(&self) -> &'static str {
            "stub-engine"
        }

        fn model_version(&self) -> std::borrow::Cow<'static, str> {
            std::borrow::Cow::Borrowed("stub-1")
        }

        fn max_input_bytes(&self) -> usize {
            usize::MAX
        }

        fn generate(
            &self,
            _system_prompt: &str,
            _evidence: &str,
            _max_tokens: i32,
            _shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            self.0.clone()
        }
    }

    /// Effects that refuse. Only the prompt kind is enabled in these tests, and
    /// it never reaches this trait — a call here would be the executor sending
    /// a prompt somewhere it does not belong.
    struct NoEffects;

    impl AutomationEffects for NoEffects {
        fn write_reminders(&self, _items: &[ReminderItem]) -> EffectOutcome {
            unreachable!("no reminders automation is enabled")
        }

        fn run_shortcut(&self, _name: &str, _stdin: &[u8]) -> EffectOutcome {
            unreachable!("no shortcut automation is enabled")
        }

        fn post_webhook(&self, _url: &str, _body: &[u8]) -> EffectOutcome {
            unreachable!("no webhook automation is enabled")
        }
    }

    /// A headless service whose only engine answers `answer`.
    ///
    /// Both slots hold it: a build with no app handle has remote intelligence
    /// off, so `choose_text_engine` picks the local one and the relay slot is
    /// only there to be inert.
    fn service(answer: Result<&str, MeetingTextGenerationError>) -> MeetingProcessingService {
        let service = MeetingProcessingService::new(None);
        let generator: Arc<dyn MeetingTextGenerator> =
            Arc::new(StubGenerator(answer.map(str::to_string)));
        service.set_text_generators(Arc::clone(&generator), generator);
        service
    }

    /// A meeting with something to read.
    ///
    /// One manual note rather than a transcript: `artifact_evidence` reads both,
    /// and a segment needs a source track that has nothing to do with what these
    /// tests are about.
    fn meeting_with_evidence(store: &MeetingStore) -> MeetingSessionId {
        let session_id = reviewable_meeting(store, "Pricing review", NOW);
        let note = ManualNote {
            note_id: ManualNoteId::new(),
            session_id,
            start_offset_ns: None,
            end_offset_ns: None,
            body: "We decided to ship the enterprise tier on Friday.".to_string(),
            revision: 0,
            created_at_utc_ms: NOW,
            updated_at_utc_ms: NOW,
        };
        store
            .create_note(MeetingOperationId::new(), NOW, &note, 0)
            .unwrap();
        session_id
    }

    fn save(store: &MeetingStore, name: &str, output: PromptOutput) -> SavedPromptId {
        let revision = store.saved_prompts().unwrap().revision;
        let result = store
            .save_saved_prompt(
                &SavedPromptSaveRequest {
                    operation_id: MeetingOperationId::new(),
                    prompt_id: None,
                    name: name.to_string(),
                    body: "List the decisions.".to_string(),
                    output,
                    target: PromptTarget::Meeting,
                    expected_revision: revision,
                },
                NOW,
            )
            .unwrap();
        assert_eq!(result.receipt.result, OperationResult::Committed);
        result.prompts.prompts.last().unwrap().prompt_id
    }

    #[test]
    fn the_seeded_prompts_are_ordinary_editable_rows() {
        let (_directory, store) = store();

        let list = store.saved_prompts().unwrap();

        assert_eq!(list.prompts.len(), 3);
        assert_eq!(list.prompts[0].name, "Decisions with owners");
        assert!(list
            .prompts
            .iter()
            .all(|prompt| prompt.output == PromptOutput::Text
                && prompt.target == PromptTarget::Meeting));
    }

    #[test]
    fn saving_listing_and_deleting_round_trips() {
        let (_directory, store) = store();
        let seeded = store.saved_prompts().unwrap().prompts.len();

        let prompt_id = save(&store, "Blockers", PromptOutput::Text);
        let after_save = store.saved_prompts().unwrap();
        assert_eq!(after_save.prompts.len(), seeded + 1);
        assert_eq!(after_save.revision, 1);

        // The same id saved again rewrites in place rather than duplicating.
        let rewritten = store
            .save_saved_prompt(
                &SavedPromptSaveRequest {
                    operation_id: MeetingOperationId::new(),
                    prompt_id: Some(prompt_id),
                    name: "Blockers and owners".to_string(),
                    body: "List the blockers.".to_string(),
                    output: PromptOutput::Text,
                    target: PromptTarget::Series,
                    expected_revision: after_save.revision,
                },
                NOW + 1,
            )
            .unwrap();
        assert_eq!(rewritten.prompts.prompts.len(), seeded + 1);
        let stored = store.saved_prompt(prompt_id).unwrap().unwrap();
        assert_eq!(stored.name, "Blockers and owners");
        assert_eq!(stored.target, PromptTarget::Series);

        let deleted = store
            .delete_saved_prompt(
                &SavedPromptDeleteRequest {
                    operation_id: MeetingOperationId::new(),
                    prompt_id,
                    expected_revision: rewritten.prompts.revision,
                },
                NOW + 2,
            )
            .unwrap();
        assert_eq!(deleted.receipt.result, OperationResult::Committed);
        assert_eq!(deleted.prompts.prompts.len(), seeded);
        assert!(store.saved_prompt(prompt_id).unwrap().is_none());
    }

    #[test]
    fn a_save_from_a_stale_read_is_refused_and_changes_nothing() {
        let (_directory, store) = store();
        save(&store, "Blockers", PromptOutput::Text);

        let refused = store
            .save_saved_prompt(
                &SavedPromptSaveRequest {
                    operation_id: MeetingOperationId::new(),
                    prompt_id: None,
                    name: "Risks".to_string(),
                    body: "List the risks.".to_string(),
                    output: PromptOutput::Text,
                    target: PromptTarget::Meeting,
                    expected_revision: 0,
                },
                NOW,
            )
            .unwrap();

        assert_eq!(refused.receipt.result, OperationResult::Rejected);
        assert!(!refused
            .prompts
            .prompts
            .iter()
            .any(|prompt| prompt.name == "Risks"));
    }

    #[test]
    fn a_prompt_that_cannot_be_stored_is_refused_at_the_boundary() {
        let (_directory, store) = store();
        let revision = store.saved_prompts().unwrap().revision;

        let error = store
            .save_saved_prompt(
                &SavedPromptSaveRequest {
                    operation_id: MeetingOperationId::new(),
                    prompt_id: None,
                    name: "Decisions".to_string(),
                    body: "List them.".to_string(),
                    output: PromptOutput::Schema {
                        json_schema: "{not json".to_string(),
                    },
                    target: PromptTarget::Meeting,
                    expected_revision: revision,
                },
                NOW,
            )
            .expect_err("an unparseable schema");

        assert_eq!(error, crate::meeting::store::StoreError::Invalid);
    }

    #[test]
    fn a_text_prompt_stores_what_the_engine_answered() {
        let (_directory, store) = store();
        let session_id = meeting_with_evidence(&store);
        let prompt_id = save(&store, "Decisions", PromptOutput::Text);

        let run = run_prompt_for_meeting(
            &store,
            &service(Ok("- Ship on Friday")),
            prompt_id,
            session_id,
            NOW,
        )
        .expect("the prompt exists");

        assert_eq!(
            run.result,
            PromptRunResult::Text {
                text: "- Ship on Friday".to_string()
            }
        );
        assert_eq!(run.model_id, "stub-engine");
        assert_eq!(
            store
                .prompt_runs(&PromptTargetRef::Meeting { session_id })
                .unwrap(),
            vec![run]
        );
    }

    #[test]
    fn a_schema_prompt_stores_json_that_checks() {
        let (_directory, store) = store();
        let session_id = meeting_with_evidence(&store);
        let prompt_id = save(
            &store,
            "Decisions",
            PromptOutput::Schema {
                json_schema: SCHEMA.to_string(),
            },
        );

        let run = run_prompt_for_meeting(
            &store,
            &service(Ok(r#"{"decisions":["Ship on Friday"]}"#)),
            prompt_id,
            session_id,
            NOW,
        )
        .expect("the prompt exists");

        assert_eq!(
            run.result,
            PromptRunResult::Json {
                json: r#"{"decisions":["Ship on Friday"]}"#.to_string()
            }
        );
    }

    #[test]
    fn an_answer_that_is_not_the_schema_stores_a_failure() {
        let (_directory, store) = store();
        let session_id = meeting_with_evidence(&store);
        let prompt_id = save(
            &store,
            "Decisions",
            PromptOutput::Schema {
                json_schema: SCHEMA.to_string(),
            },
        );

        let prose = run_prompt_for_meeting(
            &store,
            &service(Ok("We decided to ship on Friday.")),
            prompt_id,
            session_id,
            NOW,
        )
        .expect("the prompt exists");
        let wrong_shape = run_prompt_for_meeting(
            &store,
            &service(Ok(r#"{"decisions":"Ship on Friday"}"#)),
            prompt_id,
            session_id,
            NOW + 1,
        )
        .expect("the prompt exists");

        for run in [&prose, &wrong_shape] {
            assert_eq!(
                run.result,
                PromptRunResult::Failed {
                    reason: PromptRunFailure::SchemaMismatch
                }
            );
        }
        // Both attempts are kept. Nothing retries, so the record of a failed
        // run is the only thing that says it happened.
        assert_eq!(
            store
                .prompt_runs(&PromptTargetRef::Meeting { session_id })
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn a_meeting_with_nothing_written_about_it_records_no_evidence() {
        let (_directory, store) = store();
        let session_id = meeting(&store, "Empty", NOW);
        let prompt_id = save(&store, "Decisions", PromptOutput::Text);

        let run =
            run_prompt_for_meeting(&store, &service(Ok("anything")), prompt_id, session_id, NOW)
                .expect("the prompt exists");

        assert_eq!(
            run.result,
            PromptRunResult::Failed {
                reason: PromptRunFailure::NoEvidence
            }
        );
    }

    #[test]
    fn deleting_a_prompt_takes_its_answers_with_it() {
        let (_directory, store) = store();
        let session_id = meeting_with_evidence(&store);
        let prompt_id = save(&store, "Decisions", PromptOutput::Text);
        run_prompt_for_meeting(&store, &service(Ok("- Ship")), prompt_id, session_id, NOW)
            .expect("the prompt exists");

        store
            .delete_saved_prompt(
                &SavedPromptDeleteRequest {
                    operation_id: MeetingOperationId::new(),
                    prompt_id,
                    expected_revision: store.saved_prompts().unwrap().revision,
                },
                NOW + 1,
            )
            .unwrap();

        assert!(store
            .prompt_runs(&PromptTargetRef::Meeting { session_id })
            .unwrap()
            .is_empty());
    }

    #[test]
    fn the_run_prompt_automation_runs_once_and_is_never_retried() {
        let (_directory, store) = store();
        let session_id = finished_series_meeting(&store);
        let prompt_id = save(&store, "Decisions", PromptOutput::Text);
        enable_run_prompt(&store, prompt_id);
        let service = service(Ok("- Ship on Friday"));

        let first = crate::meeting::automations::run_for_meeting(
            &store, session_id, &NoEffects, &service, NOW,
        );
        let second = crate::meeting::automations::run_for_meeting(
            &store,
            session_id,
            &NoEffects,
            &service,
            NOW + 1_000,
        );

        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, MeetingAutomationKind::RunPrompt);
        assert_eq!(first[0].state, MeetingAutomationRunState::Committed);
        assert!(
            second.is_empty(),
            "one artifact revision gets one attempt at one kind"
        );
        assert_eq!(
            store
                .prompt_runs(&PromptTargetRef::Meeting { session_id })
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn an_automation_whose_prompt_was_deleted_fails_visibly() {
        let (_directory, store) = store();
        let session_id = finished_series_meeting(&store);
        let prompt_id = save(&store, "Decisions", PromptOutput::Text);
        enable_run_prompt(&store, prompt_id);
        store
            .delete_saved_prompt(
                &SavedPromptDeleteRequest {
                    operation_id: MeetingOperationId::new(),
                    prompt_id,
                    expected_revision: store.saved_prompts().unwrap().revision,
                },
                NOW,
            )
            .unwrap();

        let receipts = crate::meeting::automations::run_for_meeting(
            &store,
            session_id,
            &NoEffects,
            &service(Ok("- Ship")),
            NOW,
        );

        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].state, MeetingAutomationRunState::Failed);
    }

    fn enable_run_prompt(store: &MeetingStore, prompt_id: SavedPromptId) {
        let revision = store.series_automations(SERIES_KEY).unwrap().revision;
        let result = store
            .set_series_automation(
                &MeetingSeriesAutomationSetRequest {
                    operation_id: MeetingOperationId::new(),
                    series_key: SERIES_KEY.to_string(),
                    kind: MeetingAutomationKind::RunPrompt,
                    enabled: true,
                    target: Some(prompt_id.uuid().to_string()),
                    expected_revision: revision,
                },
                NOW,
            )
            .unwrap();
        assert_eq!(result.receipt.result, OperationResult::Committed);
    }

    /// A meeting in a calendar series, in review, with current notes — what the
    /// after-meeting pass requires before it will run anything.
    fn finished_series_meeting(store: &MeetingStore) -> MeetingSessionId {
        let session_id = meeting_with_evidence(store);
        store
            .remember_calendar_facts(
                session_id,
                &CalendarEventSummary {
                    event_key: format!("{SERIES_KEY}#{NOW}"),
                    series_key: SERIES_KEY.to_string(),
                    title: "Weekly pricing".to_string(),
                    attendee_count: 2,
                    start_utc_ms: NOW,
                    end_utc_ms: NOW + 1_800_000,
                    attendees: Vec::new(),
                    notes: None,
                    calendar_name: None,
                    url: None,
                },
            )
            .unwrap();
        let transcript_revision_id = store
            .begin_transcript_revision(TranscriptRevisionInput {
                session_id,
                engine_id: "test",
                model_version: None,
                destination: &ProcessingDestination::Local,
                source_set: &[SourceKind::Microphone],
                language: "en",
            })
            .unwrap();
        store
            .store_artifact_revision(ArtifactRevisionInput {
                session_id,
                transcript_revision_id,
                // The artifact reads as current only while its input revision
                // is the session's, and the note above moved that.
                input_revision: store.session_snapshot(session_id).unwrap().revision,
                template_id: "test",
                template_version: 1,
                generation_key: "test-key",
                state: MeetingArtifactState::Current,
                content: Some(&GeneratedMeetingArtifacts {
                    summary: CitedArtifactText {
                        text: "Pricing stayed open.".to_string(),
                        citations: Vec::new(),
                    },
                    summary_trace: Vec::new(),
                    outline: Vec::new(),
                    decisions: Vec::new(),
                    action_items: Vec::new(),
                    key_questions: Vec::new(),
                    risks: Vec::new(),
                    follow_up_draft: CitedArtifactText {
                        text: String::new(),
                        citations: Vec::new(),
                    },
                    ledger: None,
                }),
                generated_at_utc_ms: NOW,
            })
            .unwrap();
        session_id
    }
}
