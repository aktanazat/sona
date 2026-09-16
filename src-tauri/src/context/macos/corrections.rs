//! Opt-in learning from edits to the last delivered dictation only.
//!
//! Accessibility handles never leave the worker that created them. The worker
//! captures the insertion range before dispatch and must observe the exact
//! inserted text before it can treat a later edit as correction evidence.

use super::*;
use crate::delivery::DeliveryOutcome;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::Instant;
use tauri::AppHandle;

static GENERATION: AtomicU64 = AtomicU64::new(0);
const MAX_FIELD_BYTES: usize = 32 * 1024;
const OBSERVATION_WINDOW: Duration = Duration::from_secs(60);
const SETTLE_TIME: Duration = Duration::from_secs(2);

pub(crate) fn cancel() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// No reads from the destination occur unless the user enabled this source.
/// Importing settings does not import that consent.
pub(crate) fn prepare(app: &AppHandle, text: &str) -> Option<Sender<DeliveryOutcome>> {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    if !crate::settings::get_settings(app).learn_destination_corrections
        || text.is_empty()
        || text.len() > MAX_FIELD_BYTES
        || accessibility_access() != AccessibilityAccess::Granted
        || crate::secure_input::is_enabled_now()
    {
        return None;
    }
    let app = app.clone();
    let text = text.to_owned();
    let (ready, prepared) = mpsc::sync_channel(1);
    let (dispatch, dispatched) = mpsc::channel();
    std::thread::Builder::new()
        .name("destination-correction".into())
        .spawn(move || {
            let Some(target) = Target::capture(&text) else {
                return;
            };
            if ready.send(()).is_err() {
                return;
            }
            if !matches!(
                dispatched.recv_timeout(Duration::from_secs(10)),
                Ok(DeliveryOutcome::Delivered | DeliveryOutcome::DispatchedButUnconfirmed)
            ) {
                return;
            }
            target.observe(&app, &text, generation);
        })
        .ok()?;
    // The AX reader has the same 400 ms ceiling as other context capture.
    prepared.recv_timeout(Duration::from_millis(400)).ok()?;
    Some(dispatch)
}

struct Target {
    element: CFRetained<AXUIElement>,
    span: InsertionSpan,
}

impl Target {
    fn capture(text: &str) -> Option<Self> {
        let deadline = CaptureDeadline::starting_now();
        let application = focused_application(deadline).ok()?;
        let element = attribute_element(&application, AX_FOCUSED_UI_ELEMENT, deadline).ok()?;
        if is_secure_field(&element, deadline) || !is_editable_role(&element, deadline) {
            return None;
        }
        let value = attribute_string(&element, AX_VALUE, deadline).ok()??;
        if value.len() > MAX_FIELD_BYTES {
            return None;
        }
        let Some(AccessibilityAttributeValue::Range(range)) =
            attribute_value(&element, "AXSelectedTextRange", deadline).ok()?
        else {
            return None;
        };
        let span = InsertionSpan::new(&value, range, text)?;
        Some(Self { element, span })
    }

    fn read_focused(&self) -> Option<String> {
        if crate::secure_input::is_enabled_now() {
            return None;
        }
        let deadline = CaptureDeadline::starting_now();
        let application = focused_application(deadline).ok()?;
        let focused = attribute_element(&application, AX_FOCUSED_UI_ELEMENT, deadline).ok()?;
        if focused != self.element || is_secure_field(&focused, deadline) {
            return None;
        }
        let value = attribute_string(&focused, AX_VALUE, deadline).ok()??;
        (value.len() <= MAX_FIELD_BYTES).then_some(value)
    }

