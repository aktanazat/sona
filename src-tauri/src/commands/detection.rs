use std::sync::Arc;

use tauri::State;

use crate::meeting::detection::calendar::CalendarAccess;
use crate::meeting::detection::notify::NotificationAccess;
use crate::meeting::detection::{
    DetectionRuntime, DetectionSettings, DetectionStatus, MeetingRitualAction,
};
use crate::meeting::types::MeetingSessionId;

/// Writes the operator's detection policy and returns the status it produces, so
/// the caller never has to re-read to see the effect of its own write.
#[tauri::command]
#[specta::specta]
pub fn detection_settings_set(
    runtime: State<'_, Arc<DetectionRuntime>>,
    settings: DetectionSettings,
) -> DetectionStatus {
    runtime.write_settings(settings)
}
/// Everything the operator can see about detection right now. The frontend reads
/// this on mount and then follows the `detection-status` event.
#[tauri::command]
#[specta::specta]
pub fn detection_status_get(runtime: State<'_, Arc<DetectionRuntime>>) -> DetectionStatus {
    runtime.status()
}

/// Asks for EventKit full access. Reading events needs full access — Apple
/// provides no read-only grant — so this only runs when the operator turns the
/// calendar sub-toggle on, never at launch.
#[tauri::command]
#[specta::specta]
pub async fn detection_calendar_access_request(
    runtime: State<'_, Arc<DetectionRuntime>>,
) -> Result<CalendarAccess, ()> {
    let runtime = Arc::clone(&runtime);
    Ok(
        tauri::async_runtime::spawn_blocking(move || runtime.request_calendar_access())
            .await
            .unwrap_or(CalendarAccess::NotDetermined),
    )
}

/// Asks for notification authorization. Separate from the calendar grant because
/// detection is still useful without notifications: the pre-meeting card and the
/// meetings list remain.
#[tauri::command]
#[specta::specta]
pub async fn detection_notification_access_request(
    runtime: State<'_, Arc<DetectionRuntime>>,
) -> Result<NotificationAccess, ()> {
    Ok(runtime.request_notification_access().await)
}

/// Answers a prompt from inside the app. Identical in effect to clicking the
/// notification's own buttons: accepting opens the preflight consent screen, and
/// nothing here starts a capture.
#[tauri::command]
#[specta::specta]
pub fn detection_prompt_respond(
    runtime: State<'_, Arc<DetectionRuntime>>,
    prompt_id: String,
    accepted: bool,
) {
    let runtime = Arc::clone(&runtime);
    runtime.respond(&prompt_id, accepted);
}

/// Confirms that the consent webview rendered the panel delivery. If this
/// bounded acknowledgement never arrives, the backend alone falls back to the
/// native notification tier.
#[tauri::command]
#[specta::specta]
pub fn detection_prompt_panel_ack(runtime: State<'_, Arc<DetectionRuntime>>, prompt_id: String) {
    runtime.acknowledge_panel(&prompt_id);
}

#[tauri::command]
#[specta::specta]
pub fn meeting_ritual_panel_ack(runtime: State<'_, Arc<DetectionRuntime>>, ritual_id: String) {
    runtime.acknowledge_panel(&ritual_id);
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_ritual_respond(
    runtime: State<'_, Arc<DetectionRuntime>>,
    ritual_id: String,
    action: MeetingRitualAction,
) -> Result<bool, ()> {
    let runtime = Arc::clone(&runtime);
    Ok(runtime.respond_ritual(&ritual_id, action).await)
}

/// The countdown's "Keep recording": the capture runs on past the end
/// detection saw. False when no countdown for the session is still running.
#[tauri::command]
#[specta::specta]
pub fn detection_keep_recording(
    runtime: State<'_, Arc<DetectionRuntime>>,
    session_id: MeetingSessionId,
) -> bool {
    runtime.keep_recording(session_id)
}

/// Allowlisted bundle IDs whose application is running right now. The settings UI
/// uses this to show an operator whether an entry they typed is real, which is
/// the runtime validation the allowlist needs to stay honest as vendors rename.
#[tauri::command]
#[specta::specta]
pub fn detection_running_meeting_apps(runtime: State<'_, Arc<DetectionRuntime>>) -> Vec<String> {
    runtime.running_meeting_apps()
}
