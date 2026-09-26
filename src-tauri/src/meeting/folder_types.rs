//! Folders: a person's own grouping of meetings.
//!
//! A folder holds references, never meetings. One meeting can sit in several
//! folders, deleting a folder leaves every meeting in it where it was, and a
//! meeting that goes to the trash takes its places in its folders with it so a
//! restore puts them back.
//!
//! Every write is fenced on one shared revision and leaves an
//! [`OperationReceipt`], like every other user preference in the meeting store:
//! a replayed request returns the receipt it already wrote, and a stale one is
//! refused with the current revision so the caller can re-read and try again.

use super::analytics::MeetingNotesTemplate;
pub use super::types::MeetingFolderId;
use super::types::{MeetingOperationId, MeetingSessionId, OperationReceipt, SavedPromptId};
use serde::{Deserialize, Serialize};
use specta::Type;

/// The longest folder name, in characters.
pub const MAX_FOLDER_NAME_CHARS: usize = 80;

/// How many saved prompts one folder may run on each meeting filed in it.
/// Every one is a model call per meeting, so the list stays short.
pub const MAX_FOLDER_PROMPTS: usize = 5;

/// One folder as the meetings list shows it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingFolder {
    pub folder_id: MeetingFolderId,
    pub name: String,
    /// The notes template a meeting filed here is written with when neither
    /// the meeting nor its calendar series has chosen one.
    pub template: Option<MeetingNotesTemplate>,
    /// Saved meeting prompts that run on every meeting filed here once its
    /// notes are ready, in the order they were chosen.
    pub prompt_ids: Vec<SavedPromptId>,
    /// Meetings filed here right now. A meeting in the trash is not counted.
    pub meeting_count: u32,
    pub created_at_utc_ms: i64,
    pub updated_at_utc_ms: i64,
}

/// Every folder, by name, with the fence every folder write carries.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingFolderList {
    pub folders: Vec<MeetingFolder>,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingFolderCreateRequest {
    pub operation_id: MeetingOperationId,
    pub name: String,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingFolderRenameRequest {
    pub operation_id: MeetingOperationId,
    pub folder_id: MeetingFolderId,
    pub name: String,
    pub expected_revision: u64,
}

/// Delete one folder. The meetings in it stay where they are.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingFolderDeleteRequest {
    pub operation_id: MeetingOperationId,
    pub folder_id: MeetingFolderId,
    pub expected_revision: u64,
}

/// File one meeting in one folder, or take it out.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingFolderMembershipRequest {
    pub operation_id: MeetingOperationId,
    pub folder_id: MeetingFolderId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
}

/// What a folder does to the meetings filed in it: the notes template they
/// fall back to, and the saved prompts that run on them. Both are replaced
/// whole, because they are edited together on one sheet.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingFolderDefaultsSetRequest {
    pub operation_id: MeetingOperationId,
    pub folder_id: MeetingFolderId,
    pub template: Option<MeetingNotesTemplate>,
    pub prompt_ids: Vec<SavedPromptId>,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingFolderMutationResult {
    pub receipt: OperationReceipt,
    pub folders: MeetingFolderList,
}
