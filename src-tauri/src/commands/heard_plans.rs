use crate::integrations::calendar;
use crate::meeting::heard_plan_types::{HeardPlan, HeardPlanEdit};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::{HeardPlanId, MeetingSessionId};
use std::sync::Arc;
use tauri::State;

const UNAVAILABLE: &str = "The meeting library is unavailable.";

#[tauri::command]
#[specta::specta]
pub async fn meeting_heard_plans(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<Vec<HeardPlan>, String> {
    let store = store(&manager).await?;
    blocking(move || store.heard_plans(session_id)).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_heard_plan_not_now(
    manager: State<'_, Arc<MeetingSessionManager>>,
    plan_id: HeardPlanId,
) -> Result<HeardPlan, String> {
    let store = store(&manager).await?;
    blocking(move || store.heard_plan_not_now(plan_id)).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_heard_plan_edit(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: HeardPlanEdit,
) -> Result<HeardPlan, String> {
    let store = store(&manager).await?;
    blocking(move || store.heard_plan_edit(request)).await
}

/// Put a heard plan on the calendar, as heard or as the operator edited it.
/// Only ever on the operator's tap: the entry has no invitees and sends
/// nothing to anyone.
#[tauri::command]
#[specta::specta]
pub async fn meeting_heard_plan_add_to_calendar(
    manager: State<'_, Arc<MeetingSessionManager>>,
    plan_id: HeardPlanId,
) -> Result<HeardPlan, String> {
    let store = store(&manager).await?;
    let (plan, preferences) = {
        let store = Arc::clone(&store);
        blocking(move || Ok((store.heard_plan(plan_id)?, store.connection_preferences()?))).await?
    };
    if plan.on_calendar {
        return Err(message(StoreError::Conflict));
    }
    let Some(window) = plan.window.filter(|window| !window.all_day) else {
        return Err("Choose a time for this plan first.".into());
    };
    if !preferences.calendar_actions_enabled {
        return Err("Enable calendar actions in Settings > Connections first.".into());
    }
    let event_id = calendar::create(
        &plan.title,
        window.start_utc_ms,
        window.end_utc_ms,
        &crate::query::meeting_link(plan.session_id),
        plan.place.as_deref().unwrap_or_default(),
    )
    .await?;
    let recorded = {
        let event_id = event_id.clone();
        blocking(move || store.set_heard_plan_calendar_event(plan_id, Some(&event_id))).await
    };
    if recorded.is_err() {
        let _ = calendar::undo(&event_id).await;
    }
    recorded
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_heard_plan_undo_calendar(
    manager: State<'_, Arc<MeetingSessionManager>>,
    plan_id: HeardPlanId,
) -> Result<HeardPlan, String> {
    let store = store(&manager).await?;
    let event_id = {
        let store = Arc::clone(&store);
        blocking(move || store.heard_plan_calendar_event(plan_id)).await?
    };
    let Some(event_id) = event_id else {
        return Err("This plan is not on the calendar.".into());
    };
    calendar::undo(&event_id).await?;
    blocking(move || store.set_heard_plan_calendar_event(plan_id, None)).await
}

async fn store(manager: &MeetingSessionManager) -> Result<Arc<MeetingStore>, String> {
    manager.store().await.map_err(|_| UNAVAILABLE.to_string())
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, StoreError> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|_| UNAVAILABLE.to_string())?
        .map_err(message)
}

fn message(error: StoreError) -> String {
    match error {
        StoreError::NotFound => "This plan is no longer in the meeting.",
        StoreError::Invalid => "Give the plan a title, and an end after its start.",
        StoreError::Conflict => "This plan is on the calendar. Take it off first.",
        _ => UNAVAILABLE,
    }
    .to_string()
}
