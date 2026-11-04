mod avatars;
mod contact_cards;
mod continuity;
mod contributions;
mod mutations;
mod queries;
mod vocabulary;

pub(super) use mutations::{
    derive_calendar_links_in, derive_speaker_link_in, derive_title_links_in,
    link_document_mentions_in, recompute_organizations_in, upsert_link_in,
};
pub(super) use queries::{calendar_context_in, continuity_summary_in};
pub(super) use vocabulary::vocabulary_candidates_in;

use super::{MeetingStore, StoreError};
use crate::meeting::people_types::{Person, PersonId, PersonSummary};
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashSet;
use uuid::Uuid;

pub(super) const SCHEMA_VERSION: u32 = 2;

pub(super) fn people_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    let revision: i64 = connection.query_row(
        "SELECT revision FROM people_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(revision).map_err(|_| StoreError::Corrupt)
}

pub(super) fn require_people_revision_in(
    connection: &Connection,
    expected_revision: u64,
) -> Result<(), StoreError> {
    if people_revision_in(connection)? != expected_revision {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

pub(super) fn bump_people_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    connection.execute(
        "UPDATE people_state SET revision = revision + 1 WHERE singleton = 1",
        [],
    )?;
    people_revision_in(connection)
}

pub(super) fn person_by_id_in(
    connection: &Connection,
    person_id: PersonId,
) -> Result<Person, StoreError> {
    connection
        .query_row(
            "SELECT id, display_name, aliases_json, calendar_emails_json,
                    organization, created_at_utc_ms, updated_at_utc_ms,
                    summary, summary_generated_at_utc_ms, summary_model_id
               FROM persons WHERE id = ?1",
            [person_id.uuid().to_string()],
            person_from_row,
        )
        .optional()?
        .ok_or(StoreError::NotFound)
}

pub(super) fn all_people_in(connection: &Connection) -> Result<Vec<Person>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, display_name, aliases_json, calendar_emails_json,
                organization, created_at_utc_ms, updated_at_utc_ms,
                summary, summary_generated_at_utc_ms, summary_model_id
           FROM persons ORDER BY display_name COLLATE NOCASE, id",
    )?;
    let people = statement
        .query_map([], person_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(people)
}
impl MeetingStore {
    pub(crate) fn resolve_person_source(
        &self,
        source_id: PersonId,
    ) -> Result<Option<Person>, StoreError> {
        let connection = self.connection()?;
        let target: Option<String> = connection
            .query_row(
                "SELECT target_person_id FROM person_merge_redirects WHERE source_person_id = ?1",
                [source_id.uuid().to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let resolved = target
            .as_deref()
            .map(Uuid::parse_str)
            .transpose()
            .map_err(|_| StoreError::Corrupt)?
            .map(PersonId)
            .unwrap_or(source_id);
        match person_by_id_in(&connection, resolved) {
            Ok(person) => Ok(Some(person)),
            Err(StoreError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn person_source_ids(&self, person_id: PersonId) -> Result<Vec<String>, StoreError> {
        let connection = self.connection()?;
        let mut source_ids = vec![person_id.uuid().to_string()];
        let mut statement = connection.prepare(
            "SELECT source_person_id FROM person_merge_redirects WHERE target_person_id = ?1",
        )?;
        let merged = statement.query_map([person_id.uuid().to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        source_ids.extend(merged.collect::<rusqlite::Result<Vec<_>>>()?);
        Ok(source_ids)
    }
}

fn person_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Person> {
    let id: String = row.get(0)?;
    let aliases_json: String = row.get(2)?;
    let calendar_emails_json: String = row.get(3)?;
    let id = Uuid::parse_str(&id).map_err(|error| conversion_error(0, error))?;
    let aliases =
        serde_json::from_str(&aliases_json).map_err(|error| conversion_error(2, error))?;
    let calendar_emails =
        serde_json::from_str(&calendar_emails_json).map_err(|error| conversion_error(3, error))?;
    Ok(Person {
        id: PersonId(id),
        display_name: row.get(1)?,
        aliases,
        calendar_emails,
        organization: row.get(4)?,
        summary: person_summary_from_row(row)?,
        created_at_utc_ms: row.get(5)?,
        updated_at_utc_ms: row.get(6)?,
    })
}

/// The three summary columns as one value, or `None` while the person has no
/// paragraph. A row with some of the three present and some absent is a state
/// the writer cannot produce, so it reads as no summary rather than as a
/// corruption a person's page would refuse to load over.
fn person_summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<PersonSummary>> {
    let text: Option<String> = row.get(7)?;
    let generated_at_utc_ms: Option<i64> = row.get(8)?;
    let model_id: Option<String> = row.get(9)?;
    Ok(match (text, generated_at_utc_ms, model_id) {
        (Some(text), Some(generated_at_utc_ms), Some(model_id)) => Some(PersonSummary {
            text,
            generated_at_utc_ms,
            model_id,
        }),
        _ => None,
    })
}

fn conversion_error(
    column: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(error))
}

pub(super) fn normalized(value: &str) -> String {
    value
        .split_whitespace()
        .flat_map(str::chars)
        .flat_map(char::to_lowercase)
        .collect()
}

pub(super) fn normalized_email(value: &str) -> String {
    value.trim().to_lowercase()
}

pub(super) fn identity_names(person: &Person) -> impl Iterator<Item = &str> {
    std::iter::once(person.display_name.as_str()).chain(person.aliases.iter().map(String::as_str))
}

pub(super) fn owner_matches(person: &Person, owner: &str) -> bool {
    let owner = normalized(owner);
    !owner.is_empty() && identity_names(person).any(|name| normalized(name) == owner)
}

pub(super) fn exact_phrase_mentioned(text: &str, phrase: &str) -> bool {
    let phrase = phrase_tokens(phrase);
    if phrase.is_empty() {
        return false;
    }
    let text = phrase_tokens(text);
    text.windows(phrase.len()).any(|window| window == phrase)
}

fn phrase_tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

pub(super) fn merge_unique_case_insensitive(
    target: &mut Vec<String>,
    values: impl IntoIterator<Item = String>,
) -> bool {
    let mut seen = target
        .iter()
        .map(|value| normalized(value))
        .collect::<HashSet<_>>();
    let mut changed = false;
    for value in values {
        let value = value.trim().to_string();
        let key = normalized(&value);
        if !key.is_empty() && seen.insert(key) {
            target.push(value);
            changed = true;
        }
    }
    changed
}
