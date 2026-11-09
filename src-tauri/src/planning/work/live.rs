//! Promises heard during a meeting, in the tracker: filed in Triage at the
//! stop, adopted by the notes' commitment when it comes, and gathered into
//! the follow-up that says who owes what by when.

use super::hygiene::shared_percent;
use super::items::{self, NewItem};
use super::types::{
    WorkActor, WorkItem, WorkList, WorkOwner, WorkQuery, WorkStatus, WorkSuggestionField,
    WorkSuggestionValue,
};
use super::{in_both, list, now_ms, tell, with_meetings, Effects};
use crate::managers::history::HistoryManager;
use crate::meeting::loop_types::MeetingLoopRow;
use crate::meeting::processing::MeetingLivePromise;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::MeetingStore;
use crate::meeting::types::MeetingSessionId;
use crate::planning::sources;
use anyhow::Result;
use chrono::{Local, TimeZone};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

/// How far apart, in the meeting, a heard promise and the notes'
/// commitment may be and still be one promise.
const SAME_MOMENT_MS: i64 = 30_000;

/// What a meeting's promises add up to, for the people in it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkFollowUp {
    pub meeting_id: String,
    /// The message, ready to paste or send. Empty when nobody owes anything.
    pub text: String,
    pub lines: Vec<WorkFollowUpLine>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkFollowUpLine {
    pub item_id: String,
    pub owner_name: String,
    pub title: String,
    pub due_at_ms: Option<i64>,
    pub quote: Option<String>,
}

/// Where a task with no commitment behind it was heard: the meeting, the
/// moment and the words. The title and words only while the conversations
/// are open and the meeting keeps its words.
pub(super) struct Heard {
    pub meeting_id: String,
    pub meeting_title: Option<String>,
    pub at_ms: Option<i64>,
    pub quote: Option<String>,
}

/// Files the promises heard while `session_id` recorded as Triage tasks,
/// each with the moment and the words it was heard in. A promise filed
/// before is not filed again.
pub(crate) fn file_heard(
    app: Option<&AppHandle>,
    store: &MeetingStore,
    session_id: MeetingSessionId,
    promises: &[MeetingLivePromise],
) {
    let Some(app) = app else { return };
    let Some(history) = app.try_state::<Arc<HistoryManager>>() else {
        return;
    };
    let meeting_id = session_id.uuid().to_string();
    let now = now_ms();
    let filed = in_both(store, &history, |history, meetings| {
        file_promises(history, meetings, session_id, promises, now)
    });
    match filed {
        Ok(filed) if !filed.is_empty() => {
            let mut effects = Effects::default();
            for item in &filed {
                effects.item(item);
            }
            tell(app, &effects);
        }
        Ok(_) => {}
        Err(error) => log::warn!("Promises heard in {meeting_id} were not filed: {error:#}"),
    }
}

/// [`file_heard`] inside the caller's transaction on both databases.
pub(crate) fn file_promises(
    history: &Connection,
    meetings: &Connection,
    session_id: MeetingSessionId,
    promises: &[MeetingLivePromise],
    now: i64,
) -> Result<Vec<WorkItem>> {
    let meeting_id = session_id.uuid().to_string();
    let mut filed = Vec::new();
    for promise in promises {
        if let Some(item) = file(history, meetings, &meeting_id, promise, now)? {
            filed.push(item);
        }
    }
    Ok(filed)
}

fn file(
    history: &Connection,
    meetings: &Connection,
    meeting_id: &str,
    promise: &MeetingLivePromise,
    now: i64,
) -> Result<Option<WorkItem>> {
    let at_ms = i64::try_from(promise.start_offset_ns / 1_000_000)?;
    let filed: bool = history.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_evidence_links
         WHERE target_kind = 'task' AND source_kind = 'meeting' AND source_id = ?1
           AND at_ms = ?2 AND caption = ?3)",
        params![meeting_id, at_ms, promise.quote],
        |row| row.get(0),
    )?;
    if filed {
        return Ok(None);
    }
    let owner = match &promise.owner {
        WorkOwner::Person { person_id } if sources::person(meetings, person_id)?.is_none() => {
            WorkOwner::Nobody
        }
        owner => owner.clone(),
    };
    let item = items::insert(
        history,
        &NewItem {
            title: &promise.what,
            details: None,
            status: WorkStatus::Triage,
            priority: 0,
            project_id: None,
            milestone_id: None,
            owner: &owner,
            due_at_ms: promise.due_at_ms,
            label_ids: &[],
            source_loop_id: None,
            recurrence: None,
        },
        WorkActor::Sona,
        now,
    )?;
    history.execute(
        "INSERT INTO planning_evidence_links
            (id, target_kind, target_id, source_kind, source_id, at_ms, revision, created_at_ms,
             caption)
         VALUES (?1, 'task', ?2, 'meeting', ?3, ?4, 1, ?5, ?6)",
        params![
            Uuid::new_v4().to_string(),
            item.id,
            meeting_id,
            at_ms,
            now,
            promise.quote
        ],
    )?;
    Ok(Some(item))
}

