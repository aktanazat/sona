//! The native shell's line to the core.
//!
//! The SwiftUI app spawns this process with `--native-socket <path>` and talks
//! to it over that Unix socket, one JSON object per line. A request is
//! `{"id", "method", "params"}` and its reply repeats the `id` with either
//! `result` or `error`. `method` is a Tauri command name and `params` carries
//! its arguments under the names the webview bindings used; `error` is the
//! command's own error value, serialized as the bindings received it, or a
//! string when the bridge itself refused the request. Core events go the
//! other way without an `id`, as `{"event", "payload"}`, copied from the
//! Tauri event bus byte for byte so the shell reads exactly what the webview
//! used to.
//!
//! One shell owns one core. The socket is created for a launch, the listener
//! starts only after every manager is registered, and the core exits on its
//! own when the process that spawned it is gone.

mod dispatch;

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tauri::{AppHandle, Listener};

use crate::managers::history::HISTORY_STORAGE_EVENT;
use crate::tray::DICTATION_ACTIVITY_EVENT;

/// The events the managers emit by string name rather than through a specta
/// type. The typed ones are listed in [`dispatch::TYPED_EVENTS`].
const NAMED_EVENTS: [&str; 24] = [
    DICTATION_ACTIVITY_EVENT,
    HISTORY_STORAGE_EVENT,
    "handy-keys-event",
    "hide-overlay",
    "mic-level",
    "model-deleted",
    "model-download-cancelled",
    "model-download-complete",
    "model-download-failed",
    "model-download-progress",
    "model-extraction-completed",
    "model-extraction-started",
    "model-state-changed",
    "model-verification-completed",
    "model-verification-started",
    "models-updated",
    "paste-error",
    "recording-error",
    "recording-ready",
    "secure-input-changed",
    "settings-changed",
    "show-overlay",
    "theme-changed",
    "transcription-error",
];

/// How often the core checks that the shell that spawned it is still there.
const PARENT_CHECK_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Deserialize)]
struct Request {
    id: u64,
    method: String,
    params: Option<Box<RawValue>>,
}

#[derive(Serialize)]
struct Answer<'a> {
    id: u64,
    result: &'a RawValue,
}

#[derive(Serialize)]
struct Failure<'a> {
    id: u64,
    error: &'a RawValue,
}

#[derive(Serialize)]
struct EventFrame<'a> {
    event: &'a str,
    payload: &'a RawValue,
}

/// Why a request got no result.
#[derive(Debug)]
pub(crate) enum Fault {
    /// The bridge refused the request: an unknown method, unreadable
    /// arguments, a result that could not be encoded, or a runtime that
    /// dropped the work.
    Bridge(String),
    /// The command ran and returned its `Err`, already serialized.
    Command(Box<RawValue>),
}

impl Fault {
    fn unknown_method(method: &str) -> Self {
        Self::Bridge(format!("unknown method: {method}"))
    }

    fn json(&self) -> String {
        match self {
            Self::Bridge(message) => serde_json::to_string(message)
                .unwrap_or_else(|_| "\"the error could not be encoded\"".to_string()),
            Self::Command(value) => value.get().to_string(),
        }
    }
}

/// A request's arguments, read one at a time under their wire names. Tauri
/// exposes a command's `snake_case` parameters as `lowerCamelCase` keys, and
/// a missing key reads as `null` so an `Option` parameter may be omitted.
pub(crate) struct Args(HashMap<String, Box<RawValue>>);

impl Args {
    fn parse(params: Option<&RawValue>) -> Result<Self, Fault> {
        serde_json::from_str(params.map_or("{}", RawValue::get))
            .map(Self)
            .map_err(|error| Fault::Bridge(format!("invalid params: {error}")))
    }

    fn take<T: DeserializeOwned>(&mut self, name: &str) -> Result<T, Fault> {
        let raw = self.0.remove(name);
        serde_json::from_str(raw.as_deref().map_or("null", RawValue::get))
            .map_err(|error| Fault::Bridge(format!("invalid param `{name}`: {error}")))
    }
}

