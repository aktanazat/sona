//! The maybes one recording carries from its own start: the events near a
//! start made by hand, the place its invite pins, and the kind of
//! conversation its tracks heard. They wait in `filing_suggestions` beside the
//! evening's filings, and like those change nothing until the user takes one.

use super::context::{context_in, save_context_in};
use super::evening::{
    accept_filing_in, event_target, filed_event_keys_in, insert_suggestion_in, near_events,
};
use super::workflows::calendar_facts_in;
use super::{decode_json, id, session_row, MeetingStore, StoreError};
use crate::meeting::context::{
    ConversationContext, ConversationContextUpdate, ConversationKind, ConversationLocation,
};
use crate::meeting::detection::calendar::CalendarOccurrence;
use crate::meeting::detection::machine::SLACK_BUNDLE_ID;
use crate::meeting::evening::{FilingReason, FilingSuggestion, FilingTarget};
use crate::meeting::types::{MeetingOrigin, MeetingPhase, MeetingSessionId};
use rusqlite::{Connection, TransactionBehavior};
use std::collections::HashSet;

/// A start by hand offers at most this many events.
const MAX_START_EVENTS: usize = 2;

impl MeetingStore {
    /// Offers the events near a recording started by hand at `now_utc_ms`:
    /// one under way first, then the nearest. An event another meeting
    /// already holds is left out.
    pub(crate) fn suggest_start_events(
        &self,
        session_id: MeetingSessionId,
        events: &[CalendarOccurrence],
        now_utc_ms: i64,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let filed = filed_event_keys_in(&transaction)?;
        for event in near_events(now_utc_ms, now_utc_ms, events, &filed)
            .into_iter()
            .take(MAX_START_EVENTS)
        {
            let overlaps = event.start_utc_ms <= now_utc_ms && now_utc_ms < event.end_utc_ms;
            insert_suggestion_in(
                &transaction,
                session_id,
                &event_target(event),
                &[FilingReason::OnCalendar { overlaps }],
                now_utc_ms,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// The maybes still open on one recording. A finished recording whose
    /// kind is open is first offered the kind its tracks heard. A maybe the
    /// recording has since settled another way is left out.
    pub(crate) fn context_suggestions(
        &self,
        session_id: MeetingSessionId,
        now_utc_ms: i64,
    ) -> Result<Vec<FilingSuggestion>, StoreError> {
        let connection = self.connection()?;
        let context = context_in(&connection, session_id)?;
        if let Some((conversation_kind, reason)) = heard_kind_in(&connection, &context)? {
            insert_suggestion_in(
                &connection,
                session_id,
                &FilingTarget::Kind { conversation_kind },
                &[reason],
                now_utc_ms,
            )?;
        }
        let has_event = calendar_facts_in(&connection, session_id)?.is_some();
        Ok(pending_in(&connection, session_id)?
            .into_iter()
            .filter(|suggestion| match &suggestion.target {
                FilingTarget::Event { .. } => !has_event,
                FilingTarget::Kind { .. } => context.kind == ConversationKind::Unspecified,
                FilingTarget::Place { .. } => context.location.is_none(),
                FilingTarget::Person { .. } | FilingTarget::Project { .. } => true,
            })
            .collect())
    }

    /// Takes a kind or place maybe into the recording's context, with the
    /// same write the context editor makes.
    pub(crate) fn file_context(
        &self,
        suggestion_id: &str,
        session_id: MeetingSessionId,
        target: &FilingTarget,
        now_utc_ms: i64,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = context_in(&transaction, session_id)?;
        let (kind, location) = match target {
            FilingTarget::Kind { conversation_kind } => (*conversation_kind, current.location),
            FilingTarget::Place { location } => (current.kind, Some(location.clone())),
            FilingTarget::Event { .. }
            | FilingTarget::Person { .. }
            | FilingTarget::Project { .. } => return Err(StoreError::Invalid),
        };
        save_context_in(
            &transaction,
            &ConversationContextUpdate {
                session_id,
                expected_revision: current.revision,
                kind,
                location,
            },
        )?;
        accept_filing_in(&transaction, suggestion_id, now_utc_ms)?;
        transaction.commit()?;
        Ok(())
    }
}

/// Offers the place an invite pins to a recording that names none yet.
pub(super) fn suggest_place_in(
    connection: &Connection,
    session_id: MeetingSessionId,
    place: &ConversationLocation,
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    if context_in(connection, session_id)?.location.is_some() {
        return Ok(());
    }
    insert_suggestion_in(
        connection,
        session_id,
        &FilingTarget::Place {
            location: place.clone(),
        },
        &[FilingReason::InInvite],
        now_utc_ms,
    )
}

/// The kind a finished recording's tracks point to while its kind is open:
/// a call when a recorded app's audio carried voices, in person when only the
/// microphone heard any and no app was recorded.
fn heard_kind_in(
    connection: &Connection,
    context: &ConversationContext,
) -> Result<Option<(ConversationKind, FilingReason)>, StoreError> {
    if context.kind != ConversationKind::Unspecified
        || context.origin == MeetingOrigin::Import
        || session_row(connection, context.session_id)?.phase != MeetingPhase::ReviewReady
    {
        return Ok(None);
    }
    let mut statement = connection.prepare(
        "SELECT DISTINCT t.source_kind
           FROM meeting_sessions m
           JOIN meeting_transcript_segments s
             ON s.transcript_revision_id = m.current_transcript_revision_id
           JOIN meeting_source_tracks t ON t.track_id = s.track_id
          WHERE m.id = ?1",
    )?;
    let heard = statement
        .query_map([id(context.session_id)], |row| row.get::<_, String>(0))?
        .collect::<Result<HashSet<_>, _>>()?;
    let system_audio = heard.contains("system_audio");
    Ok(match context.application_bundle_ids.first() {
        Some(bundle_id) if system_audio => Some((
            if bundle_id == SLACK_BUNDLE_ID {
                ConversationKind::Huddle
            } else {
                ConversationKind::OnlineCall
            },
            FilingReason::AppAudio {
                bundle_id: bundle_id.clone(),
            },
        )),
        None if heard.contains("microphone") && !system_audio => {
            Some((ConversationKind::InPerson, FilingReason::OnlyMicrophone))
        }
        _ => None,
    })
}

fn pending_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<FilingSuggestion>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, target_json, reasons_json FROM filing_suggestions
          WHERE session_id = ?1 AND state = 'pending'
          ORDER BY rowid",
    )?;
    let rows = statement.query_map([id(session_id)], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    rows.map(|row| {
        let (suggestion, target, reasons) = row?;
        Ok(FilingSuggestion {
            id: suggestion,
            target: decode_json(&target)?,
            reasons: decode_json(&reasons)?,
        })
    })
    .collect()
}