    fn observe(&self, app: &AppHandle, original: &str, generation: u64) {
        let started = Instant::now();
        let mut confirmed = false;
        let mut last = original.to_owned();
        let mut changed_at = started;
        while started.elapsed() < OBSERVATION_WINDOW {
            std::thread::sleep(Duration::from_millis(250));
            if GENERATION.load(Ordering::SeqCst) != generation
                || !crate::settings::get_settings(app).learn_destination_corrections
            {
                return;
            }
            let Some(value) = self.read_focused() else {
                return;
            };
            let Some(edited) = self.span.extract(&value) else {
                return;
            };
            if !confirmed {
                if edited == original {
                    confirmed = true;
                } else if started.elapsed() >= SETTLE_TIME {
                    return;
                }
                continue;
            }
            if edited.is_empty() {
                // The user deleted the dictation but kept what surrounded it.
                // Nothing was corrected, so there is nothing to learn, and a
                // later value of this field is a new act rather than an edit.
                return;
            }
            if edited != last {
                last = edited.to_owned();
                changed_at = Instant::now();
            } else if last != original && changed_at.elapsed() >= SETTLE_TIME {
                // Revocation or another insertion invalidates pending evidence.
                if GENERATION.load(Ordering::SeqCst) == generation
                    && crate::settings::get_settings(app).learn_destination_corrections
                {
                    // This watcher follows one reusable field, so it cannot tell a
                    // correction from a replacement by the text alone. Narrowing to
                    // the changed words keeps a reworded passage out, but it does
                    // not keep out a short field replaced whole: three tokens or
                    // fewer still yields a pair. Recurrence is what that case has
                    // to clear.
                    if let Some((spoken, written)) =
                        crate::meeting::store::learning::rewrite_span(original, &last)
                    {
                        crate::meeting::learning::notify_dictation_corrected(
                            app, &spoken, &written,
                        );
                    }
                }
                return;
            }
        }
    }
}

struct InsertionSpan {
    prefix: String,
    suffix: String,
}

impl InsertionSpan {
    /// The text around the insertion, or nothing when there is none.
    ///
    /// The surrounding text is the only thing that makes this field readable
    /// later: [`Self::extract`] recognizes the insertion by what still sits on
    /// either side of it. An insertion that filled the whole field leaves
    /// nothing on either side, so every later value of that field matches it,
    /// including the next message a user types after sending this one. That is
    /// a field with no boundary rather than a slow correction, and no polling
    /// rate turns it into evidence, so it is refused here.
    fn new(value: &str, range: objc2_core_foundation::CFRange, inserted: &str) -> Option<Self> {
        let start_units = usize::try_from(range.location).ok()?;
        let length = usize::try_from(range.length).ok()?;
        let start = utf16_byte_index(value, start_units)?;
        let end = utf16_byte_index(value, start_units.checked_add(length)?)?;
        if inserted.is_empty() {
            return None;
        }
        let (prefix, suffix) = (&value[..start], &value[end..]);
        if prefix.is_empty() && suffix.is_empty() {
            return None;
        }
        Some(Self {
            prefix: prefix.to_owned(),
            suffix: suffix.to_owned(),
        })
    }

    fn extract<'a>(&self, value: &'a str) -> Option<&'a str> {
        value.strip_prefix(&self.prefix)?.strip_suffix(&self.suffix)
    }
}

fn utf16_byte_index(text: &str, wanted: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units == wanted {
            return Some(byte);
        }
        units += character.len_utf16();
        if units > wanted {
            return None;
        }
    }
    (units == wanted).then_some(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_foundation::CFRange;

    #[test]
    fn corrections_are_limited_to_the_inserted_span() {
        let span = InsertionSpan::new("Hello , goodbye", CFRange::new(6, 0), "Acme").unwrap();
        assert_eq!(span.extract("Hello ACME, goodbye"), Some("ACME"));
        assert_eq!(span.extract("Other ACME, goodbye"), None);
        assert_eq!(span.extract("Hello ACME, unrelated"), None);
    }

    #[test]
    fn a_selection_after_a_non_bmp_character_uses_utf16_offsets() {
        let span = InsertionSpan::new("\u{1f4dd} old end", CFRange::new(3, 3), "Acme").unwrap();
        assert_eq!(span.extract("\u{1f4dd} ACME end"), Some("ACME"));
    }

    #[test]
    fn a_range_splitting_a_surrogate_pair_is_not_observable() {
        assert!(InsertionSpan::new("\u{1f4dd} note", CFRange::new(1, 0), "Acme").is_none());
    }

    #[test]
    fn a_dictation_that_fills_the_whole_field_is_not_observable() {
        // An empty composer: the insertion has nothing on either side, so the
        // unrelated message typed after this one is sent would read back as an
        // edit of it.
        assert!(InsertionSpan::new("", CFRange::new(0, 0), "okay").is_none());
        // One character of surrounding text is enough to tell them apart.
        assert!(InsertionSpan::new(" ", CFRange::new(0, 0), "okay").is_some());
    }
}
