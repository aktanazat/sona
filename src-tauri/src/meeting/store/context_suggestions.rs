//! The maybes one recording carries from its own start: the events near a
//! start made by hand, the place its invite pins, the kind of conversation
//! its tracks heard, and the meeting it may be when nothing is on its
//! calendar. They wait in `filing_suggestions` beside the evening's filings,
//! and like those change nothing until the user takes one.

use super::context::{context_in, save_context_in};
use super::evening::{
    accept_filing_in, confirmed_people_in, event_target, filed_event_keys_in, insert_suggestion_in,
    near_events,
};
use super::meeting_maybe::{offer_meeting_maybes_in, with_plan_words_in, HeardVoice};
use super::workflows::calendar_facts_in;
use super::{
    canonical_speaker_ids, decode_json, effective_segments_for_session, id, session_row,
    MeetingStore, StoreError,
};
use crate::meeting::context::{
    ConversationContext, ConversationContextUpdate, ConversationKind, ConversationLocation,
};
use crate::meeting::detection::calendar::CalendarOccurrence;
use crate::meeting::detection::machine::SLACK_BUNDLE_ID;
use crate::meeting::evening::{FilingReason, FilingSuggestion, FilingTarget};
use crate::meeting::types::{MeetingOrigin, MeetingPhase, MeetingSessionId};
use rusqlite::{Connection, TransactionBehavior};
use std::collections::{HashMap, HashSet};

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
    /// kind is open is first offered the kind its tracks heard, and one with
    /// nothing on its calendar the meeting it may be, from the plans heard
    /// before it and the voices in `heard`. A maybe the recording has since
    /// settled another way is left out.
    pub(crate) fn context_suggestions(
        &self,
        session_id: MeetingSessionId,
        heard: &[HeardVoice],
        user_name: Option<&str>,
        now_utc_ms: i64,
    ) -> Result<Vec<FilingSuggestion>, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let context = context_in(&transaction, session_id)?;
        if let Some((conversation_kind, reason)) = heard_kind_in(&transaction, &context)? {
            insert_suggestion_in(
                &transaction,
                session_id,
                &FilingTarget::Kind { conversation_kind },
                &[reason],
                now_utc_ms,
            )?;
        }
        let has_event = calendar_facts_in(&transaction, session_id)?.is_some();
        if !has_event {
            offer_meeting_maybes_in(&transaction, &context, heard, user_name, now_utc_ms)?;
        }
        let linked = confirmed_people_in(&transaction, session_id)?;
        let suggestions = pending_in(&transaction, session_id)?
            .into_iter()
            .filter(|suggestion| match &suggestion.target {
                FilingTarget::Event { .. } | FilingTarget::Meeting { .. } => !has_event,
                FilingTarget::Kind { .. } => context.kind == ConversationKind::Unspecified,
                FilingTarget::Place { .. } => context.location.is_none(),
                FilingTarget::Person { person_id, .. } => !linked.contains(person_id),
                FilingTarget::Project { .. } => true,
                FilingTarget::Introducer { .. } => false,
            })
            .filter_map(|suggestion| with_plan_words_in(&transaction, suggestion).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        transaction.commit()?;
        Ok(suggestions)
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
            | FilingTarget::Project { .. }
            | FilingTarget::Introducer { .. }
            | FilingTarget::Meeting { .. } => return Err(StoreError::Invalid),
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

/// The kind a finished recording's lines point to while its kind is open: a
/// call when a recorded app's audio carried voices, in person when two or
/// more voices spoke into the microphone alone and no app was recorded.
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
    let mut statement = connection
        .prepare("SELECT track_id, source_kind FROM meeting_source_tracks WHERE session_id = ?1")?;
    let source_kinds = statement
        .query_map([id(context.session_id)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<HashMap<_, _>, _>>()?;
    let canonical = canonical_speaker_ids(connection, context.session_id)?;
    let mut system_audio = false;
    let mut microphone_voices = HashSet::new();
    for segment in effective_segments_for_session(connection, context.session_id)? {
        if segment.removed {
            continue;
        }
        match source_kinds
            .get(&id(segment.base.track_id))
            .map(String::as_str)
        {
            Some("system_audio") => system_audio = true,
            Some("microphone") => {
                let speaker = segment.assigned_speaker_id;
                microphone_voices.insert(canonical.get(&speaker).copied().unwrap_or(speaker));
            }
            _ => {}
        }
    }
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
        None if microphone_voices.len() >= 2 && !system_audio => {
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
