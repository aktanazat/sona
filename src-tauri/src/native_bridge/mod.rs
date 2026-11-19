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
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tauri::{AppHandle, Listener, Manager};

use crate::actions::{DICTATION_DELIVERY_EVENT, DICTATION_KEPT_EVENT};
use crate::managers::history::HISTORY_STORAGE_EVENT;
use crate::managers::media_import::MediaImportManager;
use crate::transcription_coordinator::DICTATION_MEETING_SUGGESTION_EVENT;
use crate::tray::DICTATION_ACTIVITY_EVENT;

/// The events the managers emit by string name rather than through a specta
/// type. The typed ones are listed in [`dispatch::TYPED_EVENTS`].
const NAMED_EVENTS: [&str; 30] = [
    DICTATION_ACTIVITY_EVENT,
    DICTATION_DELIVERY_EVENT,
    DICTATION_KEPT_EVENT,
    DICTATION_MEETING_SUGGESTION_EVENT,
    HISTORY_STORAGE_EVENT,
    "handy-keys-event",
    "hide-overlay",
    "local-cleanup-state",
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
    "onboarding-practice-expired",
    "paste-error",
    "recording-error",
    "recording-ready",
    "secure-input-changed",
    "settings-changed",
    "show-main-window",
    "show-overlay",
    "theme-changed",
    "transcription-error",
];

/// How often the core checks that the shell that spawned it is still there.
const PARENT_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// How many frames a connection may leave unread before it is closed. The
/// shell reads its socket as fast as frames arrive; a client this far behind
/// has stopped reading, and waiting on it would stall every thread that
/// emits, the after-call meeting pipeline included.
const UNREAD_FRAMES: usize = 4096;

/// The parent that connected as the native Sona app, not merely a launcher
/// that happened to spawn this core. Playback uses it to discount both Sona
/// processes without discounting audio from an arbitrary parent application.
#[cfg(target_os = "macos")]
static NATIVE_SHELL_PID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

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
/// events queue for its writer thread, which writes them in order, so frames
/// never interleave and a client that reads slowly never holds up a sender.
struct Connection {
    outbox: SyncSender<Arc<str>>,
    socket: UnixStream,
}

impl Connection {
    fn open(stream: &UnixStream) -> io::Result<Self> {
        let mut writer = stream.try_clone()?;
        let socket = stream.try_clone()?;
        let (outbox, frames) = mpsc::sync_channel::<Arc<str>>(UNREAD_FRAMES);
        std::thread::spawn(move || {
            for frame in frames {
                if let Err(error) = writer.write_all(frame.as_bytes()) {
                    log::debug!("Native shell connection stopped taking frames: {error}");
                    break;
                }
            }
        });
        Ok(Self { outbox, socket })
    }

    /// Queues `frame` and returns at once. A connection that has left
    /// `UNREAD_FRAMES` unread, or that can no longer be written, is shut
    /// down instead, and `false` says it is gone.
    fn send(&self, frame: Arc<str>) -> bool {
        let Err(error) = self.outbox.try_send(frame) else {
            return true;
        };
        if matches!(error, TrySendError::Full(_)) {
            log::warn!("Closing a native shell connection that stopped reading");
        }
        if let Err(error) = self.socket.shutdown(Shutdown::Both) {
            log::debug!("Native shell connection was already closed: {error}");
        }
        false
    }
}

