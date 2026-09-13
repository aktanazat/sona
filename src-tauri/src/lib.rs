mod actions;
pub mod agent_bridge;
pub mod agent_hook_wire;
mod agent_panel;
mod analytics;
pub mod upstream_import;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod apple_intelligence;
mod audio_feedback;
pub mod audio_toolkit;
mod autostart;
mod catalog;
pub mod cli;
mod clipboard;
#[cfg(feature = "cloud-realtime")]
pub mod cloud_stt;
mod cloud_sync;
mod command_mode;
mod commands;
mod context;
mod deeplink;
mod delivery;
mod fs_util;
mod helpers;
mod identity_adoption;
mod input;
mod launch_trace;
mod llm_client;
mod managers;
pub mod meeting;
#[cfg(target_os = "macos")]
pub mod meeting_macos;
mod memory;
mod modes;
mod native_bridge;
mod net_policy;
mod overlay;
mod paste_tx;
pub mod portable;
mod prompt_renderer;
pub mod query;
pub mod recorder;
#[cfg(target_os = "macos")]
pub mod recorder_macos;
mod secrets;
mod secure_input;
mod settings;
mod shortcut;
mod signal_handle;
mod snippets;
mod transcription_coordinator;
mod tray;
mod tray_i18n;
mod utils;

pub use cli::CliArgs;
#[cfg(debug_assertions)]
use specta_typescript::{BigIntExportBehavior, Typescript};
use tauri_specta::{collect_commands, collect_events, Builder};

use clap::Parser;
use commands::media_import::OpenedAudioImportFailure;
use env_filter::Builder as EnvFilterBuilder;
use managers::audio::AudioRecordingManager;
use managers::history::HistoryManager;
use managers::media_import::MediaImportManager;
use managers::model::ModelManager;
use managers::transcription::TranscriptionManager;
use meeting::session::{production_source_provider, MeetingMutationRequest, MeetingSessionManager};
use meeting::types::{
    AllowedMeetingAction, MeetingEventPayload, MeetingNavigationDestination,
    MeetingNavigationPayload, MeetingOperationId, MeetingSessionSnapshot, OperationResult,
};
use recorder::ScreenRecorderManager;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
pub use transcription_coordinator::TranscriptionCoordinator;

use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Listener, Manager};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_log::{Builder as LogBuilder, RotationStrategy, Target, TargetKind};

use crate::settings::{get_settings, AppearanceMaterial};

/// The window's material, applied before the first paint so the shell never
/// flashes the wrong one. This writes the *intent*; `apply_window_material`
/// corrects it to Solid while the window is still hidden if the native
/// vibrancy view could not be applied. Off macOS the intent is always Solid,
/// since vibrancy is the only thing that backs Glass.
fn main_window_material_init(material: AppearanceMaterial) -> String {
    let effective = if cfg!(target_os = "macos") {
        material
    } else {
        AppearanceMaterial::Solid
    };
    format!(
        "document.documentElement.dataset.material = '{}';",
        effective.as_str()
    )
}

// Global atomic to store the file log level filter
// We use u8 to store the log::LevelFilter as a number
pub static FILE_LOG_LEVEL: AtomicU8 = AtomicU8::new(4);

/// When `true`, log records are also forwarded to the webview via the
/// `log://log` event for the debug panel's live log viewer. Gated on debug
/// mode — the live log viewer is its only consumer and only exists in debug
/// mode — so normal runs never broadcast log records (which can include file
/// paths or transcribed text) onto the frontend event bus. Synced at startup
/// and whenever debug mode is toggled (see `shortcut::change_debug_mode_setting`).
pub static WEBVIEW_LOG_STREAMING: AtomicBool = AtomicBool::new(false);

fn level_filter_from_u8(value: u8) -> log::LevelFilter {
    match value {
        0 => log::LevelFilter::Off,
        1 => log::LevelFilter::Error,
        2 => log::LevelFilter::Warn,
        3 => log::LevelFilter::Info,
        4 => log::LevelFilter::Debug,
        5 => log::LevelFilter::Trace,
        _ => log::LevelFilter::Trace,
    }
}
pub(crate) const fn level_filter_code(value: log::LevelFilter) -> u8 {
    match value {
        log::LevelFilter::Off => 0,
        log::LevelFilter::Error => 1,
        log::LevelFilter::Warn => 2,
        log::LevelFilter::Info => 3,
        log::LevelFilter::Debug => 4,
        log::LevelFilter::Trace => 5,
    }
}

fn build_console_filter() -> env_filter::Filter {
    let mut builder = EnvFilterBuilder::new();

    match std::env::var("RUST_LOG") {
        Ok(spec) if !spec.trim().is_empty() => {
            if let Err(err) = builder.try_parse(&spec) {
                log::warn!(
                    "Ignoring invalid RUST_LOG value '{}': {}. Falling back to info-level console logging",
                    spec,
                    err
                );
                builder.filter_level(log::LevelFilter::Info);
            }
        }
        _ => {
            builder.filter_level(log::LevelFilter::Info);
        }
    }

    builder.build()
}

fn activate_main_window<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    main_window: &tauri::WebviewWindow<R>,
) {
    // The paint handoff is asynchronous; respect a minimize that won the race.
    if main_window.is_minimized().is_ok_and(|minimized| minimized) {
        return;
    }
    #[cfg(target_os = "macos")]
    match app.set_activation_policy(tauri::ActivationPolicy::Regular) {
        Ok(()) => launch_trace::mark_window_promotion(),
        Err(error) => log::error!("Failed to set activation policy to Regular: {error}"),
    }
    if let Err(error) = main_window.set_focus() {
        log::error!("Failed to focus webview window: {error}");
    }
}

pub(crate) fn show_main_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(main_window) = app.get_webview_window("main") {
        if let Err(error) = main_window.unminimize() {
            log::error!("Failed to unminimize webview window: {error}");
        }
        if let Err(error) = main_window.show() {
            log::error!("Failed to show webview window: {error}");
        } else {
            launch_trace::mark_shell_shown();
            if let Err(error) = app.emit(launch_trace::SHELL_VISIBLE_EVENT, ()) {
                log::error!("Failed to report visible launch shell: {error}");
            }
        }
        // During launch the webview owns the handoff: it reports a composited
        // frame, then the listener in setup activates and focuses this window.
        // Later reveals focus immediately because that first frame already ran.
        if launch_trace::first_visible_frame_recorded() {
            activate_main_window(app, &main_window);
        }
        return;
    }

    let webview_labels = app.webview_windows().keys().cloned().collect::<Vec<_>>();
    log::error!(
        "Main window not found. Webview labels: {:?}",
        webview_labels
    );
}
pub(crate) fn show_meeting_destination(
    app: &AppHandle,
    destination: MeetingNavigationDestination,
    snapshot: Option<&MeetingSessionSnapshot>,
) {
    let (session_id, revision) = snapshot
        .map(|snapshot| (Some(snapshot.session_id), snapshot.revision))
        .unwrap_or((None, 0));
    show_meeting_navigation(app, destination, session_id, revision);
}

/// Open one meeting by id.
///
/// Revision zero is the established "unknown" value on this payload: the
/// meetings controller reloads the authoritative snapshot when it arrives,
/// which is exactly what the Overview's meeting links rely on. A URL handler
/// opening the encrypted store to read a number its destination re-reads
/// anyway would be a second answer to the same question.
fn show_meeting_by_id(app: &AppHandle, session_id: meeting::types::MeetingSessionId) {
    show_meeting_navigation(
        app,
        MeetingNavigationDestination::Session,
        Some(session_id),
        0,
    );
}

fn show_meeting_navigation(
    app: &AppHandle,
    destination: MeetingNavigationDestination,
    session_id: Option<meeting::types::MeetingSessionId>,
    revision: u64,
) {
    show_main_window(app);
    let _ = app.emit(
        "meeting:navigation-requested",
        MeetingNavigationPayload {
            event_schema_version: 1,
            destination,
            session_id,
            revision,
        },
    );
}

/// Ask the shell to open one query-plane address.
///
/// Meetings are absent on purpose: they have their own navigation event and
/// [`show_meeting_by_id`] uses it. This is for the nouns whose surfaces the
/// backend could not reach at all — a person, a dictation, the search field.
fn show_query_link(app: &AppHandle, target: query::QueryLinkTarget) {
    show_main_window(app);
    let _ = app.emit(
        query::QUERY_LINK_EVENT,
        query::QueryLinkPayload {
            event_schema_version: 1,
            target,
        },
    );
}

/// Routes one `sona://` URL, returning whether it was ours.
///
/// Every arm reuses an entry point the tray, CLI, or a command already calls;
/// a deep link is another trigger, never a private path into the app. Callers
/// use the return value to decide whether the string still needs their own
/// handling, which is how file:// opens and sona:// links share one event.
/// `sona_open_link` is one of those callers: a `sona://` row inside the app
/// routes through here rather than through a second, client-side reading of
/// what an address means.
pub(crate) fn dispatch_deep_link(app: &AppHandle, raw: &str) -> bool {
    let Some(action) = deeplink::parse_deep_link(raw) else {
        // An address of ours that names no route is still ours. Declining it
        // here hands it to the next reader of the same list: on macOS the audio
        // importer, which answers a mistyped link with a failed import, and on
        // the argv path a branch that does nothing at all. Claiming it means
        // this is the only place that can answer the user, so it raises the
        // window the way every other unrecognised launch does. The route
        // keyword goes in the log and the rest of the URL does not: a refused
        // `sona://search?q=…` carries the question somebody typed.
        let Some(route) = deeplink::deep_link_route(raw) else {
            return false;
        };
        log::warn!("Ignoring a sona:// deep link with no route named {route:?}");
        show_main_window(app);
        return true;
    };
    log::info!("Handling deep link: {action:?}");
    match action {
        deeplink::DeepLinkAction::ToggleRecording => {
            signal_handle::send_transcription_intent(
                app,
                modes::TranscriptionIntent::ActiveMode,
                "deep-link",
            );
        }
        deeplink::DeepLinkAction::RecordWithMode(mode_id) => {
            signal_handle::send_transcription_intent(
                app,
                modes::TranscriptionIntent::Mode { mode_id },
                "deep-link",
            );
        }
        deeplink::DeepLinkAction::SetActiveMode(mode_id) => {
            if let Err(error) = modes::set_active_mode(app.clone(), mode_id) {
                log::warn!("Deep link could not switch mode: {error}");
            }
        }
        deeplink::DeepLinkAction::StartMeeting => {
            // Surfaces the meeting screen rather than starting capture. A URL
            // is not consent to record a room, and meetings are prompt-only.
            show_meeting_destination(app, MeetingNavigationDestination::Preflight, None);
        }
        deeplink::DeepLinkAction::OpenMeeting(session_id) => {
            show_meeting_by_id(app, meeting::types::MeetingSessionId::from_uuid(session_id));
        }
        deeplink::DeepLinkAction::OpenLoop(loop_id) => {
            // A loop's address carries its meeting, so this routes without a
            // lookup. It opens the meeting's review — which loop is in front of
            // the reader once they are there is the review screen's affordance,
            // not something a URL handler can reach into.
            let loop_id = meeting::loop_types::MeetingLoopId(loop_id);
            match loop_id.session_id() {
                Some(session_id) => show_meeting_by_id(app, session_id),
                None => log::warn!("Deep link loop id names no meeting: {}", loop_id.as_str()),
            }
        }
        deeplink::DeepLinkAction::OpenPerson(person_id) => {
            show_query_link(
                app,
                query::QueryLinkTarget::Person {
                    person_id: meeting::people_types::PersonId(person_id),
                },
            );
        }
        deeplink::DeepLinkAction::OpenOrganization(slug) => {
            show_query_link(app, query::QueryLinkTarget::Organization { slug });
        }
        deeplink::DeepLinkAction::OpenDictation(history_id) => {
            show_query_link(app, query::QueryLinkTarget::Dictation { history_id });
        }
        deeplink::DeepLinkAction::Search(query) => {
            show_query_link(app, query::QueryLinkTarget::Search { query });
        }
    }
    true
}

