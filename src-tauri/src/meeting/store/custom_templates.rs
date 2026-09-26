use super::{decode_json, encode_json, from_i64, id, parse_uuid, MeetingStore, StoreError};
use crate::meeting::template_types::{
    MeetingCustomTemplate, MeetingCustomTemplateDeleteRequest, MeetingCustomTemplateSaveRequest,
    MeetingCustomTemplateSaveResult, MeetingCustomTemplates, MeetingTemplateId, NotesTemplateChoice,
    MAX_CUSTOM_TEMPLATES,
};
use crate::meeting::types::MeetingSessionId;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

impl MeetingStore {
    pub(crate) fn custom_templates(&self) -> Result<MeetingCustomTemplates, StoreError> {
        let connection = self.connection()?;
        custom_templates_in(&connection)
    }

    pub(crate) fn custom_template(
        &self,
        template_id: MeetingTemplateId,
    ) -> Result<Option<MeetingCustomTemplate>, StoreError> {
        let connection = self.connection()?;
        custom_template_in(&connection, template_id)
    }

    pub(crate) fn save_custom_template(
        &self,
        request: &MeetingCustomTemplateSaveRequest,
        now_utc_ms: i64,
    ) -> Result<MeetingCustomTemplateSaveResult, StoreError> {
        let draft = request.draft.normalized().map_err(|_| StoreError::Invalid)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if custom_templates_revision_in(&transaction)? != request.expected_revision {
            return Err(StoreError::StaleRevision);
        }
        let sections_json = encode_json(&draft.sections)?;
        let template_id = if let Some(template_id) = request.template_id {
            let changed = transaction.execute(
                "UPDATE meeting_custom_templates SET name = ?2, purpose = ?3,
                    sections_json = ?4, updated_at_utc_ms = ?5 WHERE template_id = ?1",
                params![id(template_id), draft.name, draft.purpose, sections_json, now_utc_ms],
            )?;
            if changed == 0 {
                return Err(StoreError::NotFound);
            }
            template_id
        } else {
            let count: usize = transaction.query_row(
                "SELECT COUNT(*) FROM meeting_custom_templates", [], |row| row.get(0),
            )?;
            if count >= MAX_CUSTOM_TEMPLATES {
                return Err(StoreError::Invalid);
            }
            let template_id = MeetingTemplateId::new();
            transaction.execute(
                "INSERT INTO meeting_custom_templates
                    (template_id, name, purpose, sections_json, created_at_utc_ms, updated_at_utc_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id(template_id), draft.name, draft.purpose, sections_json, now_utc_ms],
            )?;
            template_id
        };
        bump_custom_templates_revision_in(&transaction)?;
        let templates = custom_templates_in(&transaction)?;
        transaction.commit()?;
        Ok(MeetingCustomTemplateSaveResult { template_id, templates })
    }

    pub(crate) fn delete_custom_template(
        &self,
        request: &MeetingCustomTemplateDeleteRequest,
    ) -> Result<MeetingCustomTemplates, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if custom_templates_revision_in(&transaction)? != request.expected_revision {
            return Err(StoreError::StaleRevision);
        }
        if transaction.execute(
            "DELETE FROM meeting_custom_templates WHERE template_id = ?1",
            params![id(request.template_id)],
        )? == 0 {
            return Err(StoreError::NotFound);
        }
        super::series::forget_custom_template_in(&transaction, request.template_id)?;
        bump_custom_templates_revision_in(&transaction)?;
        let templates = custom_templates_in(&transaction)?;
        transaction.commit()?;
        Ok(templates)
    }

    /// Resolve the rungs below a meeting's own saved choice in one place.
    pub(crate) fn notes_template_fallback(
        &self,
        session_id: MeetingSessionId,
        default: NotesTemplateChoice,
    ) -> Result<NotesTemplateChoice, StoreError> {
        let series = self.series_preferences_for_session(session_id)?;
        if let Some(custom_template_id) = series.custom_template_id {
            return Ok(NotesTemplateChoice { template: default.template, custom_template_id: Some(custom_template_id) });
        }
        if let Some(template) = series.template {
            return Ok(template.into());
        }
        if let Some(template) = self.folder_template_for_session(session_id)? {
            return Ok(template.into());
        }
        let connection = self.connection()?;
        existing_choice_in(&connection, default)
    }
}

pub(super) fn custom_template_exists_in(
    connection: &Connection,
    template_id: MeetingTemplateId,
) -> Result<bool, StoreError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM meeting_custom_templates WHERE template_id = ?1)",
        params![id(template_id)], |row| row.get(0),
    )?)
}

pub(super) fn existing_choice_in(
    connection: &Connection,
    mut choice: NotesTemplateChoice,
) -> Result<NotesTemplateChoice, StoreError> {
    if let Some(template_id) = choice.custom_template_id {
        if !custom_template_exists_in(connection, template_id)? {
            choice.custom_template_id = None;
        }
    }
    Ok(choice)
}

pub(super) fn custom_template_in(
    connection: &Connection,
    template_id: MeetingTemplateId,
) -> Result<Option<MeetingCustomTemplate>, StoreError> {
    connection.query_row(
        "SELECT template_id, name, purpose, sections_json, created_at_utc_ms, updated_at_utc_ms
         FROM meeting_custom_templates WHERE template_id = ?1",
        params![id(template_id)], template_columns,
    ).optional()?.map(template_from_columns).transpose()
}

fn custom_templates_in(connection: &Connection) -> Result<MeetingCustomTemplates, StoreError> {
    let mut statement = connection.prepare(
        "SELECT template_id, name, purpose, sections_json, created_at_utc_ms, updated_at_utc_ms
         FROM meeting_custom_templates ORDER BY template_id",
    )?;
    let rows = statement.query_map([], template_columns)?;
    let mut templates = rows.map(|row| template_from_columns(row?)).collect::<Result<Vec<_>, StoreError>>()?;
    templates.sort_by_cached_key(|template| template.name.to_lowercase());
    Ok(MeetingCustomTemplates { templates, revision: custom_templates_revision_in(connection)? })
}

type TemplateColumns = (String, String, String, String, i64, i64);

fn template_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<TemplateColumns> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
}

fn template_from_columns(row: TemplateColumns) -> Result<MeetingCustomTemplate, StoreError> {
    Ok(MeetingCustomTemplate {
        template_id: MeetingTemplateId::from_uuid(parse_uuid(&row.0)?),
        name: row.1,
        purpose: row.2,
        sections: decode_json(&row.3)?,
        created_at_utc_ms: row.4,
        updated_at_utc_ms: row.5,
    })
}

fn custom_templates_revision_in(connection: &Connection) -> Result<u64, StoreError> {
    from_i64(connection.query_row(
        "SELECT revision FROM meeting_custom_template_state WHERE singleton = 1", [], |row| row.get(0),
    )?)
}

fn bump_custom_templates_revision_in(connection: &Connection) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE meeting_custom_template_state SET revision = revision + 1 WHERE singleton = 1", [],
    )?;
    Ok(())
}
