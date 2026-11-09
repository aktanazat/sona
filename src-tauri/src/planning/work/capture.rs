//! Work put in from outside the tracker's screens: a task typed in the
//! new-task box over another app, with the page or file in front kept
//! beside it, and the tasks one recording says, each kept with the stretch
//! of the recording that said it.

use super::items;
use super::parse::{parse, spoken_tasks, Lexicon, Said, SpokenTask, WorkField};
use super::types::{WorkActor, WorkItem, WorkItemInput, WorkOwner, WorkStatus};
use super::{insert_input, run_history, run_with_meetings, Change, Done, Effects};
use crate::context::{read_front_window, FrontWindow, FrontWindowRead};
use crate::meeting::open_windows::window_label;
use crate::planning::sources;
use crate::planning::store::{self, Refusal};
use crate::planning::types::EvidenceSourceKind;
use anyhow::Result as StorageResult;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::ops::Range;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use uuid::Uuid;

/// The shortcut that opens the new-task box over any app.
pub(crate) const QUICK_ENTRY_BINDING_ID: &str = "work_quick_entry";
/// The shortcut held to say tasks.
pub(crate) const RAMBLE_BINDING_ID: &str = "work_ramble";

/// Where a captured task came from, kept with it as evidence.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkCaptureSource {
    /// A web page or a file, an absolute http, https or file address, with
    /// what its window called it.
    Link { url: String, title: Option<String> },
    /// The milliseconds of a recording that said the task, and its words.
    #[serde(rename_all = "camelCase")]
    Dictation {
        history_id: i64,
        start_ms: i64,
        end_ms: i64,
        quote: String,
    },
    /// A line of a meeting: where it starts in the meeting, in ms, and its
    /// words.
    #[serde(rename_all = "camelCase")]
    Meeting {
        session_id: String,
        at_ms: i64,
        quote: String,
    },
    /// Something a meeting left open: a promise, a question, a decision.
    #[serde(rename_all = "camelCase")]
    Loop { loop_id: String },
}

/// One cited line to keep as a Triage task.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkTakenLine {
    pub quote: String,
    pub source: WorkCaptureSource,
    /// Who said it, when a person was confirmed for the voice.
    pub person_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkLink {
    pub url: String,
    pub title: Option<String>,
}

/// Sent when the new-task shortcut is pressed, with the page or file in the
/// window in front when one can be read.
#[derive(Clone, Debug, Deserialize, Serialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
pub struct WorkQuickEntry {
    pub link: Option<WorkLink>,
}

/// Sent once the tasks a recording said are in Triage. No items and no
/// error: no task was heard.
#[derive(Clone, Debug, Deserialize, Serialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
pub struct WorkRambleFiled {
    pub history_id: Option<i64>,
    pub items: Vec<WorkItem>,
    pub error: Option<String>,
}

/// A held-shortcut recording said a command over the tracker rather than
/// tasks; nothing was filed.
#[derive(Clone, Debug, Deserialize, Serialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
pub struct WorkCommandHeard {
    pub said: String,
    pub history_id: Option<i64>,
}

/// [`super::work_create`] with where the task came from kept beside it.
#[tauri::command]
#[specta::specta]
pub async fn work_capture(
    app: AppHandle,
    operation_id: String,
    input: WorkItemInput,
    source: WorkCaptureSource,
) -> Result<WorkItem, String> {
    let change = Change::now(&operation_id, WorkActor::Me);
    let capture = |history: &mut Connection, meetings: Option<&Connection>| {
        store::mutate(
            history,
            change.operation_id,
            "work_capture",
            &(&input, &source),
            change.at_ms,
            |tx| {
                let item = insert_input(tx, meetings, change, &input)?;
                attach(tx, &item.id, &source, change.at_ms)?;
                let mut effects = Effects::default();
                effects.item(&item);
                Ok(Done {
                    value: item,
                    effects,
                })
            },
        )
    };
    if matches!(input.owner, Some(WorkOwner::Person { .. })) {
        run_with_meetings(&app, capture).await
    } else {
        run_history(&app, |history| capture(history, None))
    }
}

