mod derivation;
mod linking;

pub(in crate::meeting::store) use derivation::{
    derive_calendar_links_in, derive_speaker_link_in, derive_title_links_in,
    link_document_mentions_in, person_for_named_speaker_in, recompute_organizations_in,
};

pub(in crate::meeting::store) use self::linking::upsert_link_in;
use self::linking::{confidence_from_db, repoint_meeting_links_in, source_from_db};
use super::{
    bump_people_revision_in, merge_unique_case_insensitive, normalized, normalized_email,
    person_by_id_in, require_people_revision_in, SCHEMA_VERSION,
};
use crate::meeting::people_types::{
    PeopleMutationResult, Person, PersonId, PersonLinkConfidence, PersonLinkSource,
    PersonSplitRequest, PersonSplitTarget, PersonSummary, VoiceProfileMergeResolution,
};
use crate::meeting::store::documents::bump_document_revision_in;
use crate::meeting::store::transcript_corrections::{
    merge_person_corrections_in, prepare_person_merge_in,
};
use crate::meeting::store::voice_identity::{
    forget_person_voiceprints_in, merge_voice_profiles_in,
    remove_source_person_voice_evidence_for_sessions_in, remove_voice_person_evidence_in,
};
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::MeetingSessionId;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use std::collections::HashSet;

