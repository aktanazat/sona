//! What Siri, Spotlight and Shortcuts read: the people, meetings, tasks,
//! projects and notes Sona lists to the system, and what one person promised.
//! They read outside this app's own screens, so D14 applies: a meeting whose
//! series is kept on this Mac is never listed, and nothing said in one
//! answers.

use super::{KeptLocal, QueryError};
use crate::meeting::loop_types::MeetingLoopDirection;
use crate::meeting::people_types::{PersonCommitment, PersonId};
use crate::meeting::store::query_plane::SystemMeeting;
use crate::meeting::store::{MeetingStore, StoreError};
use crate::planning::work::{items, types::WorkStatus};
use crate::planning::EvidenceSourceKind;
use rusqlite::Connection;
use serde::Serialize;
use specta::Type;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Serialize, Type)]
pub struct SystemPerson {
    pub id: PersonId,
    pub display_name: String,
    pub aliases: Vec<String>,
    pub organization: Option<String>,
}

/// Every person, and every retained meeting newest first.
#[derive(Debug, Serialize, Type)]
pub struct SystemIndex {
    pub people: Vec<SystemPerson>,
    pub meetings: Vec<SystemMeeting>,
}

/// What one person promised the user, from their newest meeting back. These
/// are the person page's own commitments, on the side waiting on them.
#[derive(Debug, Serialize, Type)]
pub struct PersonPromises {
    pub person_id: PersonId,
    pub display_name: String,
    pub promises: Vec<PersonCommitment>,
}

/// The tasks, projects and notes Spotlight may list. A task drawn from a
/// meeting kept on this Mac is left out, and so is a personal document.
#[derive(Debug, Serialize, Type)]
pub struct SystemWork {
    pub tasks: Vec<SystemTask>,
    pub projects: Vec<SystemProject>,
    pub notes: Vec<SystemNote>,
}

#[derive(Debug, Serialize, Type)]
pub struct SystemTask {
    pub id: String,
    pub number: i64,
    pub title: String,
    pub details: Option<String>,
    pub status: WorkStatus,
    pub project: Option<String>,
}

#[derive(Debug, Serialize, Type)]
pub struct SystemProject {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Type)]
pub struct SystemNote {
    pub id: String,
    pub title: String,
    pub body: String,
}

pub(crate) fn system_index(store: &MeetingStore) -> Result<SystemIndex, QueryError> {
    let kept_local = KeptLocal::read(store)?;
    let people = store
        .people_list_without(kept_local.sessions())?
        .entries
        .into_iter()
        .map(|entry| SystemPerson {
            id: entry.person.id,
            display_name: entry.person.display_name,
            aliases: entry.person.aliases,
            organization: entry.person.organization,
        })
        .collect();
    let mut meetings = store.system_meetings()?;
    meetings.retain(|meeting| !kept_local.hides(meeting.id));
    Ok(SystemIndex { people, meetings })
}

/// `None` when no person has this id.
pub(crate) fn person_promises(
    store: &MeetingStore,
    person_id: PersonId,
) -> Result<Option<PersonPromises>, QueryError> {
    let kept_local = KeptLocal::read(store)?;
    let detail = match store.person_detail_without(person_id, kept_local.sessions()) {
        Ok(result) => result.detail,
        Err(StoreError::NotFound) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(PersonPromises {
        person_id,
        display_name: detail.person.display_name,
        promises: detail
            .commitments
            .into_iter()
            .filter(|promise| promise.direction == MeetingLoopDirection::WaitingOn)
            .collect(),
    }))
}

