//! Folders as the windows reach them: each store call with the error mapping
//! every meeting command uses, and what filing a meeting sets off.

use super::folder_types::{
    MeetingFolderCreateRequest, MeetingFolderDefaultsSetRequest, MeetingFolderDeleteRequest,
    MeetingFolderList, MeetingFolderMembershipRequest, MeetingFolderMutationResult,
    MeetingFolderRenameRequest,
};
use super::session::MeetingSessionManager;
use super::types::{
    MeetingCommandError, MeetingDeletionJobId, MeetingFolderId, MeetingSessionId, OperationResult,
};
use super::workflow_engine::{map_store_error, now_utc_ms};

impl MeetingSessionManager {
    pub async fn folder_list(&self) -> Result<MeetingFolderList, MeetingCommandError> {
        self.store()
            .await?
            .meeting_folder_list()
            .map_err(map_store_error)
    }

    pub async fn folder_create(
        &self,
        request: MeetingFolderCreateRequest,
    ) -> Result<MeetingFolderMutationResult, MeetingCommandError> {
        self.store()
            .await?
            .create_meeting_folder(&request, now_utc_ms())
            .map_err(map_store_error)
    }

    pub async fn folder_rename(
        &self,
        request: MeetingFolderRenameRequest,
    ) -> Result<MeetingFolderMutationResult, MeetingCommandError> {
        self.store()
            .await?
            .rename_meeting_folder(&request, now_utc_ms())
            .map_err(map_store_error)
    }

    /// Delete one folder. The meetings in it stay.
    pub async fn folder_delete(
        &self,
        request: MeetingFolderDeleteRequest,
    ) -> Result<MeetingFolderMutationResult, MeetingCommandError> {
        self.store()
            .await?
            .delete_meeting_folder(&request, now_utc_ms())
            .map_err(map_store_error)
    }

    /// File one meeting in one folder. A meeting filed for the first time is
    /// handed to the connected apps, the same hand-off finished notes get, so
    /// a destination that follows this folder receives it.
    pub async fn folder_add_meeting(
        &self,
        request: MeetingFolderMembershipRequest,
    ) -> Result<MeetingFolderMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let already_filed = store
            .meeting_folder_added_at(request.session_id, request.folder_id)
            .map_err(map_store_error)?
            .is_some();
        let result = store
            .add_meeting_to_folder(&request, now_utc_ms())
            .map_err(map_store_error)?;
        if !already_filed && result.receipt.result == OperationResult::Committed {
            if let Some(app) = self.app_handle() {
                crate::integrations::after_notes_ready(store, app.clone(), request.session_id);
            }
        }
        Ok(result)
    }

    pub async fn folder_remove_meeting(
        &self,
        request: MeetingFolderMembershipRequest,
    ) -> Result<MeetingFolderMutationResult, MeetingCommandError> {
        self.store()
            .await?
            .remove_meeting_from_folder(&request, now_utc_ms())
            .map_err(map_store_error)
    }

    /// Replace a folder's notes template and the saved prompts it runs.
    pub async fn folder_set_defaults(
        &self,
        request: MeetingFolderDefaultsSetRequest,
    ) -> Result<MeetingFolderMutationResult, MeetingCommandError> {
        self.store()
            .await?
            .set_meeting_folder_defaults(&request, now_utc_ms())
            .map_err(map_store_error)
    }

    /// The folders one meeting is filed in, in folder-name order.
    pub async fn folders_for_session(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<MeetingFolderId>, MeetingCommandError> {
        self.store()
            .await?
            .meeting_folder_ids(session_id)
            .map_err(map_store_error)
    }

    /// Delete one trashed meeting for good, before its thirty days are up.
    pub async fn trash_delete_forever(
        &self,
        job_id: MeetingDeletionJobId,
    ) -> Result<(), MeetingCommandError> {
        self.store()
            .await?
            .delete_trashed_meeting(job_id)
            .map_err(map_store_error)
    }
}
