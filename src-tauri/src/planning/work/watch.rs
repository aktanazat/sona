//! Topics to listen for. When a processed meeting or a dictation says one,
//! the line that said it is kept and one quiet notification offers to play
//! it or keep it as a task.

use super::ask::{said_line, WorkAnswerLine, WorkAnswerPlay};
use super::capture::{take_lines_as, WorkCaptureSource, WorkTakenLine};
use super::sync::People;
use super::types::{WorkActor, WorkItem};
use super::{now_ms, with_meetings};
use crate::managers::history::HistoryManager;
use crate::meeting::detection::notify::{PromptPresenter, WatchGesture, WatchOpener};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::{MeetingStore, SaidWords, Within};
use crate::meeting::types::MeetingSessionId;
use crate::planning::sources;
use crate::planning::store::Refusal;
use anyhow::Result as StorageResult;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use uuid::Uuid;

const REQUEST: &str = "computer.sona.work.watch/";
const HITS_SHOWN: usize = 5;
const LINES_READ: usize = 8;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkWatch {
    pub id: String,
    pub phrase: String,
    /// Only lines this person says.
    pub person_id: Option<String>,
    pub person_name: Option<String>,
    pub created_at_ms: i64,
    /// The latest times it came up, newest first.
    pub hits: Vec<WorkWatchHit>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkWatchHit {
    pub id: String,
    pub watch_id: String,
    pub phrase: String,
    pub heard_at_ms: i64,
    /// The task kept from it.
    pub task_id: Option<String>,
    pub line: WorkAnswerLine,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkWatchGesture {
    Open,
    Play,
}

/// A watch notification asked to open or play its line.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
pub struct WorkWatchOpened {
    pub hit: WorkWatchHit,
    pub gesture: WorkWatchGesture,
}

/// The watches or what they heard changed.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
pub struct WorkWatchesChanged {}

/// The presenter watch notifications go through.
pub(crate) struct WatchNotices(pub Arc<dyn PromptPresenter>);

#[derive(Debug)]
struct Watch {
    id: String,
    phrase: String,
    person_id: Option<String>,
}

fn watches(history: &Connection) -> StorageResult<Vec<Watch>> {
    let mut statement = history.prepare(
        "SELECT id, phrase, person_id FROM planning_watches ORDER BY created_at_ms DESC, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Watch {
            id: row.get(0)?,
            phrase: row.get(1)?,
            person_id: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

const HIT_COLUMNS: &str = "h.id, h.watch_id, w.phrase, h.heard_at_ms, h.task_id, h.source_kind, \
     h.source_id, h.title, h.quote, h.speaker, h.person_id, h.segment_id, h.at_ms, h.end_ms, \
     h.start_offset_ns, h.said_at_ms, h.link";

fn hit_row(row: &Row<'_>) -> rusqlite::Result<WorkWatchHit> {
    let id: String = row.get(0)?;
    let kind: String = row.get(5)?;
    let source_id: String = row.get(6)?;
    let quote: String = row.get(8)?;
    let segment_id: Option<String> = row.get(11)?;
    let at_ms: i64 = row.get(12)?;
    let start_offset_ns: Option<i64> = row.get(14)?;
    let source = if kind == "meeting" {
        WorkCaptureSource::Meeting {
            session_id: source_id.clone(),
            at_ms,
            quote: quote.clone(),
        }
    } else {
        WorkCaptureSource::Dictation {
            history_id: source_id.parse().unwrap_or_default(),
            start_ms: at_ms,
            end_ms: row.get::<_, Option<i64>>(13)?.unwrap_or(at_ms),
            quote: quote.clone(),
        }
    };
    let play = segment_id
        .zip(start_offset_ns)
        .map(|(segment_id, offset)| WorkAnswerPlay {
            session_id: source_id,
            segment_id,
            start_offset_ns: u64::try_from(offset).unwrap_or_default(),
        });
    Ok(WorkWatchHit {
        watch_id: row.get(1)?,
        phrase: row.get(2)?,
        heard_at_ms: row.get(3)?,
        task_id: row.get(4)?,
        line: WorkAnswerLine {
            id: format!("watch:{id}"),
            quote,
            title: row.get(7)?,
            speaker: row.get(9)?,
            person_id: row.get(10)?,
            when_utc_ms: row.get(15)?,
            link: row.get(16)?,
            source,
            play,
        },
        id,
    })
}

/// Whether the words a hit kept may still be read: its meeting keeps its
/// transcript, or its dictation is still saved. Without the conversations
/// nothing says a meeting still keeps them.
fn still_said(
    history: &Connection,
    meetings: Option<&Connection>,
    hit: &WorkWatchHit,
) -> StorageResult<bool> {
    match &hit.line.source {
        WorkCaptureSource::Meeting { session_id, .. } => match meetings {
            Some(meetings) => sources::meeting_retained(meetings, session_id),
            None => Ok(false),
        },
        WorkCaptureSource::Dictation { history_id, .. } => Ok(history.query_row(
            "SELECT EXISTS(SELECT 1 FROM transcription_history WHERE id = ?1)",
            [history_id],
            |row| row.get(0),
        )?),
        WorkCaptureSource::Link { .. } | WorkCaptureSource::Loop { .. } => Ok(false),
    }
}

fn hit(
    history: &Connection,
    meetings: Option<&Connection>,
    hit_id: &str,
) -> StorageResult<Option<WorkWatchHit>> {
    let found = history
        .query_row(
            &format!(
                "SELECT {HIT_COLUMNS} FROM planning_watch_hits h \
                 JOIN planning_watches w ON w.id = h.watch_id WHERE h.id = ?1"
            ),
            [hit_id],
            hit_row,
        )
        .optional()?;
    match found {
        Some(hit) if still_said(history, meetings, &hit)? => Ok(Some(hit)),
        _ => Ok(None),
    }
}

fn recent_hits(
    history: &Connection,
    meetings: Option<&Connection>,
    watch_id: &str,
) -> StorageResult<Vec<WorkWatchHit>> {
    let mut statement = history.prepare(&format!(
        "SELECT {HIT_COLUMNS} FROM planning_watch_hits h \
         JOIN planning_watches w ON w.id = h.watch_id WHERE h.watch_id = ?1 \
         ORDER BY h.heard_at_ms DESC, h.id"
    ))?;
    let mut hits = Vec::with_capacity(HITS_SHOWN);
    for hit in statement.query_map([watch_id], hit_row)? {
        let hit = hit?;
        if still_said(history, meetings, &hit)? {
            hits.push(hit);
        }
        if hits.len() == HITS_SHOWN {
            break;
        }
    }
    Ok(hits)
}

fn listed(
    history: &Connection,
    meetings: Option<&Connection>,
    people: Option<&People>,
) -> StorageResult<Vec<WorkWatch>> {
    let mut statement = history.prepare(
        "SELECT id, phrase, person_id, created_at_ms FROM planning_watches \
         ORDER BY created_at_ms DESC, id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(id, phrase, person_id, created_at_ms)| {
            let person_name = person_id.as_deref().and_then(|person_id| {
                people.and_then(|people| {
                    people
                        .choices
                        .iter()
                        .find(|choice| choice.id == person_id)
                        .map(|choice| choice.name.clone())
                })
            });
            Ok(WorkWatch {
                hits: recent_hits(history, meetings, &id)?,
                id,
                phrase,
                person_id,
                person_name,
                created_at_ms,
            })
        })
        .collect()
}

/// Every watch, newest first, each with what it heard last.
#[tauri::command]
#[specta::specta]
pub async fn work_watches(app: AppHandle) -> Result<Vec<WorkWatch>, String> {
    list(&app).await
}

pub(crate) async fn list(app: &AppHandle) -> Result<Vec<WorkWatch>, String> {
    let history = app.state::<Arc<HistoryManager>>();
    let meetings = app.state::<Arc<MeetingSessionManager>>();
    with_meetings(&history, &meetings, |history, meetings| {
        let people = meetings.map(People::read).transpose()?;
        listed(history, meetings, people.as_ref())
    })
    .await
}

/// Listens for `phrase`, said by anyone or only by `person_id`. Saving a
/// phrase already watched returns that watch.
#[tauri::command]
#[specta::specta]
pub async fn work_watch_save(
    app: AppHandle,
    phrase: String,
    person_id: Option<String>,
) -> Result<WorkWatch, String> {
    save(&app, &phrase, person_id).await
}

pub(crate) async fn save(
    app: &AppHandle,
    phrase: &str,
    person_id: Option<String>,
) -> Result<WorkWatch, String> {
    let phrase = phrase
        .trim()
        .trim_matches(['"', '\u{201C}', '\u{201D}', '\''])
        .trim();
    if phrase.is_empty() {
        return Err(crate::planning::failure(Refusal::Invalid.into()));
    }
    let history = app.state::<Arc<HistoryManager>>();
    let meetings = app.state::<Arc<MeetingSessionManager>>();
    let saved = with_meetings(&history, &meetings, |history, meetings| {
        let people = meetings.map(People::read).transpose()?;
        if let Some(person_id) = &person_id {
            let known = people
                .as_ref()
                .is_some_and(|people| people.choices.iter().any(|choice| &choice.id == person_id));
            if !known {
                return Err(Refusal::Invalid.into());
            }
        }
        history.execute(
            "INSERT INTO planning_watches (id, phrase, person_id, created_at_ms) \
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
            params![Uuid::new_v4().to_string(), phrase, person_id, now_ms()],
        )?;
        let id: String = history.query_row(
            "SELECT id FROM planning_watches \
             WHERE lower(phrase) = lower(?1) AND coalesce(person_id, '') = coalesce(?2, '')",
            params![phrase, person_id],
            |row| row.get(0),
        )?;
        let watch = listed(history, meetings, people.as_ref())?
            .into_iter()
            .find(|watch| watch.id == id)
            .ok_or(Refusal::NotFound)?;
        Ok(watch)
    })
    .await?;
    changed(app);
    Ok(saved)
}

/// Stops listening, and forgets what it heard.
#[tauri::command]
#[specta::specta]
pub async fn work_watch_remove(app: AppHandle, id: String) -> Result<(), String> {
    remove(&app, &id)
}

pub(crate) fn remove(app: &AppHandle, id: &str) -> Result<(), String> {
    crate::planning::history(&app.state::<Arc<HistoryManager>>(), |history| {
        if history.execute("DELETE FROM planning_watches WHERE id = ?1", [id])? == 0 {
            return Err(Refusal::NotFound.into());
        }
        Ok(())
    })?;
    changed(app);
    Ok(())
}

/// Keeps what a watch heard as a Triage task, once.
#[tauri::command]
#[specta::specta]
pub async fn work_watch_task(app: AppHandle, hit_id: String) -> Result<WorkItem, String> {
    task_from(&app, WorkActor::Me, &hit_id).await
}

pub(crate) async fn task_from(
    app: &AppHandle,
    actor: WorkActor,
    hit_id: &str,
) -> Result<WorkItem, String> {
    let history = app.state::<Arc<HistoryManager>>();
    let meetings = app.state::<Arc<MeetingSessionManager>>();
    let found = with_meetings(&history, &meetings, |history, meetings| {
        hit(history, meetings, hit_id)
    })
    .await?
    .ok_or_else(|| crate::planning::failure(Refusal::NotFound.into()))?;
    let line = WorkTakenLine {
        quote: found.line.quote.clone(),
        source: found.line.source.clone(),
        person_id: found.line.person_id.clone(),
    };
    let item = take_lines_as(app, actor, &format!("watch-task:{hit_id}"), &[line])
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| crate::planning::failure(Refusal::Invalid.into()))?;
    crate::planning::history(&history, |conn| {
        conn.execute(
            "UPDATE planning_watch_hits SET task_id = ?2 WHERE id = ?1",
            params![hit_id, item.id],
        )?;
        Ok(())
    })?;
    changed(app);
    Ok(item)
}

fn changed(app: &AppHandle) {
    if (WorkWatchesChanged {}).emit(app).is_err() {
        log::warn!("Watch change event could not be delivered");
    }
}

/// One line a watch heard, before it is kept.
struct Heard {
    watch: String,
    phrase: String,
    line: WorkAnswerLine,
    segment_id: Option<String>,
    start_offset_ns: Option<i64>,
}

/// Keeps `heard` unless its watch already heard the same meeting or
/// dictation; returns those kept, each with its new id.
fn keep<'h>(
    history: &Connection,
    heard: &'h [Heard],
    now: i64,
) -> StorageResult<Vec<(String, &'h Heard)>> {
    let mut kept = Vec::new();
    for heard in heard {
        let (kind, source_id, at_ms, end_ms) = match &heard.line.source {
            WorkCaptureSource::Meeting {
                session_id, at_ms, ..
            } => ("meeting", session_id.clone(), *at_ms, None),
            WorkCaptureSource::Dictation {
                history_id,
                start_ms,
                end_ms,
                ..
            } => (
                "dictation",
                history_id.to_string(),
                *start_ms,
                Some(*end_ms),
            ),
            _ => continue,
        };
        let id = Uuid::new_v4().to_string();
        let inserted = history.execute(
            "INSERT INTO planning_watch_hits (id, watch_id, source_kind, source_id, title, quote, \
             speaker, person_id, segment_id, at_ms, end_ms, start_offset_ns, said_at_ms, \
             heard_at_ms, link) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15) \
             ON CONFLICT DO NOTHING",
            params![
                id,
                heard.watch,
                kind,
                source_id,
                heard.line.title,
                heard.line.quote,
                heard.line.speaker,
                heard.line.person_id,
                heard.segment_id,
                at_ms,
                end_ms,
                heard.start_offset_ns,
                heard.line.when_utc_ms,
                now,
                heard.line.link,
            ],
        )?;
        if inserted == 1 {
            kept.push((id, heard));
        }
    }
    Ok(kept)
}

/// The lines of one processed meeting that say a watched topic, one per
/// watch.
fn heard_in_meeting(
    store: &MeetingStore,
    watches: &[(String, String, Option<String>)],
    session_id: MeetingSessionId,
    user_name: Option<&str>,
) -> Vec<Heard> {
    watches
        .iter()
        .filter_map(|(watch, phrase, person_id)| {
            let moments = store
                .said_matching(
                    &SaidWords::Phrase(phrase),
                    Within::Only(session_id),
                    LINES_READ,
                    user_name,
                )
                .inspect_err(|error| log::warn!("A watch could not read a meeting: {error:?}"))
                .ok()?;
            let moment = moments.into_iter().find(|moment| {
                person_id.as_deref().is_none_or(|wanted| {
                    moment
                        .person
                        .as_ref()
                        .is_some_and(|person| person.person_id.uuid().to_string() == wanted)
                })
            })?;
            let segment_id = moment
                .audio_retained
                .then(|| moment.segment_id.uuid().to_string());
            let start_offset_ns = i64::try_from(moment.start_offset_ns).ok();
            Some(Heard {
                watch: watch.clone(),
                phrase: phrase.clone(),
                line: said_line(moment),
                segment_id,
                start_offset_ns,
            })
        })
        .collect()
}

/// The watches a dictation's words say. A dictation is the operator's own
/// voice, so watches kept for someone else never hear one.
fn heard_in_dictation(
    watches: &[(String, String, Option<String>)],
    history_id: i64,
    duration_ms: i64,
    text: &str,
    said_at_ms: i64,
) -> Vec<Heard> {
    let lower = text.to_lowercase();
    watches
        .iter()
        .filter(|(_, phrase, person_id)| person_id.is_none() && says(&lower, phrase))
        .map(|(watch, phrase, _)| {
            let quote = text
                .split_inclusive(['.', '!', '?', '\n'])
                .map(str::trim)
                .find(|sentence| says(&sentence.to_lowercase(), phrase))
                .unwrap_or(text.trim())
                .to_owned();
            Heard {
                watch: watch.clone(),
                phrase: phrase.clone(),
                line: WorkAnswerLine {
                    id: format!("dictation:{history_id}"),
                    quote: quote.clone(),
                    title: "Dictation".to_owned(),
                    speaker: None,
                    person_id: None,
                    when_utc_ms: said_at_ms,
                    link: crate::query::dictation_link(history_id),
                    source: WorkCaptureSource::Dictation {
                        history_id,
                        start_ms: 0,
                        end_ms: duration_ms.max(0),
                        quote,
                    },
                    play: None,
                },
                segment_id: None,
                start_offset_ns: None,
            }
        })
        .collect()
}

/// Whether `lower` says `phrase` as whole words.
fn says(lower: &str, phrase: &str) -> bool {
    let phrase = phrase.to_lowercase();
    lower.match_indices(phrase.as_str()).any(|(at, found)| {
        let before = lower[..at].chars().next_back();
        let after = lower[at + found.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

fn watch_rows(history: &Connection) -> StorageResult<Vec<(String, String, Option<String>)>> {
    Ok(watches(history)?
        .into_iter()
        .map(|watch| (watch.id, watch.phrase, watch.person_id))
        .collect())
}

/// Listens to a just-processed meeting.
pub(crate) fn after_meeting(
    app: Option<&AppHandle>,
    store: &MeetingStore,
    session_id: MeetingSessionId,
) {
    let Some(app) = app else { return };
    let Some(history) = app.try_state::<Arc<HistoryManager>>() else {
        return;
    };
    let watches = match history.with_connection(|conn| watch_rows(conn)) {
        Ok(watches) if !watches.is_empty() => watches,
        Ok(_) => return,
        Err(error) => {
            log::warn!("Watches could not be read: {error:#}");
            return;
        }
    };
    let heard = heard_in_meeting(store, &watches, session_id, store.user_name().as_deref());
    tell_heard(app, &history, heard);
}

/// Listens to a just-saved dictation.
pub(crate) fn after_dictation(app: &AppHandle, history_id: i64, text: &str) {
    let Some(history) = app.try_state::<Arc<HistoryManager>>() else {
        return;
    };
    let read = history.with_connection(|conn| {
        let watches = watch_rows(conn)?;
        let duration_ms: Option<i64> = conn
            .query_row(
                "SELECT duration_ms FROM transcription_history WHERE id = ?1",
                [history_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok((watches, duration_ms.unwrap_or(0)))
    });
    let (watches, duration_ms) = match read {
        Ok((watches, _)) if watches.is_empty() => return,
        Ok(read) => read,
        Err(error) => {
            log::warn!("Watches could not be read: {error:#}");
            return;
        }
    };
    let heard = heard_in_dictation(&watches, history_id, duration_ms, text, now_ms());
    tell_heard(app, &history, heard);
}

fn tell_heard(app: &AppHandle, history: &HistoryManager, heard: Vec<Heard>) {
    if heard.is_empty() {
        return;
    }
    let kept = match history.with_connection(|conn| keep(conn, &heard, now_ms())) {
        Ok(kept) if kept.is_empty() => return,
        Ok(kept) => kept,
        Err(error) => {
            log::warn!("A watched line could not be kept: {error:#}");
            return;
        }
    };
    changed(app);
    let Some(notices) = app.try_state::<WatchNotices>() else {
        return;
    };
    let heard: Vec<&Heard> = kept.iter().map(|(_, heard)| *heard).collect();
    let (title, body) = notice(&heard);
    if !notices
        .0
        .present_watch(&format!("{REQUEST}{}", kept[0].0), &title, &body)
    {
        log::info!("A watch notification was not shown: {}", heard[0].phrase);
    }
}

/// One notification for everything one meeting or dictation said.
fn notice(heard: &[&Heard]) -> (String, String) {
    let phrases: Vec<String> = heard
        .iter()
        .map(|heard| format!("\u{201C}{}\u{201D}", heard.phrase))
        .collect();
    let named = match phrases.as_slice() {
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        [first, rest @ ..] => format!("{first} and {} more", rest.len()),
        [] => String::new(),
    };
    let first = &heard[0].line;
    let place = match first.source {
        WorkCaptureSource::Dictation { .. } => "a dictation".to_owned(),
        _ => first.title.clone(),
    };
    let body = match &first.speaker {
        Some(speaker) => format!("{speaker}: {}", first.quote),
        None => first.quote.clone(),
    };
    (format!("{named} came up in {place}"), body)
}

/// Where a watch notification's gestures land.
pub(crate) struct WatchAnswers {
    app: AppHandle,
}

impl WatchAnswers {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl WatchOpener for WatchAnswers {
    fn watch_answered(&self, id: &str, gesture: WatchGesture) {
        let Some(hit_id) = id.strip_prefix(REQUEST).map(str::to_owned) else {
            log::warn!("A watch notification carried an id it never posted");
            return;
        };
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            let gesture = match gesture {
                WatchGesture::Task => {
                    if let Err(error) = task_from(&app, WorkActor::Me, &hit_id).await {
                        log::warn!("A watched line could not become a task: {error}");
                    }
                    return;
                }
                WatchGesture::Open => WorkWatchGesture::Open,
                WatchGesture::Play => WorkWatchGesture::Play,
            };
            let history = app.state::<Arc<HistoryManager>>();
            let meetings = app.state::<Arc<MeetingSessionManager>>();
            let found = with_meetings(&history, &meetings, |history, meetings| {
                hit(history, meetings, &hit_id)
            })
            .await;
            let hit = match found {
                Ok(Some(hit)) => hit,
                Ok(None) => return,
                Err(error) => {
                    log::warn!("A watched line could not be read: {error}");
                    return;
                }
            };
            if gesture == WorkWatchGesture::Open {
                crate::show_main_window(&app);
            }
            if (WorkWatchOpened { hit, gesture }).emit(&app).is_err() {
                log::warn!("A watch click could not reach the window");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::history::MIGRATIONS;
    use crate::meeting::store::workflow_core_tests;
    use rusqlite_migration::Migrations;

    fn dictation(history: &Connection, text: &str) -> i64 {
        history
            .execute(
                "INSERT INTO transcription_history (file_name, timestamp, saved, title, transcription_text) \
                 VALUES ('sona-1.wav', 1, 0, 'Recording', ?1)",
                [text],
            )
            .expect("a dictation");
        history.last_insert_rowid()
    }

    fn quotes(watches: Vec<WorkWatch>) -> Vec<String> {
        let mut quotes: Vec<String> = watches
            .into_iter()
            .flat_map(|watch| watch.hits)
            .map(|hit| hit.line.quote)
            .collect();
        quotes.sort_unstable();
        quotes
    }

    /// A hit shows its words only while the meeting or dictation it quotes
    /// still keeps them; without the conversations nothing says a meeting
    /// does.
    #[test]
    fn a_watch_lists_only_hits_whose_words_are_still_kept() {
        let mut history = Connection::open_in_memory().expect("history");
        Migrations::new(MIGRATIONS.to_vec())
            .to_latest(&mut history)
            .expect("the history schema");
        history
            .execute(
                "INSERT INTO planning_watches (id, phrase, person_id, created_at_ms) \
                 VALUES ('mine', 'pricing', NULL, 1)",
                [],
            )
            .expect("a watch");
        let watches = vec![("mine".to_owned(), "pricing".to_owned(), None)];
        let (_directory, store) = workflow_core_tests::store();
        let meeting = workflow_core_tests::meeting(&store, "Pricing sync", 1_000);
        workflow_core_tests::transcript(&store, meeting, "Check the pricing page.");
        let mut heard = heard_in_meeting(&store, &watches, meeting, None);
        let mut deleted = heard[0].line.clone();
        deleted.quote = "The pricing deck is late.".to_owned();
        if let WorkCaptureSource::Meeting { session_id, .. } = &mut deleted.source {
            *session_id = Uuid::new_v4().to_string();
        }
        heard.push(Heard {
            watch: "mine".to_owned(),
            phrase: "pricing".to_owned(),
            line: deleted,
            segment_id: None,
            start_offset_ns: None,
        });
        let saved = dictation(&history, "Send the pricing sheet.");
        let erased = dictation(&history, "Pricing call notes.");
        heard.extend(heard_in_dictation(
            &watches,
            saved,
            4_000,
            "Send the pricing sheet.",
            2_000,
        ));
        heard.extend(heard_in_dictation(
            &watches,
            erased,
            4_000,
            "Pricing call notes.",
            3_000,
        ));
        keep(&history, &heard, 4_000).expect("kept");
        history
            .execute("DELETE FROM transcription_history WHERE id = ?1", [erased])
            .expect("the dictation deleted");

        let open = store
            .with_planning_snapshot(|meetings| Ok(listed(&history, Some(meetings), None)))
            .expect("the conversations")
            .expect("the watches");
        let closed = listed(&history, None, None).expect("the watches");

        assert_eq!(
            quotes(open),
            ["Check the pricing page.", "Send the pricing sheet."]
        );
        assert_eq!(quotes(closed), ["Send the pricing sheet."]);
    }

    #[test]
    fn a_watch_hears_its_words_only_as_whole_words() {
        assert!(says("the new pricing tiers", "Pricing"));
        assert!(says("pricing's still open", "pricing"));
        assert!(says("ship the beta list friday", "beta list"));
        assert!(!says("the repricing plan", "pricing"));
        assert!(!says("the beta listing", "beta list"));
    }

    #[test]
    fn a_dictation_is_kept_once_for_my_watch_and_quoted_where_it_says_it() {
        let mut history = Connection::open_in_memory().expect("history");
        Migrations::new(MIGRATIONS.to_vec())
            .to_latest(&mut history)
            .expect("the history schema");
        let watches = vec![
            ("mine".to_owned(), "pricing".to_owned(), None),
            (
                "dana's".to_owned(),
                "pricing".to_owned(),
                Some("dana".to_owned()),
            ),
        ];
        let text = "The repricing memo is done. Check the pricing page before Friday.";

        let heard = heard_in_dictation(&watches, 7, 4_000, text, 1_000);
        let first = keep(&history, &heard, 2_000).expect("kept");
        let again = keep(&history, &heard, 3_000).expect("kept again");

        let first: Vec<(&str, &str)> = first
            .iter()
            .map(|(_, heard)| (heard.watch.as_str(), heard.line.quote.as_str()))
            .collect();
        assert_eq!(first, [("mine", "Check the pricing page before Friday.")]);
        assert!(again.is_empty(), "heard twice: {}", again.len());
    }
}
