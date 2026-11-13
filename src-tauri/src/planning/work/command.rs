//! A sentence that changes many tasks at once, typed or said: "push
//! everything not urgent to Monday", "move Dana's tasks to next week",
//! "mark everything in triage for Launch done". It is read by a fixed
//! grammar, a verb, the tasks it names and what they become, into one patch
//! and the tasks it touches, so it can be shown before it is applied and
//! applied as one change that undoes as one.

use super::parse::{parse, person_named, Lexicon, Said, WorkField};
use super::types::{
    WorkDirection, WorkDue, WorkItem, WorkItemRow, WorkOwner, WorkPatch, WorkQuery, WorkStatus,
};
use super::{list, with_meetings};
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use anyhow::Result as StorageResult;
use chrono::{DateTime, Local, TimeZone};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkCommand {
    pub said: String,
    /// What happens to each task: "Move to Monday, Nov 9".
    pub summary: String,
    /// Which tasks, a phrase each: "Yours", "Not urgent", "Due before then".
    pub filters: Vec<String>,
    pub change: WorkCommandChange,
    /// The tasks it changes, in the tracker's order.
    pub items: Vec<WorkItemRow>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkCommandChange {
    Update { patch: WorkPatch },
    Archive,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Verb {
    /// Later: only work already due before the new day moves.
    Push,
    Move,
    Assign,
    Set,
    Status(WorkStatus),
    Snooze,
    Archive,
}

const VERBS: &[(&str, Verb)] = &[
    ("push", Verb::Push),
    ("postpone", Verb::Push),
    ("defer", Verb::Push),
    ("delay", Verb::Push),
    ("bump", Verb::Push),
    ("punt", Verb::Push),
    ("move", Verb::Move),
    ("reschedule", Verb::Move),
    ("schedule", Verb::Move),
    ("shift", Verb::Move),
    ("assign", Verb::Assign),
    ("reassign", Verb::Assign),
    ("give", Verb::Assign),
    ("hand", Verb::Assign),
    ("mark", Verb::Set),
    ("set", Verb::Set),
    ("make", Verb::Set),
    ("complete", Verb::Status(WorkStatus::Done)),
    ("finish", Verb::Status(WorkStatus::Done)),
    ("close", Verb::Status(WorkStatus::Done)),
    ("cancel", Verb::Status(WorkStatus::Canceled)),
    ("drop", Verb::Status(WorkStatus::Canceled)),
    ("start", Verb::Status(WorkStatus::InProgress)),
    ("snooze", Verb::Snooze),
    ("hide", Verb::Snooze),
    ("archive", Verb::Archive),
];

const LEAD_INS: &[&str] = &[
    "please", "sona", "hey", "ok", "okay", "can", "could", "you", "go", "ahead", "and",
];

/// Words that join the tasks to what they become.
const JOINS: &[&str] = &["to", "until", "till", "into", "onto", "as", "for"];

/// Words a selection says that narrow nothing.
const FILLER: &[&str] = &[
    "everything",
    "all",
    "of",
    "the",
    "tasks",
    "task",
    "todos",
    "stuff",
    "things",
    "items",
    "work",
    "that",
    "which",
    "is",
    "are",
    "i",
    "i've",
    "ive",
    "got",
    "have",
    "open",
    "remaining",
    "rest",
    "left",
    "else",
    "every",
    "one",
    "on",
    "plate",
    "please",
    "due",
    "now",
];

const MINE: &[&[&str]] = &[
    &["i", "owe"],
    &["assigned", "to", "me"],
    &["on", "my", "plate"],
    &["my"],
    &["mine"],
];

const NOT_URGENT: &[&[&str]] = &[
    &["that's", "not", "urgent"],
    &["that", "isn't", "urgent"],
    &["that", "aren't", "urgent"],
    &["that's", "not", "on", "fire"],
    &["not", "on", "fire"],
    &["isn't", "on", "fire"],
    &["not", "urgent"],
    &["isn't", "urgent"],
    &["aren't", "urgent"],
    &["non", "urgent"],
    &["non-urgent"],
    &["nonurgent"],
];

const PRIORITIES: &[(&[&str], u8)] = &[
    (&["urgent"], 1),
    (&["high", "priority"], 2),
    (&["high-priority"], 2),
    (&["medium", "priority"], 3),
    (&["low", "priority"], 4),
    (&["low-priority"], 4),
    (&["no", "priority"], 0),
];

const DUE: &[(&[&str], WorkDue)] = &[
    (&["overdue"], WorkDue::Overdue),
    (&["late"], WorkDue::Overdue),
    (&["due", "today"], WorkDue::Today),
    (&["today's"], WorkDue::Today),
    (&["today"], WorkDue::Today),
    (&["due", "this", "week"], WorkDue::ThisWeek),
    (&["this", "week's"], WorkDue::ThisWeek),
    (&["this", "week"], WorkDue::ThisWeek),
    (&["with", "no", "date"], WorkDue::NoDate),
    (&["without", "a", "date"], WorkDue::NoDate),
    (&["no", "due", "date"], WorkDue::NoDate),
    (&["undated"], WorkDue::NoDate),
];

const STATUSES: &[(&[&str], WorkStatus)] = &[
    (&["in", "triage"], WorkStatus::Triage),
    (&["triage"], WorkStatus::Triage),
    (&["in", "progress"], WorkStatus::InProgress),
    (&["in", "the", "backlog"], WorkStatus::Backlog),
    (&["in", "backlog"], WorkStatus::Backlog),
    (&["backlog"], WorkStatus::Backlog),
];

const BECOMES: &[(&[&str], WorkStatus)] = &[
    (&["done"], WorkStatus::Done),
    (&["complete"], WorkStatus::Done),
    (&["completed"], WorkStatus::Done),
    (&["finished"], WorkStatus::Done),
    (&["canceled"], WorkStatus::Canceled),
    (&["cancelled"], WorkStatus::Canceled),
    (&["in", "progress"], WorkStatus::InProgress),
    (&["started"], WorkStatus::InProgress),
    (&["todo"], WorkStatus::NotStarted),
    (&["to", "do"], WorkStatus::NotStarted),
    (&["not", "started"], WorkStatus::NotStarted),
    (&["backlog"], WorkStatus::Backlog),
];

const WAITING: &[&[&str]] = &[
    &["i'm", "waiting", "on"],
    &["im", "waiting", "on"],
    &["waiting", "on", "others"],
    &["waiting", "on"],
    &["waiting"],
];

const SELECTED: &[&str] = &["selected", "these", "them", "this", "those", "it"];

#[derive(Clone, Debug, PartialEq)]
struct Word<'t> {
    key: String,
    raw: &'t str,
    start: usize,
}

fn words(text: &str) -> Vec<Word<'_>> {
    let mut start = 0;
    text.split_inclusive(char::is_whitespace)
        .map(|piece| {
            let word = Word {
                key: piece
                    .trim_matches(|character: char| {
                        !character.is_alphanumeric() && !matches!(character, '\'' | '#' | '+' | '-')
                    })
                    .trim_end_matches(['\'', '-'])
                    .replace('\u{2019}', "'")
                    .to_lowercase(),
                raw: piece.trim_end_matches(char::is_whitespace),
                start,
            };
            start += piece.len();
            word
        })
        .filter(|word| !word.key.is_empty())
        .collect()
}

