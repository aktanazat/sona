//! One read of a meeting for its wrap. It writes nothing.

use super::context::context_in;
use super::heard_plans::plans_in;
use super::loops::loops_in;
use super::people::{confirmed_person_ids_in, person_by_id_in};
use super::transcript_corrections::confirmed_person_for_speaker_in;
use super::{
    canonical_speaker_ids, effective_segments_for_session, id, utc_now_ms, MeetingStore, StoreError,
};
use crate::meeting::loop_types::{MeetingLoopDirection, MeetingLoopKind};
use crate::meeting::people_types::PersonId;
use crate::meeting::types::{ArtifactCitation, MeetingSessionId, SpeakerAssignmentKind};
use crate::meeting::wrap_types::{MeetingWrap, MeetingWrapPerson, MeetingWrapPlace};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};

impl MeetingStore {
    pub(crate) fn meeting_wrap(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingWrap, StoreError> {
        let user_name = self.user_name();
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let context = context_in(&transaction, session_id)?;
        let mut first_heard = first_heard_in(&transaction, session_id)?;
        let people = confirmed_person_ids_in(&transaction, session_id)?
            .into_iter()
            .map(|person_id| {
                Ok(MeetingWrapPerson {
                    person_id,
                    display_name: person_by_id_in(&transaction, person_id)?.display_name,
                    first_heard: first_heard.remove(&person_id),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let place = match (context.location, context.calendar_location) {
            (Some(location), _) => Some(MeetingWrapPlace {
                name: location.name,
                from_invite: false,
            }),
            (None, Some(name)) => Some(MeetingWrapPlace {
                name,
                from_invite: true,
            }),
            (None, None) => None,
        };
        let plans = plans_in(
            &transaction,
            "p.session_id = ?1",
            params![id(session_id)],
            utc_now_ms(),
            &HashSet::new(),
        )?;
        let (mut mine, mut owed) = (Vec::new(), Vec::new());
        for row in loops_in(&transaction, session_id, user_name.as_deref())?.rows {
            if row.kind != MeetingLoopKind::Commitment || !row.is_open() {
                continue;
            }
            match row.direction {
                MeetingLoopDirection::Mine => mine.push(row),
                MeetingLoopDirection::WaitingOn => owed.push(row),
                MeetingLoopDirection::Unattributed => {}
            }
        }
        transaction.commit()?;
        Ok(MeetingWrap {
            people,
            kind: context.kind,
            place,
            plans,
            mine,
            owed,
        })
    }
}

/// Each person's first line in the meeting, read only from speakers the user
/// confirmed as that person.
fn first_heard_in(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<HashMap<PersonId, ArtifactCitation>, StoreError> {
    let canonical = canonical_speaker_ids(connection, session_id)?;
    let mut people = HashMap::new();
    let mut first_heard = HashMap::new();
    for segment in effective_segments_for_session(connection, session_id)? {
        if segment.removed
            || !matches!(
                segment.speaker_assignment,
                SpeakerAssignmentKind::LocalSpeaker | SpeakerAssignmentKind::SystemSpeaker
            )
            || segment
                .replacement_text
                .as_deref()
                .unwrap_or(&segment.base.text)
                .trim()
                .is_empty()
        {
            continue;
        }
        let speaker_id = canonical
            .get(&segment.assigned_speaker_id)
            .copied()
            .unwrap_or(segment.assigned_speaker_id);
        let person_id = match people.get(&speaker_id) {
            Some(person_id) => *person_id,
            None => {
                let person_id =
                    confirmed_person_for_speaker_in(connection, session_id, speaker_id)?;
                people.insert(speaker_id, person_id);
                person_id
            }
        };
        if let Some(person_id) = person_id {
            first_heard
                .entry(person_id)
                .or_insert_with(|| ArtifactCitation {
                    segment_id: segment.base.segment_id,
                    start_offset_ns: segment.base.start_offset_ns,
                    end_offset_ns: segment.base.end_offset_ns,
                });
        }
    }
    Ok(first_heard)
}
