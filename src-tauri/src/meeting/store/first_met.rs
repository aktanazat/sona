//! When the user first met a person on record: the earliest meeting the
//! person is confirmed in, the place that meeting names, and who introduced
//! them when its lines say so. The introducer waits in `filing_suggestions`
//! as a maybe and is named as fact only once the user takes it.

use super::context::context_in;
use super::evening::insert_suggestion_in;
use super::people::{identity_names, person_by_id_in};
use super::transcript_corrections::confirmed_person_for_speaker_in;
use super::{
    canonical_speaker_ids, decode_json, effective_segments_for_session, encode_json, id,
    parse_uuid, MeetingStore, StoreError,
};
use crate::meeting::evening::{FilingReason, FilingTarget};
use crate::meeting::people_types::{Person, PersonFirstMet, PersonId, PersonIntroduction};
use crate::meeting::types::{
    EffectiveTranscriptSegment, MeetingSessionId, SpeakerAssignmentKind, SpeakerId,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashMap;

/// Words that open an introduction when a name follows them.
const OPENINGS: &[&[&str]] = &[
    &["this", "is"],
    &["meet"],
    &["introduce", "you", "to"],
    &["introduce"],
    &["introducing"],
    &["say", "hi", "to"],
    &["say", "hello", "to"],
];

/// Words that start a description before the name: "my friend Azamat".
const DESCRIBERS: &[&str] = &["my", "our"];

/// A description is one or two words after its describer.
const DESCRIPTION_WORDS: std::ops::RangeInclusive<usize> = 2..=3;

impl MeetingStore {
    pub(crate) fn person_first_met(
        &self,
        person_id: PersonId,
        now_utc_ms: i64,
    ) -> Result<Option<PersonFirstMet>, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let first_met = first_met_in(&transaction, person_id, now_utc_ms)?;
        transaction.commit()?;
        Ok(first_met)
    }
}

fn first_met_in(
    connection: &Connection,
    person_id: PersonId,
    now_utc_ms: i64,
) -> Result<Option<PersonFirstMet>, StoreError> {
    let person = person_by_id_in(connection, person_id)?;
    let first = connection
        .query_row(
            "SELECT m.id, COALESCE(m.started_at_utc_ms, m.created_at_utc_ms) AS at_utc_ms
               FROM meeting_person_links l
               JOIN meeting_sessions m ON m.id = l.meeting_id
              WHERE l.person_id = ?1 AND l.confidence = 'confirmed' AND m.phase != 'deleting'
              ORDER BY at_utc_ms, m.id
              LIMIT 1",
            [id(person_id)],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    let Some((meeting_id, at_utc_ms)) = first else {
        return Ok(None);
    };
    let meeting_id = MeetingSessionId::from_uuid(parse_uuid(&meeting_id)?);
    let place = context_in(connection, meeting_id)?
        .location
        .map(|location| location.name);
    let segments = effective_segments_for_session(connection, meeting_id)?;
    let heard = heard_introduction_in(connection, meeting_id, &person, &segments)?;
    if let Some((target, reason)) = &heard {
        offer_in(connection, meeting_id, target, reason, now_utc_ms)?;
    }
    let introduction = introduction_in(
        connection,
        meeting_id,
        person_id,
        &segments,
        heard.is_some(),
    )?;
    Ok(Some(PersonFirstMet {
        meeting_id,
        at_utc_ms,
        place,
        introduction,
    }))
}

/// The first line in which someone the user confirmed in this meeting
/// introduces `person`. A line the person says of themselves, or one from a
/// voice nobody confirmed, such as the user's own, introduces nobody.
fn heard_introduction_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    person: &Person,
    segments: &[EffectiveTranscriptSegment],
) -> Result<Option<(FilingTarget, FilingReason)>, StoreError> {
    let names = name_forms(person);
    let canonical = canonical_speaker_ids(connection, session_id)?;
    let mut speakers: HashMap<SpeakerId, Option<PersonId>> = HashMap::new();
    for segment in segments {
        if segment.removed
            || !matches!(
                segment.speaker_assignment,
                SpeakerAssignmentKind::LocalSpeaker | SpeakerAssignmentKind::SystemSpeaker
            )
        {
            continue;
        }
        let text = segment
            .replacement_text
            .as_deref()
            .unwrap_or(&segment.base.text)
            .trim();
        if !introduces(text, &names) {
            continue;
        }
        let speaker_id = canonical
            .get(&segment.assigned_speaker_id)
            .copied()
            .unwrap_or(segment.assigned_speaker_id);
        let speaker = match speakers.get(&speaker_id) {
            Some(speaker) => *speaker,
            None => {
                let speaker = confirmed_person_for_speaker_in(connection, session_id, speaker_id)?;
                speakers.insert(speaker_id, speaker);
                speaker
            }
        };
        let Some(introducer_id) = speaker.filter(|speaker| *speaker != person.id) else {
            continue;
        };
        let introducer = person_by_id_in(connection, introducer_id)?;
        return Ok(Some((
            FilingTarget::Introducer {
                person_id: person.id,
                introducer_id,
                name: introducer.display_name,
            },
            FilingReason::Said {
                phrase: text.to_string(),
                count: 1,
                segment_id: segment.base.segment_id,
            },
        )));
    }
    Ok(None)
}

