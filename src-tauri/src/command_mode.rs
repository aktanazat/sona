//! Voice command mode: hold the command chord, speak an edit instruction, and
//! the text selected anywhere on screen is rewritten in place — or speak a
//! question, and the answer lands in Sona's chat.
//!
//! Four decisions shape this module:
//!
//! 1. **The selection is an operand, not context.** Ambient context is what a
//!    mode may glance at while dictating, and [`crate::context::ContextPolicy`]
//!    governs it. A command chord means "operate on the text I have selected",
//!    which is its own per-invocation request, so the selection is read through
//!    [`crate::context::capture_selected_text`] and frozen into the run plan
//!    alongside the audio settings.
//! 2. **An edit with no selection means no recording.** The refusal happens
//!    while the plan is built, before the microphone opens, so a mistaken chord
//!    costs nothing and reports something the user can act on. With answers
//!    routed to the chat, a chord pressed with nothing selected records a
//!    question instead: there is nothing to edit, so nothing can be damaged.
//! 3. **Nothing new delivers text.** A command rewrite is dispatched through the
//!    same [`crate::delivery`] path as dictation, whose Accessibility route
//!    already replaces the focused control's selection and whose clipboard
//!    fallback pastes over it.
//! 4. **A question is answered where it can be read, not typed.** Typing an
//!    answer over the selection would destroy the text it was about, so with
//!    `command_answers_in_chat` on a question becomes a conversation in the
//!    chat sheet — question, answer, or the failure that stood in for one —
//!    and the shell brings that sheet forward. Edits are untouched by the
//!    toggle; with it off every command is an edit, exactly as before.

use crate::actions::{post_process_transcription, ProcessedTranscription, RecordingErrorEvent};
use crate::agent_panel::protocol::{
    SonaAgentChatOutcomeV1, SonaAgentChatRoleV1, SonaAgentChatTurnV1,
};
use crate::agent_panel::{AgentPanelManager, AgentPanelTurnFailureV1};
use crate::context::TargetMetadata;
use crate::modes::{CommandPlan, RewriteOutcome, RunPlan, RunPlanError, TranscriptionIntent};
use crate::prompt_renderer::{render_instruction, InstructionRenderInput, RenderedPrompt};
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Emitter, Manager};
use tauri_specta::Event as _;

/// The persisted binding this mode listens on. Command mode is one global
/// shortcut, not a per-mode chord: the operand comes from the screen, not from
/// the active mode.
pub const COMMAND_BINDING_ID: &str = "command";

/// The one user-visible refusal for an edit spoken with nothing selected. It
/// carries no captured text, like every other `recording-error`.
const NO_SELECTION_ERROR: &str = "command_no_selection";
/// The rewrite produced nothing to deliver. The selection is deliberately left
/// alone: replacing it with the spoken instruction would destroy the user's
/// text, and replacing it with itself would hide the failure.
const REWRITE_UNAVAILABLE_ERROR: &str = "command_rewrite_unavailable";

/// The system message a spoken question is answered under. It shares the
/// command envelope, so the selection reaches the model as `input` and never
/// as an instruction.
const COMMAND_ANSWER_PROMPT: &str = include_str!("../resources/prompts/command_answer.txt");

/// How much of a selection is quoted under the question in the chat. The model
/// reads the whole selection within the prompt budget; the thread only needs
/// enough to say what the question was about.
const QUOTED_SELECTION_MAX_CHARS: usize = 600;

/// Announces a spoken question filed in the chat, so the shell can bring the
/// chat sheet forward. The exchange is already on the panel and on disk when
/// this is emitted, so the sheet has nothing to fetch by id.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct CommandAnsweredEvent;

impl tauri_specta::Event for CommandAnsweredEvent {
    const NAME: &'static str = "command-mode://answered";
}

