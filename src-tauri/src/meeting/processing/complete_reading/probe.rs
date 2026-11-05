//! Every notes and ledger pass on Apple Intelligence, run over a meeting
//! exported from Sona as JSON, each reported by how it ended. The report
//! holds no word of the meeting, so it can leave the Mac the meeting is on.
//!
//! SONA_PROBE_MEETING=<export.json> <test binary> --ignored --exact \
//!     meeting::processing::complete_reading::probe::notes_probe \
//!     --nocapture --test-threads=1

use super::*;
use crate::apple_intelligence::{AppleFailureKind, AppleGenerationError};
use crate::meeting::store::workflow_core_tests;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

const SHORT_NOTES_PROMPT: &str = "Write notes of this meeting: what was decided, what each person agreed to do, the open questions and the risks.";

#[derive(Deserialize)]
struct Export {
    review: Review,
}

#[derive(Deserialize)]
struct Review {
    speakers: Vec<Speaker>,
    transcript: Vec<Line>,
}

#[derive(Deserialize)]
struct Speaker {
    speaker_id: SpeakerId,
    display_name: String,
}

#[derive(Deserialize)]
struct Line {
    base: Base,
    replacement_text: Option<String>,
    removed: bool,
    assigned_speaker_id: SpeakerId,
}

#[derive(Deserialize)]
struct Base {
    start_offset_ns: u64,
    end_offset_ns: u64,
    text: String,
}

/// The app's Apple Intelligence engine, keeping how its last request ended.
#[derive(Default)]
struct Witness {
    failure: Mutex<Option<AppleGenerationError>>,
}

impl MeetingTextGenerator for Witness {
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
        let Ok(reply_tokens) = i32::try_from(ON_DEVICE_REPLY_TOKENS) else {
            return Err(MeetingTextGenerationError::Failed);
        };
        let result = apple_json::generate(prompt, input, max_tokens.min(reply_tokens), shape);
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) =
            result.as_ref().err().cloned();
        result.map_err(apple_generation_error)
    }
}

impl Witness {
    /// How the last request ended, in words that hold none of the meeting:
    /// the pass, the kind of stop, and the stages Apple Intelligence tried.
    /// A free-form engine message is never repeated, since it may quote the
    /// reply.
    fn why(&self) -> String {
        let Some(error) = self
            .failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return "written, but the app could not use the reply".to_string();
        };
        let kind = match error.kind {
            AppleFailureKind::Guardrail => "stopped by the safety filter",
            AppleFailureKind::Refusal => "declined",
            AppleFailureKind::ContextWindow => "too long for the model",
            AppleFailureKind::UnsupportedLanguage => "unsupported language",
            AppleFailureKind::TimedOut => "timed out",
            AppleFailureKind::Other => "failed",
        };
        let pass = error.pass.unwrap_or("request");
        match error.message.split_once(" Tries: ") {
            Some((_, tries)) => format!("{pass} {kind}; tries: {}", tries.trim_end_matches('.')),
            None => format!("{pass} {kind}"),
        }
    }
}

#[derive(Deserialize)]
struct SettingsStore {
    settings: crate::settings::AppSettings,
}

/// The settings Sona keeps on this Mac, read and never written, so the probe
/// asks Apple Intelligence what the app asks it.
fn app_settings() -> Option<crate::settings::AppSettings> {
    let path = std::path::PathBuf::from(std::env::var_os("HOME")?)
        .join("Library/Application Support/com.aktanazat.sona")
        .join(crate::settings::SETTINGS_STORE_PATH);
    serde_json::from_slice::<SettingsStore>(&std::fs::read(path).ok()?)
        .ok()
        .map(|store| store.settings)
}

fn report(step: &str, started: Instant, outcome: &str) {
    println!(
        "{step}: {outcome} ({:.0} s)",
        started.elapsed().as_secs_f64()
    );
}

fn without_untrusted_sentences(prompt: &str) -> String {
    prompt
        .split_inclusive(". ")
        .filter(|sentence| !sentence.contains("untrusted"))
        .collect()
}

