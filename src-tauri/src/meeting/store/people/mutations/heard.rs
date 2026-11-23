//! Who a meeting was with, read from the meeting itself: names said in the
//! transcript and names the call listed, matched against People.
//!
//! Everything here is a suggestion. A name being said is evidence the person
//! came up, not proof they were in the room, so the user confirms it.

use super::linking::upsert_link_in;
use crate::meeting::people_types::{Person, PersonLinkConfidence, PersonLinkSource};
use crate::meeting::store::loops::user_speaker_labels_in;
use crate::meeting::store::people::{
    all_people_in, exact_phrase_mentioned, identity_names, normalized,
};
use crate::meeting::store::voice_identity::{is_generated_speaker_label, names_user};
use crate::meeting::store::StoreError;
use crate::meeting::types::MeetingSessionId;
use rusqlite::Connection;
use std::collections::HashMap;

/// First names that are also everyday words. Capitalised at the start of a
/// sentence they prove nothing ("Will that work?"), so they need to be said
/// mid-sentence or to somebody ("Will, can you take it?").
const WORD_NAMES: &[&str] = &[
    "april", "art", "august", "bill", "chase", "dawn", "dean", "don", "drew", "faith", "frank",
    "grace", "grant", "hope", "jack", "joy", "june", "mark", "max", "may", "miles", "nick", "pat",
    "ray", "rob", "rose", "sue", "summer", "will",
];

/// The shortest single word trusted as somebody's name on its own.
const MIN_SINGLE_NAME_CHARS: usize = 3;

/// What a finalized meeting offers besides its title.
pub(super) struct Heard<'a> {
    /// Transcript lines as they read now.
    pub lines: &'a [String],
    /// Names the call listed and names speakers already carry, the user's own
    /// excluded.
    pub listed: &'a [String],
}

/// The people this meeting's own words and call roster point at.
pub(super) fn people_heard<'a>(
    people: &'a [Person],
    heard: &Heard<'_>,
    user_name: Option<&str>,
) -> Vec<&'a Person> {
    let mut first_names = HashMap::<String, usize>::new();
    for person in people {
        if let Some(first) = person.display_name.split_whitespace().next() {
            *first_names.entry(normalized(first)).or_default() += 1;
        }
    }
    people
        .iter()
        .filter(|person| !identity_names(person).any(|name| names_user(name, user_name)))
        .filter(|person| {
            let only_first_name = person
                .display_name
                .split_whitespace()
                .next()
                .filter(|first| first_names.get(&normalized(first)) == Some(&1));
            let listed = heard.listed.iter().any(|listed| {
                let listed = normalized(listed);
                identity_names(person).any(|name| normalized(name) == listed)
                    || only_first_name.is_some_and(|first| normalized(first) == listed)
            });
            listed
                || heard.lines.iter().any(|line| {
                    identity_names(person)
                        .chain(only_first_name)
                        .any(|name| said_in(line, name))
                })
        })
        .collect()
}

/// Whether `line` says `name` as a name. A name of several words only has to
/// appear; one word must be capitalised the way a transcript writes a name.
fn said_in(line: &str, name: &str) -> bool {
    let name = name.trim();
    if name.split_whitespace().count() > 1 {
        return exact_phrase_mentioned(line, name);
    }
    if name.chars().count() < MIN_SINGLE_NAME_CHARS {
        return false;
    }
    let wanted = name.to_lowercase();
    let needs_context = WORD_NAMES.contains(&wanted.as_str());
    let words = line.split_whitespace().collect::<Vec<_>>();
    words.iter().enumerate().any(|(index, word)| {
        let bare = word.trim_matches(|character: char| !character.is_alphanumeric());
        let bare = bare
            .strip_suffix("'s")
            .or_else(|| bare.strip_suffix("\u{2019}s"))
            .unwrap_or(bare);
        if bare.to_lowercase() != wanted || !bare.chars().next().is_some_and(char::is_uppercase) {
            return false;
        }
        if !needs_context {
            return true;
        }
        let addressed = word.ends_with(',');
        let opens_sentence = index == 0 || words[index - 1].ends_with(['.', '!', '?']);
        addressed || !opens_sentence
    })
}

/// Suggests a link for every person the meeting's transcript or call roster
/// names. Returns how many links were added or raised.
pub(in crate::meeting::store) fn derive_heard_links_in(
    connection: &Connection,
    meeting_id: MeetingSessionId,
    user_name: Option<&str>,
    now_utc_ms: i64,
) -> Result<usize, StoreError> {
    let people = all_people_in(connection)?;
    if people.is_empty() {
        return Ok(0);
    }
    let meeting = meeting_id.uuid().to_string();
    let lines = strings_in(
        connection,
        "SELECT COALESCE((SELECT e.replacement_text FROM meeting_segment_edits e
                           WHERE e.segment_id = s.segment_id
                           ORDER BY e.edit_sequence DESC LIMIT 1), s.base_text)
           FROM meeting_sessions m
           JOIN meeting_transcript_segments s
             ON s.transcript_revision_id = m.current_transcript_revision_id
          WHERE m.id = ?1
            AND COALESCE((SELECT e.removed FROM meeting_segment_edits e
                           WHERE e.segment_id = s.segment_id
                           ORDER BY e.edit_sequence DESC LIMIT 1), 0) = 0
          ORDER BY s.start_offset_ns, s.ordinal",
        &meeting,
    )?;
    let own_labels = user_speaker_labels_in(connection, meeting_id, user_name)?;
    let mut listed = strings_in(
        connection,
        "SELECT display_name FROM meeting_call_participants
          WHERE session_id = ?1 AND is_local = 0",
        &meeting,
    )?;
    listed.extend(strings_in(
        connection,
        "SELECT display_name FROM meeting_speakers
          WHERE session_id = ?1 AND merged_into_speaker_id IS NULL",
        &meeting,
    )?);
    listed.retain(|name| {
        !is_generated_speaker_label(name)
            && !own_labels
                .iter()
                .any(|label| normalized(label) == normalized(name))
    });
    let heard = Heard {
        lines: &lines,
        listed: &listed,
    };
    let mut changed = 0;
    for person in people_heard(&people, &heard, user_name) {
        changed += usize::from(upsert_link_in(
            connection,
            meeting_id,
            person.id,
            PersonLinkSource::Transcript,
            PersonLinkConfidence::Suggested,
            now_utc_ms,
        )?);
    }
    Ok(changed)
}

fn strings_in(
    connection: &Connection,
    sql: &str,
    meeting: &str,
) -> Result<Vec<String>, StoreError> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement
        .query_map([meeting], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
#[path = "heard_tests.rs"]
mod tests;
