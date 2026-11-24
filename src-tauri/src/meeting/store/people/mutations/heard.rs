//! Who a meeting was with, read from the meeting itself: names said in the
//! transcript and names the call listed, matched against People.
//!
//! Everything here is a suggestion. A name being said is evidence the person
//! came up, not proof they were in the room, so the user confirms it.

use super::linking::upsert_link_in;
use crate::meeting::people_types::{Person, PersonLinkConfidence, PersonLinkSource};
use crate::meeting::store::loops::user_speaker_labels_in;
use crate::meeting::store::people::{all_people_in, identity_names, normalized};
use crate::meeting::store::voice_identity::is_generated_speaker_label;
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

/// Days and months, capitalised wherever they fall in a sentence.
const CALENDAR_WORDS: &[&str] = &[
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
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
    // How many people each word belongs to, anywhere in any of their names.
    // A word on its own that could mean two people names neither.
    let mut holders = HashMap::<String, usize>::new();
    // How many people carry each name of several words, as a whole.
    let mut full_holders = HashMap::<String, usize>::new();
    for person in people {
        let mut words = identity_names(person).flat_map(words).collect::<Vec<_>>();
        words.sort();
        words.dedup();
        for word in words {
            *holders.entry(word).or_default() += 1;
        }
        let mut full = identity_names(person)
            .filter(|name| name.split_whitespace().count() > 1)
            .map(normalized)
            .collect::<Vec<_>>();
        full.sort();
        full.dedup();
        for name in full {
            *full_holders.entry(name).or_default() += 1;
        }
    }
    let user = user_name.map(normalized).filter(|user| !user.is_empty());
    let user_words = user_name.map(words).unwrap_or_default();
    // A name of several words is its holder's unless it is the user's. One
    // word has to be nobody else's, and no part of the user's name.
    let theirs = |name: &&str| {
        if name.split_whitespace().count() > 1 {
            let full = normalized(name);
            return user.as_deref() != Some(full.as_str()) && full_holders.get(&full) == Some(&1);
        }
        let word = name.trim().to_lowercase();
        !user_words.contains(&word)
            && user.as_deref() != Some(word.as_str())
            && holders.get(&word) == Some(&1)
    };
    people
        .iter()
        .filter(|person| {
            // The user's own entry is nobody else, whatever nickname it also
            // carries: the entry called what the user is called. A one-word
            // About me name is only a first name, which a contact may share.
            let is_user = user
                .as_deref()
                .is_some_and(|user| normalized(&person.display_name) == user);
            if is_user {
                return false;
            }
            let names = || {
                identity_names(person)
                    .chain(person.display_name.split_whitespace().next())
                    .filter(theirs)
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

fn words(name: &str) -> Vec<String> {
    name.split_whitespace().map(str::to_lowercase).collect()
}

/// Whether `word`, coming straight after a first name, reads as the rest of
/// that name: capitalised, and not a word transcripts capitalise anyway.
fn looks_like_a_surname(word: &str) -> bool {
    let bare = word.trim_matches(|character: char| !character.is_alphanumeric());
    let shouted = bare.chars().count() > 1 && !bare.chars().any(char::is_lowercase);
    bare.chars().next().is_some_and(char::is_uppercase)
        && !shouted
        && bare != "I"
        && !bare.starts_with("I'")
        && !bare.starts_with("I\u{2019}")
        && !CALENDAR_WORDS.contains(&bare.to_lowercase().as_str())
}

/// Whether `line` says `name` as a name. A name of several words has to be
/// said whole and not run into a surname; one word must be capitalised the
/// way a transcript writes a name.
fn said_in(line: &str, name: &str) -> bool {
    let name = name.trim();
    if name.split_whitespace().count() > 1 {
        return full_name_said_in(line, name);
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
        let opens_sentence = index == 0 || words[index - 1].ends_with(['.', '!', '?']);
        // A name a surname comes before ("Marcus Dana"), or one a surname comes
        // after ("Dana Okafor"), is somebody else: their full name is matched
        // as a whole, never by its first word.
        let surname_before = index >= 2
            && !words[index - 2].ends_with(['.', '!', '?'])
            && words[index - 1].ends_with(char::is_alphanumeric)
            && looks_like_a_surname(words[index - 1]);
        let ends_here = !word.ends_with(|character: char| character.is_alphanumeric())
            || word.ends_with("'s")
            || word.ends_with("\u{2019}s");
        if surname_before
            || (!ends_here
                && words
                    .get(index + 1)
                    .is_some_and(|next| looks_like_a_surname(next)))
        {
            return false;
        }
        if !needs_context {
            return true;
        }
        let addressed = word.ends_with(',');
        addressed || !opens_sentence
    })
}

/// Whether `line` says every word of `name`, in order, with no capitalised
/// surname straight after ("Tess Nolan Junior" is not Tess Nolan).
fn full_name_said_in(line: &str, name: &str) -> bool {
    let wanted = name_tokens(name)
        .into_iter()
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    let said = name_tokens(line);
    !wanted.is_empty()
        && said
            .windows(wanted.len())
            .enumerate()
            .any(|(index, window)| {
                window
                    .iter()
                    .map(|word| word.to_lowercase())
                    .eq(wanted.iter().cloned())
                    && !said
                        .get(index + wanted.len())
                        .is_some_and(|next| looks_like_a_surname(next))
            })
}

fn name_tokens(text: &str) -> Vec<&str> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect()
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
