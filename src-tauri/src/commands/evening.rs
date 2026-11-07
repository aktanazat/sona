use crate::managers::history::HistoryManager;
use crate::meeting::evening::HomeEvening;
use crate::meeting::session::MeetingSessionManager;
use std::sync::Arc;
use tauri::{AppHandle, State};

/// Home's end-of-day card: today's lines, and the recordings with a filing
/// suggestion waiting.
#[tauri::command]
#[specta::specta]
pub async fn home_evening(
    manager: State<'_, Arc<MeetingSessionManager>>,
    history: State<'_, Arc<HistoryManager>>,
) -> Result<HomeEvening, String> {
    manager.home_evening(&history).await
}

/// Sums up today now, on this Mac, and reviews any recording not yet filed.
#[tauri::command]
#[specta::specta]
pub async fn home_evening_write(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    history: State<'_, Arc<HistoryManager>>,
) -> Result<HomeEvening, String> {
    let pass = Arc::clone(&manager);
    let pass_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || pass.evening_now(&pass_app))
        .await
        .map_err(|_| "Sona could not sum up today.".to_string())??;
    manager.home_evening(&history).await
}

/// Files a recording where one suggestion says, or turns it down.
#[tauri::command]
#[specta::specta]
pub async fn filing_decide(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    history: State<'_, Arc<HistoryManager>>,
    suggestion_id: String,
    accept: bool,
) -> Result<HomeEvening, String> {
    manager
        .decide_filing(&app, &suggestion_id, accept, None)
        .await?;
    manager.home_evening(&history).await
}
