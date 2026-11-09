//! One item per promise. Notes written again give a meeting's commitments
//! new ids, and a promise made again in a later meeting comes in as a
//! commitment of its own; either way the item that came first goes on, and
//! the later meeting becomes one more of its receipts.

use super::items;
use super::sync::{same_words, settle, MeetingReader};
use super::types::{WorkActor, WorkItem, WorkStatus};
use crate::meeting::loop_types::{MeetingLoopDirection, MeetingLoopKind, MeetingLoopRow};
use anyhow::Result;
use rusqlite::{params, Connection};
use std::collections::HashSet;

fn words(text: &str) -> HashSet<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The share of all words in either wording that both use, in percent.
pub(super) fn shared_percent(left: &str, right: &str) -> usize {
    let (left, right) = (words(left), words(right));
    let all = left.union(&right).count();
    if all == 0 {
        return 0;
    }
    left.intersection(&right).count() * 100 / all
}

/// The item still tracking a commitment of `row`'s meeting that the notes,
/// as `rows` now hold them, no longer have, when `row` reads as that
/// commitment reworded: said at the same moment, or sharing at least half
/// of their words. None when nothing matches, or two match equally well.
pub(crate) fn reworded(
    history: &Connection,
    rows: &[MeetingLoopRow],
    row: &MeetingLoopRow,
) -> Result<Option<WorkItem>> {
    let meeting_id = row.session_id.uuid().to_string();
    let prefix = format!("{meeting_id}:{}:", MeetingLoopKind::Commitment.as_str());
    let live: HashSet<&str> = rows.iter().map(|row| row.loop_id.as_str()).collect();
    let mut statement = history.prepare_cached(
        "SELECT t.id, t.source_loop_id, t.title, EXISTS(
             SELECT 1 FROM planning_evidence_links e
             WHERE e.target_kind = 'task' AND e.target_id = COALESCE(t.duplicate_of, t.id)
               AND e.source_kind = 'meeting' AND e.source_id = ?2 AND e.at_ms = ?3)
         FROM planning_tasks t
         WHERE substr(t.source_loop_id, 1, length(?1)) = ?1",
    )?;
    let candidates = statement
        .query_map(
            params![prefix, meeting_id, i64::try_from(row.at_ms).ok()],
            |found| {
                Ok((
                    found.get::<_, String>(0)?,
                    found.get::<_, String>(1)?,
                    found.get::<_, String>(2)?,
                    found.get::<_, bool>(3)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut scored: Vec<((bool, usize), String)> = candidates
        .into_iter()
        .filter(|(_, loop_id, _, _)| !live.contains(loop_id.as_str()))
        .map(|(id, _, title, same_moment)| ((same_moment, shared_percent(&title, &row.text)), id))
        .filter(|((same_moment, shared), _)| *same_moment || *shared >= 50)
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0));
    match scored.as_slice() {
        [] => Ok(None),
        [(best, _), (next, _), ..] if best == next => Ok(None),
        [(_, id), ..] => Ok(Some(items::item(history, id)?)),
    }
}

/// Who made a promise, the way two meetings can agree on it: the user, the
/// person picked for it, else the name the notes read. None when the notes
/// name nobody.
fn promiser(row: &MeetingLoopRow) -> Option<String> {
    if row.direction == MeetingLoopDirection::Mine {
        return Some(String::from("me"));
    }
    if let Some(person) = row.owner_person_id {
        return Some(person.uuid().to_string());
    }
    row.owner_text
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_lowercase)
}

/// The oldest unfinished item that `row` promises again: older than
/// `item`, raised in another meeting by the same person, in the same words.
/// Work typed by hand has no promiser to compare, so nothing repeats it.
pub(crate) fn repeat_of(
    history: &Connection,
    meetings: &Connection,
    user_name: Option<&str>,
    row: &MeetingLoopRow,
    item: &WorkItem,
) -> Result<Option<WorkItem>> {
    let Some(promised_by) = promiser(row) else {
        return Ok(None);
    };
    let words = same_words(&row.text);
    let mut statement = history.prepare_cached(
        "SELECT id, title, source_loop_id FROM planning_tasks
         WHERE archived_at_ms IS NULL AND status NOT IN ('done', 'canceled', 'duplicate')
           AND number < ?1 AND instr(source_loop_id, ':') = 37
           AND substr(source_loop_id, 1, 36) != ?2
         ORDER BY number",
    )?;
    let candidates = statement
        .query_map(
            params![item.number, row.session_id.uuid().to_string()],
            |found| {
                Ok((
                    found.get::<_, String>(0)?,
                    found.get::<_, String>(1)?,
                    found.get::<_, String>(2)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut reader = MeetingReader::new(Some(meetings), user_name);
    for (id, title, loop_id) in candidates {
        if same_words(&title) != words {
            continue;
        }
        let Some(source) = reader.source(&loop_id)? else {
            continue;
        };
        if promiser(source.row).as_ref() == Some(&promised_by) {
            return Ok(Some(items::item(history, &id)?));
        }
    }
    Ok(None)
}

/// Marks `item` a duplicate of `original` for Sona, moving its receipts
/// and its commitment along. Returns the item they now all follow.
pub(crate) fn fold(
    history: &Connection,
    meetings: &Connection,
    item: &WorkItem,
    original: &WorkItem,
    now_ms: i64,
) -> Result<WorkItem> {
    let (root, _) =
        items::mark_duplicate(history, &item.id, &original.id, WorkActor::Sona, now_ms)?;
    settle(history, Some(meetings), &root, WorkActor::Sona, now_ms)?;
    items::item(history, &root)
}

/// Folds triage items that repeat earlier work into it, for meetings taken
/// in before repeats were folded as they came. Work the person already
/// accepted stays as they left it. Returns every item changed.
pub(crate) fn fold_repeats(
    history: &Connection,
    meetings: &Connection,
    user_name: Option<&str>,
    now_ms: i64,
) -> Result<Vec<WorkItem>> {
    let mut reader = MeetingReader::new(Some(meetings), user_name);
    let mut changed = Vec::new();
    for item in items::items(history, false)? {
        if item.status != WorkStatus::Triage {
            continue;
        }
        let Some(loop_id) = item.source_loop_id.as_deref() else {
            continue;
        };
        let Some((rows, row)) = reader
            .source(loop_id)?
            .map(|source| (source.meeting.rows.clone(), source.row.clone()))
        else {
            continue;
        };
        let original = match reworded(history, &rows, &row)? {
            Some(original) => Some(original),
            None => repeat_of(history, meetings, user_name, &row, &item)?,
        };
        if let Some(original) = original {
            let root = fold(history, meetings, &item, &original, now_ms)?;
            changed.push(items::item(history, &item.id)?);
            changed.push(root);
        }
    }
    Ok(changed)
}
