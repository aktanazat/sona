#[cfg(test)]
mod tests {
    use super::super::*;
    use serde_json::{json, Value};
    use std::sync::atomic::AtomicUsize;

    /// The reply to a request grouping an outline's topics: every topic is a
    /// subject of its own.
    fn no_topic_groups(pack: &str) -> Option<String> {
        pack.starts_with(r#"{"topics":"#)
            .then(|| r#"{"same_subject":[]}"#.to_owned())
    }

    struct ReplayGenerator {
        window: Option<usize>,
        wire: usize,
        reply: usize,
        calls: AtomicUsize,
    }

    impl MeetingTextGenerator for ReplayGenerator {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "complete-reading-replay"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            self.wire
        }
        fn context_window_bytes(&self) -> Option<usize> {
            self.window
        }
        fn reserved_output_tokens(&self, requested: i32) -> usize {
            self.reply
                .min(usize::try_from(requested).expect("positive output budget"))
        }
        fn generate(
            &self,
            prompt: &str,
            pack: &str,
            max_tokens: i32,
            _: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            if pack.starts_with("Decision: ") {
                return Ok(r#"{"settled": true}"#.to_owned());
            }
            if let Some(reply) = no_topic_groups(pack) {
                return Ok(reply);
            }
            assert!(pack.len() <= evidence_budget(self, prompt, max_tokens));
            self.calls.fetch_add(1, Ordering::SeqCst);
            let input: Value = serde_json::from_str(pack).expect("generation input JSON");
            if let Some(promises) = input.get("promises").and_then(Value::as_array) {
                let report: Vec<_> = promises
                    .iter()
                    .filter(|promise| promise["who"] == "Casey")
                    .map(|promise| promise["id"].clone())
                    .collect();
                let same_work = if report.len() > 1 {
                    vec![report]
                } else {
                    Vec::new()
                };
                return Ok(json!({
                    "headline":"Decisions and the report were discussed.",
                    "same_work":same_work,"cancelled":[],"answered":[]
                })
                .to_string());
            }
            let ledger = prompt.starts_with("Reconstruct this meeting");
            let mut values: Vec<(String, String, Vec<String>)> = Vec::new();
            let mut fallback = None;
            if let Some(transcript) = input["transcript"].as_array() {
                for turn in transcript {
                    let id = turn[if ledger { "segment_uuid" } else { "entity_id" }]
                        .as_str()
                        .expect("source id");
                    let text = turn["text"].as_str().expect("source text");
                    fallback.get_or_insert_with(|| {
                        (
                            "Discussion".to_string(),
                            text.to_string(),
                            vec![id.to_string()],
                        )
                    });
                    if text.starts_with("DECISION:")
                        || text.starts_with("Jordan says: We have decided")
                    {
                        values.push((text.to_string(), text.to_string(), vec![id.to_string()]));
                    }
                    if text.starts_with("Casey will send") || text.starts_with("Casey says:") {
                        values.push(("Report".to_string(), text.to_string(), vec![id.to_string()]));
                    }
                }
            } else {
                for fact in input["section_facts"].as_array().expect("section facts") {
                    let kind = fact["kind"].as_str().expect("fact kind");
                    let value = &fact["value"];
                    if ledger && kind == "thread" {
                        let quote = value["receipt"]["quote"].as_str().expect("receipt");
                        let ids = citation_ids(&value["receipt"]["citations"]);
                        let topic = value["topic"].as_str().expect("topic");
                        if topic == "Discussion" {
                            fallback = Some((topic.to_string(), quote.to_string(), ids));
                        } else {
                            values.push((topic.to_string(), quote.to_string(), ids));
                        }
                    }
                    if ledger && kind == "commitment" {
                        let quote = value["receipt"]["quote"]
                            .as_str()
                            .expect("commitment receipt");
                        values.push((
                            "Report".to_string(),
                            quote.to_string(),
                            citation_ids(&value["receipt"]["citations"]),
                        ));
                    }
                    if !ledger && (kind == "decision" || kind == "action_item") {
                        let cited = if kind == "action_item" {
                            &value["text"]
                        } else {
                            value
                        };
                        let text = cited["text"].as_str().expect("cited text");
                        values.push((
                            if kind == "action_item" {
                                "Report".to_string()
                            } else {
                                text.to_string()
                            },
                            text.to_string(),
                            citation_ids(&cited["citations"]),
                        ));
                    }
                    if !ledger
                        && (kind == "summary" || (kind == "follow_up_draft" && fallback.is_none()))
                    {
                        fallback = Some((
                            "Discussion".to_string(),
                            "Discussion.".to_string(),
                            citation_ids(&value["citations"]),
                        ));
                    }
                }
            }
            let mut reconciled: Vec<(String, String, Vec<String>)> = Vec::new();
            for (topic, text, ids) in values {
                if let Some(previous) = reconciled.iter_mut().find(|previous| previous.0 == topic) {
                    if ledger && topic == "Report" {
                        if !previous.1.contains(&text) {
                            previous.1 = format!("{} ... {}", previous.1, text);
                        }
                        for id in ids {
                            if !previous.2.contains(&id) {
                                previous.2.push(id);
                            }
                        }
                    } else {
                        previous.1 = text;
                        previous.2 = ids;
                    }
                } else {
                    reconciled.push((topic, text, ids));
                }
            }
            let fallback = fallback
                .or_else(|| reconciled.first().cloned())
                .unwrap_or_else(|| panic!("nonempty supplied evidence: {pack}"));
            if ledger {
                if reconciled.is_empty() {
                    reconciled.push(fallback);
                }
                let threads: Vec<Value> = reconciled
                    .iter()
                    .map(|(topic, quote, ids)| {
                        json!({
                            "topic":topic,"state":"decided","substantive":true,
                            "receipt":{"quote":quote,"speaker":null,"citations":ids},"owner":null
                        })
                    })
                    .collect();
                let commitments: Vec<Value> = reconciled.iter().filter(|value| value.0 == "Report").map(|(_, quote, ids)| json!({
                "who":"Casey","what":if quote.contains("Monday") {"Send report Monday"} else {"Send report Friday"},"firmness":"firm",
                "receipt":{"quote":quote,"speaker":"Casey","citations":ids}
            })).collect();
                Ok(json!({"headline":"Decisions and the report were discussed.","threads":threads,"open_loops":[],"commitments":commitments,"stances":[],"caveats":[]}).to_string())
            } else {
                let decisions: Vec<Value> = reconciled
                    .iter()
                    .filter(|value| value.0 != "Report")
                    .map(|(_, text, ids)| json!({"text":text,"citations":ids}))
                    .collect();
                let actions: Vec<Value> = reconciled.iter().filter(|value| value.0 == "Report").map(|(_, text, ids)| json!({"text":{"text":text,"citations":ids},"owner_text":"Casey","due_text":null})).collect();
                let summary = json!({"text":"Discussion.","citations":fallback.2});
                Ok(json!({"summary":[summary.clone()],"outline":[],"decisions":decisions,"action_items":actions,"key_questions":[],"risks":[],"follow_up_draft":summary}).to_string())
            }
        }
    }

    fn citation_ids(value: &Value) -> Vec<String> {
        value
            .as_array()
            .expect("citations")
            .iter()
            .map(|citation| {
                citation
                    .as_str()
                    .or_else(|| citation["segment_id"].as_str())
                    .expect("original transcript UUID")
                    .to_string()
            })
            .collect()
    }

    fn evidence() -> ArtifactEvidence {
        let session_id = MeetingSessionId::new();
        ArtifactEvidence {
            transcript: (0u64..160)
                .map(|index| MeetingEvidence {
                    citation: MeetingCitation {
                        kind: CitationKind::Transcript,
                        session_id,
                        entity_id: TranscriptSegmentId::from_uuid(uuid::Uuid::from_u128(
                            u128::from(index) + 1,
                        ))
                        .uuid()
                        .to_string(),
                        start_offset_ns: Some(index * 1_000_000_000),
                        end_offset_ns: Some((index + 1) * 1_000_000_000),
                    },
                    text: match index {
                        0 => "DECISION: use the blue design.".to_string(),
                        1 => "Casey will send the report Friday.".to_string(),
                        80 => "DECISION: keep the existing price.".to_string(),
                        158 => "Casey will send the report Monday instead of Friday.".to_string(),
                        159 => "DECISION: release the finished design.".to_string(),
                        _ => format!("Agenda item {index}. {}", "ordinary discussion ".repeat(65)),
                    },
                })
                .collect(),
            manual_notes: Vec::new(),
            user_notes: String::new(),
            marks: Vec::new(),
            template: crate::meeting::analytics::MeetingNotesTemplate::default().into(),
        }
    }

    fn complete_meeting_contract(generator: ReplayGenerator) {
        let evidence = evidence();
        assert!(
            evidence
                .transcript
                .iter()
                .map(|item| item.text.len())
                .sum::<usize>()
                > 96 * 1024
        );
        let prompt =
            artifact_system_prompt(&evidence.template, false, MeetingNotesLanguage::default());
        let notes = notes(&generator, &evidence, &HashMap::new(), &prompt, || Ok(()))
            .expect("complete notes");
        assert_eq!(notes.decisions.len(), 3);
        assert_eq!(
            notes.decisions[0].citations[0].segment_id.uuid(),
            uuid::Uuid::from_u128(1)
        );
        assert_eq!(
            notes.decisions[1].citations[0].segment_id.uuid(),
            uuid::Uuid::from_u128(81)
        );
        assert_eq!(
            notes.decisions[2].citations[0].segment_id.uuid(),
            uuid::Uuid::from_u128(160)
        );
        assert_eq!(notes.action_items.len(), 1);
        assert!(notes.action_items[0].text.text.contains("Monday"));
        let haystack =
            ledger::fold_haystack(evidence.transcript.iter().map(|item| item.text.as_str()));
        let ledger = ledger(&generator, &evidence, &HashMap::new(), &haystack, || Ok(()))
            .expect("complete ledger");
        assert_eq!(
            ledger
                .threads
                .iter()
                .filter(|thread| thread.topic.starts_with("DECISION:"))
                .count(),
            3
        );
        assert_eq!(ledger.commitments.len(), 1);
        assert_eq!(ledger.commitments[0].what, "Send report Monday");
        assert_eq!(ledger.commitments[0].receipt.citations.len(), 2);
        assert_eq!(ledger::unverified_receipts(&ledger, &haystack), 0);
    }

    #[test]
    fn long_meeting_notes_and_ledger_cover_all_sections_on_device() {
        complete_meeting_contract(ReplayGenerator {
            window: Some(12_288),
            wire: usize::MAX,
            reply: 1_024,
            calls: AtomicUsize::new(0),
        });
    }

    #[test]
    fn long_meeting_notes_and_ledger_cover_all_sections_on_local_endpoint() {
        complete_meeting_contract(ReplayGenerator {
            window: Some(24_576),
            wire: usize::MAX,
            reply: 3_200,
            calls: AtomicUsize::new(0),
        });
    }

    #[test]
    fn long_meeting_notes_and_ledger_cover_all_sections_over_relay() {
        complete_meeting_contract(ReplayGenerator {
            window: None,
            wire: 124 * 1024,
            reply: 3_200,
            calls: AtomicUsize::new(0),
        });
    }

    /// A local model as measured on a 30-minute meeting: its notes of a part
    /// run to about 0.4 of the part's tokens, and an answer that reaches its
    /// budget stops part-way through.
    struct MeasuredLocalModel {
        replay: ReplayGenerator,
        cut: AtomicUsize,
    }

    impl MeetingTextGenerator for MeasuredLocalModel {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "measured-local-model"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            usize::MAX
        }
        fn context_window_bytes(&self) -> Option<usize> {
            self.replay.window
        }
        fn generate(
            &self,
            prompt: &str,
            pack: &str,
            max_tokens: i32,
            shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            let reply = self.replay.generate(prompt, pack, max_tokens, shape)?;
            let needed = pack.len() / 3 * 2 / 5;
            if usize::try_from(max_tokens).is_ok_and(|budget| needed > budget) {
                self.cut.fetch_add(1, Ordering::SeqCst);
                return Ok("{\"summary\":[".to_string());
            }
            Ok(reply)
        }
    }

    #[test]
    fn a_local_model_reads_a_long_meeting_without_running_out_of_answer() {
        let generator = MeasuredLocalModel {
            replay: ReplayGenerator {
                window: Some(16_384 * 3),
                wire: usize::MAX,
                reply: usize::MAX,
                calls: AtomicUsize::new(0),
            },
            cut: AtomicUsize::new(0),
        };
        let evidence = evidence();
        let prompt =
            artifact_system_prompt(&evidence.template, false, MeetingNotesLanguage::default());
        let notes = notes(&generator, &evidence, &HashMap::new(), &prompt, || Ok(()))
            .expect("complete notes");
        assert!(notes.left_out.is_empty());
        let haystack =
            ledger::fold_haystack(evidence.transcript.iter().map(|item| item.text.as_str()));
        ledger(&generator, &evidence, &HashMap::new(), &haystack, || Ok(()))
            .expect("complete ledger");
        assert_eq!(generator.cut.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_single_oversized_unicode_turn_is_read_without_losing_bytes_or_citation() {
        let mut evidence = evidence();
        evidence.transcript.truncate(1);
        evidence.transcript[0].text = "会話 quoted \"speech\"\n".repeat(2_000);
        let scope = SourceScope::new(&evidence.transcript).expect("original unicode source");
        let packs =
            notes_packs(&evidence, &scope, &HashMap::new(), 700).expect("sectioned unicode turn");
        let mut recovered = String::new();
        for pack in packs {
            assert!(pack.as_ref().len() <= 700);
            let value: Value = serde_json::from_str(pack.as_ref()).expect("bounded pack JSON");
            for item in value["transcript"].as_array().expect("transcript") {
                assert_eq!(item["entity_id"], evidence.transcript[0].citation.entity_id);
                recovered.push_str(item["text"].as_str().expect("turn text"));
            }
        }
        assert_eq!(recovered, evidence.transcript[0].text);
    }

    #[test]
    fn cancelled_partial_reading_never_returns_whole_meeting_notes() {
        let evidence = evidence();
        let generator = ReplayGenerator {
            window: Some(12_288),
            wire: usize::MAX,
            reply: 1_024,
            calls: AtomicUsize::new(0),
        };
        let checks = AtomicUsize::new(0);
        let result = notes(
            &generator,
            &evidence,
            &HashMap::new(),
            &artifact_system_prompt(&evidence.template, false, MeetingNotesLanguage::default()),
            || {
                if checks.fetch_add(1, Ordering::SeqCst) >= 2 {
                    Err(ProcessingFailure::Cancelled.into())
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(
            result,
            Err(ReadingError::Stopped(RunFailure::Reason(
                ProcessingFailure::Cancelled
            )))
        ));
        assert_eq!(generator.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn complete_evidence_reader_keeps_the_end_of_a_large_retained_turn() {
        let (_directory, store) = crate::meeting::store::workflow_core_tests::store();
        let session_id =
            crate::meeting::store::workflow_core_tests::meeting(&store, "Long meeting", 1);
        let text = format!("{}final decision at the end", "discussion ".repeat(12_000));
        crate::meeting::store::workflow_core_tests::transcript(&store, session_id, &text);
        let evidence = store
            .artifact_evidence(session_id, usize::MAX, Default::default())
            .expect("all retained evidence");
        assert_eq!(
            evidence
                .transcript
                .iter()
                .map(|item| item.text.as_str())
                .collect::<String>(),
            text
        );
    }

    struct HeldRelationshipGenerator {
        replay: ReplayGenerator,
        started: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl MeetingTextGenerator for HeldRelationshipGenerator {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "held-relationship-replay"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            usize::MAX
        }
        fn generate(
            &self,
            prompt: &str,
            pack: &str,
            max_tokens: i32,
            shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            if matches!(shape, ReplyShape::Prose) {
                self.started.send(()).expect("optional operation started");
                self.release
                    .lock()
                    .expect("release lock")
                    .recv_timeout(Duration::from_secs(10))
                    .expect("release optional operation");
                Ok("Casey discussed the design.".to_string())
            } else {
                self.replay.generate(prompt, pack, max_tokens, shape)
            }
        }
    }

    fn held_relationship_fixture() -> (
        tempfile::TempDir,
        Arc<MeetingStore>,
        MeetingSessionId,
        PersonId,
        Arc<MeetingProcessingService>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (directory, store) = crate::meeting::store::workflow_core_tests::store();
        let session_id =
            crate::meeting::store::workflow_core_tests::reviewable_meeting(&store, "Design", 1);
        crate::meeting::store::workflow_core_tests::transcript(
            &store,
            session_id,
            "DECISION: use the blue design.",
        );
        let person_id =
            crate::meeting::store::workflow_core_tests::person(&store, "Casey", &[], &[]);
        crate::meeting::store::workflow_core_tests::link(
            &store,
            session_id,
            person_id,
            "manual",
            "confirmed",
        );
        let mut plan = empty_local_plan();
        plan.session_id = session_id;
        plan.attempt_number = 2;
        crate::meeting::store::workflow_core_tests::processing_plan(&store, &plan)
            .expect("processing fixture plan");
        let service = Arc::new(MeetingProcessingService::new(None));
        let (started_sender, started) = std::sync::mpsc::channel();
        let (release, release_receiver) = std::sync::mpsc::channel();
        let generator = Arc::new(HeldRelationshipGenerator {
            replay: ReplayGenerator {
                window: None,
                wire: usize::MAX,
                reply: 3_200,
                calls: AtomicUsize::new(0),
            },
            started: started_sender,
            release: Mutex::new(release_receiver),
        });
        service.set_text_generators(generator.clone(), generator);
        (
            directory, store, session_id, person_id, service, started, release,
        )
    }

    #[test]
    fn notes_are_ready_while_optional_relationship_description_is_held() {
        let (_directory, store, session_id, person_id, service, started, release) =
            held_relationship_fixture();
        let worker = {
            let service = Arc::clone(&service);
            let store = Arc::clone(&store);
            thread::spawn(move || {
                service.submit(store, session_id, ProcessingOrigin::ImportedTranscript)
            })
        };
        started
            .recv_timeout(Duration::from_secs(10))
            .expect("held optional description");
        let snapshot = store.session_snapshot(session_id).expect("ready snapshot");
        assert_eq!(snapshot.phase, MeetingPhase::ReviewReady);
        assert_eq!(snapshot.processing_status, ProcessingStatus::Succeeded);
        assert!(store
            .review_snapshot(session_id)
            .expect("ready review")
            .artifacts
            .iter()
            .any(|artifact| artifact.state == MeetingArtifactState::Current));
        assert!(store
            .person_detail(person_id)
            .expect("person before optional description")
            .detail
            .person
            .summary
            .is_none());
        assert!(service
            .jobs
            .lock()
            .expect("owned optional job")
            .contains_key(&session_id));
        service.cancel(session_id);
        release.send(()).expect("release optional description");
        worker.join().expect("processing worker");
        service.wait_for_job(session_id);
        assert!(store
            .person_detail(person_id)
            .expect("person after cancelled description")
            .detail
            .person
            .summary
            .is_none());
        assert_eq!(
            store
                .session_snapshot(session_id)
                .expect("still ready")
                .processing_status,
            ProcessingStatus::Succeeded
        );
    }

    #[test]
    fn a_removed_meeting_cannot_publish_a_held_relationship_description() {
        let (_directory, store, session_id, person_id, service, started, release) =
            held_relationship_fixture();
        let worker = {
            let service = Arc::clone(&service);
            let store = Arc::clone(&store);
            thread::spawn(move || {
                service.submit(store, session_id, ProcessingOrigin::ImportedTranscript)
            })
        };
        started
            .recv_timeout(Duration::from_secs(10))
            .expect("held optional description");
        let revision = store
            .session_snapshot(session_id)
            .expect("ready deletion snapshot")
            .revision;
        let (receipt, deletion) = store
            .reserve_deletion(
                MeetingOperationId::new(),
                utc_now_ms(),
                session_id,
                revision,
                DeletionCause::User,
            )
            .expect("reserve real meeting deletion");
        assert_eq!(receipt.result, OperationResult::Committed);
        store
            .finish_deletion(deletion)
            .expect("finish real meeting deletion");
        release.send(()).expect("release optional description");
        worker.join().expect("processing worker");
        service.wait_for_job(session_id);
        assert!(store
            .person_detail(person_id)
            .expect("person remains")
            .detail
            .person
            .summary
            .is_none());
    }

    #[test]
    fn tail_edits_change_the_complete_reading_cache_digest() {
        let mut evidence = evidence();
        let before = evidence_digest(&evidence).expect("full evidence digest");
        evidence
            .transcript
            .last_mut()
            .expect("last turn")
            .text
            .push_str(" revised");
        assert_ne!(
            before,
            evidence_digest(&evidence).expect("changed evidence digest")
        );
    }

    struct RefusingSecondSection(ReplayGenerator);

    impl MeetingTextGenerator for RefusingSecondSection {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "refusing-section-replay"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            6_000
        }
        fn generate(
            &self,
            prompt: &str,
            pack: &str,
            max_tokens: i32,
            shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            if self.0.calls.load(Ordering::SeqCst) > 0 {
                Err(MeetingTextGenerationError::TimedOut)
            } else {
                self.0.generate(prompt, pack, max_tokens, shape)
            }
        }
    }

    #[test]
    fn a_part_that_fails_every_try_is_left_out_and_named_on_the_notes() {
        let evidence = evidence();
        let generator = RefusingSecondSection(ReplayGenerator {
            window: None,
            wire: 6_000,
            reply: 3_200,
            calls: AtomicUsize::new(0),
        });
        let content = notes(
            &generator,
            &evidence,
            &HashMap::new(),
            &artifact_system_prompt(&evidence.template, false, MeetingNotesLanguage::default()),
            || Ok(()),
        )
        .expect("notes from the part that could be read");
        assert!(!content.summary.text.is_empty());
        let meeting_end = evidence
            .transcript
            .iter()
            .filter_map(|line| line.citation.end_offset_ns)
            .max();
        let meeting_start = evidence
            .transcript
            .iter()
            .filter_map(|line| line.citation.start_offset_ns)
            .min();
        assert_eq!(content.left_out.len(), 1, "{:?}", content.left_out);
        let span = content.left_out[0];
        assert_eq!(span.cause, EngineFailureCause::TimedOut);
        assert_eq!(span.end_offset_ns, meeting_end);
        assert!(span.start_offset_ns > meeting_start);
    }

    #[test]
    fn cited_summary_lines_can_be_reconciled_without_an_oversized_indivisible_block() {
        let evidence = evidence();
        let scope = SourceScope::new(&evidence.transcript).expect("original source identities");
        let raw = RawArtifactOutput {
            summary: (0..12)
                .map(|index| RawCitedText {
                    text: format!(
                        "Topic {}: {}.",
                        index + 1,
                        "documented evidence ".repeat(22)
                    ),
                    citations: vec![evidence.transcript[index].citation.entity_id.clone()],
                })
                .collect(),
            outline: Vec::new(),
            decisions: Vec::new(),
            action_items: Vec::new(),
            key_questions: Vec::new(),
            risks: Vec::new(),
            follow_up_draft: RawCitedText {
                text: "No follow-up was agreed.".to_string(),
                citations: vec![evidence.transcript[0].citation.entity_id.clone()],
            },
        };
        let section = validate_artifact_output(
            &raw,
            &evidence.transcript,
            &HashMap::new(),
            &evidence.template,
        )
        .expect("valid independently cited summary lines");
        let rows = notes_facts(&raw.summary, &section, &scope).expect("validated notes facts");
        let packed = packs(rows, "{\"section_facts\":[", "]}", 2_000, Fill::Even)
            .expect("individual summary lines fit even when their joined display block does not");
        let mut cited_sources = Vec::new();
        for pack in packed {
            assert!(pack.as_ref().len() <= 2_000);
            let input: Value = serde_json::from_str(pack.as_ref()).expect("bounded evidence JSON");
            for fact in input["section_facts"].as_array().expect("section facts") {
                if fact["kind"] == "summary" {
                    cited_sources.extend(citation_ids(&fact["value"]["citations"]));
                }
            }
        }
        scope
            .restore_ids(&mut cited_sources)
            .expect("original canonical citations");
        assert_eq!(
            cited_sources,
            evidence.transcript[..12]
                .iter()
                .map(|turn| turn.citation.entity_id.clone())
                .collect::<Vec<_>>()
        );
    }

    struct GradualReduction {
        condense: bool,
        calls: AtomicUsize,
    }

    impl MeetingTextGenerator for GradualReduction {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "gradual-reduction"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            2_000
        }
        fn generate(
            &self,
            _: &str,
            pack: &str,
            _: i32,
            _: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            if let Some(reply) = no_topic_groups(pack) {
                return Ok(reply);
            }
            if !self.condense && self.calls.fetch_add(1, Ordering::SeqCst) >= 6 {
                return Err(MeetingTextGenerationError::TimedOut);
            }
            let input: Value = serde_json::from_str(pack).expect("bounded input");
            let mut ids = Vec::new();
            let mut longest = 0;
            let mut decisions = Vec::new();
            let mut action_items = Vec::new();
            if let Some(turns) = input["transcript"].as_array() {
                for turn in turns {
                    ids.push(turn["entity_id"].as_str().expect("source id").to_string());
                }
                let topic = turns[0]["text"]
                    .as_str()
                    .and_then(|text| text.split(':').next())
                    .expect("topic");
                decisions
                    .push(json!({"text": format!("Adopt the {topic} plan."), "citations": ids}));
                action_items.push(json!({
                    "text": {"text": format!("Email the {topic} figures."), "citations": ids},
                    "owner_text": null, "due_text": null
                }));
            } else {
                for fact in input["section_facts"].as_array().expect("section facts") {
                    let value = &fact["value"];
                    let cited = match fact["kind"].as_str().expect("fact kind") {
                        "decision" => {
                            decisions.push(value.clone());
                            value
                        }
                        "action_item" => {
                            action_items.push(value.clone());
                            &value["text"]
                        }
                        _ => value,
                    };
                    longest = longest.max(cited["text"].as_str().expect("cited text").len());
                    for id in citation_ids(&cited["citations"]) {
                        if !ids.contains(&id) {
                            ids.push(id);
                        }
                    }
                }
            }
            let width = if input["transcript"].is_array() || !self.condense {
                800
            } else if longest > 700 {
                500
            } else {
                100
            };
            let cited = json!({"text": "reviewed ".repeat(width / 9), "citations": ids});
            Ok(json!({
                "summary": [cited.clone()], "outline": [], "decisions": decisions,
                "action_items": action_items, "key_questions": [], "risks": [],
                "follow_up_draft": cited
            })
            .to_string())
        }
    }

    fn gradual_evidence(turns: usize) -> ArtifactEvidence {
        let mut evidence = evidence();
        evidence.transcript.truncate(turns);
        for (index, item) in evidence.transcript.iter_mut().enumerate() {
            item.text = format!(
                "Topic {index}: {}",
                "The team reviewed the current dashboard. ".repeat(35)
            );
        }
        evidence
    }

    #[test]
    fn shrinking_notes_can_finish_when_consecutive_rounds_have_the_same_pack_count() {
        let evidence = gradual_evidence(3);
        let generator = GradualReduction {
            condense: true,
            calls: AtomicUsize::new(0),
        };
        let result = notes(
            &generator,
            &evidence,
            &HashMap::new(),
            "Read the meeting.",
            || Ok(()),
        )
        .expect("smaller facts keep making progress even within the same pack count");
        assert_eq!(
            result
                .summary
                .citations
                .iter()
                .map(|citation| citation.segment_id.uuid().to_string())
                .collect::<Vec<_>>(),
            evidence
                .transcript
                .iter()
                .map(|item| item.citation.entity_id.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn nonshrinking_notes_stand_as_written_before_another_model_round() {
        let evidence = gradual_evidence(3);
        let generator = GradualReduction {
            condense: false,
            calls: AtomicUsize::new(0),
        };
        let content = notes(
            &generator,
            &evidence,
            &HashMap::new(),
            "Read the meeting.",
            || Ok(()),
        )
        .expect("sections no join could shorten stand as structured notes");
        assert_eq!(generator.calls.load(Ordering::SeqCst), 6);
        let ids = |citations: &[ArtifactCitation]| {
            citations
                .iter()
                .map(|citation| citation.segment_id.uuid().to_string())
                .collect::<Vec<_>>()
        };
        let expected: Vec<_> = evidence
            .transcript
            .iter()
            .map(|item| item.citation.entity_id.clone())
            .collect();
        assert!(!content.plain);
        assert_eq!(ids(&content.summary.citations), expected);
        assert_eq!(
            content
                .decisions
                .iter()
                .flat_map(|decision| ids(&decision.citations))
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            content
                .action_items
                .iter()
                .flat_map(|item| ids(&item.text.citations))
                .collect::<Vec<_>>(),
            expected
        );
    }

    /// Reads every part of the meeting, counts each reply that joins
    /// sections, and runs out of room on every one of them.
    struct JoinRunsOut(AtomicUsize);

    impl MeetingTextGenerator for JoinRunsOut {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "join-runs-out"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            2_000
        }
        fn generate(
            &self,
            _: &str,
            pack: &str,
            _: i32,
            _: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            if pack.starts_with("{\"section_facts\"") {
                self.0.fetch_add(1, Ordering::SeqCst);
                return Err(MeetingTextGenerationError::ReplyNotStructured);
            }
            Ok(reply_citing(pack))
        }
    }

    #[test]
    fn two_sections_whose_join_runs_out_stand_as_written_after_one_try() {
        let evidence = gradual_evidence(2);
        let generator = JoinRunsOut(AtomicUsize::new(0));
        let content = notes(
            &generator,
            &evidence,
            &HashMap::new(),
            "Read the meeting.",
            || Ok(()),
        )
        .expect("two sections no join could hold stand as written");
        assert_eq!(
            generator.0.load(Ordering::SeqCst),
            1,
            "the join is tried once and not again in halves"
        );
        assert_eq!(cited_ids(&content), source_ids(&evidence));
    }

    /// A local model as measured on three 30-minute meetings: it notes every
    /// decision it reads, joins sections by restating their facts, and
    /// writes about four bytes a token, so a reply longer than its budget
    /// stops part-way through. Counts the joins it is asked and the replies
    /// cut off.
    struct RestatesFacts {
        joins: AtomicUsize,
        cut: AtomicUsize,
    }

    impl MeetingTextGenerator for RestatesFacts {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "restates-facts"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            usize::MAX
        }
        fn context_window_bytes(&self) -> Option<usize> {
            Some(8_192 * 3)
        }
        fn generate(
            &self,
            _: &str,
            pack: &str,
            max_tokens: i32,
            _: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            if let Some(reply) = no_topic_groups(pack) {
                return Ok(reply);
            }
            let input: Value = serde_json::from_str(pack).expect("bounded input");
            let decisions: Vec<Value> = match input["transcript"].as_array() {
                Some(turns) => turns
                    .iter()
                    .map(|turn| {
                        let text = turn["text"].as_str().expect("turn text");
                        let decision = text.split_inclusive('.').next().unwrap_or(text);
                        json!({"text": decision, "citations": [turn["entity_id"]]})
                    })
                    .collect(),
                None => {
                    self.joins.fetch_add(1, Ordering::SeqCst);
                    input["section_facts"]
                        .as_array()
                        .expect("section facts")
                        .iter()
                        .filter(|fact| fact["kind"] == "decision")
                        .map(|fact| fact["value"].clone())
                        .collect()
                }
            };
            let mut reply: Value = serde_json::from_str(&reply_citing(pack)).expect("notes reply");
            reply["decisions"] = decisions.into();
            let reply = reply.to_string();
            if reply.len() / 4 > usize::try_from(max_tokens).expect("positive reply budget") {
                self.cut.fetch_add(1, Ordering::SeqCst);
                return Ok("{\"summary\":[".to_string());
            }
            Ok(reply)
        }
    }

    /// A meeting of `turns` turns, each settling one item with `follow_ups`
    /// clauses before talking it through, and the decisions it settles,
    /// sorted.
    fn decisive_meeting(turns: usize, follow_ups: usize) -> (ArtifactEvidence, Vec<String>) {
        let mut evidence = evidence();
        evidence.transcript.truncate(turns);
        let mut decisions = Vec::with_capacity(turns);
        for (index, item) in evidence.transcript.iter_mut().enumerate() {
            let decision = format!(
                "Decision {index}: the team keeps the plan for item {index}{}.",
                " and its follow-up".repeat(follow_ups)
            );
            item.text = format!("{decision}{}", " The team talks it through.".repeat(22));
            decisions.push(decision);
        }
        decisions.sort();
        (evidence, decisions)
    }

    fn sorted_decisions(content: &GeneratedMeetingArtifacts) -> Vec<String> {
        let mut decisions: Vec<String> = content
            .decisions
            .iter()
            .map(|decision| decision.text.clone())
            .collect();
        decisions.sort();
        decisions
    }

    #[test]
    fn a_join_longer_than_its_reply_is_read_in_halves_without_asking_it_whole() {
        let (evidence, settled) = decisive_meeting(30, 20);
        let generator = RestatesFacts {
            joins: AtomicUsize::new(0),
            cut: AtomicUsize::new(0),
        };
        let content = notes(
            &generator,
            &evidence,
            &HashMap::new(),
            "Read the meeting.",
            || Ok(()),
        )
        .expect("notes joined in halves");
        assert_eq!(generator.cut.load(Ordering::SeqCst), 0, "no reply runs out");
        assert_eq!(generator.joins.load(Ordering::SeqCst), 2, "one join a half");
        assert_eq!(sorted_decisions(&content), settled);
        assert_eq!(cited_ids(&content), source_ids(&evidence));
    }

    #[test]
    fn two_sections_longer_than_one_reply_stand_as_written_without_a_join() {
        // Two sections whose facts fit one join part, and two whose facts
        // fill two parts that would come back as two sections again.
        for (turns, follow_ups) in [(26, 20), (24, 30)] {
            let (evidence, settled) = decisive_meeting(turns, follow_ups);
            let generator = RestatesFacts {
                joins: AtomicUsize::new(0),
                cut: AtomicUsize::new(0),
            };
            let content = notes(
                &generator,
                &evidence,
                &HashMap::new(),
                "Read the meeting.",
                || Ok(()),
            )
            .expect("two sections stand as written");
            assert_eq!(
                generator.joins.load(Ordering::SeqCst),
                0,
                "no join is asked for {turns} turns"
            );
            assert_eq!(sorted_decisions(&content), settled);
            assert_eq!(cited_ids(&content), source_ids(&evidence));
        }
    }

    /// A notes reply citing every source its pack holds.
    fn reply_citing(pack: &str) -> String {
        if let Some(reply) = no_topic_groups(pack) {
            return reply;
        }
        let input: Value = serde_json::from_str(pack).expect("bounded input");
        let mut ids = Vec::new();
        if let Some(turns) = input["transcript"].as_array() {
            for turn in turns {
                ids.push(turn["entity_id"].as_str().expect("source id").to_string());
            }
        } else {
            for fact in input["section_facts"].as_array().expect("section facts") {
                for id in citation_ids(&fact["value"]["citations"]) {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
        }
        let cited = json!({"text": "The team reviewed the dashboard.", "citations": ids});
        json!({
            "summary": [cited.clone()], "outline": [], "decisions": [],
            "action_items": [], "key_questions": [], "risks": [],
            "follow_up_draft": cited
        })
        .to_string()
    }

    fn cited_ids(result: &GeneratedMeetingArtifacts) -> Vec<String> {
        result
            .summary
            .citations
            .iter()
            .map(|citation| citation.segment_id.uuid().to_string())
            .collect()
    }

    fn source_ids(evidence: &ArtifactEvidence) -> Vec<String> {
        evidence
            .transcript
            .iter()
            .map(|item| item.citation.entity_id.clone())
            .collect()
    }

    /// A ledger reply of one thread, quoting the first sentence of `turn`.
    fn ledger_reply_citing(turn: &Value) -> String {
        let text = turn["text"].as_str().expect("turn text");
        let quote = text.split_inclusive('.').next().unwrap_or(text);
        json!({
            "headline": "The meeting was read.",
            "threads": [{"topic": "Discussion", "state": "closed", "substantive": true, "owner": null,
                "receipt": {"quote": quote, "speaker": null, "citations": [turn["segment_uuid"]]}}],
            "open_loops": [], "commitments": [], "stances": [], "caveats": []
        })
        .to_string()
    }

    type DenseFailure = fn(ReplyShape) -> Result<String, MeetingTextGenerationError>;

    /// The ways a reply to a part too big for the model fails: cut off at
    /// its limit, or citing a line the part does not hold.
    fn dense_failures() -> [(&'static str, DenseFailure); 2] {
        [
            ("cut off", |_| {
                Err(MeetingTextGenerationError::ReplyNotStructured)
            }),
            ("rejected", |shape| {
                Ok(if matches!(shape, ReplyShape::MeetingLedger) {
                    ledger_reply_citing(&json!({"segment_uuid": "not-a-line", "text": "Agenda."}))
                } else {
                    reply_citing(r#"{"transcript":[{"entity_id":"not-a-line"}]}"#)
                })
            }),
        ]
    }

    /// Answers a part of one turn, and fails the reply to a part of more
    /// turns the way its `DenseFailure` says.
    struct FailsWhenDense(DenseFailure);

    impl MeetingTextGenerator for FailsWhenDense {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "fails-when-dense"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            6_000
        }
        fn generate(
            &self,
            _: &str,
            pack: &str,
            _: i32,
            shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            let input: Value = serde_json::from_str(pack).expect("bounded input");
            if input.get("part_headlines").is_some() {
                return Ok(json!({"headline": "The meeting was read."}).to_string());
            }
            match input["transcript"].as_array() {
                Some(turns) if turns.len() > 1 => (self.0)(shape),
                Some(turns) if matches!(shape, ReplyShape::MeetingLedger) => {
                    Ok(ledger_reply_citing(&turns[0]))
                }
                _ => Ok(reply_citing(pack)),
            }
        }
    }

    #[test]
    fn a_part_too_dense_for_one_reply_is_read_in_quarters() {
        let mut evidence = evidence();
        evidence.transcript.truncate(4);
        for (failure, dense) in dense_failures() {
            let result = notes(
                &FailsWhenDense(dense),
                &evidence,
                &HashMap::new(),
                "Read the meeting.",
                || Ok(()),
            )
            .unwrap_or_else(|error| panic!("{failure}: {error:?}"));
            assert_eq!(cited_ids(&result), source_ids(&evidence), "{failure}");
            assert!(result.left_out.is_empty(), "{failure}");
        }
    }

    #[test]
    fn a_part_out_of_time_is_read_in_halves_once() {
        let mut evidence = evidence();
        evidence.transcript.truncate(3);
        let result = notes(
            &FailsWhenDense(|_| Err(MeetingTextGenerationError::TimedOut)),
            &evidence,
            &HashMap::new(),
            "Read the meeting.",
            || Ok(()),
        );
        let left_out = match result {
            Ok(result) => result.left_out.len(),
            Err(error) => panic!("{error:?}"),
        };
        assert_eq!(
            left_out, 1,
            "the half that ran out of time is left out, not halved"
        );
    }

    #[test]
    fn a_ledger_part_too_dense_for_one_reply_is_read_in_quarters() {
        let mut evidence = evidence();
        evidence.transcript.truncate(4);
        let haystack =
            ledger::fold_haystack(evidence.transcript.iter().map(|turn| turn.text.as_str()));
        for (failure, dense) in dense_failures() {
            let ledger = ledger(
                &FailsWhenDense(dense),
                &evidence,
                &HashMap::new(),
                &haystack,
                || Ok(()),
            )
            .unwrap_or_else(|error| panic!("{failure}: {error:?}"));
            let cited: Vec<String> = ledger
                .threads
                .iter()
                .flat_map(|thread| &thread.receipt.citations)
                .map(|citation| citation.segment_id.uuid().to_string())
                .collect();
            assert_eq!(cited, source_ids(&evidence), "{failure}");
            assert!(ledger.caveats.is_empty(), "{failure}");
        }
    }

    /// Reads each part as a part too dense for one reply is read, and fails
    /// the reply that joins the parts.
    struct CannotJoinParts;

    impl MeetingTextGenerator for CannotJoinParts {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "cannot-join-parts"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            6_000
        }
        fn generate(
            &self,
            instructions: &str,
            pack: &str,
            max_tokens: i32,
            shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            let input: Value = serde_json::from_str(pack).expect("bounded input");
            if input.get("part_headlines").is_some() {
                return Err(MeetingTextGenerationError::TimedOut);
            }
            FailsWhenDense(|_| Err(MeetingTextGenerationError::ReplyNotStructured)).generate(
                instructions,
                pack,
                max_tokens,
                shape,
            )
        }
    }

    #[test]
    fn a_ledger_whose_parts_cannot_be_joined_keeps_every_part() {
        let mut evidence = evidence();
        evidence.transcript.truncate(4);
        let haystack =
            ledger::fold_haystack(evidence.transcript.iter().map(|turn| turn.text.as_str()));
        let ledger = ledger(
            &CannotJoinParts,
            &evidence,
            &HashMap::new(),
            &haystack,
            || Ok(()),
        )
        .unwrap_or_else(|error| panic!("{error:?}"));
        let cited: Vec<String> = ledger
            .threads
            .iter()
            .flat_map(|thread| &thread.receipt.citations)
            .map(|citation| citation.segment_id.uuid().to_string())
            .collect();
        assert_eq!(cited, source_ids(&evidence));
        assert_eq!(ledger.headline, "The meeting was read.");
        assert_eq!(ledger.caveats, [PARTS_NOT_JOINED]);
    }

    /// Answers each reading of a ledger part, in turn, with threads quoting
    /// the part's first lines and threads quoting words nobody said, counting
    /// the readings; readings past the list repeat its last, and a reading
    /// without threads fails.
    struct ReadsLedger {
        /// Threads said and threads invented, one pair per reading.
        readings: Vec<Option<(usize, usize)>>,
        read: AtomicUsize,
    }

    impl ReadsLedger {
        fn new(readings: Vec<Option<(usize, usize)>>) -> Self {
            Self {
                readings,
                read: AtomicUsize::new(0),
            }
        }
    }

    impl MeetingTextGenerator for ReadsLedger {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "reads-ledger"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            6_000
        }
        fn generate(
            &self,
            _: &str,
            pack: &str,
            _: i32,
            _: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            let reading = self.read.fetch_add(1, Ordering::SeqCst);
            let Some((said, invented)) = self.readings[reading.min(self.readings.len() - 1)] else {
                return Err(MeetingTextGenerationError::Failed);
            };
            let input: Value = serde_json::from_str(pack).expect("bounded input");
            let turns = input["transcript"].as_array().expect("transcript");
            let thread = |topic: String, quote: &str, turn: &Value| {
                json!({"topic": topic, "state": "closed", "substantive": true, "owner": null,
                    "receipt": {"quote": quote, "speaker": null,
                    "citations": [turn["segment_uuid"]]}})
            };
            let threads: Vec<Value> = turns
                .iter()
                .take(said)
                .enumerate()
                .map(|(index, turn)| {
                    let text = turn["text"].as_str().expect("turn text");
                    let quote = text.split_inclusive('.').next().unwrap_or(text);
                    thread(format!("Said {index}"), quote, turn)
                })
                .chain((0..invented).map(|index| {
                    thread(
                        format!("Invented {index}"),
                        "We doubled the budget.",
                        &turns[0],
                    )
                }))
                .collect();
            Ok(
                json!({"headline": "The meeting was read.", "threads": threads,
                "open_loops": [], "commitments": [], "stances": [], "caveats": []})
                .to_string(),
            )
        }
    }

    fn read_four_lines(generator: &ReadsLedger) -> MeetingLedger {
        let mut evidence = evidence();
        evidence.transcript.truncate(4);
        let haystack =
            ledger::fold_haystack(evidence.transcript.iter().map(|turn| turn.text.as_str()));
        ledger(generator, &evidence, &HashMap::new(), &haystack, || Ok(()))
            .unwrap_or_else(|error| panic!("{error:?}"))
    }

    fn topics(ledger: &MeetingLedger) -> Vec<&str> {
        ledger
            .threads
            .iter()
            .map(|thread| thread.topic.as_str())
            .collect()
    }

    #[test]
    fn a_ledger_part_with_one_invented_receipt_is_read_once() {
        let generator = ReadsLedger::new(vec![Some((2, 1))]);
        let ledger = read_four_lines(&generator);
        assert_eq!(generator.read.load(Ordering::SeqCst), 1);
        assert_eq!(topics(&ledger), ["Said 0", "Said 1"]);
        assert_eq!(
            ledger.receipts,
            LedgerReceiptState::Degraded {
                dropped_threads: 1,
                dropped_commitments: 0
            }
        );
    }

    #[test]
    fn a_ledger_part_that_lost_half_its_threads_keeps_its_better_reading() {
        let generator = ReadsLedger::new(vec![Some((2, 2)), Some((1, 0))]);
        let ledger = read_four_lines(&generator);
        assert_eq!(generator.read.load(Ordering::SeqCst), 2);
        assert_eq!(topics(&ledger), ["Said 0", "Said 1"]);
    }

    #[test]
    fn a_ledger_part_whose_second_reading_fails_keeps_its_first() {
        let generator = ReadsLedger::new(vec![Some((1, 1)), None]);
        let ledger = read_four_lines(&generator);
        assert_eq!(generator.read.load(Ordering::SeqCst), 2);
        assert_eq!(topics(&ledger), ["Said 0"]);
    }

    fn cited_at(seconds: u64) -> ArtifactCitation {
        ArtifactCitation {
            segment_id: TranscriptSegmentId::new(),
            start_offset_ns: seconds * 1_000_000_000,
            end_offset_ns: (seconds + 1) * 1_000_000_000,
        }
    }

    fn thread_citing(state: LedgerThreadState, citation: &ArtifactCitation) -> LedgerThread {
        LedgerThread {
            topic: "Launch date".to_string(),
            state,
            substantive: true,
            receipt: LedgerReceipt {
                quote: "We launch on the ninth.".to_string(),
                speaker: None,
                t_ms: citation.start_offset_ns / 1_000_000,
                citations: vec![citation.clone()],
            },
            owner: None,
        }
    }

    /// An open-loop row cites the question and what happened instead, while
    /// the thread of the question cites the question alone.
    #[test]
    fn a_question_answered_in_a_later_part_closes_its_thread() {
        let (asked, instead, answer) = (cited_at(10), cited_at(20), cited_at(300));
        let mut ledger = MeetingLedger {
            headline: "The launch was discussed.".to_string(),
            threads: vec![
                thread_citing(LedgerThreadState::Unanswered, &asked),
                thread_citing(LedgerThreadState::Decided, &answer),
            ],
            open_loops: vec![LedgerOpenLoop {
                question: "When do we launch?".to_string(),
                instead: "The talk moved to pricing.".to_string(),
                at_ms: 10_000,
                citations: vec![asked, instead],
            }],
            commitments: Vec::new(),
            stances: Vec::new(),
            caveats: Vec::new(),
            receipts: LedgerReceiptState::Verified,
        };
        let reply: LedgerMergeReply = serde_json::from_value(json!({
            "headline": "The launch was discussed.",
            "answered": [{"question": "O1", "by": "T2"}]
        }))
        .expect("merge reply");
        apply_merge(&mut ledger, &[0, 1], &reply);
        assert!(ledger.open_loops.is_empty());
        let states: Vec<LedgerThreadState> =
            ledger.threads.iter().map(|thread| thread.state).collect();
        assert_eq!(states, [LedgerThreadState::Decided]);
    }

    /// A decision told in one part and revised in a later one is one thread
    /// in the latest state, with the words and speaker that settled it. A
    /// thread of the same part, or one sharing a single topic word, stays a
    /// thread of its own even when the engine groups it.
    #[test]
    fn a_subject_a_later_part_returns_to_joins_its_first_thread() {
        let thread = |topic: &str, state, seconds, quote: &str, speaker: &str| LedgerThread {
            topic: topic.to_string(),
            state,
            substantive: true,
            receipt: LedgerReceipt {
                quote: quote.to_string(),
                speaker: Some(speaker.to_string()),
                t_ms: seconds * 1_000,
                citations: vec![cited_at(seconds)],
            },
            owner: None,
        };
        let mut ledger = MeetingLedger {
            headline: "The release was planned.".to_string(),
            threads: vec![
                thread(
                    "PDF page cap",
                    LedgerThreadState::Partial,
                    100,
                    "Cap it at 25 pages for now.",
                    "Marco",
                ),
                thread(
                    "Release date",
                    LedgerThreadState::Decided,
                    200,
                    "We ship on the 20th.",
                    "Priya",
                ),
                thread(
                    "PDF streaming flag",
                    LedgerThreadState::Decided,
                    300,
                    "It goes behind a flag.",
                    "Priya",
                ),
                thread(
                    "Page cap for the PDF preview",
                    LedgerThreadState::Decided,
                    900,
                    "Thirty pages, and the rest streams.",
                    "Jen",
                ),
                thread(
                    "Rollout date",
                    LedgerThreadState::Decided,
                    1_000,
                    "Full rollout on the 25th.",
                    "Priya",
                ),
            ],
            open_loops: Vec::new(),
            commitments: Vec::new(),
            stances: Vec::new(),
            caveats: Vec::new(),
            receipts: LedgerReceiptState::Verified,
        };
        let reply: LedgerMergeReply = serde_json::from_value(json!({
            "headline": "The release was planned.",
            "same_subject": [["T1", "T3", "T4"], ["T2", "T5"]]
        }))
        .expect("merge reply");
        apply_merge(&mut ledger, &[0, 0, 0, 1, 1], &reply);
        let threads: Vec<_> = ledger
            .threads
            .iter()
            .map(|thread| {
                (
                    thread.topic.as_str(),
                    thread.state,
                    thread.receipt.quote.as_str(),
                    thread.receipt.speaker.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            threads,
            [
                (
                    "PDF page cap",
                    LedgerThreadState::Decided,
                    "Thirty pages, and the rest streams.",
                    Some("Jen")
                ),
                (
                    "Release date",
                    LedgerThreadState::Decided,
                    "We ship on the 20th.",
                    Some("Priya")
                ),
                (
                    "PDF streaming flag",
                    LedgerThreadState::Decided,
                    "It goes behind a flag.",
                    Some("Priya")
                ),
                (
                    "Rollout date",
                    LedgerThreadState::Decided,
                    "Full rollout on the 25th.",
                    Some("Priya")
                ),
            ]
        );
    }

    struct GroupsTopics(&'static str);

    impl MeetingTextGenerator for GroupsTopics {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "groups-topics"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            6_000
        }
        fn generate(
            &self,
            _: &str,
            pack: &str,
            _: i32,
            _: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            assert!(no_topic_groups(pack).is_some(), "a topic grouping request");
            Ok(self.0.to_owned())
        }
    }

    /// A subject the meeting came back to shows under a title from each part;
    /// the topics the engine reads as one subject fold into the first, which
    /// keeps every detail and citation.
    #[test]
    fn topics_about_one_subject_fold_into_the_first() {
        let evidence = evidence();
        let cited = |index: usize, text: &str| json!({"text": text, "citations": [evidence.transcript[index].citation.entity_id]});
        let raw: RawArtifactOutput = serde_json::from_value(json!({
            "summary": [cited(0, "The release was planned.")],
            "outline": [
                {"title": cited(0, "PDF crash"), "detail": cited(0, "Old iPads crash on long PDFs.")},
                {"title": cited(80, "Release date"), "detail": cited(80, "The release moves to the 25th.")},
                {"title": cited(159, "PDF page cap"), "detail": cited(159, "Thirty pages, and the rest streams.")}
            ],
            "decisions": [], "action_items": [], "key_questions": [], "risks": [],
            "follow_up_draft": cited(0, "Share the plan.")
        }))
        .expect("joined notes");
        let content = validate_artifact_output(
            &raw,
            &evidence.transcript,
            &HashMap::new(),
            &evidence.template,
        )
        .expect("cited joined notes");
        let content = one_topic_per_subject(
            &GroupsTopics(r#"{"same_subject":[["T1","T3"]]}"#),
            &evidence,
            content,
            &|| Ok(()),
        )
        .expect("grouped outline");
        let outline: Vec<_> = content
            .outline
            .iter()
            .map(|topic| {
                (
                    topic.title.text.as_str(),
                    topic.detail.as_ref().map(|detail| detail.text.as_str()),
                    topic.title.citations.len(),
                )
            })
            .collect();
        assert_eq!(
            outline,
            [
                (
                    "PDF crash",
                    Some("Old iPads crash on long PDFs.\nThirty pages, and the rest streams."),
                    2
                ),
                ("Release date", Some("The release moves to the 25th."), 1),
            ]
        );
    }

    /// Parts that share lines write the same items, and a part can file what
    /// another decided as a risk; the notes Sona joins as written hold each
    /// once.
    #[test]
    fn notes_joined_as_written_hold_an_item_parts_repeat_once() {
        let evidence = evidence();
        let cited = |index: usize, text: &str| json!({"text": text, "citations": [evidence.transcript[index].citation.entity_id]});
        let section = |decisions: Vec<Value>, risks: Vec<Value>| {
            let raw: RawArtifactOutput = serde_json::from_value(json!({
                "summary": [cited(0, "The release was planned.")], "outline": [],
                "decisions": decisions, "action_items": [], "key_questions": [],
                "risks": risks, "follow_up_draft": cited(0, "Share the plan.")
            }))
            .expect("section notes");
            let content = validate_artifact_output(
                &raw,
                &evidence.transcript,
                &HashMap::new(),
                &evidence.template,
            )
            .expect("cited section notes");
            (raw.summary, content)
        };
        let design = || cited(0, "The team chose the blue design.");
        let blocker = || cited(80, "The login bug is a release blocker.");
        let content = joined_as_written(
            vec![
                section(vec![design()], vec![blocker()]),
                section(vec![design(), blocker()], Vec::new()),
            ],
            Vec::new(),
            &evidence,
        )
        .expect("notes joined as written");
        let decisions: Vec<_> = content
            .decisions
            .iter()
            .map(|decision| decision.text.as_str())
            .collect();
        assert_eq!(
            decisions,
            [
                "The team chose the blue design.",
                "The login bug is a release blocker."
            ]
        );
        assert!(content.risks.is_empty());
    }

    #[test]
    fn a_notes_row_names_its_speaker_and_not_its_meeting() {
        let mut evidence = evidence();
        evidence.transcript.truncate(2);
        let scope = SourceScope::new(&evidence.transcript).expect("original sources");
        let speakers =
            HashMap::from([(evidence.transcript[1].citation.entity_id.clone(), "Casey")]);
        let packs = notes_packs(&evidence, &scope, &speakers, 6_000).expect("one notes part");
        let input: Value = serde_json::from_str(packs[0].as_ref()).expect("notes part");
        let rows = input["transcript"].as_array().expect("notes rows");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.get("session_id").is_none()));
        assert!(rows[0].get("speaker").is_none());
        assert_eq!(rows[1]["speaker"], "Casey");
    }

    #[test]
    fn a_meeting_longer_than_one_notes_part_is_cut_into_even_parts() {
        let evidence = evidence();
        let scope = SourceScope::new(&evidence.transcript).expect("original sources");
        let speakers = HashMap::new();
        let whole = notes_packs(&evidence, &scope, &speakers, usize::MAX).expect("one part");
        assert_eq!(whole.len(), 1);
        let length = whole[0].as_ref().len();
        let parts = notes_packs(&evidence, &scope, &speakers, length * 9 / 10).expect("parts");
        let sizes: Vec<usize> = parts.iter().map(|part| part.as_ref().len()).collect();
        assert_eq!(sizes.len(), 2);
        assert!(sizes[0].abs_diff(sizes[1]) < length / 20, "{sizes:?}");
    }

    /// Stops every notes request by the safety filter unless it is asked under
    /// the short instructions.
    struct StoppedUnlessShort;

    impl MeetingTextGenerator for StoppedUnlessShort {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "stopped-unless-short"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            6_000
        }
        fn generate(
            &self,
            instructions: &str,
            pack: &str,
            _: i32,
            _: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            if !instructions.starts_with(SHORT_NOTES_PROMPT) {
                return Err(MeetingTextGenerationError::SafetyBlocked);
            }
            Ok(reply_citing(pack))
        }
    }

    #[test]
    fn a_part_the_safety_filter_keeps_stopping_is_read_under_short_instructions() {
        let evidence = gradual_evidence(2);
        let result = notes(
            &StoppedUnlessShort,
            &evidence,
            &HashMap::new(),
            "Read the meeting.",
            || Ok(()),
        )
        .expect("the short instructions pass");
        assert_eq!(cited_ids(&result), source_ids(&evidence));
    }

    struct DenseSourceGenerator {
        invalid_reduction_reference: bool,
        several_cited_facts: bool,
    }

    impl MeetingTextGenerator for DenseSourceGenerator {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "dense-source-approval"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            usize::MAX
        }
        fn context_window_bytes(&self) -> Option<usize> {
            Some(12_288)
        }
        fn reserved_output_tokens(&self, _: i32) -> usize {
            1_024
        }
        fn generate(
            &self,
            prompt: &str,
            pack: &str,
            max_tokens: i32,
            shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            assert!(pack.len() <= evidence_budget(self, prompt, max_tokens));
            fn collect(value: &Value, ids: &mut Vec<String>) {
                match value {
                    Value::Object(object) => {
                        for key in ["entity_id", "segment_uuid"] {
                            if let Some(id) = object.get(key).and_then(Value::as_str) {
                                if !ids.iter().any(|existing| existing == id) {
                                    ids.push(id.to_string());
                                }
                            }
                        }
                        if let Some(citations) = object.get("citations") {
                            for id in citation_ids(citations) {
                                if !ids.contains(&id) {
                                    ids.push(id);
                                }
                            }
                        }
                        for child in object.values() {
                            collect(child, ids);
                        }
                    }
                    Value::Array(values) => {
                        for child in values {
                            collect(child, ids);
                        }
                    }
                    Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
                }
            }
            let input: Value = serde_json::from_str(pack).expect("bounded approval evidence");
            if input.get("part_headlines").is_some() {
                return Ok(json!({"headline":"The release was approved."}).to_string());
            }
            let mut ids = Vec::new();
            collect(&input, &mut ids);
            if self.invalid_reduction_reference && input.get("section_facts").is_some() {
                ids = vec!["S9999".to_string()];
            }
            let cited = json!({"text":"The release was approved.","citations":ids});
            if matches!(shape, ReplyShape::MeetingLedger) {
                Ok(json!({
                "headline":"The release was approved.",
                "threads":[{"topic":"Release approval","state":"agreed","substantive":true,"owner":null,
                    "receipt":{"quote":"The team approved the release.","speaker":null,"citations":ids}}],
                "open_loops":[],"commitments":[],"stances":[],"caveats":[]
            }).to_string())
            } else {
                if self.several_cited_facts {
                    let facts: Vec<_> = [
                        "The team approved the release.",
                        "The release received approval.",
                        "Release approval was confirmed.",
                    ]
                    .iter()
                    .map(|text| json!({"text":text,"citations":ids}))
                    .collect();
                    let outline: Vec<_> = facts
                        .iter()
                        .map(|fact| json!({"title":fact,"detail":fact}))
                        .collect();
                    return Ok(json!({
                        "summary":facts,"outline":outline,"decisions":[cited],
                        "action_items":[],"key_questions":[],"risks":[],
                        "follow_up_draft":{"text":"No follow-up was agreed.","citations":ids}
                    })
                    .to_string());
                }
                Ok(json!({
                    "summary":[cited],"outline":[{"title":cited,"detail":cited}],
                    "decisions":[cited],"action_items":[],"key_questions":[],"risks":[],
                    "follow_up_draft":{"text":"No follow-up was agreed.","citations":ids}
                })
                .to_string())
            }
        }
    }

    #[test]
    fn a_repeated_topic_keeps_every_source_when_provenance_exceeds_a_model_window() {
        let mut evidence = evidence();
        evidence.transcript.truncate(128);
        for turn in &mut evidence.transcript {
            turn.text = "The team approved the release.".to_string();
        }
        let expected: Vec<_> = evidence
            .transcript
            .iter()
            .map(|turn| turn.citation.entity_id.clone())
            .collect();
        let prompt =
            artifact_system_prompt(&evidence.template, false, MeetingNotesLanguage::default());
        let generator = DenseSourceGenerator {
            invalid_reduction_reference: false,
            several_cited_facts: false,
        };
        let content = notes(&generator, &evidence, &HashMap::new(), &prompt, || Ok(()))
            .expect("repeated source provenance must not prevent complete notes");
        assert_eq!(
            content
                .summary
                .citations
                .iter()
                .map(|citation| citation.segment_id.uuid().to_string())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(content.decisions.len(), 1);
        assert_eq!(content.decisions[0].citations.len(), 128);
        assert_eq!(content.outline.len(), 1);
        assert_eq!(content.outline[0].title.citations.len(), 128);
        assert_eq!(
            content.outline[0]
                .detail
                .as_ref()
                .expect("cited detail")
                .citations
                .len(),
            128
        );
        evidence.transcript.truncate(64);
        let expected: Vec<_> = evidence
            .transcript
            .iter()
            .map(|turn| turn.citation.entity_id.clone())
            .collect();
        let haystack =
            ledger::fold_haystack(evidence.transcript.iter().map(|turn| turn.text.as_str()));
        let content = ledger(&generator, &evidence, &HashMap::new(), &haystack, || Ok(()))
            .expect("repeated source provenance must not prevent a complete ledger");
        assert_eq!(content.threads.len(), 1);
        assert_eq!(
            content.threads[0]
                .receipt
                .citations
                .iter()
                .map(|citation| citation.segment_id.uuid().to_string())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(matches!(content.receipts, LedgerReceiptState::Verified));
    }

    #[test]
    fn an_unrecognized_reconciliation_source_leaves_the_notes_it_could_not_join() {
        let mut evidence = evidence();
        evidence.transcript.truncate(128);
        for (index, turn) in evidence.transcript.iter_mut().enumerate() {
            turn.text = format!("The team approved the release. Agenda item {index}.");
        }
        let generator = DenseSourceGenerator {
            invalid_reduction_reference: true,
            several_cited_facts: false,
        };
        let prompt =
            artifact_system_prompt(&evidence.template, false, MeetingNotesLanguage::default());
        let content = notes(&generator, &evidence, &HashMap::new(), &prompt, || Ok(()))
            .expect("the first round's notes stand when their join cites an unknown source");
        assert_eq!(
            content
                .summary
                .citations
                .iter()
                .map(|citation| citation.segment_id.uuid().to_string())
                .collect::<Vec<_>>(),
            evidence
                .transcript
                .iter()
                .map(|turn| turn.citation.entity_id.clone())
                .collect::<Vec<_>>()
        );
        assert!(content.left_out.is_empty());
    }

    #[test]
    fn independently_cited_facts_can_share_all_sources_without_stalling_complete_notes() {
        let mut evidence = evidence();
        evidence.transcript.truncate(128);
        for turn in &mut evidence.transcript {
            turn.text = "The team approved the release.".to_string();
        }
        let expected: Vec<_> = evidence
            .transcript
            .iter()
            .map(|turn| turn.citation.entity_id.clone())
            .collect();
        let generator = DenseSourceGenerator {
            invalid_reduction_reference: false,
            several_cited_facts: true,
        };
        let prompt =
            artifact_system_prompt(&evidence.template, false, MeetingNotesLanguage::default());
        let content = notes(&generator, &evidence, &HashMap::new(), &prompt, || Ok(())).expect(
        "several supported facts must not multiply source metadata until complete reading stops",
    );
        assert_eq!(
            content
                .summary
                .citations
                .iter()
                .map(|citation| citation.segment_id.uuid().to_string())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(content.decisions.len(), 1);
        assert_eq!(content.decisions[0].citations.len(), 128);
        assert_eq!(content.outline.len(), 3);
        for topic in &content.outline {
            assert_eq!(
                topic
                    .title
                    .citations
                    .iter()
                    .map(|citation| citation.segment_id.uuid().to_string())
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(
                topic
                    .detail
                    .as_ref()
                    .expect("independently cited detail")
                    .citations
                    .iter()
                    .map(|citation| citation.segment_id.uuid().to_string())
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }

    struct CrowdedNotesGenerator(ReplayGenerator);

    impl MeetingTextGenerator for CrowdedNotesGenerator {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "crowded-notes-replay"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            self.0.wire
        }
        fn generate(
            &self,
            prompt: &str,
            pack: &str,
            max_tokens: i32,
            shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            let mut output: Value =
                serde_json::from_str(&self.0.generate(prompt, pack, max_tokens, shape)?)
                    .expect("structured replay notes");
            let input: Value = serde_json::from_str(pack).expect("bounded notes evidence");
            if let Some(facts) = input["section_facts"].as_array() {
                let mut summaries = std::collections::HashSet::new();
                let repeated_narrative =
                    facts
                        .iter()
                        .filter(|fact| fact["kind"] == "summary")
                        .any(|fact| {
                            !summaries.insert(fact["value"]["text"].as_str().expect("summary text"))
                        });
                if repeated_narrative {
                    output["action_items"] = json!([]);
                }
            }
            Ok(output.to_string())
        }
    }

    #[test]
    fn repeated_narrative_does_not_erase_a_later_report_revision() {
        let evidence = evidence();
        let generator = CrowdedNotesGenerator(ReplayGenerator {
            window: None,
            wire: 6_000,
            reply: 3_200,
            calls: AtomicUsize::new(0),
        });
        let prompt =
            artifact_system_prompt(&evidence.template, false, MeetingNotesLanguage::default());
        let content = notes(&generator, &evidence, &HashMap::new(), &prompt, || Ok(()))
            .expect("complete reconciled meeting notes");
        assert_eq!(content.decisions.len(), 3);
        assert_eq!(content.action_items.len(), 1);
        assert_eq!(
            content.action_items[0].text.text,
            "Casey will send the report Monday instead of Friday."
        );
        assert_eq!(
            content.action_items[0].text.citations[0].segment_id.uuid(),
            uuid::Uuid::from_u128(159)
        );
    }

    #[test]
    fn nonadjacent_returning_decisions_keep_their_original_order() {
        let mut evidence = evidence();
        for (index, text) in [
            (0, "The team chose blue."),
            (80, "The team chose green."),
            (159, "The team chose blue."),
        ] {
            evidence.transcript[index].text = text.to_string();
        }
        let cited = |index: usize| {
            json!({
                "text": evidence.transcript[index].text,
                "citations": [evidence.transcript[index].citation.entity_id]
            })
        };
        let raw: RawArtifactOutput = serde_json::from_value(json!({
            "summary": [cited(0)], "outline": [],
            "decisions": [cited(0), cited(80), cited(159)], "action_items": [],
            "key_questions": [], "risks": [], "follow_up_draft": cited(159)
        }))
        .expect("returning decision fixture");
        let section = validate_artifact_output(
            &raw,
            &evidence.transcript,
            &HashMap::new(),
            &evidence.template,
        )
        .expect("cited returning decisions");
        let scope = SourceScope::new(&evidence.transcript).expect("original sources");
        let decisions: Vec<Value> = notes_facts(&raw.summary, &section, &scope)
            .expect("chronological note facts")
            .iter()
            .map(|(row, _)| serde_json::from_str::<Value>(row).expect("typed fact"))
            .filter(|row| row["kind"] == "decision")
            .collect();
        assert_eq!(
            decisions
                .iter()
                .map(|row| row["value"]["text"].as_str().expect("decision"))
                .collect::<Vec<_>>(),
            [
                "The team chose blue.",
                "The team chose green.",
                "The team chose blue."
            ]
        );
        let mut ids: Vec<_> = decisions
            .iter()
            .flat_map(|row| citation_ids(&row["value"]["citations"]))
            .collect();
        scope
            .restore_ids(&mut ids)
            .expect("original decision sources");
        assert_eq!(
            ids,
            [0, 80, 159].map(|index| evidence.transcript[index].citation.entity_id.clone())
        );
    }

    #[test]
    fn compacted_narrative_keeps_every_line_and_outline_source_separate() {
        let evidence = evidence();
        let cited = |text: &str, index: usize| json!({"text": text, "citations": [evidence.transcript[index].citation.entity_id]});
        let raw: RawArtifactOutput = serde_json::from_value(json!({
            "summary": [cited("Discussion.", 0), cited("Discussion.", 1)],
            "outline": [
                {"title": cited("Topic", 0), "detail": cited("Details", 80)},
                {"title": cited("Topic", 1), "detail": cited("Details", 159)}
            ],
            "decisions": [], "action_items": [], "key_questions": [], "risks": [],
            "follow_up_draft": cited("No follow-up was agreed.", 0)
        }))
        .expect("separately cited narrative fixture");
        let section = validate_artifact_output(
            &raw,
            &evidence.transcript,
            &HashMap::new(),
            &evidence.template,
        )
        .expect("valid narrative");
        let scope = SourceScope::new(&evidence.transcript).expect("original sources");
        let rows: Vec<Value> = notes_facts(&raw.summary, &section, &scope)
            .expect("compacted narrative facts")
            .iter()
            .map(|(row, _)| serde_json::from_str(row).expect("typed fact"))
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["kind"], "summary");
        assert_eq!(rows[1]["kind"], "outline");
        let restore = |value: &Value| {
            let mut ids = citation_ids(value);
            scope
                .restore_ids(&mut ids)
                .expect("original narrative sources");
            ids
        };
        let first_sources =
            [0, 1].map(|index| evidence.transcript[index].citation.entity_id.clone());
        assert_eq!(restore(&rows[0]["value"]["citations"]), first_sources);
        assert_eq!(
            restore(&rows[1]["value"]["title"]["citations"]),
            first_sources
        );
        assert_eq!(
            restore(&rows[1]["value"]["detail"]["citations"]),
            [80, 159].map(|index| evidence.transcript[index].citation.entity_id.clone())
        );
    }

    fn original_repeated_meeting() -> ArtifactEvidence {
        let mut evidence = evidence();
        evidence.transcript.truncate(128);
        let filler = "The team reviews the existing dashboard and checks the current setup. This part introduces no new decision, commitment, owner or deadline. ".repeat(8);
        for turn in &mut evidence.transcript {
            turn.text = filler.clone();
        }
        for (index, text) in [
        (0, "Jordan says: We have decided to use the cobalt design. Everyone agrees; this is the final design decision."),
        (1, "Casey says: I will send the report Friday. Jordan confirms that Casey owns sending this report."),
        (64, "Jordan says: We have decided to keep the annual price at 240 dollars. Everyone agrees; the annual price remains 240 dollars."),
        (126, "Casey says: Change my earlier report deadline. I will send that same report Monday instead of Friday. Friday is cancelled. Jordan confirms that Casey's one report commitment is now due Monday."),
        (127, "Jordan says: We have decided to release on November 12. Everyone agrees; November 12 is the final release date."),
    ] {
        evidence.transcript[index].text = text.to_string();
    }
        evidence
    }

    struct CompleteSourceConsumer {
        replay: ReplayGenerator,
        first_pass_calls: AtomicUsize,
        source_ids: Vec<String>,
        observed: Mutex<Vec<Value>>,
        packed_rows: Mutex<Vec<Value>>,
    }

    impl CompleteSourceConsumer {
        fn new(evidence: &ArtifactEvidence) -> Self {
            Self {
                replay: ReplayGenerator {
                    window: None,
                    wire: 5_799,
                    reply: 1_024,
                    calls: AtomicUsize::new(0),
                },
                first_pass_calls: AtomicUsize::new(0),
                source_ids: evidence
                    .transcript
                    .iter()
                    .map(|turn| turn.citation.entity_id.clone())
                    .collect(),
                observed: Mutex::new(Vec::new()),
                packed_rows: Mutex::new(Vec::new()),
            }
        }
    }

    impl MeetingTextGenerator for CompleteSourceConsumer {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "complete-source-consumer"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            self.replay.wire
        }
        fn generate(
            &self,
            prompt: &str,
            pack: &str,
            max_tokens: i32,
            shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            let input: Value = serde_json::from_str(pack).expect("complete consumer input");
            let ledger = matches!(shape, ReplyShape::MeetingLedger);
            let mut sources = Vec::new();
            if let Some(turns) = input["transcript"].as_array() {
                self.first_pass_calls.fetch_add(1, Ordering::SeqCst);
                let mut observed = self.observed.lock().expect("observed original turns");
                for turn in turns {
                    self.packed_rows
                        .lock()
                        .expect("packed raw rows")
                        .push(turn.clone());
                    let id_key = if ledger { "segment_uuid" } else { "entity_id" };
                    sources.push(turn[id_key].as_str().expect("source reference").to_string());
                    let text = turn["text"].as_str().expect("complete raw source text");
                    if let Some(occurrences) = turn["occurrences"].as_array() {
                        for occurrence in occurrences {
                            let mut original = turn.clone();
                            original
                                .as_object_mut()
                                .expect("source object")
                                .remove("occurrences");
                            for (key, value) in occurrence.as_object().expect("original occurrence")
                            {
                                original[key] = value.clone();
                            }
                            original["text"] = json!(text);
                            observed.push(original);
                        }
                    } else if let Some(occurrences) = turn["occurrences"].as_object() {
                        let number = |key: &str| {
                            occurrences[key]
                                .as_u64()
                                .expect("lossless occurrence number")
                        };
                        let first =
                            usize::try_from(number("first_source")).expect("original source index");
                        let count =
                            usize::try_from(number("count")).expect("original source count");
                        for (offset, id) in self.source_ids[first..first + count].iter().enumerate()
                        {
                            let offset = u64::try_from(offset).expect("original occurrence offset");
                            let mut original = turn.clone();
                            original
                                .as_object_mut()
                                .expect("source object")
                                .remove("occurrences");
                            original[id_key] = json!(id);
                            original["start_offset_ns"] =
                                json!(number("start_offset_ns") + offset * number("step_ns"));
                            original["end_offset_ns"] =
                                json!(number("end_offset_ns") + offset * number("step_ns"));
                            observed.push(original);
                        }
                    } else {
                        observed.push(turn.clone());
                    }
                }
            } else {
                for fact in input["section_facts"].as_array().expect("consumer facts") {
                    if fact["kind"] == "summary" {
                        sources.extend(citation_ids(&fact["value"]["citations"]));
                    }
                }
            }
            let mut output: Value =
                serde_json::from_str(&self.replay.generate(prompt, pack, max_tokens, shape)?)
                    .expect("semantic consumer output");
            if !ledger {
                output["summary"] = json!([{
                    "text":"The team reviewed the dashboard, chose the design, kept the annual price and set the release date.",
                    "citations":sources
                }]);
            }
            Ok(output.to_string())
        }
    }

    #[test]
    fn original_repeated_meeting_is_read_losslessly_with_at_most_half_the_first_pass_requests() {
        let evidence = original_repeated_meeting();
        assert_eq!(evidence.transcript.len(), 128);
        assert_eq!(
            evidence
                .transcript
                .iter()
                .map(|turn| turn.text.len())
                .sum::<usize>(),
            137_404
        );
        let generator = CompleteSourceConsumer::new(&evidence);
        let prompt =
            artifact_system_prompt(&evidence.template, false, MeetingNotesLanguage::default());
        let content = notes(&generator, &evidence, &HashMap::new(), &prompt, || Ok(()))
            .expect("complete original meeting");
        let expected: Vec<_> = evidence
            .transcript
            .iter()
            .map(|turn| {
                serde_json::to_value(NotesPromptEvidence::from(turn))
                    .expect("original source fields")
            })
            .collect();
        assert_eq!(
            *generator.observed.lock().expect("all read source fields"),
            expected
        );
        assert_eq!(
            content
                .summary
                .citations
                .iter()
                .map(|citation| {
                    (
                        citation.segment_id.uuid().to_string(),
                        citation.start_offset_ns,
                        citation.end_offset_ns,
                    )
                })
                .collect::<Vec<_>>(),
            evidence
                .transcript
                .iter()
                .map(|turn| (
                    turn.citation.entity_id.clone(),
                    turn.citation.start_offset_ns.expect("original start"),
                    turn.citation.end_offset_ns.expect("original end")
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(content.decisions.len(), 3);
        for (decision, index) in content.decisions.iter().zip([0, 64, 127]) {
            assert_eq!(decision.text, evidence.transcript[index].text);
            assert_eq!(
                decision.citations[0].segment_id.uuid().to_string(),
                evidence.transcript[index].citation.entity_id
            );
        }
        assert_eq!(content.action_items.len(), 1);
        assert!(content.action_items[0].text.text.contains("Monday"));
        assert_eq!(
            content.action_items[0].text.citations[0]
                .segment_id
                .uuid()
                .to_string(),
            evidence.transcript[126].citation.entity_id
        );
        assert!(content.key_questions.is_empty());
        assert!(content.risks.is_empty());
        assert!(generator
            .packed_rows
            .lock()
            .expect("notes rows")
            .iter()
            .all(|row| row.get("speaker").is_none()));
        let requests = generator.first_pass_calls.load(Ordering::SeqCst);
        assert!(
            requests <= 1,
            "identical adjacent source runs must fit one first pass; observed {requests}"
        );
        let speakers: HashMap<_, _> = evidence
            .transcript
            .iter()
            .enumerate()
            .map(|(index, turn)| {
                (
                    turn.citation.entity_id.clone(),
                    if index == 1 || index == 126 {
                        "Casey"
                    } else {
                        "Jordan"
                    },
                )
            })
            .collect();
        let generator = CompleteSourceConsumer::new(&evidence);
        let haystack =
            ledger::fold_haystack(evidence.transcript.iter().map(|turn| turn.text.as_str()));
        let content = ledger(&generator, &evidence, &speakers, &haystack, || Ok(()))
            .expect("complete original ledger");
        let expected: Vec<_> = evidence
            .transcript
            .iter()
            .map(|turn| {
                serde_json::to_value(LedgerPromptEvidence {
                    segment_uuid: &turn.citation.entity_id,
                    speaker: speakers.get(&turn.citation.entity_id).copied(),
                    start_offset_ns: turn.citation.start_offset_ns,
                    end_offset_ns: turn.citation.end_offset_ns,
                    text: &turn.text,
                })
                .expect("original ledger source fields")
            })
            .collect();
        assert_eq!(
            *generator.observed.lock().expect("all ledger source fields"),
            expected
        );
        assert_eq!(content.commitments.len(), 1);
        assert_eq!(content.commitments[0].what, "Send report Monday");
        assert_eq!(
            content.commitments[0]
                .receipt
                .citations
                .iter()
                .map(|citation| citation.segment_id.uuid().to_string())
                .collect::<Vec<_>>(),
            [1, 126].map(|index| evidence.transcript[index].citation.entity_id.clone())
        );
        let decisions: Vec<_> = content
            .threads
            .iter()
            .filter(|thread| thread.topic.starts_with("Jordan says: We have decided"))
            .collect();
        assert_eq!(decisions.len(), 3);
        for (thread, index) in decisions.iter().zip([0, 64, 127]) {
            assert_eq!(thread.receipt.quote, evidence.transcript[index].text);
            assert_eq!(
                thread.receipt.citations[0].segment_id.uuid().to_string(),
                evidence.transcript[index].citation.entity_id
            );
        }
        assert!(content.open_loops.is_empty());
        assert!(matches!(content.receipts, LedgerReceiptState::Verified));
        assert_eq!(ledger::unverified_receipts(&content, &haystack), 0);
        let requests = generator.first_pass_calls.load(Ordering::SeqCst);
        assert!(
            requests <= 1,
            "identical adjacent ledger source runs must fit one first pass; observed {requests}"
        );
    }

    #[test]
    fn raw_source_grouping_preserves_changed_speakers_content_and_nonadjacent_returns() {
        let mut evidence = original_repeated_meeting();
        evidence.transcript.truncate(8);
        let repeated = evidence.transcript[2].text.clone();
        for turn in &mut evidence.transcript {
            turn.text = repeated.clone();
        }
        evidence.transcript[3].text = "The team checked a different dashboard.".to_string();
        evidence.transcript[6].text = "Casey will send the report Friday.".to_string();
        evidence.transcript[7].text =
            "Casey will send the report Monday instead of Friday.".to_string();
        let speakers: HashMap<_, _> = evidence
            .transcript
            .iter()
            .enumerate()
            .map(|(index, turn)| {
                (
                    turn.citation.entity_id.clone(),
                    if matches!(index, 0 | 1 | 5) {
                        "Jordan"
                    } else {
                        "Casey"
                    },
                )
            })
            .collect();
        let generator = CompleteSourceConsumer::new(&evidence);
        let haystack =
            ledger::fold_haystack(evidence.transcript.iter().map(|turn| turn.text.as_str()));
        let content = ledger(&generator, &evidence, &speakers, &haystack, || Ok(()))
            .expect("mixed original ledger");
        let rows = generator.packed_rows.lock().expect("mixed packed rows");
        assert_eq!(rows.len(), 7);
        for (row, index) in rows[1..].iter().zip(2..8) {
            assert_eq!(
                row["segment_uuid"],
                evidence.transcript[index].citation.entity_id
            );
            assert_eq!(row["text"], evidence.transcript[index].text);
            assert_eq!(
                row["speaker"],
                speakers[&evidence.transcript[index].citation.entity_id]
            );
            assert!(row.get("occurrences").is_none());
        }
        assert_eq!(content.commitments.len(), 1);
        assert_eq!(content.commitments[0].what, "Send report Monday");
    }

    #[test]
    fn grouped_first_pass_and_unique_sources_restore_without_a_reduction_round() {
        let mut evidence = original_repeated_meeting();
        evidence.transcript.truncate(4);
        let generator = CompleteSourceConsumer::new(&evidence);
        let prompt =
            artifact_system_prompt(&evidence.template, false, MeetingNotesLanguage::default());
        let content = notes(&generator, &evidence, &HashMap::new(), &prompt, || Ok(()))
            .expect("single-pass grouped notes");
        assert_eq!(generator.replay.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            content
                .summary
                .citations
                .iter()
                .map(|citation| citation.segment_id.uuid().to_string())
                .collect::<Vec<_>>(),
            evidence
                .transcript
                .iter()
                .map(|turn| turn.citation.entity_id.clone())
                .collect::<Vec<_>>()
        );
        let rows = generator.packed_rows.lock().expect("single-pass rows");
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0]["entity_id"],
            evidence.transcript[0].citation.entity_id
        );
        assert_eq!(
            rows[1]["entity_id"],
            evidence.transcript[1].citation.entity_id
        );
        evidence.transcript[2].text = evidence.transcript[0].text.clone();
        evidence.transcript[3].text = evidence.transcript[0].text.clone();
        let generator = CompleteSourceConsumer::new(&evidence);
        let haystack =
            ledger::fold_haystack(evidence.transcript.iter().map(|turn| turn.text.as_str()));
        let content = ledger(&generator, &evidence, &HashMap::new(), &haystack, || Ok(()))
            .expect("single-pass grouped ledger");
        assert_eq!(generator.replay.calls.load(Ordering::SeqCst), 1);
        let decision = content
            .threads
            .iter()
            .find(|thread| thread.topic.contains("cobalt"))
            .expect("grouped decision");
        assert_eq!(
            decision
                .receipt
                .citations
                .iter()
                .map(|citation| citation.segment_id.uuid().to_string())
                .collect::<Vec<_>>(),
            [2, 3].map(|index| evidence.transcript[index].citation.entity_id.clone())
        );
    }

    #[test]
    fn a_grouped_ledger_receipt_keeps_all_sources_beyond_the_thread_row_limit() {
        let mut evidence = evidence();
        evidence.transcript.truncate(96);
        for turn in &mut evidence.transcript {
            turn.text = "DECISION: Keep the current annual price.".to_string();
        }
        let generator = ReplayGenerator {
            window: None,
            wire: 64 * 1024,
            reply: 1024,
            calls: AtomicUsize::new(0),
        };
        let haystack =
            ledger::fold_haystack(evidence.transcript.iter().map(|turn| turn.text.as_str()));
        let content = ledger(&generator, &evidence, &HashMap::new(), &haystack, || Ok(()))
            .expect("one decision with every original occurrence");
        assert_eq!(content.threads.len(), 1);
        assert_eq!(
            content.threads[0].receipt.quote,
            evidence.transcript[0].text
        );
        assert_eq!(
            content.threads[0]
                .receipt
                .citations
                .iter()
                .map(|citation| citation.segment_id.uuid().to_string())
                .collect::<Vec<_>>(),
            evidence
                .transcript
                .iter()
                .map(|turn| turn.citation.entity_id.clone())
                .collect::<Vec<_>>()
        );
        let mut ids: Vec<_> = evidence
            .transcript
            .iter()
            .map(|turn| turn.citation.entity_id.clone())
            .collect();
        ids.push(uuid::Uuid::from_u128(9001).to_string());
        let index = transcript_citation_index(&evidence.transcript).expect("all original sources");
        assert!(
            resolve_citations(&ids, &index, "threads.receipt").is_err(),
            "a foreign source after the first page is still rejected"
        );
    }

    #[test]
    fn identical_turns_preserve_irregular_original_offsets_and_source_identity() {
        let mut evidence = original_repeated_meeting();
        evidence.transcript.truncate(7);
        let text = evidence.transcript[2].text.clone();
        for turn in &mut evidence.transcript {
            turn.text.clone_from(&text);
        }
        evidence.transcript[3].citation.start_offset_ns = evidence.transcript[3]
            .citation
            .start_offset_ns
            .map(|value| value + 17);
        evidence.transcript[5].citation.end_offset_ns = evidence.transcript[5]
            .citation
            .end_offset_ns
            .map(|value| value - 23);
        let generator = CompleteSourceConsumer::new(&evidence);
        let prompt =
            artifact_system_prompt(&evidence.template, false, MeetingNotesLanguage::default());
        let content = notes(&generator, &evidence, &HashMap::new(), &prompt, || Ok(()))
            .expect("all irregular original occurrences");
        assert_eq!(
            *generator.observed.lock().expect("observed original fields"),
            evidence
                .transcript
                .iter()
                .map(|turn| serde_json::to_value(NotesPromptEvidence::from(turn))
                    .expect("original source fields"))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            content
                .summary
                .citations
                .iter()
                .map(|citation| (
                    citation.segment_id.uuid().to_string(),
                    citation.start_offset_ns,
                    citation.end_offset_ns
                ))
                .collect::<Vec<_>>(),
            evidence
                .transcript
                .iter()
                .map(|turn| (
                    turn.citation.entity_id.clone(),
                    turn.citation.start_offset_ns.expect("original start"),
                    turn.citation.end_offset_ns.expect("original end")
                ))
                .collect::<Vec<_>>()
        );
    }

    struct PlainJoinBlocked(AtomicUsize);

    impl MeetingTextGenerator for PlainJoinBlocked {
        fn is_available(&self) -> bool {
            true
        }
        fn model_id(&self) -> &'static str {
            "plain-join-blocked"
        }
        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("1")
        }
        fn max_input_bytes(&self) -> usize {
            6_000
        }
        fn generate(
            &self,
            prompt: &str,
            _pack: &str,
            _max_tokens: i32,
            _shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            if prompt.ends_with(PLAIN_NOTES_JOIN) {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(MeetingTextGenerationError::SafetyBlocked)
            } else {
                Ok("The team went through its agenda.".to_string())
            }
        }
    }

    #[test]
    fn plain_notes_whose_join_is_stopped_keep_every_part() {
        let generator = PlainJoinBlocked(AtomicUsize::new(0));
        let (text, left_out) = plain_notes(
            &generator,
            &evidence(),
            &HashMap::new(),
            "Write plain notes.",
            || Ok(()),
        )
        .expect("the parts are the notes");
        assert!(generator.0.load(Ordering::SeqCst) > 0);
        assert!(left_out.is_empty());
        assert!(
            text.matches("The team went through its agenda.").count() > 1,
            "{text}"
        );
    }
}