/// Assembles the meeting-detection runtime and starts its loop.
///
/// Every platform observer is optional. A missing input device, an unbundled
/// build, or a denied grant degrades detection to the paths that still work
/// rather than failing startup, and `DetectionStatus` names which paths those
/// are. The loop never blocks the window paint: it runs on its own thread, and
/// its idle tick touches no database — a settings read, an atomic load, and the
/// running-application list, which is what keeps the allowlist honest while
/// nothing is happening.
fn start_meeting_detection(
    app_handle: &AppHandle,
    meetings: Arc<MeetingSessionManager>,
    self_lease: Arc<meeting::detection::input_device::SelfInputDeviceLease>,
) {
    use meeting::detection::input_device::InputDeviceLevel;
    use meeting::detection::{apps, calendar, notify, DetectionRuntime};

    // The delegate must be registered before the runtime exists, and its target
    // is the runtime. One cell, bound once, below.
    let responder = Arc::new(notify::ResponderCell::default());
    // Shared with the CoreAudio listener, which the detection thread starts.
    let level = Arc::new(InputDeviceLevel::default());

    #[cfg(target_os = "macos")]
    let (running_apps, calendar_source, prompts, browser_titles) = {
        let prompts: Arc<dyn notify::PromptPresenter> =
            match notify::UserNotificationPrompts::start(Arc::clone(&responder) as Arc<_>) {
                Some(prompts) => Arc::new(prompts),
                None => {
                    log::info!(
                        "Meeting detection prompts are in-app only: this build has no \
                         notification center"
                    );
                    Arc::new(notify::NoPrompts)
                }
            };
        // One observer serves both the activation edges the suggestion store
        // listens to and the on-demand reads the detection tick asks for.
        let browser_titles: Arc<dyn apps::BrowserTitleReader> =
            match meeting_macos::MacosMeetingSuggestionObserver::start(
                &[],
                meetings.suggestion_sink(),
            ) {
                Ok(observer) => Arc::new(observer),
                Err(error) => {
                    log::warn!("Meeting suggestion observer is unavailable: {error:?}");
                    Arc::new(apps::NoBrowserTitles)
                }
            };
        (
            Arc::new(apps::WorkspaceApps) as Arc<dyn apps::RunningAppsSource>,
            calendar::platform_calendar(),
            prompts,
            browser_titles,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let (running_apps, calendar_source, prompts, browser_titles) = (
        Arc::new(apps::NoRunningApps) as Arc<dyn apps::RunningAppsSource>,
        calendar::platform_calendar(),
        Arc::new(notify::NoPrompts) as Arc<dyn notify::PromptPresenter>,
        Arc::new(apps::NoBrowserTitles) as Arc<dyn apps::BrowserTitleReader>,
    );
    // The digest shares this presenter rather than making its own. There is one
    // notification centre per process, one delegate on it, and both categories
    // are registered together — a second presenter would be a second delegate
    // and would silently unregister the first one's actions.
    let native_prompts = Arc::clone(&prompts);
    let prompts = Arc::new(notify::ConsentPromptPresenter::new(
        app_handle.clone(),
        prompts,
    ));

    meetings.start_digest_scheduler(app_handle.clone(), native_prompts);
    responder.bind_digest(Arc::new(meeting::digest::CaptureOpener::new(
        app_handle.clone(),
    )));

    // The chat brain's tools and corpus card read the calendar ahead through
    // `query::tools` and `query::card`, which take the source as a handle.
    // Managed here, beside the runtime that also holds it, so the panel loop
    // reaches the same calendar detection reads rather than a second one.
    app_handle.manage(Arc::clone(&calendar_source));

    // The lease arrives from the recording manager rather than being looked up
    // here. It is the only thing in the process that can hold the input device,
    // and a lookup that missed would hand detection a lease nobody writes —
    // which reads exactly like the bug this whole change exists to fix, with no
    // symptom to distinguish it. A parameter cannot miss.

    let runtime = Arc::new(DetectionRuntime::with_parts(
        app_handle.clone(),
        meetings,
        self_lease,
        calendar_source,
        running_apps,
        Arc::clone(&level) as Arc<_>,
        prompts,
        browser_titles,
    ));
    responder.bind(runtime.prompt_responder());

    // The CoreAudio listener is registered by the loop itself, on its own
    // thread. Nothing above this line touches a platform framework: every one of
    // them — the notification center, the EventKit store, and the input-device
    // monitor — is now created on first use, because doing any of it here means
    // doing it during `setup`, before the window and its webview exist.
    runtime.spawn_loop(level);
    app_handle.manage(runtime);
}

fn refresh_meeting_tray(app: AppHandle, manager: Arc<MeetingSessionManager>) {
    tauri::async_runtime::spawn(async move {
        if let Ok(snapshot) = manager.tray_snapshot().await {
            tray::set_meeting_tray_snapshot(&app, snapshot.as_ref());
        }
    });
}

fn resolve_opened_audio_path(path: &Path, cwd: &str) -> PathBuf {
    if path.is_absolute() || cwd.is_empty() {
        path.to_path_buf()
    } else {
        Path::new(cwd).join(path)
    }
}

fn initial_opened_audio_paths(
    paths: &[PathBuf],
) -> impl Iterator<Item = Result<&Path, OpenedAudioImportFailure>> {
    paths
        .iter()
        .map(|path| Ok::<&Path, OpenedAudioImportFailure>(path.as_path()))
}

/// The `sona://` addresses an operating system handed us as opened files.
///
/// Windows and Linux cold-start the binary with a registered protocol URL in
/// argv, and `opened_audio_files` is the variadic positional, so the address
/// arrives looking like a file. The forwarded path and macOS `RunEvent::Opened`
/// both route before the import funnel sees the list; a cold start had no
/// router in front of it at all, so `sona://search?q=…` — an address the query
/// plane mints itself — produced no navigation, no error and no log line.
fn opened_deep_link_addresses(paths: &[PathBuf]) -> impl Iterator<Item = &str> {
    paths
        .iter()
        .filter_map(|path| path.to_str())
        .filter(|raw| deeplink::deep_link_route(raw).is_some())
}

fn forwarded_opened_audio_paths<'a>(
    paths: &'a [PathBuf],
    cwd: &'a str,
) -> impl Iterator<Item = Result<PathBuf, OpenedAudioImportFailure>> + 'a {
    paths.iter().map(move |path| {
        Ok::<PathBuf, OpenedAudioImportFailure>(resolve_opened_audio_path(path, cwd))
    })
}

#[cfg(target_os = "macos")]
fn macos_opened_audio_paths(
    urls: &[tauri::Url],
) -> impl Iterator<Item = Result<PathBuf, OpenedAudioImportFailure>> + '_ {
    urls.iter().map(|url| {
        url.to_file_path()
            .map_err(|_| OpenedAudioImportFailure::non_file_url())
    })
}

fn enqueue_opened_audio_path(app: &AppHandle, path: &Path) -> Result<(), OpenedAudioImportFailure> {
    let media_import_manager = app
        .try_state::<Arc<MediaImportManager>>()
        .ok_or_else(OpenedAudioImportFailure::unavailable)?;
    commands::media_import::enqueue_opened_audio_file(
        app,
        media_import_manager.inner().as_ref(),
        path,
    )
    .map(|_| ())
}

fn report_opened_audio_failure(
    app: &AppHandle,
    path: Option<&Path>,
    failure: OpenedAudioImportFailure,
) {
    log::warn!("Rejected an operating-system opened audio file: {failure}");
    show_main_window(app);
    commands::media_import::report_opened_audio_failure(app, path, failure);
}

fn dispatch_opened_audio_paths<I, P>(
    paths: I,
    mut enqueue: impl FnMut(&Path) -> Result<(), OpenedAudioImportFailure>,
    mut reject: impl FnMut(Option<&Path>, OpenedAudioImportFailure),
) -> bool
where
    I: IntoIterator<Item = Result<P, OpenedAudioImportFailure>>,
    P: AsRef<Path>,
{
    let mut queued_any = false;
    for path in paths {
        match path {
            Ok(path) => {
                let path = path.as_ref();
                // A `sona://` address is never a file to import.
                //
                // Three callers hand this function the URL list an operating
                // system gave us, and the invariant used to be enforced in two
                // of them: the forwarded path filtered, the macOS open-urls
                // path filtered, and `initial_opened_audio_paths` did not. So a
                // cold start turned every deep link — including one the query
                // plane mints itself, `sona://search?q=…` — into "Select a
                // readable audio file", a diagnosis pointing at the filesystem
                // for a problem that is a URL, while the route table never saw
                // the address at all.
                //
                // Here rather than in a third per-caller filter, so the next
                // caller inherits it instead of having to remember. It does not
                // make the forwarded path's own filter redundant, and that is
                // not an oversight: `resolve_opened_audio_path` joins a
                // relative path onto the cwd, and `sona://quit` *is* relative
                // as a `Path`, so by the time a forwarded argv reaches this
                // loop it reads `<cwd>/sona://quit` and no longer parses as a
                // URL. That filter has to run before resolution; this one is
                // what covers everybody who has not transformed the string.
                if path
                    .to_str()
                    .is_some_and(|raw| deeplink::deep_link_route(raw).is_some())
                {
                    continue;
                }
                match enqueue(path) {
                    Ok(()) => queued_any = true,
                    Err(failure) => reject(Some(path), failure),
                }
            }
            Err(failure) => reject(None, failure),
        }
    }
    queued_any
}

fn enqueue_opened_audio_paths<I, P>(app: &AppHandle, paths: I) -> bool
where
    I: IntoIterator<Item = Result<P, OpenedAudioImportFailure>>,
    P: AsRef<Path>,
{
    dispatch_opened_audio_paths(
        paths,
        |path| enqueue_opened_audio_path(app, path),
        |path, failure| report_opened_audio_failure(app, path, failure),
    )
}
fn is_opened_share_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("sona"))
}

fn enqueue_opened_paths<I, P>(app: &AppHandle, paths: I) -> bool
where
    I: IntoIterator<Item = Result<P, OpenedAudioImportFailure>>,
    P: AsRef<Path>,
{
    let mut share_paths = Vec::new();
    let mut audio_paths = Vec::new();
    for path in paths {
        match path {
            Ok(path) => {
                let path = path.as_ref().to_path_buf();
                if is_opened_share_file(&path) {
                    share_paths.push(path);
                } else {
                    audio_paths.push(Ok(path));
                }
            }
            Err(error) => audio_paths.push(Err(error)),
        }
    }
    if share_paths.is_empty() {
        return enqueue_opened_audio_paths(app, audio_paths);
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(runtime) = app.try_state::<Arc<cloud_sync::CloudSyncRuntime>>() {
            for path in share_paths {
                let _ = runtime.opened_share_file(path).await;
            }
        }
        if enqueue_opened_audio_paths(&app, audio_paths) {
            show_main_window(&app);
        }
    });
    true
}

#[allow(unused_variables)]
fn should_force_show_permissions_window(app: &AppHandle) -> bool {
    #[cfg(target_os = "windows")]
    {
        let model_manager = app.state::<Arc<ModelManager>>();
        let has_downloaded_models = model_manager
            .get_available_models()
            .iter()
            .any(|model| model.is_downloaded);

        if !has_downloaded_models {
            return false;
        }

        let status = commands::audio::get_windows_microphone_permission_status();
        if status.supported && status.overall_access == commands::audio::PermissionAccess::Denied {
            log::info!(
                "Windows microphone permissions are denied; forcing main window visible for onboarding"
            );
            return true;
        }
    }

    false
}

