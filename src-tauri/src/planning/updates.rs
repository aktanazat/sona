//! Project updates: a health and a note on how a project is going, newest
//! first, and a plain draft of what moved since the last one.

use super::progress;
use super::store::{self, Refusal};
use super::types::{
    ProjectHealth, ProjectUpdate, ProjectUpdateAuthor, ProjectUpdatePage, TaskStatus,
};
use super::work::comments::Author;
use crate::query::KeptLocal;
use anyhow::Result;
use chrono::{Local, TimeZone};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::HashSet;
use std::fmt::Write as _;
use uuid::Uuid;

pub(crate) const PAGE: usize = 20;
pub(crate) const MAX_PAGE: usize = 100;
/// The draft names progress only once it moved more than this many points.
const PROGRESS_STEP: u8 = 2;
/// How much of a meeting's words a draft quotes, around the linked moment.
const PASSAGE_WINDOW_MS: i64 = 15_000;
const PASSAGE_CHARS: usize = 240;
/// How a draft's line quoting a meeting begins.
const DISCUSSED: &str = "- Discussed in ";

/// The meetings a drafted update quoted, as a JSON list of ids. Null for an
/// update its author wrote, or one posted before this was kept.
pub(crate) const QUOTED_MEETINGS_MIGRATION: &str =
    "ALTER TABLE planning_project_updates ADD COLUMN quoted_meetings TEXT;";

const COLUMNS: &str = "id, project_id, health, body, progress, author_kind, author_name,
    created_at_ms, edited_at_ms, deleted_at_ms";

fn read(row: &Row<'_>) -> rusqlite::Result<ProjectUpdate> {
    let deleted = row.get::<_, Option<i64>>(9)?.is_some();
    Ok(ProjectUpdate {
        id: row.get(0)?,
        project_id: row.get(1)?,
        health: row.get(2)?,
        body: if deleted { None } else { Some(row.get(3)?) },
        progress: row.get(4)?,
        author: ProjectUpdateAuthor {
            kind: row.get(5)?,
            name: row.get(6)?,
        },
        created_at_ms: row.get(7)?,
        edited_at_ms: row.get(8)?,
        deleted,
    })
}

pub(crate) fn update(conn: &Connection, id: &str) -> Result<ProjectUpdate> {
    let id = store::uuid(id)?;
    let found = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM planning_project_updates WHERE id = ?1"),
            [&id],
            read,
        )
        .optional()?
        .ok_or(Refusal::NotFound)?;
    if found.deleted {
        return Err(Refusal::NotFound.into());
    }
    Ok(found)
}

fn wrote(author: Author<'_>, update: &ProjectUpdate) -> bool {
    update.author.kind == author.actor
        && match (author.name(), update.author.name.as_deref()) {
            (Some(mine), Some(theirs)) => mine.eq_ignore_ascii_case(theirs),
            (None, None) => true,
            _ => false,
        }
}

fn clean_body(body: &str) -> Result<String> {
    let body = body.trim();
    if body.is_empty() {
        return Err(Refusal::Invalid.into());
    }
    Ok(body.to_owned())
}

fn touch(conn: &Connection, project_id: &str, now_ms: i64) -> Result<()> {
    conn.execute(
        "UPDATE planning_projects SET updated_at_ms = ?2 WHERE id = ?1",
        params![project_id, now_ms],
    )?;
    Ok(())
}

