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
use crate::meeting::store::{effective_segments_for_session, StoreError};
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
    // How many people each single word could mean: as a first name, or as a
    // one-word name or alias. A word that could mean two people names neither.
    let mut single_words = HashMap::<String, usize>::new();
    for person in people {
        let mut words = identity_names(person)
            .filter(|name| name.split_whitespace().count() == 1)
            .chain(person.display_name.split_whitespace().next())
            .map(normalized)
            .collect::<Vec<_>>();
        words.sort();
        words.dedup();
        for word in words {
            *single_words.entry(word).or_default() += 1;
        }
    }
    let unshared = |name: &&str| {
        name.split_whitespace().count() > 1 || single_words.get(&normalized(name)) == Some(&1)
    };
    people
        .iter()
        .filter(|person| {
            // An entry called what the user is called is the user, whatever
            // nickname it also carries.
            if user_name.is_some_and(|user| normalized(&person.display_name) == normalized(user)) {
                return false;
            }
            // Any other name the user also answers to is dropped on its own,
            // so a contact who shares it is still found by the rest of
            // theirs.
            let names = || {
                identity_names(person)
                    .chain(person.display_name.split_whitespace().next())
                    .filter(|name| !names_user(name, user_name))
                    .filter(unshared)
            };
            heard.listed.iter().any(|listed| {
                let listed = normalized(listed);
                names().any(|name| normalized(name) == listed)
            }) || heard
                .lines
                .iter()
                .any(|line| names().any(|name| said_in(line, name)))
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
    // The transcript as Review shows it: edits and learned corrections applied.
    let lines = effective_segments_for_session(connection, meeting_id)?
        .into_iter()
        .filter(|segment| !segment.removed)
        .map(|segment| segment.replacement_text.unwrap_or(segment.base.text))
        .collect::<Vec<_>>();
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
