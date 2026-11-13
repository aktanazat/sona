//! Who changed a meeting's doc and board, when, and how far into the call:
//! Sona's readings, the user's hand edits, and teammates' edits from the
//! meeting's link. The marks on points and cards and the timeline read
//! these, during the call and after it.

use super::LiveTranscript;
use crate::meeting::live_doc_types::{
    MeetingLiveAuthor, MeetingLiveChange, MeetingLiveChangeKind, MeetingLiveTarget,
};
use crate::meeting::store::MeetingStore;
use crate::meeting::types::MeetingSessionId;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::MutexGuard;
use uuid::Uuid;

/// Changes kept for one meeting; past it the oldest go first.
const MAX_CHANGES: usize = 2_000;

/// Who made a change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Author {
    Sona,
    Owner,
    /// Someone with the meeting's link, by the name they gave it. The id
    /// their browser gave the edit names the change, so the browser knows
    /// its edit landed.
    Teammate {
        name: String,
        edit_id: String,
    },
}

impl Author {
    /// This author's change, made now.
    pub(crate) fn change(
        &self,
        call_ns: Option<u64>,
        target: MeetingLiveTarget,
        target_id: &str,
        kind: MeetingLiveChangeKind,
        text: &str,
    ) -> MeetingLiveChange {
        let (id, by, name) = match self {
            Self::Sona => (Uuid::new_v4().to_string(), MeetingLiveAuthor::Sona, None),
            Self::Owner => (Uuid::new_v4().to_string(), MeetingLiveAuthor::Owner, None),
            Self::Teammate { name, edit_id } => (
                edit_id.clone(),
                MeetingLiveAuthor::Teammate,
                Some(name.clone()),
            ),
        };
        MeetingLiveChange {
            id,
            by,
            name,
            at_ms: chrono::Utc::now().timestamp_millis(),
            call_ns,
            target,
            target_id: target_id.to_owned(),
            kind,
            text: text.to_owned(),
        }
    }
}

impl LiveTranscript {
    fn changes(&self) -> MutexGuard<'_, Vec<MeetingLiveChange>> {
        self.changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Note a change made now, as far into the call as has been heard.
    pub(crate) fn record_change(
        &self,
        author: &Author,
        target: MeetingLiveTarget,
        target_id: &str,
        kind: MeetingLiveChangeKind,
        text: &str,
    ) {
        let call_ns = self.heard_ns.load(Ordering::Relaxed);
        push(
            &mut self.changes(),
            author.change(Some(call_ns), target, target_id, kind, text),
        );
    }

    /// Every change so far, oldest first.
    pub(crate) fn live_changes(&self) -> Vec<MeetingLiveChange> {
        self.changes().clone()
    }

    /// Whether a change of that name was made.
    pub(crate) fn has_change(&self, id: &str) -> bool {
        self.changes().iter().any(|change| change.id == id)
    }

    /// Forget the changes `lost` picks out.
    pub(crate) fn forget_changes(&self, lost: impl Fn(&MeetingLiveChange) -> bool) {
        self.changes().retain(|change| !lost(change));
    }
}

/// Add `change`, dropping the oldest past [`MAX_CHANGES`].
pub(crate) fn push(changes: &mut Vec<MeetingLiveChange>, change: MeetingLiveChange) {
    if changes.len() >= MAX_CHANGES {
        changes.remove(0);
    }
    changes.push(change);
}

/// The capture's changes into the store, with those the meeting's page
/// made after the stop, so the marks outlive the call. The capture takes
/// the page's too, so its room shows them.
pub(crate) fn save(store: &MeetingStore, session_id: MeetingSessionId, live: &LiveTranscript) {
    let saved = match store.live_changes(session_id) {
        Ok(saved) => saved,
        Err(error) => {
            log::warn!("Could not read the live changes of {session_id:?}: {error:?}");
            return;
        }
    };
    let changes = {
        let mut kept = live.changes();
        let changes = merged(saved, kept.clone());
        kept.clone_from(&changes);
        changes
    };
    if let Err(error) = store.save_live_changes(session_id, &changes) {
        log::warn!("Could not keep the live changes of {session_id:?}: {error:?}");
    }
}

/// The saved changes with the capture's newer ones, oldest first.
fn merged(saved: Vec<MeetingLiveChange>, live: Vec<MeetingLiveChange>) -> Vec<MeetingLiveChange> {
    let known: HashSet<String> = saved.iter().map(|change| change.id.clone()).collect();
    let mut changes = saved;
    for change in live
        .into_iter()
        .filter(|change| !known.contains(&change.id))
    {
        push(&mut changes, change);
    }
    changes.sort_by_key(|change| change.at_ms);
    changes
}
