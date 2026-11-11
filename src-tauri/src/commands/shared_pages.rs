use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::MeetingStore;
use crate::shared_pages::{
    self, Draft, DraftEntry, Paths, ShareRecord, ShareState, SharedPagesErrorCode, SharedPreview,
    SiteSettings,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, State};

#[derive(Clone, Debug, Deserialize, Type)]
pub struct SharedDraftRequest {
    pub meeting_id: String,
}

#[derive(Clone, Debug, Deserialize, Type)]
pub struct SharedDraftBody {
    pub draft: Draft,
}

#[derive(Clone, Debug, Deserialize, Type)]
pub struct SharedRevokeRequest {
    pub slug: String,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct SharedEmpty {}

fn paths(app: &AppHandle) -> Result<Paths, SharedPagesErrorCode> {
    crate::portable::app_data_dir(app)
        .map(|dir| Paths::new(&dir))
        .map_err(|_| SharedPagesErrorCode::Failed)
}

/// The meeting store when meetings are open; a saved draft needs none.
async fn meetings(manager: &MeetingSessionManager) -> Option<Arc<MeetingStore>> {
    manager.store().await.ok()
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, shared_pages::SharedPagesError> + Send + 'static,
) -> Result<T, SharedPagesErrorCode> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|_| SharedPagesErrorCode::Failed)?
        .map_err(|error| error.code)
}

#[tauri::command]
#[specta::specta]
pub async fn shared_draft(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: SharedDraftRequest,
) -> Result<Draft, SharedPagesErrorCode> {
    let paths = paths(&app)?;
    let id = shared_pages::meeting_id(&request.meeting_id).map_err(|error| error.code)?;
    let store = meetings(&manager).await;
    blocking(move || shared_pages::draft(&paths, store.as_deref(), id)).await
}

#[tauri::command]
#[specta::specta]
pub async fn shared_draft_save(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: SharedDraftBody,
) -> Result<Draft, SharedPagesErrorCode> {
    let paths = paths(&app)?;
    let store = meetings(&manager).await;
    blocking(move || shared_pages::save_draft(&paths, store.as_deref(), &request.draft)).await
}

#[tauri::command]
#[specta::specta]
pub async fn shared_draft_discard(
    app: AppHandle,
    request: SharedDraftRequest,
) -> Result<SharedEmpty, SharedPagesErrorCode> {
    let paths = paths(&app)?;
    blocking(move || shared_pages::discard_draft(&paths, &request.meeting_id))
        .await
        .map(|()| SharedEmpty {})
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct SharedRecapAnswer {
    pub paragraphs: Vec<String>,
}

/// Summary paragraphs for someone who was not in the meeting, written by
/// the engine this meeting's series allows.
#[tauri::command]
#[specta::specta]
pub async fn shared_draft_recap(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: SharedDraftBody,
) -> Result<SharedRecapAnswer, SharedPagesErrorCode> {
    let store = meetings(&manager)
        .await
        .ok_or(SharedPagesErrorCode::NoStore)?;
    let meetings = Arc::clone(manager.inner());
    let draft = request.draft;
    blocking(move || shared_pages::recap(&store, meetings.processing(), &draft))
        .await
        .map(|paragraphs| SharedRecapAnswer { paragraphs })
}

#[tauri::command]
#[specta::specta]
pub async fn shared_drafts(app: AppHandle) -> Result<Vec<DraftEntry>, SharedPagesErrorCode> {
    let paths = paths(&app)?;
    blocking(move || shared_pages::drafts(&paths)).await
}

#[tauri::command]
#[specta::specta]
pub async fn shared_preview(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: SharedDraftBody,
) -> Result<SharedPreview, SharedPagesErrorCode> {
    let paths = paths(&app)?;
    let store = meetings(&manager).await;
    blocking(move || shared_pages::preview(&paths, store.as_deref(), &request.draft)).await
}

/// Publishes one meeting's page. Only the share sheet's Share button sends
/// this; it answers once the page is live, or still publishing after four
/// minutes of checking.
#[tauri::command]
#[specta::specta]
pub async fn shared_publish(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: SharedDraftBody,
) -> Result<ShareRecord, SharedPagesErrorCode> {
    let paths = paths(&app)?;
    let store = meetings(&manager).await;
    let pushed = {
        let paths = paths.clone();
        blocking(move || {
            let draft = request.draft;
            let audio = shared_pages::draft_audio(&paths, store.as_deref(), &draft)?;
            let record = shared_pages::publish(&paths, store.as_deref(), &draft, audio.as_deref())?;
            if record.for_person.is_some() {
                shared_pages::reset_published_draft(&paths, &draft.meeting_id);
            } else {
                shared_pages::forget_published_draft(&paths, &draft.meeting_id);
            }
            Ok(record)
        })
        .await?
    };
    shared_pages::settle(&paths, &pushed.slug, ShareState::Live)
        .await
        .map_err(|error| error.code)
}

#[tauri::command]
#[specta::specta]
pub async fn shared_list(app: AppHandle) -> Result<Vec<ShareRecord>, SharedPagesErrorCode> {
    let paths = paths(&app)?;
    shared_pages::list(&paths).await.map_err(|error| error.code)
}

#[tauri::command]
#[specta::specta]
pub async fn shared_revoke(
    app: AppHandle,
    request: SharedRevokeRequest,
) -> Result<ShareRecord, SharedPagesErrorCode> {
    let paths = paths(&app)?;
    let removing = {
        let paths = paths.clone();
        blocking(move || shared_pages::revoke(&paths, &request.slug)).await?
    };
    if removing.state != ShareState::Removing {
        return Ok(removing);
    }
    shared_pages::settle(&paths, &removing.slug, ShareState::Removed)
        .await
        .map_err(|error| error.code)
}

#[tauri::command]
#[specta::specta]
pub async fn shared_site_get(app: AppHandle) -> Result<Option<SiteSettings>, SharedPagesErrorCode> {
    let paths = paths(&app)?;
    blocking(move || paths.site()).await
}

#[tauri::command]
#[specta::specta]
pub async fn shared_site_set(
    app: AppHandle,
    request: SiteSettings,
) -> Result<SiteSettings, SharedPagesErrorCode> {
    let paths = paths(&app)?;
    blocking(move || shared_pages::set_site(&paths, request)).await
}
