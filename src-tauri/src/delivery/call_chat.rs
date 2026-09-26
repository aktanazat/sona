//! Meeting notices never use the dictation insertion route.
use super::{DeliveryMethod, DeliveryOutcome, DeliveryReceipt};

pub struct CallChatResult {
    pub receipt: DeliveryReceipt,
    pub reason: String,
}

impl CallChatResult {
    pub fn not_posted(reason: &str) -> Self {
        Self { receipt: DeliveryReceipt::not_dispatched(), reason: reason.to_string() }
    }
}

/// The caller must durably claim the meeting's attempt before reaching here.
/// No clipboard, focused-input insertion, keyboard event, or retry is allowed.
pub fn announce(text: &str, bundle_id: Option<&str>) -> CallChatResult {
    if text.trim().is_empty() || text.chars().count() > 1_000 || text.contains('\0') {
        return CallChatResult::not_posted("The saved notice is empty or too long. Send a notice yourself.");
    }
    let Some(bundle_id) = bundle_id else {
        return CallChatResult::not_posted("The meeting app could not be identified.");
    };
    #[cfg(target_os = "macos")]
    { native::announce(text, bundle_id) }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bundle_id;
        CallChatResult::not_posted("Automatic chat notices are only available on macOS.")
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use serde::Deserialize;
    use std::ffi::{c_char, CStr, CString};

    extern "C" {
        fn sona_call_chat_announce_json(bundle: *const c_char, text: *const c_char) -> *mut c_char;
        fn sona_call_chat_free_string(value: *mut c_char);
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Outcome { NotPosted, SendRequested, AlreadyPresent, DraftOnly }

    #[derive(Deserialize)]
    struct NativeResult { outcome: Outcome, reason: String }

    pub(super) fn announce(text: &str, bundle_id: &str) -> CallChatResult {
        let (Ok(text), Ok(bundle_id)) = (CString::new(text), CString::new(bundle_id)) else {
            return CallChatResult::not_posted("The notice or meeting app is invalid.");
        };
        // SAFETY: both inputs are live NUL-terminated CStrings, copied by the bridge during this call.
        let pointer = unsafe { sona_call_chat_announce_json(bundle_id.as_ptr(), text.as_ptr()) };
        if pointer.is_null() {
            // Once the native boundary was crossed, allocation or decoding
            // failure cannot prove that the send did not happen.
            return uncertain();
        }
        // SAFETY: the non-null result is a live NUL-terminated strdup allocation owned by this call.
        let result = unsafe { serde_json::from_slice::<NativeResult>(CStr::from_ptr(pointer).to_bytes()) };
        // SAFETY: the bridge pairs strdup with free; parsing kept no borrows, and this is the only free.
        unsafe { sona_call_chat_free_string(pointer) };
        match result {
            Ok(result) => {
                let (method, outcome) = match result.outcome {
                    Outcome::NotPosted | Outcome::AlreadyPresent => (
                        DeliveryMethod::None, DeliveryOutcome::DefinitelyNotDispatched,
                    ),
                    Outcome::SendRequested | Outcome::DraftOnly => (
                        DeliveryMethod::AccessibilityInsertion, DeliveryOutcome::DispatchedButUnconfirmed,
                    ),
                };
                CallChatResult { receipt: DeliveryReceipt::new(method, outcome), reason: result.reason }
            }
            Err(_) => uncertain(),
        }
    }

    fn uncertain() -> CallChatResult {
        CallChatResult {
            receipt: DeliveryReceipt::new(DeliveryMethod::AccessibilityInsertion, DeliveryOutcome::DispatchedButUnconfirmed),
            reason: "Sona could not confirm the notice. Check the meeting chat before sending it yourself.".to_string(),
        }
    }
}
