//! Follow-through: work comes back when it should. What I owe today and
//! what someone owes me past their day reach me as one notification each, a
//! wait carries a nudge to send, and a meeting with open work between its
//! people gets a brief a few minutes before it starts.

use super::super::sources;
use super::super::store::{self, Refusal};
use super::items;
use super::sync::People;
use super::types::{WorkItem, WorkOwner, WorkSource};
use super::{in_both, now_ms, run_history, with_both, Done, Effects};
use crate::managers::history::HistoryManager;
use crate::meeting::detection::calendar::CalendarAccess;
use crate::meeting::detection::machine::{CalendarEventSummary, ParticipationStatus};
use crate::meeting::detection::notify::{FollowUpOpener, PromptPresenter};
use crate::meeting::detection::DetectionRuntime;
use crate::meeting::follow_up::{self, MeetingFollowUpMailBody};
use crate::meeting::loop_types::MeetingLoopId;
use crate::meeting::mail_context::{self, MailContextState};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::loops::readable_artifacts_in;
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::MeetingSessionId;
use anyhow::Result;
use chrono::{Local, TimeZone};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager, State};
use tauri_specta::Event;
use uuid::Uuid;

const MINUTE_MS: i64 = 60 * 1000;
const DAY_MS: i64 = 24 * 60 * MINUTE_MS;
/// How long a wait stands before it is worth asking about, counted from
/// the last nudge or, never nudged, from when its status last moved.
const FOLLOW_UP_AFTER_MS: i64 = 7 * DAY_MS;
/// The hour of a due day its reminder arrives.
const MORNING_HOUR: u32 = 9;
/// How long before a meeting its brief arrives.
const BRIEF_LEAD_MS: i64 = 5 * MINUTE_MS;
const TICK: Duration = Duration::from_secs(60);
/// How long a shown reminder is remembered. A wait still untouched after
/// this is worth another word.
const REMEMBERED_MS: i64 = 60 * DAY_MS;
/// How many lines a notification names before "and N more".
const NAMED: usize = 3;
/// How much of a decision a brief's notification quotes.
const DECISION_CHARS: usize = 90;
/// Every reminder's notification id starts with this, so a click can be
/// told apart from anything else the notification centre delivers.
const REQUEST: &str = "computer.sona.work.follow/";

/// When each waiting-on item was last nudged, read once per request.
#[derive(Default)]
pub(crate) struct FollowUps {
    nudged: HashMap<String, i64>,
}

impl FollowUps {
    pub(crate) fn read(history: &Connection) -> Result<Self> {
        let mut statement = history.prepare("SELECT task_id, nudged_at_ms FROM planning_nudges")?;
        let nudged = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Self { nudged })
    }

    /// When to ask about open work someone else owes: the morning of its
    /// due day while that is still ahead of the last nudge, otherwise a
    /// week after the last nudge or, never nudged, after its status last
    /// moved. A snooze holds it back. Work nobody else owes has none.
    pub(crate) fn at(&self, item: &WorkItem) -> Result<Option<i64>> {
        if !matches!(item.owner(), WorkOwner::Person { .. })
            || !item.status.is_open()
            || item.archived_at_ms.is_some()
        {
            return Ok(None);
        }
        let nudged = self.nudged.get(&item.id).copied();
        let morning = item.due_at_ms.map(morning_of).transpose()?;
        let at = match (morning, nudged) {
            (Some(morning), Some(nudged)) if morning > nudged => morning,
            (Some(morning), None) => morning,
            (_, Some(nudged)) => nudged + FOLLOW_UP_AFTER_MS,
            (None, None) => item.status_changed_at_ms + FOLLOW_UP_AFTER_MS,
        };
        Ok(Some(
            item.snoozed_until_ms.map_or(at, |until| at.max(until)),
        ))
    }
}

/// Nine in the morning of the local day `at_ms` falls on, or `at_ms`
/// itself when that comes first.
fn morning_of(at_ms: i64) -> Result<i64> {
    let day = Local
        .timestamp_millis_opt(at_ms)
        .single()
        .ok_or_else(|| anyhow::anyhow!("clock is outside the supported calendar"))?
        .date_naive();
    let morning = day
        .and_hms_opt(MORNING_HOUR, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("the morning is outside the calendar"))?;
    let morning = Local
        .from_local_datetime(&morning)
        .earliest()
        .ok_or_else(|| anyhow::anyhow!("the morning does not exist on this clock"))?;
    Ok(morning.timestamp_millis().min(at_ms))
}

/// Where each item came from: an agent that made it, a scratchpad note it
/// cites, a meeting it tracks or cites, and otherwise a person.
pub(crate) fn item_sources(history: &Connection) -> Result<HashMap<String, WorkSource>> {
    let mut statement = history.prepare(
        "SELECT t.id,
            CASE
              WHEN EXISTS (SELECT 1 FROM planning_task_activity a
                            WHERE a.task_id = t.id AND a.kind = 'created' AND a.actor = 'agent')
                THEN 'agent'
              WHEN EXISTS (SELECT 1 FROM planning_evidence_links l
                            WHERE l.target_kind = 'task' AND l.target_id = t.id
                              AND l.source_kind = 'note')
                THEN 'dictation'
              WHEN t.source_loop_id IS NOT NULL
                OR EXISTS (SELECT 1 FROM planning_evidence_links l
                            WHERE l.target_kind = 'task' AND l.target_id = t.id
                              AND l.source_kind IN ('meeting', 'loop'))
                THEN 'meeting'
              ELSE 'person'
            END
         FROM planning_tasks t",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| {
        let (id, source) = row?;
        let source = match source.as_str() {
            "agent" => WorkSource::Agent,
            "dictation" => WorkSource::Dictation,
            "meeting" => WorkSource::Meeting,
            _ => WorkSource::Person,
        };
        Ok((id, source))
    })
    .collect()
}

