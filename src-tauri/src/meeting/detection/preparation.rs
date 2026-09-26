use super::{calendar::CalendarAccess, DetectionRuntime};
use std::sync::{atomic::Ordering, Arc};
use std::time::Duration;

impl DetectionRuntime {
    pub(super) fn spawn_preparation_loop(self: &Arc<Self>) {
        let runtime = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            // The first pass waits two minutes so preparing never competes
            // with launch.
            let period = Duration::from_secs(15 * 60);
            let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(120), period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(runtime) = runtime.upgrade() else { return; };
                if runtime.stop.load(Ordering::Acquire) { return; }
                let settings = crate::settings::get_settings(&runtime.app);
                if !settings.detection_calendar_enabled || !settings.meeting_prep.overnight_enabled
                    || runtime.calendar.access() != CalendarAccess::Authorized {
                    continue;
                }
                // Today plus tomorrow, in the calendar's local timezone. The
                // same pass after wake fills anything that changed overnight.
                let (start, end) = crate::meeting::upcoming::upcoming_window(chrono::Local::now(), 1);
                let calendar = Arc::clone(&runtime.calendar);
                let Ok(events) = tauri::async_runtime::spawn_blocking(move || calendar.events_between(start, end)).await else { continue; };
                for occurrence in events {
                    if runtime.stop.load(Ordering::Acquire) { return; }
                    let settings = crate::settings::get_settings(&runtime.app);
                    if !settings.detection_calendar_enabled || !settings.meeting_prep.overnight_enabled { break; }
                    if occurrence.summary.end_utc_ms <= super::utc_now_ms()
                        || occurrence.summary.attendee_count < super::machine::ATTENDEE_FLOOR {
                        continue;
                    }
                    let calendar = Arc::clone(&runtime.calendar);
                    let key = occurrence.summary.event_key;
                    let Ok(Some(event)) = tauri::async_runtime::spawn_blocking(move || calendar.event_by_key(&key)).await else { continue; };
                    // Errors are shown by the on-demand surface. No event title,
                    // attendee or email text goes into background logs.
                    let _ = runtime.meetings.prepare_brief(event, false).await;
                }
            }
        });
    }
}