/// Keeps `source` as evidence for the task `task_id`, once.
pub(crate) fn attach(
    tx: &Connection,
    task_id: &str,
    source: &WorkCaptureSource,
    now_ms: i64,
) -> StorageResult<()> {
    let caption = |text: Option<&str>| {
        text.map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let (kind, source_id, at_ms, end_ms, caption) = match source {
        WorkCaptureSource::Link { url, title } => (
            EvidenceSourceKind::Link,
            sources::link_url(url)?,
            None,
            None,
            caption(title.as_deref()),
        ),
        WorkCaptureSource::Dictation {
            history_id,
            start_ms,
            end_ms,
            quote,
        } => {
            if *start_ms < 0 || end_ms < start_ms {
                return Err(Refusal::Invalid.into());
            }
            let kept: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM transcription_history WHERE id = ?1)",
                [history_id],
                |row| row.get(0),
            )?;
            if !kept {
                return Err(Refusal::SourceUnavailable.into());
            }
            (
                EvidenceSourceKind::Dictation,
                history_id.to_string(),
                Some(*start_ms),
                Some(*end_ms),
                caption(Some(quote)),
            )
        }
        WorkCaptureSource::Meeting {
            session_id,
            at_ms,
            quote,
        } => {
            if *at_ms < 0 {
                return Err(Refusal::Invalid.into());
            }
            (
                EvidenceSourceKind::Meeting,
                store::uuid(session_id)?,
                Some(*at_ms),
                None,
                caption(Some(quote)),
            )
        }
        WorkCaptureSource::Loop { loop_id } => {
            (EvidenceSourceKind::Loop, loop_id.clone(), None, None, None)
        }
    };
    tx.execute(
        "INSERT INTO planning_evidence_links
            (id, target_kind, target_id, source_kind, source_id, at_ms, revision, created_at_ms, end_ms, caption)
         VALUES (?1, 'task', ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8)
         ON CONFLICT DO NOTHING",
        params![
            Uuid::new_v4().to_string(),
            task_id,
            kind.as_str(),
            source_id,
            at_ms,
            now_ms,
            end_ms,
            caption
        ],
    )?;
    Ok(())
}

/// Keeps each line as a Triage task with the line as its evidence, read for
/// a date, a priority, an owner and a project the way speech is. A line
/// whose loop the tracker already follows gives that loop's item instead.
#[tauri::command]
#[specta::specta]
pub async fn work_take_lines(
    app: AppHandle,
    operation_id: String,
    lines: Vec<WorkTakenLine>,
) -> Result<Vec<WorkItem>, String> {
    take_lines_as(&app, WorkActor::Me, &operation_id, &lines).await
}

pub(crate) async fn take_lines_as(
    app: &AppHandle,
    actor: WorkActor,
    operation_id: &str,
    lines: &[WorkTakenLine],
) -> Result<Vec<WorkItem>, String> {
    if lines.is_empty() || lines.iter().any(|line| line.quote.trim().is_empty()) {
        return Err(crate::planning::failure(Refusal::Invalid.into()));
    }
    let change = Change::now(operation_id, actor);
    run_with_meetings(app, |history, meetings| {
        let lexicon = Lexicon::read(history, meetings)?;
        let now = chrono::Local::now();
        store::mutate(
            history,
            change.operation_id,
            "work_take_lines",
            &lines,
            change.at_ms,
            |tx| {
                let mut effects = Effects::default();
                let mut items = Vec::with_capacity(lines.len());
                for line in lines {
                    let followed = match &line.source {
                        WorkCaptureSource::Loop { loop_id } => items::by_source_loop(tx, loop_id)?,
                        _ => None,
                    };
                    let item = match followed {
                        Some(item) => item,
                        None => {
                            let item = insert_input(
                                tx,
                                meetings,
                                change,
                                &line_input(line, &lexicon, &now),
                            )?;
                            attach(tx, &item.id, &line.source, change.at_ms)?;
                            item
                        }
                    };
                    effects.item(&item);
                    items.push(item);
                }
                Ok(Done {
                    value: items,
                    effects,
                })
            },
        )
    })
    .await
}

/// A cited line as a new Triage item: its words read as speech, owned by
/// whoever they name, else by whoever said them.
fn line_input<Tz: chrono::TimeZone>(
    line: &WorkTakenLine,
    lexicon: &Lexicon,
    now: &chrono::DateTime<Tz>,
) -> WorkItemInput {
    let quote = line.quote.trim().trim_matches('…').trim();
    let parsed = parse(quote, Said::Spoken, &[], lexicon, now);
    let task = SpokenTask {
        said: 0..quote.len(),
        title: if parsed.title.is_empty() {
            quote.to_owned()
        } else {
            parsed.title
        },
        fields: parsed.tokens.into_iter().map(|token| token.field).collect(),
        someone_else: false,
    };
    let mut input = input(&task);
    let named = task
        .fields
        .iter()
        .any(|field| matches!(field, WorkField::Owner { .. }));
    if !named {
        input.owner = Some(match &line.person_id {
            Some(person_id) => WorkOwner::Person {
                person_id: person_id.clone(),
            },
            None => WorkOwner::Nobody,
        });
    }
    input
}