/// Posts an update. `quoted` holds the `discussed` lines of the draft the
/// body was made from, and is none for a body its author wrote.
pub(crate) fn add(
    conn: &Connection,
    author: Author<'_>,
    project_id: &str,
    health: ProjectHealth,
    body: &str,
    quoted: Option<&[Discussed]>,
    now_ms: i64,
) -> Result<ProjectUpdate> {
    let project = store::project(conn, &store::uuid(project_id)?)?;
    if project.trashed_at_ms.is_some() {
        return Err(Refusal::NotFound.into());
    }
    let quoted = quoted
        .map(|lines| {
            serde_json::to_string(
                &lines
                    .iter()
                    .map(|line| line.meeting_id.as_str())
                    .collect::<Vec<_>>(),
            )
        })
        .transpose()?;
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO planning_project_updates
            (id, project_id, health, body, progress, author_kind, author_name, created_at_ms,
             quoted_meetings)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            project.id,
            health.as_str(),
            clean_body(body)?,
            progress::percent_now(conn, &project.id)?,
            author.actor.as_str(),
            author.name(),
            now_ms,
            quoted
        ],
    )?;
    touch(conn, &project.id, now_ms)?;
    update(conn, &id)
}

/// The updates an agent may read the `discussed` lines of: each that
/// recorded the meetings it quoted, none of which `hides` now hides. Any
/// other update, or one no longer here, is read without them.
pub(crate) fn quotes_shown(
    conn: &Connection,
    hides: impl Fn(&str) -> bool,
) -> Result<HashSet<String>> {
    let mut statement = conn.prepare(
        "SELECT id, quoted_meetings FROM planning_project_updates
          WHERE quoted_meetings IS NOT NULL",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut shown = HashSet::new();
    for (id, quoted) in rows {
        let quoted: Vec<String> = serde_json::from_str(&quoted)?;
        if !quoted.iter().any(|meeting| hides(meeting)) {
            shown.insert(id);
        }
    }
    Ok(shown)
}

/// The body as an agent reads it: without its `discussed` lines unless the
/// update is in `shown`.
pub(crate) fn for_agents(id: &str, body: &str, shown: &HashSet<String>) -> String {
    if shown.contains(id) {
        body.to_owned()
    } else {
        unquoted(body)
    }
}

/// One-line `text` cut before its first `discussed` line, for text that no
/// longer says which meetings it quoted; none when nothing comes before it.
pub(crate) fn before_quotes(text: &str) -> Option<String> {
    let Some(at) = text.find(DISCUSSED) else {
        return Some(text.to_owned());
    };
    let head = text[..at].trim_end();
    (!head.is_empty()).then(|| format!("{head}…"))
}

/// A body without its `discussed` lines.
fn unquoted(body: &str) -> String {
    let mut quoting = false;
    body.lines()
        .filter(|line| {
            let line = line.trim_start();
            if line.starts_with(DISCUSSED) {
                quoting = true;
            } else if line.starts_with("- ") {
                quoting = false;
            }
            !quoting
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn edit(
    conn: &Connection,
    author: Author<'_>,
    id: &str,
    health: Option<ProjectHealth>,
    body: Option<&str>,
    now_ms: i64,
) -> Result<ProjectUpdate> {
    let current = update(conn, id)?;
    if !wrote(author, &current) {
        return Err(Refusal::NotAuthor.into());
    }
    let body = match body {
        Some(body) => clean_body(body)?,
        None => current.body.clone().unwrap_or_default(),
    };
    let health = health.unwrap_or(current.health);
    if Some(&body) == current.body.as_ref() && health == current.health {
        return Ok(current);
    }
    conn.execute(
        "UPDATE planning_project_updates SET health = ?2, body = ?3, edited_at_ms = ?4 WHERE id = ?1",
        params![current.id, health.as_str(), body, now_ms],
    )?;
    touch(conn, &current.project_id, now_ms)?;
    update(conn, &current.id)
}

/// Blanks the update and keeps its row; it no longer lists.
pub(crate) fn delete(
    conn: &Connection,
    author: Author<'_>,
    id: &str,
    now_ms: i64,
) -> Result<ProjectUpdate> {
    let current = update(conn, id)?;
    if !wrote(author, &current) {
        return Err(Refusal::NotAuthor.into());
    }
    conn.execute(
        "UPDATE planning_project_updates SET body = '', deleted_at_ms = ?2 WHERE id = ?1",
        params![current.id, now_ms],
    )?;
    touch(conn, &current.project_id, now_ms)?;
    Ok(ProjectUpdate {
        body: None,
        deleted: true,
        ..current
    })
}

pub(crate) fn latest(conn: &Connection, project_id: &str) -> Result<Option<ProjectUpdate>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM planning_project_updates
                 WHERE project_id = ?1 AND deleted_at_ms IS NULL
                 ORDER BY created_at_ms DESC, id DESC LIMIT 1"
            ),
            [project_id],
            read,
        )
        .optional()?)
}

