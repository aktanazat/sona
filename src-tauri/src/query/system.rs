//! What Siri, Spotlight and Shortcuts read: the people and meetings Sona lists
//! to the system, and what one person promised. They read outside this app's
//! own screens, so D14 applies: a meeting whose series is kept on this Mac is
//! never listed, and nothing said in one answers.

use super::{KeptLocal, QueryError};
use crate::meeting::loop_types::MeetingLoopDirection;
use crate::meeting::people_types::{PersonCommitment, PersonId};
use crate::meeting::store::query_plane::SystemMeeting;
use crate::meeting::store::{MeetingStore, StoreError};
use serde::Serialize;
use specta::Type;

#[derive(Debug, Serialize, Type)]
pub struct SystemPerson {
    pub id: PersonId,
    pub display_name: String,
    pub aliases: Vec<String>,
    pub organization: Option<String>,
}

/// Every person, and every retained meeting newest first.
#[derive(Debug, Serialize, Type)]
pub struct SystemIndex {
    pub people: Vec<SystemPerson>,
    pub meetings: Vec<SystemMeeting>,
}

/// What one person promised the user, from their newest meeting back. These
/// are the person page's own commitments, on the side waiting on them.
#[derive(Debug, Serialize, Type)]
pub struct PersonPromises {
    pub person_id: PersonId,
    pub display_name: String,
    pub promises: Vec<PersonCommitment>,
}

pub(crate) fn system_index(store: &MeetingStore) -> Result<SystemIndex, QueryError> {
    let kept_local = KeptLocal::read(store)?;
    let people = store
        .people_list_without(kept_local.sessions())?
        .entries
        .into_iter()
        .map(|entry| SystemPerson {
            id: entry.person.id,
            display_name: entry.person.display_name,
            aliases: entry.person.aliases,
            organization: entry.person.organization,
        })
        .collect();
    let mut meetings = store.system_meetings()?;
    meetings.retain(|meeting| !kept_local.hides(meeting.id));
    Ok(SystemIndex { people, meetings })
}

/// `None` when no person has this id.
pub(crate) fn person_promises(
    store: &MeetingStore,
    person_id: PersonId,
) -> Result<Option<PersonPromises>, QueryError> {
    let kept_local = KeptLocal::read(store)?;
    let detail = match store.person_detail_without(person_id, kept_local.sessions()) {
        Ok(result) => result.detail,
        Err(StoreError::NotFound) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(PersonPromises {
        person_id,
        display_name: detail.person.display_name,
        promises: detail
            .commitments
            .into_iter()
            .filter(|promise| promise.direction == MeetingLoopDirection::WaitingOn)
            .collect(),
    }))
}