/// Reads the window in front, then asks the app to open the new-task box
/// with its page or file. A password field, a private window or Sona's own
/// window give the box nothing.
pub(crate) async fn open_quick_entry(app: AppHandle) {
    let keep_full_url = crate::settings::get_settings(&app).context_url_capture_enabled;
    let read = tauri::async_runtime::spawn_blocking(move || read_front_window(keep_full_url)).await;
    let link = match read {
        Ok(FrontWindowRead::Window(window)) => front_link(&window),
        _ => None,
    };
    if (WorkQuickEntry { link }).emit(&app).is_err() {
        log::warn!("The new-task box could not be opened");
    }
}

/// The window's page, else its file, else, when full addresses are not
/// kept, its site.
fn front_link(window: &FrontWindow) -> Option<WorkLink> {
    let url = window
        .url
        .clone()
        .or_else(|| {
            let path = window.document_path.as_deref()?;
            url::Url::from_file_path(path).ok().map(String::from)
        })
        .or_else(|| {
            window
                .host
                .as_deref()
                .map(|host| format!("https://{host}/"))
        })?;
    Some(WorkLink {
        url,
        title: window_label(
            window.document_path.as_deref(),
            window.title.as_deref(),
            window.host.as_deref(),
        ),
    })
}

const SAMPLE_RATE: usize = 16_000;
const FRAME_SAMPLES: usize = 480;
const FRAME_MS: i64 = 30;
/// A pause shorter than this sits inside one phrase.
const PHRASE_GAP_FRAMES: usize = 10;
/// How far a cut between two tasks may move to reach a pause.
const SNAP_MS: i64 = 1_500;
/// Room left either side of a task's stretch.
const PAD_MS: i64 = 150;

/// Files the tasks `passage` says into Triage, each with the stretch of the
/// recording `history_id` that said it, then tells the app what was filed.
/// A passage that reads as a command over the tracker files nothing: the
/// app is told what was heard so it can show what the command would do.
pub(crate) fn file_spoken(
    app: AppHandle,
    history_id: Option<i64>,
    passage: String,
    samples: Arc<Vec<f32>>,
) {
    tauri::async_runtime::spawn(async move {
        let commanded = {
            let history = app.state::<Arc<crate::managers::history::HistoryManager>>();
            let meetings = app.state::<Arc<crate::meeting::session::MeetingSessionManager>>();
            super::with_meetings(&history, &meetings, |history, meetings| {
                Ok(super::command::is_command(
                    &passage,
                    &Lexicon::read(history, meetings)?,
                ))
            })
            .await
            .unwrap_or(false)
        };
        if commanded {
            let heard = WorkCommandHeard {
                said: passage,
                history_id,
            };
            if heard.emit(&app).is_err() {
                log::warn!("A spoken command could not reach the window");
            }
            return;
        }
        let duration_ms = i64::try_from(samples.len() * 1_000 / SAMPLE_RATE).unwrap_or(i64::MAX);
        let detector_app = app.clone();
        let voiced = tauri::async_runtime::spawn_blocking(move || {
            voiced_frames(&detector_app, samples.as_slice())
        })
        .await
        .unwrap_or_default();
        let filed = run_with_meetings(&app, |history, meetings| {
            file(
                history,
                meetings,
                history_id,
                &passage,
                &voiced,
                duration_ms,
            )
        })
        .await;
        let event = match filed {
            Ok(items) => WorkRambleFiled {
                history_id,
                items,
                error: None,
            },
            Err(error) => {
                log::warn!("Spoken tasks could not be filed: {error}");
                WorkRambleFiled {
                    history_id,
                    items: Vec::new(),
                    error: Some(error),
                }
            }
        };
        if event.emit(&app).is_err() {
            log::warn!("Filed spoken tasks could not be announced");
        }
    });
}

/// Which 30 ms frames of the recording hold a voice; none when the
/// detector cannot open, and the tasks are then laid over the whole
/// recording.
fn voiced_frames(app: &AppHandle, samples: &[f32]) -> Vec<bool> {
    use crate::audio_toolkit::vad::VoiceActivityDetector;
    let mut detector = match crate::managers::audio::create_voice_detector(app, 0) {
        Ok(detector) => detector,
        Err(error) => {
            log::warn!("Spoken tasks are timed without a voice detector: {error:#}");
            return Vec::new();
        }
    };
    samples
        .chunks_exact(FRAME_SAMPLES)
        .map(|frame| detector.is_voice(frame).unwrap_or(false))
        .collect()
}

