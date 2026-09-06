use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};

// Define the response structure from Swift
#[repr(C)]
pub struct AppleLLMResponse {
    pub response: *mut c_char,
    pub success: c_int,
    pub error_message: *mut c_char,
}

// Link to the Swift functions
extern "C" {
    pub fn apple_intelligence_status() -> c_int;
    pub fn free_apple_llm_response(response: *mut AppleLLMResponse);
}

/// The reason Apple Intelligence cannot answer right now, or `None` when it
/// can. Status codes are defined once, in `swift/apple_intelligence_bridge.h`.
///
/// The OS distinguishes three unavailability reasons and they call for
/// different action — a switch to flip, a download to wait for, or hardware
/// that will never qualify — so the bridge carries the reason rather than
/// collapsing them into a bare "unavailable". The text is diagnostic English
/// for `sona.log`; the settings UI has its own translated string.
///
/// This is deliberately a reason string and not a typed enum. Nothing branches
/// per reason today, so an enum's discriminating power would be unused, and the
/// consumer that would use it — settings copy choosing one of three translated
/// keys — does not exist yet. The Swift codes are the stable contract; when
/// that consumer arrives it can map them to variants then.
pub fn apple_intelligence_unavailable_reason() -> Option<&'static str> {
    // SAFETY: The Swift bridge exports this no-argument function for the lifetime of the process.
    match unsafe { apple_intelligence_status() } {
        0 => None,
        1 => Some(
            "Apple Intelligence is switched off in System Settings > Apple Intelligence & Siri",
        ),
        2 => Some("Apple Intelligence is still downloading its model"),
        3 => Some("this Mac is not eligible for Apple Intelligence"),
        4 => Some("Apple Intelligence requires macOS 26 or newer"),
        _ => Some("Apple Intelligence is unavailable for an unrecognized reason"),
    }
}

pub fn check_apple_intelligence_availability() -> bool {
    apple_intelligence_unavailable_reason().is_none()
}

// Link to the Swift function for system prompt support
extern "C" {
    pub fn process_text_with_system_prompt_apple(
        system_prompt: *const c_char,
        user_content: *const c_char,
        max_tokens: i32,
    ) -> *mut AppleLLMResponse;
}

/// Process text with Apple Intelligence using separate system prompt and user content
pub fn process_text_with_system_prompt(
    system_prompt: &str,
    user_content: &str,
    max_tokens: i32,
) -> Result<String, String> {
    let system_cstr = CString::new(system_prompt).map_err(|e| e.to_string())?;
    let user_cstr = CString::new(user_content).map_err(|e| e.to_string())?;

    // SAFETY: Both C strings stay alive through the synchronous Swift call, and max_tokens is ABI-compatible.
    let response_ptr = unsafe {
        process_text_with_system_prompt_apple(system_cstr.as_ptr(), user_cstr.as_ptr(), max_tokens)
    };

    if response_ptr.is_null() {
        return Err("Null response from Apple LLM".to_string());
    }

    // SAFETY: The null check above established that the Swift-owned response points to a live AppleLLMResponse.
    let response = unsafe { &*response_ptr };

    let result = if response.success == 1 {
        if response.response.is_null() {
            Ok(String::new())
        } else {
            // SAFETY: A successful non-null response string is NUL-terminated and valid until freed below.
            let c_str = unsafe { CStr::from_ptr(response.response) };
            let rust_str = c_str.to_string_lossy().into_owned();
            Ok(rust_str)
        }
    } else {
        let error_c_str = if !response.error_message.is_null() {
            // SAFETY: A non-null Swift error string is NUL-terminated and valid until the enclosing response is freed.
            unsafe { CStr::from_ptr(response.error_message) }
        } else {
            c"Unknown error"
        };
        let error_msg = error_c_str.to_string_lossy().into_owned();
        Err(error_msg)
    };

    // SAFETY: response_ptr came from the paired Swift allocation function and has not been freed on this path.
    unsafe { free_apple_llm_response(response_ptr) };

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives the whole bridge: prompt in through the FFI, and either text or a
    /// reason back. A refusal carrying an empty or fallback message means the
    /// Swift side never populated `error_message`, which is a marshalling bug
    /// this catches and a bare availability print would not.
    ///
    /// Ignored by default for the same reason the loopback tests in
    /// `llm_client` are: on a Mac with Apple Intelligence enabled this performs
    /// a real on-device generation, and the Swift side blocks on a semaphore
    /// with no timeout, so a wedged `LanguageModelSession` would hang
    /// `cargo test` indefinitely — libtest has no per-test deadline. Run it
    /// with `bun run test:backend apple_intelligence -- --ignored
    /// --nocapture`.
    ///
    /// Deliberately asserts nothing about status agreeing with the answer.
    /// Rust reads the status here, Swift re-reads availability inside the call,
    /// and the Swift read is the authoritative one; the two can legitimately
    /// disagree when the model finishes downloading between them. A guardrail
    /// refusal from `session.respond` is likewise a legitimate `Err` on a
    /// perfectly healthy Mac. Neither is a bridge fault, so neither fails.
    #[test]
    #[ignore = "performs a real on-device generation when Apple Intelligence is enabled"]
    fn the_bridge_answers_or_says_why_not() {
        println!(
            "availability: {}",
            apple_intelligence_unavailable_reason().unwrap_or("available")
        );

        let started = std::time::Instant::now();
        let answer = process_text_with_system_prompt(
            "Rewrite the user's text as a question. Reply with the rewrite only.",
            "The plan is ready by Friday.",
            0,
        );
        println!("round trip: {:?}", started.elapsed());

        match answer {
            Ok(text) => {
                println!("output: {text}");
                assert!(!text.trim().is_empty(), "answered with empty text");
            }
            Err(error) => {
                println!("refusal: {error}");
                assert!(!error.trim().is_empty(), "refused with an empty reason");
                assert_ne!(
                    error, "Unknown error",
                    "the Swift side left error_message null"
                );
            }
        }
    }
}
