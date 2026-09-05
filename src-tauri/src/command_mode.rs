//! Voice command mode: hold the command chord, speak an edit instruction, and
//! the text selected anywhere on screen is rewritten in place.
//!
//! Three decisions shape this module:
//!
//! 1. **The selection is an operand, not context.** Ambient context is what a
//!    mode may glance at while dictating, and [`crate::context::ContextPolicy`]
//!    governs it. A command chord means "operate on the text I have selected",
//!    which is its own per-invocation request, so the selection is read through
//!    [`crate::context::capture_selected_text`] and frozen into the run plan
//!    alongside the audio settings.
//! 2. **No selection means no recording.** The refusal happens while the plan is
//!    built, before the microphone opens, so a mistaken chord costs nothing and
//!    reports something the user can act on.
//! 3. **Nothing new delivers text.** A command rewrite is dispatched through the
//!    same [`crate::delivery`] path as dictation, whose Accessibility route
//!    already replaces the focused control's selection and whose clipboard
//!    fallback pastes over it.

use crate::actions::{post_process_transcription, ProcessedTranscription, RecordingErrorEvent};
use crate::modes::{CommandPlan, RunPlan, RunPlanError, TranscriptionIntent};
use crate::prompt_renderer::{render_instruction, InstructionRenderInput};
use log::{debug, warn};
use tauri::{AppHandle, Emitter};

/// The persisted binding this mode listens on. Command mode is one global
/// shortcut, not a per-mode chord: the operand comes from the screen, not from
/// the active mode.
pub const COMMAND_BINDING_ID: &str = "command";

/// The one user-visible refusal for a command chord pressed with nothing
/// selected. It carries no captured text, like every other `recording-error`.
const NO_SELECTION_ERROR: &str = "command_no_selection";
/// The rewrite produced nothing to deliver. The selection is deliberately left
/// alone: replacing it with the spoken instruction would destroy the user's
/// text, and replacing it with itself would hide the failure.
const REWRITE_UNAVAILABLE_ERROR: &str = "command_rewrite_unavailable";

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
    crate::settings::update_settings(&app, |settings| {
        settings.command_mode_enabled = enabled;
    })?;
    crate::shortcut::suspend_all_shortcuts(&app);
    crate::shortcut::resume_all_shortcuts(&app);
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

/// Applies the spoken instruction to the frozen selection and returns the text
/// delivery should replace it with. An empty `final_text` tells the caller to
/// dispatch nothing, which is the only non-destructive answer when the rewrite
/// cannot be performed.
pub(crate) async fn rewrite_selection(
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
    debug!(
        "Command prompt budget: {} of {} bytes (instruction truncated: {}, selection truncated: {})",
        rendered.budget_receipt.user_bytes,
        rendered.budget_receipt.user_budget_bytes,
        rendered.budget_receipt.transcript_truncated,
        rendered.budget_receipt.context_truncated
    );

    match post_process_transcription(app, run, &rendered, instruction).await {
        Some(rewritten) if !rewritten.trim().is_empty() => ProcessedTranscription {
            post_processed_text: Some(rewritten.clone()),
            final_text: rewritten,
        },
        _ => {
            warn!("Command rewrite produced no text; the selection was left unchanged");
            let _ = app.emit(
                "recording-error",
                RecordingErrorEvent::typed(REWRITE_UNAVAILABLE_ERROR),
            );
            ProcessedTranscription {
                final_text: String::new(),
                post_processed_text: None,
            }
        }
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
}