/// What a reminder is about, and so where its click lands.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkReminderKind {
    Due,
    FollowUp,
    Brief,
}

impl WorkReminderKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Due => "due",
            Self::FollowUp => "follow",
            Self::Brief => "brief",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Due, Self::FollowUp, Self::Brief]
            .into_iter()
            .find(|kind| kind.as_str() == value)
    }
}

/// A reminder was clicked: `id` is the item or calendar event it was about,
/// or none when it named several items.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
pub struct WorkReminderOpened {
    pub kind: WorkReminderKind,
    pub id: Option<String>,
}

fn request_id(kind: WorkReminderKind, about: Option<&str>) -> String {
    match about {
        Some(about) => format!("{REQUEST}{}/{about}", kind.as_str()),
        None => format!("{REQUEST}{}", kind.as_str()),
    }
}

fn opened(request: &str) -> Option<WorkReminderOpened> {
    let rest = request.strip_prefix(REQUEST)?;
    let (kind, id) = match rest.split_once('/') {
        Some((kind, id)) => (kind, Some(id.to_owned())),
        None => (rest, None),
    };
    Some(WorkReminderOpened {
        kind: WorkReminderKind::parse(kind)?,
        id,
    })
}

/// Where a reminder click lands: the main window, told what to open.
pub(crate) struct ReminderOpener {
    app: AppHandle,
}

impl ReminderOpener {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl FollowUpOpener for ReminderOpener {
    fn follow_up_opened(&self, id: &str) {
        let Some(opened) = opened(id) else {
            log::warn!("A follow-through notification carried an id it never posted");
            return;
        };
        crate::show_main_window(&self.app);
        if opened.emit(&self.app).is_err() {
            log::warn!("A follow-through click could not reach the window");
        }
    }
}

/// One reminder still to show, under the key that remembers it was shown.
struct Reminder {
    key: String,
    item: WorkItem,
}

/// One notification: the id it is posted under and what it says.
#[derive(Debug, PartialEq)]
struct Notice {
    id: String,
    title: String,
    body: String,
}

fn sent(history: &Connection) -> Result<HashSet<String>> {
    let mut statement = history.prepare("SELECT key FROM planning_reminders")?;
    let keys = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(keys)
}

/// What is due to be said and has not been: open work I owe that is due
/// today, once its morning has come, and waits whose follow-up moment has
/// come.
fn pending(
    history: &Connection,
    sent: &HashSet<String>,
    now_ms: i64,
) -> Result<(Vec<Reminder>, Vec<Reminder>)> {
    let follow_ups = FollowUps::read(history)?;
    let (today_ms, tomorrow_ms) = items::today_bounds(now_ms)?;
    let mut due = Vec::new();
    let mut waits = Vec::new();
    for item in items::items(history, false)? {
        let (key, reminders) = match item.owner() {
            WorkOwner::Myself => match item.due_at_ms {
                Some(due_at)
                    if item.status.is_open()
                        && !items::snoozed(&item, now_ms)
                        && (today_ms..tomorrow_ms).contains(&due_at)
                        && morning_of(due_at)? <= now_ms =>
                {
                    (format!("due:{}:{due_at}", item.id), &mut due)
                }
                _ => continue,
            },
            WorkOwner::Person { .. } => match follow_ups.at(&item)? {
                Some(at) if at <= now_ms => (format!("follow:{}:{at}", item.id), &mut waits),
                _ => continue,
            },
            WorkOwner::Nobody => continue,
        };
        if !sent.contains(&key) {
            reminders.push(Reminder { key, item });
        }
    }
    Ok((due, waits))
}

/// The first few lines joined, then how many more there are.
fn named(lines: Vec<String>, separator: &str) -> String {
    let more = lines.len().saturating_sub(NAMED);
    let mut body = lines
        .into_iter()
        .take(NAMED)
        .collect::<Vec<_>>()
        .join(separator);
    if more > 0 {
        let _ = write!(body, "{separator}and {more} more");
    }
    body
}

fn due_notice(due: &[Reminder]) -> Option<Notice> {
    match due {
        [] => None,
        [one] => Some(Notice {
            id: request_id(WorkReminderKind::Due, Some(&one.item.id)),
            title: one.item.title.clone(),
            body: "Due today".to_owned(),
        }),
        many => Some(Notice {
            id: request_id(WorkReminderKind::Due, None),
            title: "Due today".to_owned(),
            body: named(
                many.iter().map(|due| due.item.title.clone()).collect(),
                " · ",
            ),
        }),
    }
}

fn follow_notice(waits: &[Reminder], people: &People) -> Option<Notice> {
    let name = |item: &WorkItem| {
        item.owner_person_id
            .as_deref()
            .and_then(|id| people.name(id))
    };
    match waits {
        [] => None,
        [one] => Some(Notice {
            id: request_id(WorkReminderKind::FollowUp, Some(&one.item.id)),
            title: name(&one.item).map_or_else(
                || "Time to follow up".to_owned(),
                |name| format!("Follow up with {name}"),
            ),
            body: one.item.title.clone(),
        }),
        many => Some(Notice {
            id: request_id(WorkReminderKind::FollowUp, None),
            title: "Time to follow up".to_owned(),
            body: named(
                many.iter()
                    .map(|wait| match name(&wait.item) {
                        Some(name) => format!("{name}: {}", wait.item.title),
                        None => wait.item.title.clone(),
                    })
                    .collect(),
                " · ",
            ),
        }),
    }
}

/// Remembers reminders as shown, and forgets ones shown long ago.
fn record(history: &mut Connection, keys: &[String], now_ms: i64) -> Result<()> {
    let tx = history.transaction()?;
    for key in keys {
        tx.execute(
            "INSERT OR IGNORE INTO planning_reminders (key, sent_at_ms) VALUES (?1, ?2)",
            params![key, now_ms],
        )?;
    }
    tx.execute(
        "DELETE FROM planning_reminders WHERE sent_at_ms < ?1",
        [now_ms - REMEMBERED_MS],
    )?;
    tx.commit()?;
    Ok(())
}

fn unreadable(error: StoreError) -> anyhow::Error {
    anyhow::anyhow!("conversations could not be read: {error:?}")
}

/// A saved point to raise with someone.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkTalkingPoint {
    pub id: String,
    pub text: String,
    pub created_at_ms: i64,
}

