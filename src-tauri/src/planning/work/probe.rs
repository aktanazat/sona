//! Every meeting exported from Sona under one folder, oldest first, seeded
//! into one scratch store and taken into one scratch tracker the way the app
//! takes a finished meeting in: the meeting's commitments as the loops page
//! reads them, the Triage items they become, repeats folded, what Sona
//! suggests for each, and the follow-up the meeting page offers. Unlike the
//! notes probe, the report quotes the meetings, so it stays on this Mac.
//!
//! With SONA_PROBE_ENGINE set, in the form the notes probe takes, the notes
//! are written again by that engine; without it each export's stored notes
//! stand. The About-me name is SONA_PROBE_USER_NAME, else the export's
//! `speakers.json` `user_name`, else "Aktan Azat".
//!
//! SONA_PROBE_MEETINGS=<folder of <id>/meeting.json> <test binary> --ignored \
//!     --exact planning::work::probe::tasks_probe --nocapture --test-threads=1

use super::types::{WorkFieldSuggestion, WorkOwner, WorkQuery, WorkSuggestionValue};
use super::{follow, hygiene, items, list, live, sync};
use crate::meeting::loop_types::MeetingLoopKind;
use crate::meeting::processing::notes_probe::{pipeline, seed, Export};
use crate::meeting::store::loops::readable_artifacts_in;
use crate::meeting::store::workflow_core_tests;
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::MeetingSessionId;
use crate::planning::schema;
use crate::settings::MeetingLocalEngine;
use chrono::{Local, TimeZone};
use rusqlite::{params, Connection, TransactionBehavior};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct Meeting {
    title: String,
    started_utc_ms: i64,
    ended_utc_ms: i64,
    export: Export,
    stored_notes: Option<Value>,
    user_name: Option<String>,
}