/// Turn the command chord on or off.
///
/// Shaped like every other `change_*_setting` command so the regenerated
/// binding drops straight into `settingUpdaters` in `settingsStore.ts`. The
/// re-registration is the point: [`crate::shortcut::bindings_for_registration`]
/// filters this binding out while the flag is false, so without resuming here
/// the chord would keep firing until the next launch (or stay dead after being
/// switched back on).
#[tauri::command]
#[specta::specta]
pub fn change_command_mode_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    // The flag is in the store's memory whether or not the disk took it, and
    // registration reads that memory, so the chord follows it before a refused
    // write is reported.
    let persisted = crate::settings::update_settings(&app, |settings| {
        settings.command_mode_enabled = enabled;
    });
    crate::shortcut::suspend_all_shortcuts(&app);
    crate::shortcut::resume_all_shortcuts(&app);
    persisted?;
    Ok(())
}

/// Choose where a spoken question goes: the chat sheet, or the selection as
/// one more edit. Read when a command run is frozen, so a mid-recording flip
/// cannot change where the answer in flight lands.
#[tauri::command]
#[specta::specta]
pub fn change_command_answers_in_chat_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    crate::settings::update_settings(&app, |settings| {
        settings.command_answers_in_chat = enabled;
    })?;
    Ok(())
}

/// The typed error a refused command chord reports, or `None` when the
/// rejection is not something the user caused by pressing it.
fn refusal_error_type(error: &RunPlanError) -> Option<&'static str> {
    match error {
        RunPlanError::CommandWithoutSelection => Some(NO_SELECTION_ERROR),
        // A command is a rewrite by definition, so an unusable rewrite provider
        // is the same dead end as a rewrite that returned nothing.
        RunPlanError::MissingPostProcessProvider
        | RunPlanError::InvalidPostProcessDestination
        | RunPlanError::PostProcessConsentRequired => Some(REWRITE_UNAVAILABLE_ERROR),
        RunPlanError::NoMatchingMode
        | RunPlanError::CloudConsentRequired { .. }
        | RunPlanError::CloudPrivacyConsentRequired { .. }
        | RunPlanError::CloudTimestampsRequired { .. }
        | RunPlanError::CloudFallbackModelRequired { .. } => None,
    }
}

/// Reports a plan that never opened the microphone, for the refusals a user has
/// to see. Only a command chord produces those: every other intent's rejection
/// stays a log line, exactly as before.
pub(crate) fn report_refused_run(
    app: &AppHandle,
    intent: &TranscriptionIntent,
    error: &RunPlanError,
) {
    if !matches!(intent, TranscriptionIntent::Command) {
        return;
    }
    if let Some(error_type) = refusal_error_type(error) {
        let _ = app.emit("recording-error", RecordingErrorEvent::typed(error_type));
    }
}

/// What a spoken command asks for, as far as its wording says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpokenIntent {
    /// Text to put back in place of the selection: "make this shorter",
    /// "fix the grammar".
    Edit,
    /// Something to read: "what does this mean", "summarize this",
    /// "is this grammatically correct".
    Question,
    /// Wording that names neither. With a selection it is an edit, as every
    /// command was before the chat route existed; with nothing selected it
    /// is refused rather than guessed to be a question.
    Unclear,
}

/// Openers that carry no intent of their own. Stripped before the first word
/// is read, so "can you make it shorter" is the edit "make it shorter" and not
/// a question because it starts with "can".
const COURTESY_OPENERS: &[&str] = &[
    "hey sona",
    "sona",
    "please",
    "can you",
    "could you",
    "would you",
    "will you",
    "i want you to",
    "i'd like you to",
    "i would like you to",
    "go ahead and",
    "just",
];

