use crate::meeting::local_generator::AppleIntelligenceBlocker;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::future::Future;
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

#[repr(C)]
pub struct AppleLLMResponse {
    pub response: *mut c_char,
    pub success: c_int,
    pub error_message: *mut c_char,
}

extern "C" {
    pub fn apple_intelligence_status() -> c_int;
    pub fn free_apple_llm_response(response: *mut AppleLLMResponse);
}

/// The reason Apple Intelligence cannot answer right now, or `None` when it
/// can. Status codes are defined once, in `swift/apple_intelligence_bridge.h`;
/// the meeting engine status carries the variant to the settings and meetings
/// pages, and its `Display` is the diagnostic line for `sona.log`.
pub fn apple_intelligence_blocker() -> Option<AppleIntelligenceBlocker> {
    // SAFETY: The Swift bridge exports this no-argument function for the lifetime of the process.
    match unsafe { apple_intelligence_status() } {
        0 => None,
        1 => Some(AppleIntelligenceBlocker::NotEnabled),
        2 => Some(AppleIntelligenceBlocker::ModelNotReady),
        3 => Some(AppleIntelligenceBlocker::DeviceNotEligible),
        4 => Some(AppleIntelligenceBlocker::OsTooOld),
        _ => Some(AppleIntelligenceBlocker::Unknown),
    }
}

pub fn check_apple_intelligence_availability() -> bool {
    apple_intelligence_blocker().is_none()
}

type GenerationResult = Result<String, String>;
type GenerationSender = oneshot::Sender<GenerationResult>;

const GENERATION_DEADLINE: Duration = Duration::from_secs(30);
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);
static REQUESTS: LazyLock<Mutex<HashMap<u64, GenerationSender>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[repr(i32)]
#[derive(Clone, Copy)]
pub(crate) enum ResponseShape {
    Text = 0,
    Json = 1,
    MeetingNotes = 2,
    MeetingLedger = 3,
}

extern "C" {
    fn start_apple_intelligence_generation(
        request_id: u64,
        system_prompt: *const c_char,
        user_content: *const c_char,
        max_tokens: i32,
        response_shape: ResponseShape,
        json_schema: *const c_char,
        completion: extern "C" fn(u64, *mut AppleLLMResponse),
    );
    fn cancel_apple_intelligence_generation(request_id: u64);
}

fn requests() -> &'static Mutex<HashMap<u64, GenerationSender>> {
    &REQUESTS
}

struct GenerationRequest {
    id: u64,
}

impl Drop for GenerationRequest {
    fn drop(&mut self) {
        requests()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.id);
        // SAFETY: The bridge accepts any request id; already-finished requests are a no-op.
        unsafe { cancel_apple_intelligence_generation(self.id) };
    }
}

extern "C" fn generation_completed(request_id: u64, response_ptr: *mut AppleLLMResponse) {
    let result = if response_ptr.is_null() {
        Err("Null response from Apple LLM".to_string())
    } else {
        // SAFETY: Swift transfers one live response to this callback, including ownership of its strings.
        let response = unsafe { &*response_ptr };
        let result = if response.success == 1 {
            if response.response.is_null() {
                Ok(String::new())
            } else {
                // SAFETY: Swift response strings are NUL-terminated and live until the paired free below.
                Ok(unsafe { CStr::from_ptr(response.response) }
                    .to_string_lossy()
                    .into_owned())
            }
        } else if response.error_message.is_null() {
            Err("Unknown error".to_string())
        } else {
            // SAFETY: The error string has the same lifetime as the enclosing response.
            Err(unsafe { CStr::from_ptr(response.error_message) }
                .to_string_lossy()
                .into_owned())
        };
        // SAFETY: This callback is the sole owner of this Swift allocation, even after cancellation.
        unsafe { free_apple_llm_response(response_ptr) };
        result
    };
    #[cfg(test)]
    let result = test_receipts::complete(request_id, result);
    let sender = requests()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&request_id);
    if let Some(sender) = sender {
        let _ = sender.send(result);
    }
}

async fn await_generation(
    receiver: oneshot::Receiver<GenerationResult>,
    deadline: impl Future<Output = ()>,
) -> GenerationResult {
    tokio::select! {
        biased;
        result = receiver => result.unwrap_or_else(|_| Err("Apple Intelligence generation ended without a response".to_string())),
        () = deadline => Err("Apple Intelligence generation timed out".to_string()),
    }
}

pub async fn process_text_with_system_prompt_async(
    system_prompt: &str,
    user_content: &str,
    max_tokens: i32,
) -> GenerationResult {
    generate(
        system_prompt,
        user_content,
        max_tokens,
        ResponseShape::Text,
        None,
    )
    .await
}

async fn generate(
    system_prompt: &str,
    user_content: &str,
    max_tokens: i32,
    response_shape: ResponseShape,
    json_schema: Option<&str>,
) -> GenerationResult {
    let system_cstr = CString::new(system_prompt).map_err(|error| error.to_string())?;
    let user_cstr = CString::new(user_content).map_err(|error| error.to_string())?;
    let schema_cstr = json_schema
        .map(CString::new)
        .transpose()
        .map_err(|error| error.to_string())?;
    let id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = oneshot::channel();
    requests()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id, sender);
    let _request = GenerationRequest { id };
    #[cfg(test)]
    test_receipts::start(
        id,
        response_shape,
        max_tokens,
        system_prompt,
        user_content,
        json_schema,
    )?;
    // SAFETY: Swift copies the request strings before returning and invokes the callback at most once.
    unsafe {
        start_apple_intelligence_generation(
            id,
            system_cstr.as_ptr(),
            user_cstr.as_ptr(),
            max_tokens,
            response_shape,
            schema_cstr
                .as_ref()
                .map_or(std::ptr::null(), |schema| schema.as_ptr()),
            generation_completed,
        )
    };
    await_generation(receiver, tokio::time::sleep(GENERATION_DEADLINE)).await
}

pub(crate) fn process_with_response_shape(
    system_prompt: &str,
    user_content: &str,
    max_tokens: i32,
    response_shape: ResponseShape,
    json_schema: Option<&str>,
) -> GenerationResult {
    tauri::async_runtime::block_on(generate(
        system_prompt,
        user_content,
        max_tokens,
        response_shape,
        json_schema,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_held_generation_releases_the_waiter_at_its_deadline() {
        let (sender, receiver) = oneshot::channel();
        let result = await_generation(receiver, std::future::ready(())).await;
        assert_eq!(
            result,
            Err("Apple Intelligence generation timed out".to_string())
        );
        assert!(sender.send(Ok("late output".to_string())).is_err());
    }

    #[tokio::test]
    async fn a_completed_generation_preserves_the_provider_error() {
        let (sender, receiver) = oneshot::channel();
        sender
            .send(Err("Model not ready".to_string()))
            .expect("live receiver");
        assert_eq!(
            await_generation(receiver, std::future::pending()).await,
            Err("Model not ready".to_string())
        );
    }

    #[test]
    fn cancelling_a_request_prevents_a_late_callback_from_delivering() {
        let id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        let (sender, mut receiver) = oneshot::channel();
        requests().lock().expect("requests").insert(id, sender);
        drop(GenerationRequest { id });
        generation_completed(id, std::ptr::null_mut());
        assert_eq!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        );
    }
}

#[cfg(test)]
pub(crate) mod test_receipts;