/// Leaves the introduction waiting, or brings a waiting one up to what the
/// lines say now. One the user took or turned down stays as it is.
fn offer_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    target: &FilingTarget,
    reason: &FilingReason,
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    let FilingTarget::Introducer { person_id, .. } = target else {
        return Err(StoreError::Invalid);
    };
    let reasons = std::slice::from_ref(reason);
    insert_suggestion_in(connection, session_id, target, reasons, now_utc_ms)?;
    connection.execute(
        "UPDATE filing_suggestions SET target_json = ?3, reasons_json = ?4
          WHERE session_id = ?1 AND target_kind = 'introducer' AND target_key = ?2
            AND state = 'pending' AND (target_json != ?3 OR reasons_json != ?4)",
        params![
            id(session_id),
            id(*person_id),
            encode_json(target)?,
            encode_json(reasons)?
        ],
    )?;
    Ok(())
}

/// The introduction to show: one the user took, or one waiting while the
/// lines still say it. The introducer goes by their name today.
fn introduction_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    person_id: PersonId,
    segments: &[EffectiveTranscriptSegment],
    heard: bool,
) -> Result<Option<PersonIntroduction>, StoreError> {
    let row = connection
        .query_row(
            "SELECT id, target_json, reasons_json, state FROM filing_suggestions
              WHERE session_id = ?1 AND target_kind = 'introducer' AND target_key = ?2",
            params![id(session_id), id(person_id)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((suggestion_id, target, reasons, state)) = row else {
        return Ok(None);
    };
    let confirmed = match state.as_str() {
        "accepted" => true,
        "pending" if heard => false,
        "pending" | "declined" => return Ok(None),
        _ => return Err(StoreError::Corrupt),
    };
    let FilingTarget::Introducer { introducer_id, .. } = decode_json(&target)? else {
        return Err(StoreError::Corrupt);
    };
    let reasons: Vec<FilingReason> = decode_json(&reasons)?;
    let [FilingReason::Said {
        phrase, segment_id, ..
    }] = reasons.as_slice()
    else {
        return Err(StoreError::Corrupt);
    };
    let introducer_name = connection
        .query_row(
            "SELECT display_name FROM persons
              WHERE id = COALESCE((SELECT target_person_id FROM person_merge_redirects
                                    WHERE source_person_id = ?1), ?1)",
            [id(introducer_id)],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(introducer_name) = introducer_name else {
        return Ok(None);
    };
    let segment_id = segments
        .iter()
        .any(|segment| segment.base.segment_id == *segment_id && !segment.removed)
        .then_some(*segment_id);
    Ok(Some(PersonIntroduction {
        suggestion_id,
        introducer_name,
        quote: phrase.clone(),
        segment_id,
        confirmed,
    }))
}

/// Each name the person answers to, as words, longest first: every full
/// name and alias, and the first word of each, so "Azamat" is Azamat Bekov.
fn name_forms(person: &Person) -> Vec<Vec<String>> {
    let mut forms = Vec::new();
    for name in identity_names(person) {
        let name = words(name);
        if let Some(first) = name.first().filter(|first| first.chars().count() >= 2) {
            forms.push(vec![first.clone()]);
        }
        if !name.is_empty() {
            forms.push(name);
        }
    }
    forms.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    forms.dedup();
    forms
}

/// Whether a line introduces someone by one of `names`: an opening followed
/// by the name, or by a description the name closes, clause by clause.
fn introduces(text: &str, names: &[Vec<String>]) -> bool {
    text.split(|character: char| ",.;:!?()\"“”—–".contains(character))
        .any(|clause| {
            let words = words(clause);
            (0..words.len()).any(|start| {
                OPENINGS.iter().any(|opening| {
                    begins_with(&words[start..], opening)
                        && named_next(&words[start + opening.len()..], names)
                })
            })
        })
}

fn named_next(rest: &[String], names: &[Vec<String>]) -> bool {
    names.iter().any(|name| {
        rest.starts_with(name)
            || (rest
                .first()
                .is_some_and(|word| DESCRIBERS.contains(&word.as_str()))
                && DESCRIPTION_WORDS
                    .clone()
                    .any(|skip| rest.len() == skip + name.len() && rest[skip..] == name[..]))
    })
}

fn begins_with(words: &[String], opening: &[&str]) -> bool {
    words.len() >= opening.len() && words.iter().zip(opening).all(|(word, open)| word == open)
}

/// Lower-cased words, each keeping an apostrophe inside it so "Azamat's" is
/// never "Azamat".
fn words(text: &str) -> Vec<String> {
    text.split(|character: char| {
        !(character.is_alphanumeric() || character == '\'' || character == '’')
    })
    .map(|word| word.replace('’', "'").trim_matches('\'').to_lowercase())
    .filter(|word| !word.is_empty())
    .collect()
}
