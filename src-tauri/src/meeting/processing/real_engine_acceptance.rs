#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::apple_intelligence::test_receipts::started as actual_sdk_started;
    use crate::meeting::import_formats::read_transcript_export;
    use std::collections::HashSet;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    const SCRATCH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../.progress/meeting-acceptance"
    );
    const MAX_ACTUAL_CALLS: usize = 192;

    struct ActualAppleWitness {
        output: std::path::PathBuf,
        calls: AtomicUsize,
    }

    impl MeetingTextGenerator for ActualAppleWitness {
        fn is_available(&self) -> bool {
            AppleIntelligenceGenerator.is_available()
        }
        fn model_id(&self) -> &'static str {
            AppleIntelligenceGenerator.model_id()
        }
        fn model_version(&self) -> Cow<'static, str> {
            AppleIntelligenceGenerator.model_version()
        }
        fn max_input_bytes(&self) -> usize {
            AppleIntelligenceGenerator.max_input_bytes()
        }
        fn context_window_bytes(&self) -> Option<usize> {
            AppleIntelligenceGenerator.context_window_bytes()
        }
        fn reserved_output_tokens(&self, requested: i32) -> usize {
            AppleIntelligenceGenerator.reserved_output_tokens(requested)
        }
        fn generate(
            &self,
            prompt: &str,
            input: &str,
            max_tokens: i32,
            shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(
                index < MAX_ACTUAL_CALLS,
                "bounded smoke exhausted {MAX_ACTUAL_CALLS} logical generator requests"
            );
            assert!(input.len() <= evidence_budget(self, prompt, max_tokens));
            let stem = self.output.join(format!("call-{index:03}"));
            std::fs::write(stem.with_extension("request.json"), serde_json::to_vec_pretty(&serde_json::json!({
            "engine":self.model_id(),"model_version":self.model_version(),"prompt":prompt,"input":input,
            "requested_output_tokens":max_tokens,"reserved_output_tokens":self.reserved_output_tokens(max_tokens),
            "prompt_bytes":prompt.len(),"input_bytes":input.len(),"evidence_budget_bytes":evidence_budget(self,prompt,max_tokens)
        })).expect("synthetic request JSON")).expect("write synthetic actual-engine request");
            let started = Instant::now();
            let bounded_tokens = max_tokens
                .min(i32::try_from(ON_DEVICE_REPLY_TOKENS).expect("fixed native reply bound"));
            std::env::set_var(
                "SONA_ACTUAL_MEETING_SDK_RECEIPT",
                self.output.join("sdk-calls.jsonl"),
            );
            let sdk_before = actual_sdk_started();
            let native_result = apple_json::generate(prompt, input, bounded_tokens, shape);
            if let Err(error) = &native_result {
                println!("actual_native_error={error}");
            }
            let sdk_after = actual_sdk_started();
            assert!(
                sdk_after > sdk_before,
                "the actual SDK boundary must be observed"
            );
            assert!(
                (1..=2).contains(&(sdk_after - sdk_before)),
                "a logical operation uses at most the original two bounded SDK requests"
            );
            assert!(
                sdk_after <= MAX_ACTUAL_CALLS,
                "the original actual SDK request budget remains enforced"
            );
            let result = native_result.map_err(|_| MeetingTextGenerationError::Failed);
            std::fs::write(stem.with_extension("response.json"), serde_json::to_vec_pretty(&serde_json::json!({
            "elapsed_ms":started.elapsed().as_millis(),"reply":result.as_ref().ok(),"error":result.as_ref().err().map(|error| format!("{error:?}")),"sdk_requests_before":sdk_before,"sdk_requests_after":sdk_after
        })).expect("actual response JSON")).expect("write actual-engine response");
            println!("actual_apple_request={index} actual_sdk_requests={sdk_after} input_bytes={} elapsed_ms={} result={:?}",input.len(),started.elapsed().as_millis(),result.as_ref().map(|reply|reply.len()));
            result
        }
    }

    struct SyntheticMeeting {
        session_id: MeetingSessionId,
        expected_decisions: Vec<(TranscriptSegmentId, Vec<&'static str>)>,
        report_opening: TranscriptSegmentId,
        report_revision: TranscriptSegmentId,
    }

    /// `distinct` numbers every filler turn, so no two lines repeat and the
    /// meeting is read in many parts, as a real one is.
    fn seed_synthetic_meeting(
        store: &MeetingStore,
        output: &Path,
        distinct: bool,
    ) -> SyntheticMeeting {
        let filler = "The team reviews the existing dashboard and checks the current setup. This part introduces no new decision, commitment, owner or deadline. ".repeat(8);
        let mut texts: Vec<String> = (0..128)
            .map(|index| {
                if distinct {
                    format!("Status check {index}. {filler}")
                } else {
                    filler.clone()
                }
            })
            .collect();
        texts[0] = "Jordan says: We have decided to use the cobalt design. Everyone agrees; this is the final design decision.".to_string();
        texts[1] = "Casey says: I will send the report Friday. Jordan confirms that Casey owns sending this report.".to_string();
        texts[64] = "Jordan says: We have decided to keep the annual price at 240 dollars. Everyone agrees; the annual price remains 240 dollars.".to_string();
        texts[126] = "Casey says: Change my earlier report deadline. I will send that same report Monday instead of Friday. Friday is cancelled. Jordan confirms that Casey's one report commitment is now due Monday.".to_string();
        texts[127] = "Jordan says: We have decided to release on November 12. Everyone agrees; November 12 is the final release date.".to_string();
        let transcript_bytes = texts.iter().map(String::len).sum::<usize>();
        assert!(
            transcript_bytes > 96 * 1024,
            "the source must exceed the old prefix ceiling"
        );
        let mut otter = String::from(
            "Synthetic complete-reading acceptance meeting\nTranscribed by Otter.ai\n\n",
        );
        for (index, text) in texts.iter().enumerate() {
            let speaker = if matches!(index, 1 | 126) {
                "Casey"
            } else {
                "Jordan"
            };
            otter.push_str(&format!(
                "{speaker}  {}:{:02}:00\n{text}\n\n",
                index / 60,
                index % 60
            ));
        }
        let export = output.join("synthetic-otter-export.txt");
        std::fs::write(&export, otter).expect("write synthetic export");
        let parsed = read_transcript_export(&export).expect("real transcript import parser");
        assert_eq!(parsed.segments.len(), texts.len());
        let session_id =
            crate::meeting::store::workflow_core_tests::reviewable_meeting(store, &parsed.title, 1);
        let imported_texts: Vec<&str> = parsed
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        crate::meeting::store::workflow_core_tests::transcript_segments(
            store,
            session_id,
            &imported_texts,
        );
        let segments = store
            .analytics_segments(session_id)
            .expect("real retained source segments");
        assert_eq!(segments.len(), texts.len());
        let mut plan = empty_local_plan();
        plan.session_id = session_id;
        plan.attempt_number = 2;
        crate::meeting::store::workflow_core_tests::processing_plan(store, &plan)
            .expect("real processing fixture plan");
        let track_id = store
            .review_snapshot(session_id)
            .expect("isolated import track")
            .tracks[0]
            .track_id;
        let revision = store
            .begin_transcript_revision(TranscriptRevisionInput {
                session_id,
                engine_id: "import",
                model_version: None,
                destination: &ProcessingDestination::Local,
                source_set: &[SourceKind::Microphone],
                language: "en",
            })
            .expect("real imported transcript revision");
        let spans = crate::meeting::import_formats::resolve_spans(&parsed.segments);
        let inputs: Vec<_> = parsed
            .segments
            .iter()
            .zip(spans)
            .map(|(segment, (start_ms, end_ms))| TranscriptSegmentInput {
                track_id,
                source_kind: SourceKind::Microphone,
                start_offset_ns: start_ms * 1_000_000,
                end_offset_ns: end_ms * 1_000_000,
                text: segment.text.clone(),
                confidence_milli: None,
                speaker: segment.speaker.clone(),
            })
            .collect();
        store
            .append_transcript_segments(session_id, revision, &inputs)
            .expect("append through immutable production transcript storage");
        store
            .complete_transcript_revision(session_id, revision)
            .expect("complete real imported transcript");
        let segments = store
            .analytics_segments(session_id)
            .expect("retained imported timeline");
        let source = serde_json::json!({"session_id":session_id,"source_bytes":transcript_bytes,"source_segments":segments.len(),"source":segments.iter().map(|segment|serde_json::json!({"segment_id":segment.segment_id,"speaker_id":segment.speaker_id,"start_offset_ns":segment.start_offset_ns,"end_offset_ns":segment.end_offset_ns,"text":segment.text})).collect::<Vec<_>>()});
        std::fs::write(
            output.join("source.json"),
            serde_json::to_vec_pretty(&source).expect("synthetic source JSON"),
        )
        .expect("write synthetic source receipt");
        println!(
            "synthetic_source_bytes={transcript_bytes} source_segments={} isolated_store={}",
            texts.len(),
            store.root().display()
        );
        SyntheticMeeting {
            session_id,
            expected_decisions: vec![
                (segments[0].segment_id, vec!["cobalt"]),
                (segments[64].segment_id, vec!["240"]),
                (segments[127].segment_id, vec!["november", "12"]),
            ],
            report_opening: segments[1].segment_id,
            report_revision: segments[126].segment_id,
        }
    }

    fn assert_actual_outcome(content: &GeneratedMeetingArtifacts, fixture: &SyntheticMeeting) {
        for (segment_id, words) in &fixture.expected_decisions {
            assert!(content.decisions.iter().any(|decision| {
            let text = decision.text.to_lowercase();
            words.iter().all(|word|text.contains(word)) && decision.citations.iter().any(|citation|citation.segment_id == *segment_id)
        }), "notes omitted an expected beginning/middle/end decision or its real citation: {segment_id:?} {words:?}");
        }
        assert_eq!(
            content.action_items.len(),
            1,
            "no task was agreed beyond Casey's report"
        );
        assert!(
            content.key_questions.is_empty(),
            "the source has no unresolved question"
        );
        assert!(content.risks.is_empty(), "the source introduces no risk");
        let report_actions: Vec<_> = content
            .action_items
            .iter()
            .filter(|action| action.text.text.to_lowercase().contains("report"))
            .collect();
        assert_eq!(
            report_actions.len(),
            1,
            "notes must reconcile the report into one commitment"
        );
        let report_action = report_actions[0];
        assert!(
            format!(
                "{} {}",
                report_action.text.text,
                report_action.due_text.as_deref().unwrap_or("")
            )
            .to_lowercase()
            .contains("monday"),
            "the notes must use the revised Monday deadline"
        );
        assert_eq!(
            report_action.owner_text.as_deref(),
            Some("Casey"),
            "notes must retain the actual person assigned the report"
        );
        let ledger = content
            .ledger
            .as_ref()
            .expect("actual complete ledger, not an unavailable ledger tab");
        assert!(
            matches!(ledger.receipts, LedgerReceiptState::Verified),
            "real-model acceptance requires verified receipts"
        );
        assert!(
            ledger.open_loops.is_empty(),
            "a meeting with no unresolved questions must not invent them: {:?}",
            ledger.open_loops
        );
        assert!(ledger.stances.iter().any(|stance| stance.from == "Jordan" && stance.what.to_lowercase().contains("cobalt") && stance.citations.iter().any(|citation| citation.segment_id == fixture.expected_decisions[0].0)), "the explicit adoption of the cobalt design must retain its position, actual speaker and original source");
        let private_excerpt_label =
            regex::Regex::new(r"\b[QDA][1-9][0-9]*\b").expect("private label guard");
        assert!(
            ledger
                .caveats
                .iter()
                .all(|caveat| !private_excerpt_label.is_match(caveat)),
            "internal fact and quote labels must not appear in user-facing caveats"
        );
        let mut independent_decision_rows = HashSet::new();
        for (segment_id, words) in &fixture.expected_decisions {
            let index = ledger.threads.iter().position(|thread| {
            let quote = thread.receipt.quote.to_lowercase();
            matches!(thread.state, LedgerThreadState::Decided | LedgerThreadState::Agreed) && thread.substantive && words.iter().all(|word|quote.contains(word)) && thread.receipt.citations.iter().any(|citation|citation.segment_id == *segment_id)
        }).unwrap_or_else(|| panic!("ledger omitted a beginning/middle/end decision or its real citation: {segment_id:?}"));
            assert!(independent_decision_rows.insert(index), "independent choices must remain independently reviewable, not merged by sharing the same state");
            assert!(
                ledger.threads[index].owner.is_none(),
                "speaking or agreeing does not assign ownership of a choice"
            );
        }
        assert_eq!(
            ledger.commitments.len(),
            1,
            "settled choices do not create extra personal promises"
        );
        let report_commitments: Vec<_> = ledger
            .commitments
            .iter()
            .filter(|commitment| commitment.what.to_lowercase().contains("report"))
            .collect();
        assert_eq!(
            report_commitments.len(),
            1,
            "ledger must reconcile the returning report commitment"
        );
        assert!(
            report_commitments[0].what.to_lowercase().contains("monday"),
            "ledger must use the revised Monday deadline"
        );
        assert_eq!(
            report_commitments[0].who, "Casey",
            "confirming someone else's responsibility does not transfer it"
        );
        assert_eq!(
            report_commitments[0].receipt.speaker.as_deref(),
            Some("Casey"),
            "the receipt speaker is the original turn's speaker"
        );
        let report_source_ids: HashSet<_> = report_commitments[0]
            .receipt
            .citations
            .iter()
            .map(|citation| citation.segment_id)
            .collect();
        assert!(report_source_ids.contains(&fixture.report_opening) && report_source_ids.contains(&fixture.report_revision), "the commitment itself must retain the first promise and its revision, not borrow another topic's receipts");
        assert!(ledger.threads.iter().filter(|thread| thread.topic.to_lowercase().contains("dashboard")).all(|thread| thread.owner.is_none() && !thread.receipt.quote.to_lowercase().contains("report")), "a report assignment must not assign ownership of dashboard discussion or contaminate its receipt");
        assert!(
            ledger.threads.iter().any(|thread| {
                let ids: HashSet<_> = thread
                    .receipt
                    .citations
                    .iter()
                    .map(|citation| citation.segment_id)
                    .collect();
                ids.contains(&fixture.report_opening) && ids.contains(&fixture.report_revision)
            }),
            "returning report thread must retain the original and revised citations"
        );
    }

    #[test]
    fn actual_apple_long_meeting_pipeline_and_regeneration() {
        assert!(
            AppleIntelligenceGenerator.is_available(),
            "actual Apple Intelligence is required; blocker={:?}",
            AppleIntelligenceGenerator::blocker()
        );
        let output =
            std::path::PathBuf::from(SCRATCH).join(format!("run-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&output).expect("create owned synthetic smoke directory");
        println!("actual_apple_smoke_output={}", output.display());
        let (_directory, store) = crate::meeting::store::workflow_core_tests::store();
        let fixture = seed_synthetic_meeting(&store, &output, false);
        let service = Arc::new(MeetingProcessingService::new(None));
        let generator = Arc::new(ActualAppleWitness {
            output: output.clone(),
            calls: AtomicUsize::new(0),
        });
        service.set_text_generators(generator.clone(), generator.clone());
        service.submit(
            Arc::clone(&store),
            fixture.session_id,
            ProcessingOrigin::ImportedTranscript,
        );
        service.wait_for_job(fixture.session_id);
        let review = store
            .review_snapshot(fixture.session_id)
            .expect("real encrypted review");
        std::fs::write(
            output.join("pipeline-review.json"),
            serde_json::to_vec_pretty(&review).expect("review JSON"),
        )
        .expect("write actual pipeline receipt");
        println!(
            "actual_pipeline_review={}",
            serde_json::to_string_pretty(&review).expect("print actual review")
        );
        assert_eq!(review.session.phase, MeetingPhase::ReviewReady);
        assert_eq!(
            review.session.processing_status,
            ProcessingStatus::Succeeded
        );
        let artifact = review
            .artifacts
            .iter()
            .find(|artifact| artifact.state == MeetingArtifactState::Current)
            .expect("actual current notes");
        assert_actual_outcome(
            artifact.content.as_ref().expect("actual generated content"),
            &fixture,
        );
        let operation_id = MeetingOperationId::new();
        let (rewritten, engine, refresh) = service
            .regenerate(
                &store,
                fixture.session_id,
                review.session.revision,
                operation_id,
            )
            .expect("actual manual regeneration");
        std::fs::write(
            output.join("regenerated-artifact.json"),
            serde_json::to_vec_pretty(&rewritten).expect("rewritten JSON"),
        )
        .expect("write actual regeneration receipt");
        println!(
            "actual_regenerated_artifact={}",
            serde_json::to_string_pretty(&rewritten).expect("print actual regenerated notes")
        );
        assert_eq!(engine, AppleIntelligenceGenerator.model_id());
        assert_ne!(rewritten.artifact_id, artifact.artifact_id);
        assert_actual_outcome(
            rewritten
                .content
                .as_ref()
                .expect("actual rewritten content"),
            &fixture,
        );
        let receipt = store
            .record_artifact_regeneration(
                operation_id,
                utc_now_ms(),
                fixture.session_id,
                review.session.revision,
                rewritten.artifact_id,
                engine,
            )
            .expect("real regeneration receipt");
        assert_eq!(receipt.result, OperationResult::Committed);
        std::fs::write(
            output.join("regeneration-receipt.json"),
            serde_json::to_vec_pretty(&receipt).expect("receipt JSON"),
        )
        .expect("write real regeneration receipt");
        if let Some(refresh) = refresh {
            service.start_relationship_refresh(Arc::clone(&store), fixture.session_id, refresh);
        }
        service.wait_for_job(fixture.session_id);
        assert!(
        actual_sdk_started() <= MAX_ACTUAL_CALLS,
        "initial notes and regeneration must stay within the original actual SDK request budget"
    );
        println!("actual_apple_smoke_passed logical_generator_requests={} actual_sdk_requests={} source_bytes_over_96_kib=true",generator.calls.load(Ordering::SeqCst),actual_sdk_started());
    }

    #[test]
    #[ignore]
    fn actual_apple_named_assignment_is_not_the_speaker() {
        let first = uuid::Uuid::new_v4().to_string();
        let last = uuid::Uuid::new_v4().to_string();
        let input = serde_json::json!({"transcript":[
        {"segment_uuid":first,"speaker":"Jordan","text":"We decided to use the cobalt design for the homepage. Casey owns implementing this design."},
        {"segment_uuid":last,"speaker":"Casey","text":"I will implement the cobalt design by Friday."}
    ]}).to_string();
        let output = Path::new(SCRATCH).join(format!("named-assignment-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&output).expect("owned synthetic receipts");
        println!("actual_named_assignment_output={}", output.display());
        let witness = ActualAppleWitness {
            output,
            calls: AtomicUsize::new(0),
        };
        let response = witness
            .generate(
                &ledger_system_prompt(),
                &input,
                ARTIFACT_MAX_TOKENS,
                ReplyShape::MeetingLedger,
            )
            .expect("actual native ledger");
        let ledger: RawLedgerOutput = first_json_value(&response).expect("canonical ledger");
        let choice = ledger
            .threads
            .iter()
            .find(|thread| {
                thread.state == "decided" && thread.topic.to_lowercase().contains("cobalt")
            })
            .expect("source decision");
        assert_eq!(
            choice.owner.as_deref(),
            Some("Casey"),
            "the explicitly assigned person is not the announcing speaker"
        );
        assert!(
            choice.receipt.citations.contains(&first),
            "the decision cites its actual source"
        );
        assert_ne!(
            choice.receipt.speaker.as_deref(),
            Some("Casey"),
            "assignment must not rewrite who spoke the decision"
        );
        assert_eq!(
            ledger.commitments.len(),
            1,
            "assignment and confirmation are one promised job"
        );
        assert_eq!(ledger.commitments[0].who, "Casey");
        assert!(ledger.commitments[0].what.to_lowercase().contains("friday"));
        assert_eq!(actual_sdk_started(), 2);
        println!("actual_named_assignment_passed");
    }

    /// How a forced request fails: as the safety filter stops it, or with a
    /// reply that is not the notes asked for.
    #[derive(Clone, Copy, Debug)]
    enum Forced {
        Blocked,
        Garbled,
    }

    /// Apple Intelligence as the app calls it, except that a request of one
    /// of `shapes` whose input holds `marker` (every one when `marker` is
    /// empty) fails as `forced` says.
    struct ForcedFailure {
        marker: &'static str,
        shapes: &'static [ReplyShape],
        forced: Forced,
        forced_requests: AtomicUsize,
        requests: AtomicUsize,
    }

    impl MeetingTextGenerator for ForcedFailure {
        fn is_available(&self) -> bool {
            AppleIntelligenceGenerator.is_available()
        }
        fn model_id(&self) -> &'static str {
            AppleIntelligenceGenerator.model_id()
        }
        fn model_version(&self) -> Cow<'static, str> {
            AppleIntelligenceGenerator.model_version()
        }
        fn max_input_bytes(&self) -> usize {
            AppleIntelligenceGenerator.max_input_bytes()
        }
        fn context_window_bytes(&self) -> Option<usize> {
            AppleIntelligenceGenerator.context_window_bytes()
        }
        fn reserved_output_tokens(&self, requested: i32) -> usize {
            AppleIntelligenceGenerator.reserved_output_tokens(requested)
        }
        fn generate(
            &self,
            prompt: &str,
            input: &str,
            max_tokens: i32,
            shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            let index = self.requests.fetch_add(1, Ordering::SeqCst);
            assert!(
                index < 4 * MAX_ACTUAL_CALLS,
                "forced-failure run exhausted its request budget"
            );
            if self.shapes.contains(&shape) && input.contains(self.marker) {
                self.forced_requests.fetch_add(1, Ordering::SeqCst);
                println!(
                    "forced_failure request={index} shape={shape:?} forced={:?}",
                    self.forced
                );
                return match self.forced {
                    Forced::Blocked => Err(MeetingTextGenerationError::SafetyBlocked),
                    Forced::Garbled => Ok("{\"summary\": [{\"text\": \"".to_string()),
                };
            }
            AppleIntelligenceGenerator.generate(prompt, input, max_tokens, shape)
        }
    }

    /// The synthetic meeting read and written again through the app's own
    /// import and "Write the notes again" paths with `generator`, returning
    /// the notes each of the `rewrites` presses left current.
    fn notes_through_app_paths(
        generator: Arc<ForcedFailure>,
        rewrites: usize,
    ) -> Vec<GeneratedMeetingArtifacts> {
        assert!(
            AppleIntelligenceGenerator.is_available(),
            "actual Apple Intelligence is required; blocker={:?}",
            AppleIntelligenceGenerator::blocker()
        );
        let output =
            std::path::PathBuf::from(SCRATCH).join(format!("forced-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&output).expect("create owned synthetic directory");
        let (_directory, store) = crate::meeting::store::workflow_core_tests::store();
        let fixture = seed_synthetic_meeting(&store, &output, true);
        let service = Arc::new(MeetingProcessingService::new(None));
        service.set_text_generators(generator.clone(), generator.clone());
        service.submit(
            Arc::clone(&store),
            fixture.session_id,
            ProcessingOrigin::ImportedTranscript,
        );
        service.wait_for_job(fixture.session_id);
        let mut written = Vec::new();
        for press in 1..=rewrites {
            let review = store
                .review_snapshot(fixture.session_id)
                .expect("synthetic review");
            let (rewritten, _engine, _refresh) = service
                .regenerate(
                    &store,
                    fixture.session_id,
                    review.session.revision,
                    MeetingOperationId::new(),
                )
                .unwrap_or_else(|failure| {
                    panic!("press {press} of \"Write the notes again\" wrote no notes: {failure:?}")
                });
            let content = rewritten.content.expect("rewritten notes");
            println!(
                "forced_failure press={press} plain={} decisions={} left_out={:?} ledger_failure={:?}",
                content.plain,
                content.decisions.len(),
                content.left_out,
                content.ledger_failure
            );
            written.push(content);
        }
        written
    }

    const MARKED_TURN_NS: u64 = 64 * 60 * 1_000_000_000;

    fn assert_marked_turn_left_out(content: &GeneratedMeetingArtifacts) {
        assert!(
            !content.plain,
            "one failing part must not cost the structured notes"
        );
        assert!(
            content.left_out.iter().any(|span| {
                span.start_offset_ns
                    .is_some_and(|start| start <= MARKED_TURN_NS)
                    && span.end_offset_ns.is_some_and(|end| end > MARKED_TURN_NS)
            }),
            "the notes must say the stretch holding the failing turn was left out: {:?}",
            content.left_out
        );
        assert!(
            content.decisions.iter().any(|decision| {
                let text = decision.text.to_lowercase();
                text.contains("cobalt") || text.contains("november")
            }),
            "the notes keep what the readable parts decided: {:?}",
            content.decisions
        );
    }

    #[test]
    #[ignore = "runs Apple Intelligence over a synthetic meeting for minutes"]
    fn actual_apple_notes_leave_out_a_part_the_safety_filter_always_stops() {
        let generator = Arc::new(ForcedFailure {
            marker: "annual price at 240 dollars",
            shapes: &[
                ReplyShape::MeetingNotes,
                ReplyShape::PlainNotes,
                ReplyShape::MeetingLedger,
            ],
            forced: Forced::Blocked,
            forced_requests: AtomicUsize::new(0),
            requests: AtomicUsize::new(0),
        });
        for content in notes_through_app_paths(generator.clone(), 2) {
            assert_marked_turn_left_out(&content);
            assert!(content
                .left_out
                .iter()
                .all(|span| span.cause == EngineFailureCause::SafetyBlocked));
        }
        assert!(generator.forced_requests.load(Ordering::SeqCst) > 0);
    }

    #[test]
    #[ignore = "runs Apple Intelligence over a synthetic meeting for minutes"]
    fn actual_apple_notes_leave_out_a_part_whose_replies_never_parse() {
        let generator = Arc::new(ForcedFailure {
            marker: "annual price at 240 dollars",
            shapes: &[ReplyShape::MeetingNotes],
            forced: Forced::Garbled,
            forced_requests: AtomicUsize::new(0),
            requests: AtomicUsize::new(0),
        });
        for content in notes_through_app_paths(generator.clone(), 2) {
            assert_marked_turn_left_out(&content);
        }
        assert!(generator.forced_requests.load(Ordering::SeqCst) > 0);
    }

    #[test]
    #[ignore = "runs Apple Intelligence over a synthetic meeting for minutes"]
    fn actual_apple_structured_notes_that_never_parse_give_plain_notes() {
        let generator = Arc::new(ForcedFailure {
            marker: "",
            shapes: &[ReplyShape::MeetingNotes],
            forced: Forced::Garbled,
            forced_requests: AtomicUsize::new(0),
            requests: AtomicUsize::new(0),
        });
        for content in notes_through_app_paths(generator, 1) {
            assert!(
                content.plain,
                "failed structured notes fall back to plain notes"
            );
            assert!(!content.summary.text.trim().is_empty());
            assert!(content.ledger_failure.is_some());
        }
    }
}
