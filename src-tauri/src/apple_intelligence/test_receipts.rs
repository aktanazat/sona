use super::*;
use serde::Serialize;
use std::io::Write;
use std::sync::atomic::AtomicUsize;

static STARTED: AtomicUsize = AtomicUsize::new(0);
static LOG: Mutex<()> = Mutex::new(());
const RECEIPT_ENV: &str = "SONA_ACTUAL_MEETING_SDK_RECEIPT";

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum SdkReceipt<'a> {
    Start {
        index: usize,
        request_id: u64,
        shape: &'a str,
        max_tokens: i32,
        prompt: &'a str,
        input: &'a str,
        schema: Option<&'a str>,
    },
    Complete {
        request_id: u64,
        reply: Option<&'a str>,
        error: Option<&'a str>,
    },
}

pub(crate) fn started() -> usize {
    STARTED.load(Ordering::SeqCst)
}

fn append(value: &SdkReceipt<'_>) -> Result<(), String> {
    let Some(path) = std::env::var_os(RECEIPT_ENV) else {
        return Ok(());
    };
    let _guard = LOG.lock().map_err(|error| error.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    file.write_all(&bytes).map_err(|error| error.to_string())
}

pub(super) fn start(
    id: u64,
    shape: ResponseShape,
    max_tokens: i32,
    prompt: &str,
    input: &str,
    schema: Option<&str>,
) -> Result<(), String> {
    if std::env::var_os(RECEIPT_ENV).is_none() {
        return Ok(());
    }
    let count = STARTED.fetch_add(1, Ordering::SeqCst);
    if count >= 192 {
        return Err("Actual SDK receipt guard reached the unchanged192request maximum".to_string());
    }
    if !(1..=1024).contains(&max_tokens) {
        return Err("Actual SDK request exceeded the unchanged1024reply-token maximum".to_string());
    }
    let shape = match shape {
        ResponseShape::Text => "text",
        ResponseShape::Json => "json",
        ResponseShape::MeetingNotes => "meeting_notes",
        ResponseShape::MeetingLedger => "meeting_ledger",
    };
    append(&SdkReceipt::Start {
        index: count,
        request_id: id,
        shape,
        max_tokens,
        prompt,
        input,
        schema,
    })
}

pub(super) fn complete(id: u64, result: GenerationResult) -> GenerationResult {
    if std::env::var_os(RECEIPT_ENV).is_none() {
        return result;
    }
    let receipt = SdkReceipt::Complete {
        request_id: id,
        reply: result.as_ref().ok().map(String::as_str),
        error: result.as_ref().err().map(String::as_str),
    };
    match append(&receipt) {
        Ok(()) => result,
        Err(error) => Err(format!("Actual SDK completion receipt failed: {error}")),
    }
}
