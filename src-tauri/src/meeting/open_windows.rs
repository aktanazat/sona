//! What was open during a call.
//!
//! While a meeting records, Sona looks at the window in front every few
//! seconds and keeps, for each window, the app, the title, the file behind
//! it and the page's host; the full address only when full browser
//! addresses are allowed. A private browser window and Sona's own windows
//! are never kept, and a window that had a password field in focus is
//! forgotten for the rest of the meeting.
//!
//! Review offers the list. What the person attaches becomes one of the
//! meeting's documents or links; the rest is deleted when the offer is
//! dismissed, and in any case seven days after it was seen. The list stays
//! in the meeting store on this Mac: no command-line, agent or query-plane
//! read reaches it. Turning the list off deletes what was not attached. The
//! attach-window shortcut keeps the window in front at once, whether the
//! list is on or not: pressing it is the request.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Emitter, Listener};
use uuid::Uuid;

use super::clock::host_monotonic_now_ns;
use super::document_types::DocumentIngestRequest;
use super::session::MeetingSessionManager;
use super::store::{MeetingStore, StoreError};
use super::types::{
    MeetingCommandError, MeetingEventPayload, MeetingOperationId, MeetingPhase, MeetingSessionId,
};
use super::workflow_engine::{map_store_error, now_utc_ms};
use crate::context::{read_front_window, FrontWindow, FrontWindowRead};
use crate::meeting::detection::apps::is_browser_bundle_id;

const LOOK_INTERVAL: Duration = Duration::from_secs(3);
/// The shortcut that keeps the window in front during a recording.
pub(crate) const ATTACH_WINDOW_BINDING_ID: &str = "meeting_attach_window";
/// Windows one meeting offers; a long meeting's later ones are not kept.
pub(crate) const OFFERED_LIMIT: u32 = 100;
/// How long a window that was never attached is kept.
pub(crate) const OFFERED_KEEP_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(transparent)]
pub struct OpenWindowId(Uuid);

impl OpenWindowId {
    pub(crate) fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub(crate) fn uuid(self) -> Uuid {
        self.0
    }
}

/// One window that was in front while a meeting recorded.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingOpenWindow {
    pub item_id: OpenWindowId,
    /// The file's name, else the window's title, else the page's host.
    pub label: String,
    pub app_name: Option<String>,
    pub bundle_id: Option<String>,
    pub title: Option<String>,
    pub document_path: Option<String>,
    pub url: Option<String>,
    pub host: Option<String>,
    /// When it was first and last in front, from the start of the recording.
    pub first_offset_ns: u64,
    pub last_offset_ns: u64,
    /// How long it was in front in all.
    pub frontmost_ns: u64,
    /// Set once the person keeps it as one of the meeting's links.
    pub attached_at_utc_ms: Option<i64>,
}

/// What an attached window became.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum OpenWindowAttachment {
    Document,
    Link,
}

/// What the attach-window shortcut did.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WindowAttachOutcome {
    Document,
    Link,
    NotRecording,
    PasswordField,
    PrivateWindow,
    NothingToAttach,
    AccessibilityDenied,
    Failed,
}

/// Sent after the attach-window shortcut, for the card that says what it
/// took.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingWindowAttachedEvent {
    pub session_id: Option<MeetingSessionId>,
    pub outcome: WindowAttachOutcome,
    pub label: Option<String>,
    pub app_name: Option<String>,
}

impl tauri_specta::Event for MeetingWindowAttachedEvent {
    const NAME: &'static str = "meeting:window-attached";
}

/// What a window is called: its file's name, else its title, else its
/// page's host.
pub(crate) fn window_label(
    document_path: Option<&str>,
    title: Option<&str>,
    host: Option<&str>,
) -> Option<String> {
    document_path
        .and_then(|path| Path::new(path).file_name()?.to_str())
        .or(title)
        .or(host)
        .map(str::to_string)
}

