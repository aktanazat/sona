use super::mutations::{encode_strings, mutation_result};
use super::{
    all_people_in, bump_people_revision_in, merge_unique_case_insensitive, normalized,
    normalized_email, people_revision_in, person_by_id_in,
};
use crate::meeting::people_types::{PeopleMutationResult, PersonId};
use crate::meeting::store::{MeetingStore, StoreError};
use rusqlite::{params, Connection, TransactionBehavior};

/// What a person's profile is set to. A field left `None` keeps what the
/// person has; an empty string or list clears it.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub(crate) struct PersonProfileEdit {
    pub display_name: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub role: Option<String>,
    pub organization: Option<String>,
    pub calendar_emails: Option<Vec<String>>,
    pub notes: Option<String>,
}

fn present(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn clean_list(values: Vec<String>, key: fn(&str) -> String) -> Vec<String> {
    let mut kept = Vec::new();
    merge_unique_case_insensitive(
        &mut kept,
        values
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !key(value).is_empty()),
    );
    kept
}

fn taken_by_other(
    connection: &Connection,
    person_id: Option<PersonId>,
    names: &[String],
) -> Result<bool, StoreError> {
    let wanted: Vec<String> = names.iter().map(|name| normalized(name)).collect();
    Ok(all_people_in(connection)?
        .into_iter()
        .filter(|person| Some(person.id) != person_id)
        .any(|person| {
            std::iter::once(&person.display_name)
                .chain(person.aliases.iter())
                .any(|name| wanted.contains(&normalized(name)))
        }))
}

impl MeetingStore {
    pub(crate) fn create_person_profile(
        &self,
        display_name: String,
        edit: PersonProfileEdit,
        now_utc_ms: i64,
    ) -> Result<PeopleMutationResult, StoreError> {
        let display_name = display_name.trim().to_string();
        if normalized(&display_name).is_empty() {
            return Err(StoreError::Invalid);
        }
        let mut aliases = clean_list(edit.aliases.unwrap_or_default(), normalized);
        aliases.retain(|alias| normalized(alias) != normalized(&display_name));
        let emails = clean_list(edit.calendar_emails.unwrap_or_default(), normalized_email);
        let organization = edit.organization.and_then(present);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let claimed: Vec<String> = std::iter::once(display_name.clone())
            .chain(aliases.iter().cloned())
            .collect();
        if taken_by_other(&transaction, None, &claimed)? {
            return Err(StoreError::Conflict);
        }
        let person_id = PersonId::new();
        transaction.execute(
            "INSERT INTO persons (
                id, display_name, aliases_json, calendar_emails_json, organization,
                organization_pinned, role, notes, created_at_utc_ms, updated_at_utc_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                person_id.uuid().to_string(),
                display_name,
                encode_strings(&aliases)?,
                encode_strings(&emails)?,
                organization,
                i64::from(organization.is_some()),
                edit.role.and_then(present),
                edit.notes.and_then(present),
                now_utc_ms
            ],
        )?;
        let revision = bump_people_revision_in(&transaction)?;
        let person = person_by_id_in(&transaction, person_id)?;
        transaction.commit()?;
        Ok(mutation_result(revision, Some(person), false))
    }

    pub(crate) fn update_person_profile(
        &self,
        person_id: PersonId,
        edit: PersonProfileEdit,
        now_utc_ms: i64,
    ) -> Result<PeopleMutationResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let before = person_by_id_in(&transaction, person_id)?;
        let mut person = before.clone();
        let mut old_name = None;
        if let Some(name) = edit.display_name {
            let name = name.trim().to_string();
            if normalized(&name).is_empty() {
                return Err(StoreError::Invalid);
            }
            if normalized(&name) != normalized(&person.display_name) {
                old_name = Some(std::mem::replace(&mut person.display_name, name));
            }
        }
        if let Some(aliases) = edit.aliases {
            person.aliases = clean_list(aliases, normalized);
        }
        merge_unique_case_insensitive(&mut person.aliases, old_name);
        let name_key = normalized(&person.display_name);
        person.aliases.retain(|alias| normalized(alias) != name_key);
        if let Some(emails) = edit.calendar_emails {
            person.calendar_emails = clean_list(emails, normalized_email);
        }
        if let Some(role) = edit.role {
            person.role = present(role);
        }
        if let Some(notes) = edit.notes {
            person.notes = present(notes);
        }
        let mut pinned = None;
        if let Some(organization) = edit.organization {
            person.organization = present(organization);
            pinned = Some(i64::from(person.organization.is_some()));
        }
        let claimed: Vec<String> = std::iter::once(person.display_name.clone())
            .chain(person.aliases.iter().cloned())
            .collect();
        if taken_by_other(&transaction, Some(person_id), &claimed)? {
            return Err(StoreError::Conflict);
        }
        if person == before {
            let revision = people_revision_in(&transaction)?;
            return Ok(mutation_result(revision, Some(person), false));
        }
        transaction.execute(
            "UPDATE persons
                SET display_name = ?1, aliases_json = ?2, calendar_emails_json = ?3,
                    organization = ?4,
                    organization_pinned = COALESCE(?5, organization_pinned),
                    role = ?6, notes = ?7, updated_at_utc_ms = ?8
              WHERE id = ?9",
            params![
                person.display_name,
                encode_strings(&person.aliases)?,
                encode_strings(&person.calendar_emails)?,
                person.organization,
                pinned,
                person.role,
                person.notes,
                now_utc_ms,
                person_id.uuid().to_string()
            ],
        )?;
        let revision = bump_people_revision_in(&transaction)?;
        let person = person_by_id_in(&transaction, person_id)?;
        transaction.commit()?;
        Ok(mutation_result(revision, Some(person), false))
    }
}
