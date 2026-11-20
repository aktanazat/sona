//! Edits an automation proposes to notes and projects: text to add to a
//! note, a project's new description, or a drafted project update. Each
//! waits until the user accepts it, when it lands in the automation's name,
//! or declines it.

use super::loops::{message, LoopRefusal};
use super::{now_ms, tell, Effects};
use crate::managers::history::HistoryManager;
use crate::planning::catalog::{self, ProjectEdit};
use crate::planning::store;
use crate::planning::types::ProjectHealth;
use crate::planning::updates;
use crate::planning::work::comments::Author;
use crate::planning::work::types::WorkActor;
use crate::scratchpad::{self, ScratchpadError};
use anyhow::Result;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

pub(crate) const DOC_PROPOSALS_MIGRATION: &str = "CREATE TABLE planning_doc_proposals (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('note', 'project_description', 'project_update')),
    target_id TEXT NOT NULL,
    agent TEXT NOT NULL,
    body TEXT NOT NULL CHECK(length(trim(body)) > 0),
    before TEXT,
    health TEXT CHECK(health IS NULL OR health IN ('onTrack', 'atRisk', 'offTrack')),
    run_id TEXT,
    state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open', 'accepted', 'rejected')),
    created_at_ms INTEGER NOT NULL,
    decided_at_ms INTEGER
);
CREATE INDEX planning_doc_proposals_open ON planning_doc_proposals(state, created_at_ms);
CREATE TRIGGER planning_doc_proposals_project_delete AFTER DELETE ON planning_projects BEGIN
    DELETE FROM planning_doc_proposals WHERE kind != 'note' AND target_id = old.id;
END;";

/// A note's proposals leave with it. Kept apart from
/// [`DOC_PROPOSALS_MIGRATION`] because notes are not in the planner's own
/// schema.
pub(crate) const NOTE_DOC_PROPOSALS_MIGRATION: &str =
    "CREATE TRIGGER planning_doc_proposals_note_delete AFTER DELETE ON scratch_notes BEGIN
    DELETE FROM planning_doc_proposals WHERE kind = 'note' AND target_id = old.id;
END;";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DocProposalKind {
    Note,
    ProjectDescription,
    ProjectUpdate,
}

crate::planning::types::sql_enum!(DocProposalKind {
    Note => "note",
    ProjectDescription => "project_description",
    ProjectUpdate => "project_update",
});

impl DocProposalKind {
    pub(crate) const fn noun(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::ProjectDescription | Self::ProjectUpdate => "project",
        }
    }
}

/// An edit waiting for the user. `before` is the description it replaces;
/// `target_title` is the note's or project's title as it reads now.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DocProposal {
    pub id: String,
    pub kind: DocProposalKind,
    pub target_id: String,
    pub target_title: String,
    pub agent: String,
    pub body: String,
    pub before: Option<String>,
    pub health: Option<ProjectHealth>,
    pub run_id: Option<String>,
    pub created_at_ms: i64,
}

impl DocProposal {
    pub(crate) fn summary(&self) -> String {
        match self.kind {
            DocProposalKind::Note => format!("an addition to the note {}", self.target_title),
            DocProposalKind::ProjectDescription => {
                format!("a new description for {}", self.target_title)
            }
            DocProposalKind::ProjectUpdate => format!("a drafted update on {}", self.target_title),
        }
    }
}

pub(crate) struct NewDocProposal<'a> {
    pub(crate) kind: DocProposalKind,
    pub(crate) target: &'a str,
    pub(crate) agent: &'a str,
    pub(crate) body: &'a str,
    pub(crate) health: Option<ProjectHealth>,
    pub(crate) run_id: Option<&'a str>,
}

fn refuse<T>(text: &str) -> Result<T> {
    Err(LoopRefusal(text.to_owned()).into())
}

const COLUMNS: &str = "id, kind, target_id, agent, body, before, health, run_id, created_at_ms";

fn read(conn: &Connection, row: &Row<'_>) -> rusqlite::Result<DocProposal> {
    let kind: DocProposalKind = row.get(1)?;
    let target_id: String = row.get(2)?;
    let target_title = title(conn, kind, &target_id)
        .ok()
        .flatten()
        .unwrap_or_default();
    Ok(DocProposal {
        id: row.get(0)?,
        kind,
        target_id,
        target_title,
        agent: row.get(3)?,
        body: row.get(4)?,
        before: row.get(5)?,
        health: row.get(6)?,
        run_id: row.get(7)?,
        created_at_ms: row.get(8)?,
    })
}

