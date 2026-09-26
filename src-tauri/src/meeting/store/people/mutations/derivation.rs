use super::encode_strings;
use super::linking::upsert_link_in;
use crate::meeting::detection::machine::{CalendarAttendee, CalendarEventSummary};
use crate::meeting::document_types::DocumentId;
use crate::meeting::people_types::{Person, PersonId, PersonLinkConfidence, PersonLinkSource};
use crate::meeting::store::loops::microphone_speaker_labels_in;
use crate::meeting::store::people::{
    all_people_in, exact_phrase_mentioned, identity_names, merge_unique_case_insensitive,
    normalized, normalized_email, person_by_id_in,
};
use crate::meeting::store::StoreError;
use crate::meeting::types::MeetingSessionId;
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};

pub(in crate::meeting::store) fn derive_calendar_links_in(
    connection: &Connection,
    meeting_id: MeetingSessionId,
    attendees: &[CalendarAttendee],
    now_utc_ms: i64,
) -> Result<usize, StoreError> {
    let mut changed = 0;
    for attendee in attendees.iter().filter(|attendee| !attendee.is_self) {
        let Some(email) = attendee.email.as_deref().map(normalized_email) else {
            continue;
        };
        if email.is_empty() {
            continue;
        }
        let person = find_person_by_email_in(connection, &email)?.unwrap_or_else(|| Person {
            id: PersonId::new(),
            display_name: attendee.name.trim().to_string(),
            aliases: Vec::new(),
            calendar_emails: vec![email.clone()],
            organization: None,
            summary: None,
            created_at_utc_ms: now_utc_ms,
            updated_at_utc_ms: now_utc_ms,
        });
        let (person, identity_changed) =
            save_calendar_identity_in(connection, person, attendee, &email, now_utc_ms)?;
        changed += usize::from(identity_changed);
        changed += usize::from(upsert_link_in(
            connection,
            meeting_id,
            person.id,
            PersonLinkSource::Calendar,
            PersonLinkConfidence::Confirmed,
            now_utc_ms,
        )?);
    }
    Ok(changed)
}

pub(in crate::meeting::store) fn recompute_organizations_in(
    connection: &Connection,
) -> Result<usize, StoreError> {
    let mut changed = 0;
    for person in all_people_in(connection)? {
        let organization = organization_for_person_in(connection, &person)?;
        if organization == person.organization {
            continue;
        }
        connection.execute(
            "UPDATE persons SET organization = ?1 WHERE id = ?2",
            params![organization, person.id.uuid().to_string()],
        )?;
        changed += 1;
    }
    Ok(changed)
}

