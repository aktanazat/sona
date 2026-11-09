//! Where the tracker meets the meetings its work came from. A commitment
//! comes in as a triage item, an item and the commitment it tracks close and
//! reopen together, and what the meeting said about an item becomes a
//! suggestion for it.

use super::super::sources;
use super::super::untracked::{any, lineage, DISMISSED, TRACKED};
use super::items::{self, NewItem};
use super::types::{
    WorkActivityKind, WorkActor, WorkFieldSuggestion, WorkItem, WorkOwner, WorkPatch,
    WorkPersonChoice, WorkStatus, WorkSuggestionField, WorkSuggestionValue,
};
use crate::meeting::automations::{due_day_for, local_day};
use crate::meeting::loop_types::{
    MeetingLoopDirection, MeetingLoopId, MeetingLoopKind, MeetingLoopRow, MeetingLoopStatus,
};
use crate::meeting::store::loops::{self, MeetingLedgerRows};
use crate::meeting::store::StoreError;
use crate::meeting::types::{MeetingSessionId, OperationActor};
use anyhow::Result;
use chrono::{Local, NaiveDate, TimeZone};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn meeting_error(error: StoreError) -> anyhow::Error {
    anyhow::anyhow!("meeting loops could not be read: {error:?}")
}

/// One meeting's rows as its loops page shows them, or none for a meeting
/// gone or never processed.
pub(crate) fn meeting_rows(
    meetings: &Connection,
    session_id: MeetingSessionId,
    user_name: Option<&str>,
) -> Result<Option<MeetingLedgerRows>> {
    loops::meeting_ledger_rows_in(meetings, session_id, user_name).map_err(meeting_error)
}

struct ReadMeeting {
    rows: MeetingLedgerRows,
    retained: bool,
}

/// A commitment as its meeting holds it, and whether its words may still
/// be shown.
pub(crate) struct Source<'r> {
    pub meeting: &'r MeetingLedgerRows,
    pub row: &'r MeetingLoopRow,
    pub retained: bool,
}

/// Each meeting's rows read once, for the length of one request, or none
/// while the conversations are closed.
pub(crate) struct MeetingReader<'a> {
    conn: Option<&'a Connection>,
    user_name: Option<&'a str>,
    meetings: HashMap<MeetingSessionId, Option<ReadMeeting>>,
}

impl<'a> MeetingReader<'a> {
    pub(crate) fn new(conn: Option<&'a Connection>, user_name: Option<&'a str>) -> Self {
        Self {
            conn,
            user_name,
            meetings: HashMap::new(),
        }
    }

    pub(crate) const fn conn(&self) -> Option<&'a Connection> {
        self.conn
    }

    fn meeting(
        &mut self,
        conn: &Connection,
        session_id: MeetingSessionId,
    ) -> Result<Option<&ReadMeeting>> {
        if !self.meetings.contains_key(&session_id) {
            let read = match meeting_rows(conn, session_id, self.user_name)? {
                Some(rows) => Some(ReadMeeting {
                    retained: sources::meeting_retained(conn, &session_id.uuid().to_string())?,
                    rows,
                }),
                None => None,
            };
            self.meetings.insert(session_id, read);
        }
        Ok(self.meetings.get(&session_id).and_then(Option::as_ref))
    }

    /// The meeting a commitment was said in and its row, while the meeting's
    /// ledger still holds it.
    pub(crate) fn source(&mut self, loop_id: &str) -> Result<Option<Source<'_>>> {
        let Some(conn) = self.conn else {
            return Ok(None);
        };
        let Some(session_id) = MeetingLoopId(loop_id.to_owned()).session_id() else {
            return Ok(None);
        };
        let Some(meeting) = self.meeting(conn, session_id)? else {
            return Ok(None);
        };
        let Some(row) = meeting
            .rows
            .rows
            .iter()
            .find(|row| row.loop_id.as_str() == loop_id)
        else {
            return Ok(None);
        };
        let retained = meeting.retained
            && sources::words_retained(
                conn,
                &session_id.uuid().to_string(),
                &row.citations,
                row.at_ms,
            )?;
        Ok(Some(Source {
            meeting: &meeting.rows,
            row,
            retained,
        }))
    }
}

