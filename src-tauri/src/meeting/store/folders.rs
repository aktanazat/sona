//! Folders at the store boundary: the rows, the fence, and what a folder means
//! to the meetings filed in it.
//!
//! Four tables. `meeting_folders` holds one row per folder with its name and
//! notes template. `meeting_folder_members` files one meeting in one folder.
//! `meeting_folder_prompts` lists the saved prompts a folder runs, in order.
//! `meeting_folder_trash` remembers where a trashed meeting was filed, keyed by
//! its deletion job, so a restore can put it back.
//!
//! Every foreign key cascades, and none of them points at a meeting from a
//! folder: deleting a folder takes its memberships, its prompt list and its
//! trash memory with it and never a meeting. Deleting a meeting's row takes its
//! memberships. Deleting a saved prompt takes it off every folder that ran it.
//!
//! Every write goes through [`write_fenced`] on one shared revision, the same
//! skeleton the series preferences and saved prompts use.

use super::fence::{write_fenced, Fence, FencedWrite};
use super::{id, parse_uuid, MeetingStore, StoreError};
use crate::meeting::analytics::MeetingNotesTemplate;
use crate::meeting::folder_types::{
    MeetingFolder, MeetingFolderCreateRequest, MeetingFolderDefaultsSetRequest,
    MeetingFolderDeleteRequest, MeetingFolderList, MeetingFolderMembershipRequest,
    MeetingFolderMutationResult, MeetingFolderRenameRequest, MAX_FOLDER_NAME_CHARS,
    MAX_FOLDER_PROMPTS,
};
use crate::meeting::types::{
    MeetingCommandKind, MeetingFolderId, MeetingOperationId, MeetingSessionId, SavedPromptId,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};

impl MeetingStore {
    /// Every folder, by name.
    pub(crate) fn meeting_folders(&self) -> Result<Vec<MeetingFolder>, StoreError> {
        let connection = self.connection()?;
        folders_in(&connection)
    }

    /// Every folder with the fence its writes carry.
    pub(crate) fn meeting_folder_list(&self) -> Result<MeetingFolderList, StoreError> {
        let connection = self.connection()?;
        folder_list_in(&connection)
    }

    /// The folders one meeting is filed in, in folder-name order. Empty for a
    /// meeting in no folder and for one that does not exist.
    pub(crate) fn meeting_folder_ids(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<MeetingFolderId>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT f.folder_id
               FROM meeting_folder_members m
               JOIN meeting_folders f ON f.folder_id = m.folder_id
              WHERE m.session_id = ?1
              ORDER BY f.name COLLATE NOCASE, f.folder_id",
        )?;
        let ids = statement
            .query_map(params![id(session_id)], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.iter()
            .map(|value| parse_uuid(value).map(MeetingFolderId::from_uuid))
            .collect()
    }

