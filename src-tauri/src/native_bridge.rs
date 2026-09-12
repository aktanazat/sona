//! The native shell's line to the core.
//!
//! The SwiftUI app spawns this process with `--native-socket <path>` and talks
//! to it over that Unix socket, one JSON object per line. A request is
//! `{"id", "method", "params"}` and its reply repeats the `id` with either
//! `result` or `error`. Core events go the other way without an `id`, as
//! `{"event", "payload"}`, copied from the Tauri event bus byte for byte so
//! the shell reads exactly what the webview used to.
//!
//! One shell owns one core. The socket is created for a launch, the listener
//! starts only after every manager is registered, and the core exits on its
//! own when the process that spawned it is gone.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tauri::{AppHandle, Listener, Manager};
use tauri_specta::Event as _;

use crate::commands;
use crate::managers::history::{HistoryManager, HistoryUpdatePayload, HISTORY_STORAGE_EVENT};
use crate::managers::model::ModelManager;
use crate::managers::transcription::{StreamPhaseEvent, StreamTextEvent, TranscriptionManager};
use crate::tray::DICTATION_ACTIVITY_EVENT;

/// What the shell hears. The specta events keep the names the webview
/// bindings export; the plain strings are emitted by hand in the managers.
const FORWARDED_EVENTS: [&str; 12] = [
    DICTATION_ACTIVITY_EVENT,
    HistoryUpdatePayload::NAME,
    HISTORY_STORAGE_EVENT,
    StreamTextEvent::NAME,
    StreamPhaseEvent::NAME,
    "model-state-changed",
    "models-updated",
    "model-download-progress",
    "model-download-complete",
    "model-download-failed",
    "model-download-cancelled",
    "model-deleted",
];

const HISTORY_UNAVAILABLE: &str = "history is unavailable";
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
    error: &'a str,
}

#[derive(Serialize)]
struct EventFrame<'a> {
    event: &'a str,
    payload: &'a RawValue,
}

#[derive(Deserialize)]
struct PageParams {
    cursor: Option<i64>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct SearchParams {
    query: String,
    cursor: Option<i64>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct EntryParams {
    id: i64,
}

#[derive(Deserialize)]
struct ModelParams {
    model_id: String,
}

#[derive(Serialize)]
struct ModelLoad {
    is_loaded: bool,
    current_model: Option<String>,
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
/// methods below read is managed: they use `state()`, not `try_state()`.
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

    for name in FORWARDED_EVENTS {
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
                Err(error) => failure(request.id, &error),
            };
            if let Err(error) = connection.send(&frame) {
                log::debug!("Native shell reply was not delivered: {error}");
            }
        });
    }
    lock_recover(&registry).retain(|other| !Arc::ptr_eq(other, &connection));
}

fn answer(id: u64, result: &str) -> String {
    match serde_json::from_str::<&RawValue>(result)
        .map_err(|error| error.to_string())
        .and_then(|result| {
            serde_json::to_string(&Answer { id, result }).map_err(|error| error.to_string())
        }) {
        Ok(frame) => frame,
        Err(error) => failure(id, &error),
    }
}

fn failure(id: u64, error: &str) -> String {
    serde_json::to_string(&Failure { id, error })
        .unwrap_or_else(|_| format!("{{\"id\":{id},\"error\":\"the error could not be encoded\"}}"))
}

fn parse<T: DeserializeOwned>(params: Option<&RawValue>) -> Result<T, String> {
    serde_json::from_str(params.map_or("{}", RawValue::get))
        .map_err(|error| format!("invalid params: {error}"))
}

fn encode<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| error.to_string())
}

/// Runs `work` on the main thread and waits for its value. Shortcut and
/// input registration make the same assumption the webview commands did:
/// they run where the event loop lives.
async fn on_main_thread<T: Send + 'static>(
    app: &AppHandle,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (sender, mut receiver) = tauri::async_runtime::channel(1);
    app.run_on_main_thread(move || {
        let _ = sender.try_send(work());
    })
    .map_err(|error| error.to_string())?;
    receiver
        .recv()
        .await
        .ok_or_else(|| "the main thread dropped the request".to_string())
}

