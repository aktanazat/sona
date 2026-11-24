//! Personal documents: notes that start private to the person in the app.
//! Agents are the only other readers and writers a note has, so an agent is
//! what a personal document is shared with, at one of three roles. Filing a
//! personal document under a project, milestone, task or cycle moves it out
//! of the personal space: it becomes an ordinary note, words, versions and
//! comments kept, and its invitations go with the privacy they scoped.

use super::{
    create_note_in, emit_changed, failure_sentence, run, Author, ScratchNote, ScratchpadError,
    ScratchpadRefusal,
};
use crate::managers::history::HistoryManager;
use crate::planning::work::agents::agent_name;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashSet;
use std::sync::Arc;
use tauri::{AppHandle, State};

pub const MIGRATION: &str = "ALTER TABLE scratch_notes ADD COLUMN personal INTEGER NOT NULL DEFAULT 0 CHECK(personal IN (0, 1));
CREATE TABLE scratch_note_collaborators (
    note_id TEXT NOT NULL REFERENCES scratch_notes(id),
    agent TEXT NOT NULL COLLATE NOCASE CHECK(length(trim(agent)) > 0),
    role TEXT NOT NULL CHECK(role IN ('view', 'comment', 'edit')),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(note_id, agent)
);
CREATE TRIGGER scratch_note_collaborators_note_delete AFTER DELETE ON scratch_notes BEGIN
    DELETE FROM scratch_note_collaborators WHERE note_id = old.id;
END;";

/// What an invited agent may do with a personal document; each role holds
/// the ones before it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ScratchNoteRole {
    /// Read it and its comments.
    View,
    /// Also comment on it.
    Comment,
    /// Also change it, share it and move it out of the personal space.
    Edit,
}