fn organization_for_person_in(
    connection: &Connection,
    person: &Person,
) -> Result<Option<String>, StoreError> {
    let emails = person
        .calendar_emails
        .iter()
        .map(|email| normalized_email(email))
        .collect::<HashSet<_>>();
    if emails.is_empty() {
        return Ok(None);
    }
    let mut statement = connection.prepare(
        "SELECT f.event_json
           FROM meeting_person_links l
           JOIN meeting_calendar_facts f ON f.session_id = l.meeting_id
          WHERE l.person_id = ?1",
    )?;
    let facts = statement
        .query_map([person.id.uuid().to_string()], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut scores = HashMap::<String, (u64, i64)>::new();
    for event_json in facts {
        let event: CalendarEventSummary =
            serde_json::from_str(&event_json).map_err(|_| StoreError::Corrupt)?;
        for email in event
            .attendees
            .iter()
            .filter_map(|attendee| attendee.email.as_deref())
            .filter(|email| emails.contains(&normalized_email(email)))
        {
            let Some(organization) = organization_from_email(email) else {
                continue;
            };
            let score = scores
                .entry(organization)
                .or_insert((0, event.start_utc_ms));
            score.0 += 1;
            score.1 = score.1.max(event.start_utc_ms);
        }
    }
    Ok(scores
        .into_iter()
        .max_by(
            |(left_name, (left_count, left_newest)), (right_name, (right_count, right_newest))| {
                left_count
                    .cmp(right_count)
                    .then_with(|| left_newest.cmp(right_newest))
                    .then_with(|| left_name.cmp(right_name))
            },
        )
        .map(|(organization, _)| organization))
}

const PUBLIC_EMAIL_DOMAINS: &[&str] = &[
    "126.com",
    "163.com",
    "aol.com",
    "fastmail.com",
    "gmail.com",
    "gmx.com",
    "gmx.de",
    "googlemail.com",
    "hey.com",
    "hotmail.com",
    "icloud.com",
    "live.com",
    "mac.com",
    "mail.com",
    "mail.ru",
    "me.com",
    "msn.com",
    "outlook.com",
    "pm.me",
    "proton.me",
    "protonmail.com",
    "qq.com",
    "tuta.com",
    "tutamail.com",
    "tutanota.com",
    "yahoo.com",
    "yandex.com",
    "yandex.ru",
    "zoho.com",
];

fn organization_from_email(email: &str) -> Option<String> {
    let domain = email
        .trim()
        .rsplit_once('@')?
        .1
        .trim_end_matches('.')
        .to_lowercase();
    let registrable = psl::domain_str(&domain)?;
    if PUBLIC_EMAIL_DOMAINS.binary_search(&registrable).is_ok()
        || registrable.starts_with("yahoo.")
        || registrable.starts_with("gmx.")
        || registrable.starts_with("yandex.")
    {
        return None;
    }
    let mut label = registrable.split('.').next()?.chars();
    let first = label.next()?;
    Some(
        first
            .to_uppercase()
            .chain(label.flat_map(char::to_lowercase))
            .collect(),
    )
}

impl crate::meeting::store::MeetingStore {
    /// Reuse the calendar identity rule without exposing addresses to research.
    pub(crate) fn organization_from_email(email: &str) -> Option<String> {
        organization_from_email(email)
    }
}

/// Derive a person from a named speaker in one meeting.
///
/// Two bars, on two different axes, and both are load-bearing.
///
/// One name-shaped: a bare first name is not specific enough to mint an
/// identity the rest of the corpus will be keyed to.
///
/// One voice-shaped: a microphone speaker is the user. `loop_types` states the
/// invariant — Sona models no `Person` for its own user, so a `PersonId` is
/// always somebody else — and `microphone_speaker_labels_in` is already the
/// whole answer to which voice that is. Reused here rather than re-derived, so
/// this and `loop_direction` cannot disagree about it: a person minted from the
/// user's own renamed voice would carry a `PersonId`, and `loop_direction`
/// treats an explicit owner as settling the question *because* a person is
/// somebody else — flipping what the user owes into what they are waiting on.
/// `Local speaker`, the label the store writes for that same row, clears the
/// two-word bar on its own.
pub(in crate::meeting::store) fn derive_speaker_link_in(
    connection: &Connection,
    meeting_id: MeetingSessionId,
    display_name: &str,
    now_utc_ms: i64,
) -> Result<usize, StoreError> {
    let display_name = display_name.trim();
    if display_name.split_whitespace().count() < 2 {
        return Ok(0);
    }
    if microphone_speaker_labels_in(connection, meeting_id)?
        .iter()
        .any(|label| normalized(label) == normalized(display_name))
    {
        return Ok(0);
    }
    let mut matches = find_people_by_name_in(connection, display_name)?;
    if matches.len() > 1 {
        return Ok(0);
    }
    let person = matches.pop().unwrap_or_else(|| Person {
        id: PersonId::new(),
        display_name: display_name.to_string(),
        aliases: Vec::new(),
        calendar_emails: Vec::new(),
        organization: None,
        summary: None,
        created_at_utc_ms: now_utc_ms,
        updated_at_utc_ms: now_utc_ms,
    });
    let identity_changed = insert_person_if_missing_in(connection, &person)?;
    let link_changed = upsert_link_in(
        connection,
        meeting_id,
        person.id,
        PersonLinkSource::Speaker,
        PersonLinkConfidence::Confirmed,
        now_utc_ms,
    )?;
    Ok(usize::from(identity_changed) + usize::from(link_changed))
}

pub(in crate::meeting::store) fn derive_title_links_in(
    connection: &Connection,
    meeting_id: MeetingSessionId,
    title: &str,
    now_utc_ms: i64,
) -> Result<usize, StoreError> {
    let people = all_people_in(connection)?;
    let mut first_names = HashMap::<String, usize>::new();
    for person in &people {
        if let Some(first) = person.display_name.split_whitespace().next() {
            *first_names.entry(normalized(first)).or_default() += 1;
        }
    }
    let mut changed = 0;
    for person in people {
        let full_match = identity_names(&person).any(|name| exact_phrase_mentioned(title, name));
        let first_match = person
            .display_name
            .split_whitespace()
            .next()
            .is_some_and(|first| {
                first_names.get(&normalized(first)) == Some(&1)
                    && exact_phrase_mentioned(title, first)
            });
        if full_match || first_match {
            changed += usize::from(upsert_link_in(
                connection,
                meeting_id,
                person.id,
                PersonLinkSource::Title,
                PersonLinkConfidence::Suggested,
                now_utc_ms,
            )?);
        }
    }
    Ok(changed)
}

pub(in crate::meeting::store) fn link_document_mentions_in(
    connection: &Connection,
    document_id: DocumentId,
    content: &str,
    now_utc_ms: i64,
) -> Result<usize, StoreError> {
    let mut changed = 0;
    for person in all_people_in(connection)? {
        if identity_names(&person).any(|name| exact_phrase_mentioned(content, name)) {
            changed += connection.execute(
                "INSERT OR IGNORE INTO document_person_links
                    (document_id, person_id, created_at_utc_ms) VALUES (?1, ?2, ?3)",
                params![
                    document_id.uuid().to_string(),
                    person.id.uuid().to_string(),
                    now_utc_ms
                ],
            )?;
        }
    }
    Ok(changed)
}

fn insert_person_if_missing_in(
    connection: &Connection,
    person: &Person,
) -> Result<bool, StoreError> {
    let changed = connection.execute(
        "INSERT OR IGNORE INTO persons (
            id, display_name, aliases_json, calendar_emails_json,
            created_at_utc_ms, updated_at_utc_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            person.id.uuid().to_string(),
            person.display_name,
            encode_strings(&person.aliases)?,
            encode_strings(&person.calendar_emails)?,
            person.created_at_utc_ms,
            person.updated_at_utc_ms
        ],
    )? != 0;
    Ok(changed)
}