fn starts(words: &[Word<'_>], at: usize, phrase: &[&str]) -> bool {
    phrase.iter().enumerate().all(|(offset, wanted)| {
        words
            .get(at + offset)
            .is_some_and(|word| word.key == *wanted)
    })
}

/// What a command was read as, before any task is looked at.
#[derive(Debug, PartialEq)]
pub(crate) struct Reading {
    summary: String,
    filters: Vec<String>,
    change: WorkCommandChange,
    query: WorkQuery,
    /// Only work due before this moment, epoch ms.
    due_before_ms: Option<i64>,
    selected: bool,
}

/// Reads `text` as a command, or `None` when it is not one: a sentence
/// that starts with no verb this knows, names nothing it can change, or
/// holds a word this cannot place.
pub(crate) fn read<Tz: TimeZone>(
    text: &str,
    lexicon: &Lexicon,
    now: &DateTime<Tz>,
) -> Option<Reading> {
    let all = words(text.trim().trim_end_matches(['.', '!', '?']));
    let start = all
        .iter()
        .position(|word| !LEAD_INS.contains(&word.key.as_str()))?;
    let verb = VERBS
        .iter()
        .find(|(said, _)| all[start].key == *said)
        .map(|(_, verb)| *verb)?;
    let rest = &all[start + 1..];
    let (selection, change, summary, due_at) = target(verb, rest, text, lexicon, now)?;
    let mut reading = select(selection, lexicon)?;
    if matches!(verb, Verb::Push) {
        reading.due_before_ms = due_at;
        reading.filters.push("Due before then".to_owned());
    }
    reading.change = change;
    reading.summary = summary;
    Some(reading)
}

type Target<'w, 't> = (&'w [Word<'t>], WorkCommandChange, String, Option<i64>);