impl MeetingStore {
    pub(crate) fn rename_person(
        &self,
        person_id: PersonId,
        display_name: String,
        expected_revision: u64,
        now_utc_ms: i64,
    ) -> Result<PeopleMutationResult, StoreError> {
        let display_name = display_name.trim().to_string();
        if display_name.is_empty() {
            return Err(StoreError::Invalid);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_people_revision_in(&transaction, expected_revision)?;
        let current = person_by_id_in(&transaction, person_id)?;
        if normalized(&current.display_name) == normalized(&display_name) {
            return Ok(mutation_result(expected_revision, Some(current), false));
        }
        let mut aliases = current.aliases;
        merge_unique_case_insensitive(&mut aliases, [current.display_name]);
        transaction.execute(
            "UPDATE persons
                SET display_name = ?1, aliases_json = ?2, updated_at_utc_ms = ?3
              WHERE id = ?4",
            params![
                display_name,
                encode_strings(&aliases)?,
                now_utc_ms,
                person_id.uuid().to_string()
            ],
        )?;
        let revision = bump_people_revision_in(&transaction)?;
        let person = person_by_id_in(&transaction, person_id)?;
        transaction.commit()?;
        Ok(mutation_result(revision, Some(person), false))
    }

    pub(crate) fn merge_persons_with_voice_resolution(
        &self,
        source_id: PersonId,
        target_id: PersonId,
        expected_revision: u64,
        voice_profile_resolution: Option<VoiceProfileMergeResolution>,
        now_utc_ms: i64,
    ) -> Result<PeopleMutationResult, StoreError> {
        if source_id == target_id {
            return Err(StoreError::Invalid);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_people_revision_in(&transaction, expected_revision)?;
        let source = person_by_id_in(&transaction, source_id)?;
        let mut target = person_by_id_in(&transaction, target_id)?;
        prepare_person_merge_in(&transaction, source_id, target_id)?;
        merge_unique_case_insensitive(
            &mut target.aliases,
            std::iter::once(source.display_name).chain(source.aliases),
        );
        merge_unique_case_insensitive(&mut target.calendar_emails, source.calendar_emails);
        transaction.execute(
            "UPDATE persons
                SET aliases_json = ?1, calendar_emails_json = ?2, updated_at_utc_ms = ?3
              WHERE id = ?4",
            params![
                encode_strings(&target.aliases)?,
                encode_strings(&target.calendar_emails)?,
                now_utc_ms,
                target_id.uuid().to_string()
            ],
        )?;
        repoint_meeting_links_in(&transaction, source_id, target_id, now_utc_ms)?;
        transaction.execute(
            "INSERT OR IGNORE INTO document_person_links(document_id, person_id, created_at_utc_ms)
             SELECT document_id, ?1, created_at_utc_ms
               FROM document_person_links WHERE person_id = ?2",
            params![target_id.uuid().to_string(), source_id.uuid().to_string()],
        )?;
        merge_voice_profiles_in(
            &transaction,
            source_id,
            target_id,
            voice_profile_resolution,
            now_utc_ms,
        )?;
        transaction.execute(
            "UPDATE meeting_voice_speaker_resolutions SET resolved_person_id = ?1
              WHERE resolved_person_id = ?2 AND is_current = 1",
            params![target_id.uuid().to_string(), source_id.uuid().to_string()],
        )?;
        transaction.execute(
            "UPDATE meeting_voice_names SET person_id = ?1 WHERE person_id = ?2",
            params![target_id.uuid().to_string(), source_id.uuid().to_string()],
        )?;
        merge_person_corrections_in(&transaction, source_id, target_id)?;
        transaction.execute(
            "INSERT OR IGNORE INTO person_avatars (person_id, png, updated_at_utc_ms, provenance_json, original)
             SELECT ?1, png, updated_at_utc_ms, provenance_json, original
               FROM person_avatars WHERE person_id = ?2",
            params![target_id.uuid().to_string(), source_id.uuid().to_string()],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO person_contact_cards (person_id, card_json, updated_at_utc_ms)
             SELECT ?1, card_json, updated_at_utc_ms
               FROM person_contact_cards WHERE person_id = ?2",
            params![target_id.uuid().to_string(), source_id.uuid().to_string()],
        )?;
        transaction.execute(
            "UPDATE person_merge_redirects SET target_person_id = ?1 WHERE target_person_id = ?2",
            params![target_id.uuid().to_string(), source_id.uuid().to_string()],
        )?;
        transaction.execute(
            "INSERT INTO person_merge_redirects (source_person_id, target_person_id) VALUES (?1, ?2)",
            params![source_id.uuid().to_string(), target_id.uuid().to_string()],
        )?;
        transaction.execute(
            "DELETE FROM persons WHERE id = ?1",
            [source_id.uuid().to_string()],
        )?;
        let revision = bump_people_revision_in(&transaction)?;
        let target = person_by_id_in(&transaction, target_id)?;
        transaction.commit()?;
        Ok(mutation_result(revision, Some(target), true))
    }

    pub(crate) fn split_person(
        &self,
        request: PersonSplitRequest,
        now_utc_ms: i64,
    ) -> Result<PeopleMutationResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_people_revision_in(&transaction, request.expected_revision)?;
        let mut source = person_by_id_in(&transaction, request.source_person_id)?;
        let (target_id, mut target, mut changed) = match request.target {
            PersonSplitTarget::Create { display_name } => {
                let display_name = display_name.trim().to_string();
                if display_name.is_empty() {
                    return Err(StoreError::Invalid);
                }
                let target = Person {
                    id: PersonId::new(),
                    display_name,
                    aliases: Vec::new(),
                    calendar_emails: Vec::new(),
                    organization: None,
                    summary: None,
                    created_at_utc_ms: now_utc_ms,
                    updated_at_utc_ms: now_utc_ms,
                };
                transaction.execute(
                    "INSERT INTO persons (
                        id, display_name, aliases_json, calendar_emails_json,
                        created_at_utc_ms, updated_at_utc_ms
                     ) VALUES (?1, ?2, '[]', '[]', ?3, ?3)",
                    params![
                        target.id.uuid().to_string(),
                        target.display_name,
                        now_utc_ms
                    ],
                )?;
                (target.id, target, true)
            }
            PersonSplitTarget::Existing { person_id } => {
                if person_id == request.source_person_id {
                    return Err(StoreError::Invalid);
                }
                (person_id, person_by_id_in(&transaction, person_id)?, false)
            }
        };

        let alias_keys = selected_keys(&request.aliases, normalized)?;
        let mut moved_aliases = Vec::with_capacity(alias_keys.len());
        source.aliases.retain(|alias| {
            if alias_keys.contains(&normalized(alias)) {
                moved_aliases.push(alias.clone());
                false
            } else {
                true
            }
        });
        if moved_aliases.len() != alias_keys.len() {
            return Err(StoreError::Invalid);
        }
        moved_aliases.retain(|alias| normalized(alias) != normalized(&target.display_name));
        changed |= merge_unique_case_insensitive(&mut target.aliases, moved_aliases);

        let email_keys = selected_keys(&request.calendar_emails, normalized_email)?;
        let mut moved_emails = Vec::with_capacity(email_keys.len());
        source.calendar_emails.retain(|email| {
            if email_keys.contains(&normalized_email(email)) {
                moved_emails.push(email.clone());
                false
            } else {
                true
            }
        });
        if moved_emails.len() != email_keys.len() {
            return Err(StoreError::Invalid);
        }
        changed |= merge_unique_case_insensitive(&mut target.calendar_emails, moved_emails);

        if !alias_keys.is_empty() || !email_keys.is_empty() {
            transaction.execute(
                "UPDATE persons
                    SET aliases_json = ?1, calendar_emails_json = ?2, updated_at_utc_ms = ?3
                  WHERE id = ?4",
                params![
                    encode_strings(&source.aliases)?,
                    encode_strings(&source.calendar_emails)?,
                    now_utc_ms,
                    request.source_person_id.uuid().to_string()
                ],
            )?;
            transaction.execute(
                "UPDATE persons
                    SET aliases_json = ?1, calendar_emails_json = ?2, updated_at_utc_ms = ?3
                  WHERE id = ?4",
                params![
                    encode_strings(&target.aliases)?,
                    encode_strings(&target.calendar_emails)?,
                    now_utc_ms,
                    target_id.uuid().to_string()
                ],
            )?;
            changed = true;
        }

        let meeting_ids = request.meeting_ids.into_iter().collect::<HashSet<_>>();
        for &meeting_id in &meeting_ids {
            let link: Option<(String, String, i64)> = transaction
                .query_row(
                    "SELECT source, confidence, created_at_utc_ms
                       FROM meeting_person_links
                      WHERE meeting_id = ?1 AND person_id = ?2",
                    params![
                        meeting_id.uuid().to_string(),
                        request.source_person_id.uuid().to_string()
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((link_source, confidence, created_at_utc_ms)) = link else {
                return Err(StoreError::Invalid);
            };
            upsert_link_in(
                &transaction,
                meeting_id,
                target_id,
                source_from_db(&link_source)?,
                confidence_from_db(&confidence)?,
                created_at_utc_ms,
            )?;
            transaction.execute(
                "DELETE FROM meeting_person_links WHERE meeting_id = ?1 AND person_id = ?2",
                params![
                    meeting_id.uuid().to_string(),
                    request.source_person_id.uuid().to_string()
                ],
            )?;
            changed = true;
        }

        let voice_change = remove_source_person_voice_evidence_for_sessions_in(
            &transaction,
            request.source_person_id,
            &meeting_ids,
        )?;
        changed |= voice_change.people_changed();

        let document_ids = request.document_ids.into_iter().collect::<HashSet<_>>();
        let mut documents_changed = false;
        for document_id in document_ids {
            let created_at_utc_ms: Option<i64> = transaction
                .query_row(
                    "SELECT created_at_utc_ms FROM document_person_links
                      WHERE document_id = ?1 AND person_id = ?2",
                    params![
                        document_id.uuid().to_string(),
                        request.source_person_id.uuid().to_string()
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(created_at_utc_ms) = created_at_utc_ms else {
                return Err(StoreError::Invalid);
            };
            transaction.execute(
                "INSERT OR IGNORE INTO document_person_links (
                    document_id, person_id, created_at_utc_ms
                 ) VALUES (?1, ?2, ?3)",
                params![
                    document_id.uuid().to_string(),
                    target_id.uuid().to_string(),
                    created_at_utc_ms
                ],
            )?;
            transaction.execute(
                "DELETE FROM document_person_links WHERE document_id = ?1 AND person_id = ?2",
                params![
                    document_id.uuid().to_string(),
                    request.source_person_id.uuid().to_string()
                ],
            )?;
            changed = true;
            documents_changed = true;
        }

        let revision = if changed {
            bump_people_revision_in(&transaction)?
        } else {
            request.expected_revision
        };
        if documents_changed {
            bump_document_revision_in(&transaction)?;
        }
        let target = person_by_id_in(&transaction, target_id)?;
        transaction.commit()?;
        Ok(mutation_result(revision, Some(target), false))
    }

    pub(crate) fn delete_person(
        &self,
        person_id: PersonId,
        expected_revision: u64,
    ) -> Result<PeopleMutationResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_people_revision_in(&transaction, expected_revision)?;
        person_by_id_in(&transaction, person_id)?;
        forget_person_voiceprints_in(&transaction, person_id)?;
        remove_voice_person_evidence_in(&transaction, person_id)?;
        let removed = transaction.execute(
            "DELETE FROM persons WHERE id = ?1",
            [person_id.uuid().to_string()],
        )? != 0;
        if !removed {
            return Err(StoreError::NotFound);
        }
        let revision = bump_people_revision_in(&transaction)?;
        transaction.commit()?;
        Ok(mutation_result(revision, None, true))
    }

    pub(crate) fn confirm_person_link(
        &self,
        meeting_id: MeetingSessionId,
        person_id: PersonId,
        expected_revision: u64,
    ) -> Result<PeopleMutationResult, StoreError> {
        self.mutate_person_link(
            meeting_id,
            person_id,
            expected_revision,
            LinkMutation::Confirm,
        )
    }

    pub(crate) fn remove_person_link(
        &self,
        meeting_id: MeetingSessionId,
        person_id: PersonId,
        expected_revision: u64,
    ) -> Result<PeopleMutationResult, StoreError> {
        self.mutate_person_link(
            meeting_id,
            person_id,
            expected_revision,
            LinkMutation::Remove,
        )
    }

    pub(crate) fn add_manual_person_link(
        &self,
        meeting_id: MeetingSessionId,
        person_id: PersonId,
        expected_revision: u64,
        now_utc_ms: i64,
    ) -> Result<PeopleMutationResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_people_revision_in(&transaction, expected_revision)?;
        person_by_id_in(&transaction, person_id)?;
        let changed = upsert_link_in(
            &transaction,
            meeting_id,
            person_id,
            PersonLinkSource::Manual,
            PersonLinkConfidence::Confirmed,
            now_utc_ms,
        )?;
        let revision = if changed {
            bump_people_revision_in(&transaction)?
        } else {
            expected_revision
        };
        let person = person_by_id_in(&transaction, person_id)?;
        transaction.commit()?;
        Ok(mutation_result(revision, Some(person), false))
    }

    /// Writes one person's relationship paragraph.
    ///
    /// No fence and no revision bump, unlike every mutation above it. Those
    /// guard *identity* — a name, a merge, a link — which two screens can
    /// disagree about. A paragraph is a projection of meetings that have
    /// already happened, regenerated on demand and thrown away by the next
    /// pass, so fencing it would make an artifact finishing in the background
    /// invalidate the rename a person is halfway through typing.
    pub(crate) fn set_person_summary(
        &self,
        person_id: PersonId,
        summary: PersonSummary,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let written = connection.execute(
            "UPDATE persons
                SET summary = ?1, summary_generated_at_utc_ms = ?2, summary_model_id = ?3
              WHERE id = ?4",
            params![
                summary.text,
                summary.generated_at_utc_ms,
                summary.model_id,
                person_id.uuid().to_string()
            ],
        )?;
        if written == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    fn mutate_person_link(
        &self,
        meeting_id: MeetingSessionId,
        person_id: PersonId,
        expected_revision: u64,
        mutation: LinkMutation,
    ) -> Result<PeopleMutationResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_people_revision_in(&transaction, expected_revision)?;
        let changed = match mutation {
            LinkMutation::Confirm => transaction.execute(
                "UPDATE meeting_person_links SET confidence = 'confirmed'
                  WHERE meeting_id = ?1 AND person_id = ?2 AND confidence = 'suggested'",
                params![meeting_id.uuid().to_string(), person_id.uuid().to_string()],
            )?,
            LinkMutation::Remove => transaction.execute(
                "DELETE FROM meeting_person_links WHERE meeting_id = ?1 AND person_id = ?2",
                params![meeting_id.uuid().to_string(), person_id.uuid().to_string()],
            )?,
        } != 0;
        if !changed {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM meeting_person_links
                  WHERE meeting_id = ?1 AND person_id = ?2)",
                params![meeting_id.uuid().to_string(), person_id.uuid().to_string()],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(StoreError::NotFound);
            }
        }
        let revision = if changed {
            bump_people_revision_in(&transaction)?
        } else {
            expected_revision
        };
        let person = person_by_id_in(&transaction, person_id)?;
        transaction.commit()?;
        Ok(mutation_result(
            revision,
            Some(person),
            mutation == LinkMutation::Remove,
        ))
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum LinkMutation {
    Confirm,
    Remove,
}

fn selected_keys(
    values: &[String],
    normalize: fn(&str) -> String,
) -> Result<HashSet<String>, StoreError> {
    let mut keys = HashSet::with_capacity(values.len());
    for value in values {
        let key = normalize(value);
        if key.is_empty() || !keys.insert(key) {
            return Err(StoreError::Invalid);
        }
    }
    Ok(keys)
}

fn mutation_result(revision: u64, person: Option<Person>, removed: bool) -> PeopleMutationResult {
    PeopleMutationResult {
        schema_version: SCHEMA_VERSION,
        revision,
        person,
        removed,
    }
}

pub(super) fn encode_strings(values: &[String]) -> Result<String, StoreError> {
    serde_json::to_string(values).map_err(|_| StoreError::Corrupt)
}