fn file(
    history: &mut Connection,
    meetings: Option<&Connection>,
    history_id: Option<i64>,
    passage: &str,
    voiced: &[bool],
    duration_ms: i64,
) -> StorageResult<Done<Vec<WorkItem>>> {
    let lexicon = Lexicon::read(history, meetings)?;
    let tasks = spoken_tasks(passage, &lexicon, &chrono::Local::now());
    if tasks.is_empty() {
        return Ok(Done {
            value: Vec::new(),
            effects: Effects::default(),
        });
    }
    let said: Vec<Range<usize>> = tasks.iter().map(|task| task.said.clone()).collect();
    let spans = spans(passage, &said, voiced, duration_ms);
    let operation_id = Uuid::new_v4().to_string();
    let change = Change::now(&operation_id, WorkActor::Sona);
    store::mutate(
        history,
        &operation_id,
        "work_ramble",
        &(history_id, passage),
        change.at_ms,
        |tx| {
            let mut effects = Effects::default();
            let mut items = Vec::with_capacity(tasks.len());
            for (task, span) in tasks.iter().zip(&spans) {
                let item = insert_input(tx, meetings, change, &input(task))?;
                if let Some(history_id) = history_id {
                    let source = WorkCaptureSource::Dictation {
                        history_id,
                        start_ms: span.start,
                        end_ms: span.end,
                        quote: passage[task.said.clone()].to_owned(),
                    };
                    attach(tx, &item.id, &source, change.at_ms)?;
                }
                effects.item(&item);
                items.push(item);
            }
            Ok(Done {
                value: items,
                effects,
            })
        },
    )
}

/// A spoken task as a new Triage item. With nobody named it is the
/// speaker's, unless it starts with someone else doing it.
fn input(task: &SpokenTask) -> WorkItemInput {
    let mut input = WorkItemInput {
        title: task.title.clone(),
        details: None,
        status: Some(WorkStatus::Triage),
        priority: None,
        project_id: None,
        milestone_id: None,
        owner: None,
        due_at_ms: None,
        label_ids: None,
        recurrence: None,
    };
    let mut labels = Vec::new();
    for field in &task.fields {
        match field {
            WorkField::Due { at, .. } => input.due_at_ms = Some(*at),
            WorkField::Priority { level } => input.priority = Some(*level),
            WorkField::Label { id: Some(id), .. } => labels.push(id.clone()),
            WorkField::Label { id: None, .. } => {}
            WorkField::Owner { owner, .. } => input.owner = Some(owner.clone()),
            WorkField::Project { id, .. } => input.project_id = Some(id.clone()),
            WorkField::Repeat { every, next_at } => {
                input.recurrence = Some(every.clone());
                input.due_at_ms = input.due_at_ms.or(*next_at);
            }
        }
    }
    if input.owner.is_none() {
        input.owner = Some(if task.someone_else {
            WorkOwner::Nobody
        } else {
            WorkOwner::Myself
        });
    }
    if !labels.is_empty() {
        input.label_ids = Some(labels);
    }
    input
}

/// Where each part of `passage` in `said` sits in its recording, in ms: the
/// passage's letters and digits laid evenly over the voiced stretches, each
/// cut between two parts moved to the nearest pause, and a little room left
/// either side. With no voice found the letters are laid over the whole
/// recording.
fn spans(
    passage: &str,
    said: &[Range<usize>],
    voiced: &[bool],
    duration_ms: i64,
) -> Vec<Range<i64>> {
    let mut phrases = voiced_stretches(voiced, duration_ms);
    if phrases.is_empty() {
        phrases.push(0..duration_ms);
    }
    let spoken = |end: usize| {
        passage[..end]
            .chars()
            .filter(|character| character.is_alphanumeric())
            .count()
    };
    let total = spoken(passage.len());
    let voiced_ms: i64 = phrases.iter().map(|phrase| phrase.end - phrase.start).sum();
    let at = |byte: usize| {
        if total == 0 {
            return 0;
        }
        let share = i64::try_from(spoken(byte)).unwrap_or(i64::MAX);
        let total = i64::try_from(total).unwrap_or(i64::MAX);
        let mut left = voiced_ms.saturating_mul(share) / total;
        for phrase in &phrases {
            let length = phrase.end - phrase.start;
            if left <= length {
                return phrase.start + left;
            }
            left -= length;
        }
        phrases.last().map_or(0, |phrase| phrase.end)
    };
    let mut spans: Vec<Range<i64>> = said
        .iter()
        .map(|part| at(part.start)..at(part.end))
        .collect();
    let pauses: Vec<Range<i64>> = phrases
        .windows(2)
        .map(|pair| pair[0].end..pair[1].start)
        .collect();
    let mut next_pause = 0;
    for index in 1..spans.len() {
        let cut = (spans[index - 1].end + spans[index].start) / 2;
        let nearest = (next_pause..pauses.len())
            .map(|pause| {
                (
                    pause,
                    ((pauses[pause].start + pauses[pause].end) / 2 - cut).abs(),
                )
            })
            .filter(|(_, distance)| *distance <= SNAP_MS)
            .min_by_key(|(_, distance)| *distance);
        match nearest {
            Some((pause, _)) => {
                spans[index - 1].end = pauses[pause].start;
                spans[index].start = pauses[pause].end;
                next_pause = pause + 1;
            }
            None => {
                spans[index - 1].end = cut;
                spans[index].start = cut;
            }
        }
    }
    spans
        .into_iter()
        .map(|span| {
            let start = (span.start - PAD_MS).max(0);
            start..(span.end + PAD_MS).min(duration_ms).max(start)
        })
        .collect()
}

