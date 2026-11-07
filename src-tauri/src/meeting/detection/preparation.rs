use super::{calendar::CalendarAccess, machine, DetectionRuntime, MeetingPrepCard};
use crate::meeting::heard_plan_types::HeardPlan;
use crate::meeting::store::MeetingStore;
use std::sync::{atomic::Ordering, Arc};
use std::time::Duration;

impl DetectionRuntime {
    pub(super) fn spawn_preparation_loop(self: &Arc<Self>) {
        let runtime = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            // The first pass waits two minutes so preparing never competes
            // with launch.
            let period = Duration::from_secs(15 * 60);
            let mut interval = tokio::time::interval_at(
                tokio::time::Instant::now() + Duration::from_secs(120),
                period,
            );
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(runtime) = runtime.upgrade() else {
                    return;
                };
                if runtime.stop.load(Ordering::Acquire) {
                    return;
                }
                let settings = crate::settings::get_settings(&runtime.app);
                if !settings.detection_calendar_enabled
                    || !settings.meeting_prep.overnight_enabled
                    || runtime.calendar.access() != CalendarAccess::Authorized
                {
                    continue;
                }
                // Today plus tomorrow, in the calendar's local timezone. The
                // same pass after wake fills anything that changed overnight.
                let (start, end) =
                    crate::meeting::upcoming::upcoming_window(chrono::Local::now(), 1);
                let calendar = Arc::clone(&runtime.calendar);
                let Ok(events) = tauri::async_runtime::spawn_blocking(move || {
                    calendar.events_between(start, end)
                })
                .await
                else {
                    continue;
                };
                for occurrence in events {
                    if runtime.stop.load(Ordering::Acquire) {
                        return;
                    }
                    let settings = crate::settings::get_settings(&runtime.app);
                    if !settings.detection_calendar_enabled
                        || !settings.meeting_prep.overnight_enabled
                    {
                        break;
                    }
                    if occurrence.summary.end_utc_ms <= super::utc_now_ms()
                        || occurrence.summary.attendee_count < super::machine::ATTENDEE_FLOOR
                    {
                        continue;
                    }
                    let calendar = Arc::clone(&runtime.calendar);
                    let key = occurrence.summary.event_key;
                    let Ok(Some(event)) =
                        tauri::async_runtime::spawn_blocking(move || calendar.event_by_key(&key))
                            .await
                    else {
                        continue;
                    };
                    // Errors are shown by the on-demand surface. No event title,
                    // attendee or email text goes into background logs.
                    let _ = runtime.meetings.prepare_brief(event, false).await;
                }
            }
        });
    }

    /// The prep for a meeting only a heard plan predicts: once the plan's
    /// time is within the reminder lead, the same card a calendar event gets.
    pub(super) fn spawn_plan_prep_loop(self: &Arc<Self>) {
        let runtime = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            // Half the shortest lead, so no plan's start falls between passes.
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(runtime) = runtime.upgrade() else {
                    return;
                };
                if runtime.stop.load(Ordering::Acquire) {
                    return;
                }
                runtime.present_plan_preps(super::utc_now_ms()).await;
            }
        });
    }

    async fn present_plan_preps(self: &Arc<Self>, now_utc_ms: i64) {
        let settings = crate::settings::get_settings(&self.app);
        if !self.enabled.load(Ordering::Acquire) || !settings.meeting_prep.upcoming_alerts_enabled {
            return;
        }
        let lead_ms = i64::from(settings.meeting_prep.alert_minutes.clamp(1, 60)) * 60_000;
        let Ok(store) = self.meetings.store().await else {
            return;
        };
        let Ok(plans) = store.heard_plans_starting(now_utc_ms, now_utc_ms + lead_ms) else {
            return;
        };
        let reads_calendar = settings.detection_calendar_enabled
            && self.calendar.access() == CalendarAccess::Authorized;
        for plan in plans {
            let Some(card) = MeetingPrepCard::for_plan(plan.clone()) else {
                continue;
            };
            let claimed = self.lock().briefing_events.insert(card.event_key.clone());
            if !claimed || (reads_calendar && self.invited_event_keeps(&store, &plan).await) {
                continue;
            }
            self.present_prep_card(card, now_utc_ms).await;
        }
    }

    /// Whether an invited event the calendar prep will brief keeps `plan`,
    /// so its card carries the plan and this one would be a second.
    async fn invited_event_keeps(&self, store: &MeetingStore, plan: &HeardPlan) -> bool {
        let Some(window) = plan.window else {
            return false;
        };
        let calendar = Arc::clone(&self.calendar);
        let Ok(events) = tauri::async_runtime::spawn_blocking(move || {
            calendar.events_between(window.start_utc_ms, window.end_utc_ms)
        })
        .await
        else {
            return false;
        };
        events
            .iter()
            .filter(|occurrence| occurrence.summary.attendee_count >= machine::ATTENDEE_FLOOR)
            .any(|occurrence| {
                store
                    .heard_plan_for_event(&occurrence.summary)
                    .ok()
                    .flatten()
                    .is_some_and(|kept| kept.plan_id == plan.plan_id)
            })
    }
}