fn title(conn: &Connection, kind: DocProposalKind, id: &str) -> Result<Option<String>> {
    let sql = match kind {
        DocProposalKind::Note => "SELECT title FROM scratch_notes WHERE id = ?1",
        DocProposalKind::ProjectDescription | DocProposalKind::ProjectUpdate => {
            "SELECT title FROM planning_projects WHERE id = ?1 AND trashed_at_ms IS NULL"
        }
    };
    Ok(conn.query_row(sql, [id], |row| row.get(0)).optional()?)
}

/// The note or project `target` names, by id or by title ignoring case.
fn target_id(conn: &Connection, kind: DocProposalKind, target: &str) -> Result<Option<String>> {
    let target = target.trim();
    let sql = match kind {
        DocProposalKind::Note => {
            "SELECT id FROM scratch_notes WHERE id = ?1 OR lower(title) = lower(?1)
             ORDER BY id = ?1 DESC, updated_at_ms DESC LIMIT 1"
        }
        DocProposalKind::ProjectDescription | DocProposalKind::ProjectUpdate => {
            "SELECT id FROM planning_projects
             WHERE (id = ?1 OR lower(title) = lower(?1)) AND trashed_at_ms IS NULL
             ORDER BY id = ?1 DESC LIMIT 1"
        }
    };
    Ok(conn.query_row(sql, [target], |row| row.get(0)).optional()?)
}

/// Files a proposal; none when `target` names no note or project.
pub(crate) fn propose(
    conn: &Connection,
    new: &NewDocProposal<'_>,
    now: i64,
) -> Result<Option<DocProposal>> {
    let body = new.body.trim();
    if body.is_empty() {
        return refuse("A proposal needs some text.");
    }
    let Some(target_id) = target_id(conn, new.kind, new.target)? else {
        return Ok(None);
    };
    let before = match new.kind {
        DocProposalKind::ProjectDescription => conn.query_row(
            "SELECT description FROM planning_projects WHERE id = ?1",
            [&target_id],
            |row| row.get::<_, Option<String>>(0),
        )?,
        DocProposalKind::Note | DocProposalKind::ProjectUpdate => None,
    };
    let health = match new.kind {
        DocProposalKind::ProjectUpdate => Some(new.health.unwrap_or(ProjectHealth::OnTrack)),
        DocProposalKind::Note | DocProposalKind::ProjectDescription => None,
    };
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO planning_doc_proposals
            (id, kind, target_id, agent, body, before, health, run_id, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            new.kind.as_str(),
            target_id,
            new.agent,
            body,
            before,
            health.map(ProjectHealth::as_str),
            new.run_id,
            now
        ],
    )?;
    proposal(conn, &id).map(Some)
}

fn proposal(conn: &Connection, id: &str) -> Result<DocProposal> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM planning_doc_proposals WHERE id = ?1"),
        [id],
        |row| read(conn, row),
    )
    .optional()?
    .ok_or_else(|| LoopRefusal("This proposal no longer exists.".to_owned()).into())
}

/// Proposals still waiting, oldest first.
pub(crate) fn open(conn: &Connection) -> Result<Vec<DocProposal>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM planning_doc_proposals WHERE state = 'open'
         ORDER BY created_at_ms, rowid"
    ))?;
    let rows = statement
        .query_map([], |row| read(conn, row))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn waiting(conn: &Connection, id: &str) -> Result<DocProposal> {
    let state: Option<String> = conn
        .query_row(
            "SELECT state FROM planning_doc_proposals WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    match state.as_deref() {
        Some("open") => proposal(conn, id),
        Some(_) => refuse("This proposal was already decided."),
        None => refuse("This proposal no longer exists."),
    }
}

fn decide(conn: &Connection, id: &str, state: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE planning_doc_proposals SET state = ?2, decided_at_ms = ?3 WHERE id = ?1",
        params![id, state, now],
    )?;
    Ok(())
}

fn scratch(error: ScratchpadError) -> anyhow::Error {
    match error {
        ScratchpadError::Refused(_) => {
            LoopRefusal("The note can't take this edit.".to_owned()).into()
        }
        ScratchpadError::Storage(error) => error,
    }
}

