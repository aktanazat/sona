//! Who a meeting was with, read from the meeting itself: names said in the
//! transcript and names the call listed, matched against People.
//!
//! Everything here is a suggestion. A name being said is evidence the person
//! came up, not proof they were in the room, so the user confirms it.

use super::derivation::derive_title_links_for_in;
use super::linking::upsert_link_in;
use crate::meeting::people_types::{Person, PersonId, PersonLinkConfidence, PersonLinkSource};
use crate::meeting::store::loops::user_speaker_labels_in;
use crate::meeting::store::people::{
    all_people_in, exact_phrase_mentioned, identity_names, normalized,
};
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
    for person in people {
        let mut words = identity_names(person).flat_map(words).collect::<Vec<_>>();
        words.sort();
        words.dedup();
        for word in words {
            *holders.entry(word).or_default() += 1;
        }
    }
    let user = user_name.map(normalized).filter(|user| !user.is_empty());
    let user_words = user_name.map(words).unwrap_or_default();
    // A name of several words is its holder's unless it is the user's. One
    // word has to be nobody else's, and no part of the user's name.
    let theirs = |name: &&str| {
        if name.split_whitespace().count() > 1 {
            return user.as_deref() != Some(normalized(name).as_str());
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
            // carries: the entry called what the user is called, or one with
            // the user's full name as an alias. A one-word About me name is
            // only a first name, which a contact may share.
            let is_user = user.as_deref().is_some_and(|user| {
                normalized(&person.display_name) == user
                    || (user_words.len() > 1
                        && person.aliases.iter().any(|alias| normalized(alias) == user))
            });
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
        let opens_sentence = index == 0 || words[index - 1].ends_with(['.', '!', '?']);
        // Followed by a surname ("Dana Okafor") it is somebody else: their
        // full name is matched as a whole, never by its first word.
        let ends_here = !word.ends_with(|character: char| character.is_alphanumeric())
            || word.ends_with("'s")
            || word.ends_with("\u{2019}s");
        if !ends_here
            && words
                .get(index + 1)
                .is_some_and(|next| looks_like_a_surname(next))
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

/// Suggests a link for every person the meeting's transcript or call roster
/// names. Returns how many links were added or raised.
pub(in crate::meeting::store) fn derive_heard_links_in(
    connection: &Connection,
    meeting_id: MeetingSessionId,
    user_name: Option<&str>,
    now_utc_ms: i64,
) -> Result<usize, StoreError> {
    derive_heard_links_for_in(connection, meeting_id, user_name, None, now_utc_ms)
}

/// As [`derive_heard_links_in`], for `only` that person when one is given.
/// Everyone is still read, because a name two people share names neither.
fn derive_heard_links_for_in(
    connection: &Connection,
    meeting_id: MeetingSessionId,
    user_name: Option<&str>,
    only: Option<PersonId>,
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
    for person in people_heard(&people, &heard, user_name)
        .into_iter()
        .filter(|person| only.is_none_or(|only| only == person.id))
    {
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

/// How far back saving a person looks for meetings that named them.
const RECHECK_WINDOW_MS: i64 = 90 * 24 * 60 * 60 * 1000;
/// The most meetings one save re-reads.
const RECHECK_MEETINGS: i64 = 100;

/// What re-reading recent meetings for one person found.
pub(in crate::meeting::store) struct Recheck {
    /// Meetings read.
    pub meetings: usize,
    /// Links added or raised.
    pub changes: usize,
    /// Of the meetings read, how many now wait on the user to confirm this
    /// person.
    pub suggestions: usize,
}

/// Re-reads recent finished meetings for one person, after they were saved
/// under names those meetings may have used. Suggests, never confirms.
pub(in crate::meeting::store) fn recheck_recent_meetings_in(
    connection: &Connection,
    person_id: PersonId,
    user_name: Option<&str>,
    now_utc_ms: i64,
) -> Result<Recheck, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, title FROM meeting_sessions
          WHERE phase = 'review_ready'
            AND COALESCE(started_at_utc_ms, created_at_utc_ms) >= ?1
          ORDER BY COALESCE(started_at_utc_ms, created_at_utc_ms) DESC
          LIMIT ?2",
    )?;
    let recent = statement
        .query_map(
            rusqlite::params![now_utc_ms - RECHECK_WINDOW_MS, RECHECK_MEETINGS],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let mut found = Recheck {
        meetings: recent.len(),
        changes: 0,
        suggestions: 0,
    };
    for (meeting, title) in recent {
        let meeting_id = MeetingSessionId::from_uuid(
            uuid::Uuid::parse_str(&meeting).map_err(|_| StoreError::Corrupt)?,
        );
        found.changes +=
            derive_title_links_for_in(connection, meeting_id, &title, Some(person_id), now_utc_ms)?
                + derive_heard_links_for_in(
                    connection,
                    meeting_id,
                    user_name,
                    Some(person_id),
                    now_utc_ms,
                )?;
        found.suggestions += usize::from(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM meeting_person_links
              WHERE meeting_id = ?1 AND person_id = ?2 AND confidence = 'suggested')",
            rusqlite::params![meeting, person_id.uuid().to_string()],
            |row| row.get::<_, bool>(0),
        )?);
    }
    Ok(found)
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