/// Every task that is not archived, binned or a duplicate, every project out
/// of the bin, and every note but the personal ones. `hides` names the
/// meeting evidence a task or note may not draw on: a note that quotes or
/// cites a hidden meeting stays out like a task drawn from one.
pub(crate) fn system_work(
    conn: &Connection,
    hides: impl Fn(EvidenceSourceKind, &str) -> bool,
) -> anyhow::Result<SystemWork> {
    let hidden = crate::planning::work::drawing_on(conn, &hides)?;
    let hidden_notes = notes_drawing_on(conn, &hides)?;
    let projects: Vec<SystemProject> = crate::planning::store::projects(conn, false)?
        .into_iter()
        .map(|summary| SystemProject {
            id: summary.project.id,
            title: summary.project.title,
            description: summary.project.description,
        })
        .collect();
    let titles: HashMap<&str, &str> = projects
        .iter()
        .map(|project| (project.id.as_str(), project.title.as_str()))
        .collect();
    let tasks = items::items(conn, false)?
        .into_iter()
        .filter(|item| item.status != WorkStatus::Duplicate && !hidden.contains(&item.id))
        .map(|item| SystemTask {
            project: item
                .project_id
                .as_deref()
                .and_then(|id| titles.get(id))
                .map(|title| (*title).to_owned()),
            id: item.id,
            number: item.number,
            title: item.title,
            details: item.details,
            status: item.status,
        })
        .collect();
    let notes = crate::scratchpad::list_notes_with_connection(conn, "")?
        .into_iter()
        .filter(|note| !note.personal && !hidden_notes.contains(&note.id))
        .map(|note| SystemNote {
            id: note.id,
            title: note.title,
            body: note.body,
        })
        .collect();
    Ok(SystemWork {
        tasks,
        projects,
        notes,
    })
}

/// Notes that carry a meeting's words or cite it: a saved passage, or a
/// meeting among the note's sources.
fn notes_drawing_on(
    conn: &Connection,
    hides: impl Fn(EvidenceSourceKind, &str) -> bool,
) -> anyhow::Result<HashSet<String>> {
    let mut statement = conn.prepare(
        "SELECT note_id, meeting_id FROM scratch_note_passages
         UNION ALL
         SELECT note_id, target_id FROM scratch_note_sources WHERE kind = 'meeting'",
    )?;
    let mut rows = statement.query([])?;
    let mut hidden = HashSet::new();
    while let Some(row) = rows.next()? {
        let meeting: String = row.get(1)?;
        if hides(EvidenceSourceKind::Meeting, &meeting) {
            hidden.insert(row.get(0)?);
        }
    }
    Ok(hidden)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::work::items::NewItem;
    use crate::planning::work::types::{WorkActor, WorkOwner};
    use crate::scratchpad::Author;
    use rusqlite_migration::Migrations;

    fn task(conn: &Connection, title: &str, source_loop_id: Option<&str>) -> String {
        items::insert(
            conn,
            &NewItem {
                title,
                details: None,
                status: WorkStatus::NotStarted,
                priority: 0,
                project_id: None,
                milestone_id: None,
                owner: &WorkOwner::Myself,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id,
                recurrence: None,
            },
            WorkActor::Me,
            1,
        )
        .expect("a task")
        .id
    }

    /// Spotlight is read outside Sona's own screens: work from a meeting
    /// kept on this Mac and personal documents never reach it.
    #[test]
    fn spotlight_leaves_out_kept_local_work_and_personal_documents() {
        let mut conn = Connection::open_in_memory().expect("a history database");
        Migrations::new(crate::managers::history::MIGRATIONS.to_vec())
            .to_latest(&mut conn)
            .expect("the history schema");
        let kept = "4c3ad4c6-58a5-4d8b-9a55-5f3b0d0e1a01";
        let open = task(&conn, "Send the deck", None);
        let kept_loop = format!("{kept}:commitment:abc");
        task(&conn, "Raise the salary question", Some(&kept_loop));
        let note =
            crate::scratchpad::create_note_in(&conn, "Harbor plan", Author::Me, 1).expect("a note");
        let quote = crate::scratchpad::create_note_in(&conn, "What she said about pay", Author::Me, 3)
            .expect("a quote");
        conn.execute(
            "INSERT INTO scratch_note_passages
                (note_id, meeting_id, segment_id, start_ms, end_ms, wording)
             VALUES (?1, ?2, 'segment', 0, 1000, 'raw')",
            rusqlite::params![quote.id, kept],
        )
        .expect("a passage from the kept meeting");
        crate::scratchpad::personal::create_in(&conn, "My salary numbers", Author::Me, 2)
            .expect("a personal document");

        let work = system_work(&conn, |kind, source_id| match kind {
            EvidenceSourceKind::Loop => source_id.starts_with(kept),
            EvidenceSourceKind::Meeting => source_id == kept,
            _ => false,
        })
        .expect("the listing");

        assert_eq!(
            work.tasks.iter().map(|task| &task.id).collect::<Vec<_>>(),
            [&open]
        );
        assert_eq!(
            work.notes.iter().map(|note| &note.id).collect::<Vec<_>>(),
            [&note.id]
        );
    }
}