/// Splits off what the tasks become, from the last join word that leaves
/// a reading, and reads it.
fn target<'w, 't, Tz: TimeZone>(
    verb: Verb,
    rest: &'w [Word<'t>],
    text: &str,
    lexicon: &Lexicon,
    now: &DateTime<Tz>,
) -> Option<Target<'w, 't>> {
    let update = |patch| WorkCommandChange::Update { patch };
    match verb {
        Verb::Archive => Some((rest, WorkCommandChange::Archive, "Archive".to_owned(), None)),
        Verb::Status(status) => Some((
            rest,
            update(status_patch(status)),
            status_summary(status),
            None,
        )),
        Verb::Push | Verb::Move | Verb::Snooze | Verb::Assign => {
            (0..rest.len()).rev().find_map(|join| {
                if !JOINS.contains(&rest[join].key.as_str()) {
                    return None;
                }
                let said = joined(text, &rest[join + 1..])?;
                let selection = &rest[..join];
                match verb {
                    Verb::Assign => {
                        let (owner, name) = owner_named(&said, lexicon)?;
                        let patch = WorkPatch {
                            owner: Some(owner),
                            ..WorkPatch::default()
                        };
                        Some((selection, update(patch), format!("Assign to {name}"), None))
                    }
                    _ => match whole_field(&said, lexicon, now)? {
                        WorkField::Due { at, .. } => {
                            let day = day_word(at);
                            let (patch, summary) = if verb == Verb::Snooze {
                                let morning = day_start(at)?;
                                (
                                    WorkPatch {
                                        snoozed_until_ms: Some(Some(morning)),
                                        ..WorkPatch::default()
                                    },
                                    format!("Snooze until {day}"),
                                )
                            } else {
                                (
                                    WorkPatch {
                                        due_at_ms: Some(Some(at)),
                                        ..WorkPatch::default()
                                    },
                                    format!("Move to {day}"),
                                )
                            };
                            Some((selection, update(patch), summary, Some(day_start(at)?)))
                        }
                        WorkField::Project { id, title } if verb != Verb::Snooze => Some((
                            selection,
                            update(WorkPatch {
                                project_id: Some(Some(id)),
                                ..WorkPatch::default()
                            }),
                            format!("Move to {title}"),
                            None,
                        )),
                        _ => None,
                    },
                }
            })
        }
        Verb::Set => (0..rest.len()).rev().find_map(|cut| {
            let tail: Vec<&str> = rest[cut..].iter().map(|word| word.key.as_str()).collect();
            let mut end = cut;
            while end > 0 && matches!(rest[end - 1].key.as_str(), "as" | "to" | "be") {
                end -= 1;
            }
            let selection = &rest[..end];
            if let Some((_, status)) = BECOMES
                .iter()
                .find(|(phrase, _)| *phrase == tail.as_slice())
            {
                return Some((
                    selection,
                    update(status_patch(*status)),
                    status_summary(*status),
                    None,
                ));
            }
            let tail = tail
                .iter()
                .filter(|key| **key != "priority")
                .copied()
                .collect::<Vec<_>>();
            let level = match tail.as_slice() {
                ["urgent"] | ["p1"] | ["top"] => 1,
                ["high"] | ["p2"] => 2,
                ["medium"] | ["normal"] | ["p3"] => 3,
                ["low"] | ["p4"] => 4,
                ["no"] | ["none"] => 0,
                _ => return None,
            };
            let patch = WorkPatch {
                priority: Some(level),
                ..WorkPatch::default()
            };
            Some((
                selection,
                update(patch),
                format!("Set priority to {}", priority_name(level)),
                None,
            ))
        }),
    }
}