impl ScratchNoteRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Comment => "comment",
            Self::Edit => "edit",
        }
    }

    fn stored(value: &str) -> rusqlite::Result<Self> {
        match value {
            "view" => Ok(Self::View),
            "comment" => Ok(Self::Comment),
            "edit" => Ok(Self::Edit),
            other => Err(rusqlite::Error::InvalidColumnType(
                0,
                other.to_owned(),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

/// One agent a personal document is shared with.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ScratchCollaborator {
    pub agent: String,
    pub role: ScratchNoteRole,
    pub added_at_ms: i64,
}

/// How far a reader reaches into one note.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Reach {
    /// An ordinary note: what External access allows.
    Open,
    /// A personal document, with the role the reader was given, if any.
    Personal(Option<ScratchNoteRole>),
}

fn named_agent(agent: Option<&str>) -> Option<String> {
    agent
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| agent_name(Some(name)))
}

/// The reach of the agent named `agent`, or of a reader who gives no name
/// when `None`, into note `id`; `None` when there is no such note.
pub(crate) fn reach(
    conn: &Connection,
    id: &str,
    agent: Option<&str>,
) -> rusqlite::Result<Option<Reach>> {
    let personal: Option<bool> = conn
        .query_row(
            "SELECT personal FROM scratch_notes WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(personal) = personal else {
        return Ok(None);
    };
    if !personal {
        return Ok(Some(Reach::Open));
    }
    let Some(agent) = named_agent(agent) else {
        return Ok(Some(Reach::Personal(None)));
    };
    let role = conn
        .query_row(
            "SELECT role FROM scratch_note_collaborators WHERE note_id = ?1 AND agent = ?2",
            params![id, agent],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|role| ScratchNoteRole::stored(&role))
        .transpose()?;
    Ok(Some(Reach::Personal(role)))
}

/// Every personal document, by id: what a reader who gives no name never
/// sees.
pub(crate) fn personal_ids(conn: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut statement = conn.prepare_cached("SELECT id FROM scratch_notes WHERE personal = 1")?;
    let ids = statement.query_map([], |row| row.get(0))?.collect();
    ids
}

/// The agents note `id` is shared with, first invited first.
pub(crate) fn collaborators(
    conn: &Connection,
    id: &str,
) -> rusqlite::Result<Vec<ScratchCollaborator>> {
    let mut statement = conn.prepare_cached(
        "SELECT agent, role, created_at_ms FROM scratch_note_collaborators
         WHERE note_id = ?1 ORDER BY created_at_ms, agent",
    )?;
    let found = statement
        .query_map(params![id], |row| {
            Ok(ScratchCollaborator {
                agent: row.get(0)?,
                role: ScratchNoteRole::stored(&row.get::<_, String>(1)?)?,
                added_at_ms: row.get(2)?,
            })
        })?
        .collect();
    found
}

/// The personal documents `agent` was given a role on, newest modified
/// first, after the one `after` names when it is given.
pub(crate) fn shared_with(
    conn: &Connection,
    agent: Option<&str>,
    after: Option<&str>,
    limit: usize,
) -> rusqlite::Result<Vec<(ScratchNote, ScratchNoteRole)>> {
    let Some(agent) = named_agent(agent) else {
        return Ok(Vec::new());
    };
    let mut statement = conn.prepare_cached(
        "SELECT n.id, n.title, n.body, n.pinned, n.created_at_ms, n.updated_at_ms, n.personal, c.role
         FROM scratch_notes n JOIN scratch_note_collaborators c ON c.note_id = n.id
         WHERE n.personal = 1 AND c.agent = ?1
           AND (?2 IS NULL OR (n.updated_at_ms, n.id) < (
                SELECT updated_at_ms, id FROM scratch_notes WHERE id = ?2))
         ORDER BY n.updated_at_ms DESC, n.id DESC LIMIT ?3",
    )?;
    let found = statement
        .query_map(
            params![agent, after, i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| {
                Ok((
                    super::read_note_row(row)?,
                    ScratchNoteRole::stored(&row.get::<_, String>(7)?)?,
                ))
            },
        )?
        .collect();
    found
}

/// A new personal document. One an agent makes is shared with that agent
/// at [`ScratchNoteRole::Edit`], as its maker; one made in the app is
/// shared with no one.
pub(crate) fn create_in(
    transaction: &Connection,
    body: &str,
    author: Author<'_>,
    now_ms: i64,
) -> Result<ScratchNote, ScratchpadError> {
    let mut note = create_note_in(transaction, body, author, now_ms)?;
    transaction.execute(
        "UPDATE scratch_notes SET personal = 1 WHERE id = ?1",
        params![note.id],
    )?;
    note.personal = true;
    if let Author::Agent(agent) = author {
        if let Some(agent) = named_agent(agent) {
            transaction.execute(
                "INSERT INTO scratch_note_collaborators (note_id, agent, role, created_at_ms)
                 VALUES (?1, ?2, 'edit', ?3)",
                params![note.id, agent, now_ms],
            )?;
        }
    }
    Ok(note)
}

/// Shares personal document `id` with `agent` at `role`, changes the role
/// it has, or with `None` takes the agent off. True when anything changed.
/// A note that is not a personal document is refused as not found: it has
/// no sharing of its own to change.
pub(crate) fn share_in(
    conn: &Connection,
    id: &str,
    agent: &str,
    role: Option<ScratchNoteRole>,
    now_ms: i64,
) -> Result<bool, ScratchpadError> {
    if !matches!(reach(conn, id, None)?, Some(Reach::Personal(_))) {
        return Err(ScratchpadRefusal::NotFound.into());
    }
    let agent = agent_name(Some(agent));
    let changed = match role {
        Some(role) => conn.execute(
            "INSERT INTO scratch_note_collaborators (note_id, agent, role, created_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(note_id, agent) DO UPDATE SET role = excluded.role
             WHERE role != excluded.role",
            params![id, agent, role.as_str(), now_ms],
        )?,
        None => conn.execute(
            "DELETE FROM scratch_note_collaborators WHERE note_id = ?1 AND agent = ?2",
            params![id, agent],
        )?,
    };
    Ok(changed > 0)
}

/// Note `id` was filed under a project, milestone, task or cycle: it is no
/// longer a personal document, and no one holds a role on it.
pub(crate) fn filed_in(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    let has_notes: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'scratch_notes')",
        [],
        |row| row.get(0),
    )?;
    if !has_notes {
        return Ok(());
    }
    conn.execute(
        "UPDATE scratch_notes SET personal = 0 WHERE id = ?1 AND personal = 1",
        params![id],
    )?;
    conn.execute(
        "DELETE FROM scratch_note_collaborators WHERE note_id = ?1",
        params![id],
    )?;
    Ok(())
}

