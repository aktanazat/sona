//! Deterministic text delivery.
//!
//! Target apps do not give Sona an insertion acknowledgement for keyboard or
//! clipboard requests. This module therefore treats delivery as a one-shot
//! operation: it may fall back only before a dispatch, never after an uncertain
//! dispatch. The result is a typed receipt that history can persist without
//! claiming more certainty than the platform provides.

use crate::clipboard::{self, ClipboardDispatch};
use crate::modes::DeliveryPlan;
use crate::settings::{ClipboardHandling, PasteMethod};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMethod {
    #[default]
    None,
    AccessibilityInsertion,
    ClipboardPaste,
    DirectTyping,
    ExternalScript,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOutcome {
    /// The operating system confirmed the Accessibility set operation.
    Delivered,
    /// The selected mechanism did not issue any insertion/paste event.
    #[default]
    DefinitelyNotDispatched,
    /// Input was sent or may have reached the target, but the target cannot
    /// confirm it. Never retry this outcome.
    DispatchedButUnconfirmed,
    /// Dispatched through synthetic key events while macOS held Secure Event
    /// Input, which is the one condition under which those events are known
    /// to be dropped before any application sees them. Still unconfirmed —
    /// the platform never reports where a key event landed — but not the same
    /// unconfirmed as an ordinary paste, because here Sona knows the target
    /// was locked to exactly the mechanism it used. Never retry.
    DispatchedUnderSecureInput,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct DeliveryReceipt {
    pub method: DeliveryMethod,
    pub outcome: DeliveryOutcome,
    pub dispatched_at_ms: u64,
}

impl DeliveryReceipt {
    pub fn not_dispatched() -> Self {
        Self::new(
            DeliveryMethod::None,
            DeliveryOutcome::DefinitelyNotDispatched,
        )
    }

    fn new(method: DeliveryMethod, outcome: DeliveryOutcome) -> Self {
        Self {
            method,
            outcome,
            dispatched_at_ms: now_ms(),
        }
    }
}

/// Dispatches text from one frozen mode. Optional learning reads its own consent.
///
/// This function owns the two decisions every route shares: the exact text
/// that gets dispatched, and what still has to happen once a route dispatched
/// it. Routes below only move bytes.
pub fn deliver(app: &AppHandle, text: String, settings: &DeliveryPlan) -> DeliveryReceipt {
    let text = compose_final_text(text, settings);
    #[cfg(target_os = "macos")]
    let observation = if !settings.auto_submit
        && !matches!(
            settings.paste_method,
            PasteMethod::None | PasteMethod::ExternalScript
        ) {
        crate::context::macos::corrections::prepare(app, &text)
    } else {
        crate::context::macos::corrections::cancel();
        None
    };
    let receipt = dispatch(app, &text, settings);
    #[cfg(target_os = "macos")]
    if let Some(observation) = observation {
        let _ = observation.send(receipt.outcome);
    }
    receipt
}

fn dispatch(app: &AppHandle, text: &str, settings: &DeliveryPlan) -> DeliveryReceipt {
    let primary = primary_method(settings);
    #[cfg(target_os = "macos")]
    if should_prefer_accessibility(settings) {
        match crate::context::macos::insert_into_focused_editable(&text) {
            crate::context::macos::AccessibilityInsertion::Delivered => {
                finish_after_dispatch(app, settings, &text);
                return DeliveryReceipt::new(
                    DeliveryMethod::AccessibilityInsertion,
                    DeliveryOutcome::Delivered,
                );
            }
            crate::context::macos::AccessibilityInsertion::DispatchedButUnconfirmed => {
                // Same certainty class as a clipboard chord that returned
                // without a local error: the request left Sona. The user's
                // configured finishing applies to both.
                finish_after_dispatch(app, settings, &text);
                return DeliveryReceipt::new(
                    DeliveryMethod::AccessibilityInsertion,
                    DeliveryOutcome::DispatchedButUnconfirmed,
                );
            }
            crate::context::macos::AccessibilityInsertion::NotDispatched => {
                // Only this branch may use a fallback. The AX request did not
                // commit a value, so no duplicate text can result.
            }
        }
    }

    // Sampled before dispatch, because that is when the events go out and
    // Secure Event Input can end at any moment. Sona already runs this
    // monitor for its own shortcuts; asking it here is what keeps a
    // suppressed paste distinguishable from an ordinary unconfirmed one.
    let dispatched = dispatched_outcome(settings.paste_method, secure_input_holds_keys_now());

    let clipboard_dispatch = clipboard::paste_frozen(&text, app, settings);
    if clipboard_dispatch_owes_finishing(&clipboard_dispatch) {
        finish_after_dispatch(app, settings, &text);
    }

    match clipboard_dispatch {
        ClipboardDispatch::DefinitelyNotDispatched(error) => {
            log::warn!("Text delivery was not dispatched: {error}");
            DeliveryReceipt::new(primary, DeliveryOutcome::DefinitelyNotDispatched)
        }
        // Every state that reached a backend carries the same certainty: the
        // platform never reports where the insertion landed. What each of them
        // still owes the user was decided once, above.
        ClipboardDispatch::Dispatched
        | ClipboardDispatch::DispatchedWithBackendError
        | ClipboardDispatch::DispatchedAndFinished => DeliveryReceipt::new(primary, dispatched),
    }
}

/// Whether this route inserts text by posting synthetic key events, which is
/// what Secure Event Input suppresses. Accessibility insertion is not one of
/// them (it is a cross-process attribute write, and it refuses secure fields
/// on its own), and an external script owns whatever mechanism it chooses.
fn uses_synthetic_keys(method: PasteMethod) -> bool {
    is_clipboard_family(method) || method == PasteMethod::Direct
}

/// The receipt value for a route that has dispatched. The platform never says
/// where a key event landed, so neither of these claims delivery; they differ
/// in whether Sona knows the events were posted into a system that drops
/// them.
fn dispatched_outcome(method: PasteMethod, secure_input_active: bool) -> DeliveryOutcome {
    if secure_input_active && uses_synthetic_keys(method) {
        DeliveryOutcome::DispatchedUnderSecureInput
    } else {
        DeliveryOutcome::DispatchedButUnconfirmed
    }
}

/// Live Secure Event Input check. Only macOS has the state.
fn secure_input_holds_keys_now() -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::secure_input::is_enabled_now()
    }

    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Posts one line into whatever the frontmost application has focused, and
/// never anywhere else.
///
/// This is [`deliver`]'s Accessibility branch on its own, with no clipboard
/// route behind it. It is not a second delivery path: it is the one insertion
/// mechanism this module already prefers, asked for a case where a fallback
/// would be wrong.
///
/// A recording disclosure is text Sona puts in somebody else's chat. AX
/// insertion is the only route that asks the target whether it can accept text
/// at the caret and answers honestly; a clipboard chord would take the
/// operator's clipboard and press ⌘V at an application that may have no
/// composer open — into a document, a search field, or nothing. So a target that
/// cannot accept an insertion is a refusal to record, not a reason to try
/// harder.
pub fn announce(text: &str) -> DeliveryReceipt {
    #[cfg(target_os = "macos")]
    {
        match crate::context::macos::insert_into_focused_editable(text) {
            crate::context::macos::AccessibilityInsertion::Delivered => DeliveryReceipt::new(
                DeliveryMethod::AccessibilityInsertion,
                DeliveryOutcome::Delivered,
            ),
            crate::context::macos::AccessibilityInsertion::DispatchedButUnconfirmed => {
                DeliveryReceipt::new(
                    DeliveryMethod::AccessibilityInsertion,
                    DeliveryOutcome::DispatchedButUnconfirmed,
                )
            }
            crate::context::macos::AccessibilityInsertion::NotDispatched => {
                DeliveryReceipt::not_dispatched()
            }
        }
    }

    // No Accessibility API, so nothing can be told it is being recorded.
    #[cfg(not(target_os = "macos"))]
    {
        let _ = text;
        DeliveryReceipt::not_dispatched()
    }
}

/// The one place a trailing space is added, so every route dispatches the same
/// string and the receipt describes the same delivery.
fn compose_final_text(mut text: String, settings: &DeliveryPlan) -> String {
    if settings.append_trailing_space {
        text.push(' ');
    }
    text
}

fn primary_method(settings: &DeliveryPlan) -> DeliveryMethod {
    match settings.paste_method {
        PasteMethod::None => DeliveryMethod::None,
        PasteMethod::Direct => DeliveryMethod::DirectTyping,
        PasteMethod::ExternalScript => DeliveryMethod::ExternalScript,
        PasteMethod::CtrlV | PasteMethod::CtrlShiftV | PasteMethod::ShiftInsert => {
            DeliveryMethod::ClipboardPaste
        }
    }
}

/// Accessibility insertion is a better *implementation* of the clipboard-paste
/// intent: it writes at the caret, leaves the clipboard alone, and can confirm
/// the result. It is not a better implementation of the other methods —
/// `Direct` is chosen precisely for targets where paste and AX do not work
/// (terminals, VMs, remote desktops), and `ExternalScript` delegates insertion
/// entirely. Preferring AX for those would discard the user's explicit choice.
#[cfg(target_os = "macos")]
fn should_prefer_accessibility(settings: &DeliveryPlan) -> bool {
    is_clipboard_family(settings.paste_method)
}

fn is_clipboard_family(method: PasteMethod) -> bool {
    matches!(
        method,
        PasteMethod::CtrlV | PasteMethod::CtrlShiftV | PasteMethod::ShiftInsert
    )
}

/// What a dispatch state owes the user, in one place.
///
/// An exhaustive match, not a `matches!`: a new dispatch state has to state
/// its own answer here rather than inheriting a silent `false`.
fn clipboard_dispatch_owes_finishing(dispatch: &ClipboardDispatch) -> bool {
    match dispatch {
        // The one state that proves a route invoked its input backend.
        ClipboardDispatch::Dispatched => true,
        // Nothing was dispatched; or a backend error left the insertion
        // unproven, so no submit key and no clipboard rewrite may follow it;
        // or the reliable transaction already ran this mode's finishing,
        // timed against the target's own paste receipt.
        ClipboardDispatch::DefinitelyNotDispatched(_)
        | ClipboardDispatch::DispatchedWithBackendError
        | ClipboardDispatch::DispatchedAndFinished => false,
    }
}

/// What Sona still owes the user after a route dispatched text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Finishing {
    auto_submit: bool,
    copy_final_to_clipboard: bool,
}

