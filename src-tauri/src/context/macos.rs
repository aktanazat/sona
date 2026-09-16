//! macOS Accessibility-backed context capture.
//!
//! AX calls cross into a different application process. This module is called
//! only from a capture worker or from the thread about to consume the context,
//! and every read is bounded twice: by its own message timeout, and by the
//! stage's remaining budget ([`super::deadline::CaptureDeadline`]). An
//! unresponsive target therefore degrades the sources it owns instead of
//! starving the whole capture or delaying the recording hotkey.

pub(crate) mod corrections;

use super::deadline::{CaptureDeadline, ReadBudget};
use super::{
    clipboard_recency, website_host_from_url, AccessibilityAccess, ApplicationCapture,
    CaptureOptions, ContextPolicy, ContextSourceStatus, SelectionCapture, SourceOutcome,
    StartCapture, WebsiteHostCapture,
};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString, NSWorkspace};
use objc2_application_services::{AXError, AXIsProcessTrusted, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{
    CFGetTypeID, CFRange, CFRetained, CFString, CFType, ConcreteType, CFURL,
};
use std::ptr::{self, NonNull};
use std::time::Duration;

const AX_FOCUSED_APPLICATION: &str = "AXFocusedApplication";
const AX_FOCUSED_UI_ELEMENT: &str = "AXFocusedUIElement";
const AX_FOCUSED_WINDOW: &str = "AXFocusedWindow";
const AX_PARENT: &str = "AXParent";
const AX_ROLE: &str = "AXRole";
const AX_SUBROLE: &str = "AXSubrole";
const AX_TITLE: &str = "AXTitle";
const AX_DESCRIPTION: &str = "AXDescription";
const AX_VALUE: &str = "AXValue";
const AX_SELECTED_TEXT: &str = "AXSelectedText";
const AX_URL: &str = "AXURL";
const AX_SECURE_TEXT_FIELD: &str = "AXSecureTextField";
const MAX_ANCESTORS: usize = 8;

enum AccessibilityAttributeValue {
    Text(CFRetained<CFString>),
    Url(CFRetained<CFURL>),
    Element(CFRetained<AXUIElement>),
    Range(CFRange),
    Other,
}

impl AccessibilityAttributeValue {
    fn from_copied(value: CFRetained<CFType>) -> Self {
        let type_id = CFGetTypeID(Some(value.as_ref()));
        if type_id == CFString::type_id() {
            // SAFETY: CFGetTypeID proved that this retained Core Foundation value is exactly a CFString.
            let text: CFRetained<CFString> = unsafe { CFRetained::cast_unchecked(value) };
            return Self::Text(text);
        }
        if type_id == CFURL::type_id() {
            // SAFETY: CFGetTypeID proved that this retained Core Foundation value is exactly a CFURL.
            let url: CFRetained<CFURL> = unsafe { CFRetained::cast_unchecked(value) };
            return Self::Url(url);
        }
        if type_id == AXUIElement::type_id() {
            // SAFETY: CFGetTypeID proved that this retained Core Foundation value is exactly an AXUIElement.
            let element: CFRetained<AXUIElement> = unsafe { CFRetained::cast_unchecked(value) };
            return Self::Element(element);
        }
        if type_id == AXValue::type_id() {
            // SAFETY: the type ID above proves this is an AXValue.
            let value: CFRetained<AXValue> = unsafe { CFRetained::cast_unchecked(value) };
            let mut range = CFRange::new(0, 0);
            // SAFETY: the output points to a live CFRange of the requested type.
            if unsafe { value.value(AXValueType::CFRange, NonNull::from(&mut range).cast()) } {
                return Self::Range(range);
            }
        }
        Self::Other
    }
}

pub(crate) fn accessibility_access() -> AccessibilityAccess {
    // No options is the documented non-prompting Accessibility check. Permission
    // prompting remains an explicit user action in the existing onboarding UI.
    // SAFETY: AXIsProcessTrusted takes no caller-owned pointers and only reads the process accessibility state.
    if unsafe { AXIsProcessTrusted() } {
        AccessibilityAccess::Granted
    } else {
        AccessibilityAccess::Denied
    }
}

/// Reads the sources whose value is only true at record start: the selection the
/// user is looking at, and a clipboard copy inside the pre-roll window.
pub(crate) fn read_start(
    policy: ContextPolicy,
    options: CaptureOptions,
    clipboard_generation: clipboard_recency::Generation,
    deadline: CaptureDeadline,
) -> StartCapture {
    let accessibility = accessibility_access();
    let mut start = StartCapture {
        accessibility,
        ..StartCapture::default()
    };

    if policy.wants_clipboard() {
        start.clipboard = read_recent_clipboard(clipboard_generation, options.clipboard_preroll_ms);
    }

    if !policy.wants_selection() {
        return start;
    }

    if accessibility != AccessibilityAccess::Granted {
        start.selected_text = SourceOutcome::Unavailable(ContextSourceStatus::PermissionDenied);
        return start;
    }

    start.selected_text = match focused_element(deadline) {
        Ok(element) => read_selection(&element, deadline),
        Err(status) => SourceOutcome::Unavailable(status),
    };
    start
}

/// Reads the frontmost application and the control the text is about to land in.
/// Called immediately before the step that consumes the context: switching
/// windows between record start and this read deliberately changes the answer.
pub(crate) fn read_application(
    policy: ContextPolicy,
    options: CaptureOptions,
    captured_at_ms: u64,
    deadline: CaptureDeadline,
) -> ApplicationCapture {
    let (application_name, application_identifier) = frontmost_application();
    let target = SourceOutcome::read(
        application_identifier
            .clone()
            .or_else(|| application_name.clone()),
    );
    let mut capture = ApplicationCapture {
        captured_at_ms: Some(captured_at_ms),
        application_name,
        application_identifier,
        target,
        ..ApplicationCapture::default()
    };

    // Opting out of URL capture is a user setting, not an absent source, and it
    // is reported that way on every exit below.
    if !options.url_capture_enabled {
        capture.browser_url = SourceOutcome::Unavailable(ContextSourceStatus::Disabled);
        if !policy.wants_focused_field() {
            return capture;
        }
    }

    if accessibility_access() != AccessibilityAccess::Granted {
        assign_ax_unavailable(
            &mut capture,
            policy,
            options,
            ContextSourceStatus::PermissionDenied,
        );
        return capture;
    }

    let focused_application = match focused_application(deadline) {
        Ok(element) => element,
        Err(status) => {
            assign_ax_unavailable(&mut capture, policy, options, status);
            return capture;
        }
    };
    let focused_element =
        match attribute_element(&focused_application, AX_FOCUSED_UI_ELEMENT, deadline) {
            Ok(element) => element,
            Err(status) => {
                assign_ax_unavailable(&mut capture, policy, options, status);
                return capture;
            }
        };

    let secure_field = is_secure_field(&focused_element, deadline);
    if secure_field {
        // Do not ask a secure control for its value. Accessibility can expose
        // that value to a trusted client, but Sona never does.
        if policy.wants_focused_field() {
            capture.focused_field = SourceOutcome::Unavailable(ContextSourceStatus::SecureField);
        }
    } else if policy.wants_focused_field() {
        capture.focused_field_name = attribute_string(&focused_element, AX_TITLE, deadline)
            .ok()
            .flatten()
            .or_else(|| {
                attribute_string(&focused_element, AX_DESCRIPTION, deadline)
                    .ok()
                    .flatten()
            });
        capture.focused_field = attribute_string(&focused_element, AX_VALUE, deadline)
            .map(SourceOutcome::read)
            .unwrap_or_else(SourceOutcome::Unavailable);
    }

    if options.url_capture_enabled {
        capture.browser_url = if secure_field {
            // A browser URL identifies the same private interaction as the
            // focused secure control, so it is excluded with that control.
            SourceOutcome::Unavailable(ContextSourceStatus::SecureField)
        } else {
            find_url(&focused_element, &focused_application, deadline)
        };
    }

    capture
}

/// Reads the current selection for an explicit user action. Independent of the
/// ambient policy: the caller's shortcut is the request.
pub(crate) fn capture_selected_text() -> SelectionCapture {
    if accessibility_access() != AccessibilityAccess::Granted {
        return SelectionCapture::Unavailable(ContextSourceStatus::PermissionDenied);
    }

    let deadline = CaptureDeadline::starting_now();
    match focused_element(deadline).map(|element| read_selection(&element, deadline)) {
        Ok(SourceOutcome::Captured(text)) => SelectionCapture::Captured(text),
        Ok(SourceOutcome::Unavailable(status)) => SelectionCapture::Unavailable(status),
        Err(status) => SelectionCapture::Unavailable(status),
    }
}

/// The focused control of the frontmost application, or the reason it could not
/// be reached. Both selection readers start here.
fn focused_element(
    deadline: CaptureDeadline,
) -> Result<CFRetained<AXUIElement>, ContextSourceStatus> {
    let focused_application = focused_application(deadline)?;
    attribute_element(&focused_application, AX_FOCUSED_UI_ELEMENT, deadline)
}

/// A secure control's selection is never read, even though Accessibility would
/// hand it over.
fn read_selection(element: &AXUIElement, deadline: CaptureDeadline) -> SourceOutcome {
    if is_secure_field(element, deadline) {
        return SourceOutcome::Unavailable(ContextSourceStatus::SecureField);
    }
    attribute_string(element, AX_SELECTED_TEXT, deadline)
        .map(SourceOutcome::read)
        .unwrap_or_else(SourceOutcome::Unavailable)
}

pub(crate) fn frontmost_application_identifier() -> Option<String> {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .as_ref()
        .and_then(|application| application.bundleIdentifier())
        .map(|identifier| identifier.to_string())
}

/// Reads just the browser host needed to select a local mode. The captured URL
/// stays inside this function and is discarded after parsing.
pub(crate) fn frontmost_website_host() -> WebsiteHostCapture {
    if accessibility_access() != AccessibilityAccess::Granted {
        return WebsiteHostCapture::Unavailable;
    }

    let deadline = CaptureDeadline::starting_now();
    let focused_application = match focused_application(deadline) {
        Ok(application) => application,
        Err(_) => return WebsiteHostCapture::Unavailable,
    };
    let focused_element =
        match attribute_element(&focused_application, AX_FOCUSED_UI_ELEMENT, deadline) {
            Ok(element) => element,
            Err(_) => return WebsiteHostCapture::Unavailable,
        };
    if is_secure_field(&focused_element, deadline) {
        return WebsiteHostCapture::SecureField;
    }

    match find_url(&focused_element, &focused_application, deadline) {
        SourceOutcome::Captured(url) => website_host_from_url(&url)
            .map(WebsiteHostCapture::Captured)
            .unwrap_or(WebsiteHostCapture::Unavailable),
        SourceOutcome::Unavailable(_) => WebsiteHostCapture::Unavailable,
    }
}

fn frontmost_application() -> (Option<String>, Option<String>) {
    let application = NSWorkspace::sharedWorkspace().frontmostApplication();
    let name = application
        .as_ref()
        .and_then(|application| application.localizedName())
        .map(|name| name.to_string());
    let identifier = application
        .as_ref()
        .and_then(|application| application.bundleIdentifier())
        .map(|identifier| identifier.to_string());
    (name, identifier)
}

/// Reads clipboard text only when its last change is provably inside the
/// caller's pre-roll window. `preroll_ms` is the run's frozen setting, so a
/// mid-run edit cannot widen what this run may read.
fn read_recent_clipboard(
    generation: clipboard_recency::Generation,
    preroll_ms: u64,
) -> SourceOutcome {
    clipboard_recency::read_if_fresh(
        generation,
        super::now_ms(),
        preroll_ms,
        || {
            objc2::rc::autoreleasepool(|_| {
                i64::try_from(NSPasteboard::generalPasteboard().changeCount()).ok()
            })
        },
        || {
            // This runs on the per-run capture worker, a thread with no pool
            // of its own. Without one, autoreleased AppKit values would live
            // until the thread exits.
            objc2::rc::autoreleasepool(|_| {
                let pasteboard = NSPasteboard::generalPasteboard();
                // SAFETY: NSPasteboardTypeString is a read-only AppKit framework static.
                let pasteboard_type = unsafe { NSPasteboardTypeString };
                pasteboard
                    .stringForType(pasteboard_type)
                    .map(|text| text.to_string())
            })
        },
    )
}

fn focused_application(
    deadline: CaptureDeadline,
) -> Result<CFRetained<AXUIElement>, ContextSourceStatus> {
    // SAFETY: the system-wide AX element has no caller-owned pointer input.
    let system = unsafe { AXUIElement::new_system_wide() };
    attribute_element(&system, AX_FOCUSED_APPLICATION, deadline)
}

fn find_url(
    focused: &AXUIElement,
    application: &AXUIElement,
    deadline: CaptureDeadline,
) -> SourceOutcome {
    if let Some(url) = ancestor_url(focused, deadline) {
        return SourceOutcome::read(Some(url));
    }
    match attribute_element(application, AX_FOCUSED_WINDOW, deadline) {
        Ok(window) => ancestor_url(&window, deadline)
            .map(|url| SourceOutcome::read(Some(url)))
            .unwrap_or_else(|| SourceOutcome::Unavailable(ContextSourceStatus::Empty)),
        Err(status) => SourceOutcome::Unavailable(status),
    }
}

/// The walk needs no deadline check of its own: once the budget is spent every
/// read below returns an error, so the loop stops on the first one.
fn ancestor_url(element: &AXUIElement, deadline: CaptureDeadline) -> Option<String> {
    if let Ok(Some(url)) = attribute_string(element, AX_URL, deadline) {
        return Some(url);
    }
    let mut current = attribute_element(element, AX_PARENT, deadline).ok()?;
    for _ in 0..MAX_ANCESTORS {
        if let Ok(Some(url)) = attribute_string(&current, AX_URL, deadline) {
            return Some(url);
        }
        current = attribute_element(&current, AX_PARENT, deadline).ok()?;
    }
    None
}

fn is_secure_field(element: &AXUIElement, deadline: CaptureDeadline) -> bool {
    [AX_ROLE, AX_SUBROLE].into_iter().any(|attribute| {
        attribute_string(element, attribute, deadline)
            .ok()
            .flatten()
            .is_some_and(|value| value == AX_SECURE_TEXT_FIELD)
    })
}

/// Reports one Accessibility failure against every source of the application
/// stage that asked for it. The selection is not one of them: it belongs to the
/// record-start stage and reports its own reason there.
fn assign_ax_unavailable(
    capture: &mut ApplicationCapture,
    policy: ContextPolicy,
    options: CaptureOptions,
    status: ContextSourceStatus,
) {
    if policy.wants_focused_field() {
        capture.focused_field = SourceOutcome::Unavailable(status);
    }
    capture.browser_url = SourceOutcome::Unavailable(if options.url_capture_enabled {
        status
    } else {
        ContextSourceStatus::Disabled
    });
}

fn set_timeout(element: &AXUIElement, timeout: Duration) -> Result<(), ContextSourceStatus> {
    // Do not start a read with AppKit's default timeout. If the element rejects
    // the bounded timeout, the source failed rather than silently escaping the
    // capture deadline.
    // SAFETY: `element` is a live AXUIElement reference for this call.
    let error = unsafe { element.set_messaging_timeout(timeout.as_secs_f32()) };
    if error == AXError::Success {
        Ok(())
    } else {
        Err(status_for_error(error))
    }
}

fn attribute_string(
    element: &AXUIElement,
    attribute: &str,
    deadline: CaptureDeadline,
) -> Result<Option<String>, ContextSourceStatus> {
    match attribute_value(element, attribute, deadline)? {
        Some(AccessibilityAttributeValue::Text(text)) => Ok(Some(text.to_string())),
        Some(AccessibilityAttributeValue::Url(url)) => Ok(Some(url.string().to_string())),
        Some(AccessibilityAttributeValue::Element(_))
        | Some(AccessibilityAttributeValue::Range(_))
        | Some(AccessibilityAttributeValue::Other)
        | None => Ok(None),
    }
}

fn attribute_element(
    element: &AXUIElement,
    attribute: &str,
    deadline: CaptureDeadline,
) -> Result<CFRetained<AXUIElement>, ContextSourceStatus> {
    match attribute_value(element, attribute, deadline)? {
        Some(AccessibilityAttributeValue::Element(element)) => Ok(element),
        None => Err(ContextSourceStatus::Empty),
        Some(_) => Err(ContextSourceStatus::Failed),
    }
}

/// The one place a cross-process read starts, and therefore the one place the
/// capture's budget is spent. Each element gets its own timeout because
/// attribute_element returns a fresh AXUIElement and messaging timeouts are
/// per element.
fn attribute_value(
    element: &AXUIElement,
    attribute: &str,
    deadline: CaptureDeadline,
) -> Result<Option<AccessibilityAttributeValue>, ContextSourceStatus> {
    let ReadBudget::Remaining(timeout) = deadline.next_read() else {
        // Out of budget. Failed is the status for a source that did not answer
        // in time, whether the target stalled or an earlier source spent the
        // time. Every remaining read lands here too, so the capture returns what
        // it already has instead of the join dropping all of it.
        return Err(ContextSourceStatus::Failed);
    };
    set_timeout(element, timeout)?;
    let attribute = CFString::from_str(attribute);
    let mut raw: *const CFType = ptr::null();
    // The Copy API returns either null or one retained CFType owned by this call.
    // SAFETY: raw is a live null-initialized output slot.
    let error = unsafe { element.copy_attribute_value(&attribute, NonNull::from(&mut raw)) };
    if error != AXError::Success {
        return Err(status_for_error(error));
    }
    let Some(raw) = NonNull::new(raw.cast_mut()) else {
        return Ok(None);
    };
    // SAFETY: CopyAttributeValue returned this pointer under the Create/Copy rule, so this call owns one retain.
    let value = unsafe { CFRetained::from_raw(raw) };
    Ok(Some(AccessibilityAttributeValue::from_copied(value)))
}

fn status_for_error(error: AXError) -> ContextSourceStatus {
    match error {
        AXError::NoValue => ContextSourceStatus::Empty,
        AXError::AttributeUnsupported | AXError::NotImplemented => ContextSourceStatus::Unsupported,
        AXError::APIDisabled => ContextSourceStatus::PermissionDenied,
        AXError::CannotComplete | AXError::Failure | AXError::InvalidUIElement => {
            ContextSourceStatus::Failed
        }
        _ => ContextSourceStatus::Failed,
    }
}

/// Result of one Accessibility insertion attempt. It deliberately separates an
/// unsupported target (safe to fall back) from a timeout/transport failure
/// (possibly dispatched, never retry).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccessibilityInsertion {
    Delivered,
    NotDispatched,
    DispatchedButUnconfirmed,
}