/// A first word that asks for the selection back, changed. Checked before the
/// question openers, so "make this a question" stays an edit. Only words that
/// are edits whatever follows belong here: "number", "quote" and "check" can
/// open a question as easily, and an unclear wording with a selection is an
/// edit anyway.
const EDIT_VERBS: &[&str] = &[
    "rewrite",
    "reword",
    "rephrase",
    "paraphrase",
    "revise",
    "edit",
    "make",
    "change",
    "turn",
    "convert",
    "fix",
    "correct",
    "proofread",
    "spell",
    "punctuate",
    "shorten",
    "condense",
    "tighten",
    "trim",
    "cut",
    "expand",
    "lengthen",
    "simplify",
    "clarify",
    "improve",
    "polish",
    "clean",
    "translate",
    "format",
    "reformat",
    "capitalize",
    "capitalise",
    "uppercase",
    "lowercase",
    "bold",
    "italicize",
    "italicise",
    "indent",
    "bullet",
    "sort",
    "reorder",
    "rearrange",
    "reverse",
    "split",
    "merge",
    "combine",
    "join",
    "remove",
    "delete",
    "strip",
    "add",
    "insert",
    "replace",
    "swap",
    "put",
    "wrap",
    "adjust",
    "tweak",
    "update",
];

/// Openers that ask for something to read rather than text to put back. Each
/// is matched at a word boundary: "is" opens "is this right?", not "island".
const QUESTION_OPENERS: &[&str] = &[
    "what",
    "why",
    "how",
    "who",
    "whom",
    "whose",
    "when",
    "where",
    "which",
    "is",
    "are",
    "am",
    "was",
    "were",
    "does",
    "did",
    "should",
    "shall",
    "will",
    "would",
    "could",
    "can",
    "may",
    "might",
    "must",
    "explain",
    "summarize",
    "summarise",
    "sum up",
    "describe",
    "define",
    "tell me",
    "give me",
    "show me",
    "list",
    "count",
    "compare",
    "calculate",
    "compute",
    "check",
    "find",
    "search",
    "look up",
    "identify",
    "suggest",
    "recommend",
    "brainstorm",
    "answer",
    "help me",
    "any",
    "anything",
];

/// Whether `text` opens with `phrase` as whole words.
fn opens_with(text: &str, phrase: &str) -> bool {
    text.strip_prefix(phrase).is_some_and(|rest| {
        rest.chars()
            .next()
            .is_none_or(|next| !next.is_alphanumeric())
    })
}

fn strip_courtesy(mut text: &str) -> &str {
    loop {
        text = text.trim_start_matches(|character: char| {
            character.is_whitespace() || matches!(character, ',' | '.' | '!' | '-' | ':')
        });
        let Some(opener) = COURTESY_OPENERS
            .iter()
            .find(|opener| opens_with(text, opener))
        else {
            return text;
        };
        text = &text[opener.len()..];
    }
}

/// Read the intent off the wording alone. Deterministic on purpose: the
/// decision between editing a selection and answering about it is made before
/// any model is asked, and a model asked to edit must never answer instead.
fn classify_instruction(instruction: &str) -> SpokenIntent {
    let lowered = instruction.to_lowercase();
    let text = strip_courtesy(lowered.trim());
    let first_word = text
        .split(|character: char| !character.is_alphabetic())
        .next()
        .unwrap_or("");
    if EDIT_VERBS.contains(&first_word) {
        return SpokenIntent::Edit;
    }
    if text.ends_with('?')
        || QUESTION_OPENERS
            .iter()
            .any(|opener| opens_with(text, opener))
    {
        return SpokenIntent::Question;
    }
    SpokenIntent::Unclear
}

/// Where one command run goes once its words are in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandRoute {
    /// The selection is replaced by the rewrite, as every command was before
    /// the chat route existed.
    Rewrite,
    /// The words are answered into the chat and nothing is typed.
    Answer,
    /// An edit or unclear instruction was spoken with nothing selected.
    /// Nothing to change: the same refusal the chord gives before recording
    /// when the chat route is off.
    NothingToEdit,
}