/// Sona owes finishing exactly for the insertions it performs itself. `None`
/// dispatched nothing, and an external script owns everything after its own
/// insertion — pressing Enter or rewriting the clipboard behind it would fight
/// the script.
fn finishing_plan(settings: &DeliveryPlan) -> Finishing {
    let handy_inserted =
        is_clipboard_family(settings.paste_method) || settings.paste_method == PasteMethod::Direct;
    Finishing {
        auto_submit: handy_inserted && settings.auto_submit,
        copy_final_to_clipboard: handy_inserted
            && settings.clipboard_handling == ClipboardHandling::CopyToClipboard,
    }
}

/// Gives the target application time to process the insertion before the
/// submit key, so Enter cannot arrive ahead of the text.
const AUTO_SUBMIT_DELAY_MS: u64 = 50;

/// Runs the finishing steps for a dispatch that already happened. A failure
/// here is logged and never retried: the text is already on its way.
fn finish_after_dispatch(app: &AppHandle, settings: &DeliveryPlan, text: &str) {
    let finishing = finishing_plan(settings);

    if finishing.auto_submit {
        std::thread::sleep(std::time::Duration::from_millis(AUTO_SUBMIT_DELAY_MS));
        if let Err(error) = clipboard::send_auto_submit(app, settings.auto_submit_key) {
            log::warn!("Delivery dispatched, but auto-submit failed: {error}");
        }
    }
    if finishing.copy_final_to_clipboard {
        if let Err(error) = clipboard::write_text_to_clipboard(app, text) {
            log::warn!("Delivery dispatched, but copying final text failed: {error}");
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{AutoSubmitKey, TypingTool};

    const EVERY_METHOD: [PasteMethod; 6] = [
        PasteMethod::CtrlV,
        PasteMethod::CtrlShiftV,
        PasteMethod::ShiftInsert,
        PasteMethod::Direct,
        PasteMethod::ExternalScript,
        PasteMethod::None,
    ];

    fn plan(paste_method: PasteMethod) -> DeliveryPlan {
        DeliveryPlan {
            paste_method,
            clipboard_handling: ClipboardHandling::DontModify,
            auto_submit: false,
            auto_submit_key: AutoSubmitKey::Enter,
            append_trailing_space: false,
            paste_delay_ms: 0,
            paste_delay_after_ms: 0,
            reliable_paste: false,
            typing_tool: TypingTool::Auto,
            external_script_path: None,
        }
    }

    /// The H1 regression trap: Accessibility insertion replaces the clipboard
    /// chord only. `Direct` and `ExternalScript` are explicit mechanism
    /// choices and must reach their own backend.
    #[test]
    fn accessibility_replaces_only_the_clipboard_family() {
        assert!(is_clipboard_family(PasteMethod::CtrlV));
        assert!(is_clipboard_family(PasteMethod::CtrlShiftV));
        assert!(is_clipboard_family(PasteMethod::ShiftInsert));
        assert!(!is_clipboard_family(PasteMethod::Direct));
        assert!(!is_clipboard_family(PasteMethod::ExternalScript));
        assert!(!is_clipboard_family(PasteMethod::None));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn accessibility_preference_follows_the_clipboard_family() {
        for method in EVERY_METHOD {
            assert_eq!(
                should_prefer_accessibility(&plan(method)),
                is_clipboard_family(method),
                "accessibility preference for {method:?}"
            );
        }
    }

    #[test]
    fn trailing_space_is_appended_once_for_every_route() {
        for method in EVERY_METHOD {
            let mut settings = plan(method);
            settings.append_trailing_space = true;
            assert_eq!(
                compose_final_text("hello".to_string(), &settings),
                "hello ",
                "composed text for {method:?}"
            );

            settings.append_trailing_space = false;
            assert_eq!(
                compose_final_text("hello".to_string(), &settings),
                "hello",
                "composed text for {method:?}"
            );
        }
    }

    #[test]
    fn every_handy_insertion_finishes_the_configured_way() {
        for method in [
            PasteMethod::CtrlV,
            PasteMethod::CtrlShiftV,
            PasteMethod::ShiftInsert,
            PasteMethod::Direct,
        ] {
            let mut settings = plan(method);
            settings.auto_submit = true;
            settings.clipboard_handling = ClipboardHandling::CopyToClipboard;
            assert_eq!(
                finishing_plan(&settings),
                Finishing {
                    auto_submit: true,
                    copy_final_to_clipboard: true,
                },
                "finishing for {method:?}"
            );
        }
    }

    #[test]
    fn external_script_and_disabled_delivery_never_finish_in_handy() {
        for method in [PasteMethod::ExternalScript, PasteMethod::None] {
            let mut settings = plan(method);
            settings.auto_submit = true;
            settings.clipboard_handling = ClipboardHandling::CopyToClipboard;
            assert_eq!(
                finishing_plan(&settings),
                Finishing {
                    auto_submit: false,
                    copy_final_to_clipboard: false,
                },
                "finishing for {method:?}"
            );
        }
    }

    #[test]
    fn finishing_stays_off_when_the_user_disabled_it() {
        for method in EVERY_METHOD {
            assert_eq!(
                finishing_plan(&plan(method)),
                Finishing {
                    auto_submit: false,
                    copy_final_to_clipboard: false,
                },
                "finishing for {method:?}"
            );
        }
    }
    #[test]
    fn only_a_dispatched_clipboard_route_earns_coordinator_finishing() {
        assert!(clipboard_dispatch_owes_finishing(
            &ClipboardDispatch::Dispatched
        ));
        assert!(!clipboard_dispatch_owes_finishing(
            &ClipboardDispatch::DefinitelyNotDispatched("refused".to_string())
        ));
        assert!(!clipboard_dispatch_owes_finishing(
            &ClipboardDispatch::DispatchedWithBackendError
        ));
        assert!(!clipboard_dispatch_owes_finishing(
            &ClipboardDispatch::DispatchedAndFinished
        ));
    }

    /// A dictation that vanishes into a password field used to produce the
    /// same receipt as one that landed. Secure Event Input is the one thing
    /// Sona can observe about that, and only the routes it actually
    /// suppresses may carry the distinction.
    #[test]
    fn secure_input_separates_a_suppressed_dispatch_from_an_ordinary_one() {
        for method in EVERY_METHOD {
            assert_eq!(
                dispatched_outcome(method, false),
                DeliveryOutcome::DispatchedButUnconfirmed,
                "no secure input, {method:?}"
            );
        }

        for method in [
            PasteMethod::CtrlV,
            PasteMethod::CtrlShiftV,
            PasteMethod::ShiftInsert,
            PasteMethod::Direct,
        ] {
            assert_eq!(
                dispatched_outcome(method, true),
                DeliveryOutcome::DispatchedUnderSecureInput,
                "secure input, {method:?}"
            );
        }

        // An external script owns its own insertion mechanism, and `None`
        // dispatched nothing, so neither may blame Secure Input.
        for method in [PasteMethod::ExternalScript, PasteMethod::None] {
            assert_eq!(
                dispatched_outcome(method, true),
                DeliveryOutcome::DispatchedButUnconfirmed,
                "secure input, {method:?}"
            );
        }
    }

    /// A receipt is persisted history. Any new field carrying transcript text
    /// would leak it into the delivery record, so the field set is pinned.
    #[test]
    fn receipt_serializes_exactly_the_content_free_field_set() {
        let receipt = DeliveryReceipt::new(
            DeliveryMethod::ClipboardPaste,
            DeliveryOutcome::DispatchedButUnconfirmed,
        );
        let serialized = serde_json::to_value(&receipt).expect("receipt serializes");
        let mut fields = serialized
            .as_object()
            .expect("receipt is a JSON object")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        fields.sort();

        assert_eq!(fields, ["dispatched_at_ms", "method", "outcome"]);
    }
}
