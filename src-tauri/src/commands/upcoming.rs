use std::sync::Arc;

use chrono::Local;
use tauri::State;

use crate::meeting::detection::DetectionRuntime;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::MeetingCommandError;
use crate::meeting::upcoming::upcoming_window;
use crate::meeting::upcoming_types::MeetingUpcomingEvents;

/// D28: today plus the next `days` local days of calendar, with each recurring
/// row joined to what its series remembers.
///
/// Two owners, wired here and nowhere else: the detection runtime owns the
/// calendar grant and the EventKit read, and the meeting store owns the series
/// records and the address book. Neither knows about the other, and this
/// command is the only place they meet.
///
/// The EventKit read is blocking and can walk a week of events, so it runs off
/// the async runtime's worker threads rather than on one of them.
#[tauri::command]
#[specta::specta]
pub async fn meeting_upcoming_events(
    manager: State<'_, Arc<MeetingSessionManager>>,
    detection: State<'_, Arc<DetectionRuntime>>,
    days: u32,
) -> Result<MeetingUpcomingEvents, MeetingCommandError> {
    let window = upcoming_window(Local::now(), days);
    let detection = Arc::clone(&detection);
    let enabled = crate::settings::get_settings(detection.app_handle()).detection_calendar_enabled;
    let access = if enabled { detection.calendar_access() }
        else { crate::meeting::detection::calendar::CalendarAccess::NotDetermined };
    let occurrences = if enabled {
        tauri::async_runtime::spawn_blocking(move || {
            detection.calendar_events_between(window.0, window.1)
        }).await.map_err(|_| MeetingCommandError::EngineFailure)?
    } else { Vec::new() };
    manager.upcoming_events(access, window, occurrences).await
}