    /// When one meeting was filed in one folder, or `None` when it is not in
    /// it. A meeting restored from the trash keeps the time it was first
    /// filed.
    pub(crate) fn meeting_folder_added_at(
        &self,
        session_id: MeetingSessionId,
        folder_id: MeetingFolderId,
    ) -> Result<Option<i64>, StoreError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT added_at_utc_ms FROM meeting_folder_members
                  WHERE folder_id = ?1 AND session_id = ?2",
                params![id(folder_id), id(session_id)],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// One folder and every meeting filed in it, read together so a caller
    /// scoping a read to the folder sees one moment of it. `NotFound` when the
    /// folder is gone: a deleted folder is not an empty one.
    pub(crate) fn meeting_folder_members(
        &self,
        folder_id: MeetingFolderId,
    ) -> Result<(MeetingFolder, HashSet<MeetingSessionId>), StoreError> {
        let connection = self.connection()?;
        let folder = folders_in(&connection)?
            .into_iter()
            .find(|folder| folder.folder_id == folder_id)
            .ok_or(StoreError::NotFound)?;
        let mut statement = connection
            .prepare("SELECT session_id FROM meeting_folder_members WHERE folder_id = ?1")?;
        let ids = statement
            .query_map(params![id(folder_id)], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let members = ids
            .iter()
            .map(|value| parse_uuid(value).map(MeetingSessionId::from_uuid))
            .collect::<Result<HashSet<_>, _>>()?;
        Ok((folder, members))
    }

    /// The notes template the first of a meeting's folders asks for, by
    /// folder name, or `None` when none of them has one.
    pub(crate) fn folder_template_for_session(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Option<MeetingNotesTemplate>, StoreError> {
        let connection = self.connection()?;
        let stored: Option<String> = connection
            .query_row(
                "SELECT f.template_id
                   FROM meeting_folder_members m
                   JOIN meeting_folders f ON f.folder_id = m.folder_id
                  WHERE m.session_id = ?1 AND f.template_id IS NOT NULL
                  ORDER BY f.name COLLATE NOCASE, f.folder_id
                  LIMIT 1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .optional()?;
        stored.as_deref().map(decode_template).transpose()
    }

    /// The saved prompts a meeting's folders run, each once: folder-name
    /// order, then the order each folder lists them in.
    pub(crate) fn folder_prompt_ids_for_session(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<SavedPromptId>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT p.prompt_id
               FROM meeting_folder_members m
               JOIN meeting_folders f ON f.folder_id = m.folder_id
               JOIN meeting_folder_prompts p ON p.folder_id = f.folder_id
              WHERE m.session_id = ?1
              ORDER BY f.name COLLATE NOCASE, f.folder_id, p.position",
        )?;
        let ids = statement
            .query_map(params![id(session_id)], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen = HashSet::new();
        let mut prompts = Vec::new();
        for value in ids {
            let prompt_id = SavedPromptId::from_uuid(parse_uuid(&value)?);
            if seen.insert(prompt_id) {
                prompts.push(prompt_id);
            }
        }
        Ok(prompts)
    }

    pub(crate) fn create_meeting_folder(
        &self,
        request: &MeetingFolderCreateRequest,
        requested_at_utc_ms: i64,
    ) -> Result<MeetingFolderMutationResult, StoreError> {
        let name = folder_name(&request.name)?;
        let folder_id = MeetingFolderId::new();
        self.write_folders(
            request.operation_id,
            request.expected_revision,
            requested_at_utc_ms,
            MeetingCommandKind::FolderCreate,
            vec![id(folder_id)],
            |connection, now| {
                ensure_name_free_in(connection, &name, None)?;
                connection.execute(
                    "INSERT INTO meeting_folders (
                        folder_id, name, template_id, created_at_utc_ms, updated_at_utc_ms
                     ) VALUES (?1, ?2, NULL, ?3, ?3)",
                    params![id(folder_id), name, now],
                )?;
                Ok(())
            },
        )
    }

    pub(crate) fn rename_meeting_folder(
        &self,
        request: &MeetingFolderRenameRequest,
        requested_at_utc_ms: i64,
    ) -> Result<MeetingFolderMutationResult, StoreError> {
        let name = folder_name(&request.name)?;
        self.write_folders(
            request.operation_id,
            request.expected_revision,
            requested_at_utc_ms,
            MeetingCommandKind::FolderRename,
            vec![id(request.folder_id)],
            |connection, now| {
                ensure_name_free_in(connection, &name, Some(request.folder_id))?;
                let changed = connection.execute(
                    "UPDATE meeting_folders SET name = ?2, updated_at_utc_ms = ?3
                      WHERE folder_id = ?1",
                    params![id(request.folder_id), name, now],
                )?;
                if changed == 0 {
                    return Err(StoreError::NotFound);
                }
                Ok(())
            },
        )
    }

    /// Delete one folder. The meetings filed in it stay where they are; only
    /// the folder, its prompt list and its memberships go.
    pub(crate) fn delete_meeting_folder(
        &self,
        request: &MeetingFolderDeleteRequest,
        requested_at_utc_ms: i64,
    ) -> Result<MeetingFolderMutationResult, StoreError> {
        self.write_folders(
            request.operation_id,
            request.expected_revision,
            requested_at_utc_ms,
            MeetingCommandKind::FolderDelete,
            vec![id(request.folder_id)],
            |connection, _| {
                let changed = connection.execute(
                    "DELETE FROM meeting_folders WHERE folder_id = ?1",
                    params![id(request.folder_id)],
                )?;
                if changed == 0 {
                    return Err(StoreError::NotFound);
                }
                Ok(())
            },
        )
    }

    /// File one meeting in one folder. Filing it again changes nothing and
    /// keeps the time it was first filed.
    pub(crate) fn add_meeting_to_folder(
        &self,
        request: &MeetingFolderMembershipRequest,
        requested_at_utc_ms: i64,
    ) -> Result<MeetingFolderMutationResult, StoreError> {
        self.write_folders(
            request.operation_id,
            request.expected_revision,
            requested_at_utc_ms,
            MeetingCommandKind::FolderAddMeeting,
            vec![id(request.folder_id), id(request.session_id)],
            |connection, now| {
                ensure_folder_in(connection, request.folder_id)?;
                ensure_listed_meeting_in(connection, request.session_id)?;
                connection.execute(
                    "INSERT OR IGNORE INTO meeting_folder_members (
                        folder_id, session_id, added_at_utc_ms
                     ) VALUES (?1, ?2, ?3)",
                    params![id(request.folder_id), id(request.session_id), now],
                )?;
                Ok(())
            },
        )
    }

    /// Take one meeting out of one folder. The meeting itself is untouched.
    pub(crate) fn remove_meeting_from_folder(
        &self,
        request: &MeetingFolderMembershipRequest,
        requested_at_utc_ms: i64,
    ) -> Result<MeetingFolderMutationResult, StoreError> {
        self.write_folders(
            request.operation_id,
            request.expected_revision,
            requested_at_utc_ms,
            MeetingCommandKind::FolderRemoveMeeting,
            vec![id(request.folder_id), id(request.session_id)],
            |connection, _| {
                ensure_folder_in(connection, request.folder_id)?;
                connection.execute(
                    "DELETE FROM meeting_folder_members WHERE folder_id = ?1 AND session_id = ?2",
                    params![id(request.folder_id), id(request.session_id)],
                )?;
                Ok(())
            },
        )
    }

    /// Replace what a folder does to its meetings: the template and the
    /// prompt list, together. Every prompt must be a saved prompt about a
    /// meeting; a repeated id is kept once, where it first appears.
    pub(crate) fn set_meeting_folder_defaults(
        &self,
        request: &MeetingFolderDefaultsSetRequest,
        requested_at_utc_ms: i64,
    ) -> Result<MeetingFolderMutationResult, StoreError> {
        let mut seen = HashSet::new();
        let prompt_ids = request
            .prompt_ids
            .iter()
            .copied()
            .filter(|prompt_id| seen.insert(*prompt_id))
            .collect::<Vec<_>>();
        if prompt_ids.len() > MAX_FOLDER_PROMPTS {
            return Err(StoreError::Invalid);
        }
        self.write_folders(
            request.operation_id,
            request.expected_revision,
            requested_at_utc_ms,
            MeetingCommandKind::FolderDefaultsSet,
            vec![id(request.folder_id)],
            |connection, now| {
                let changed = connection.execute(
                    "UPDATE meeting_folders SET template_id = ?2, updated_at_utc_ms = ?3
                      WHERE folder_id = ?1",
                    params![
                        id(request.folder_id),
                        request.template.map(MeetingNotesTemplate::artifact_template_id),
                        now
                    ],
                )?;
                if changed == 0 {
                    return Err(StoreError::NotFound);
                }
                for prompt_id in &prompt_ids {
                    let target: Option<String> = connection
                        .query_row(
                            "SELECT target FROM saved_prompts WHERE prompt_id = ?1",
                            params![id(*prompt_id)],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if target.as_deref() != Some("meeting") {
                        return Err(StoreError::Invalid);
                    }
                }
                connection.execute(
                    "DELETE FROM meeting_folder_prompts WHERE folder_id = ?1",
                    params![id(request.folder_id)],
                )?;
                for (position, prompt_id) in prompt_ids.iter().enumerate() {
                    connection.execute(
                        "INSERT INTO meeting_folder_prompts (folder_id, prompt_id, position)
                         VALUES (?1, ?2, ?3)",
                        params![
                            id(request.folder_id),
                            id(*prompt_id),
                            i64::try_from(position).map_err(|_| StoreError::Invalid)?
                        ],
                    )?;
                }
                Ok(())
            },
        )
    }

    fn write_folders(
        &self,
        operation_id: MeetingOperationId,
        expected_revision: u64,
        requested_at_utc_ms: i64,
        command: MeetingCommandKind,
        effect_ids: Vec<String>,
        write: impl FnOnce(&Connection, i64) -> Result<(), StoreError>,
    ) -> Result<MeetingFolderMutationResult, StoreError> {
        let (receipt, folders) = write_fenced(
            self,
            FencedWrite {
                fence: FOLDER_FENCE,
                command,
                effect_ids,
                operation_id,
                expected_revision,
                requested_at_utc_ms,
            },
            folder_list_in,
            write,
        )?;
        Ok(MeetingFolderMutationResult { receipt, folders })
    }
}

/// Remember where a meeting about to go to the trash is filed, under its
/// deletion job. Called in the transaction that reserves the deletion, while
/// the memberships still exist: deleting the meeting's row takes them.
pub(super) fn remember_trashed_memberships_in(
    connection: &Connection,
    job_id: &str,
    session_id: &str,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT OR REPLACE INTO meeting_folder_trash (job_id, folder_id, added_at_utc_ms)
         SELECT ?1, folder_id, added_at_utc_ms FROM meeting_folder_members WHERE session_id = ?2",
        params![job_id, session_id],
    )?;
    Ok(())
}

/// File a restored meeting back in the folders it was in, the ones that still
/// exist, with the time it was first filed in each.
pub(super) fn restore_trashed_memberships_in(
    connection: &Connection,
    job_id: &str,
    session_id: &str,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT OR IGNORE INTO meeting_folder_members (folder_id, session_id, added_at_utc_ms)
         SELECT folder_id, ?2, added_at_utc_ms FROM meeting_folder_trash WHERE job_id = ?1",
        params![job_id, session_id],
    )?;
    forget_trashed_memberships_in(connection, job_id)
}