fn read_meeting(folder: &Path) -> Option<Meeting> {
    let bytes = std::fs::read(folder.join("meeting.json")).ok()?;
    // PANIC: malformed selected exports must stop the evaluation, not vanish from it.
    let raw: Value = serde_json::from_slice(&bytes).expect("a Sona JSON export");
    // PANIC: the probe accepts Sona meeting exports, not arbitrary JSON.
    let export: Export = serde_json::from_slice(&bytes).expect("a Sona JSON export");
    let review = &raw["review"];
    let started_utc_ms = review["session"]["started_at_utc_ms"].as_i64()?;
    let last_ns = review["transcript"]
        .as_array()?
        .iter()
        .filter_map(|line| line["base"]["end_offset_ns"].as_i64())
        .max()
        .unwrap_or(0);
    let stored_notes = review["artifacts"]
        .as_array()?
        .iter()
        .filter(|artifact| artifact["state"] == "current")
        .max_by_key(|artifact| artifact["generated_at_utc_ms"].as_i64())
        .map(|artifact| artifact["content"].clone());
    let user_name = std::fs::read(folder.join("speakers.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|speakers| speakers["user_name"].as_str().map(str::to_owned));
    Some(Meeting {
        title: review["session"]["title"]
            .as_str()
            .unwrap_or("Meeting")
            .to_owned(),
        started_utc_ms,
        ended_utc_ms: started_utc_ms + last_ns / 1_000_000,
        export,
        stored_notes: stored_notes.map(|mut notes| {
            let places: HashMap<String, String> = review["transcript"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|line| {
                    let base = &line["base"];
                    let text = line["replacement_text"]
                        .as_str()
                        .or_else(|| base["text"].as_str())?;
                    Some((
                        base["segment_id"].as_str()?.to_owned(),
                        place(
                            base["start_offset_ns"].as_i64()?,
                            base["end_offset_ns"].as_i64()?,
                            text,
                        ),
                    ))
                })
                .collect();
            rewrite_ids(&mut notes, &places);
            notes
        }),
        user_name,
    })
}

fn place(start_ns: i64, end_ns: i64, text: &str) -> String {
    format!("{start_ns}|{end_ns}|{text}")
}

fn rewrite_ids(value: &mut Value, ids: &HashMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                match value {
                    Value::String(id) if key == "segment_id" => {
                        if let Some(new) = ids.get(id.as_str()) {
                            *id = new.clone();
                        }
                    }
                    _ => rewrite_ids(value, ids),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| rewrite_ids(item, ids)),
        _ => {}
    }
}

fn cite_scratch_lines(store: &MeetingStore, session_id: MeetingSessionId, notes: &mut Value) {
    // PANIC: missing seeded segments invalidate the evaluation's source links.
    let ids: HashMap<String, String> = store
        .with_planning_snapshot(|connection| {
            let mut statement = connection.prepare(
                "SELECT s.segment_id, s.start_offset_ns, s.end_offset_ns, s.base_text
                   FROM meeting_transcript_segments s
                   JOIN meeting_sessions m ON m.current_transcript_revision_id = s.transcript_revision_id
                  WHERE m.id = ?1",
            )?;
            let ids = statement
                .query_map([session_id.uuid().to_string()], |row| {
                    Ok((
                        place(row.get(1)?, row.get(2)?, &row.get::<_, String>(3)?),
                        row.get::<_, String>(0)?,
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(ids)
        })
        .expect("the scratch segments");
    rewrite_ids(notes, &ids);
}

fn scratch_update(store: &MeetingStore, sql: &str, values: impl rusqlite::Params) {
    // PANIC: a failed scratch write must abort the evaluation.
    store
        .with_planning_snapshot(|connection| Ok(connection.execute(sql, values)?))
        .expect("the scratch meeting updated");
}

fn engine() -> Option<MeetingLocalEngine> {
    std::env::var("SONA_PROBE_ENGINE").ok().map(|engine| {
        // PANIC: an invalid explicit engine configuration must stop the probe.
        serde_json::from_str(&engine)
            .expect("SONA_PROBE_ENGINE is a meeting engine in the form Sona's settings keep it")
    })
}

fn day(at_ms: i64) -> String {
    Local
        .timestamp_millis_opt(at_ms)
        .single()
        .map_or_else(|| "?".to_owned(), |at| at.format("%a %-d %b").to_string())
}

fn clock(at_ms: u64) -> String {
    let seconds = at_ms / 1000;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn clipped(text: &str) -> String {
    match text.char_indices().nth(90) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

fn shown(suggestion: &WorkFieldSuggestion, people: &HashMap<String, String>) -> String {
    let value = match &suggestion.value {
        WorkSuggestionValue::Owner(WorkOwner::Myself) => "me".to_owned(),
        WorkSuggestionValue::Owner(WorkOwner::Person { person_id }) => people
            .get(person_id)
            .cloned()
            .unwrap_or_else(|| person_id.clone()),
        WorkSuggestionValue::Owner(WorkOwner::Nobody) => "nobody".to_owned(),
        WorkSuggestionValue::Due(at_ms) => day(*at_ms),
        WorkSuggestionValue::Id(id) => id.clone(),
    };
    format!("{:?} {value} ({})", suggestion.field, suggestion.reason)
}

fn failed(step: &str, error: &anyhow::Error) -> StoreError {
    println!("{step} failed: {error:#}");
    StoreError::Unavailable
}

#[ignore = "takes the meeting exports SONA_PROBE_MEETINGS holds into a scratch tracker"]
#[test]
fn tasks_probe() {
    let folder = PathBuf::from(
        std::env::var_os("SONA_PROBE_MEETINGS")
            .expect("SONA_PROBE_MEETINGS names a folder of meeting exports"),
    );
    let mut meetings: Vec<Meeting> = std::fs::read_dir(&folder)
        .expect("the export folder")
        .filter_map(|entry| read_meeting(&entry.ok()?.path()))
        .collect();
    meetings.sort_by_key(|meeting| meeting.started_utc_ms);
    let user_name = std::env::var("SONA_PROBE_USER_NAME")
        .ok()
        .or_else(|| {
            meetings
                .iter()
                .find_map(|meeting| meeting.user_name.clone())
        })
        .unwrap_or_else(|| "Aktan Azat".to_owned());
    let engine = engine();
    println!("\nsona build {}", env!("SONA_BUILD_COMMIT"));
    println!(
        "{} meetings; about me name {user_name}; notes {}",
        meetings.len(),
        match &engine {
            Some(engine) => format!("written again by {engine:?}"),
            None => "as each export stored them".to_owned(),
        }
    );

    let (_directory, store) = workflow_core_tests::store();
    let name = user_name.clone();
    store.set_user_name_source(move || Some(name.clone()));
    let mut history = Connection::open_in_memory().expect("a scratch tracker");
    schema::apply_all(&history).expect("the planner schema");
    let mut sessions = Vec::new();

    for (index, meeting) in meetings.iter().enumerate() {
        let session_id = seed(&store, &meeting.export.review).expect("the scratch meeting");
        scratch_update(
            &store,
            "UPDATE meeting_sessions SET title = ?2, created_at_utc_ms = ?3, started_at_utc_ms = ?3
              WHERE id = ?1",
            params![
                session_id.uuid().to_string(),
                meeting.title,
                meeting.started_utc_ms
            ],
        );
        println!(
            "\n=== M{} {} {}: {}",
            index + 1,
            day(meeting.started_utc_ms),
            Local
                .timestamp_millis_opt(meeting.started_utc_ms)
                .single()
                .map_or_else(String::new, |at| at.format("%H:%M").to_string()),
            meeting.title
        );
        match (&engine, &meeting.stored_notes) {
            (Some(engine), _) => pipeline(engine, &store, session_id).expect("the notes written"),
            (None, Some(notes)) => {
                let mut notes = notes.clone();
                cite_scratch_lines(&store, session_id, &mut notes);
                workflow_core_tests::current_artifact(
                    &store,
                    session_id,
                    &notes,
                    meeting.ended_utc_ms,
                );
                scratch_update(
                    &store,
                    "UPDATE meeting_sessions SET phase = 'review_ready' WHERE id = ?1",
                    [session_id.uuid().to_string()],
                );
            }
            (None, None) => println!("no stored notes"),
        }
        sessions.push((session_id, meeting.ended_utc_ms, meeting.title.clone()));

        store
            .with_planning_snapshot(|meetings| {
                if let Some((_, notes)) = readable_artifacts_in(meetings, session_id)? {
                    for action in &notes.action_items {
                        println!(
                            "  action item: {} | owner {} | due {}",
                            action.text.text,
                            action.owner_text.as_deref().unwrap_or("-"),
                            action.due_text.as_deref().unwrap_or("-")
                        );
                    }
                    if notes.ledger.is_none() {
                        println!("  no ledger: {:?}", notes.ledger_failure);
                    }
                    println!(
                        "  follow-up draft in the notes: {}",
                        notes.follow_up_draft.text
                    );
                }
                let rows = sync::meeting_rows(meetings, session_id, Some(&user_name))
                    .map_err(|error| failed("rows", &error))?;
                for row in rows.iter().flat_map(|rows| &rows.rows) {
                    if row.kind != MeetingLoopKind::Commitment {
                        continue;
                    }
                    println!(
                        "  commitment {:?} [{:?}] who {} said by {} at {}: {} | “{}”",
                        row.direction,
                        row.firmness,
                        row.owner_text.as_deref().unwrap_or("-"),
                        row.speaker.as_deref().unwrap_or("-"),
                        clock(row.at_ms),
                        row.text,
                        clipped(row.quote.as_deref().unwrap_or(""))
                    );
                }
                let mut run = || -> anyhow::Result<usize> {
                    let tx = history.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let taken = sync::take_in_meeting(
                        &tx,
                        meetings,
                        session_id,
                        Some(&user_name),
                        meeting.ended_utc_ms,
                    )?;
                    tx.commit()?;
                    Ok(taken.len())
                };
                let taken = run().map_err(|error| failed("intake", &error))?;
                println!("  taken into triage: {taken}");
                Ok(())
            })
            .expect("the meeting taken in");
    }

    let now = sessions.last().map_or(0, |(_, ended, _)| ended + 60_000);
    store
        .with_planning_snapshot(|meetings| {
            let folded = hygiene::fold_repeats(&history, meetings, Some(&user_name), now)
                .map_err(|error| failed("fold", &error))?;
            println!("\nrepeats folded after intake: {}", folded.len());
            let all = list(
                &history,
                Some(meetings),
                Some(&user_name),
                &WorkQuery {
                    include_snoozed: true,
                    ..WorkQuery::default()
                },
                now,
            )
            .map_err(|error| failed("list", &error))?;
            let people: HashMap<String, String> = all
                .people
                .iter()
                .map(|person| (person.id.clone(), person.name.clone()))
                .collect();
            println!("tracker: {} items", all.items.len());
            let before = serde_json::to_value(
                items::items(&history, true).map_err(|error| failed("items", &error))?,
            )
            .expect("the tracker rows");
            let mut retaken = 0;
            for (session_id, _, _) in &sessions {
                retaken += sync::take_in_meeting(
                    &history,
                    meetings,
                    *session_id,
                    Some(&user_name),
                    now,
                )
                .map_err(|error| failed("intake again", &error))?
                .len();
            }
            let refolded = hygiene::fold_repeats(&history, meetings, Some(&user_name), now)
                .map_err(|error| failed("fold again", &error))?;
            let after = serde_json::to_value(
                items::items(&history, true).map_err(|error| failed("items", &error))?,
            )
            .expect("the tracker rows");
            assert_eq!(before, after, "intake again preserves every saved item");
            assert_eq!(retaken, 0, "intake again creates no tasks");
            assert!(refolded.is_empty(), "folding again makes no changes");
            println!("intake again: {retaken} tasks; every saved row unchanged");
            for (session_id, ended, title) in &sessions {
                let meeting_id = session_id.uuid().to_string();
                println!("\n--- tasks from {} ({})", title, day(*ended));
                for row in all
                    .items
                    .iter()
                    .filter(|row| row.source_meeting_id.as_deref() == Some(meeting_id.as_str()))
                {
                    println!(
                        "  #{} [{:?}{}] {} | raised in {} | said by {} | “{}”",
                        row.item.number,
                        row.item.status,
                        row.item
                            .duplicate_of_id
                            .as_ref()
                            .map_or(String::new(), |_| ", duplicate".to_owned()),
                        row.item.title,
                        row.raised_in,
                        row.source_speaker.as_deref().unwrap_or("-"),
                        clipped(row.source_quote.as_deref().unwrap_or(""))
                    );
                    let loop_id = row.item.source_loop_id.as_deref().expect("a source loop");
                    let words = crate::planning::sources::loop_words(&history, meetings, loop_id)
                        .map_err(|error| failed("source words", &error))?;
                    assert!(words.is_some(), "the source loop still exists in its meeting");
                    let evidence = crate::planning::store::task_evidence(&history, &row.item.id)
                        .map_err(|error| failed("source links", &error))?;
                    for source in &evidence {
                        let at_ms = source.at_ms.expect("a transcript offset");
                        assert!(
                            crate::planning::sources::retained_passage(
                                meetings,
                                &source.source_id,
                                at_ms,
                            )
                            .map_err(|error| failed("source passage", &error))?,
                            "the source link points into the retained transcript"
                        );
                    }
                    println!(
                        "      source {} | {} saved transcript links | offset {:?}",
                        crate::query::loop_link(&crate::meeting::loop_types::MeetingLoopId(
                            loop_id.to_owned()
                        )),
                        evidence.len(),
                        row.source_at_ms,
                    );
                    for suggestion in all.suggestions.get(&row.item.id).into_iter().flatten() {
                        println!("      suggests {}", shown(suggestion, &people));
                    }
                }
                let listed = list(
                    &history,
                    Some(meetings),
                    Some(&user_name),
                    &WorkQuery {
                        source_meeting_id: Some(meeting_id.clone()),
                        include_snoozed: true,
                        ..WorkQuery::default()
                    },
                    now,
                )
                .map_err(|error| failed("list", &error))?;
                let follow_up = live::follow_up(meeting_id, Some(title), Some(&user_name), listed);
                println!("  follow-up the meeting page offers:\n{}", follow_up.text);
            }
            let plan_day = items::local_today(now)
                .map_err(|error| failed("local day", &error))?
                .format("%Y-%m-%d")
                .to_string();
            let initial_plan = items::day(&history, &plan_day)
                .map_err(|error| failed("day plan", &error))?;
            println!(
                "\nautomatic day plan: {} focus items; {}",
                initial_plan.focus_ids.len(),
                follow::probe_inputs(&history, meetings, now)
                    .map_err(|error| failed("morning and brief", &error))?,
            );
            if let Some(row) = all.items.iter().find(|row| {
                all.suggestions
                    .get(&row.item.id)
                    .into_iter()
                    .flatten()
                    .any(|suggestion| {
                        suggestion.value == WorkSuggestionValue::Owner(WorkOwner::Myself)
                    })
            }) {
                let mut reader = sync::MeetingReader::new(Some(meetings), Some(&user_name));
                let patch = sync::acceptance(&history, &mut reader, &row.item, true, None)
                    .map_err(|error| failed("acceptance", &error))?;
                let (accepted, _) = sync::accept(
                    &history,
                    Some(meetings),
                    &row.item,
                    &patch,
                    super::types::WorkActor::Me,
                    now,
                )
                .map_err(|error| failed("accept task", &error))?;
                let planned = items::plan_day(
                    &history,
                    &plan_day,
                    std::slice::from_ref(&accepted.id),
                    now,
                )
                .map_err(|error| failed("choose day focus", &error))?;
                assert_eq!(planned.focus_ids.as_slice(), std::slice::from_ref(&accepted.id));
                assert_eq!(
                    items::day(&history, &plan_day)
                        .map_err(|error| failed("reread day", &error))?,
                    planned,
                );
                println!(
                    "manual demo only: accepted #{} with owner {:?}, due {:?}; chose it for {}; saved plan has {} focus item",
                    accepted.number,
                    accepted.owner(),
                    accepted.due_at_ms,
                    plan_day,
                    planned.focus_ids.len(),
                );
            }
            Ok(())
        })
        .expect("the tracker read");
    if let Some(path) = std::env::var_os("SONA_PROBE_TRACKER") {
        history
            .execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])
            .expect("the scratch tracker saved");
        let saved = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("the saved scratch tracker reopened");
        assert_eq!(
            items::items(&history, true).expect("the tracker"),
            items::items(&saved, true).expect("the reopened tracker"),
            "every tracker field survives reopening",
        );
        let focused: i64 = saved
            .query_row("SELECT count(*) FROM planning_days", [], |row| row.get(0))
            .expect("the saved day plan");
        println!("saved tracker reopened: all task fields identical; {focused} day plan");
    }
    history.close().expect("the scratch tracker closed");
}

#[ignore = "reads conversational meeting plans from SONA_PROBE_MEETINGS"]
#[test]
fn plans_probe() {
    let folder = PathBuf::from(
        std::env::var_os("SONA_PROBE_MEETINGS")
            .expect("SONA_PROBE_MEETINGS names the meeting exports"),
    );
    let mut meetings: Vec<Meeting> = std::fs::read_dir(folder)
        .expect("the export folder")
        .filter_map(|entry| read_meeting(&entry.ok()?.path()))
        .collect();
    meetings.sort_by_key(|meeting| meeting.started_utc_ms);
    let engine = engine().expect("SONA_PROBE_ENGINE names the local meeting engine");
    let (_directory, store) = workflow_core_tests::store();
    for (index, meeting) in meetings.iter().enumerate() {
        let session_id = seed(&store, &meeting.export.review).expect("the scratch meeting");
        scratch_update(
            &store,
            "UPDATE meeting_sessions SET title = ?2, created_at_utc_ms = ?3,
                    started_at_utc_ms = ?3, phase = 'review_ready' WHERE id = ?1",
            params![
                session_id.uuid().to_string(),
                meeting.title,
                meeting.started_utc_ms,
            ],
        );
        println!(
            "\n=== M{} {}: {}",
            index + 1,
            day(meeting.started_utc_ms),
            meeting.title
        );
        crate::meeting::processing::notes_probe::plans(
            &engine,
            &store,
            session_id,
            meeting.ended_utc_ms,
        )
        .expect("the conversational plans read");
    }
}