/// Replaces the focused editable control's selection (or inserts at its caret)
/// without changing the clipboard. This does not read a control's value.
///
/// Insertion reads the same out-of-process elements as capture, so it takes the
/// same budget: a wedged target costs one bounded attempt and then the caller's
/// existing clipboard fallback, instead of holding delivery for one timeout per
/// element read.
pub(crate) fn insert_into_focused_editable(text: &str) -> AccessibilityInsertion {
    if accessibility_access() != AccessibilityAccess::Granted {
        return AccessibilityInsertion::NotDispatched;
    }
    let deadline = CaptureDeadline::starting_now();
    let Ok(application) = focused_application(deadline) else {
        return AccessibilityInsertion::NotDispatched;
    };
    let Ok(element) = attribute_element(&application, AX_FOCUSED_UI_ELEMENT, deadline) else {
        return AccessibilityInsertion::NotDispatched;
    };
    if is_secure_field(&element, deadline) || !is_editable_role(&element, deadline) {
        return AccessibilityInsertion::NotDispatched;
    }
    let ReadBudget::Remaining(timeout) = deadline.next_read() else {
        // Nothing was dispatched, so the caller's fallback is still safe.
        return AccessibilityInsertion::NotDispatched;
    };

    if set_timeout(&element, timeout).is_err() {
        return AccessibilityInsertion::NotDispatched;
    }
    // A focused editable role is not always an input path: terminals
    // (Ghostty, Terminal.app) publish the screen grid as an AXTextArea for
    // screen readers, and Ghostty answers `AXSelectedText` writes with
    // Success without feeding the pty, so the text vanishes while the
    // receipt claims Delivered. Writability is the platform's own signal
    // for the difference — the grid reports non-settable — and nothing has
    // been dispatched yet, so refusing here safely falls back to the real
    // paste chord.
    if !selected_text_is_settable(&element) {
        return AccessibilityInsertion::NotDispatched;
    }
    let attribute = CFString::from_static_str(AX_SELECTED_TEXT);
    let replacement = CFString::from_str(text);
    // SAFETY: element is the currently focused editable AX element, and both attribute values remain valid for this call.
    match unsafe { element.set_attribute_value(&attribute, &replacement) } {
        AXError::Success => AccessibilityInsertion::Delivered,
        // The request may have reached a wedged target before the AX client
        // timed out. Treat it as unknown and do not emit a second event.
        AXError::CannotComplete | AXError::Failure => {
            AccessibilityInsertion::DispatchedButUnconfirmed
        }
        _ => AccessibilityInsertion::NotDispatched,
    }
}

/// Asks the target process whether `AXSelectedText` accepts writes. Any
/// error counts as non-settable: a blind write into a target that cannot
/// answer is exactly what this gate exists to prevent.
fn selected_text_is_settable(element: &AXUIElement) -> bool {
    let attribute = CFString::from_static_str(AX_SELECTED_TEXT);
    // `Boolean` in the AX signature is MacTypes' `u8` alias, private in the
    // binding crate; the alias is transparent so `u8` is the same type.
    let mut settable: u8 = 0;
    // SAFETY: `settable` points to a live stack slot for the whole call.
    let error = unsafe { element.is_attribute_settable(&attribute, NonNull::from(&mut settable)) };
    error == AXError::Success && settable != 0
}

fn is_editable_role(element: &AXUIElement, deadline: CaptureDeadline) -> bool {
    matches!(
        attribute_string(element, AX_ROLE, deadline)
            .ok()
            .flatten()
            .as_deref(),
        Some("AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField")
    )
}
