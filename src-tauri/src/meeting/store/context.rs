use super::{decode_json, encode_json, id, session_row, to_i64, MeetingStore, StoreError};
use crate::meeting::context::{
    ConversationContext, ConversationContextUpdate, ConversationKind, ConversationLocation,
};
use crate::meeting::types::{MeetingOrigin, MeetingPhase, MeetingSessionId};
use rusqlite::{params, Connection, OptionalExtension};

impl MeetingStore {
    pub(crate) fn conversation_context(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<ConversationContext, StoreError> {
        let connection = self.connection()?;
        context_in(&connection, session_id)
    }

    pub(crate) fn update_conversation_context(
        &self,
        request: ConversationContextUpdate,
    ) -> Result<ConversationContext, StoreError> {
        if request
            .location
            .as_ref()
            .is_some_and(|location| !location.is_valid())
        {
            return Err(StoreError::Invalid);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let current = context_in(&transaction, request.session_id)?;
        if current.revision != request.expected_revision {
            return Err(StoreError::StaleRevision);
        }
        let revision = current.revision.checked_add(1).ok_or(StoreError::Invalid)?;
        transaction.execute(
            "INSERT INTO conversation_context(session_id, revision, kind_json, location_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id) DO UPDATE SET
                revision = excluded.revision, kind_json = excluded.kind_json,
                location_json = excluded.location_json",
            params![
                id(request.session_id),
                i64::try_from(revision).map_err(|_| StoreError::Invalid)?,
                encode_json(&request.kind)?,
                request.location.as_ref().map(encode_json).transpose()?,
            ],
        )?;
        let session = session_row(&transaction, request.session_id)?;
        let revision = session.revision.checked_add(1).ok_or(StoreError::Invalid)?;
        transaction.execute(
            "UPDATE meeting_sessions SET revision = ?1 WHERE id = ?2",
            params![to_i64(revision)?, id(request.session_id)],
        )?;
        let result = context_in(&transaction, request.session_id)?;
        transaction.commit()?;
        Ok(result)
    }
}

pub(super) fn context_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<ConversationContext, StoreError> {
    let session = session_row(connection, session_id)?;
    if session.phase == MeetingPhase::Deleting {
        return Err(StoreError::NotFound);
    }
    let origin: String = connection.query_row(
        "SELECT origin_kind FROM meeting_sessions WHERE id = ?1",
        params![id(session_id)],
        |row| row.get(0),
    )?;
    let origin: MeetingOrigin = decode_json(&origin)?;
    let preflight: serde_json::Value = decode_json(&session.preflight_json)?;
    let application_bundle_ids: Vec<String> = preflight
        .get("frozen_system_audio_application_bundle_ids")
        .and_then(serde_json::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let (captured_microphone, captured_system_audio): (bool, bool) = if origin
        == MeetingOrigin::Import
    {
        (false, false)
    } else {
        connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM meeting_source_tracks track
                    JOIN meeting_track_checkpoints checkpoint ON checkpoint.track_id = track.track_id
                    WHERE track.session_id = ?1 AND track.source_kind = 'microphone' AND checkpoint.durable_bytes > 0),
                EXISTS(SELECT 1 FROM meeting_source_tracks track
                    JOIN meeting_track_checkpoints checkpoint ON checkpoint.track_id = track.track_id
                    WHERE track.session_id = ?1 AND track.source_kind = 'system_audio' AND checkpoint.durable_bytes > 0)",
            params![id(session_id)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
    };
    let calendar = super::workflows::calendar_facts_in(connection, session_id)?;
    let stored = connection
        .query_row(
            "SELECT revision, kind_json, location_json, calendar_name, calendar_location
         FROM conversation_context WHERE session_id = ?1",
            params![id(session_id)],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    let inferred = if origin == MeetingOrigin::Import {
        ConversationKind::Imported
    } else {
        ConversationKind::Unspecified
    };
    let (revision, kind, location, saved_calendar_name, saved_calendar_location): (
        u64,
        ConversationKind,
        Option<ConversationLocation>,
        Option<String>,
        Option<String>,
    ) = match stored {
        Some((revision, kind, location, name, place)) => (
            u64::try_from(revision).map_err(|_| StoreError::Corrupt)?,
            decode_json(&kind)?,
            location.map(|json| decode_json(&json)).transpose()?,
            name,
            place,
        ),
        None => (0, inferred, None, None, None),
    };
    let (calendar_name, calendar_location) = calendar
        .map(|event| (event.calendar_name, event.location))
        .unwrap_or_default();
    Ok(ConversationContext {
        session_id,
        revision,
        kind,
        origin,
        application_bundle_ids,
        captured_microphone,
        captured_system_audio,
        calendar_name: calendar_name.or(saved_calendar_name),
        calendar_location: calendar_location.or(saved_calendar_location),
        location,
    })
}

pub(super) fn import_in(
    connection: &Connection,
    context: &ConversationContext,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO conversation_context
         (session_id, revision, kind_json, location_json, calendar_name, calendar_location)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id(context.session_id),
            to_i64(context.revision)?,
            encode_json(&context.kind)?,
            context.location.as_ref().map(encode_json).transpose()?,
            context.calendar_name,
            context.calendar_location,
        ],
    )?;
    Ok(())
}