/// Forget where a trashed meeting was filed, once it can no longer come back.
pub(super) fn forget_trashed_memberships_in(
    connection: &Connection,
    job_id: &str,
) -> Result<(), StoreError> {
    connection.execute(
        "DELETE FROM meeting_folder_trash WHERE job_id = ?1",
        params![job_id],
    )?;
    Ok(())
}

fn folder_list_in(connection: &Connection) -> Result<MeetingFolderList, StoreError> {
    Ok(MeetingFolderList {
        folders: folders_in(connection)?,
        revision: folder_revision_in(connection)?,
    })
}

fn folders_in(connection: &Connection) -> Result<Vec<MeetingFolder>, StoreError> {
    let mut prompts: HashMap<String, Vec<SavedPromptId>> = HashMap::new();
    let mut statement = connection.prepare(
        "SELECT folder_id, prompt_id FROM meeting_folder_prompts ORDER BY folder_id, position",
    )?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (folder_id, prompt_id) in rows {
        prompts
            .entry(folder_id)
            .or_default()
            .push(SavedPromptId::from_uuid(parse_uuid(&prompt_id)?));
    }
    // A meeting on its way to the trash is not counted: the list stops
    // showing it the moment its deletion is reserved.
    let mut statement = connection.prepare(
        "SELECT f.folder_id, f.name, f.template_id, f.created_at_utc_ms, f.updated_at_utc_ms,
                (SELECT COUNT(*)
                   FROM meeting_folder_members m
                   JOIN meeting_sessions s ON s.id = m.session_id
                  WHERE m.folder_id = f.folder_id AND s.phase != 'deleting')
           FROM meeting_folders f
          ORDER BY f.name COLLATE NOCASE, f.folder_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |(folder_id, name, template_id, created_at_utc_ms, updated_at_utc_ms, count)| {
                Ok(MeetingFolder {
                    folder_id: MeetingFolderId::from_uuid(parse_uuid(&folder_id)?),
                    name,
                    template: template_id.as_deref().map(decode_template).transpose()?,
                    prompt_ids: prompts.remove(&folder_id).unwrap_or_default(),
                    meeting_count: u32::try_from(count).map_err(|_| StoreError::Corrupt)?,
                    created_at_utc_ms,
                    updated_at_utc_ms,
                })
            },
        )
        .collect()
}