/// Two looks are the same item when they show the same file, the same
/// address, the same page title on the same host, or the same window title
/// in the same app.
pub(crate) fn item_key(window: &FrontWindow) -> Option<String> {
    if let Some(path) = &window.document_path {
        return Some(format!("file\n{path}"));
    }
    if let Some(url) = &window.url {
        return Some(format!("url\n{url}"));
    }
    let title = window.title.as_deref()?;
    Some(match &window.host {
        Some(host) => format!("page\n{host}\n{title}"),
        None => format!(
            "window\n{}\n{title}",
            window.bundle_id.as_deref().unwrap_or_default()
        ),
    })
}

/// What one look at the window in front does to a meeting's list. The call
/// app's own windows are the call itself and are not kept; a browser call
/// keeps its pages, which say where the person was.
pub(crate) fn record_look(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    read: FrontWindowRead,
    offset_ns: u64,
    call_apps: &[String],
    password_windows: &mut HashSet<String>,
) -> Result<(), StoreError> {
    match read {
        FrontWindowRead::Window(window) => {
            let Some(key) = item_key(&window) else {
                return Ok(());
            };
            let call_app = window.bundle_id.as_deref().is_some_and(|bundle| {
                !is_browser_bundle_id(bundle)
                    && call_apps.iter().any(|app| app.eq_ignore_ascii_case(bundle))
            });
            if call_app || password_windows.contains(&key) {
                return Ok(());
            }
            store.note_open_window(
                session_id,
                &key,
                &window,
                offset_ns,
                look_interval_ns(),
                now_utc_ms(),
            )
        }
        FrontWindowRead::PasswordField(window) => {
            let Some(key) = item_key(&window) else {
                return Ok(());
            };
            store.forget_open_window(session_id, &key)?;
            password_windows.insert(key);
            Ok(())
        }
        FrontWindowRead::PrivateWindow
        | FrontWindowRead::Nothing
        | FrontWindowRead::AccessibilityDenied => Ok(()),
    }
}

fn look_interval_ns() -> u64 {
    u64::try_from(LOOK_INTERVAL.as_nanos()).unwrap_or(u64::MAX)
}

pub struct MeetingOpenWindowsService {
    app: AppHandle,
    manager: Arc<MeetingSessionManager>,
    watching: Mutex<HashSet<MeetingSessionId>>,
    /// Held while a look is written and while the list is cleared, so a
    /// look taken before the list was turned off never lands after it.
    writes: Mutex<()>,
}

impl MeetingOpenWindowsService {
    /// Starts watching for meetings that begin recording. A meeting gets one
    /// watcher thread while it captures; the thread looks at the window in
    /// front only while it records and the list is on.
    pub fn start(app: AppHandle, manager: Arc<MeetingSessionManager>) -> Arc<Self> {
        let service = Arc::new(Self {
            app: app.clone(),
            manager,
            watching: Mutex::default(),
            writes: Mutex::default(),
        });
        let listener = Arc::clone(&service);
        app.listen("meeting:session-changed", move |event| {
            let Ok(payload) = serde_json::from_str::<MeetingEventPayload>(event.payload()) else {
                return;
            };
            let Some(session_id) = payload.session_id else {
                return;
            };
            let service = Arc::clone(&listener);
            tauri::async_runtime::spawn(async move { service.session_changed(session_id).await });
        });
        service
    }

    fn watching(&self) -> MutexGuard<'_, HashSet<MeetingSessionId>> {
        self.watching.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn writes(&self) -> MutexGuard<'_, ()> {
        self.writes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn session_changed(self: Arc<Self>, session_id: MeetingSessionId) {
        let Ok(store) = self.manager.store().await else {
            return;
        };
        if phase(&store, session_id) != Some(MeetingPhase::CapturingRecording) {
            return;
        }
        if !self.watching().insert(session_id) {
            return;
        }
        let worker = Arc::clone(&self);
        let spawned = std::thread::Builder::new()
            .name("sona-meeting-open-windows".to_owned())
            .spawn(move || worker.watch(&store, session_id));
        if spawned.is_err() {
            self.watching().remove(&session_id);
        }
    }