/// The voiced stretches of a recording in ms, a short pause kept inside
/// the stretch around it.
fn voiced_stretches(voiced: &[bool], duration_ms: i64) -> Vec<Range<i64>> {
    let mut frames: Vec<Range<usize>> = Vec::new();
    for (index, voice) in voiced.iter().enumerate() {
        if !voice {
            continue;
        }
        match frames.last_mut() {
            Some(last) if index - last.end < PHRASE_GAP_FRAMES => last.end = index + 1,
            _ => frames.push(index..index + 1),
        }
    }
    let ms = |frame: usize| (i64::try_from(frame).unwrap_or(i64::MAX) * FRAME_MS).min(duration_ms);
    frames
        .into_iter()
        .map(|frames| ms(frames.start)..ms(frames.end))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::history::MIGRATIONS;
    use crate::planning::store::task_evidence;
    use rusqlite_migration::Migrations;

    #[test]
    fn a_ramble_files_a_triage_task_per_part_with_the_stretch_that_said_it() {
        let passage = "I owe Sam the Deck Friday, Dana sending the rebate sheet Thursday, high priority on the pricing page.";
        let mut history = Connection::open_in_memory().expect("history");
        Migrations::new(MIGRATIONS.to_vec())
            .to_latest(&mut history)
            .expect("the history schema");
        history
            .execute(
                "INSERT INTO transcription_history (file_name, timestamp, saved, title, transcription_text)
                 VALUES ('sona-1.wav', 1, 0, 'Recording', ?1)",
                [passage],
            )
            .expect("the recording");
        let recording = history.last_insert_rowid();
        let voiced: Vec<bool> = [
            (60, true),
            (15, false),
            (100, true),
            (15, false),
            (70, true),
        ]
        .into_iter()
        .flat_map(|(frames, voice)| std::iter::repeat_n(voice, frames))
        .collect();

        let filed = file(&mut history, None, Some(recording), passage, &voiced, 8_000)
            .expect("the tasks filed")
            .value;

        let filed: Vec<_> = filed
            .iter()
            .map(|item| {
                let evidence: Vec<_> = task_evidence(&history, &item.id)
                    .expect("the task's evidence")
                    .into_iter()
                    .map(|link| {
                        (
                            link.source_kind,
                            link.source_id,
                            link.at_ms,
                            link.end_ms,
                            link.caption,
                        )
                    })
                    .collect();
                (
                    item.title.as_str(),
                    item.status,
                    item.owner(),
                    item.priority,
                    evidence,
                )
            })
            .collect();
        let stretch = |start: i64, end: i64, quote: &str| {
            vec![(
                EvidenceSourceKind::Dictation,
                recording.to_string(),
                Some(start),
                Some(end),
                Some(quote.to_owned()),
            )]
        };
        assert_eq!(
            filed,
            [
                (
                    "Owe Sam the Deck",
                    WorkStatus::Triage,
                    WorkOwner::Myself,
                    0,
                    stretch(0, 1_800 + PAD_MS, "I owe Sam the Deck Friday")
                ),
                (
                    "Dana sending the rebate sheet",
                    WorkStatus::Triage,
                    WorkOwner::Nobody,
                    0,
                    stretch(
                        2_250 - PAD_MS,
                        5_250 + PAD_MS,
                        "Dana sending the rebate sheet Thursday"
                    )
                ),
                (
                    "Pricing page",
                    WorkStatus::Triage,
                    WorkOwner::Myself,
                    2,
                    stretch(
                        5_700 - PAD_MS,
                        7_800 + PAD_MS,
                        "high priority on the pricing page."
                    )
                ),
            ]
        );
    }
}