fn status_patch(status: WorkStatus) -> WorkPatch {
    WorkPatch {
        status: Some(status),
        ..WorkPatch::default()
    }
}

fn status_summary(status: WorkStatus) -> String {
    match status {
        WorkStatus::Done => "Mark done",
        WorkStatus::Canceled => "Cancel",
        WorkStatus::InProgress => "Start",
        WorkStatus::NotStarted => "Mark to do",
        WorkStatus::Backlog => "Move to backlog",
        WorkStatus::Triage => "Move to triage",
        WorkStatus::Duplicate => "Mark duplicate",
    }
    .to_owned()
}

fn priority_name(level: u8) -> &'static str {
    match level {
        1 => "Urgent",
        2 => "High",
        3 => "Medium",
        4 => "Low",
        _ => "None",
    }
}

/// The original words from the first of `words` to the last.
fn joined(text: &str, words: &[Word<'_>]) -> Option<String> {
    let first = words.first()?;
    let last = words.last()?;
    let start = first.start;
    let end = last.start + last.raw.len();
    Some(
        text.get(start..end)?
            .trim_end_matches(['.', '!', '?'])
            .to_owned(),
    )
}

/// The one field `said` names with no words left over.
fn whole_field<Tz: TimeZone>(
    said: &str,
    lexicon: &Lexicon,
    now: &DateTime<Tz>,
) -> Option<WorkField> {
    let read = parse(said, Said::Spoken, &[], lexicon, now);
    let leftover = read.title.trim().to_lowercase();
    if !leftover.is_empty() && !matches!(leftover.as_str(), "the" | "a" | "an") {
        return None;
    }
    match read.tokens.as_slice() {
        [token] => Some(token.field.clone()),
        _ => None,
    }
}

fn owner_named(said: &str, lexicon: &Lexicon) -> Option<(WorkOwner, String)> {
    match said.trim().to_lowercase().as_str() {
        "me" | "myself" => Some((WorkOwner::Myself, "you".to_owned())),
        "nobody" | "no one" | "noone" => Some((WorkOwner::Nobody, "nobody".to_owned())),
        name => person_named(&lexicon.people, name, true).map(|person| {
            (
                WorkOwner::Person {
                    person_id: person.id.clone(),
                },
                person.name.clone(),
            )
        }),
    }
}

fn local_day(at: i64) -> Option<chrono::NaiveDate> {
    Local
        .timestamp_millis_opt(at)
        .single()
        .map(|at| at.date_naive())
}

fn day_start(at: i64) -> Option<i64> {
    let day = local_day(at)?;
    Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
        .earliest()
        .map(|start| start.timestamp_millis())
}

fn day_word(at: i64) -> String {
    local_day(at).map_or_else(String::new, |day| day.format("%A, %b %-d").to_string())
}

/// Reads the words that pick the tasks.
fn select(words: &[Word<'_>], lexicon: &Lexicon) -> Option<Reading> {
    let mut query = WorkQuery::default();
    let mut filters = Vec::new();
    let mut mine = false;
    let mut said_anything = false;
    let mut selected = false;
    let mut at = 0;
    'words: while at < words.len() {
        let key = words[at].key.as_str();
        said_anything = true;
        for phrase in NOT_URGENT {
            if starts(words, at, phrase) {
                query.priorities = Some(vec![0, 2, 3, 4]);
                filters.push("Not urgent".to_owned());
                at += phrase.len();
                continue 'words;
            }
        }
        for phrase in WAITING {
            if starts(words, at, phrase) {
                query.direction = Some(WorkDirection::WaitingOn);
                filters.push("Waiting on others".to_owned());
                at += phrase.len();
                continue 'words;
            }
        }
        for phrase in MINE {
            if starts(words, at, phrase) {
                mine = true;
                at += phrase.len();
                continue 'words;
            }
        }
        for (phrase, level) in PRIORITIES {
            if starts(words, at, phrase) {
                query.priorities = Some(vec![*level]);
                filters.push(priority_name(*level).to_owned());
                at += phrase.len();
                continue 'words;
            }
        }
        for (phrase, due) in DUE {
            if starts(words, at, phrase) {
                query.due.get_or_insert_with(Vec::new).push(*due);
                filters.push(
                    match due {
                        WorkDue::Overdue => "Overdue",
                        WorkDue::Today => "Due today",
                        WorkDue::ThisWeek => "Due this week",
                        WorkDue::NoDate | WorkDue::NextDays { .. } => "No date",
                    }
                    .to_owned(),
                );
                at += phrase.len();
                continue 'words;
            }
        }
        for (phrase, status) in STATUSES {
            if starts(words, at, phrase) {
                query.statuses = Some(vec![*status]);
                filters.push(status_name(*status).to_owned());
                at += phrase.len();
                continue 'words;
            }
        }
        if SELECTED.contains(&key) {
            selected = true;
            at += 1;
            continue;
        }
        if let Some(name) = key.strip_prefix('#') {
            let label = lexicon
                .labels
                .iter()
                .find(|label| label.name.to_lowercase() == name)?;
            query.label_ids = Some(vec![label.id.clone()]);
            filters.push(format!("#{}", label.name));
            at += 1;
            continue;
        }
        if matches!(key, "tagged" | "labeled" | "labelled") {
            let name = words.get(at + 1)?.key.trim_start_matches('#');
            let label = lexicon
                .labels
                .iter()
                .find(|label| label.name.to_lowercase() == name)?;
            query.label_ids = Some(vec![label.id.clone()]);
            filters.push(format!("#{}", label.name));
            at += 2;
            continue;
        }
        if let Some((length, id, title)) = project_at(words, at, lexicon) {
            query.project_ids = Some(vec![id]);
            filters.push(title);
            at += length;
            continue;
        }
        if let Some((length, person_id, name)) = person_at(words, at, lexicon) {
            query.owner_person_ids = Some(vec![person_id]);
            filters.push(name);
            at += length;
            continue;
        }
        if FILLER.contains(&key)
            || matches!(
                key,
                "in" | "for" | "from" | "with" | "assigned" | "owned" | "by"
            )
        {
            at += 1;
            continue;
        }
        return None;
    }
    if !said_anything {
        return None;
    }
    let narrowed = query.owner_person_ids.is_some()
        || query.project_ids.is_some()
        || query.label_ids.is_some()
        || query.direction.is_some()
        || query.statuses.is_some()
        || selected;
    if mine || !narrowed {
        query.direction = Some(WorkDirection::Mine);
        filters.insert(0, "Yours".to_owned());
    }
    if query.statuses.is_none() {
        query.statuses = Some(vec![
            WorkStatus::Backlog,
            WorkStatus::NotStarted,
            WorkStatus::InProgress,
        ]);
    }
    Some(Reading {
        summary: String::new(),
        filters,
        change: WorkCommandChange::Archive,
        query,
        due_before_ms: None,
        selected,
    })
}

fn status_name(status: WorkStatus) -> &'static str {
    match status {
        WorkStatus::Triage => "In triage",
        WorkStatus::Backlog => "In backlog",
        WorkStatus::InProgress => "In progress",
        WorkStatus::NotStarted => "To do",
        WorkStatus::Done => "Done",
        WorkStatus::Canceled => "Canceled",
        WorkStatus::Duplicate => "Duplicate",
    }
}