/// Everyone an item can wait on, and the name each id goes by, merged
/// people's old ids included. Nobody while the conversations are closed.
#[derive(Default)]
pub(crate) struct People {
    pub choices: Vec<WorkPersonChoice>,
    names: HashMap<String, String>,
    /// Each merged person's old id and the id it was merged into. A merge
    /// re-points earlier redirects, so none of them chain.
    merged: Vec<(String, String)>,
}

impl People {
    pub(crate) fn read(meetings: &Connection) -> Result<Self> {
        let mut statement = meetings
            .prepare("SELECT id, display_name FROM persons ORDER BY lower(display_name), id")?;
        let choices = statement
            .query_map([], |row| {
                Ok(WorkPersonChoice {
                    id: row.get(0)?,
                    name: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut names = choices
            .iter()
            .map(|person| (person.id.clone(), person.name.clone()))
            .collect::<HashMap<_, _>>();
        let mut statement = meetings.prepare(
            "SELECT r.source_person_id, r.target_person_id, p.display_name
             FROM person_merge_redirects r JOIN persons p ON p.id = r.target_person_id",
        )?;
        let mut merged = Vec::new();
        for redirect in
            statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        {
            let (from, into, name): (String, String, String) = redirect?;
            names.entry(from.clone()).or_insert(name);
            merged.push((from, into));
        }
        Ok(Self {
            choices,
            names,
            merged,
        })
    }

    pub(crate) fn name(&self, id: &str) -> Option<&str> {
        self.names.get(id).map(String::as_str)
    }

    /// Every id the people `ids` name have gone by: each id, the person it
    /// was merged into, and the old ids merged into either.
    pub(crate) fn with_merged<'a>(&'a self, ids: &'a [String]) -> HashSet<&'a str> {
        let named = ids
            .iter()
            .map(String::as_str)
            .chain(
                self.merged
                    .iter()
                    .filter(|(from, _)| ids.contains(from))
                    .map(|(_, into)| into.as_str()),
            )
            .collect::<HashSet<_>>();
        let mut everyone = named.clone();
        everyone.extend(
            self.merged
                .iter()
                .filter(|(_, into)| named.contains(into.as_str()))
                .map(|(from, _)| from.as_str()),
        );
        everyone
    }
}

const fn loop_status_for(status: WorkStatus) -> MeetingLoopStatus {
    match status {
        WorkStatus::Done => MeetingLoopStatus::Done,
        WorkStatus::Canceled | WorkStatus::Duplicate => MeetingLoopStatus::Dropped,
        WorkStatus::Triage
        | WorkStatus::Backlog
        | WorkStatus::NotStarted
        | WorkStatus::InProgress => MeetingLoopStatus::Open,
    }
}

/// How the meeting side files a change this actor made.
const fn operation_actor(actor: WorkActor) -> OperationActor {
    match actor {
        WorkActor::Me => OperationActor::User,
        WorkActor::Sona => OperationActor::System,
        WorkActor::Agent => OperationActor::External,
    }
}

/// The item every duplicate in an item's family follows.
fn root(history: &Connection, item: WorkItem) -> Result<WorkItem> {
    match &item.duplicate_of_id {
        Some(id) => items::item(history, id),
        None => Ok(item),
    }
}

/// Moves the commitments behind an item's family to match the item they
/// all follow, filed under `actor`, and forgets "not work" for any family
/// that is open again. Returns the loops it moved: none while the
/// conversations are closed, when the catch-up moves them as they open.
pub(crate) fn settle(
    history: &Connection,
    meetings: Option<&Connection>,
    item_id: &str,
    actor: WorkActor,
    now_ms: i64,
) -> Result<Vec<MeetingLoopId>> {
    let root = root(history, items::item(history, item_id)?)?;
    let status = loop_status_for(root.status);
    let mut moved = Vec::new();
    for member in items::family(history, &root.id)? {
        let Some(loop_id) = member.source_loop_id else {
            continue;
        };
        if status == MeetingLoopStatus::Open {
            history.execute(
                "DELETE FROM planning_dismissed_loops WHERE loop_id = ?1",
                [&loop_id],
            )?;
        }
        let Some(meetings) = meetings else {
            continue;
        };
        let loop_id = MeetingLoopId(loop_id);
        if loops::follow_tracker_in(meetings, &loop_id, status, operation_actor(actor), now_ms)
            .map_err(meeting_error)?
        {
            moved.push(loop_id);
        }
    }
    Ok(moved)
}

fn followed_status(
    history: &Connection,
    root: &WorkItem,
    loop_status: MeetingLoopStatus,
) -> Result<Option<WorkStatus>> {
    Ok(match loop_status {
        MeetingLoopStatus::Carried => None,
        MeetingLoopStatus::Done => (root.status != WorkStatus::Done).then_some(WorkStatus::Done),
        MeetingLoopStatus::Dropped => {
            (root.status != WorkStatus::Canceled).then_some(WorkStatus::Canceled)
        }
        MeetingLoopStatus::Open if root.status.is_closed() => {
            Some(items::status_before_close(history, &root.id)?)
        }
        MeetingLoopStatus::Open => None,
    })
}

/// What following one loop changed: the items that moved, and the other
/// commitments that moved with them.
#[derive(Default)]
pub(crate) struct Followed {
    pub items: Vec<WorkItem>,
    pub loops: Vec<MeetingLoopId>,
}

/// Moves the item that tracks a loop after the loop moved in its meeting,
/// then the rest of the item's family's loops after it. Nothing tracks a
/// loop carried into a later meeting; its live occurrence does.
pub(crate) fn follow_loop_in(
    history: &Connection,
    meetings: &Connection,
    loop_id: &MeetingLoopId,
    now_ms: i64,
) -> Result<Followed> {
    let Some(item) = items::by_source_loop(history, loop_id.as_str())? else {
        return Ok(Followed::default());
    };
    let (status, _) = loops::status_in(meetings, loop_id).map_err(meeting_error)?;
    let root = root(history, item)?;
    let Some(status) = followed_status(history, &root, status)? else {
        return Ok(Followed::default());
    };
    let changed = items::follow(history, &root.id, status, now_ms)?;
    Ok(Followed {
        loops: settle(history, Some(meetings), &root.id, WorkActor::Sona, now_ms)?,
        items: vec![changed.after],
    })
}

/// Who last moved the status of an item's family: whom a commitment that
/// never followed the move is filed under.
fn last_mover(history: &Connection, root_id: &str) -> Result<WorkActor> {
    Ok(history
        .query_row(
            "SELECT a.actor FROM planning_task_activity a
             JOIN planning_tasks t ON t.id = a.task_id
             WHERE (t.id = ?1 OR t.duplicate_of = ?1)
               AND a.kind IN ('status', 'accepted', 'declined', 'duplicate')
             ORDER BY a.at_ms DESC, a.rowid DESC LIMIT 1",
            [root_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(WorkActor::Sona))
}

/// Puts every tracked item and its commitment back in step after they
/// parted: a crash between the two databases' commits, or a change made
/// while the conversations were closed. Whichever side moved last wins, and
/// a commitment that follows its item is filed under whoever moved it.
pub(crate) fn reconcile(
    history: &Connection,
    meetings: &Connection,
    now_ms: i64,
) -> Result<Followed> {
    let mut followed = Followed::default();
    for item in items::items(history, true)? {
        let Some(loop_id) = item.source_loop_id.clone() else {
            continue;
        };
        let loop_id = MeetingLoopId(loop_id);
        let (status, moved_at) = loops::status_in(meetings, &loop_id).map_err(meeting_error)?;
        let root = root(history, item)?;
        if status == MeetingLoopStatus::Carried || status == loop_status_for(root.status) {
            continue;
        }
        if moved_at.is_some_and(|at| at > root.status_changed_at_ms) {
            let more = follow_loop_in(history, meetings, &loop_id, now_ms)?;
            followed.items.extend(more.items);
            followed.loops.extend(more.loops);
        } else {
            followed.loops.extend(settle(
                history,
                Some(meetings),
                &root.id,
                last_mover(history, &root.id)?,
                now_ms,
            )?);
        }
    }
    Ok(followed)
}

fn link_meeting(
    history: &Connection,
    task_id: &str,
    meeting_id: &str,
    at_ms: Option<i64>,
    now_ms: i64,
) -> Result<()> {
    history.execute(
        "INSERT INTO planning_evidence_links
            (id, target_kind, target_id, source_kind, source_id, at_ms, revision, created_at_ms, meeting_note_id)
         VALUES (?1, 'task', ?2, 'meeting', ?3, ?4, 1, ?5, NULL)
         ON CONFLICT DO NOTHING",
        params![Uuid::new_v4().to_string(), task_id, meeting_id, at_ms, now_ms],
    )?;
    Ok(())
}

/// Brings one open commitment into the tracker, unless it is already
/// there, set aside as not work, or tracked by other planner work. A
/// commitment carried forward from an earlier meeting moves the item that
/// tracked the earlier occurrence instead of making a second one. Returns
/// the item it made or moved.
pub(crate) fn take_in_row(
    history: &Connection,
    meetings: &Connection,
    meeting: &MeetingLedgerRows,
    row: &MeetingLoopRow,
    now_ms: i64,
) -> Result<Option<WorkItem>> {
    if row.kind != MeetingLoopKind::Commitment || row.status != MeetingLoopStatus::Open {
        return Ok(None);
    }
    let loop_id = row.loop_id.as_str();
    if items::by_source_loop(history, loop_id)?.is_some()
        || sources::loop_words(history, meetings, loop_id)?.is_none()
    {
        return Ok(None);
    }
    let meeting_id = meeting.session_id.uuid().to_string();
    let at_ms = i64::try_from(row.at_ms)
        .ok()
        .map(|at_ms| {
            sources::retained_passage(meetings, &meeting_id, at_ms)
                .map(|kept| kept.then_some(at_ms))
        })
        .transpose()?
        .flatten();
    let lineage = lineage(meetings, loop_id)?;
    if let Some(item) = tracker_of(history, &lineage)? {
        history.execute(
            "UPDATE planning_tasks SET source_loop_id = ?2, revision = revision + 1, updated_at_ms = ?3
             WHERE id = ?1",
            params![item.id, loop_id, now_ms],
        )?;
        link_meeting(history, &item.id, &meeting_id, at_ms, now_ms)?;
        settle(history, Some(meetings), &item.id, WorkActor::Sona, now_ms)?;
        return Ok(Some(items::item(history, &item.id)?));
    }
    if any(history, TRACKED, &lineage)? || any(history, DISMISSED, &lineage)? {
        return Ok(None);
    }
    let item = items::insert(
        history,
        &NewItem {
            title: &row.text,
            details: None,
            status: WorkStatus::Triage,
            priority: 0,
            project_id: None,
            milestone_id: None,
            owner: &WorkOwner::Nobody,
            due_at_ms: None,
            label_ids: &[],
            source_loop_id: Some(loop_id),
        },
        WorkActor::Sona,
        now_ms,
    )?;
    link_meeting(history, &item.id, &meeting_id, at_ms, now_ms)?;
    Ok(Some(item))
}

/// Brings every open commitment of one meeting into the tracker; see
/// [`take_in_row`]. Returns the items made or moved; a meeting gone or
/// never processed brings nothing.
pub(crate) fn take_in_meeting(
    history: &Connection,
    meetings: &Connection,
    session_id: MeetingSessionId,
    user_name: Option<&str>,
    now_ms: i64,
) -> Result<Vec<WorkItem>> {
    let Some(meeting) = meeting_rows(meetings, session_id, user_name)? else {
        return Ok(Vec::new());
    };
    let mut taken = Vec::new();
    for row in &meeting.rows {
        taken.extend(take_in_row(history, meetings, &meeting, row, now_ms)?);
    }
    Ok(taken)
}

/// The meetings held since `since_ms`, oldest first.
pub(crate) fn meetings_since(
    meetings: &Connection,
    since_ms: i64,
) -> Result<Vec<MeetingSessionId>> {
    let mut statement = meetings.prepare(
        "SELECT id FROM meeting_sessions
         WHERE phase != 'deleting' AND COALESCE(started_at_utc_ms, created_at_utc_ms) >= ?1
         ORDER BY COALESCE(started_at_utc_ms, created_at_utc_ms), id",
    )?;
    let ids = statement
        .query_map([since_ms], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.iter()
        .map(|id| Ok(MeetingSessionId::from_uuid(Uuid::parse_str(id)?)))
        .collect()
}

pub(crate) const BACKFILL_MS: i64 = 30 * DAY_MS;

fn local_noon_ms(day: NaiveDate) -> Result<i64> {
    let noon = day
        .and_hms_opt(12, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("noon is outside the calendar"))?;
    Local
        .from_local_datetime(&noon)
        .earliest()
        .map(|at| at.timestamp_millis())
        .ok_or_else(|| anyhow::anyhow!("local noon does not exist"))
}

fn person_named(meetings: &Connection, name: &str) -> Result<Option<(String, String)>> {
    let mut statement = meetings.prepare_cached(
        "SELECT id, display_name FROM persons WHERE lower(display_name) = lower(?1) LIMIT 2",
    )?;
    let matches = statement
        .query_map([name.trim()], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(match <[_; 1]>::try_from(matches) {
        Ok([person]) => Some(person),
        Err(_) => None,
    })
}

fn owner_suggestion(
    meetings: &Connection,
    row: &MeetingLoopRow,
) -> Result<Option<(WorkOwner, String)>> {
    match row.direction {
        MeetingLoopDirection::Mine => Ok(Some((WorkOwner::Myself, "You took this on".to_owned()))),
        MeetingLoopDirection::Unattributed => Ok(None),
        MeetingLoopDirection::WaitingOn => {
            let person = match &row.owner_person_id {
                Some(id) => crate::meeting::store::memory_index::person_source_in(
                    meetings,
                    &id.uuid().to_string(),
                )
                .map_err(meeting_error)?
                .map(|person| (person.id, person.title)),
                None => match &row.owner_text {
                    Some(text) => person_named(meetings, text)?,
                    None => None,
                },
            };
            Ok(person.map(|(person_id, name)| {
                (
                    WorkOwner::Person { person_id },
                    format!("{name} took this on"),
                )
            }))
        }
    }
}

/// The project this meeting's work most likely belongs to: one Sona already
/// matched the commitment or the meeting to, else the one the same series'
/// earlier meetings' work went to.
fn project_suggestion(
    history: &Connection,
    meetings: &Connection,
    session_id: MeetingSessionId,
    loop_id: &str,
) -> Result<Option<(String, String)>> {
    let meeting_id = session_id.uuid().to_string();
    let matched: Option<(String, String)> = history
        .query_row(
            "SELECT p.id, p.title FROM planning_suggestions s
             LEFT JOIN planning_milestones m ON s.target_kind = 'milestone' AND m.id = s.target_id
             LEFT JOIN planning_tasks t ON s.target_kind = 'task' AND t.id = s.target_id
             JOIN planning_projects p ON p.id = CASE s.target_kind
                 WHEN 'project' THEN s.target_id WHEN 'milestone' THEN m.project_id ELSE t.project_id END
             WHERE s.kind = 'possible_link' AND s.state != 'declined' AND p.status != 'completed'
               AND ((s.source_kind = 'loop' AND s.source_id = ?1)
                 OR (s.source_kind = 'meeting' AND s.source_id = ?2))
             ORDER BY s.source_kind = 'loop' DESC, s.updated_at_ms DESC, s.id
             LIMIT 1",
            params![loop_id, meeting_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((id, title)) = matched {
        return Ok(Some((id, format!("Sona matched this meeting to {title}"))));
    }
    let mut statement = meetings.prepare_cached(
        "SELECT f.session_id FROM meeting_calendar_facts f
         JOIN meeting_sessions m ON m.id = f.session_id
         WHERE json_extract(f.event_json, '$.seriesKey') = (
                SELECT json_extract(event_json, '$.seriesKey')
                FROM meeting_calendar_facts WHERE session_id = ?1)
           AND f.session_id != ?1 AND m.phase != 'deleting'
         ORDER BY COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) DESC, m.id DESC
         LIMIT 10",
    )?;
    let earlier = statement
        .query_map([&meeting_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut statement = history.prepare_cached(
        "SELECT p.id, p.title FROM planning_projects p JOIN (
             SELECT t.project_id AS project_id, t.updated_at_ms AS at_ms FROM planning_tasks t
             WHERE t.source_loop_id LIKE ?1 || ':%' AND t.status != 'triage'
               AND t.archived_at_ms IS NULL
             UNION ALL
             SELECT CASE e.target_kind WHEN 'project' THEN e.target_id
                    WHEN 'milestone' THEN m.project_id ELSE t.project_id END, e.created_at_ms
             FROM planning_evidence_links e
             LEFT JOIN planning_milestones m ON e.target_kind = 'milestone' AND m.id = e.target_id
             LEFT JOIN planning_tasks t ON e.target_kind = 'task' AND t.id = e.target_id
             WHERE ((e.source_kind = 'meeting' AND e.source_id = ?1)
                 OR (e.source_kind = 'loop' AND e.source_id LIKE ?1 || ':%'))
               AND (t.id IS NULL OR (t.status != 'triage' AND t.archived_at_ms IS NULL))
         ) work ON work.project_id = p.id
         WHERE p.status != 'completed'
         ORDER BY work.at_ms DESC, p.id
         LIMIT 1",
    )?;
    for session in earlier {
        let went: Option<(String, String)> = statement
            .query_row([&session], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        if let Some((id, title)) = went {
            return Ok(Some((
                id,
                format!("Earlier meetings in this series went to {title}"),
            )));
        }
    }
    Ok(None)
}

/// Words compared the way people repeat a task: case, spacing and closing
/// punctuation do not count.
fn same_words(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', '!', '?', ',', ';', ':'])
        .to_lowercase()
}

/// An unfinished item with the same words as `item`, for a triage item
/// that may already be tracked.
fn same_as<'a>(all: &'a [WorkItem], item: &WorkItem) -> Option<&'a WorkItem> {
    let words = same_words(&item.title);
    all.iter().find(|other| {
        other.id != item.id
            && other.archived_at_ms.is_none()
            && !other.status.is_closed()
            && same_words(&other.title) == words
    })
}

/// What Sona suggests for a triage item's empty fields, read off the
/// meeting that raised it, and the item it may duplicate. Items past triage
/// get none.
pub(crate) fn suggestions(
    history: &Connection,
    reader: &mut MeetingReader<'_>,
    all: &[WorkItem],
    item: &WorkItem,
) -> Result<Vec<WorkFieldSuggestion>> {
    if item.status != WorkStatus::Triage {
        return Ok(Vec::new());
    }
    let mut suggestions = fill_suggestions(history, reader, item)?;
    if let Some(original) = same_as(all, item) {
        suggestions.push(WorkFieldSuggestion {
            field: WorkSuggestionField::Duplicate,
            value: WorkSuggestionValue::Id(original.id.clone()),
            reason: format!("Same words as #{}", original.number),
            quote: None,
            at_ms: None,
            meeting_id: None,
        });
    }
    Ok(suggestions)
}

/// The owner, due day and project the meeting suggests for fields `item`
/// leaves empty.
pub(crate) fn fill_suggestions(
    history: &Connection,
    reader: &mut MeetingReader<'_>,
    item: &WorkItem,
) -> Result<Vec<WorkFieldSuggestion>> {
    let Some(loop_id) = item.source_loop_id.as_deref() else {
        return Ok(Vec::new());
    };
    let Some(meetings) = reader.conn() else {
        return Ok(Vec::new());
    };
    let Some(Source {
        meeting,
        row,
        retained,
    }) = reader.source(loop_id)?
    else {
        return Ok(Vec::new());
    };
    let meeting_id = meeting.session_id.uuid().to_string();
    let suggestion = |field, value, reason| WorkFieldSuggestion {
        field,
        value,
        reason,
        quote: row.quote.clone().filter(|_| retained),
        at_ms: i64::try_from(row.at_ms).ok().filter(|_| retained),
        meeting_id: Some(meeting_id.clone()),
    };
    let mut suggestions = Vec::new();
    if item.owner() == WorkOwner::Nobody {
        if let Some((owner, reason)) = owner_suggestion(meetings, row)? {
            suggestions.push(suggestion(
                WorkSuggestionField::Owner,
                WorkSuggestionValue::Owner(owner),
                reason,
            ));
        }
    }
    if item.due_at_ms.is_none() {
        if let Some(day) = due_day_for(row, &meeting.action_items, local_day(meeting.at_utc_ms)) {
            suggestions.push(suggestion(
                WorkSuggestionField::Due,
                WorkSuggestionValue::Due(local_noon_ms(day)?),
                "The meeting set this date".to_owned(),
            ));
        }
    }
    if item.project_id.is_none() {
        if let Some((project_id, reason)) =
            project_suggestion(history, meetings, meeting.session_id, loop_id)?
        {
            suggestions.push(suggestion(
                WorkSuggestionField::Project,
                WorkSuggestionValue::Id(project_id),
                reason,
            ));
        }
    }
    Ok(suggestions)
}

/// The change accepting a triage item makes: not started, with each empty
/// field filled from Sona's suggestion when asked, and `project_id` when
/// given.
pub(crate) fn acceptance(
    history: &Connection,
    reader: &mut MeetingReader<'_>,
    item: &WorkItem,
    apply_suggestions: bool,
    project_id: Option<&str>,
) -> Result<WorkPatch> {
    let mut patch = WorkPatch {
        status: Some(WorkStatus::NotStarted),
        ..WorkPatch::default()
    };
    if apply_suggestions {
        for suggestion in fill_suggestions(history, reader, item)? {
            match (suggestion.field, suggestion.value) {
                (WorkSuggestionField::Owner, WorkSuggestionValue::Owner(owner)) => {
                    patch.owner = Some(owner);
                }
                (WorkSuggestionField::Due, WorkSuggestionValue::Due(due_at_ms)) => {
                    patch.due_at_ms = Some(Some(due_at_ms));
                }
                (WorkSuggestionField::Project, WorkSuggestionValue::Id(project_id)) => {
                    patch.project_id = Some(Some(project_id));
                }
                _ => {}
            }
        }
    }
    if let Some(project_id) = project_id {
        patch.project_id = Some(Some(project_id.to_owned()));
    }
    Ok(patch)
}

/// Accepts one triage item, or refuses one that left triage since it was
/// shown.
pub(crate) fn accept(
    history: &Connection,
    meetings: Option<&Connection>,
    item: &WorkItem,
    patch: &WorkPatch,
    actor: WorkActor,
    now_ms: i64,
) -> Result<(WorkItem, Vec<MeetingLoopId>)> {
    if item.status != WorkStatus::Triage {
        return Err(super::super::store::Refusal::StaleRevision.into());
    }
    let changed = items::apply(
        history,
        &item.id,
        patch,
        WorkActivityKind::Accepted,
        actor,
        now_ms,
    )?;
    let moved = settle(history, meetings, &item.id, actor, now_ms)?;
    Ok((changed.after, moved))
}

/// Declines one triage item: canceled, its commitment dropped in the
/// meeting and remembered as not work.
pub(crate) fn decline(
    history: &Connection,
    meetings: Option<&Connection>,
    item: &WorkItem,
    actor: WorkActor,
    now_ms: i64,
) -> Result<(WorkItem, Vec<MeetingLoopId>)> {
    if item.status != WorkStatus::Triage {
        return Err(super::super::store::Refusal::StaleRevision.into());
    }
    let changed = items::apply(
        history,
        &item.id,
        &WorkPatch {
            status: Some(WorkStatus::Canceled),
            ..WorkPatch::default()
        },
        WorkActivityKind::Declined,
        actor,
        now_ms,
    )?;
    let moved = settle(history, meetings, &item.id, actor, now_ms)?;
    if let Some(loop_id) = &item.source_loop_id {
        history.execute(
            "INSERT INTO planning_dismissed_loops (loop_id, dismissed_at_ms) VALUES (?1, ?2)
             ON CONFLICT DO NOTHING",
            params![loop_id, now_ms],
        )?;
    }
    Ok((changed.after, moved))
}

/// The item that tracks a commitment or any earlier occurrence of it.
pub(crate) fn tracking_item(
    history: &Connection,
    meetings: &Connection,
    loop_id: &str,
) -> Result<Option<WorkItem>> {
    tracker_of(history, &lineage(meetings, loop_id)?)
}

/// The item that tracks any loop of a lineage.
pub(crate) fn tracker_of(history: &Connection, lineage: &[String]) -> Result<Option<WorkItem>> {
    for id in lineage {
        if let Some(item) = items::by_source_loop(history, id)? {
            return Ok(Some(item));
        }
    }
    Ok(None)
}