/// The meeting, seeded into a scratch store the way the app stores an
/// imported transcript, with every kept line under its speaker's name.
fn seed(store: &MeetingStore, review: &Review) -> Result<MeetingSessionId, StoreError> {
    let names: HashMap<SpeakerId, &str> = review
        .speakers
        .iter()
        .map(|speaker| (speaker.speaker_id, speaker.display_name.as_str()))
        .collect();
    let mut lines: Vec<(&Line, &str)> = review
        .transcript
        .iter()
        .filter(|line| !line.removed)
        .map(|line| {
            (
                line,
                line.replacement_text.as_deref().unwrap_or(&line.base.text),
            )
        })
        .filter(|(_, text)| !text.trim().is_empty())
        .collect();
    lines.sort_by_key(|(line, _)| line.base.start_offset_ns);
    let texts: Vec<&str> = lines.iter().map(|(_, text)| *text).collect();
    let session_id = workflow_core_tests::reviewable_meeting(store, "probe", 1);
    workflow_core_tests::transcript_segments(store, session_id, &texts);
    let mut plan = empty_local_plan();
    plan.session_id = session_id;
    plan.attempt_number = 2;
    workflow_core_tests::processing_plan(store, &plan)?;
    let track_id = store.review_snapshot(session_id)?.tracks[0].track_id;
    let revision = store.begin_transcript_revision(TranscriptRevisionInput {
        session_id,
        engine_id: "import",
        model_version: None,
        destination: &ProcessingDestination::Local,
        source_set: &[SourceKind::Microphone],
        language: "en",
    })?;
    let inputs: Vec<_> = lines
        .iter()
        .map(|(line, text)| TranscriptSegmentInput {
            track_id,
            source_kind: SourceKind::Microphone,
            start_offset_ns: line.base.start_offset_ns,
            end_offset_ns: line.base.end_offset_ns,
            text: (*text).to_string(),
            confidence_milli: None,
            speaker: names
                .get(&line.assigned_speaker_id)
                .map(|name| (*name).to_string()),
        })
        .collect();
    store.append_transcript_segments(session_id, revision, &inputs)?;
    store.complete_transcript_revision(session_id, revision)?;
    Ok(session_id)
}