/// What is open between me and one person, as item ids in the order to
/// raise them.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkBriefPerson {
    pub person_id: String,
    pub name: String,
    /// My open work from meetings they were in.
    pub you_owe: Vec<String>,
    /// Their open work.
    pub they_owe: Vec<String>,
    pub talking_points: Vec<WorkTalkingPoint>,
}

/// A decision, with where it was said when those words are still kept.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkBriefDecision {
    pub text: String,
    pub at_ms: Option<i64>,
}

/// The meeting before this one, and what it decided.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkBriefMeeting {
    pub meeting_id: String,
    pub title: String,
    pub at_ms: i64,
    pub decisions: Vec<WorkBriefDecision>,
}

/// One screen before talking to someone.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkBrief {
    /// The meeting's title, or the person's name.
    pub title: String,
    pub starts_at_ms: Option<i64>,
    pub people: Vec<WorkBriefPerson>,
    pub last_time: Option<WorkBriefMeeting>,
}

/// Who a brief is for.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkBriefSubject {
    #[serde(rename_all = "camelCase")]
    Event { event_key: String },
    #[serde(rename_all = "camelCase")]
    Person { person_id: String },
}

/// Every id a person is known by: their own and the ids of people merged
/// into them.
fn known_by(meetings: &Connection, person_id: &str) -> Result<HashSet<String>> {
    let mut statement = meetings.prepare_cached(
        "SELECT source_person_id FROM person_merge_redirects WHERE target_person_id = ?1",
    )?;
    let mut ids = statement
        .query_map([person_id], |row| row.get(0))?
        .collect::<rusqlite::Result<HashSet<String>>>()?;
    ids.insert(person_id.to_owned());
    Ok(ids)
}

/// The meetings any of these ids was confirmed in.
fn met_in(meetings: &Connection, ids: &HashSet<String>) -> Result<HashSet<String>> {
    let mut statement = meetings.prepare_cached(
        "SELECT meeting_id FROM meeting_person_links WHERE person_id = ?1 AND confidence = 'confirmed'",
    )?;
    let mut met = HashSet::new();
    for id in ids {
        for meeting in statement.query_map([id], |row| row.get::<_, String>(0))? {
            met.insert(meeting?);
        }
    }
    Ok(met)
}

/// The meeting a loop or meeting id names.
fn session_of(loop_or_meeting: &str) -> Option<String> {
    match Uuid::parse_str(loop_or_meeting) {
        Ok(meeting) => Some(meeting.to_string()),
        Err(_) => MeetingLoopId(loop_or_meeting.to_owned())
            .session_id()
            .map(|session| session.uuid().to_string()),
    }
}

/// The meetings each open item came from or cites, by item id.
fn heard_in(history: &Connection, open: &[WorkItem]) -> Result<HashMap<String, HashSet<String>>> {
    let mut heard = HashMap::<String, HashSet<String>>::new();
    for item in open {
        if let Some(meeting) = item.source_loop_id.as_deref().and_then(session_of) {
            heard.entry(item.id.clone()).or_default().insert(meeting);
        }
    }
    let mut statement = history.prepare(
        "SELECT target_id, source_id FROM planning_evidence_links
          WHERE target_kind = 'task' AND source_kind IN ('meeting', 'loop')",
    )?;
    for row in statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (item, source) = row?;
        if let Some(meeting) = session_of(&source) {
            heard.entry(item).or_default().insert(meeting);
        }
    }
    Ok(heard)
}

/// Open work first by due day, then priority, then number.
fn in_order(mut items: Vec<&WorkItem>) -> Vec<String> {
    items.sort_by_key(|item| {
        (
            item.due_at_ms.is_none(),
            item.due_at_ms,
            if item.priority == 0 {
                u8::MAX
            } else {
                item.priority
            },
            item.number,
        )
    });
    items.into_iter().map(|item| item.id.clone()).collect()
}

fn talking_points(history: &Connection, ids: &HashSet<String>) -> Result<Vec<WorkTalkingPoint>> {
    let mut statement = history.prepare_cached(
        "SELECT id, text, created_at_ms FROM planning_talking_points
          WHERE person_id = ?1 AND done_at_ms IS NULL",
    )?;
    let mut points = Vec::new();
    for id in ids {
        for point in statement.query_map([id], |row| {
            Ok(WorkTalkingPoint {
                id: row.get(0)?,
                text: row.get(1)?,
                created_at_ms: row.get(2)?,
            })
        })? {
            points.push(point?);
        }
    }
    points.sort_by(|a, b| (a.created_at_ms, &a.id).cmp(&(b.created_at_ms, &b.id)));
    Ok(points)
}

type Held = (String, String, i64);

fn held(row: &rusqlite::Row<'_>) -> rusqlite::Result<Held> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
}