    fn watch(&self, store: &MeetingStore, session_id: MeetingSessionId) {
        let Ok(plan) = store.processing_plan(session_id) else {
            self.watching().remove(&session_id);
            return;
        };
        let anchor_ns = plan.session_clock_anchor.host_monotonic_anchor_ns;
        let call_apps = plan.frozen_system_audio_application_bundle_ids;
        let mut password_windows = HashSet::new();
        loop {
            std::thread::sleep(LOOK_INTERVAL);
            match self.capturing(store, session_id) {
                None => return,
                Some(MeetingPhase::CapturingRecording) => {}
                Some(_) => continue,
            }
            let settings = crate::settings::get_settings(&self.app);
            if !settings.meeting_open_windows_enabled {
                continue;
            }
            let read = read_front_window(settings.context_url_capture_enabled);
            let offset_ns = host_monotonic_now_ns().saturating_sub(anchor_ns);
            let _writes = self.writes();
            if !crate::settings::get_settings(&self.app).meeting_open_windows_enabled {
                continue;
            }
            if let Err(error) = record_look(
                store,
                session_id,
                read,
                offset_ns,
                &call_apps,
                &mut password_windows,
            ) {
                log::debug!("meeting open windows: look not kept: {error:?}");
            }
        }
    }

    /// The meeting's phase while it captures. Once it does not, the watcher
    /// gives up its claim and then looks once more, so a meeting that came
    /// back between the two looks is never left without one.
    fn capturing(
        &self,
        store: &MeetingStore,
        session_id: MeetingSessionId,
    ) -> Option<MeetingPhase> {
        let capturing = || phase(store, session_id).filter(|phase| phase.capture_mode().is_some());
        if let Some(phase) = capturing() {
            return Some(phase);
        }
        self.watching().remove(&session_id);
        let phase = capturing()?;
        self.watching().insert(session_id).then_some(phase)
    }

    pub async fn list(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<MeetingOpenWindow>, MeetingCommandError> {
        self.manager
            .store()
            .await?
            .open_windows(session_id)
            .map_err(map_store_error)
    }

    /// Keeps an offered window: a text, Markdown or PDF file becomes one of
    /// the meeting's documents, anything else one of its links.
    pub async fn attach(
        &self,
        session_id: MeetingSessionId,
        item_id: OpenWindowId,
    ) -> Result<OpenWindowAttachment, MeetingCommandError> {
        let store = self.manager.store().await?;
        let item = store
            .open_window(session_id, item_id)
            .map_err(map_store_error)?;
        if item.attached_at_utc_ms.is_some() {
            return Ok(OpenWindowAttachment::Link);
        }
        if let Some(path) = &item.document_path {
            if self.add_document(session_id, path).await {
                store
                    .remove_open_window(session_id, item_id)
                    .map_err(map_store_error)?;
                return Ok(OpenWindowAttachment::Document);
            }
        }
        store
            .attach_open_window(session_id, item_id, now_utc_ms())
            .map_err(map_store_error)?;
        Ok(OpenWindowAttachment::Link)
    }

    pub async fn remove(
        &self,
        session_id: MeetingSessionId,
        item_id: OpenWindowId,
    ) -> Result<(), MeetingCommandError> {
        self.manager
            .store()
            .await?
            .remove_open_window(session_id, item_id)
            .map_err(map_store_error)
    }

    /// Deletes what the meeting offered and the person did not attach.
    pub async fn dismiss(&self, session_id: MeetingSessionId) -> Result<(), MeetingCommandError> {
        self.manager
            .store()
            .await?
            .dismiss_open_windows(session_id)
            .map(drop)
            .map_err(map_store_error)
    }

    /// Turns the list on or off. Off, every window offered and not attached
    /// is deleted.
    pub async fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        crate::settings::update_settings(&self.app, |settings| {
            settings.meeting_open_windows_enabled = enabled;
        })?;
        if enabled {
            return Ok(());
        }
        let store = self
            .manager
            .store()
            .await
            .map_err(|error| format!("{error:?}"))?;
        let _writes = self.writes();
        store
            .forget_offered_open_windows()
            .map(drop)
            .map_err(|error| format!("{error:?}"))
    }