/// One shell connection. Requests arrive on its reader thread; replies and
/// events share the writer under one lock so frames never interleave.
struct Connection {
    writer: Mutex<UnixStream>,
}

impl Connection {
    fn send(&self, frame: &str) -> io::Result<()> {
        let mut writer = lock_recover(&self.writer);
        writer.write_all(frame.as_bytes())?;
        writer.write_all(b"\n")
    }
}

type Registry = Arc<Mutex<Vec<Arc<Connection>>>>;

/// Binds the socket and starts serving. Call it only once every manager the
/// commands read is managed: the dispatch uses `state()`, not `try_state()`.
pub fn start(app: &AppHandle, path: &Path) -> io::Result<()> {
    // A killed core leaves its socket file behind; the shell picks a fresh
    // path per launch, so an entry at this path is never a live listener.
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    let registry: Registry = Arc::new(Mutex::new(Vec::new()));

    for name in dispatch::TYPED_EVENTS.into_iter().chain(NAMED_EVENTS) {
        let registry = Arc::clone(&registry);
        app.listen(name, move |event| {
            broadcast(&registry, name, event.payload())
        });
    }

    let app = app.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(stream) => stream,
                Err(error) => {
                    log::warn!("Native shell connection failed: {error}");
                    continue;
                }
            };
            let writer = match stream.try_clone() {
                Ok(writer) => writer,
                Err(error) => {
                    log::warn!("Native shell connection could not be split: {error}");
                    continue;
                }
            };
            let connection = Arc::new(Connection {
                writer: Mutex::new(writer),
            });
            lock_recover(&registry).push(Arc::clone(&connection));
            let app = app.clone();
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || serve(app, connection, stream, registry));
        }
    });
    Ok(())
}

/// Ends the core when the shell that spawned it is gone. A shell that quits
/// asks for `shutdown` first; this covers the shell that crashed instead.
pub fn watch_parent(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(PARENT_CHECK_INTERVAL);
        if std::os::unix::process::parent_id() == 1 {
            log::info!("The native shell is gone; exiting the core");
            app.exit(0);
            return;
        }
    });
}

fn broadcast(registry: &Registry, name: &str, payload: &str) {
    let payload = match serde_json::from_str::<&RawValue>(payload) {
        Ok(payload) => payload,
        Err(error) => {
            log::warn!("Event {name} carried an unreadable payload: {error}");
            return;
        }
    };
    let frame = match serde_json::to_string(&EventFrame {
        event: name,
        payload,
    }) {
        Ok(frame) => frame,
        Err(error) => {
            log::warn!("Event {name} could not be framed: {error}");
            return;
        }
    };
    lock_recover(registry).retain(|connection| match connection.send(&frame) {
        Ok(()) => true,
        Err(error) => {
            log::debug!("Dropping a native shell connection: {error}");
            false
        }
    });
}

fn serve(app: AppHandle, connection: Arc<Connection>, stream: UnixStream, registry: Registry) {
    for line in BufReader::new(stream).lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                log::debug!("Native shell connection closed: {error}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                log::warn!("Native shell sent an unreadable request: {error}");
                continue;
            }
        };
        let app = app.clone();
        let connection = Arc::clone(&connection);
        tauri::async_runtime::spawn(async move {
            let frame = match call(&app, &request.method, request.params.as_deref()).await {
                Ok(result) => answer(request.id, &result),
                Err(fault) => failure(request.id, &fault),
            };
            if let Err(error) = connection.send(&frame) {
                log::debug!("Native shell reply was not delivered: {error}");
            }
        });
    }
    lock_recover(&registry).retain(|other| !Arc::ptr_eq(other, &connection));
}

async fn call(app: &AppHandle, method: &str, params: Option<&RawValue>) -> Result<String, Fault> {
    if method == "shutdown" {
        app.exit(0);
        return encode_plain(());
    }
    dispatch::call(app, method, params).await
}

fn answer(id: u64, result: &str) -> String {
    match serde_json::from_str::<&RawValue>(result)
        .map_err(|error| Fault::Bridge(error.to_string()))
        .and_then(|result| {
            serde_json::to_string(&Answer { id, result })
                .map_err(|error| Fault::Bridge(error.to_string()))
        }) {
        Ok(frame) => frame,
        Err(fault) => failure(id, &fault),
    }
}