/// The last finished meeting before `before_ms`: the series' own when the
/// event's series has met before, otherwise the last one any of these
/// people was in.
fn last_meeting(
    meetings: &Connection,
    series_key: Option<&str>,
    known: &[HashSet<String>],
    before_ms: i64,
) -> Result<Option<WorkBriefMeeting>> {
    let mut last = match series_key.map(str::trim).filter(|key| !key.is_empty()) {
        Some(series_key) => meetings
            .query_row(
                "SELECT m.id, m.title, COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) AS at
                   FROM meeting_calendar_facts f
                   JOIN meeting_sessions m ON m.id = f.session_id
                  WHERE json_extract(f.event_json, '$.seriesKey') = ?1
                    AND m.ended_at_utc_ms IS NOT NULL AND m.phase != 'deleting'
                    AND COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) < ?2
                  ORDER BY at DESC, m.id DESC LIMIT 1",
                params![series_key, before_ms],
                held,
            )
            .optional()?,
        None => None,
    };
    if last.is_none() {
        let mut statement = meetings.prepare_cached(
            "SELECT m.id, m.title, COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) AS at
               FROM meeting_person_links l
               JOIN meeting_sessions m ON m.id = l.meeting_id
              WHERE l.person_id = ?1 AND l.confidence = 'confirmed'
                AND m.ended_at_utc_ms IS NOT NULL AND m.phase != 'deleting'
                AND COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) < ?2
              ORDER BY at DESC, m.id DESC LIMIT 1",
        )?;
        for id in known.iter().flatten() {
            let found = statement
                .query_row(params![id, before_ms], held)
                .optional()?;
            if let Some(found) = found {
                if last
                    .as_ref()
                    .is_none_or(|kept: &Held| (found.2, &found.0) > (kept.2, &kept.0))
                {
                    last = Some(found);
                }
            }
        }
    }
    let Some((meeting_id, title, at_ms)) = last else {
        return Ok(None);
    };
    let session = MeetingSessionId::from_uuid(Uuid::parse_str(&meeting_id)?);
    let retained = sources::meeting_retained(meetings, &meeting_id)?;
    let mut decisions = Vec::new();
    if let Some((_, content)) = readable_artifacts_in(meetings, session).map_err(unreadable)? {
        for decision in content.decisions {
            let text = decision.text.trim();
            if text.is_empty() {
                continue;
            }
            let at_ms = match decision.citations.first() {
                Some(first)
                    if retained
                        && sources::words_retained(
                            meetings,
                            &meeting_id,
                            &decision.citations,
                            0,
                        )? =>
                {
                    i64::try_from(first.start_offset_ns / 1_000_000).ok()
                }
                _ => None,
            };
            decisions.push(WorkBriefDecision {
                text: text.to_owned(),
                at_ms,
            });
        }
    }
    Ok(Some(WorkBriefMeeting {
        meeting_id,
        title,
        at_ms,
        decisions,
    }))
}

/// Everything a brief shows. `person_ids` may name a merged person by an
/// old id; each person counts once, by the id they go by now.
fn brief(
    history: &Connection,
    meetings: &Connection,
    event: Option<&CalendarEventSummary>,
    person_ids: &[String],
    now_ms: i64,
) -> Result<WorkBrief> {
    let people = People::read(meetings)?;
    let mut current = Vec::<String>::new();
    for id in person_ids {
        if let Some(id) = sources::person(meetings, &store::uuid(id)?)? {
            if !current.contains(&id) {
                current.push(id);
            }
        }
    }
    let open = items::items(history, false)?
        .into_iter()
        .filter(|item| item.status.is_open())
        .collect::<Vec<_>>();
    let heard = heard_in(history, &open)?;
    let mut briefs = Vec::new();
    let mut known = Vec::new();
    for person_id in current {
        let Some(name) = people.name(&person_id) else {
            continue;
        };
        let ids = known_by(meetings, &person_id)?;
        let met = met_in(meetings, &ids)?;
        let they_owe = open
            .iter()
            .filter(|item| {
                item.owner_person_id
                    .as_ref()
                    .is_some_and(|owner| ids.contains(owner))
            })
            .collect();
        let you_owe = open
            .iter()
            .filter(|item| {
                item.owner_is_self
                    && heard
                        .get(&item.id)
                        .is_some_and(|meetings| !meetings.is_disjoint(&met))
            })
            .collect();
        briefs.push(WorkBriefPerson {
            name: name.to_owned(),
            you_owe: in_order(you_owe),
            they_owe: in_order(they_owe),
            talking_points: talking_points(history, &ids)?,
            person_id,
        });
        known.push(ids);
    }
    briefs.sort_by_cached_key(|person| person.name.to_lowercase());
    let last_time = last_meeting(
        meetings,
        event.map(|event| event.series_key.as_str()),
        &known,
        event.map_or(now_ms, |event| event.start_utc_ms),
    )?;
    let title = match event {
        Some(event) => event.title.trim().to_owned(),
        None => briefs
            .first()
            .map(|person| person.name.clone())
            .unwrap_or_default(),
    };
    Ok(WorkBrief {
        title,
        starts_at_ms: event.map(|event| event.start_utc_ms),
        people: briefs,
        last_time,
    })
}

fn clipped(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((end, _)) => format!("{}…", text[..end].trim_end()),
        None => text.to_owned(),
    }
}