async fn call(app: &AppHandle, method: &str, params: Option<&RawValue>) -> Result<String, String> {
    match method {
        "get_app_settings" => encode(&commands::get_app_settings(app.clone())?),
        "get_history_entries" => {
            let page: PageParams = parse(params)?;
            let history = app.state::<Arc<HistoryManager>>();
            let entries = commands::history::get_history_entries(
                app.clone(),
                history,
                page.cursor,
                page.limit,
            )
            .await
            .map_err(|()| HISTORY_UNAVAILABLE.to_string())?;
            encode(&entries)
        }
        "search_history_entries" => {
            let search: SearchParams = parse(params)?;
            let history = app.state::<Arc<HistoryManager>>();
            let entries = commands::history::search_history_entries(
                app.clone(),
                history,
                search.query,
                search.cursor,
                search.limit,
            )
            .await
            .map_err(|()| HISTORY_UNAVAILABLE.to_string())?;
            encode(&entries)
        }
        "get_history_stats" => {
            let history = app.state::<Arc<HistoryManager>>();
            let stats = commands::history::get_history_stats(app.clone(), history)
                .await
                .map_err(|()| HISTORY_UNAVAILABLE.to_string())?;
            encode(&stats)
        }
        "delete_history_entry" => {
            let entry: EntryParams = parse(params)?;
            let history = app.state::<Arc<HistoryManager>>();
            commands::history::delete_history_entry(app.clone(), history, entry.id).await?;
            encode(&())
        }
        "toggle_history_entry_saved" => {
            let entry: EntryParams = parse(params)?;
            let history = app.state::<Arc<HistoryManager>>();
            commands::history::toggle_history_entry_saved(app.clone(), history, entry.id).await?;
            encode(&())
        }
        "hud_toggle_recording" => {
            commands::hud::hud_toggle_recording(app.clone());
            encode(&())
        }
        "cancel_operation" => {
            commands::cancel_operation(app.clone());
            encode(&())
        }
        "is_recording" => encode(&commands::audio::is_recording(app.clone())),
        "get_available_models" => {
            let models = app.state::<Arc<ModelManager>>();
            encode(&commands::models::get_available_models(models).await?)
        }
        "get_current_model" => encode(&commands::models::get_current_model(app.clone()).await?),
        "get_model_load_status" => {
            let transcription = app.state::<Arc<TranscriptionManager>>();
            encode(&ModelLoad {
                is_loaded: transcription.is_model_loaded(),
                current_model: transcription.get_current_model(),
            })
        }
        "switch_active_model" => {
            let model: ModelParams = parse(params)?;
            let app = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                commands::models::switch_active_model(&app, &model.model_id)
            })
            .await
            .map_err(|error| error.to_string())??;
            encode(&())
        }
        "rescan_local_models" => {
            let models = app.state::<Arc<ModelManager>>();
            commands::models::rescan_local_models(models).await?;
            encode(&())
        }
        "download_model" => {
            let model: ModelParams = parse(params)?;
            let models = app.state::<Arc<ModelManager>>();
            commands::models::download_model(app.clone(), models, model.model_id).await?;
            encode(&())
        }
        "cancel_download" => {
            let model: ModelParams = parse(params)?;
            let models = app.state::<Arc<ModelManager>>();
            commands::models::cancel_download(models, model.model_id).await?;
            encode(&())
        }
        "delete_model" => {
            let model: ModelParams = parse(params)?;
            let models = app.state::<Arc<ModelManager>>();
            let transcription = app.state::<Arc<TranscriptionManager>>();
            commands::models::delete_model(app.clone(), models, transcription, model.model_id)
                .await?;
            encode(&())
        }
        "initialize_enigo" => {
            let app = app.clone();
            on_main_thread(&app.clone(), move || commands::initialize_enigo(app)).await??;
            encode(&())
        }
        "initialize_shortcuts" => {
            let app = app.clone();
            on_main_thread(&app.clone(), move || commands::initialize_shortcuts(app)).await??;
            encode(&())
        }
        "shutdown" => {
            app.exit(0);
            encode(&())
        }
        other => Err(format!("unknown method: {other}")),
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::{answer, failure, parse, EntryParams, PageParams};

    #[test]
    fn missing_params_read_as_an_empty_object() {
        let page: PageParams = parse(None).expect("optional params");
        assert_eq!(page.cursor, None);
        assert_eq!(page.limit, None);
        let error = match parse::<EntryParams>(None) {
            Ok(_) => panic!("a missing required id must fail"),
            Err(error) => error,
        };
        assert!(error.starts_with("invalid params:"), "{error}");
    }

    #[test]
    fn frames_carry_the_result_verbatim_and_the_error_escaped() {
        assert_eq!(
            answer(7, r#"{"entries":[],"has_more":false}"#),
            r#"{"id":7,"result":{"entries":[],"has_more":false}}"#
        );
        assert_eq!(
            failure(7, "bad \"quote\""),
            r#"{"id":7,"error":"bad \"quote\""}"#
        );
    }
}