/// The task a promise heard live was filed as, when the notes' commitment
/// `row` is that promise: heard in the same meeting with no commitment
/// behind it yet, and said at the same moment in words that overlap, or in
/// mostly the same words. None when nothing matches, or two match equally
/// well.
pub(super) fn heard_live(history: &Connection, row: &MeetingLoopRow) -> Result<Option<WorkItem>> {
    let meeting_id = row.session_id.uuid().to_string();
    let at_ms = i64::try_from(row.at_ms).ok();
    let quote = row.quote.as_deref().unwrap_or_default();
    let mut statement = history.prepare_cached(
        "SELECT t.id, t.title, e.at_ms, e.caption
         FROM planning_tasks t
         JOIN planning_evidence_links e ON e.target_kind = 'task' AND e.target_id = t.id
         WHERE t.source_loop_id IS NULL AND e.source_kind = 'meeting' AND e.source_id = ?1
           AND e.caption IS NOT NULL",
    )?;
    let candidates = statement
        .query_map([&meeting_id], |found| {
            Ok((
                found.get::<_, String>(0)?,
                found.get::<_, String>(1)?,
                found.get::<_, Option<i64>>(2)?,
                found.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut scored: Vec<((bool, usize), String)> = candidates
        .into_iter()
        .map(|(id, title, heard_at, caption)| {
            let close = heard_at
                .zip(at_ms)
                .is_some_and(|(heard, said)| (heard - said).abs() <= SAME_MOMENT_MS);
            let shared = shared_percent(&title, &row.text).max(shared_percent(&caption, quote));
            ((close, shared), id)
        })
        .filter(|((close, shared), _)| (*close && *shared >= 25) || *shared >= 50)
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0));
    scored.dedup_by(|later, earlier| later.1 == earlier.1);
    match scored.as_slice() {
        [] => Ok(None),
        [(best, _), (next, _), ..] if best == next => Ok(None),
        [(_, id), ..] => Ok(Some(items::item(history, id)?)),
    }
}

/// Where each task with no commitment behind it was heard in a meeting, by
/// task id. A meeting deleted or with its words purged is left out.
pub(super) fn heard(
    history: &Connection,
    meetings: Option<&Connection>,
) -> Result<HashMap<String, Heard>> {
    let mut statement = history.prepare_cached(
        "SELECT e.target_id, e.source_id, e.at_ms, e.caption
         FROM planning_evidence_links e
         JOIN planning_tasks t ON t.id = e.target_id
         WHERE e.target_kind = 'task' AND e.source_kind = 'meeting' AND e.caption IS NOT NULL
           AND t.source_loop_id IS NULL
         ORDER BY e.created_at_ms, e.id",
    )?;
    let links = statement
        .query_map([], |found| {
            Ok((
                found.get::<_, String>(0)?,
                found.get::<_, String>(1)?,
                found.get::<_, Option<i64>>(2)?,
                found.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut titles: HashMap<String, Option<String>> = HashMap::new();
    let mut heard = HashMap::new();
    for (task_id, meeting_id, at_ms, quote) in links {
        if heard.contains_key(&task_id) {
            continue;
        }
        let (meeting_title, quote) = match meetings {
            Some(meetings) => {
                if !titles.contains_key(&meeting_id) {
                    titles.insert(meeting_id.clone(), meeting_title(meetings, &meeting_id)?);
                }
                let Some(Some(title)) = titles.get(&meeting_id) else {
                    continue;
                };
                (Some(title.clone()), Some(quote))
            }
            None => (None, None),
        };
        heard.insert(
            task_id,
            Heard {
                meeting_id,
                meeting_title,
                at_ms,
                quote,
            },
        );
    }
    Ok(heard)
}

fn meeting_title(meetings: &Connection, id: &str) -> Result<Option<String>> {
    Ok(meetings
        .query_row(
            "SELECT title FROM meeting_sessions WHERE id = ?1
             AND phase != 'deleting' AND transcript_purged_at_utc_ms IS NULL",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Who owes what from one meeting, by when, in the words they said it.
#[tauri::command]
#[specta::specta]
pub async fn work_follow_up(
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    meeting_id: String,
) -> Result<WorkFollowUp, String> {
    let user_name = meetings.user_name();
    let now = now_ms();
    with_meetings(&history_state, &meetings, |history, meeting_conn| {
        let title = match meeting_conn {
            Some(meetings) => meeting_title(meetings, &meeting_id)?,
            None => None,
        };
        let query = WorkQuery {
            source_meeting_id: Some(meeting_id.clone()),
            include_snoozed: true,
            ..WorkQuery::default()
        };
        let listed = list(history, meeting_conn, user_name.as_deref(), &query, now)?;
        Ok(follow_up(
            meeting_id,
            title.as_deref(),
            user_name.as_deref(),
            listed,
        ))
    })
    .await
}

/// The follow-up for `listed`: every promise still owed, grouped by who owes
/// it, the user first and nobody last, soonest due first. A promise still in
/// triage stands under the owner and day Sona suggests for it.
pub(crate) fn follow_up(
    meeting_id: String,
    title: Option<&str>,
    user_name: Option<&str>,
    listed: WorkList,
) -> WorkFollowUp {
    let suggested = |item_id: &str, field: WorkSuggestionField| {
        listed
            .suggestions
            .get(item_id)
            .into_iter()
            .flatten()
            .find(|suggestion| suggestion.field == field)
            .map(|suggestion| &suggestion.value)
    };
    let mut lines: Vec<(u8, WorkFollowUpLine, i64)> = listed
        .items
        .into_iter()
        .filter(|row| row.item.status == WorkStatus::Triage || row.item.status.is_open())
        .filter(|row| row.item.archived_at_ms.is_none())
        .map(|row| {
            let (rank, owner_name) = if row.item.owner_is_self {
                (0, user_name.unwrap_or("Me").to_string())
            } else if let Some(name) = row.owner_name.as_deref() {
                (1, name.to_string())
            } else {
                match suggested(&row.item.id, WorkSuggestionField::Owner) {
                    Some(WorkSuggestionValue::Owner(WorkOwner::Myself)) => {
                        (0, user_name.unwrap_or("Me").to_string())
                    }
                    Some(WorkSuggestionValue::Owner(WorkOwner::Person { person_id })) => {
                        match listed.people.iter().find(|person| person.id == *person_id) {
                            Some(person) => (1, person.name.clone()),
                            None => (2, "Not yet assigned".to_string()),
                        }
                    }
                    _ => (2, "Not yet assigned".to_string()),
                }
            };
            let due_at_ms = row.item.due_at_ms.or_else(|| {
                match suggested(&row.item.id, WorkSuggestionField::Due) {
                    Some(WorkSuggestionValue::Due(at_ms)) => Some(*at_ms),
                    _ => None,
                }
            });
            (
                rank,
                WorkFollowUpLine {
                    item_id: row.item.id,
                    owner_name,
                    title: row.item.title,
                    due_at_ms,
                    quote: row.source_quote,
                },
                row.item.number,
            )
        })
        .collect();
    lines.sort_by(|(rank, left, number), (other_rank, right, other_number)| {
        rank.cmp(other_rank)
            .then_with(|| {
                left.owner_name
                    .to_lowercase()
                    .cmp(&right.owner_name.to_lowercase())
            })
            .then_with(|| match (left.due_at_ms, right.due_at_ms) {
                (Some(left), Some(right)) => left.cmp(&right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| number.cmp(other_number))
    });
    let lines: Vec<WorkFollowUpLine> = lines.into_iter().map(|(_, line, _)| line).collect();
    WorkFollowUp {
        meeting_id,
        text: text(title, &lines),
        lines,
    }
}

fn text(title: Option<&str>, lines: &[WorkFollowUpLine]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut text = match title {
        Some(title) => format!("Who owes what from {title}:\n"),
        None => "Who owes what:\n".to_string(),
    };
    let mut owner: Option<&str> = None;
    for line in lines {
        if owner != Some(line.owner_name.as_str()) {
            owner = Some(&line.owner_name);
            text.push('\n');
            text.push_str(&line.owner_name);
            text.push('\n');
        }
        text.push_str("- ");
        text.push_str(&line.title);
        if let Some(due) = line.due_at_ms.and_then(day) {
            text.push_str(", by ");
            text.push_str(&due);
        }
        text.push('\n');
        if let Some(quote) = line.quote.as_deref().filter(|quote| !quote.is_empty()) {
            text.push_str("  \u{201c}");
            text.push_str(quote);
            text.push_str("\u{201d}\n");
        }
    }
    text
}

fn day(at_ms: i64) -> Option<String> {
    Local
        .timestamp_millis_opt(at_ms)
        .single()
        .map(|at| at.format("%a %-d %b").to_string())
}