#[ignore = "runs Apple Intelligence over the meeting export SONA_PROBE_MEETING names"]
#[test]
fn notes_probe() {
    let path = std::env::var_os("SONA_PROBE_MEETING")
        .expect("SONA_PROBE_MEETING names a meeting exported from Sona as JSON");
    let bytes = std::fs::read(&path).expect("read the meeting export");
    let export: Export = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "not a Sona JSON export (line {}, column {})",
            error.line(),
            error.column()
        )
    });
    println!("\nsona build {}", env!("SONA_BUILD_COMMIT"));
    let witness = Witness::default();
    if !witness.is_available() {
        println!(
            "apple intelligence is not available: {:?}",
            AppleIntelligenceGenerator::blocker()
        );
        return;
    }

    let settings = app_settings();
    match &settings {
        Some(settings) if settings.meeting_notes_custom_template_id.is_some() => println!(
            "settings: this mac's sona settings, but the built-in notes template stands in for the custom one"
        ),
        Some(_) => println!("settings: this mac's sona settings"),
        None => println!("settings: defaults, this mac's sona settings could not be read"),
    }
    let (_directory, store) = workflow_core_tests::store();
    let session_id = seed(&store, &export.review)
        .unwrap_or_else(|_| panic!("the scratch store would not take the transcript"));
    let choice = settings
        .as_ref()
        .map(|settings| NotesTemplateChoice {
            template: settings.meeting_notes_template,
            custom_template_id: None,
        })
        .unwrap_or_default();
    let template = store
        .notes_template_fallback(session_id, choice)
        .expect("notes template");
    let mut evidence = store
        .artifact_evidence(session_id, usize::MAX, template)
        .expect("notes evidence");
    let lines = evidence.transcript.len();
    let vocabulary = settings.as_ref().map_or_else(
        || crate::settings::get_default_settings().custom_words,
        |settings| settings.custom_words.clone(),
    );
    drop_vocabulary_echoes(&mut evidence, &vocabulary);
    println!(
        "transcript: {lines} lines, {} only repeat vocabulary words, {} bytes kept",
        lines - evidence.transcript.len(),
        evidence
            .transcript
            .iter()
            .map(|item| item.text.len())
            .sum::<usize>()
    );
    if evidence.transcript.is_empty() {
        println!("nothing left to write notes from");
        return;
    }

    let language = settings
        .as_ref()
        .map(|settings| settings.meeting_notes_language)
        .unwrap_or_default();
    let prompt = notes_system_prompt(&evidence, settings.as_ref());
    let scope = SourceScope::new(&evidence.transcript).expect("source scope");
    let mut input = notes_packs(
        &evidence,
        &scope,
        evidence_budget(&witness, &prompt, ARTIFACT_MAX_TOKENS),
    )
    .expect("notes parts");
    let mut instructions = prompt.clone();
    for round in 1..=MAX_REDUCTION_ROUNDS {
        let mut sections = Vec::new();
        for (index, pack) in input.iter().enumerate() {
            let step = format!(
                "notes round {round} part {} of {} ({} bytes)",
                index + 1,
                input.len(),
                pack.len()
            );
            let started = Instant::now();
            let Ok(output) = witness.generate(
                &instructions,
                pack,
                ARTIFACT_MAX_TOKENS,
                ReplyShape::MeetingNotes,
            ) else {
                report(&step, started, &witness.why());
                if round == 1 {
                    for (variant, other) in [
                        ("short instructions", SHORT_NOTES_PROMPT.to_string()),
                        (
                            "without the untrusted-data sentences",
                            without_untrusted_sentences(&prompt),
                        ),
                    ] {
                        let started = Instant::now();
                        let outcome = match witness.generate(
                            &other,
                            pack,
                            ARTIFACT_MAX_TOKENS,
                            ReplyShape::MeetingNotes,
                        ) {
                            Ok(_) => "ok".to_string(),
                            Err(_) => witness.why(),
                        };
                        report(&format!("  same part, {variant}"), started, &outcome);
                    }
                }
                continue;
            };
            let read = first_json_value::<RawArtifactOutput>(&output)
                .ok()
                .and_then(|mut raw| {
                    scope.restore_notes(&mut raw).ok()?;
                    let content =
                        validate_artifact_output(&raw, &evidence.transcript, &evidence.template)
                            .ok()?;
                    Some((raw.summary, content))
                });
            match read {
                Some(section) => {
                    report(&step, started, "ok");
                    sections.push(section);
                }
                None => report(
                    &step,
                    started,
                    "written, but the app could not use the reply",
                ),
            }
        }
        if input.len() == 1 || sections.is_empty() {
            break;
        }
        let mut facts = NoteFacts::default();
        for (summary, content) in &sections {
            facts
                .extend(summary, content, &scope)
                .expect("section facts");
        }
        instructions = format!("{prompt}{NOTES_RECONCILIATION}");
        input = packs(
            facts.rows(&scope).expect("section fact rows"),
            "{\"section_facts\":[",
            "]}",
            evidence_budget(&witness, &instructions, ARTIFACT_MAX_TOKENS),
        )
        .expect("join parts");
    }

    let segments = store
        .analytics_segments(session_id)
        .expect("scratch segments");
    let speaker_names = store
        .speaker_display_names(session_id)
        .expect("scratch speakers");
    let speakers = turn_speakers(&segments, &speaker_names);
    let haystack = ledger::fold_haystack(evidence.transcript.iter().map(|item| item.text.as_str()));
    let ledger_prompt = ledger_system_prompt();
    let ledger_input = ledger_packs(
        &scope,
        &speakers,
        evidence_budget(&witness, &ledger_prompt, LEDGER_MAX_TOKENS),
    )
    .expect("ledger parts");
    let mut ledgers = Vec::new();
    for (index, pack) in ledger_input.iter().enumerate() {
        let step = format!(
            "ledger part {} of {} ({} bytes)",
            index + 1,
            ledger_input.len(),
            pack.len()
        );
        let started = Instant::now();
        match read_ledger_pack(
            &witness,
            &evidence,
            &haystack,
            &ledger_prompt,
            pack,
            Some(&scope),
        ) {
            Ok(ledger) => {
                report(&step, started, "ok");
                ledgers.push(ledger);
            }
            Err(cause) => report(&step, started, &format!("{}; {cause:?}", witness.why())),
        }
    }
    if ledgers.len() > 1 {
        let started = Instant::now();
        match merge_ledgers(&witness, ledgers, || Ok(())) {
            Ok(_) => report("ledger merge", started, "ok"),
            Err(error) => report(
                "ledger merge",
                started,
                &format!("{}; {error:?}", witness.why()),
            ),
        }
    }

    let started = Instant::now();
    match plain_notes(
        &witness,
        &evidence,
        &speakers,
        &plain_notes_prompt(language),
        || Ok(()),
    ) {
        Ok(text) => report("plain notes", started, &format!("ok, {} bytes", text.len())),
        Err(error) => report(
            "plain notes",
            started,
            &format!("{}; {error:?}", witness.why()),
        ),
    }
}