/// Updates newest first; `cursor` is the last update of the page before.
pub(crate) fn page(
    conn: &Connection,
    project_id: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<ProjectUpdatePage> {
    let project_id = store::uuid(project_id)?;
    store::project(conn, &project_id)?;
    let limit = limit.clamp(1, MAX_PAGE);
    let after = match cursor {
        Some(cursor) => {
            let found = update(conn, cursor)?;
            if found.project_id != project_id {
                return Err(Refusal::NotFound.into());
            }
            Some((found.created_at_ms, found.id))
        }
        None => None,
    };
    let (at, id) = after.unwrap_or((i64::MAX, String::new()));
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM planning_project_updates
         WHERE project_id = ?1 AND deleted_at_ms IS NULL
           AND (?4 = '' OR created_at_ms < ?2 OR (created_at_ms = ?2 AND id < ?4))
         ORDER BY created_at_ms DESC, id DESC LIMIT ?3"
    ))?;
    let mut updates = statement
        .query_map(params![project_id, at, i64::try_from(limit + 1)?, id], read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = if updates.len() > limit {
        updates.truncate(limit);
        updates.last().map(|update| update.id.clone())
    } else {
        None
    };
    Ok(ProjectUpdatePage {
        updates,
        next_cursor,
    })
}

fn moved(conn: &Connection, project_id: &str, since_ms: i64, to: &str) -> Result<u32> {
    Ok(conn.query_row(
        "SELECT count(DISTINCT a.task_id) FROM planning_task_activity a
         JOIN planning_tasks t ON t.id = a.task_id
         WHERE t.project_id = ?1 AND t.archived_at_ms IS NULL AND a.kind = 'status'
           AND a.to_value = ?3 AND a.at_ms > ?2 AND t.status = ?3",
        params![project_id, since_ms, to],
        |row| row.get(0),
    )?)
}