    /// The attach-window shortcut: keeps the window in front as one of the
    /// recording meeting's documents or links, and says what it took.
    pub async fn attach_front_window(&self) {
        let event = self.attach_front_window_now().await;
        // Emitting fails only when the payload cannot be serialized, which a
        // plain record never does.
        let _ = self.app.emit(
            <MeetingWindowAttachedEvent as tauri_specta::Event>::NAME,
            event,
        );
    }

    async fn attach_front_window_now(&self) -> MeetingWindowAttachedEvent {
        let outcome = |session_id, outcome| MeetingWindowAttachedEvent {
            session_id,
            outcome,
            label: None,
            app_name: None,
        };
        let session_id = match self.manager.tray_snapshot().await {
            Ok(Some(snapshot)) if snapshot.phase == MeetingPhase::CapturingRecording => {
                snapshot.session_id
            }
            Ok(_) => return outcome(None, WindowAttachOutcome::NotRecording),
            Err(_) => return outcome(None, WindowAttachOutcome::Failed),
        };
        let keep_full_url = crate::settings::get_settings(&self.app).context_url_capture_enabled;
        let read =
            tauri::async_runtime::spawn_blocking(move || read_front_window(keep_full_url)).await;
        let window = match read {
            Ok(FrontWindowRead::Window(window)) => window,
            Ok(FrontWindowRead::PasswordField(_)) => {
                return outcome(Some(session_id), WindowAttachOutcome::PasswordField);
            }
            Ok(FrontWindowRead::PrivateWindow) => {
                return outcome(Some(session_id), WindowAttachOutcome::PrivateWindow);
            }
            Ok(FrontWindowRead::Nothing) => {
                return outcome(Some(session_id), WindowAttachOutcome::NothingToAttach);
            }
            Ok(FrontWindowRead::AccessibilityDenied) => {
                return outcome(Some(session_id), WindowAttachOutcome::AccessibilityDenied);
            }
            Err(_) => return outcome(Some(session_id), WindowAttachOutcome::Failed),
        };
        let Some(key) = item_key(&window) else {
            return outcome(Some(session_id), WindowAttachOutcome::NothingToAttach);
        };
        let outcome = match self.attach_window(session_id, &key, &window).await {
            Ok(OpenWindowAttachment::Document) => WindowAttachOutcome::Document,
            Ok(OpenWindowAttachment::Link) => WindowAttachOutcome::Link,
            Err(_) => WindowAttachOutcome::Failed,
        };
        MeetingWindowAttachedEvent {
            session_id: Some(session_id),
            outcome,
            label: window_label(
                window.document_path.as_deref(),
                window.title.as_deref(),
                window.host.as_deref(),
            ),
            app_name: window.app_name,
        }
    }

    async fn attach_window(
        &self,
        session_id: MeetingSessionId,
        key: &str,
        window: &FrontWindow,
    ) -> Result<OpenWindowAttachment, MeetingCommandError> {
        let store = self.manager.store().await?;
        if let Some(path) = &window.document_path {
            if self.add_document(session_id, path).await {
                store
                    .forget_open_window(session_id, key)
                    .map_err(map_store_error)?;
                return Ok(OpenWindowAttachment::Document);
            }
        }
        let anchor_ns = store
            .processing_plan(session_id)
            .map_err(map_store_error)?
            .session_clock_anchor
            .host_monotonic_anchor_ns;
        store
            .attach_open_window_now(
                session_id,
                key,
                window,
                host_monotonic_now_ns().saturating_sub(anchor_ns),
                now_utc_ms(),
            )
            .map_err(map_store_error)?;
        Ok(OpenWindowAttachment::Link)
    }

    /// Adds the file as one of the meeting's documents. A file the document
    /// list does not take is kept as a link instead.
    async fn add_document(&self, session_id: MeetingSessionId, path: &str) -> bool {
        self.manager
            .doc_ingest(DocumentIngestRequest {
                path: path.to_string(),
                operation_id: MeetingOperationId::new(),
                session_id: Some(session_id),
            })
            .await
            .is_ok()
    }
}

fn phase(store: &MeetingStore, session_id: MeetingSessionId) -> Option<MeetingPhase> {
    store
        .session_snapshot(session_id)
        .ok()
        .map(|snapshot| snapshot.phase)
}