fn find_person_by_email_in(
    connection: &Connection,
    email: &str,
) -> Result<Option<Person>, StoreError> {
    Ok(all_people_in(connection)?.into_iter().find(|person| {
        person
            .calendar_emails
            .iter()
            .any(|candidate| normalized_email(candidate) == email)
    }))
}

fn find_people_by_name_in(connection: &Connection, name: &str) -> Result<Vec<Person>, StoreError> {
    let name = normalized(name);
    Ok(all_people_in(connection)?
        .into_iter()
        .filter(|person| identity_names(person).any(|candidate| normalized(candidate) == name))
        .collect())
}

fn save_calendar_identity_in(
    connection: &Connection,
    mut person: Person,
    attendee: &CalendarAttendee,
    email: &str,
    now_utc_ms: i64,
) -> Result<(Person, bool), StoreError> {
    if person_by_id_in(connection, person.id).is_err() {
        if person.display_name.is_empty() {
            person.display_name = email.to_string();
        }
        let changed = insert_person_if_missing_in(connection, &person)?;
        return Ok((person, changed));
    }
    let mut changed =
        merge_unique_case_insensitive(&mut person.calendar_emails, [email.to_string()]);
    if !attendee.name.trim().is_empty()
        && normalized(&person.display_name) != normalized(&attendee.name)
    {
        changed |= merge_unique_case_insensitive(&mut person.aliases, [attendee.name.clone()]);
    }
    if changed {
        connection.execute(
            "UPDATE persons SET aliases_json = ?1, calendar_emails_json = ?2,
                    updated_at_utc_ms = ?3 WHERE id = ?4",
            params![
                encode_strings(&person.aliases)?,
                encode_strings(&person.calendar_emails)?,
                now_utc_ms,
                person.id.uuid().to_string()
            ],
        )?;
    }
    Ok((person, changed))
}
