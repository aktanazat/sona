//! The live doc through the meeting manager: the running capture's doc
//! while it records, the saved doc after.

use super::{map_store_error, MeetingSessionManager};
use crate::meeting::live_doc_types::{MeetingLiveDoc, MeetingLiveTarget};
use crate::meeting::processing::live_changes::{self, Author};
use crate::meeting::processing::live_doc::{self, EditRefusal};
use crate::meeting::processing::live_share::{SharedMeeting, SharedRooms};
use crate::meeting::processing::MeetingLiveCard;
use crate::meeting::store::MeetingStore;
use crate::meeting::types::{MeetingCommandError, MeetingSessionId};
use std::sync::Arc;

impl MeetingSessionManager {
    /// The running capture's doc, or the saved one once it has stopped.
    pub async fn live_doc(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingLiveDoc, MeetingCommandError> {
        if let Some(live) = self.live_transcript(session_id) {
            return Ok(live.live_doc(session_id));
        }
        let store = self.store().await?;
        tauri::async_runtime::spawn_blocking(move || saved(&store, session_id))
            .await
            .map_err(|_| MeetingCommandError::StorageUnavailable)?
    }

    /// Start the running capture's doc. The first reading of what was
    /// said so far runs in the background and announces the doc when done.
    pub async fn start_live_doc(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingLiveDoc, MeetingCommandError> {
        let live = self
            .live_transcript(session_id)
            .ok_or(MeetingCommandError::NotFound)?;
        if live.start_doc() {
            let store = self.store().await?;
            let processing = Arc::clone(&self.processing);
            let reading = Arc::clone(&live);
            tauri::async_runtime::spawn_blocking(move || {
                processing.read_live_doc_now(&store, session_id, &reading);
            });
        }
        Ok(live.live_doc(session_id))
    }

    /// A hand edit to a meeting's doc, live or saved: new text for a point,
    /// a new point when `point_id` is `None`, or the point gone when the
    /// text is blank.
    pub async fn edit_live_doc(
        &self,
        session_id: MeetingSessionId,
        point_id: Option<String>,
        text: String,
    ) -> Result<MeetingLiveDoc, MeetingCommandError> {
        let store = self.store().await?;
        if let Some(live) = self.live_transcript(session_id) {
            live.edit_doc(&Author::Owner, point_id.as_deref(), &text)
                .map_err(refused)?;
            let saving = Arc::clone(&live);
            tauri::async_runtime::spawn_blocking(move || {
                live_doc::save(&store, session_id, &saving);
            })
            .await
            .map_err(|_| MeetingCommandError::StorageUnavailable)?;
            return Ok(live.live_doc(session_id));
        }
        tauri::async_runtime::spawn_blocking(move || {
            let mut points = store.live_doc_points(session_id).map_err(map_store_error)?;
            let edited =
                live_doc::edit(&mut points, point_id.as_deref(), &text).map_err(refused)?;
            store
                .save_live_doc_points(session_id, &points)
                .map_err(map_store_error)?;
            let mut changes = store.live_changes(session_id).map_err(map_store_error)?;
            live_changes::push(
                &mut changes,
                Author::Owner.change(
                    None,
                    MeetingLiveTarget::Point,
                    &edited.point_id,
                    edited.kind,
                    &edited.text,
                ),
            );
            store
                .save_live_changes(session_id, &changes)
                .map_err(map_store_error)?;
            saved(&store, session_id)
        })
        .await
        .map_err(|_| MeetingCommandError::StorageUnavailable)?
    }

    /// New words and date for one promise on the running capture's card.
    pub fn edit_live_card(
        &self,
        session_id: MeetingSessionId,
        promise_id: &str,
        what: &str,
        due_at_ms: Option<i64>,
    ) -> Result<MeetingLiveCard, MeetingCommandError> {
        let live = self
            .live_transcript(session_id)
            .ok_or(MeetingCommandError::NotFound)?;
        live.edit_live_promise(&Author::Owner, promise_id, what, due_at_ms)
            .ok_or(MeetingCommandError::InvalidRequest)?;
        Ok(live.live_card(session_id))
    }

    /// The running capture as its link's room sees it. `NotFound` once
    /// the meeting has stopped.
    pub(crate) async fn shared_meeting(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<SharedMeeting, MeetingCommandError> {
        let live = self
            .live_transcript(session_id)
            .ok_or(MeetingCommandError::NotFound)?;
        Ok(SharedMeeting::new(
            Arc::clone(&self.processing),
            self.store().await?,
            session_id,
            live,
        ))
    }

    /// The rooms of meetings shared by link, and word of their deletes.
    pub(crate) fn shared_rooms(&self) -> &SharedRooms {
        self.processing.shared_rooms()
    }
}

fn saved(
    store: &MeetingStore,
    session_id: MeetingSessionId,
) -> Result<MeetingLiveDoc, MeetingCommandError> {
    let points = store.live_doc_points(session_id).map_err(map_store_error)?;
    Ok(MeetingLiveDoc {
        session_id,
        started: !points.is_empty(),
        points,
        asks: Vec::new(),
        model_requests: 0,
        live: false,
        changes: store.live_changes(session_id).map_err(map_store_error)?,
    })
}

fn refused(refusal: EditRefusal) -> MeetingCommandError {
    match refusal {
        EditRefusal::NotFound => MeetingCommandError::NotFound,
        EditRefusal::Invalid | EditRefusal::Full => MeetingCommandError::InvalidRequest,
    }
}