fn route(command: &CommandPlan, instruction: &str) -> CommandRoute {
    if !command.answers_in_chat() {
        return CommandRoute::Rewrite;
    }
    match (command.has_selection(), classify_instruction(instruction)) {
        (true, SpokenIntent::Question) => CommandRoute::Answer,
        (true, SpokenIntent::Edit | SpokenIntent::Unclear) => CommandRoute::Rewrite,
        (false, SpokenIntent::Edit | SpokenIntent::Unclear) => CommandRoute::NothingToEdit,
        (false, SpokenIntent::Question) => CommandRoute::Answer,
    }
}

/// Carries out the spoken words of one command run and returns what delivery
/// should type. An empty `final_text` tells the caller to dispatch nothing,
/// which is the only non-destructive answer when the words were a question or
/// the rewrite could not be performed.
pub(crate) async fn process_command(
    app: &AppHandle,
    run: &RunPlan,
    command: &CommandPlan,
    instruction: &str,
    language: &str,
) -> ProcessedTranscription {
    match route(command, instruction) {
        CommandRoute::Rewrite => rewrite_selection(app, run, command, instruction, language).await,
        CommandRoute::Answer => answer_in_chat(app, run, command, instruction, language).await,
        CommandRoute::NothingToEdit => {
            debug!("Command edit spoken with nothing selected; nothing was changed");
            let _ = app.emit(
                "recording-error",
                RecordingErrorEvent::typed(NO_SELECTION_ERROR),
            );
            // No provider was asked anything: the row in the Library keeps the
            // spoken words and says no processing produced text.
            ProcessedTranscription {
                final_text: String::new(),
                post_processed_text: None,
                rewrite: RewriteOutcome::NotRequested,
            }
        }
    }
}

/// Applies the spoken instruction to the frozen selection and returns the text
/// delivery should replace it with.
async fn rewrite_selection(
    app: &AppHandle,
    run: &RunPlan,
    command: &CommandPlan,
    instruction: &str,
    language: &str,
) -> ProcessedTranscription {
    let rendered = render_instruction(InstructionRenderInput {
        instruction,
        input: command.selection(),
        language,
        target: run.context().target(),
    });
    log_prompt_budget("Command", &rendered);

    match post_process_transcription(app, run, &rendered, instruction).await {
        Ok(rewritten) => ProcessedTranscription {
            post_processed_text: Some(rewritten.clone()),
            final_text: rewritten,
            rewrite: RewriteOutcome::Applied,
        },
        Err(outcome) => {
            warn!(
                "Command rewrite produced no text ({outcome:?}); the selection was left unchanged"
            );
            let _ = app.emit(
                "recording-error",
                RecordingErrorEvent::typed(REWRITE_UNAVAILABLE_ERROR),
            );
            ProcessedTranscription {
                final_text: String::new(),
                post_processed_text: None,
                rewrite: outcome,
            }
        }
    }
}

/// Answers the spoken question through the run's rewrite provider and files
/// the exchange in the chat. Nothing is typed either way: the answer, or the
/// failure that stood in for it, is read in the chat sheet.
async fn answer_in_chat(
    app: &AppHandle,
    run: &RunPlan,
    command: &CommandPlan,
    question: &str,
    language: &str,
) -> ProcessedTranscription {
    // Words the model heard as nothing are not a question; a blank user turn
    // carrying a failure would be the chat's way of saying so, and it is not.
    if question.trim().is_empty() {
        debug!("Command question was blank; nothing was asked");
        return ProcessedTranscription {
            final_text: String::new(),
            post_processed_text: None,
            rewrite: RewriteOutcome::NotRequested,
        };
    }
    let rendered = render_answer(
        question,
        command.selection(),
        language,
        run.context().target(),
    );
    log_prompt_budget("Command answer", &rendered);

    let answered = post_process_transcription(app, run, &rendered, question).await;
    let (rewrite, post_processed_text) = match &answered {
        Ok(answer) => (RewriteOutcome::Applied, Some(answer.clone())),
        Err(outcome) => {
            warn!("Command question got no answer ({outcome:?}); the failure is filed in the chat");
            (*outcome, None)
        }
    };
    let turns = exchange(question, command.selection(), answered);

    let conversation_id = format!("command-{}-{}", run.run_id, run.run_started_at_ms);
    match app.try_state::<AgentPanelManager>() {
        Some(panel) => {
            panel.record_exchange(&conversation_id, turns);
            let _ = CommandAnsweredEvent.emit(app);
        }
        None => warn!("Command answer dropped: the agent panel is not managed"),
    }

    ProcessedTranscription {
        final_text: String::new(),
        post_processed_text,
        rewrite,
    }
}