/// A project named at `at`, written whole, with or without a leading `+`.
fn project_at(words: &[Word<'_>], at: usize, lexicon: &Lexicon) -> Option<(usize, String, String)> {
    lexicon
        .projects
        .iter()
        .filter_map(|project| {
            let title: Vec<String> = project
                .title
                .split_whitespace()
                .map(|word| word.to_lowercase())
                .collect();
            let length = title.len();
            let fits = title.iter().enumerate().all(|(offset, wanted)| {
                words.get(at + offset).is_some_and(|word| {
                    let key = if offset == 0 {
                        word.key.trim_start_matches('+')
                    } else {
                        word.key.as_str()
                    };
                    key == wanted || key.strip_suffix("'s") == Some(wanted)
                })
            });
            (length > 0 && fits).then(|| (length, project.id.clone(), project.title.clone()))
        })
        .max_by_key(|(length, ..)| *length)
}

/// A person named at `at`: "Dana's", "Dana Lee's", or "Dana" before
/// "owes".
fn person_at(words: &[Word<'_>], at: usize, lexicon: &Lexicon) -> Option<(usize, String, String)> {
    let first = &words[at].key;
    let base = |key: &str| {
        key.strip_suffix("'s")
            .or_else(|| key.strip_suffix('\''))
            .unwrap_or(key)
            .to_owned()
    };
    if let Some(second) = words.get(at + 1) {
        let two = format!("{} {}", first, base(&second.key));
        if let Some(person) = person_named(&lexicon.people, &two, false) {
            return Some((2, person.id.clone(), person.name.clone()));
        }
    }
    let person = person_named(&lexicon.people, &base(first), false)?;
    Some((1, person.id.clone(), person.name.clone()))
}

fn preview(
    history: &Connection,
    meetings: Option<&Connection>,
    user_name: Option<&str>,
    said: &str,
    reading: Reading,
    selection: &[String],
    now_ms: i64,
) -> StorageResult<WorkCommand> {
    let listed = list(history, meetings, user_name, &reading.query, now_ms)?;
    let items = listed
        .items
        .into_iter()
        .filter(|row| !reading.selected || selection.contains(&row.item.id))
        .filter(|row| {
            reading
                .due_before_ms
                .is_none_or(|before| row.item.due_at_ms.is_some_and(|due| due < before))
        })
        .filter(|row| !already(&row.item, &reading.change))
        .collect();
    Ok(WorkCommand {
        said: said.to_owned(),
        summary: reading.summary,
        filters: reading.filters,
        change: reading.change,
        items,
    })
}

/// Whether `item` already is what the change would make it.
fn already(item: &WorkItem, change: &WorkCommandChange) -> bool {
    let WorkCommandChange::Update { patch } = change else {
        return item.archived_at_ms.is_some();
    };
    patch.status.is_some_and(|status| status == item.status)
        || patch
            .priority
            .is_some_and(|priority| priority == item.priority)
        || patch.owner.as_ref().is_some_and(|owner| {
            *owner == WorkOwner::of(item.owner_is_self, item.owner_person_id.as_deref())
        })
        || patch
            .project_id
            .as_ref()
            .is_some_and(|project| *project == item.project_id)
        || patch
            .due_at_ms
            .is_some_and(|due| due.and_then(local_day) == item.due_at_ms.and_then(local_day))
}

/// Reads `text` as a command over the tracker, and the tasks it would
/// change; `None` when it is not a command. `selection` is what "these"
/// and "selected" name.
#[tauri::command]
#[specta::specta]
pub async fn work_command(
    history_state: tauri::State<'_, Arc<HistoryManager>>,
    meetings: tauri::State<'_, Arc<MeetingSessionManager>>,
    text: String,
    selection: Vec<String>,
) -> Result<Option<WorkCommand>, String> {
    command_preview(&history_state, &meetings, &text, &selection).await
}

pub(crate) async fn command_preview(
    history: &HistoryManager,
    meetings: &MeetingSessionManager,
    text: &str,
    selection: &[String],
) -> Result<Option<WorkCommand>, String> {
    let user_name = meetings.user_name();
    let now = Local::now();
    with_meetings(history, meetings, |history, meeting_conn| {
        let lexicon = Lexicon::read(history, meeting_conn)?;
        read(text, &lexicon, &now)
            .map(|reading| {
                preview(
                    history,
                    meeting_conn,
                    user_name.as_deref(),
                    text,
                    reading,
                    selection,
                    now.timestamp_millis(),
                )
            })
            .transpose()
    })
    .await
}

/// Whether a spoken passage is a command rather than tasks.
pub(crate) fn is_command(passage: &str, lexicon: &Lexicon) -> bool {
    read(passage, lexicon, &Local::now()).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::work::types::WorkPersonChoice;
    use chrono::NaiveDate;

    /// Wednesday 11 November 2026, ten in the morning, here.
    fn wednesday() -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 11, 11, 10, 0, 0)
            .earliest()
            .expect("the test's clock")
    }

    fn lexicon() -> Lexicon {
        Lexicon {
            labels: Vec::new(),
            projects: Vec::new(),
            people: vec![WorkPersonChoice {
                id: "dana".to_owned(),
                name: "Dana Reyes".to_owned(),
                aliases: Vec::new(),
            }],
        }
    }

    fn next_monday() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 16).expect("a day")
    }

    fn due_day(reading: &Reading) -> Option<NaiveDate> {
        match &reading.change {
            WorkCommandChange::Update { patch } => patch.due_at_ms.flatten().and_then(local_day),
            WorkCommandChange::Archive => None,
        }
    }

    #[test]
    fn push_not_urgent_moves_my_other_work_due_before_monday_to_monday() {
        let reading = read(
            "push everything not urgent to monday",
            &lexicon(),
            &wednesday(),
        )
        .expect("a command");

        assert_eq!(due_day(&reading), Some(next_monday()));
        assert_eq!(
            reading.due_before_ms.and_then(local_day),
            Some(next_monday())
        );
        assert_eq!(
            reading.due_before_ms,
            reading.due_before_ms.and_then(day_start)
        );
        assert_eq!(reading.query.priorities, Some(vec![0, 2, 3, 4]));
        assert_eq!(reading.query.direction, Some(WorkDirection::Mine));
    }

    #[test]
    fn move_a_persons_tasks_picks_what_they_owe_whatever_its_date() {
        let reading =
            read("move Dana's tasks to next week", &lexicon(), &wednesday()).expect("a command");

        assert_eq!(due_day(&reading), Some(next_monday()));
        assert_eq!(
            reading.query.owner_person_ids,
            Some(vec!["dana".to_owned()])
        );
        assert_eq!(reading.query.direction, None);
        assert_eq!(reading.due_before_ms, None);
    }

    #[test]
    fn a_task_that_starts_with_a_command_verb_is_not_a_command() {
        let lexicon = lexicon();
        let tasks = [
            "Push the release notes to the blog",
            "Push the investor update to Friday",
            "Move the dentist appointment to next week",
            "Move the couch to the garage",
            "Mark the launch post as reviewed",
            "Send Dana the pricing deck Friday",
        ];
        let read_as_commands: Vec<&str> = tasks
            .into_iter()
            .filter(|said| read(said, &lexicon, &wednesday()).is_some())
            .collect();

        assert_eq!(read_as_commands, Vec::<&str>::new());
    }
}