fn initialize_core_logic(app_handle: &AppHandle, runtime: StartupRuntime) -> anyhow::Result<()> {
    // Note: Enigo (keyboard/mouse simulation) is NOT initialized here.
    // The frontend is responsible for calling the `initialize_enigo` command
    // after onboarding completes. This avoids triggering permission dialogs
    // on macOS before the user is ready.

    // Initialize the managers. The audio recorder receives the streaming router
    // explicitly, so always-on microphone startup can wire live-preview frames
    // even before Tauri state is populated.
    let model_manager = Arc::new(ModelManager::new(app_handle)?);
    let transcription_manager = Arc::new(TranscriptionManager::new(
        app_handle,
        model_manager.clone(),
    )?);
    let recording_manager = Arc::new(AudioRecordingManager::new(
        app_handle,
        transcription_manager.stream_router(),
    )?);
    let history_manager = Arc::new(HistoryManager::new(app_handle)?);
    // The configured asset scope is empty, so the webview may read exactly one
    // directory: the recordings this app wrote. The grant happens here because
    // only the manager knows the portable-aware data directory, which does not
    // always match the `$APPDATA` the capability files can name. Non-recursive:
    // recordings are flat files.
    if let Err(error) = app_handle
        .asset_protocol_scope()
        .allow_directory(history_manager.recordings_dir(), false)
    {
        log::error!("History audio playback is unavailable: {error}");
    }
    let media_import_manager = Arc::new(MediaImportManager::new(
        app_handle,
        transcription_manager.clone(),
        history_manager.clone(),
    ));
    let screen_recorder_manager = ScreenRecorderManager::new(
        app_handle,
        Arc::clone(&history_manager),
        Arc::clone(&recording_manager),
    );
    let meeting_secrets = Arc::clone(&app_handle.state::<Arc<secrets::SecretManager>>());
    let meeting_manager = Arc::new(MeetingSessionManager::new(
        app_handle,
        Arc::clone(&meeting_secrets),
    ));
    meeting_manager.set_source_provider(production_source_provider(Arc::clone(&recording_manager)));
    meeting_manager.set_transcription_manager(Arc::clone(&transcription_manager));
    // Meeting storage opens with a key from the OS credential store, and that
    // read can block behind a system prompt. Recovery therefore runs off the
    // startup path, below, once state is registered.
    let cloud_runtime = Arc::new(cloud_sync::CloudSyncRuntime::new(
        app_handle.clone(),
        Arc::clone(&meeting_manager),
        meeting_secrets,
    ));

    // Initialize the transcribe-cpp native backend (logging + backend module
    // registration) once, before any whisper model is loaded.
    managers::transcription::init_transcribe_backend();

    // Apply accelerator preferences before any model loads
    managers::transcription::apply_accelerator_settings(app_handle);

    // Add managers to Tauri's managed state
    app_handle.manage(recording_manager.clone());
    app_handle.manage(model_manager.clone());
    app_handle.manage(transcription_manager.clone());
    app_handle.manage(history_manager.clone());
    app_handle.manage(Arc::clone(&screen_recorder_manager));
    app_handle.manage(media_import_manager.clone());
    app_handle.manage(Arc::clone(&meeting_manager));
    app_handle.manage(Arc::clone(&cloud_runtime));
    // The startup orchestrator calls this only after the launch shell's DOM
    // paint. Recovery can now open the keyring and sweep without occupying it.
    let recovery_meetings = Arc::clone(&meeting_manager);
    let recovery_cloud = Arc::clone(&cloud_runtime);
    tauri::async_runtime::spawn(async move {
        let recovered = {
            let _span = launch_trace::span("recovery_sweep");
            match recovery_meetings.recover_at_startup().await {
                Ok(recovered) => recovered,
                Err(error) => {
                    log::warn!("Meeting recovery is unavailable at startup: {error:?}");
                    Vec::new()
                }
            }
        };
        recovery_cloud.start();
        recovery_meetings.start_retention_sweeper();
        recovery_meetings.start_recovery_reprocess(recovered);
    });
    match agent_bridge::AgentBridgeManager::new(app_handle) {
        Ok(manager) => {
            if let Err(error) = manager.reconcile() {
                log::warn!("Agent bridge is unavailable: {error}");
            }
            app_handle.manage(manager);
        }
        Err(error) => log::warn!("Agent bridge runtime is unavailable: {error}"),
    }
    app_handle.manage(tray::TrayState::new());
    let meeting_manager_for_changes = Arc::clone(&meeting_manager);
    let app_handle_for_meeting_changes = app_handle.clone();
    app_handle.listen("meeting:session-changed", move |event| {
        let Ok(payload) = serde_json::from_str::<MeetingEventPayload>(event.payload()) else {
            return;
        };
        if payload.session_id.is_some() {
            refresh_meeting_tray(
                app_handle_for_meeting_changes.clone(),
                Arc::clone(&meeting_manager_for_changes),
            );
        }
    });
    let meeting_manager_for_removals = Arc::clone(&meeting_manager);
    let app_handle_for_meeting_removals = app_handle.clone();
    app_handle.listen("meeting:removed", move |_| {
        refresh_meeting_tray(
            app_handle_for_meeting_removals.clone(),
            Arc::clone(&meeting_manager_for_removals),
        );
    });
    start_meeting_detection(
        app_handle,
        Arc::clone(&meeting_manager),
        recording_manager.self_input_device_lease(),
    );

    // Note: Shortcuts are NOT initialized here.
    // The frontend is responsible for calling the `initialize_shortcuts` command
    // after permissions are confirmed (on macOS) or after onboarding completes.
    // This matches the pattern used for Enigo initialization.

    // Set up signal handlers for toggling transcription. On Linux, SIGUSR1 is
    // deliberately not handled — it belongs to WebKitGTK's garbage collector
    // (#1660) — see signal_handle.rs.
    #[cfg(unix)]
    signal_handle::setup_signal_handler(app_handle.clone());

    // Apply macOS Accessory policy if starting hidden and tray is available.
    // If the tray icon is disabled, keep the dock icon so the user can reopen.
    #[cfg(target_os = "macos")]
    {
        if runtime.starts_hidden && runtime.tray_available {
            let _ = app_handle.set_activation_policy(tauri::ActivationPolicy::Accessory);
        }
    }
    // Get the current theme to set the appropriate initial icon
    let initial_theme = tray::get_current_theme(app_handle);

    // Choose the appropriate initial icon based on theme
    let initial_icon_path = tray::get_icon_path(initial_theme, tray::TrayIconState::Idle, false);

    let initial_icon = tray::load_initial_tray_icon(app_handle, initial_icon_path);
    let mut tray_builder = TrayIconBuilder::new()
        .icon(initial_icon)
        .tooltip(tray::tray_tooltip())
        .icon_as_template(true);

    // Windows notification-area convention: left click opens the app, right click
    // shows the menu. Elsewhere (macOS menu bar, Linux) the menu stays on left click.
    #[cfg(target_os = "windows")]
    {
        tray_builder = tray_builder
            .show_menu_on_left_click(false)
            .on_tray_icon_event(|tray, event| {
                use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
                let opens_window = matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    }
                );
                if opens_window {
                    show_main_window(tray.app_handle());
                }
            });
    }
    #[cfg(not(target_os = "windows"))]
    {
        tray_builder = tray_builder.show_menu_on_left_click(true);
    }

    let tray = tray_builder
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open_sona" => {
                show_main_window(app);
            }
            "start_dictation" | "stop_dictation" => {
                let coordinator = app.state::<TranscriptionCoordinator>();
                coordinator.send_intent(modes::TranscriptionIntent::ActiveMode, "tray");
            }
            "settings" => {
                tray::open_settings(app);
            }
            "secure_input_warning" => {
                // Full explanation lives in the settings-window banner
                show_main_window(app);
            }
            "copy_last_transcript" => {
                tray::copy_last_transcript(app);
            }
            "unload_model" => {
                let transcription_manager = app.state::<Arc<TranscriptionManager>>();
                if !transcription_manager.is_model_loaded() {
                    log::warn!("No model is currently loaded.");
                    return;
                }
                match transcription_manager.unload_model() {
                    Ok(()) => log::info!("Model unloaded via tray."),
                    Err(e) => log::error!("Failed to unload model via tray: {}", e),
                }
            }
            "cancel_transcription" => {
                utils::cancel_current_operation(app);
            }
            "start_meeting_notes" => {
                let manager = Arc::clone(&app.state::<Arc<MeetingSessionManager>>());
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    match manager.create_manual_preflight_from_tray().await {
                        Ok(snapshot) => {
                            tray::set_meeting_tray_snapshot(&app, Some(&snapshot));
                            show_meeting_destination(
                                &app,
                                MeetingNavigationDestination::Preflight,
                                Some(&snapshot),
                            );
                        }
                        Err(error) => {
                            log::warn!("Meeting preflight request was rejected: {error:?}");
                        }
                    }
                });
            }
            "open_meeting_notes" => {
                let manager = Arc::clone(&app.state::<Arc<MeetingSessionManager>>());
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let snapshot = manager.tray_snapshot().await.ok().flatten();
                    let destination = match snapshot.as_ref() {
                        Some(snapshot)
                            if snapshot.phase == meeting::types::MeetingPhase::Preflight =>
                        {
                            MeetingNavigationDestination::Preflight
                        }
                        Some(_) => MeetingNavigationDestination::Session,
                        None => MeetingNavigationDestination::List,
                    };
                    show_meeting_destination(&app, destination, snapshot.as_ref());
                });
            }
            "stop_meeting_notes" => {
                let manager = Arc::clone(&app.state::<Arc<MeetingSessionManager>>());
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let Ok(Some(snapshot)) = manager.tray_snapshot().await else {
                        return;
                    };
                    if !snapshot
                        .allowed_actions
                        .contains(&AllowedMeetingAction::Stop)
                    {
                        return;
                    }
                    match manager
                        .stop(
                            MeetingMutationRequest {
                                operation_id: MeetingOperationId::new(),
                                session_id: snapshot.session_id,
                                expected_revision: snapshot.revision,
                            },
                            meeting::session::MeetingStopCause::Tray,
                        )
                        .await
                    {
                        Ok(result) => tray::set_meeting_tray_snapshot(&app, Some(&result.snapshot)),
                        Err(error) => log::warn!("Meeting stop request was rejected: {error:?}"),
                    }
                });
            }
            "cancel_meeting_notes" => {
                let manager = Arc::clone(&app.state::<Arc<MeetingSessionManager>>());
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let Ok(Some(snapshot)) = manager.tray_snapshot().await else {
                        return;
                    };
                    let request = MeetingMutationRequest {
                        operation_id: MeetingOperationId::new(),
                        session_id: snapshot.session_id,
                        expected_revision: snapshot.revision,
                    };
                    if snapshot
                        .allowed_actions
                        .contains(&AllowedMeetingAction::CancelPreflight)
                    {
                        match manager.cancel_preflight(request).await {
                            Ok(receipt) if receipt.result == OperationResult::Committed => {
                                tray::set_meeting_tray_snapshot(&app, None);
                            }
                            Ok(_) => refresh_meeting_tray(app, manager),
                            Err(error) => {
                                log::warn!(
                                    "Meeting preflight cancellation was rejected: {error:?}"
                                );
                            }
                        }
                    } else if snapshot
                        .allowed_actions
                        .contains(&AllowedMeetingAction::Discard)
                    {
                        match manager.discard(request).await {
                            Ok(result) if result.removed => {
                                tray::set_meeting_tray_snapshot(&app, None);
                            }
                            Ok(_) => refresh_meeting_tray(app, manager),
                            Err(error) => {
                                log::warn!("Meeting discard request was rejected: {error:?}");
                            }
                        }
                    }
                });
            }
            "quit" => {
                app.exit(0);
            }
            id if id.starts_with("model_select:") => {
                let Some(model_id) = id.strip_prefix("model_select:") else {
                    return;
                };
                let model_id = model_id.to_string();
                let current_model = settings::get_settings(app).selected_model;
                if model_id == current_model {
                    return;
                }
                let app_clone = app.clone();
                std::thread::spawn(move || {
                    match commands::models::switch_active_model(&app_clone, &model_id) {
                        Ok(()) => {
                            log::info!("Model switched to {} via tray.", model_id);
                        }
                        Err(e) => {
                            log::error!("Failed to switch model via tray: {}", e);
                        }
                    }
                    tray::update_tray_menu(&app_clone);
                });
            }
            _ => {}
        })
        .build(app_handle)?;
    app_handle.manage(tray);

    // Initialize tray menu with idle state
    tray::update_tray_menu(app_handle);

    // Apply show_tray_icon setting
    if !runtime.tray_available {
        tray::set_tray_visibility(app_handle, false);
    }

    // Refresh tray menu when model state changes
    let app_handle_for_listener = app_handle.clone();
    app_handle.listen("model-state-changed", move |_| {
        tray::update_tray_menu(&app_handle_for_listener);
    });

    // Reconcile the autostart preference off the startup path. On macOS 13+ the
    // SMAppService status query is a synchronous XPC round-trip that measured
    // ~1.75 s on a cold launch — more than the rest of startup put together —
    // and the login item it settles only takes effect at the next login, so
    // nothing before the first paint has any reason to wait for it.
    // `reconcile_autostart` reads the persisted preference under its own lock,
    // so a settings change made while this is still in flight wins.
    let autostart_app = app_handle.clone();
    std::thread::spawn(move || autostart::reconcile_autostart(&autostart_app));
    Ok(())
}

/// Create the always-hidden recording overlay window and wire its native mode
/// menu, then show the idle pill if it is enabled.
///
/// Called after the main window is shown: this builds a second webview, which
/// measured ~43 ms of main-thread time, and every consumer of the overlay
/// (dictation shortcuts, tray, HUD commands, media import) is reachable only
/// once the event loop is dispatching, which cannot happen before `setup`
/// returns. So the overlay is always in place before anything can look for it,
/// while the main window's first paint no longer queues behind it.
fn initialize_recording_overlay(app_handle: &AppHandle) {
    utils::create_recording_overlay(app_handle);
    // The idle pill lives in that same window. Its mode menu is a real OS menu,
    // so its selections arrive as menu events rather than through the webview.
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        overlay_window.on_menu_event(|window, event| {
            let Some(mode_id) = event
                .id()
                .as_ref()
                .strip_prefix(commands::hud::HUD_MODE_MENU_PREFIX)
            else {
                return;
            };
            if let Err(error) =
                modes::set_active_mode(window.app_handle().clone(), mode_id.to_string())
            {
                log::warn!("HUD mode menu could not switch mode: {error}");
            }
        });
    }
    overlay::sync_hud_pill(app_handle);
}

#[tauri::command]
#[specta::specta]
fn show_main_window_command(app: AppHandle) -> Result<(), String> {
    show_main_window(&app);
    Ok(())
}

/// Convert an unexpected panic on a headless worker into a normal CLI
/// failure. Without this guard the Tauri event loop remains alive after the
/// worker exits, leaving `--transcribe-file` or a `--query` hung indefinitely.
fn run_headless_guarded<F>(operation: F) -> i32
where
    F: FnOnce() -> i32,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)) {
        Ok(code) => code,
        Err(_) => {
            eprintln!("error: the headless run failed unexpectedly");
            1
        }
    }
}

fn is_headless_mode(args: &CliArgs) -> bool {
    args.transcribe_file.is_some()
        || args.list_devices
        || args.list_models
        || args.agent_panel_public_identity
        || query::external::is_external_query(args)
}

/// macOS's plugin singleton is keyed by bundle identifier, not the data root.
/// A portable GUI instead owns its adjacent Data directory with an OS lock.
const fn uses_bundle_single_instance(portable: bool) -> bool {
    !cfg!(target_os = "macos") || !portable
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StartupRuntime {
    log_level: settings::LogLevel,
    debug_mode: bool,
    starts_hidden: bool,
    tray_available: bool,
    show_window_before_core: bool,
}

fn tray_is_available(settings: &settings::AppSettings, cli_args: &CliArgs) -> bool {
    settings.show_tray_icon && !cli_args.no_tray
}

/// What a close of the main window means.
///
/// A tray icon is the way back in, so the window hides. Without one there is no
/// way back, and closing the window does not end the process either: the
/// recording overlay and, on macOS, the consent panel stay alive and hidden, so
/// wry never reports an empty window map. The process would keep holding global
/// shortcuts with no surface left that can stop a recording, so this close ends
/// the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MainWindowCloseAction {
    Hide,
    Exit,
}

fn main_window_close_action(
    settings: &settings::AppSettings,
    cli_args: &CliArgs,
) -> MainWindowCloseAction {
    if tray_is_available(settings, cli_args) {
        MainWindowCloseAction::Hide
    } else {
        MainWindowCloseAction::Exit
    }
}