/// The chat pair for one spoken question: the command envelope, so the
/// selection is `input` and never an instruction, under the answer prompt.
fn render_answer(
    question: &str,
    selection: &str,
    language: &str,
    target: &TargetMetadata,
) -> RenderedPrompt {
    let mut rendered = render_instruction(InstructionRenderInput {
        instruction: question,
        input: selection,
        language,
        target,
    });
    rendered.system_message = COMMAND_ANSWER_PROMPT.to_string();
    rendered
}

fn log_prompt_budget(label: &str, rendered: &RenderedPrompt) {
    debug!(
        "{label} prompt budget: {} of {} bytes (instruction truncated: {}, selection truncated: {})",
        rendered.budget_receipt.user_bytes,
        rendered.budget_receipt.user_budget_bytes,
        rendered.budget_receipt.transcript_truncated,
        rendered.budget_receipt.context_truncated
    );
}

/// The turns one spoken question leaves in the chat: the question, with the
/// selection it was about quoted under it, then the answer — or the question
/// alone, carrying the failure, which the sheet draws as an error with a
/// retry.
fn exchange(
    question: &str,
    selection: &str,
    answer: Result<String, RewriteOutcome>,
) -> Vec<SonaAgentChatTurnV1> {
    let mut asked = question.trim().to_string();
    if !selection.is_empty() {
        asked.push_str("\n\nSelected text:\n");
        asked.push_str(&quoted_selection(selection));
    }
    match answer {
        Ok(answer) => vec![
            SonaAgentChatTurnV1 {
                role: SonaAgentChatRoleV1::User,
                message: asked,
                outcome: None,
            },
            SonaAgentChatTurnV1 {
                role: SonaAgentChatRoleV1::Assistant,
                message: answer,
                outcome: None,
            },
        ],
        Err(outcome) => vec![SonaAgentChatTurnV1 {
            role: SonaAgentChatRoleV1::User,
            message: asked,
            outcome: Some(SonaAgentChatOutcomeV1::Failure {
                failure: chat_failure(outcome),
            }),
        }],
    }
}

/// The selection as the chat quotes it: whole when short, cut at a character
/// boundary with an ellipsis when long.
fn quoted_selection(selection: &str) -> String {
    let selection = selection.trim();
    match selection.char_indices().nth(QUOTED_SELECTION_MAX_CHARS) {
        Some((cut, _)) => format!("{}…", selection[..cut].trim_end()),
        None => selection.to_string(),
    }
}