fn plural(count: u32, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// A meeting the project's work is linked to, and the moment in it.
#[derive(Clone, Debug)]
pub(crate) struct MeetingMention {
    pub meeting_id: String,
    pub at_ms: Option<i64>,
    pub caption: Option<String>,
}

/// When the next update's draft counts from: the last update, or the
/// project's start.
fn since(conn: &Connection, project_id: &str) -> Result<(Option<ProjectUpdate>, i64)> {
    let project = store::project(conn, &store::uuid(project_id)?)?;
    let last = latest(conn, &project.id)?;
    let since_ms = last
        .as_ref()
        .map_or(project.created_at_ms, |update| update.created_at_ms);
    Ok((last, since_ms))
}

/// The meetings linked to the project, its milestones or its open tasks,
/// with the moment the next draft counts from.
pub(crate) fn meeting_mentions(
    conn: &Connection,
    project_id: &str,
) -> Result<(i64, Vec<MeetingMention>)> {
    let (_, since_ms) = since(conn, project_id)?;
    let mut statement = conn.prepare(
        "SELECT source_id, at_ms, caption FROM planning_evidence_links
          WHERE source_kind = 'meeting' AND (
                (target_kind = 'project' AND target_id = ?1)
             OR (target_kind = 'milestone' AND target_id IN
                    (SELECT id FROM planning_milestones WHERE project_id = ?1))
             OR (target_kind = 'task' AND target_id IN
                    (SELECT id FROM planning_tasks WHERE project_id = ?1 AND archived_at_ms IS NULL)))
          ORDER BY source_id, at_ms IS NULL, at_ms, id",
    )?;
    let mentions = statement
        .query_map([store::uuid(project_id)?], |row| {
            Ok(MeetingMention {
                meeting_id: row.get(0)?,
                at_ms: row.get(1)?,
                caption: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((since_ms, mentions))
}

/// The words said around a moment of a meeting, as its transcript now
/// reads, shortened to a quote.
fn passage(meetings: &Connection, meeting_id: &str, at_ms: i64) -> Result<Option<String>> {
    let from_ns = at_ms.saturating_sub(PASSAGE_WINDOW_MS).max(0).saturating_mul(1_000_000);
    let to_ns = at_ms.saturating_add(PASSAGE_WINDOW_MS).saturating_mul(1_000_000);
    let mut statement = meetings.prepare_cached(
        "SELECT coalesce(
                (SELECT e.replacement_text FROM meeting_segment_edits e
                  WHERE e.segment_id = s.segment_id ORDER BY e.edit_sequence DESC LIMIT 1),
                s.base_text)
           FROM meeting_sessions m
           JOIN meeting_transcript_segments s ON s.transcript_revision_id = m.current_transcript_revision_id
          WHERE m.id = ?1 AND s.start_offset_ns < ?3 AND s.end_offset_ns > ?2
            AND coalesce((SELECT e.removed FROM meeting_segment_edits e
                  WHERE e.segment_id = s.segment_id ORDER BY e.edit_sequence DESC LIMIT 1), 0) = 0
          ORDER BY s.start_offset_ns",
    )?;
    let words = statement
        .query_map(params![meeting_id, from_ns, to_ns], |row| {
            row.get::<_, Option<String>>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if words.is_empty() {
        return Ok(None);
    }
    if words.chars().count() <= PASSAGE_CHARS {
        return Ok(Some(words));
    }
    let cut: String = words.chars().take(PASSAGE_CHARS).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    Ok(Some(format!("{}…", cut.trim_end())))
}

/// A draft's line quoting a meeting, and the meeting.
#[derive(Clone, Debug)]
pub(crate) struct Discussed {
    pub meeting_id: String,
    pub line: String,
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One draft line per linked meeting held since `since_ms`, quoting what
/// was said at the linked moment. A meeting kept on this Mac, deleted, or
/// with its transcript purged is left out.
pub(crate) fn discussed(
    meetings: &Connection,
    kept: &KeptLocal,
    since_ms: i64,
    mentions: &[MeetingMention],
) -> Result<Vec<Discussed>> {
    let mut lines = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for mention in mentions {
        if seen.contains(&mention.meeting_id.as_str()) {
            continue;
        }
        seen.push(&mention.meeting_id);
        if kept.hides_meeting(&mention.meeting_id) {
            continue;
        }
        let meeting: Option<(Option<String>, i64)> = meetings
            .query_row(
                "SELECT title, started_at_utc_ms FROM meeting_sessions
                  WHERE id = ?1 AND phase != 'deleting' AND transcript_purged_at_utc_ms IS NULL",
                [&mention.meeting_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((title, started_at_ms)) = meeting else {
            continue;
        };
        if started_at_ms <= since_ms {
            continue;
        }
        let title = title
            .map(|title| one_line(&title))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "a conversation".to_owned());
        let day = Local
            .timestamp_millis_opt(started_at_ms)
            .single()
            .map_or_else(String::new, |at| at.format("%b %-d").to_string());
        let quote = match mention.at_ms {
            Some(at_ms) => passage(meetings, &mention.meeting_id, at_ms)?,
            None => None,
        }
        .or_else(|| mention.caption.clone());
        lines.push(Discussed {
            meeting_id: mention.meeting_id.clone(),
            line: match quote {
                Some(quote) => format!("{DISCUSSED}{title} ({day}): \"{}\"", one_line(&quote)),
                None => format!("{DISCUSSED}{title} ({day})."),
            },
        });
    }
    Ok(lines)
}

/// A plain Markdown draft of what changed since the last update, or since
/// the project began: tasks finished, started and added, milestones
/// reached, the one in focus, progress once it moved past two points, and
/// the `discussed` lines for meetings.
pub(crate) fn draft(
    conn: &Connection,
    project_id: &str,
    discussed: &[Discussed],
) -> Result<String> {
    let project = store::project(conn, &store::uuid(project_id)?)?;
    let (last, since_ms) = since(conn, &project.id)?;
    let done = moved(conn, &project.id, since_ms, "done")?;
    let started = moved(conn, &project.id, since_ms, "in_progress")?;
    let added: u32 = conn.query_row(
        "SELECT count(*) FROM planning_tasks
         WHERE project_id = ?1 AND archived_at_ms IS NULL AND created_at_ms > ?2",
        params![project.id, since_ms],
        |row| row.get(0),
    )?;
    let (milestones, progress) = progress::milestone_progress(conn, &project.id)?;
    let mut text = String::new();
    let tasks = [
        (done, "task finished", "tasks finished"),
        (started, "task started", "tasks started"),
        (added, "task added", "tasks added"),
    ]
    .into_iter()
    .filter(|(count, _, _)| *count > 0)
    .map(|(count, one, many)| plural(count, one, many))
    .collect::<Vec<_>>();
    if tasks.is_empty() {
        text.push_str("- No task moved");
    } else {
        write!(text, "- {}", tasks.join(", "))?;
    }
    if last.is_some() {
        text.push_str(" since the last update.\n");
    } else {
        text.push_str(" so far.\n");
    }
    for (milestone, counts) in milestones.iter().zip(&progress) {
        let reached = milestone.updated_at_ms > since_ms
            && (milestone.status == TaskStatus::Done
                || (counts.scope > 0 && counts.done == counts.scope));
        if reached {
            writeln!(text, "- Milestone reached: {}.", milestone.title)?;
        }
        if counts.next_open {
            writeln!(
                text,
                "- In focus: {}, {}% complete.",
                milestone.title, counts.percent
            )?;
        }
    }
    let now = progress::percent_now(conn, &project.id)?;
    let before = last.as_ref().map_or(0, |update| update.progress);
    if now.abs_diff(before) > PROGRESS_STEP {
        writeln!(text, "- Progress: {before}% to {now}%.")?;
    }
    for discussed in discussed {
        writeln!(text, "{}", discussed.line)?;
    }
    Ok(text.trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::store::{workflow_core_tests, MeetingStore};
    use crate::meeting::types::{MeetingOperationId, MeetingSessionId};
    use crate::planning::catalog::{save_project, ProjectEdit};
    use crate::planning::schema;
    use crate::planning::work::types::WorkActor;
    use crate::query::external::tracker::MeetingFence;

    fn keep_series_here(meetings: &MeetingStore, meeting: MeetingSessionId, started_ms: i64) {
        let event = CalendarEventSummary {
            event_key: "board-prep#1".to_owned(),
            series_key: "board-prep".to_owned(),
            title: "Board prep".to_owned(),
            attendee_count: 2,
            start_utc_ms: started_ms,
            end_utc_ms: started_ms + 1_800_000,
            attendees: Vec::new(),
            notes: None,
            calendar_name: None,
            location: None,
            url: None,
            place: None,
        };
        meetings
            .remember_calendar_facts(meeting, &event)
            .expect("the calendar facts");
        meetings
            .set_series_remote_opt_out(
                &MeetingSeriesRemoteOptOutSetRequest {
                    operation_id: MeetingOperationId::new(),
                    series_key: event.series_key.clone(),
                    remote_intelligence_opt_out: true,
                    expected_revision: 0,
                },
                3,
            )
            .expect("the series kept on this Mac");
    }

    /// A draft quotes the meetings that discussed the project, and never
    /// one whose series is kept on this Mac.
    #[test]
    fn a_draft_quotes_shared_meetings_and_leaves_out_ones_kept_on_this_mac() {
        let (_directory, meetings) = workflow_core_tests::store();
        let shared = workflow_core_tests::meeting(&meetings, "Pricing sync", 1_700_000_000_000);
        workflow_core_tests::transcript(&meetings, shared, "We agreed to ship the beta");
        let private = workflow_core_tests::meeting(&meetings, "Board prep", 1_700_000_100_000);
        workflow_core_tests::transcript(&meetings, private, "Salary numbers for the board");
        keep_series_here(&meetings, private, 1_700_000_100_000);
        let kept = KeptLocal::read(&meetings).expect("the kept meetings");
        let mentions = [shared, private].map(|id| MeetingMention {
            meeting_id: id.uuid().to_string(),
            at_ms: Some(0),
            caption: None,
        });

        let lines = meetings
            .with_planning_snapshot(|conn| Ok(discussed(conn, &kept, 0, &mentions)))
            .expect("a snapshot")
            .expect("the discussed lines");

        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].line.starts_with("- Discussed in Pricing sync ("),
            "{lines:?}"
        );
        assert!(
            lines[0].line.ends_with(": \"We agreed to ship the beta\""),
            "{lines:?}"
        );
    }

    struct Posted {
        _directory: tempfile::TempDir,
        meetings: std::sync::Arc<MeetingStore>,
        board: MeetingSessionId,
        history: Connection,
        drafted: ProjectUpdate,
        written: ProjectUpdate,
    }

    /// An active project with a drafted update quoting a shared meeting, and
    /// a later one its author wrote around the same quote.
    fn posted() -> Posted {
        let (directory, meetings) = workflow_core_tests::store();
        let board = workflow_core_tests::meeting(&meetings, "Board prep", 1_700_000_100_000);
        workflow_core_tests::transcript(&meetings, board, "Salary numbers for the board");
        let history = Connection::open_in_memory().expect("a history database");
        schema::apply_all(&history).expect("the planner schema");
        let project = save_project(
            &history,
            None,
            &ProjectEdit {
                title: Some("Launch".to_owned()),
                status: Some(crate::planning::types::ProjectStatus::Active),
                ..ProjectEdit::default()
            },
            1,
        )
        .expect("a project");
        let agent = Author {
            actor: WorkActor::Agent,
            agent: Some("bot"),
        };
        let mentions = [MeetingMention {
            meeting_id: board.uuid().to_string(),
            at_ms: Some(0),
            caption: None,
        }];
        let shared = KeptLocal::read(&meetings).expect("the kept meetings");
        let lines = meetings
            .with_planning_snapshot(|conn| Ok(discussed(conn, &shared, 0, &mentions)))
            .expect("a snapshot")
            .expect("the discussed lines");
        let body = draft(&history, &project.id, &lines).expect("a draft");
        assert!(body.contains("Salary numbers"), "{body}");
        let drafted = add(
            &history,
            agent,
            &project.id,
            ProjectHealth::OnTrack,
            &body,
            Some(&lines),
            2,
        )
        .expect("the drafted update");
        let written = add(
            &history,
            agent,
            &project.id,
            ProjectHealth::AtRisk,
            &format!("{body}\n- Next: pricing."),
            None,
            3,
        )
        .expect("the written update");
        Posted {
            _directory: directory,
            meetings,
            board,
            history,
            drafted,
            written,
        }
    }

    fn read_by_agents(update: &ProjectUpdate, shown: &HashSet<String>) -> String {
        for_agents(&update.id, update.body.as_deref().unwrap_or_default(), shown)
    }

    fn shown(history: &Connection, meetings: &MeetingStore) -> HashSet<String> {
        let fence = MeetingFence::of_store(Some(meetings));
        quotes_shown(history, |meeting| fence.hides_meeting(meeting)).expect("the shown quotes")
    }

    /// A posted draft that quoted a meeting loses the quote for agents once
    /// the meeting's series is kept on this Mac; so does an update with no
    /// record of what it quoted.
    #[test]
    fn a_posted_quote_is_hidden_once_its_meeting_is_kept_on_this_mac() {
        let posted = posted();
        let open = shown(&posted.history, &posted.meetings);
        assert!(read_by_agents(&posted.drafted, &open).contains("Salary"));
        let written = read_by_agents(&posted.written, &open);
        assert!(!written.contains("Board prep") && !written.contains("Salary"), "{written}");
        assert!(written.ends_with("- Next: pricing."), "{written}");

        keep_series_here(&posted.meetings, posted.board, 1_700_000_100_000);

        let kept = shown(&posted.history, &posted.meetings);
        assert!(!read_by_agents(&posted.drafted, &kept).contains("Salary"));
    }

    /// A quote of a meeting the store no longer holds stays hidden, since
    /// the meeting can no longer say whether it was kept on this Mac.
    #[test]
    fn a_posted_quote_is_hidden_once_its_meeting_is_gone() {
        let posted = posted();
        let (_elsewhere, without) = workflow_core_tests::store();

        let gone = shown(&posted.history, &without);

        assert!(!read_by_agents(&posted.drafted, &gone).contains("Salary"));
    }

    /// The change stream keeps an update's old body after the update is
    /// purged; the quote in it stays hidden with no row left to vouch for it.
    #[test]
    fn a_purged_update_keeps_its_quote_hidden() {
        let posted = posted();
        posted
            .history
            .execute(
                "DELETE FROM planning_project_updates WHERE id = ?1",
                [&posted.drafted.id],
            )
            .expect("the update purged");

        let purged = shown(&posted.history, &posted.meetings);

        assert!(!read_by_agents(&posted.drafted, &purged).contains("Salary"));
    }

    /// The digest's excerpt of a project's latest update leaves out the
    /// quote of a meeting kept on this Mac.
    #[test]
    fn a_digest_leaves_out_the_quote_of_a_meeting_kept_on_this_mac() {
        let posted = posted();
        posted
            .history
            .execute(
                "DELETE FROM planning_project_updates WHERE id = ?1",
                [&posted.written.id],
            )
            .expect("only the draft left");
        keep_series_here(&posted.meetings, posted.board, 1_700_000_100_000);
        let quotes = shown(&posted.history, &posted.meetings);

        let digest = crate::planning::work::digest::build(
            &posted.history,
            &HashSet::new(),
            &quotes,
            0,
            1_700_000_200_000,
        )
        .expect("the digest");

        let excerpts: Vec<_> = digest
            .projects
            .iter()
            .map(|project| project.latest_update.as_ref().and_then(|u| u.excerpt.clone()))
            .collect();
        assert_eq!(excerpts.len(), 1, "{excerpts:?}");
        assert!(!format!("{excerpts:?}").contains("Salary"), "{excerpts:?}");
    }

    /// A digest stored in the inbox while its quote could be shown loses the
    /// quote when an agent reads it back, whatever the meeting is now.
    #[test]
    fn a_stored_digest_read_by_agents_leaves_out_quotes() {
        let posted = posted();
        posted
            .history
            .execute(
                "DELETE FROM planning_project_updates WHERE id = ?1",
                [&posted.written.id],
            )
            .expect("only the draft left");
        let quotes = shown(&posted.history, &posted.meetings);
        let digest = crate::planning::work::digest::build(
            &posted.history,
            &HashSet::new(),
            &quotes,
            0,
            1_700_000_200_000,
        )
        .expect("the digest");
        assert!(format!("{digest:?}").contains("Salary"), "{digest:?}");
        let stored = crate::planning::work::inbox::digest_posted(&posted.history, &digest, None, 4)
            .expect("a post")
            .and_then(|notice| notice.digest)
            .expect("the stored digest");

        let read = stored.for_agents();

        assert_eq!(read.projects.len(), 1);
        assert!(!format!("{read:?}").contains("Salary"), "{read:?}");
    }
}