fn startup_runtime(settings: &settings::AppSettings, cli_args: &CliArgs) -> StartupRuntime {
    let starts_hidden = settings.start_hidden || cli_args.start_hidden;
    let tray_available = tray_is_available(settings, cli_args);

    StartupRuntime {
        log_level: if cli_args.debug {
            settings::LogLevel::Trace
        } else {
            settings.log_level
        },
        debug_mode: settings.debug_mode || cli_args.debug,
        starts_hidden,
        tray_available,
        show_window_before_core: !starts_hidden || !tray_available,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ForwardedControl {
    ToggleTranscription,
    TogglePostProcess,
    Cancel,
}

fn forwarded_control(args: &[String]) -> Option<ForwardedControl> {
    if args.iter().any(|arg| arg == "--toggle-transcription") {
        Some(ForwardedControl::ToggleTranscription)
    } else if args.iter().any(|arg| arg == "--toggle-post-process") {
        Some(ForwardedControl::TogglePostProcess)
    } else if args.iter().any(|arg| arg == "--cancel") {
        Some(ForwardedControl::Cancel)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ForwardedOpenAction {
    DeepLink,
    OpenedAudio,
    RaiseWindow,
}

fn forwarded_open_action(
    deep_link_dispatched: bool,
    opened_audio_queued: bool,
) -> ForwardedOpenAction {
    if deep_link_dispatched {
        ForwardedOpenAction::DeepLink
    } else if opened_audio_queued {
        ForwardedOpenAction::OpenedAudio
    } else {
        ForwardedOpenAction::RaiseWindow
    }
}

/// How a portable copy reports that another copy holds the Data directory.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PortableLockRefusal {
    /// A launch the user is watching. A window was going to appear, so the
    /// refusal has to be readable without a terminal.
    NativeAlert,
    /// A command line asking the running copy to start, stop or cancel. It was
    /// never going to show a window, and the copy holding the lock is the one
    /// it is talking to, so a modal telling the user to close that copy
    /// describes the wrong problem.
    Stderr,
}

/// Remote-control flags are a message to the running copy, not a second launch.
#[cfg(target_os = "macos")]
fn portable_lock_refusal(args: &CliArgs) -> PortableLockRefusal {
    if args.toggle_transcription || args.toggle_post_process || args.cancel {
        PortableLockRefusal::Stderr
    } else {
        PortableLockRefusal::NativeAlert
    }
}

#[cfg(target_os = "macos")]
fn exit_for_portable_lock_error(
    error: portable::DataDirInstanceLockError,
    refusal: PortableLockRefusal,
) -> ! {
    if refusal == PortableLockRefusal::Stderr {
        eprintln!(
            "error: this portable copy cannot forward remote control because {error}. Use the running copy's tray icon or shortcuts."
        );
        std::process::exit(1);
    }

    let message = format!(
        "Sona could not open this portable copy because {error}. Close the other copy, then try again."
    );
    let script =
        "on run argv\ndisplay alert \"Sona\" message (item 1 of argv) as critical\nend run";

    match std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .arg("--")
        .arg(&message)
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("error: {message} Native alert exited with {status}"),
        Err(error) => eprintln!("error: {message} Native alert unavailable: {error}"),
    }
    std::process::exit(1);
}

/// The ASR plan a headless run transcribes under: the active mode's, exactly as
/// a dictation would freeze it.
///
/// Reading the legacy global roots instead made this flag reproduce a
/// configuration no dictation uses. Filler removal, vocabulary and literal
/// punctuation are carried by the mode, and `literal_punctuation` has no global
/// at all — `AsrPlan::from_settings` hardcodes it false — so a check run through
/// this flag certified the wrong plan while looking like evidence.
/// `active_mode_id` is persisted, so nothing was missing here; it was simply not
/// being read. The mode still inherits every field it does not carry (the model
/// when its own id is empty, the global vocabulary, the text-pass settings that
/// live only at the root).
fn headless_asr_plan(settings: &crate::settings::AppSettings) -> modes::AsrPlan {
    match modes::active_mode(settings) {
        Some(mode) => modes::AsrPlan::from_mode(settings, &mode.asr),
        // Unreachable through a loaded document: every load runs
        // `ensure_mode_settings`, which seeds the default modes.
        None => modes::AsrPlan::from_settings(settings),
    }
}

/// Headless one-shot transcription for the `--transcribe-file` / `--list-devices`
/// path. Drives the same `TranscriptionManager::transcribe` the app uses; no
/// mic, no VAD, no download. Returns a process exit code (0 ok, 1 runtime
/// failure, 2 bad input/usage).
fn run_headless_transcription(app: &AppHandle, args: &CliArgs) -> i32 {
    use std::time::{Duration, Instant};

    // --list-devices: print registered compute devices (with indices) and exit.
    // Useful on multi-GPU machines to discover the index for --device-index.
    if args.list_devices {
        let devices = crate::managers::transcription::describe_compute_devices();
        if devices.is_empty() {
            println!("No transcribe-cpp compute devices registered.");
        } else {
            println!("transcribe-cpp compute devices:");
            for d in &devices {
                println!("  {}", d);
            }
        }
        if args.transcribe_file.is_none() {
            return 0;
        }
    }

    // --list-models: print the model registry (catalog + on-disk + custom) with
    // their ids — the same ids `--model` accepts — then exit. `--json` emits the
    // full ModelInfo array for scripting.
    if args.list_models {
        let model_manager = app.state::<Arc<ModelManager>>();
        let models = model_manager.get_available_models();
        if args.json {
            match serde_json::to_string_pretty(&models) {
                Ok(s) => println!("{}", s),
                Err(e) => {
                    eprintln!("error: failed to serialize models: {}", e);
                    return 1;
                }
            }
        } else if models.is_empty() {
            println!("No models available.");
        } else {
            println!("Available models (✓ = installed):");
            let width = models.iter().map(|m| m.id.len()).max().unwrap_or(0);
            for m in &models {
                let mark = if m.is_downloaded { "✓" } else { " " };
                let rec = if m.is_recommended {
                    "  [recommended]"
                } else {
                    ""
                };
                println!(
                    "  {}  {:<width$}  {}{}",
                    mark,
                    m.id,
                    m.name,
                    rec,
                    width = width
                );
            }
        }
        if args.transcribe_file.is_none() {
            return 0;
        }
    }

    let Some(wav) = args.transcribe_file.clone() else {
        return 0;
    };

    // read_wav_samples reads 16-bit int samples and does no validation; the app
    // only ever saves 16 kHz mono 16-bit PCM, so reject anything else rather than
    // transcribe garbage / mis-time / mis-decode.
    match hound::WavReader::open(&wav) {
        Ok(reader) => {
            let spec = reader.spec();
            if spec.sample_rate != 16_000
                || spec.channels != 1
                || spec.bits_per_sample != 16
                || spec.sample_format != hound::SampleFormat::Int
            {
                eprintln!(
                    "error: expected 16 kHz mono 16-bit PCM WAV, got {} Hz / {} ch / {}-bit {:?}",
                    spec.sample_rate, spec.channels, spec.bits_per_sample, spec.sample_format
                );
                return 2;
            }
        }
        Err(e) => {
            eprintln!("error: cannot open {}: {}", wav.display(), e);
            return 2;
        }
    }

    let samples = match crate::audio_toolkit::read_wav_samples(&wav) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read {}: {}", wav.display(), e);
            return 2;
        }
    };
    let sample_count = match u64::try_from(samples.len()) {
        Ok(count) => count,
        Err(_) => {
            eprintln!("error: audio file is too large to transcribe");
            return 2;
        }
    };
    let audio_secs = (Duration::from_secs(sample_count / 16_000)
        + Duration::from_nanos((sample_count % 16_000) * 62_500))
    .as_secs_f64();

    let tm = app.state::<Arc<TranscriptionManager>>();

    let settings = get_settings(app);
    let mode_id = modes::active_mode(&settings)
        .map(|mode| mode.id.clone())
        .unwrap_or_default();
    let mut asr = headless_asr_plan(&settings);
    if let Some(model_id) = args.model.clone() {
        asr.model_id = model_id;
    }
    if asr.model_id.is_empty() {
        eprintln!("error: no model selected (pass --model or pick one in the app)");
        return 2;
    }
    let model_id = asr.model_id.clone();

    // --device-index hard-selects a compute device by its --list-devices registry
    // index (transcribe-cpp / whisper-family models only; not persisted). Omit it
    // to use the persisted accelerator setting.
    let device_index = args.device_index;
    let requested_device = match device_index {
        Some(idx) => format!("index {}", idx),
        None => "settings".to_string(),
    };

    // Cold load (timed).
    let load_start = Instant::now();
    if let Err(e) = tm.load_model_with_device(&model_id, device_index) {
        eprintln!("error: load_model('{}') failed: {}", model_id, e);
        return 1;
    }
    let load_ms = u64::try_from(load_start.elapsed().as_millis()).unwrap_or(u64::MAX);
    let bound_backend = tm.current_backend();

    let runs = args.repeat.unwrap_or(1).max(1);
    let mut times_ms: Vec<u64> = Vec::new();
    let mut text = String::new();
    for i in 0..runs {
        // If the model's unload-timeout is "Immediately", transcribe() unloads
        // the engine after each run; reload (untimed) so repeats keep working
        // and the inference timing below stays clean.
        if !tm.is_model_loaded() {
            if let Err(e) = tm.load_model_with_device(&model_id, device_index) {
                eprintln!("error: reload before run {} failed: {}", i + 1, e);
                return 1;
            }
        }
        let t = Instant::now();
        match tm.transcribe_shared(&asr, &samples) {
            Ok(out) => text = out.text,
            Err(e) => {
                eprintln!("error: transcribe failed: {}", e);
                return 1;
            }
        }
        times_ms.push(u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX));
    }
    let best_ms = times_ms.iter().copied().min().unwrap_or(0);
    let rtf = if best_ms > 0 {
        audio_secs / Duration::from_millis(best_ms).as_secs_f64()
    } else {
        0.0
    };

    if args.json {
        println!(
            "{}",
            serde_json::json!({
                "mode": mode_id,
                "model": model_id,
                "requested_device": requested_device,
                "bound_backend": bound_backend,
                "audio_secs": audio_secs,
                "load_ms": load_ms,
                "transcribe_ms": times_ms,
                "best_ms": best_ms,
                "rtf": rtf,
                "text": text,
            })
        );
    } else {
        println!(
            "mode={} model={} device={} backend={} audio={:.2}s load={}ms best={}ms rtf={:.2}x",
            mode_id,
            model_id,
            requested_device,
            bound_backend.as_deref().unwrap_or("?"),
            audio_secs,
            load_ms,
            best_ms,
            rtf,
        );
        println!("text: {}", text);
    }
    0
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run(cli_args: CliArgs) {
    // Avoid ggml-metal residency-set teardown assertions when a native engine
    // outlives the Tauri shutdown sequence (#1902). This must happen before
    // transcribe-cpp initializes its Metal device. Advanced users can restore
    // upstream residency behavior with SONA_METAL_RESIDENCY=1.
    #[cfg(target_os = "macos")]
    if std::env::var("SONA_METAL_RESIDENCY").as_deref() == Ok("1") {
        // ggml treats GGML_METAL_NO_RESIDENCY as presence-based, so remove an
        // inherited value as well when explicitly opting back in.
        std::env::remove_var("GGML_METAL_NO_RESIDENCY");
    } else {
        std::env::set_var("GGML_METAL_NO_RESIDENCY", "1");
    }

    // Pin glibc's dynamic mmap threshold before the first large allocation,
    // so per-dictation transient buffers are returned to the OS on free
    // instead of accumulating in malloc arenas (#1792). No-op off Linux/glibc.
    memory::init_allocator();

    // Detect portable mode before anything else
    portable::init();

    if cli_args.agent_panel_public_identity {
        match tauri::async_runtime::block_on(agent_panel::cli_public_identity()) {
            Ok(identity) => match serde_json::to_string(&identity) {
                Ok(json) => println!("{json}"),
                Err(error) => {
                    eprintln!("error: failed to serialize public identity: {error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("error: failed to access public identity: {error:?}");
                std::process::exit(1);
            }
        }
        return;
    }

    let context = tauri::generate_context!();

    // Parse console logging directives from RUST_LOG, falling back to info-level logging
    // when the variable is unset
    let console_filter = build_console_filter();
    let headless_mode = is_headless_mode(&cli_args);
    // The native shell spawned this process and draws every surface itself,
    // so the core keeps no webview and answers over the socket instead.
    let native_socket = cli_args.native_socket.clone();
    let native_mode = native_socket.is_some();

    // Claim the portable GUI runtime before the logger or any Tauri plugin
    // can write under this root. Headless CLI/MCP commands retain their
    // existing concurrent path; the upstream macOS singleton cannot do this.
    #[cfg(target_os = "macos")]
    let portable_data_lock = if headless_mode {
        None
    } else {
        match portable::claim_instance_lock() {
            Ok(lock) => lock,
            Err(error) => exit_for_portable_lock_error(error, portable_lock_refusal(&cli_args)),
        }
    };

    if !headless_mode {
        launch_trace::start();
    }

    let specta_builder = Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            upstream_import::get_upstream_import_status,
            upstream_import::import_legacy_app,
            upstream_import::revert_upstream_import_settings,
            identity_adoption::get_identity_adoption_status,
            identity_adoption::revert_identity_adoption,
            agent_bridge::get_agent_bridge_status,
            agent_bridge::get_agent_bridge_sessions,
            agent_bridge::get_agent_bridge_requests,
            agent_bridge::get_agent_bridge_pending_messages,
            agent_bridge::set_agent_bridge_master,
            agent_bridge::set_agent_bridge_agent_enabled,
            agent_bridge::authorize_agent_bridge_project,
            agent_bridge::remove_agent_bridge_project,
            agent_bridge::create_agent_bridge_reply_preview,
            agent_bridge::confirm_agent_bridge_reply,
            agent_bridge::cancel_agent_bridge_message,
            agent_bridge::dismiss_agent_bridge_request,
            agent_bridge::create_agent_bridge_permission_rule,
            agent_bridge::delete_agent_bridge_permission_rule,
            agent_bridge::respond_agent_bridge_permission,
            agent_bridge::get_agent_bridge_hook_snippet,
            agent_panel::agent_panel_status,
            agent_panel::agent_panel_send_turn,
            agent_panel::agent_panel_cancel_turn,
            agent_panel::agent_panel_apply_change,
            agent_panel::agent_panel_undo_change,
            agent_panel::agent_panel_apply_action,
            agent_panel::agent_panel_dismiss_action,
            agent_panel::agent_chat_history_list,
            agent_panel::agent_chat_open,
            agent_panel::agent_chat_new,
            agent_panel::agent_panel_public_identity,
            agent_panel::change_agent_panel_enabled_setting,
            agent_panel::set_agent_panel_pairing,
            agent_panel::clear_agent_panel_pairing,
            agent_panel::agent_panel_test_connection,
            modes::get_modes,
            modes::set_active_mode,
            modes::upsert_mode,
            modes::delete_mode,
            modes::reorder_modes,
            commands::vocabulary::list_vocabulary_entries,
            modes::capture_mode_activation_rule,
            modes::remove_mode_activation_rule,
            modes::capture_mode_website_activation_rule,
            modes::remove_mode_website_activation_rule,
            commands::vocabulary::update_vocabulary_entries,
            commands::vocabulary::preview_vocabulary_csv,
            commands::vocabulary::apply_vocabulary_csv,
            commands::vocabulary::export_vocabulary_csv,
            commands::vocabulary::update_emoji_replacements,
            commands::vocabulary::update_emoji_replacements_enabled,
            commands::vocabulary::add_vocabulary_correction,
            commands::vocabulary::get_text_replacements,
            commands::vocabulary::save_text_replacements,
            commands::vocabulary::reset_text_replacements,
            commands::vocabulary::update_text_replacements_enabled,
            commands::vocabulary::update_spoken_edits_enabled,
            commands::hud::hud_pill_state,
            commands::hud::set_hud_pill_enabled,
            commands::hud::set_hud_pill_position,
            commands::hud::hud_toggle_recording,
            commands::hud::hud_open_mode_menu,
            commands::persona::get_persona_samples,
            commands::persona::save_persona_samples,
            commands::snippets::list_snippets,
            commands::snippets::upsert_snippet,
            commands::snippets::delete_snippet,
            commands::snippets::set_snippet_enabled,
            commands::snippets::set_snippets_enabled,
            settings::change_context_policy_ceiling_setting,
            settings::change_context_url_capture_enabled_setting,
            settings::change_external_query_enabled_setting,
            settings::change_external_mutations_enabled_setting,
            settings::change_meeting_local_engine_setting,
            settings::change_meeting_remote_intelligence_enabled_setting,
            settings::change_meeting_digest_enabled_setting,
            settings::change_meeting_digest_minute_of_day_setting,
            settings::accept_cloud_stt_provider_consent,
            settings::accept_post_process_provider_consent,
            command_mode::change_command_mode_enabled_setting,
            shortcut::change_binding,
            shortcut::reset_binding,
            shortcut::change_ptt_setting,
            shortcut::change_audio_feedback_setting,
            shortcut::change_audio_feedback_volume_setting,
            shortcut::change_sound_theme_setting,
            shortcut::change_theme_setting,
            shortcut::change_appearance_material_setting,
            shortcut::change_start_hidden_setting,
            shortcut::change_autostart_setting,
            shortcut::change_translate_to_english_setting,
            shortcut::change_selected_language_setting,
            shortcut::change_overlay_position_setting,
            shortcut::change_english_spelling_setting,
            shortcut::change_overlay_style_setting,
            shortcut::change_debug_mode_setting,
            shortcut::change_word_correction_threshold_setting,
            shortcut::change_extra_recording_buffer_setting,
            shortcut::get_available_typing_tools,
            shortcut::change_external_script_path_setting,
            shortcut::change_post_process_enabled_setting,
            shortcut::change_experimental_enabled_setting,
            shortcut::change_post_process_base_url_setting,
            secrets::get_provider_secret_state,
            secrets::set_provider_secret,
            secrets::delete_provider_secret,
            secrets::verify_stt_provider_secret,
            shortcut::change_post_process_model_setting,
            shortcut::set_post_process_provider,
            shortcut::discover_post_process_model_catalog,
            shortcut::add_post_process_prompt,
            shortcut::update_post_process_prompt,
            shortcut::delete_post_process_prompt,
            shortcut::set_post_process_selected_prompt,
            shortcut::suspend_all_bindings,
            shortcut::resume_all_bindings,
            shortcut::change_mute_while_recording_setting,
            shortcut::change_append_trailing_space_setting,
            shortcut::change_lazy_stream_close_setting,
            shortcut::change_vad_enabled_setting,
            shortcut::change_filler_word_removal_enabled_setting,
            shortcut::change_app_language_setting,
            shortcut::change_show_whats_new_on_update_setting,
            shortcut::change_whats_new_last_seen_version_setting,
            shortcut::change_keyboard_implementation_setting,
            shortcut::get_keyboard_implementation,
            shortcut::change_show_tray_icon_setting,
            shortcut::change_transcribe_accelerator_setting,
            shortcut::change_ort_accelerator_setting,
            shortcut::change_transcribe_gpu_device,
            shortcut::get_available_accelerators,
            shortcut::handy_keys::start_handy_keys_recording,
            shortcut::handy_keys::stop_handy_keys_recording,
            secure_input::get_secure_input_status,
            secure_input::run_keyboard_diagnostic,
            show_main_window_command,
            commands::cancel_operation,
            commands::is_portable,
            commands::get_app_dir_path,
            commands::get_app_settings,
            commands::get_default_settings,
            context::get_context_diagnostics,
            commands::get_log_dir_path,
            commands::set_log_level,
            commands::open_recordings_folder,
            commands::open_log_dir,
            commands::open_app_data_dir,
            commands::open_license_notices,
            commands::check_apple_intelligence_available,
            commands::initialize_enigo,
            commands::initialize_shortcuts,
            commands::models::get_available_models,
            commands::models::get_model_info,
            commands::models::download_model,
            commands::models::delete_model,
            commands::models::cancel_download,
            commands::models::set_active_model,
            commands::models::get_current_model,
            commands::models::get_transcription_model_status,
            commands::models::is_model_loading,
            commands::models::rescan_local_models,
            commands::audio::update_microphone_mode,
            commands::audio::get_microphone_mode,
            commands::audio::get_windows_microphone_permission_status,
            commands::audio::open_microphone_privacy_settings,
            commands::audio::get_available_microphones,
            commands::audio::set_selected_microphone,
            commands::audio::get_selected_microphone,
            commands::audio::get_available_output_devices,
            commands::audio::set_selected_output_device,
            commands::audio::get_selected_output_device,
            commands::audio::play_test_sound,
            commands::audio::check_custom_sounds,
            commands::audio::set_clamshell_microphone,
            commands::audio::get_clamshell_microphone,
            commands::audio::is_recording,
            commands::audio::get_microphone_channels,
            commands::audio::set_selected_channel,
            commands::transcription::set_model_unload_timeout,
            commands::transcription::get_model_load_status,
            commands::transcription::unload_model_manually,
            commands::media_import::import_audio_file,
            commands::media_import::cancel_audio_import,
            commands::media_import::list_audio_import_jobs,
            commands::recorder::recorder_preflight,
            commands::recorder::recorder_preview_start,
            commands::recorder::recorder_preview_stop,
            commands::recorder::recorder_start,
            commands::recorder::recorder_pause,
            commands::recorder::recorder_resume,
            commands::recorder::recorder_stop,
            commands::recorder::recorder_cancel,
            commands::voice_identity::voice_identity_status,
            commands::voice_identity::voice_identify_speaker,
            commands::voice_identity::voice_enroll_profile,
            commands::voice_identity::voice_remove_profile,
            commands::history::get_history_stats,
            commands::history::get_history_trend,
            commands::history::get_history_entries,
            commands::history::search_history_entries,
            commands::history::get_history_run_receipts,
            commands::history::toggle_history_entry_saved,
            commands::history::read_history_audio_chunk,
            commands::history::delete_history_entry,
            commands::history::retry_history_entry_transcription,
            commands::history::reprocess_history_entry,
            commands::history::update_history_limit,
            commands::history::update_recording_retention_period,
            commands::history::history_storage_status,
            commands::meeting::meeting_suggestions_list,
            commands::meeting::meeting_preflight_create,
            commands::meeting::meeting_preflight_refresh,
            commands::meeting::meeting_preflight_cancel,
            commands::meeting::meeting_start,
            commands::meeting::meeting_consent_panel_start,
            commands::meeting::meeting_consent_panel_active_state,
            commands::meeting::meeting_consent_panel_forget_series,
            commands::meeting::meeting_consent_panel_fit_disclosure,
            commands::meeting::meeting_announce_disclosure,
            commands::meeting::meeting_trash_list,
            commands::meeting::meeting_trash_restore,
            commands::meeting::meeting_pause,
            commands::meeting::meeting_resume,
            commands::meeting::meeting_stop,
            commands::meeting::meeting_discard,
            commands::meeting::meeting_import_recording,
            commands::meeting::meeting_import_transcript,
            commands::meeting::meeting_recovery_list,
            commands::meeting::meeting_recovery_finalize,
            commands::meeting::meeting_list,
            commands::meeting::meeting_trend,
            commands::meeting::meeting_get,
            commands::meeting::meeting_search,
            commands::meeting::meeting_title_set,
            commands::meeting::meeting_speaker_rename,
            commands::meeting::meeting_speaker_merge,
            commands::meeting::meeting_segment_edit,
            commands::meeting::meeting_note_create,
            commands::meeting::meeting_note_update,
            commands::meeting::meeting_note_delete,
            commands::meeting::meeting_artifacts_regenerate,
            commands::meeting::meeting_question_forget,
            commands::meeting::meeting_export,
            commands::meeting::produce_ledger_html,
            commands::meeting::meeting_delete,
            commands::meeting::meeting_retention_get,
            commands::meeting::meeting_local_engine_status,
            commands::meeting::meeting_retention_set,
            commands::meeting::meeting_remote_cancel,
            commands::meeting::get_meeting_analytics,
            commands::meeting::list_keyword_trackers,
            commands::meeting::save_keyword_trackers,
            commands::meeting::set_action_item_done,
            commands::meeting::get_meeting_user_notes,
            commands::meeting::save_meeting_user_notes,
            commands::meeting::reenhance_meeting_with_notes,
            commands::meeting::meeting_catch_up,
            commands::meeting::meeting_series_template_get,
            commands::meeting::meeting_series_template_for_session,
            commands::meeting::meeting_series_template_set,
            commands::meeting::meeting_series_digest_set,
            commands::meeting::meeting_series_always_record_set,
            commands::meeting::meeting_series_remote_opt_out_set,
            commands::meeting::meeting_series_remote_roster,
            commands::upcoming::meeting_upcoming_events,
            commands::people::people_list,
            commands::people::person_detail,
            commands::people::organization_detail,
            commands::people::person_summary_regenerate,
            commands::people::person_context,
            commands::people::meeting_people_context,
            commands::people::person_rename,
            commands::people::person_merge,
            commands::people::person_split,
            commands::people::person_delete,
            commands::people::link_confirm,
            commands::people::link_remove,
            commands::people::link_add_manual,
            commands::people::open_loops_inbox,
            commands::people::vocabulary_candidates,
            commands::loops::meeting_loops,
            commands::loops::meeting_loop_resolve,
            commands::loops::meeting_loop_reopen,
            commands::loops::meeting_loop_assign,
            commands::followup::meeting_follow_up_draft,
            commands::followup::meeting_follow_up_mail,
            commands::workflows::workflows_list,
            commands::workflows::workflow_set_enabled,
            commands::workflows::workflow_runs,
            commands::learning::learning_suggestions,
            commands::learning::learning_decide,
            commands::documents::doc_ingest,
            commands::documents::doc_list,
            commands::documents::doc_delete,
            commands::cloud_sync::cloud_sync_overview_get,
            commands::cloud_sync::cloud_sync_meeting_status_get,
            commands::cloud_sync::cloud_sync_meeting_status_list,
            commands::cloud_sync::cloud_sync_bootstrap,
            commands::cloud_sync::cloud_sync_recover,
            commands::cloud_sync::cloud_sync_pairing_offer,
            commands::cloud_sync::cloud_sync_pairing_approve,
            commands::cloud_sync::cloud_sync_pairing_fingerprint,
            commands::cloud_sync::cloud_sync_pairing_accept,
            commands::cloud_sync::cloud_sync_pause,
            commands::cloud_sync::cloud_sync_resume,
            commands::cloud_sync::cloud_sync_retry,
            commands::cloud_sync::cloud_sync_conflict_resolve,
            commands::cloud_sync::cloud_share_create,
            commands::cloud_sync::cloud_browser_share_create,
            commands::cloud_sync::cloud_share_revoke,
            commands::cloud_sync::cloud_share_import_file,
            commands::cloud_sync::cloud_sync_service_status,
            commands::updates::check_for_updates,
            commands::updates::change_update_check_enabled_setting,
            helpers::clamshell::is_laptop,
            commands::detection::detection_status_get,
            commands::detection::detection_calendar_access_request,
            commands::detection::detection_notification_access_request,
            commands::detection::detection_prompt_respond,
            commands::detection::detection_prompt_panel_ack,
            commands::detection::meeting_ritual_panel_ack,
            commands::detection::meeting_ritual_respond,
            commands::detection::detection_running_meeting_apps,
            commands::detection::detection_settings_set,
            commands::query::sona_query_search,
            commands::query::sona_query_events,
            commands::query::sona_query_pack,
            commands::query::sona_open_link,
            commands::automations::meeting_series_automations_get,
            commands::automations::meeting_series_automations_for_session,
            commands::automations::meeting_series_automation_set,
            commands::automations::meeting_automation_roster,
            commands::automations::meeting_automation_runs,
            commands::prompts::saved_prompt_list,
            commands::prompts::saved_prompt_save,
            commands::prompts::saved_prompt_delete,
            commands::prompts::saved_prompt_run,
            commands::prompts::saved_prompt_runs,
        ])
        .events(collect_events![
            upstream_import::UpstreamImportProgressEvent,
            agent_bridge::AgentBridgeUpdateEvent,
            agent_panel::AgentPanelStatusChangedEvent,
            agent_panel::AgentPanelTurnChangedEvent,
            agent_panel::AgentPanelProposalChangedEvent,
            modes::ModesChangedEvent,
            managers::audio::DictationRecordingChangedEvent,
            managers::history::HistoryUpdatePayload,
            managers::transcription::StreamTextEvent,
            managers::transcription::StreamPhaseEvent,
            managers::transcription::StreamEngineEvent,
            managers::media_import::AudioImportUpdateEvent,
            managers::media_import::AudioImportRoutedEvent,
            recorder::RecorderStateChangedEvent,
            meeting::types::MeetingSuggestionChangedEvent,
            meeting::types::MeetingSessionChangedEvent,
            meeting::types::MeetingSourceHealthChangedEvent,
            meeting::types::MeetingTranscriptChangedEvent,
            meeting::types::MeetingNoteChangedEvent,
            meeting::types::MeetingArtifactChangedEvent,
            meeting::types::MeetingRemoteJobChangedEvent,
            meeting::types::MeetingRemovedEvent,
            cloud_sync::types::CloudSyncChangedEvent,
            meeting::types::MeetingNavigationRequestedEvent,
            meeting::detection::DetectionPromptEvent,
            meeting::detection::DetectionPromptRetractedEvent,
            meeting::detection::MeetingRitualEvent,
            meeting::detection::MeetingRitualRetractedEvent,
            meeting::detection::DetectionStatus,
            meeting::digest::CaptureRequestedEvent,
            tray::TraySettingsRequestedEvent,
            tray::TrayCopyFailedEvent,
            query::QueryLinkRequestedEvent,
        ]);

    // The export keeps `src/bindings.ts` in step for the webview. A core the
    // native shell spawned has no webview, and runs from inside the app bundle
    // where that path does not exist.
    #[cfg(debug_assertions)]
    if !native_mode {
        const BINDINGS_PATH: &str = "../src/bindings.ts";
        if let Err(error) = specta_builder.export(
            Typescript::default().bigint(BigIntExportBehavior::Number),
            BINDINGS_PATH,
        ) {
            panic!("Failed to export TypeScript bindings: {error}");
        }
        let source = match std::fs::read_to_string(BINDINGS_PATH) {
            Ok(source) => source,
            Err(error) => panic!("Failed to read exported TypeScript bindings: {error}"),
        };
        let mut normalized = source
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        normalized.push('\n');
        if let Err(error) = std::fs::write(BINDINGS_PATH, normalized) {
            panic!("Failed to normalize exported TypeScript bindings: {error}");
        }
    }

    let invoke_handler = specta_builder.invoke_handler();

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .device_event_filter(tauri::DeviceEventFilter::Always)
        // Every webview boots from an initialization script that writes the
        // conservative `solid` material, so each document has to be told the
        // material actually in force once it loads — otherwise a reload (or the
        // overlay window being created long after startup) silently drops Glass.
        .on_page_load(|webview, payload| {
            if webview.label() == "main"
                && payload.event() == tauri::webview::PageLoadEvent::Started
            {
                launch_trace::mark_webview_navigation_started();
            }
            shortcut::reassert_window_material(webview);
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            LogBuilder::new()
                .level(log::LevelFilter::Trace) // Set to most verbose level globally
                .max_file_size(500_000)
                .rotation_strategy(RotationStrategy::KeepOne)
                .clear_targets()
                .targets([
                    // Console output respects RUST_LOG environment variable. In
                    // headless mode (--transcribe-file/--list-devices/--list-models)
                    // stdout carries only the result (JSON or plain), so send console
                    // logs to stderr instead to keep stdout clean for CI parsing.
                    // The native shell's core is a child process and logs the
                    // same way.
                    Target::new(if headless_mode || native_mode {
                        TargetKind::Stderr
                    } else {
                        TargetKind::Stdout
                    })
                    .filter({
                        let console_filter = console_filter.clone();
                        move |metadata| console_filter.enabled(metadata)
                    }),
                    // File logs respect the user's settings (stored in FILE_LOG_LEVEL atomic).
                    //
                    // Headless invocations get their own file, and that is not tidiness.
                    // Sharing one path with the running app is a data-loss bug: the app
                    // holds its fd open for a whole session while a CLI process lives a
                    // couple of hundred milliseconds, so when a CLI write crosses
                    // `max_file_size` the rotation replaces the *directory entry* and the
                    // app's fd keeps pointing at the old inode. From that moment the app
                    // appends to an unlinked file whose bytes are freed when it exits, and
                    // nothing will ever rotate it again. Measured on a real install: the
                    // app's fd held 526431 bytes — past the 500_000 cap — while the path
                    // resolved to a different inode and `find -inum` found no entry for
                    // the one being written. So any user who ever runs the CLI silently
                    // detaches their app's logging, and every diagnostic they send after
                    // that is empty. One writer per file is what prevents it; it also
                    // stops CLI traffic evicting the app's own history.
                    Target::new({
                        let file_name =
                            Some(if headless_mode { "sona-cli" } else { "sona" }.to_string());
                        if let Some(data_dir) = portable::data_dir() {
                            TargetKind::Folder {
                                path: data_dir.join("logs"),
                                file_name,
                            }
                        } else {
                            TargetKind::LogDir { file_name }
                        }
                    })
                    .filter(|metadata| {
                        metadata.target() == "sona::launch" || {
                            let file_level = FILE_LOG_LEVEL.load(Ordering::Relaxed);
                            metadata.level() <= level_filter_from_u8(file_level)
                        }
                    }),
                    // Stream logs to the webview (via the `log://log` event) so the
                    // debug panel's live log viewer can show them in real time. Only
                    // active while debug mode is on (its sole consumer), and shares the
                    // file log level so the "Log Level" setting controls verbosity.
                    Target::new(TargetKind::Webview).filter(|metadata| {
                        WEBVIEW_LOG_STREAMING.load(Ordering::Relaxed)
                            && metadata.level()
                                <= level_filter_from_u8(FILE_LOG_LEVEL.load(Ordering::Relaxed))
                    }),
                ])
                .build(),
        );

    #[cfg(target_os = "macos")]
    {
        builder = builder.plugin(tauri_nspanel::init());
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if autostart::should_install_autostart_plugin() {
        builder = builder.plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![]),
        ));
    }

    // Single-instance forwards CLI args to an already-running Sona and exits.
    // That would make the headless path
    // (--transcribe-file/--list-devices/--list-models) a silent no-op whenever the
    // app is already open, so skip it in headless mode and run a standalone
    // instance instead. The native shell's core is owned by the shell that
    // spawned it, so it never hands itself to another instance either.
    if !headless_mode && !native_mode && uses_bundle_single_instance(portable::is_portable()) {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            // Windows and Linux deliver a registered protocol URL as argv, and
            // `opened_audio_files` is the variadic positional, so every
            // `sona://…` link forwarded here also looks like a file to import.
            // The route table owns those addresses; the importer never sees one.
            //
            let handled = if let Some(control) = forwarded_control(&args) {
                // A remote-control invocation is terminal: it must not also
                // import a file, route a link, or raise the window.
                match control {
                    ForwardedControl::ToggleTranscription => {
                        signal_handle::send_transcription_intent(
                            app,
                            modes::TranscriptionIntent::ActiveMode,
                            "CLI",
                        );
                        "toggled transcription"
                    }
                    ForwardedControl::TogglePostProcess => {
                        signal_handle::send_transcription_intent(
                            app,
                            modes::TranscriptionIntent::ActiveModeWithPostProcess,
                            "CLI",
                        );
                        "toggled transcription with post-processing"
                    }
                    ForwardedControl::Cancel => {
                        crate::utils::cancel_current_operation(app);
                        "cancelled the current operation"
                    }
                }
            } else {
                // Route `sona://` values before they become relative paths.
                let opened_audio_paths: Vec<PathBuf> = CliArgs::try_parse_from(args.clone())
                    .map(|parsed| parsed.opened_audio_files)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|path| {
                        path.to_str()
                            .map_or(true, |raw| deeplink::deep_link_route(raw).is_none())
                    })
                    .collect();
                let opened_audio_queued = enqueue_opened_paths(
                    app,
                    forwarded_opened_audio_paths(&opened_audio_paths, &cwd),
                );

                let deep_link_dispatched = args
                    .iter()
                    .any(|argument| dispatch_deep_link(app, argument));

                match forwarded_open_action(deep_link_dispatched, opened_audio_queued) {
                    ForwardedOpenAction::DeepLink => {
                        // Windows and Linux deliver a registered protocol URL as argv to
                        // a second process, which this plugin forwards here. macOS uses
                        // RunEvent::Opened instead.
                        "dispatched a deep link"
                    }
                    ForwardedOpenAction::OpenedAudio => {
                        show_main_window(app);
                        "queued opened files and raised the window"
                    }
                    ForwardedOpenAction::RaiseWindow => {
                        // A second process was launched without remote-control flags
                        // (e.g. the binary run from a shell). On macOS, relaunching the
                        // bundle from Spotlight/Finder/Dock does not start a process —
                        // it arrives as RunEvent::Reopen below — but treat this the
                        // same way: raise the window and recreate a possibly vanished
                        // tray icon (#1948).
                        #[cfg(target_os = "macos")]
                        tray::recreate_tray_icon(app);
                        show_main_window(app);
                        "raised the window and recreated the tray icon"
                    }
                }
            };
            log::info!("Second instance forwarded {args:?} from {cwd}: {handled}");
        }));
    }

    let app = match builder
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_macos_permissions::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(cli_args.clone())
        .setup(move |app| {
            {
                let _span = launch_trace::span("migrations");
                identity_adoption::adopt_before_startup(app.handle())?;
            }
            let secret_manager = Arc::new(secrets::SecretManager::native());
            let migration_pending =
                settings::legacy_provider_secret_migration_pending(app.handle());
            let legacy_secret_cutover_pending =
                settings::legacy_provider_secret_cutover_pending(app.handle());
            secret_manager.set_migration_pending(migration_pending);
            app.manage(secret_manager.clone());

            specta_builder.mount_events(app);

            // Headless read-only corpus query (`--query` / `--meetings` /
            // `--meeting` / `--transcript` / `--loops` / `--people` /
            // `--events`): mount the two managers the query plane reads
            // through and nothing else. No model, no engine, no mic — this
            // path never transcribes, and building the transcription stack for
            // it would load a GGUF to answer a SELECT. The consent gate lives
            // in `query::external::answer`, so nothing below opens the store
            // until the setting says it may.
            if query::external::is_external_query(&cli_args) {
                let app_handle = app.handle().clone();
                let history_manager = Arc::new(HistoryManager::new(&app_handle)?);
                let meeting_secrets =
                    Arc::clone(&app_handle.state::<Arc<secrets::SecretManager>>());
                let meeting_manager =
                    Arc::new(MeetingSessionManager::new(&app_handle, meeting_secrets));
                app_handle.manage(history_manager);
                app_handle.manage(meeting_manager);

                let handle = app_handle.clone();
                let args = cli_args.clone();
                std::thread::spawn(move || {
                    let code = run_headless_guarded(|| query::external::run_cli(&handle, &args));
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                    let _ = std::io::stderr().flush();
                    std::process::exit(code);
                });
                return Ok(());
            }

            // Headless one-shot path (`--transcribe-file` / `--list-devices` /
            // `--list-models`): initialize only what transcription needs — the
            // store/paths plugins, the model + transcription managers, and the
            // transcribe-cpp backend + accelerator settings — then run on a worker
            // thread and exit. Deliberately skips the window, tray, overlay, audio
            // recorder (so it never opens the mic, even with always_on_microphone),
            // signal handlers, and autostart that initialize_core_logic sets up.
            if headless_mode {
                let app_handle = app.handle().clone();
                let model_manager = Arc::new(ModelManager::new(&app_handle)?);
                let transcription_manager = Arc::new(TranscriptionManager::new(
                    &app_handle,
                    model_manager.clone(),
                )?);
                app_handle.manage(model_manager);
                app_handle.manage(transcription_manager);
                managers::transcription::init_transcribe_backend();
                managers::transcription::apply_accelerator_settings(&app_handle);

                let handle = app_handle.clone();
                let args = cli_args.clone();
                std::thread::spawn(move || {
                    let code = run_headless_guarded(|| run_headless_transcription(&handle, &args));
                    // Drop the loaded engine before teardown: ggml-metal's global
                    // device free asserts (SIGABRT) if a model's Metal resources
                    // are still alive at C++ static-destructor time.
                    if let Some(tm) = handle.try_state::<Arc<TranscriptionManager>>() {
                        let _ = tm.unload_model();
                    }
                    // process::exit (not app.exit, which exits 0 regardless) so the
                    // exit code propagates to the shell for CI gating. Flush first
                    // since process::exit runs no destructors / buffer flushes.
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                    let _ = std::io::stderr().flush();
                    std::process::exit(code);
                });
                return Ok(());
            }

            let paint_app = app.handle().clone();
            app.listen(launch_trace::FIRST_DOM_PAINT_EVENT, move |event| {
                match serde_json::from_str::<f64>(event.payload()) {
                    Ok(epoch_ms) => launch_trace::mark_first_dom_paint(epoch_ms),
                    Err(error) => log::warn!("Invalid first DOM paint mark: {error}"),
                }
                if launch_trace::shell_shown() {
                    let _ = paint_app.emit(launch_trace::SHELL_VISIBLE_EVENT, ());
                }
            });
            let activation_app = app.handle().clone();
            app.listen(launch_trace::FIRST_VISIBLE_FRAME_EVENT, move |event| {
                let epoch_ms = match serde_json::from_str::<f64>(event.payload()) {
                    Ok(epoch_ms) => epoch_ms,
                    Err(error) => {
                        log::warn!("Invalid first visible frame mark: {error}");
                        return;
                    }
                };
                launch_trace::mark_first_visible_frame(epoch_ms);
                if let Some(main_window) = activation_app.get_webview_window("main") {
                    activate_main_window(&activation_app, &main_window);
                }
            });

            let settings = get_settings(app.handle());
            // Keep the first window non-activating until the webview reports a
            // composited frame. This prevents macOS from focusing empty chrome.
            // The native shell's core stays here for good: the shell owns the
            // Dock tile, and a second one would be a bug.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            modes::refresh_clipboard_context_watcher(&settings);

            // The chat panel serves both shells: a native Chat page asks it
            // for status on its first paint, and `state()` on an unmanaged
            // type is a panic, not an error.
            app.manage(agent_panel::AgentPanelManager::new(app.handle()));

            if !native_mode {
                // Create main window programmatically so we can set data_directory
                // for portable mode (redirects WebView2 cache to portable Data dir)
                let mut win_builder = tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    tauri::WebviewUrl::App("/".into()),
                )
                .title("Sona")
                .inner_size(900.0, 800.0)
                .min_inner_size(900.0, 800.0)
                .resizable(true)
                .maximizable(true)
                .minimizable(true)
                .fullscreen(false)
                .visible(false);

                if let Some(data_dir) = portable::data_dir() {
                    win_builder = win_builder.data_directory(data_dir.join("webview"));
                }

                #[cfg(target_os = "macos")]
                {
                    win_builder = win_builder
                        .transparent(true)
                        .title_bar_style(tauri::TitleBarStyle::Overlay)
                        .hidden_title(true);
                }

                win_builder = win_builder
                    .initialization_script(main_window_material_init(settings.appearance_material));
                let _main_window = win_builder.build()?;
                launch_trace::mark_native_window_created();

                // Glass is opt-in now, so vibrancy is applied only when the setting
                // asks for it. This also corrects the initialization script above if
                // the native view could not be applied — still before the window is
                // shown, so a failed apply costs a log line and nothing visible.
                shortcut::apply_window_material(app.handle(), settings.appearance_material);

                // Apply the persisted appearance theme to the native title bar before
                // the window is shown, so it matches the in-app palette without a flash
                // of the wrong theme. See `apply_window_theme` for what this does per
                // platform.
                #[cfg(any(target_os = "windows", target_os = "macos"))]
                shortcut::apply_window_theme(app.handle(), settings.theme);
            }

            let runtime = startup_runtime(&settings, &cli_args);
            let tauri_log_level: tauri_plugin_log::LogLevel = runtime.log_level.into();
            let file_log_level: log::Level = tauri_log_level.into();
            // Store the file log level in the atomic for the filter to use
            FILE_LOG_LEVEL.store(
                level_filter_code(file_log_level.to_level_filter()),
                Ordering::Relaxed,
            );
            // Only forward logs to the webview while debug mode is on (the live log
            // viewer is the sole consumer and only exists in debug mode).
            WEBVIEW_LOG_STREAMING.store(runtime.debug_mode, Ordering::Relaxed);
            let app_handle = app.handle().clone();
            app.manage(TranscriptionCoordinator::new(app_handle.clone()));
            // Reveal the styled launch shell first. Its DOM-paint event below
            // releases manager construction, including catalog and HF scans.
            // The native shell draws its own launch surface, so there is no
            // paint to wait for and startup completes here in `setup`.
            let shell_shown_before_startup = !native_mode && runtime.show_window_before_core;
            if shell_shown_before_startup {
                show_main_window(&app_handle);
            }

            let startup_app = app_handle.clone();
            let complete_startup = move || {
                if let Err(error) = initialize_core_logic(&startup_app, runtime) {
                    log::error!("Startup initialization failed: {error:#}");
                    startup_app.exit(1);
                    return;
                }

                // The shell connects as soon as this socket exists, so it is
                // bound only now, with every manager its requests read in place.
                if let Some(path) = &native_socket {
                    if let Err(error) = native_bridge::start(&startup_app, path) {
                        log::error!(
                            "Native shell socket {} is unavailable: {error}",
                            path.display()
                        );
                        startup_app.exit(1);
                        return;
                    }
                    native_bridge::watch_parent(startup_app.clone());
                }

                for address in opened_deep_link_addresses(&cli_args.opened_audio_files) {
                    dispatch_deep_link(&startup_app, address);
                }

                let opened_audio_queued = enqueue_opened_paths(
                    &startup_app,
                    initial_opened_audio_paths(&cli_args.opened_audio_files),
                );

                if legacy_secret_cutover_pending {
                    let migration_app = startup_app.clone();
                    let migration_secrets = secret_manager.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = secrets::migrate_legacy_provider_secrets(
                            &migration_app,
                            migration_secrets,
                        )
                        .await;
                    });
                }

                // Secure Input monitor (macOS): detects stuck secure input that
                // silently blocks keyed shortcuts and activates the Carbon fallback.
                secure_input::init(&startup_app);
                overlay::update_overlay_enabled_cache(
                    !native_mode && settings.overlay_style != settings::OverlayStyle::None,
                );

                std::thread::spawn(|| {
                    let _ = crate::managers::transcription::get_available_accelerators();
                });
                let prewarm_audio = startup_app.clone();
                std::thread::spawn(move || {
                    if let Some(manager) = prewarm_audio.try_state::<Arc<AudioRecordingManager>>() {
                        manager.prewarm();
                    }
                });

                let should_force_show = should_force_show_permissions_window(&startup_app);
                if !shell_shown_before_startup
                    && !native_mode
                    && (should_force_show || opened_audio_queued)
                {
                    show_main_window(&startup_app);
                }

                // The overlay and the consent panel are webviews; the native
                // shell draws both itself.
                if !native_mode {
                    let ready_app = startup_app.clone();
                    if let Err(error) = startup_app.run_on_main_thread(move || {
                        initialize_recording_overlay(&ready_app);
                        meeting::consent_panel::create(&ready_app);
                        if let Err(error) = ready_app.emit(launch_trace::BACKEND_READY_EVENT, ()) {
                            log::error!("Failed to release the launch shell: {error}");
                            ready_app.exit(1);
                        }
                    }) {
                        log::error!("Failed to schedule launch completion: {error}");
                        startup_app.exit(1);
                        return;
                    }
                }

                // Keyring work starts only after the shell paint that triggered
                // this callback, so an OS prompt always has an owning surface.
                let unlock_history = Arc::clone(&startup_app.state::<Arc<HistoryManager>>());
                let unlock_secrets = secret_manager.clone();
                tauri::async_runtime::spawn(async move {
                    unlock_history.unlock_storage(&unlock_secrets).await;
                    // The one trigger retention has that is not a new dictation.
                    //
                    // `cleanup_old_entries` otherwise runs only from an insert and
                    // from the two settings commands, and for `preserve_limit` that
                    // is enough: a count can only be exceeded by an insert, and an
                    // insert runs the sweep. The three time-based periods are made
                    // eligible by time passing instead, and nothing else observes
                    // time passing — so "delete after 3 days" kept every recording
                    // for a user who chose it and then stopped dictating.
                    //
                    // Here rather than in `unlock_storage`, which is the obvious
                    // home and a trap: `ensure_storage_unlocked` reaches that
                    // function on the read path, which is how the headless corpus
                    // verbs open the database, so a sweep inside it would make
                    // `sona --query` delete recordings while `cli.rs` promises
                    // those verbs only read. This callback belongs to a process
                    // that has a window, and it runs after the paint.
                    //
                    // Once per launch, deliberately. A timer would be a second
                    // source of truth about when deletion happens.
                    if let Err(error) = unlock_history.cleanup_old_entries() {
                        log::warn!("Retention sweep at startup is unavailable: {error:#}");
                    }
                });
            };
            if native_mode {
                complete_startup();
            } else {
                app_handle.once(launch_trace::FIRST_DOM_PAINT_EVENT, move |_| {
                    complete_startup()
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            let label = window.label();
            match event {
                tauri::WindowEvent::Focused(true) if label == "main" => {
                    launch_trace::mark_window_focus();
                }
                tauri::WindowEvent::CloseRequested { api, .. } if label == "main" => {
                    let settings = get_settings(window.app_handle());
                    let cli_args = window.app_handle().state::<CliArgs>();
                    if main_window_close_action(&settings, &cli_args) == MainWindowCloseAction::Exit
                    {
                        window.app_handle().exit(0);
                        return;
                    }

                    api.prevent_close();
                    let _ = window.hide();

                    #[cfg(target_os = "macos")]
                    if let Err(error) = window
                        .app_handle()
                        .set_activation_policy(tauri::ActivationPolicy::Accessory)
                    {
                        log::error!("Failed to set activation policy: {error}");
                    }
                }
                tauri::WindowEvent::Destroyed if label == "main" => {
                    /* The surface the chat sheet lives in is gone. Nothing is
                     * left to read an answer, so the poll loop stops with it —
                     * the relay job is cancelled at shutdown, not here, because
                     * the app can outlive its window. */
                    if let Some(panel) = window
                        .app_handle()
                        .try_state::<agent_panel::AgentPanelManager>()
                    {
                        panel.stop_polling();
                    }
                }
                tauri::WindowEvent::ThemeChanged(theme) if label == "main" => {
                    log::info!("Theme changed to: {theme:?}");
                    utils::refresh_tray_icon(window.app_handle());
                }
                _ => {}
            }
        })
        .invoke_handler(invoke_handler)
        .build(context)
    {
        Ok(app) => app,
        Err(error) => {
            eprintln!("error: failed to build Tauri application: {error}");
            return;
        }
    };
    app.run(|app, event| match &event {
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Opened { urls } => {
            // The same slice carries file:// opens and sona:// deep links, so
            // each URL is offered to the deep-link router first and only then
            // treated as an audio file.
            let unrouted: Vec<tauri::Url> = urls
                .iter()
                .filter(|url| !dispatch_deep_link(app, url.as_str()))
                .cloned()
                .collect();
            let opened_audio_queued =
                enqueue_opened_paths(app, macos_opened_audio_paths(&unrouted));
            if opened_audio_queued {
                show_main_window(app);
            }
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => {
            // Fired when the already-running bundle is launched again from
            // Spotlight/Finder or the Dock icon is clicked. If the settings
            // window is hidden, the user is likely looking for a tray icon
            // that vanished (#1948): recreate it. When the window is
            // already visible this is just a focus request and the tray is
            // left alone.
            let window_visible = app
                .get_webview_window("main")
                .and_then(|w| w.is_visible().ok())
                .unwrap_or(false);
            if !window_visible {
                tray::recreate_tray_icon(app);
            }
            show_main_window(app);
        }
        // Teardown transcribe.cpp before exit
        tauri::RunEvent::Exit => {
            if let Some(runtime) = app.try_state::<Arc<cloud_sync::CloudSyncRuntime>>() {
                runtime.shutdown();
            }
            if let Some(panel) = app.try_state::<agent_panel::AgentPanelManager>() {
                tauri::async_runtime::block_on(panel.shutdown());
            }
            if let Some(tm) = app.try_state::<Arc<TranscriptionManager>>() {
                let _ = tm.unload_model();
            }
        }
        _ => {}
    });

    #[cfg(target_os = "macos")]
    drop(portable_data_lock);
}

#[cfg(all(test, target_os = "macos"))]
mod single_instance_policy_tests {
    use super::uses_bundle_single_instance;

    #[test]
    fn portable_gui_does_not_use_the_bundle_wide_single_instance_socket() {
        assert!(!uses_bundle_single_instance(true));
        assert!(uses_bundle_single_instance(false));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod portable_lock_refusal_tests {
    use super::{portable_lock_refusal, PortableLockRefusal};
    use crate::cli::CliArgs;
    use clap::Parser;

    /// A remote-control flag is a message to the copy that already holds the
    /// Data directory, so a refusal belongs on the terminal that sent it. Only
    /// a launch raises the modal, because a launch is what was going to put a
    /// window on screen.
    #[test]
    fn only_a_launch_refuses_a_held_data_directory_through_the_modal() {
        let refusals = [
            vec!["sona", "--cancel"],
            vec!["sona", "--toggle-transcription"],
            vec!["sona", "--toggle-post-process"],
            vec!["sona"],
            vec!["sona", "recording.wav"],
        ]
        .map(|argv| {
            let args = CliArgs::try_parse_from(argv).expect("parse the invocation");
            portable_lock_refusal(&args)
        });

        assert_eq!(
            refusals,
            [
                PortableLockRefusal::Stderr,
                PortableLockRefusal::Stderr,
                PortableLockRefusal::Stderr,
                PortableLockRefusal::NativeAlert,
                PortableLockRefusal::NativeAlert,
            ]
        );
    }
}

#[cfg(test)]
mod startup_runtime_tests {
    use super::{main_window_close_action, startup_runtime, MainWindowCloseAction};
    use crate::cli::CliArgs;
    use crate::settings::{get_default_settings, LogLevel};
    use clap::Parser;

    #[test]
    fn start_hidden_keeps_the_shell_hidden_while_the_tray_is_available() {
        let settings = get_default_settings();
        let cli_args =
            CliArgs::try_parse_from(["sona", "--start-hidden"]).expect("parse --start-hidden");

        let runtime = startup_runtime(&settings, &cli_args);

        assert!(runtime.starts_hidden);
        assert!(runtime.tray_available);
        assert!(!runtime.show_window_before_core);
        assert_eq!(
            main_window_close_action(&settings, &cli_args),
            MainWindowCloseAction::Hide
        );
    }

    #[test]
    fn no_tray_forces_a_visible_shell_when_start_hidden_would_hide_it() {
        let mut settings = get_default_settings();
        settings.start_hidden = true;
        let cli_args = CliArgs::try_parse_from(["sona", "--no-tray"]).expect("parse --no-tray");

        let runtime = startup_runtime(&settings, &cli_args);

        assert!(!runtime.tray_available);
        assert!(runtime.show_window_before_core);
        assert_eq!(
            main_window_close_action(&settings, &cli_args),
            MainWindowCloseAction::Exit
        );
    }

    /// Every route that leaves no tray icon ends the process on close, not just
    /// the flag: `show_tray_icon` is user-togglable at runtime, and a hidden
    /// overlay window keeps the process alive after the main window is gone.
    #[test]
    fn a_close_with_no_tray_to_reopen_from_ends_the_process() {
        let cases = [(true, false), (false, false), (true, true), (false, true)].map(
            |(show_tray_icon, no_tray)| {
                let mut settings = get_default_settings();
                settings.show_tray_icon = show_tray_icon;
                let cli_args = if no_tray {
                    CliArgs::try_parse_from(["sona", "--no-tray"]).expect("parse --no-tray")
                } else {
                    CliArgs::try_parse_from(["sona"]).expect("parse a bare launch")
                };

                (
                    (show_tray_icon, no_tray),
                    main_window_close_action(&settings, &cli_args),
                )
            },
        );

        assert_eq!(
            cases,
            [
                ((true, false), MainWindowCloseAction::Hide),
                ((false, false), MainWindowCloseAction::Exit),
                ((true, true), MainWindowCloseAction::Exit),
                ((false, true), MainWindowCloseAction::Exit),
            ]
        );
    }

    #[test]
    fn debug_enables_trace_for_this_launch_without_persisting_debug_settings() {
        let mut settings = get_default_settings();
        settings.debug_mode = false;
        settings.log_level = LogLevel::Warn;
        let persisted_debug_settings = (settings.debug_mode, settings.log_level);
        let cli_args = CliArgs::try_parse_from(["sona", "--debug"]).expect("parse --debug");

        let runtime = startup_runtime(&settings, &cli_args);

        assert!(runtime.debug_mode);
        assert_eq!(runtime.log_level, LogLevel::Trace);
        assert_eq!(
            (settings.debug_mode, settings.log_level),
            persisted_debug_settings
        );
    }
}

#[cfg(test)]
mod forwarded_control_tests {
    use super::{forwarded_control, forwarded_open_action, ForwardedControl, ForwardedOpenAction};
    use crate::cli::CliArgs;
    use clap::Parser;

    #[test]
    fn remote_control_flags_select_one_terminal_action() {
        for (flag, expected) in [
            (
                "--toggle-transcription",
                ForwardedControl::ToggleTranscription,
            ),
            ("--toggle-post-process", ForwardedControl::TogglePostProcess),
            ("--cancel", ForwardedControl::Cancel),
        ] {
            CliArgs::try_parse_from(["sona", flag]).expect("parse remote control flag");
            let args = vec!["sona".to_string(), flag.to_string()];

            assert_eq!(forwarded_control(&args), Some(expected));
        }

        let conflicting = [
            "sona",
            "--toggle-transcription",
            "--toggle-post-process",
            "--cancel",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        assert_eq!(
            forwarded_control(&conflicting),
            Some(ForwardedControl::ToggleTranscription)
        );
    }

    #[test]
    fn bare_second_instance_launch_reaches_the_raise_window_fallback() {
        let args = vec!["sona".to_string()];

        assert_eq!(forwarded_control(&args), None);
        assert_eq!(
            forwarded_open_action(false, false),
            ForwardedOpenAction::RaiseWindow
        );
    }
}
#[cfg(test)]
mod headless_guard_tests {
    use super::{is_headless_mode, run_headless_guarded};
    use crate::cli::CliArgs;

    #[test]
    fn preserves_normal_exit_codes() {
        assert_eq!(run_headless_guarded(|| 2), 2);
    }

    #[test]
    fn converts_worker_panics_to_runtime_failures() {
        assert_eq!(run_headless_guarded(|| panic!("simulated failure")), 1);
    }

    #[test]
    fn treats_list_models_as_headless_before_binding_export() {
        let args = CliArgs {
            list_models: true,
            ..Default::default()
        };
        assert!(is_headless_mode(&args));
    }
}

#[cfg(test)]
mod headless_plan_tests {
    use super::headless_asr_plan;
    use crate::modes::ensure_mode_settings;
    use crate::settings::get_default_settings;

    /// The flag is the only route an automated check has to the text pipeline,
    /// so it has to freeze the plan a dictation would. Filler removal and
    /// literal punctuation are carried by the mode alone, and reading the roots
    /// instead reported the mode's choices as their global namesakes.
    #[test]
    fn a_headless_run_transcribes_under_the_active_mode() {
        let mut settings = get_default_settings();
        ensure_mode_settings(&mut settings);
        settings.selected_model = "globally-chosen-model".to_string();
        settings.filler_word_removal_enabled = true;
        settings.active_mode_id = settings.modes[1].id.clone();
        let mode = &mut settings.modes[1];
        mode.asr.model_id = "mode-chosen-model".to_string();
        mode.asr.literal_punctuation = true;
        mode.asr.filler_word_removal_enabled = false;

        let plan = headless_asr_plan(&settings);

        assert_eq!(plan.model_id, "mode-chosen-model");
        assert!(plan.literal_punctuation);
        assert!(!plan.filler_word_removal_enabled);
    }

    /// An empty per-mode model still inherits the global selection, which is
    /// what Aktan's `message` mode carries.
    #[test]
    fn an_empty_mode_model_still_inherits_the_global_selection() {
        let mut settings = get_default_settings();
        ensure_mode_settings(&mut settings);
        settings.selected_model = "globally-chosen-model".to_string();
        settings.modes[0].asr.model_id.clear();

        assert_eq!(
            headless_asr_plan(&settings).model_id,
            "globally-chosen-model"
        );
    }
}
#[cfg(test)]
mod opened_audio_tests {
    #[cfg(target_os = "macos")]
    use super::macos_opened_audio_paths;
    use super::{
        dispatch_opened_audio_paths, forwarded_opened_audio_paths, initial_opened_audio_paths,
        opened_deep_link_addresses,
    };
    use crate::commands::media_import::OpenedAudioImportFailure;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    fn rejected_path() -> OpenedAudioImportFailure {
        OpenedAudioImportFailure::unavailable()
    }

    #[test]
    fn initial_startup_enqueues_all_audio_paths_in_input_order() {
        let paths = vec![PathBuf::from("first.wav"), PathBuf::from("second.mp3")];
        let mut enqueued = Vec::new();
        let queued_any = dispatch_opened_audio_paths(
            initial_opened_audio_paths(&paths),
            |path| {
                enqueued.push(path.to_path_buf());
                Ok(())
            },
            |_, _| panic!("valid startup path was rejected"),
        );

        assert!(queued_any);
        assert_eq!(enqueued, paths);
    }

    #[test]
    fn mixed_opened_paths_continue_after_each_rejection_and_reveal_the_window() {
        let paths = vec![
            PathBuf::from("first.wav"),
            PathBuf::from("unsupported.mp4"),
            PathBuf::from("second.flac"),
        ];
        let mut enqueued = Vec::new();
        let mut rejected = Vec::new();
        let mut reveal_count = 0;
        let queued_any = dispatch_opened_audio_paths(
            initial_opened_audio_paths(&paths),
            |path| {
                if path.file_name() == Some(OsStr::new("unsupported.mp4")) {
                    Err(rejected_path())
                } else {
                    enqueued.push(path.to_path_buf());
                    Ok(())
                }
            },
            |path, _| {
                rejected.push(path.map(Path::to_path_buf));
                reveal_count += 1;
            },
        );

        assert!(queued_any);
        assert_eq!(
            enqueued,
            [PathBuf::from("first.wav"), PathBuf::from("second.flac")]
        );
        assert_eq!(rejected, [Some(PathBuf::from("unsupported.mp4"))]);
        assert_eq!(reveal_count, 1);
    }

    #[test]
    fn single_instance_forwarding_resolves_and_enqueues_every_path_in_order() {
        let forwarded = vec![
            PathBuf::from("first.wav"),
            PathBuf::from("invalid.mp4"),
            PathBuf::from("second.ogg"),
        ];
        let mut enqueued = Vec::new();
        let mut rejected = Vec::new();
        let mut reveal_count = 0;
        let queued_any = dispatch_opened_audio_paths(
            forwarded_opened_audio_paths(&forwarded, "/tmp/opened"),
            |path| {
                if path.file_name() == Some(OsStr::new("invalid.mp4")) {
                    Err(rejected_path())
                } else {
                    enqueued.push(path.to_path_buf());
                    Ok(())
                }
            },
            |path, _| {
                rejected.push(path.map(Path::to_path_buf));
                reveal_count += 1;
            },
        );

        assert!(queued_any);
        assert_eq!(
            enqueued,
            [
                PathBuf::from("/tmp/opened/first.wav"),
                PathBuf::from("/tmp/opened/second.ogg"),
            ]
        );
        assert_eq!(rejected, [Some(PathBuf::from("/tmp/opened/invalid.mp4"))]);
        assert_eq!(reveal_count, 1);
    }

    /// A `sona://` address in the opened-file list is skipped, not imported.
    ///
    /// Three callers hand `dispatch_opened_audio_paths` the URL list an
    /// operating system gave us, and the guard used to live in two of them.
    /// Measured on the shipped binary before this fix, own portable pid, own
    /// log, positive control at 9 hits: a cold start with `sona://quit` and a
    /// cold start with `sona://search?q=collagen` **both** logged
    /// `Rejected an operating-system opened audio file: Select a readable
    /// audio file.` and neither reached the route table. The second one is an
    /// address the query plane mints itself.
    #[test]
    fn a_sona_url_in_the_opened_list_is_never_an_audio_file() {
        let paths = vec![
            PathBuf::from("sona://quit"),
            PathBuf::from("sona://search?q=collagen"),
            PathBuf::from("sona:record"),
            PathBuf::from("real.wav"),
        ];
        let mut enqueued = Vec::new();
        let queued_any = dispatch_opened_audio_paths(
            initial_opened_audio_paths(&paths),
            |path| {
                enqueued.push(path.to_path_buf());
                Ok(())
            },
            |path, _| panic!("a sona:// address was reported as a bad audio file: {path:?}"),
        );

        assert!(queued_any);
        assert_eq!(
            enqueued,
            [PathBuf::from("real.wav")],
            "only the file is a file"
        );
    }

    /// The forwarded path keeps its own filter, and this is why.
    ///
    /// `resolve_opened_audio_path` joins a relative path onto the cwd, and
    /// `sona://quit` *is* relative as a `Path`. So by the time a forwarded
    /// argv reaches the funnel it reads `<cwd>/sona://quit` and no longer
    /// parses as a URL — the guard in the funnel cannot see it, and the filter
    /// at the forwarding call site has to run *before* resolution. Asserted
    /// rather than commented so that removing that filter as "now redundant"
    /// turns this red instead of silently restoring the defect.
    #[test]
    fn resolution_hides_a_url_from_the_funnel_guard() {
        let forwarded = vec![PathBuf::from("sona://quit")];
        let mut enqueued = Vec::new();
        dispatch_opened_audio_paths(
            forwarded_opened_audio_paths(&forwarded, "/tmp/opened"),
            |path| {
                enqueued.push(path.to_path_buf());
                Ok(())
            },
            |_, _| {},
        );

        assert_eq!(
            enqueued,
            [PathBuf::from("/tmp/opened/sona://quit")],
            "cwd-joined, so the funnel guard no longer recognises it as a URL"
        );
    }

    /// A cold start is the ingress with no router in front of the funnel.
    ///
    /// Windows and Linux start the binary with a registered `sona://` address
    /// in argv, where `opened_audio_files` collects it, so these strings have
    /// to reach the route table. The funnel then skips them; skipping alone
    /// left `sona://search?q=…` with no navigation, no error and no log line.
    #[test]
    fn a_cold_start_offers_every_sona_address_to_the_router_in_argv_order() {
        let argv = [
            PathBuf::from("first.wav"),
            PathBuf::from("sona://search?q=collagen"),
            PathBuf::from("second.flac"),
            PathBuf::from("sona://quit"),
        ];

        assert_eq!(
            opened_deep_link_addresses(&argv).collect::<Vec<_>>(),
            ["sona://search?q=collagen", "sona://quit"]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_opened_urls_enqueue_file_urls_and_report_non_file_urls() {
        let urls = [
            tauri::Url::from_file_path("/tmp/first.wav").expect("file URL"),
            tauri::Url::parse("https://example.com/recording.wav").expect("web URL"),
            tauri::Url::from_file_path("/tmp/second.m4a").expect("file URL"),
        ];
        let mut enqueued = Vec::new();
        let mut rejected = Vec::new();
        let mut reveal_count = 0;
        let queued_any = dispatch_opened_audio_paths(
            macos_opened_audio_paths(&urls),
            |path| {
                enqueued.push(path.to_path_buf());
                Ok(())
            },
            |path, _| {
                rejected.push(path.map(Path::to_path_buf));
                reveal_count += 1;
            },
        );

        assert!(queued_any);
        assert_eq!(
            enqueued,
            [
                PathBuf::from("/tmp/first.wav"),
                PathBuf::from("/tmp/second.m4a")
            ]
        );
        assert_eq!(rejected, [None]);
        assert_eq!(reveal_count, 1);
    }
}