/// Takes note `id` back into the personal space: off every project,
/// milestone, task and cycle it was filed under, private again. An agent
/// doing it keeps the edit role, as it would on a document it made. True
/// when the note was not personal before.
pub(crate) fn make_personal_in(
    conn: &Connection,
    id: &str,
    author: Author<'_>,
    now_ms: i64,
) -> Result<bool, ScratchpadError> {
    let Some(reach) = reach(conn, id, None)? else {
        return Err(ScratchpadRefusal::NotFound.into());
    };
    conn.execute(
        "DELETE FROM planning_evidence_links WHERE source_kind = 'note' AND source_id = ?1",
        params![id],
    )?;
    conn.execute(
        "DELETE FROM scratch_note_cycles WHERE note_id = ?1",
        params![id],
    )?;
    conn.execute(
        "UPDATE scratch_notes SET personal = 1 WHERE id = ?1",
        params![id],
    )?;
    if let Author::Agent(agent) = author {
        if let Some(agent) = named_agent(agent) {
            conn.execute(
                "INSERT INTO scratch_note_collaborators (note_id, agent, role, created_at_ms)
                 VALUES (?1, ?2, 'edit', ?3) ON CONFLICT(note_id, agent) DO UPDATE SET role = 'edit'",
                params![id, agent, now_ms],
            )?;
        }
    }
    Ok(reach == Reach::Open)
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_create_personal(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    body: String,
) -> Result<ScratchNote, String> {
    let note = super::immediate(&history, "The document could not be saved.", |tx| {
        create_in(tx, &body, Author::Me, chrono::Utc::now().timestamp_millis())
    })?;
    emit_changed(&app, &note.id, false);
    Ok(note)
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_make_personal(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    id: String,
) -> Result<ScratchNote, String> {
    let note = super::immediate(&history, "The note could not be made private.", |tx| {
        make_personal_in(tx, &id, Author::Me, chrono::Utc::now().timestamp_millis())?;
        super::note_with_connection(tx, &id)?.ok_or_else(|| ScratchpadRefusal::NotFound.into())
    })?;
    emit_changed(&app, &note.id, false);
    Ok(note)
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_collaborators(
    history: State<'_, Arc<HistoryManager>>,
    id: String,
) -> Result<Vec<ScratchCollaborator>, String> {
    run(&history, |conn| Ok(collaborators(conn, &id)?)).map_err(|error| {
        failure_sentence(&history, error, "The document's sharing could not be read.")
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn scratchpad_share(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    id: String,
    agent: String,
    role: Option<ScratchNoteRole>,
) -> Result<Vec<ScratchCollaborator>, String> {
    if agent.trim().is_empty() {
        return Err("Name the agent to share with.".to_string());
    }
    let shared = super::immediate(
        &history,
        "The document's sharing could not be changed.",
        |tx| {
            share_in(tx, &id, &agent, role, chrono::Utc::now().timestamp_millis())?;
            Ok(collaborators(tx, &id)?)
        },
    )?;
    emit_changed(&app, &id, false);
    Ok(shared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::work::comments;
    use crate::planning::work::types::{WorkActor, WorkCommentSubject};
    use rusqlite_migration::Migrations;

    fn connection() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        Migrations::new(crate::managers::history::MIGRATIONS.to_vec())
            .to_latest(&mut conn)
            .expect("apply history migrations");
        conn
    }

    fn change_rows(conn: &Connection, id: &str) -> i64 {
        conn.query_row(
            "SELECT count(*) FROM planning_changes WHERE (entity = 'note' AND entity_id = ?1)
                OR json_extract(data, '$.subject_id') = ?1",
            params![id],
            |row| row.get(0),
        )
        .expect("count changes")
    }

    /// A document made in the app reaches no agent, whatever name it gives,
    /// until the owner invites one; then only that agent, at its role.
    #[test]
    fn a_personal_document_reaches_no_agent_until_one_is_invited() {
        let conn = connection();
        let ordinary =
            super::super::create_note_in(&conn, "An ordinary note", Author::Me, 1).expect("note");
        let private =
            create_in(&conn, "Salary numbers for the board", Author::Me, 2).expect("personal");

        assert_eq!(reach(&conn, &ordinary.id, None).unwrap(), Some(Reach::Open));
        assert_eq!(
            personal_ids(&conn).unwrap(),
            HashSet::from([private.id.clone()])
        );
        for agent in [None, Some("Codex"), Some("Claude Code")] {
            assert_eq!(
                reach(&conn, &private.id, agent).unwrap(),
                Some(Reach::Personal(None)),
                "{agent:?} reached a document no one was invited to"
            );
        }
        assert!(shared_with(&conn, Some("Codex"), None, 10)
            .unwrap()
            .is_empty());

        share_in(&conn, &private.id, "codex", Some(ScratchNoteRole::View), 3).unwrap();
        assert_eq!(
            reach(&conn, &private.id, Some("Codex")).unwrap(),
            Some(Reach::Personal(Some(ScratchNoteRole::View)))
        );
        assert_eq!(
            reach(&conn, &private.id, Some("Claude Code")).unwrap(),
            Some(Reach::Personal(None))
        );
        assert_eq!(
            reach(&conn, &private.id, None).unwrap(),
            Some(Reach::Personal(None))
        );
        assert_eq!(
            shared_with(&conn, Some("Codex"), None, 10).unwrap().len(),
            1
        );
    }

    /// Each role holds the ones below it and none above; changing a role
    /// replaces it and taking the agent off leaves it no reach.
    #[test]
    fn a_role_is_changed_in_place_and_taken_off_whole() {
        let conn = connection();
        let note = create_in(
            &conn,
            "A draft the agent made",
            Author::Agent(Some("codex")),
            1,
        )
        .expect("personal");
        assert_eq!(
            collaborators(&conn, &note.id).unwrap(),
            vec![ScratchCollaborator {
                agent: "Codex".to_string(),
                role: ScratchNoteRole::Edit,
                added_at_ms: 1,
            }]
        );
        assert!(ScratchNoteRole::View < ScratchNoteRole::Comment);
        assert!(ScratchNoteRole::Comment < ScratchNoteRole::Edit);

        assert!(share_in(&conn, &note.id, "Grok", Some(ScratchNoteRole::Comment), 2).unwrap());
        assert!(!share_in(&conn, &note.id, "grok", Some(ScratchNoteRole::Comment), 3).unwrap());
        assert!(share_in(&conn, &note.id, "Grok", Some(ScratchNoteRole::View), 4).unwrap());
        assert_eq!(
            reach(&conn, &note.id, Some("Grok")).unwrap(),
            Some(Reach::Personal(Some(ScratchNoteRole::View)))
        );
        assert!(share_in(&conn, &note.id, "Grok", None, 5).unwrap());
        assert_eq!(
            reach(&conn, &note.id, Some("Grok")).unwrap(),
            Some(Reach::Personal(None))
        );

        let ordinary = super::super::create_note_in(&conn, "Filed already", Author::Me, 6).unwrap();
        assert!(matches!(
            share_in(&conn, &ordinary.id, "Grok", Some(ScratchNoteRole::Edit), 7),
            Err(ScratchpadError::Refused(ScratchpadRefusal::NotFound))
        ));
    }

    /// Filing a personal document under a cycle moves it out of the
    /// personal space with its words, versions and comments, and ends its
    /// sharing; making it private again takes it off the cycle.
    #[test]
    fn moving_keeps_words_versions_and_comments_and_ends_sharing() {
        let conn = connection();
        conn.execute(
            "INSERT INTO planning_cycles (number, start_day, end_day, created_at_ms)
             VALUES (1, '2026-11-16', '2026-11-30', 0)",
            [],
        )
        .unwrap();
        let note = create_in(&conn, "First words of the plan", Author::Me, 1).unwrap();
        super::super::update_body_in(
            &conn,
            &note.id,
            "Second words of the plan",
            Author::Me,
            20 * 60_000,
        )
        .unwrap();
        share_in(&conn, &note.id, "Codex", Some(ScratchNoteRole::Comment), 2).unwrap();
        comments::add(
            &conn,
            WorkCommentSubject::Note,
            &note.id,
            None,
            comments::Author {
                actor: WorkActor::Agent,
                agent: Some("Codex"),
            },
            "Looks right",
            3,
        )
        .unwrap();

        super::super::documents::link_cycle_in(&conn, &note.id, 1, 4).unwrap();

        let moved = super::super::note_with_connection(&conn, &note.id)
            .unwrap()
            .unwrap();
        assert!(!moved.personal);
        assert_eq!(moved.body, "Second words of the plan");
        assert_eq!(
            super::super::versions_with_connection(&conn, &note.id)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            comments::page(&conn, WorkCommentSubject::Note, &note.id, 10, None)
                .unwrap()
                .threads
                .len(),
            1
        );
        assert!(collaborators(&conn, &note.id).unwrap().is_empty());
        assert_eq!(reach(&conn, &note.id, None).unwrap(), Some(Reach::Open));

        assert!(make_personal_in(&conn, &note.id, Author::Me, 5).unwrap());
        assert!(personal_ids(&conn).unwrap().contains(&note.id));
        let filed: i64 = conn
            .query_row(
                "SELECT count(*) FROM scratch_note_cycles WHERE note_id = ?1",
                params![note.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(filed, 0);
    }

    /// A deleted personal document can no longer say it was personal, so
    /// nothing it or its comments wrote stays in the change history.
    #[test]
    fn a_deleted_personal_document_leaves_no_change_history() {
        let conn = connection();
        let private = create_in(&conn, "Offer letter draft for Dana", Author::Me, 1).unwrap();
        let ordinary =
            super::super::create_note_in(&conn, "Team lunch list", Author::Me, 1).unwrap();
        assert!(change_rows(&conn, &private.id) > 0);

        super::super::delete_note_with_connection(&conn, &private.id).unwrap();
        super::super::delete_note_with_connection(&conn, &ordinary.id).unwrap();

        assert_eq!(change_rows(&conn, &private.id), 0);
        assert!(change_rows(&conn, &ordinary.id) > 0);
    }

    /// Blank or missing agent names never match a collaborator and never
    /// insert the fallback "Agent" name as a collaborator.
    #[test]
    fn blank_or_missing_agent_never_gains_access_or_collaborator_row() {
        let conn = connection();
        let unnamed = create_in(&conn, "Unnamed agent draft", Author::Agent(None), 1).unwrap();
        let blank = create_in(&conn, "Blank agent draft", Author::Agent(Some("   ")), 2).unwrap();
        assert!(collaborators(&conn, &unnamed.id).unwrap().is_empty());
        assert!(collaborators(&conn, &blank.id).unwrap().is_empty());

        let ordinary = super::super::create_note_in(&conn, "Ordinary note", Author::Me, 3).unwrap();
        assert!(make_personal_in(&conn, &ordinary.id, Author::Agent(Some("  ")), 4).unwrap());
        assert!(collaborators(&conn, &ordinary.id).unwrap().is_empty());

        // Even if a document were explicitly shared with "Agent", blank/empty
        // agent names must be treated as anonymous and never match it.
        share_in(&conn, &unnamed.id, "Agent", Some(ScratchNoteRole::Edit), 5).unwrap();
        for missing in [None, Some(""), Some("   ")] {
            assert_eq!(
                reach(&conn, &unnamed.id, missing).unwrap(),
                Some(Reach::Personal(None)),
                "{missing:?} must not match collaborator Agent"
            );
            assert!(
                shared_with(&conn, missing, None, 10).unwrap().is_empty(),
                "{missing:?} must not list documents shared with Agent"
            );
        }
    }
}