/// A frame as it goes on the wire, ended by its newline.
fn wire(mut frame: String) -> Arc<str> {
    frame.push('\n');
    Arc::from(frame)
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
            let connection = match Connection::open(&stream) {
                Ok(connection) => Arc::new(connection),
                Err(error) => {
                    log::warn!("Native shell connection could not be split: {error}");
                    continue;
                }
            };
            #[cfg(target_os = "macos")]
            if let Some(pid) = verified_native_shell_pid(&stream) {
                let _ = NATIVE_SHELL_PID.set(pid);
            }
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

/// The shell that owns this core, while it remains our parent. Reparenting
/// after the shell exits must not make a reused PID look like Sona playback.
#[cfg(target_os = "macos")]
pub(crate) fn native_shell_pid() -> Option<u32> {
    NATIVE_SHELL_PID
        .get()
        .copied()
        .filter(|pid| *pid == std::os::unix::process::parent_id())
}

/// Core.swift spawns the helper and connects from the shell itself. Verify
/// that relationship with the kernel's socket peer PID and LaunchServices'
/// bundle identity; a parent PID or a client-supplied value alone is not it.
#[cfg(target_os = "macos")]
fn verified_native_shell_pid(stream: &UnixStream) -> Option<u32> {
    use objc2_app_kit::NSRunningApplication;
    use std::os::fd::AsRawFd;

    let mut pid: libc::pid_t = 0;
    let expected_size = libc::socklen_t::try_from(std::mem::size_of_val(&pid)).ok()?;
    let mut size = expected_size;
    // SAFETY: stream owns the open descriptor; pid is writable storage of the exact size passed for LOCAL_PEERPID.
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            std::ptr::from_mut(&mut pid).cast(),
            &mut size,
        )
    };
    if status != 0 || size != expected_size || pid <= 1 {
        return None;
    }
    let parent = u32::try_from(pid).ok()?;
    if parent != std::os::unix::process::parent_id() {
        return None;
    }
    objc2::rc::autoreleasepool(|_| {
        let application = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
        let bundle_id = application.bundleIdentifier()?;
        (bundle_id.to_string() == "com.aktanazat.sona.mac").then_some(parent)
    })
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
        Ok(frame) => wire(frame),
        Err(error) => {
            log::warn!("Event {name} could not be framed: {error}");
            return;
        }
    };
    lock_recover(registry).retain(|connection| connection.send(Arc::clone(&frame)));
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
            if !connection.send(wire(frame)) {
                log::debug!("Native shell reply was not delivered: the connection is closed");
            }
        });
    }
    lock_recover(&registry).retain(|other| !Arc::ptr_eq(other, &connection));
}

/// The methods only the shell can send come first; everything else is a
/// Tauri command, dispatched by the generated table.
async fn call(app: &AppHandle, method: &str, params: Option<&RawValue>) -> Result<String, Fault> {
    match method {
        "shutdown" => {
            app.exit(0);
            encode_plain(())
        }
        // The shell's open panel and window drop. Not a Tauri command: the
        // webview's picker is granted to the file scope by the dialog plugin,
        // and a command that granted any path it was handed would give a
        // webview the scope back. The shell has no plugin and no webview.
        "import_chosen_media_file" => {
            let mut args = Args::parse(params)?;
            let path: String = args.take("path")?;
            let handle = app.clone();
            on_main_thread(app, move || {
                let manager: tauri::State<'_, Arc<MediaImportManager>> = handle.state();
                reply(crate::commands::media_import::enqueue_chosen_media_file(
                    &handle,
                    manager.inner().as_ref(),
                    Path::new(&path),
                ))
            })
            .await?
        }
        _ => dispatch::call(app, method, params).await,
    }
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
            let error =
                serde_json::to_string(&error).map_err(|error| Fault::Bridge(error.to_string()))?;
            let error =
                RawValue::from_string(error).map_err(|error| Fault::Bridge(error.to_string()))?;
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
    use super::{answer, failure, reply, wire, Args, Connection, Fault, UNREAD_FRAMES};
    use serde_json::value::RawValue;
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    #[test]
    fn a_client_that_stops_reading_is_closed_instead_of_stalling_the_sender() {
        let (core_end, mut client) = UnixStream::pair().expect("a socket pair");
        let connection = Connection::open(&core_end).expect("a connection");
        let frame = wire("x".repeat(1023));
        let (report, outcome) = mpsc::channel();
        std::thread::spawn(move || {
            let open: Vec<bool> = (0..2 * UNREAD_FRAMES)
                .map(|_| connection.send(Arc::clone(&frame)))
                .collect();
            report.send(open).expect("the test waits for the outcome");
        });
        let open = outcome
            .recv_timeout(Duration::from_secs(30))
            .expect("sending never waits for the client to read");
        assert!(open[..UNREAD_FRAMES].iter().all(|open| *open));
        assert_eq!(open.last(), Some(&false));
        client
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("a read timeout");
        let mut received = Vec::new();
        client
            .read_to_end(&mut received)
            .expect("the client sees the connection close");
    }

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
        assert_eq!(
            failure(7, &typed),
            r#"{"id":7,"error":{"kind":"unpaired"}}"#
        );
        assert_eq!(
            failure(7, &Fault::Bridge("bad \"quote\"".to_string())),
            r#"{"id":7,"error":"bad \"quote\""}"#
        );
    }
}