fn failure(id: u64, fault: &Fault) -> String {
    let error = fault.json();
    serde_json::from_str::<&RawValue>(&error)
        .ok()
        .and_then(|error| serde_json::to_string(&Failure { id, error }).ok())
        .unwrap_or_else(|| format!("{{\"id\":{id},\"error\":\"the error could not be encoded\"}}"))
}

/// The reply for a command that returns its value directly.
fn encode_plain<T: Serialize>(value: T) -> Result<String, Fault> {
    serde_json::to_string(&value).map_err(|error| Fault::Bridge(error.to_string()))
}

/// The reply for a command that returns `Result`: the value on success, the
/// error serialized as the command's own type on failure.
fn reply<T: Serialize, E: Serialize>(result: Result<T, E>) -> Result<String, Fault> {
    match result {
        Ok(value) => encode_plain(value),
        Err(error) => {
            let error = serde_json::to_string(&error)
                .map_err(|error| Fault::Bridge(error.to_string()))?;
            let error = RawValue::from_string(error)
                .map_err(|error| Fault::Bridge(error.to_string()))?;
            Err(Fault::Command(error))
        }
    }
}

/// Runs `work` on the main thread and waits for its value. Tauri ran every
/// sync command there, and the shortcut, input, and window code they reach
/// assumes the thread that owns the event loop.
async fn on_main_thread<T: Send + 'static>(
    app: &AppHandle,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Fault> {
    let (sender, mut receiver) = tauri::async_runtime::channel(1);
    app.run_on_main_thread(move || {
        let _ = sender.try_send(work());
    })
    .map_err(|error| Fault::Bridge(error.to_string()))?;
    receiver
        .recv()
        .await
        .ok_or_else(|| Fault::Bridge("the main thread dropped the request".to_string()))
}

/// Runs `work` on a blocking thread, where Tauri ran a sync function marked
/// `#[tauri::command(async)]`.
async fn off_main_thread<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Fault> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| Fault::Bridge(error.to_string()))
}

fn lock_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::{answer, failure, reply, Args, Fault};
    use serde_json::value::RawValue;

    #[test]
    fn a_missing_argument_reads_as_null_so_only_options_may_be_omitted() {
        let mut args = Args::parse(None).expect("no params is an empty object");
        let cursor: Option<i64> = args.take("cursor").expect("a missing option is none");
        assert_eq!(cursor, None);
        let error = match args.take::<i64>("id") {
            Ok(_) => panic!("a missing required id must fail"),
            Err(Fault::Bridge(error)) => error,
            Err(Fault::Command(_)) => panic!("a missing id is the bridge's complaint"),
        };
        assert!(error.starts_with("invalid param `id`:"), "{error}");
    }

    #[test]
    fn arguments_are_read_under_their_wire_names() {
        let params = RawValue::from_string(r#"{"modelId":"tiny","limit":3}"#.to_string())
            .expect("valid json");
        let mut args = Args::parse(Some(&params)).expect("an object");
        let model: String = args.take("modelId").expect("present");
        let limit: usize = args.take("limit").expect("present");
        assert_eq!((model.as_str(), limit), ("tiny", 3));
    }

    #[test]
    fn frames_carry_the_result_verbatim_and_the_error_as_its_own_type() {
        assert_eq!(
            answer(7, r#"{"entries":[],"has_more":false}"#),
            r#"{"id":7,"result":{"entries":[],"has_more":false}}"#
        );
        let typed = match reply::<(), _>(Err(serde_json::json!({"kind":"unpaired"}))) {
            Err(fault) => fault,
            Ok(_) => panic!("an err must fault"),
        };
        assert_eq!(failure(7, &typed), r#"{"id":7,"error":{"kind":"unpaired"}}"#);
        assert_eq!(
            failure(7, &Fault::Bridge("bad \"quote\"".to_string())),
            r#"{"id":7,"error":"bad \"quote\""}"#
        );
    }
}