/// A name as it is stored: trimmed, not empty, no longer than the ceiling,
/// and free of control characters.
fn folder_name(raw: &str) -> Result<String, StoreError> {
    let name = raw.trim();
    if name.is_empty()
        || name.chars().count() > MAX_FOLDER_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(StoreError::Invalid);
    }
    Ok(name.to_string())
}

/// Two folders may not share a name, ignoring case: the filter and the
/// "Add to folder" menu name folders and nothing else.
fn ensure_name_free_in(
    connection: &Connection,
    name: &str,
    except: Option<MeetingFolderId>,
) -> Result<(), StoreError> {
    let taken: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM meeting_folders
             WHERE name = ?1 COLLATE NOCASE AND (?2 IS NULL OR folder_id != ?2)
         )",
        params![name, except.map(id)],
        |row| row.get(0),
    )?;
    if taken {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

fn ensure_folder_in(connection: &Connection, folder_id: MeetingFolderId) -> Result<(), StoreError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM meeting_folders WHERE folder_id = ?1)",
        params![id(folder_id)],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(StoreError::NotFound);
    }
    Ok(())
}

/// A meeting the list shows: present, and not on its way to the trash.
fn ensure_listed_meeting_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<(), StoreError> {
    let listed: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM meeting_sessions WHERE id = ?1 AND phase != 'deleting')",
        params![id(session_id)],
        |row| row.get(0),
    )?;
    if !listed {
        return Err(StoreError::NotFound);
    }
    Ok(())
}

/// The stored form is the artifact template id, the same string the series
/// preference stores, so a folder and the notes it produced name a template
/// the same way.
fn decode_template(value: &str) -> Result<MeetingNotesTemplate, StoreError> {
    MeetingNotesTemplate::from_artifact_template_id(value).ok_or(StoreError::Corrupt)
}

fn folder_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    let revision: i64 = connection.query_row(
        "SELECT revision FROM meeting_folder_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(revision).map_err(|_| StoreError::Corrupt)
}

fn bump_folder_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    connection.execute(
        "UPDATE meeting_folder_state SET revision = revision + 1 WHERE singleton = 1",
        [],
    )?;
    folder_revision_in(connection)
}

/// The counter every folder write is fenced on: one for the whole list, since
/// one surface shows every folder at once.
const FOLDER_FENCE: Fence = Fence {
    read: folder_revision_in,
    bump: bump_folder_revision_in,
};