/// The chat's word for how a rewrite provider let a question down. A
/// destination that cannot be used is the provider out of reach; everything
/// else is an answer that did not come.
fn chat_failure(outcome: RewriteOutcome) -> AgentPanelTurnFailureV1 {
    match outcome {
        RewriteOutcome::Unavailable | RewriteOutcome::NoCredential => {
            AgentPanelTurnFailureV1::Unreachable
        }
        RewriteOutcome::NotRequested
        | RewriteOutcome::Applied
        | RewriteOutcome::TooLong
        | RewriteOutcome::Failed => AgentPanelTurnFailureV1::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::{CommandPlan, TranscriptionIntent};
    use crate::settings::get_default_settings;

    fn plan(selection: &str) -> CommandPlan {
        CommandPlan::new(selection.to_string())
    }

    #[test]
    fn the_command_chord_resolves_to_the_command_intent() {
        assert_eq!(
            TranscriptionIntent::from_binding(COMMAND_BINDING_ID),
            Some(TranscriptionIntent::Command)
        );
        assert_eq!(
            TranscriptionIntent::Command.recording_id(),
            COMMAND_BINDING_ID
        );
    }

    #[test]
    fn the_command_binding_ships_enabled_with_its_own_chord() {
        let settings = get_default_settings();
        let binding = settings
            .bindings
            .get(COMMAND_BINDING_ID)
            .expect("the command binding ships by default");
        assert!(settings.command_mode_enabled);
        assert_ne!(
            binding.current_binding,
            settings.bindings["transcribe"].current_binding
        );
    }

    /// The operand is frozen before the microphone opens, so rendering consumes
    /// the selection captured at command start rather than consulting the
    /// frontmost application again.
    #[test]
    fn the_rewrite_uses_the_selection_frozen_at_record_start() {
        let frozen = plan("the original selection");

        let rendered = render_instruction(InstructionRenderInput {
            instruction: "shorten it",
            input: frozen.selection(),
            language: "en",
            target: &crate::context::TargetMetadata::default(),
        });

        let envelope: serde_json::Value = serde_json::from_str(&rendered.user_message).unwrap();
        assert_eq!(envelope["input"], "the original selection");
        // Rendering is the only consumer, and it cannot write back.
        assert_eq!(frozen.selection(), "the original selection");
    }

    /// The mode layer rejects a missing explicit operand before any ambient
    /// context capture. This module maps that rejection to the typed event.
    #[test]
    fn a_command_run_without_a_selection_has_a_typed_refusal() {
        assert_eq!(
            refusal_error_type(&RunPlanError::CommandWithoutSelection),
            Some(NO_SELECTION_ERROR)
        );
    }

    /// An unusable rewrite provider is the command chord's other dead end, and
    /// it gets its own copy. Everything the user did not cause by pressing the
    /// chord stays silent, exactly as before this mode existed.
    #[test]
    fn only_refusals_the_chord_caused_are_reported() {
        assert_eq!(
            refusal_error_type(&RunPlanError::PostProcessConsentRequired),
            Some("command_rewrite_unavailable")
        );
        assert_eq!(
            refusal_error_type(&RunPlanError::MissingPostProcessProvider),
            Some("command_rewrite_unavailable")
        );
        assert_eq!(refusal_error_type(&RunPlanError::NoMatchingMode), None);
        assert_eq!(
            refusal_error_type(&RunPlanError::CloudConsentRequired {
                provider: crate::modes::CloudSttProvider::DeepgramNova3
            }),
            None
        );
    }

    #[test]
    fn the_refusal_names_what_the_user_has_to_do() {
        assert_eq!(
            RunPlanError::CommandWithoutSelection.to_string(),
            "Voice command mode needs text selected before you speak"
        );
    }

    /// The wording decides before any model is asked, so the cases here are
    /// the contract: a question phrased politely is still a question, an edit
    /// asked as a favour is still an edit, and a trailing question mark cannot
    /// turn an edit into an answer.
    #[test]
    fn spoken_wording_classifies_questions_apart_from_edits() {
        let questions = [
            "what does this mean?",
            "What's the capital of France",
            "can this be shorter",
            "Explain this in simple terms",
            "Sona, summarize this",
            "please tell me how many words this is",
            "is this grammatically correct",
            "how do I say this in French",
            "does this sound formal",
            "could you give me three alternatives",
            "any typos in here?",
            "in French?",
        ];
        for question in questions {
            assert_eq!(
                classify_instruction(question),
                SpokenIntent::Question,
                "{question:?}"
            );
        }

        let edits = [
            "make this shorter",
            "Can you make it more formal?",
            "please rewrite this as a haiku",
            "fix the grammar",
            "translate this to French",
            "could you please shorten it?",
            "hey sona, turn this into a list",
            "make this a question",
        ];
        for edit in edits {
            assert_eq!(classify_instruction(edit), SpokenIntent::Edit, "{edit:?}");
        }

        let unclear = [
            "",
            "in French",
            "as a haiku",
            "write a limerick about cats",
            "island",
        ];
        for wording in unclear {
            assert_eq!(
                classify_instruction(wording),
                SpokenIntent::Unclear,
                "{wording:?}"
            );
        }
    }

    /// With the toggle off nothing about a command changes. With it on, a
    /// selection is rewritten unless the wording is a question. Without a
    /// selection, only an explicit question can be answered.
    #[test]
    fn the_toggle_and_the_selection_decide_where_a_command_goes() {
        let edits_only = plan("some text");
        assert_eq!(route(&edits_only, "what is this?"), CommandRoute::Rewrite);
        assert_eq!(route(&edits_only, "make it shorter"), CommandRoute::Rewrite);
        assert_eq!(route(&edits_only, "as a haiku"), CommandRoute::Rewrite);

        let selected = CommandPlan::answering_in_chat("some text".to_string());
        assert_eq!(route(&selected, "what is this?"), CommandRoute::Answer);
        assert_eq!(route(&selected, "make it shorter"), CommandRoute::Rewrite);
        assert_eq!(route(&selected, "as a haiku"), CommandRoute::Rewrite);

        let nothing_selected = CommandPlan::answering_in_chat(String::new());
        assert_eq!(
            route(&nothing_selected, "what is the capital of France?"),
            CommandRoute::Answer
        );
        assert_eq!(
            route(&nothing_selected, "write a limerick about cats"),
            CommandRoute::NothingToEdit
        );
        assert_eq!(
            route(&nothing_selected, "as a haiku"),
            CommandRoute::NothingToEdit
        );
        assert_eq!(route(&nothing_selected, ""), CommandRoute::NothingToEdit);
        assert_eq!(
            route(&nothing_selected, "make it shorter"),
            CommandRoute::NothingToEdit
        );
    }

    /// The chat reads the outcome off the shape of the turns: an answer is an
    /// assistant turn after the question, a failure is the question alone
    /// carrying it. A provider that cannot be reached and one that answered
    /// nothing are different things for the reader to do.
    #[test]
    fn an_exchange_is_shaped_for_the_chat() {
        let answered = exchange("what is this?", "", Ok("A greeting.".to_string()));
        assert_eq!(answered.len(), 2);
        assert_eq!(answered[0].role, SonaAgentChatRoleV1::User);
        assert_eq!(answered[0].message, "what is this?");
        assert_eq!(answered[0].outcome, None);
        assert_eq!(answered[1].role, SonaAgentChatRoleV1::Assistant);
        assert_eq!(answered[1].message, "A greeting.");

        let failed = exchange("what is this?", "hello", Err(RewriteOutcome::NoCredential));
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].message, "what is this?\n\nSelected text:\nhello");
        assert_eq!(
            failed[0].outcome,
            Some(SonaAgentChatOutcomeV1::Failure {
                failure: AgentPanelTurnFailureV1::Unreachable
            })
        );
        assert_eq!(
            chat_failure(RewriteOutcome::Failed),
            AgentPanelTurnFailureV1::Failed
        );
    }

    #[test]
    fn a_long_selection_is_quoted_short_and_on_a_character_boundary() {
        let selection = "é".repeat(QUOTED_SELECTION_MAX_CHARS + 40);
        let quoted = quoted_selection(&selection);
        assert_eq!(quoted.chars().count(), QUOTED_SELECTION_MAX_CHARS + 1);
        assert!(quoted.ends_with('…'));
        assert_eq!(quoted_selection("  short  "), "short");
    }
}