/// Lands a proposal in its agent's name: the text added to the end of the
/// note, the description set when the project's still reads as it did, or
/// the update posted.
pub(crate) fn accept(conn: &Connection, id: &str, now: i64) -> Result<DocProposal> {
    let proposal = waiting(conn, id)?;
    let agent = Author {
        actor: WorkActor::Agent,
        agent: Some(&proposal.agent),
    };
    match proposal.kind {
        DocProposalKind::Note => {
            let Some(note) = scratchpad::note_with_connection(conn, &proposal.target_id)? else {
                return refuse("The note is gone.");
            };
            let body = format!("{}\n\n{}", note.body.trim_end(), proposal.body);
            scratchpad::update_body_in(
                conn,
                &note.id,
                &body,
                scratchpad::Author::Agent(Some(&proposal.agent)),
                now,
            )
            .map_err(scratch)?;
        }
        DocProposalKind::ProjectDescription => {
            let project = store::project(conn, &proposal.target_id)?;
            if project.description != proposal.before {
                return refuse(
                    "The project's description changed after this was proposed, so it wasn't applied.",
                );
            }
            catalog::save_project(
                conn,
                Some(&project.id),
                &ProjectEdit {
                    description: Some(proposal.body.clone()),
                    ..ProjectEdit::default()
                },
                now,
            )?;
        }
        DocProposalKind::ProjectUpdate => {
            updates::add(
                conn,
                agent,
                &proposal.target_id,
                proposal.health.unwrap_or(ProjectHealth::OnTrack),
                &proposal.body,
                None,
                now,
            )?;
        }
    }
    decide(conn, id, "accepted", now)?;
    Ok(proposal)
}

pub(crate) fn reject(conn: &Connection, id: &str, now: i64) -> Result<DocProposal> {
    let proposal = waiting(conn, id)?;
    decide(conn, id, "rejected", now)?;
    Ok(proposal)
}

pub(crate) fn open_read(history: &HistoryManager) -> Result<Vec<DocProposal>, String> {
    history.with_connection(|conn| open(conn)).map_err(message)
}

fn decided_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
    accepting: bool,
) -> Result<DocProposal, String> {
    let now = now_ms();
    let command = if accepting {
        "work_doc_proposal_accept"
    } else {
        "work_doc_proposal_reject"
    };
    let decided = app
        .state::<Arc<HistoryManager>>()
        .with_connection(|conn| {
            store::mutate(conn, operation_id, command, &id, now, |tx| {
                if accepting {
                    accept(tx, id, now)
                } else {
                    reject(tx, id, now)
                }
            })
        })
        .map_err(message)?;
    let projects = match decided.kind {
        DocProposalKind::Note => {
            if accepting {
                scratchpad::emit_changed(app, &decided.target_id, false);
            }
            Vec::new()
        }
        DocProposalKind::ProjectDescription | DocProposalKind::ProjectUpdate => {
            vec![decided.target_id.clone()]
        }
    };
    tell(
        app,
        &Effects {
            projects,
            ..Effects::default()
        },
    );
    Ok(decided)
}

pub(crate) fn accept_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
) -> Result<DocProposal, String> {
    decided_as(app, operation_id, id, true)
}

pub(crate) fn reject_as(
    app: &AppHandle,
    operation_id: &str,
    id: &str,
) -> Result<DocProposal, String> {
    decided_as(app, operation_id, id, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::schema;

    #[test]
    fn a_proposed_description_is_refused_once_the_project_s_description_changed() {
        let mut conn = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&conn).expect("the planner schema");
        let project = store::create_project(
            &mut conn,
            "Harbor",
            Some("Ship the dock."),
            &Uuid::new_v4().to_string(),
            1,
        )
        .expect("a project");
        let proposed = propose(
            &conn,
            &NewDocProposal {
                kind: DocProposalKind::ProjectDescription,
                target: &project.id,
                agent: "Weekly review",
                body: "Ship the dock and the ferry.",
                health: None,
                run_id: None,
            },
            2,
        )
        .expect("a filed proposal")
        .expect("a proposal for the project");
        catalog::save_project(
            &conn,
            Some(&project.id),
            &ProjectEdit {
                description: Some("Ship the dock by March.".to_owned()),
                ..ProjectEdit::default()
            },
            3,
        )
        .expect("the description edited by hand");

        let accepted = accept(&conn, &proposed.id, 4).map_err(message);

        assert_eq!(
            accepted.map(|proposal| proposal.id),
            Err(
                "The project's description changed after this was proposed, so it wasn't applied."
                    .to_owned()
            )
        );
        assert_eq!(
            store::project(&conn, &project.id)
                .expect("the project")
                .description
                .as_deref(),
            Some("Ship the dock by March.")
        );
    }
}