/// What a brief's notification says: each person's open work with me, then
/// the first decision the last meeting made. None when nothing is open.
fn brief_notice(event: &CalendarEventSummary, brief: &WorkBrief, now_ms: i64) -> Option<Notice> {
    let lines = brief
        .people
        .iter()
        .filter_map(|person| {
            let mut parts = Vec::new();
            if !person.you_owe.is_empty() {
                parts.push(format!("you owe {}", person.you_owe.len()));
            }
            if !person.they_owe.is_empty() {
                parts.push(format!("they owe {}", person.they_owe.len()));
            }
            match person.talking_points.len() {
                0 => {}
                1 => parts.push("1 talking point".to_owned()),
                count => parts.push(format!("{count} talking points")),
            }
            (!parts.is_empty()).then(|| format!("{}: {}", person.name, parts.join(", ")))
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    let mut body = named(lines, "; ");
    if let Some(decision) = brief
        .last_time
        .as_ref()
        .and_then(|meeting| meeting.decisions.first())
    {
        let _ = write!(
            body,
            ". Decided last time: {}",
            clipped(decision.text.trim_end_matches('.'), DECISION_CHARS)
        );
    }
    body.push('.');
    let minutes = (event.start_utc_ms - now_ms + MINUTE_MS - 1) / MINUTE_MS;
    let title = match event.title.trim() {
        "" => "Meeting",
        title => title,
    };
    Some(Notice {
        id: request_id(WorkReminderKind::Brief, Some(&event.event_key)),
        title: format!("{title} in {minutes} min"),
        body,
    })
}

/// The people Sona knows among an event's other attendees.
fn attendees(store: &MeetingStore, event: &CalendarEventSummary) -> Result<Vec<String>> {
    let emails = follow_up::recipient_addresses(&event.attendees);
    let mut ids = store
        .person_ids_for_calendar_emails(&emails)
        .map_err(unreadable)?
        .into_values()
        .map(|person| person.uuid().to_string())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn declined(event: &CalendarEventSummary) -> bool {
    event
        .attendees
        .iter()
        .any(|attendee| attendee.is_self && attendee.status == ParticipationStatus::Declined)
}

/// The briefs to show now: meetings starting within the lead whose people
/// have open work with me, not shown yet.
fn upcoming_briefs(
    app: &AppHandle,
    store: &MeetingStore,
    history: &HistoryManager,
    sent: &HashSet<String>,
    now_ms: i64,
) -> Result<Vec<(String, Notice)>> {
    let settings = crate::settings::get_settings(app);
    if !settings.detection_calendar_enabled || !settings.meeting_prep.upcoming_alerts_enabled {
        return Ok(Vec::new());
    }
    let Some(runtime) = app.try_state::<Arc<DetectionRuntime>>() else {
        return Ok(Vec::new());
    };
    if runtime.calendar_access() != CalendarAccess::Authorized {
        return Ok(Vec::new());
    }
    let mut notices = Vec::new();
    for occurrence in runtime.calendar_events_between(now_ms, now_ms + BRIEF_LEAD_MS + MINUTE_MS) {
        let starts_ms = occurrence.summary.start_utc_ms;
        let key = format!("brief:{}", occurrence.summary.event_key);
        if starts_ms <= now_ms || starts_ms > now_ms + BRIEF_LEAD_MS || sent.contains(&key) {
            continue;
        }
        let Some(event) = runtime.calendar_event_by_key(&occurrence.summary.event_key) else {
            continue;
        };
        if declined(&event) {
            continue;
        }
        let people = attendees(store, &event)?;
        if people.is_empty() {
            continue;
        }
        let brief = in_both(store, history, |history, meetings| {
            brief(history, meetings, Some(&event), &people, now_ms)
        })?;
        if let Some(notice) = brief_notice(&event, &brief, now_ms) {
            notices.push((key, notice));
        }
    }
    Ok(notices)
}

#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
pub(super) fn probe_inputs(
    history: &Connection,
    meetings: &Connection,
    now_ms: i64,
) -> Result<serde_json::Value> {
    let (due, waits) = pending(history, &sent(history)?, now_ms)?;
    let notice = due_notice(&due).map(|notice| (notice.title, notice.body));
    let mut statement = meetings.prepare("SELECT id FROM persons")?;
    let person_ids = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(serde_json::json!({
        "morning_due": notice,
        "waiting_follow_ups": waits.len(),
        "brief": brief(history, meetings, None, &person_ids, now_ms)?,
    }))
}

/// One minute's look: shows what is due to be said, then remembers what
/// was shown. Nothing shown is remembered, so a reminder the notification
/// centre refused is tried again on the next tick.
fn tick(app: &AppHandle, prompts: &dyn PromptPresenter, now_ms: i64) -> Result<()> {
    let history = app.state::<Arc<HistoryManager>>();
    let store = app.state::<Arc<MeetingSessionManager>>().open_store();
    let (sent, (due, waits)) = history.with_connection(|conn| {
        let sent = sent(conn)?;
        let pending = pending(conn, &sent, now_ms)?;
        Ok((sent, pending))
    })?;
    let mut shown = Vec::new();
    let mut show = |notice: &Notice, keys: Vec<String>| {
        if prompts.present_follow_up(&notice.id, &notice.title, &notice.body) {
            shown.extend(keys);
        }
    };
    if let Some(notice) = due_notice(&due) {
        show(&notice, due.into_iter().map(|due| due.key).collect());
    }
    if let Some(store) = &store {
        if !waits.is_empty() {
            let people = store
                .with_planning_snapshot(|conn| {
                    People::read(conn).map_err(|_| StoreError::Unavailable)
                })
                .map_err(unreadable)?;
            if let Some(notice) = follow_notice(&waits, &people) {
                show(&notice, waits.into_iter().map(|wait| wait.key).collect());
            }
        }
        for (key, notice) in upcoming_briefs(app, store, &history, &sent, now_ms)? {
            show(&notice, vec![key]);
        }
    }
    if !shown.is_empty() {
        history.with_connection(|conn| record(conn, &shown, now_ms))?;
    }
    Ok(())
}

/// Starts the once-a-minute clock behind reminders and briefs, on its own
/// thread: every calendar and database read it makes blocks.
pub(crate) fn start(app: AppHandle, prompts: Arc<dyn PromptPresenter>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(TICK);
        if let Err(error) = tick(&app, prompts.as_ref(), now_ms()) {
            log::warn!("Follow-through reminders could not run: {error:#}");
        }
    });
}

#[tauri::command]
#[specta::specta]
pub async fn work_brief(
    runtime: State<'_, Arc<DetectionRuntime>>,
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    subject: WorkBriefSubject,
) -> Result<WorkBrief, String> {
    let store = meetings
        .store()
        .await
        .map_err(|_| super::super::conversations_closed())?;
    let (event, people) = match subject {
        WorkBriefSubject::Event { event_key } => {
            let runtime = Arc::clone(&runtime);
            if !crate::settings::get_settings(runtime.app_handle()).detection_calendar_enabled {
                return Err("Turn on Use my calendar in Meeting settings.".into());
            }
            if runtime.calendar_access() != CalendarAccess::Authorized {
                return Err("Allow Sona to read Calendar in System Settings.".into());
            }
            let event = tauri::async_runtime::spawn_blocking(move || {
                runtime.calendar_event_by_key(&event_key)
            })
            .await
            .map_err(|_| "The calendar could not be read.")?
            .ok_or("This event is no longer on your calendar.")?;
            let people = attendees(&store, &event).map_err(super::super::failure)?;
            (Some(event), people)
        }
        WorkBriefSubject::Person { person_id } => (None, vec![person_id]),
    };
    in_both(&store, &history_state, |history, meetings| {
        brief(history, meetings, event.as_ref(), &people, now_ms())
    })
    .map_err(super::super::failure)
}

#[tauri::command]
#[specta::specta]
pub async fn work_talking_point_add(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    operation_id: String,
    person_id: String,
    text: String,
) -> Result<WorkTalkingPoint, String> {
    let text = text.trim().to_owned();
    if text.is_empty() || text.chars().count() > 500 {
        return Err("A talking point must contain between 1 and 500 characters.".into());
    }
    let person_id = with_both(&history_state, &meetings, |_, meetings| {
        sources::require_person(meetings, &store::uuid(&person_id)?)
    })
    .await?;
    let now = now_ms();
    run_history(&app, |history| {
        store::mutate(
            history,
            &operation_id,
            "work_talking_point_add",
            &(&person_id, &text),
            now,
            |tx| {
                let point = WorkTalkingPoint {
                    id: Uuid::new_v4().to_string(),
                    text: text.clone(),
                    created_at_ms: now,
                };
                tx.execute(
                    "INSERT INTO planning_talking_points (id, person_id, text, created_at_ms)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![point.id, person_id, point.text, now],
                )?;
                Ok(Done {
                    value: point,
                    effects: Effects::default(),
                })
            },
        )
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_talking_point_done(
    app: AppHandle,
    operation_id: String,
    id: String,
    done: bool,
) -> Result<(), String> {
    let now = now_ms();
    run_history(&app, |history| {
        store::mutate(
            history,
            &operation_id,
            "work_talking_point_done",
            &(&id, done),
            now,
            |tx| {
                let changed = tx.execute(
                    "UPDATE planning_talking_points SET done_at_ms = ?2 WHERE id = ?1",
                    params![store::uuid(&id)?, done.then_some(now)],
                )?;
                if changed == 0 {
                    return Err(Refusal::NotFound.into());
                }
                Ok(Done {
                    value: (),
                    effects: Effects::default(),
                })
            },
        )
    })
}

/// Marks a wait as just asked about, so it comes back a week from now.
fn nudge(history: &mut Connection, id: &str, now_ms: i64) -> Result<Done<()>> {
    let item = items::item(history, &store::uuid(id)?)?;
    if !matches!(item.owner(), WorkOwner::Person { .. }) {
        return Err(Refusal::Invalid.into());
    }
    history.execute(
        "INSERT INTO planning_nudges (task_id, nudged_at_ms) VALUES (?1, ?2)
         ON CONFLICT(task_id) DO UPDATE SET nudged_at_ms = excluded.nudged_at_ms",
        params![item.id, now_ms],
    )?;
    Ok(Done {
        value: (),
        effects: Effects {
            items: vec![item.id],
            ..Effects::default()
        },
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn work_nudged(app: AppHandle, id: String) -> Result<(), String> {
    let now = now_ms();
    run_history(&app, |history| nudge(history, &id, now))
}

/// How a nudge reached a compose window. Nothing is ever sent.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkNudgeMail {
    /// Mail has a compose window open with the whole draft.
    Composed,
    /// A `mailto:` link for the window to open; a draft too long for it
    /// goes on the clipboard.
    Link {
        url: String,
        body: MeetingFollowUpMailBody,
    },
}

#[tauri::command]
#[specta::specta]
pub async fn work_nudge_mail(
    app: AppHandle,
    history_state: State<'_, Arc<HistoryManager>>,
    meetings: State<'_, Arc<MeetingSessionManager>>,
    id: String,
    subject: String,
    body: String,
    over_bound_note: String,
) -> Result<WorkNudgeMail, String> {
    if body.trim().is_empty() || body.chars().count() > 8000 {
        return Err("A mail draft must contain between 1 and 8,000 characters.".into());
    }
    let recipients = with_both(&history_state, &meetings, |history, meetings| {
        let item = items::item(history, &store::uuid(&id)?)?;
        let Some(person_id) = item.owner_person_id else {
            return Err(Refusal::Invalid.into());
        };
        let emails: Option<String> = meetings
            .query_row(
                "SELECT calendar_emails_json FROM persons
                  WHERE id = COALESCE(
                    (SELECT target_person_id FROM person_merge_redirects WHERE source_person_id = ?1),
                    ?1)",
                [&person_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(match emails {
            Some(emails) => serde_json::from_str::<Vec<String>>(&emails)?,
            None => Vec::new(),
        })
    })
    .await?;
    if crate::settings::get_settings(&app)
        .meeting_prep
        .mail_compose_enabled
    {
        let mail = mail_context::compose(&recipients, &subject, &body).await;
        return if mail.state == MailContextState::Ready {
            Ok(WorkNudgeMail::Composed)
        } else {
            Err(mail.status_text().to_owned())
        };
    }
    let fits = follow_up::body_fits(&body);
    Ok(WorkNudgeMail::Link {
        url: follow_up::mailto_url(
            &recipients,
            &subject,
            if fits { &body } else { &over_bound_note },
        ),
        body: if fits {
            MeetingFollowUpMailBody::Draft
        } else {
            MeetingFollowUpMailBody::Clipboard
        },
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::schema;
    use super::super::types::{WorkActor, WorkItemInput, WorkPatch};
    use super::super::{create, update, Change};
    use super::*;
    use crate::meeting::people_types::PersonId;
    use crate::meeting::store::workflow_core_tests;
    use chrono::NaiveDate;
    use tempfile::TempDir;

    /// `hour` o'clock on the local day `day` days after a Monday in June,
    /// weeks from any clock change.
    fn at(day: i64, hour: u32) -> i64 {
        // PANIC: a June day and a whole hour exist on every clock.
        let date = NaiveDate::from_ymd_opt(2026, 6, 15).expect("a calendar day")
            + chrono::Duration::days(day);
        Local
            .from_local_datetime(&date.and_hms_opt(hour, 0, 0).expect("an hour of the day"))
            .earliest()
            .expect("an hour this clock shows")
            .timestamp_millis()
    }

    /// An empty tracker beside a meeting store that knows Dana.
    struct Desk {
        _directory: TempDir,
        meetings: Arc<MeetingStore>,
        history: Connection,
        dana: PersonId,
    }

    fn desk() -> Desk {
        let (directory, meetings) = workflow_core_tests::store();
        let dana = workflow_core_tests::person(&meetings, "Dana Reyes", &["Dana"], &[]);
        // PANIC: a fixture that cannot open its database is a broken test.
        let history = Connection::open_in_memory().expect("an in-memory history database");
        // PANIC: as above, for the planner schema.
        schema::apply_all(&history).expect("the planner schema");
        Desk {
            _directory: directory,
            meetings,
            history,
            dana,
        }
    }

    impl Desk {
        fn dana(&self) -> WorkOwner {
            WorkOwner::Person {
                person_id: self.dana.uuid().to_string(),
            }
        }

        /// Files open work the way the new-task sheet does, or an agent
        /// does when `actor` is one.
        fn file(
            &mut self,
            actor: WorkActor,
            title: &str,
            owner: WorkOwner,
            due_at_ms: Option<i64>,
            at_ms: i64,
        ) -> WorkItem {
            let history = &mut self.history;
            let operation_id = Uuid::new_v4().to_string();
            let input = WorkItemInput {
                title: title.to_owned(),
                details: None,
                status: None,
                priority: None,
                project_id: None,
                milestone_id: None,
                owner: Some(owner),
                due_at_ms,
                label_ids: None,
                recurrence: None,
            };
            // PANIC: filing work for the fixture's own people cannot fail.
            self.meetings
                .with_planning_snapshot(|meetings| {
                    let change = Change {
                        operation_id: &operation_id,
                        actor,
                        at_ms,
                    };
                    Ok(create(history, Some(meetings), change, &input)
                        .expect("the task filed")
                        .value)
                })
                .expect("the meeting database")
        }

        fn task(
            &mut self,
            title: &str,
            owner: WorkOwner,
            due_at_ms: Option<i64>,
            at_ms: i64,
        ) -> WorkItem {
            self.file(WorkActor::Me, title, owner, due_at_ms, at_ms)
        }

        fn snooze(&mut self, item: &WorkItem, until_ms: i64, at_ms: i64) {
            let history = &mut self.history;
            let operation_id = Uuid::new_v4().to_string();
            let patch = WorkPatch {
                snoozed_until_ms: Some(Some(until_ms)),
                ..WorkPatch::default()
            };
            // PANIC: the patch carries the revision the task was filed at.
            self.meetings
                .with_planning_snapshot(|meetings| {
                    let change = Change {
                        operation_id: &operation_id,
                        actor: WorkActor::Me,
                        at_ms,
                    };
                    update(
                        history,
                        Some(meetings),
                        change,
                        &item.id,
                        item.revision,
                        &patch,
                    )
                    .expect("the task snoozed");
                    Ok(())
                })
                .expect("the meeting database");
        }

        /// Cites a meeting or a note on `item`.
        fn cite(&self, item: &WorkItem, kind: &str, source_id: &str) {
            // PANIC: a citation row for the fixture's own task cannot fail.
            self.history
                .execute(
                    "INSERT INTO planning_evidence_links
                        (id, target_kind, target_id, source_kind, source_id, revision, created_at_ms)
                     VALUES (?1, 'task', ?2, ?3, ?4, 1, 1)",
                    params![Uuid::new_v4().to_string(), item.id, kind, source_id],
                )
                .expect("the citation");
        }

        /// What one tick of the reminder loop shows at `now_ms`, by title,
        /// remembered as shown afterwards.
        fn said(&mut self, now_ms: i64) -> Vec<String> {
            // PANIC: the fixture's own reminders table is readable.
            let shown = sent(&self.history).expect("the reminders shown before");
            let (due, waits) = pending(&self.history, &shown, now_ms).expect("the reminders due");
            let reminders = due.into_iter().chain(waits).collect::<Vec<_>>();
            let keys = reminders
                .iter()
                .map(|reminder| reminder.key.clone())
                .collect::<Vec<_>>();
            record(&mut self.history, &keys, now_ms).expect("the reminders remembered");
            reminders
                .into_iter()
                .map(|reminder| reminder.item.title)
                .collect()
        }
    }

    #[test]
    fn work_due_today_is_said_once_on_its_morning_and_not_while_snoozed() {
        let mut desk = desk();
        desk.task(
            "Draft the launch email",
            WorkOwner::Myself,
            Some(at(1, 12)),
            at(0, 10),
        );
        desk.task(
            "Book the venue",
            WorkOwner::Myself,
            Some(at(2, 12)),
            at(0, 10),
        );
        let shortlist = desk.task(
            "Send the venue shortlist",
            WorkOwner::Myself,
            Some(at(1, 17)),
            at(0, 10),
        );
        desk.snooze(&shortlist, at(1, 15), at(0, 11));

        assert!(desk.said(at(1, 8)).is_empty());
        assert_eq!(desk.said(at(1, 9)), ["Draft the launch email"]);
        assert!(desk.said(at(1, 14)).is_empty());
        assert_eq!(desk.said(at(1, 15)), ["Send the venue shortlist"]);
    }

    #[test]
    fn a_wait_comes_back_on_its_due_morning_and_a_week_after_it_moved_or_was_nudged() {
        let mut desk = desk();
        let dana = desk.dana();
        let deck = desk.task("Send the pricing deck", dana.clone(), None, at(0, 10));
        desk.task("Review the launch post", dana, Some(at(3, 17)), at(0, 10));

        assert!(desk.said(at(3, 8)).is_empty());
        assert_eq!(desk.said(at(3, 9)), ["Review the launch post"]);
        assert!(desk.said(at(7, 9)).is_empty());
        assert_eq!(desk.said(at(7, 10)), ["Send the pricing deck"]);
        nudge(&mut desk.history, &deck.id, at(7, 11)).expect("the nudge");
        assert!(desk.said(at(14, 10)).is_empty());
        assert_eq!(desk.said(at(14, 11)), ["Send the pricing deck"]);
    }

    #[test]
    fn a_brief_counts_open_work_both_ways_and_is_silent_when_none_is_open() {
        let mut desk = desk();
        let priya = workflow_core_tests::person(&desk.meetings, "Priya Shah", &["Priya"], &[]);
        let launch = workflow_core_tests::meeting(&desk.meetings, "Launch sync", at(-3, 10));
        workflow_core_tests::link(&desk.meetings, launch, desk.dana, "manual", "confirmed");
        let dana = desk.dana();
        desk.task("Send the pricing deck", dana, None, at(-3, 11));
        let email = desk.task(
            "Draft the launch email",
            WorkOwner::Myself,
            Some(at(1, 12)),
            at(-3, 11),
        );
        desk.cite(&email, "meeting", &launch.uuid().to_string());
        desk.task(
            "Renew the launch domain",
            WorkOwner::Myself,
            None,
            at(-3, 11),
        );
        let event = |key: &str| CalendarEventSummary {
            event_key: key.to_owned(),
            series_key: key.to_owned(),
            title: "Launch sync".to_owned(),
            attendee_count: 3,
            start_utc_ms: at(0, 10),
            end_utc_ms: at(0, 11),
            attendees: Vec::new(),
            notes: None,
            calendar_name: None,
            url: None,
            location: None,
            place: None,
        };
        let now = at(0, 10) - 4 * MINUTE_MS;
        let notice = |event: &CalendarEventSummary, people: &[PersonId]| {
            let people = people
                .iter()
                .map(|person| person.uuid().to_string())
                .collect::<Vec<_>>();
            // PANIC: the fixture's own databases are readable.
            desk.meetings
                .with_planning_snapshot(|meetings| {
                    let brief = brief(&desk.history, meetings, Some(event), &people, now)
                        .expect("the brief");
                    Ok(brief_notice(event, &brief, now))
                })
                .expect("the meeting database")
        };

        assert_eq!(
            notice(&event("launch#1"), &[desk.dana, priya]),
            Some(Notice {
                id: request_id(WorkReminderKind::Brief, Some("launch#1")),
                title: "Launch sync in 4 min".to_owned(),
                body: "Dana Reyes: you owe 1, they owe 1.".to_owned(),
            })
        );
        assert_eq!(notice(&event("venue#1"), &[priya]), None);
    }

    #[test]
    fn each_item_counts_toward_one_triage_split() {
        let mut desk = desk();
        let launch = workflow_core_tests::meeting(&desk.meetings, "Launch sync", at(-1, 10))
            .uuid()
            .to_string();
        let note = Uuid::new_v4().to_string();
        let typed = desk.task(
            "Reply to Sam about the press kit",
            WorkOwner::Myself,
            None,
            at(0, 10),
        );
        let heard = desk.task(
            "Ask legal about the beta terms",
            WorkOwner::Myself,
            None,
            at(0, 10),
        );
        let noted = desk.task(
            "Book the launch photographer",
            WorkOwner::Myself,
            None,
            at(0, 10),
        );
        let made = desk.file(
            WorkActor::Agent,
            "Summarize the beta feedback",
            WorkOwner::Myself,
            None,
            at(0, 10),
        );
        desk.cite(&heard, "meeting", &launch);
        desk.cite(&noted, "meeting", &launch);
        desk.cite(&noted, "note", &note);
        desk.cite(&made, "note", &note);

        let sources = item_sources(&desk.history).expect("each item's source");
        assert_eq!(
            [&typed, &heard, &noted, &made].map(|item| sources.get(&item.id).copied()),
            [
                Some(WorkSource::Person),
                Some(WorkSource::Meeting),
                Some(WorkSource::Dictation),
                Some(WorkSource::Agent),
            ]
        );
    }
}
