//! A recording meeting's doc and board, shared by link: what teammates see,
//! and their edits coming back. A teammate's edit is kept like the user's
//! own, so later readings never write over it.

use super::live_changes::{self, Author};
use super::{live_doc, LiveTranscript, MeetingProcessingService};
use crate::meeting::analytics::MeetingProvisionalSegment;
use crate::meeting::live_doc_types::{MeetingLiveChange, MeetingLiveTarget};
use crate::meeting::store::{LiveRoom, MeetingStore, StoreError};
use crate::meeting::types::{MeetingSessionId, SourceKind, SourceTrackId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Most bytes of state sent to the room; the oldest lines go first past it,
/// so the sealed frame stays under the room's limit.
const MAX_STATE_BYTES: usize = 700_000;
/// How long the last readings after a stop may take before the room is told
/// the meeting is over without them.
const CLOSING_GRACE: Duration = Duration::from_secs(600);
const MAX_NAME_CHARS: usize = 40;
/// Names that would pass a teammate's change off as the call's own.
const RESERVED_NAMES: [&str; 3] = ["Sona", "Host", "You"];
const EDIT_ID_CHARS: std::ops::RangeInclusive<usize> = 16..=64;
/// Most words half a second of speech holds.
const REPEATED_WORDS: usize = 4;
/// What teammates see once the meeting was deleted.
pub(crate) const REMOVED: &[u8] = br#"{"v":1,"removed":true}"#;

/// Where the meeting's link is in its life.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SharePhase {
    /// Recording: teammates may edit.
    #[default]
    Live,
    /// The capture stopped and the last readings run; edits are closed.
    Closing,
    /// The last readings are in.
    Ended,
    /// The meeting was deleted: the room is emptied.
    Withdrawn,
}

/// The meeting's link once asked for, and where it is in its life.
#[derive(Default)]
pub(crate) struct Share {
    link: Option<String>,
    phase: SharePhase,
    closed_at: Option<Instant>,
}

impl LiveTranscript {
    fn share(&self) -> MutexGuard<'_, Share> {
        self.share
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The link teammates open, once one was made.
    pub(crate) fn share_link(&self) -> Option<String> {
        self.share().link.clone()
    }

    /// Where the link is now. A stop whose last readings never finish
    /// counts as over once [`CLOSING_GRACE`] has passed.
    pub(crate) fn share_phase(&self) -> SharePhase {
        let share = self.share();
        match (share.phase, share.closed_at) {
            (SharePhase::Closing, Some(at)) if at.elapsed() > CLOSING_GRACE => SharePhase::Ended,
            (phase, _) => phase,
        }
    }

    /// The capture stopped: no more edits from the link.
    pub(crate) fn close_share(&self) {
        let mut share = self.share();
        if share.phase == SharePhase::Live {
            share.phase = SharePhase::Closing;
            share.closed_at = Some(Instant::now());
        }
    }

    /// The last readings are in.
    pub(crate) fn end_share(&self) {
        let mut share = self.share();
        if share.phase != SharePhase::Withdrawn {
            share.phase = SharePhase::Ended;
        }
    }

    /// The meeting was deleted: its room is emptied.
    pub(crate) fn withdraw_share(&self) {
        self.share().phase = SharePhase::Withdrawn;
    }
}

/// The rooms of meetings shared by link, by meeting, so deleting a meeting
/// empties its room whenever the delete comes: during the call, at the
/// stop, or after.
#[derive(Clone, Default)]
pub(crate) struct SharedRooms(Arc<Mutex<HashMap<MeetingSessionId, Arc<Notify>>>>);

impl SharedRooms {
    fn rooms(&self) -> MutexGuard<'_, HashMap<MeetingSessionId, Arc<Notify>>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The meeting was deleted: its room's feed empties the room.
    pub(crate) fn withdraw(&self, session_id: MeetingSessionId) {
        if let Some(deleted) = self.rooms().remove(&session_id) {
            deleted.notify_one();
        }
    }

    /// Word of the meeting's delete, for its room's feed.
    pub(crate) fn watch(&self, session_id: MeetingSessionId) -> DeleteWatch {
        let deleted = Arc::clone(self.rooms().entry(session_id).or_default());
        DeleteWatch {
            rooms: self.clone(),
            session_id,
            deleted,
        }
    }
}

/// Word that a shared meeting was deleted, held by its room's feed. The
/// meeting is forgotten once the feed lets this go.
pub(crate) struct DeleteWatch {
    rooms: SharedRooms,
    session_id: MeetingSessionId,
    deleted: Arc<Notify>,
}

impl DeleteWatch {
    /// Resolves once the meeting is deleted.
    pub(crate) async fn deleted(&self) {
        self.deleted.notified().await;
    }
}

impl Drop for DeleteWatch {
    fn drop(&mut self) {
        self.rooms.rooms().remove(&self.session_id);
    }
}

/// What teammates see, as the room's state frame carries it sealed.
#[derive(Serialize)]
struct State<'a> {
    v: u8,
    title: &'a str,
    live: bool,
    heard_ns: u64,
    points: Vec<Point>,
    cards: Vec<Card>,
    lines: Vec<Line>,
    changes: Vec<MeetingLiveChange>,
}

#[derive(Serialize)]
struct Point {
    id: String,
    text: String,
    said_ns: Option<u64>,
    edited: bool,
}

#[derive(Serialize)]
struct Card {
    id: String,
    what: String,
    owner: Option<String>,
    due_at_ms: Option<i64>,
    due_words: Option<String>,
    said_ns: u64,
    edited: bool,
}

#[derive(Serialize)]
struct Line {
    said_ns: u64,
    who: &'static str,
    text: String,
}

/// The transcript as sentences. The live pass cuts its audio where its
/// speaker guess names a new voice, and again where a reading ends; it
/// reads each piece on its own, so the page showed tails like "fixture." or
/// "about the births." as lines of their own, often punctuated as sentences
/// and under another speaker guess. A pause always ends a piece, so a piece
/// that starts before the one before it on the same track stopped is the
/// rest of it, even when the other track spoke in between.
fn joined(segments: impl Iterator<Item = MeetingProvisionalSegment>) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut open: HashMap<SourceTrackId, (usize, u64)> = HashMap::new();
    for segment in segments {
        let piece = segment.text.trim();
        let index = match open.get(&segment.track_id) {
            Some(&(index, end)) if segment.start_offset_ns <= end => {
                let line = &mut lines[index];
                let rest = unrepeated(&line.text, piece);
                if !rest.is_empty() {
                    if rest.chars().next().is_some_and(char::is_lowercase) && line.text.ends_with('.') {
                        line.text.pop();
                    }
                    line.text.push(' ');
                    line.text.push_str(rest);
                }
                index
            }
            _ => {
                lines.push(Line {
                    said_ns: segment.start_offset_ns,
                    who: match segment.source_kind {
                        SourceKind::Microphone => "You",
                        _ => "Call",
                    },
                    text: piece.to_string(),
                });
                lines.len() - 1
            }
        };
        open.insert(segment.track_id, (index, segment.end_offset_ns));
    }
    lines
}

/// `piece` without the words it repeats from the end of `line`: a cut keeps
/// its last half second for the next piece, and a speaker window can split
/// a word that both sides then read, so a joined line read "to the to the"
/// and "the LED. LED fixtures".
fn unrepeated<'a>(line: &str, piece: &'a str) -> &'a str {
    let bare = |word: &str| word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    let tail: Vec<String> = line.split_whitespace().map(bare).collect();
    for count in (1..=REPEATED_WORDS.min(tail.len())).rev() {
        let mut words = piece.splitn(count + 1, ' ');
        let head: Vec<String> = words.by_ref().take(count).map(bare).collect();
        if head == tail[tail.len() - count..] {
            return words.next().unwrap_or_default().trim_start();
        }
    }
    piece
}

/// A teammate's edit, as their browser sends it sealed: new words for a
/// point or a card, a new point when `target_id` is `None`, or the point
/// gone when the words are blank.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    v: u8,
    id: String,
    name: String,
    target: MeetingLiveTarget,
    target_id: Option<String>,
    text: String,
}

/// A recording meeting as its room sees it.
pub(crate) struct SharedMeeting {
    service: Arc<MeetingProcessingService>,
    store: Arc<MeetingStore>,
    session_id: MeetingSessionId,
    live: Arc<LiveTranscript>,
}

impl SharedMeeting {
    pub(crate) fn new(
        service: Arc<MeetingProcessingService>,
        store: Arc<MeetingStore>,
        session_id: MeetingSessionId,
        live: Arc<LiveTranscript>,
    ) -> Self {
        Self {
            service,
            store,
            session_id,
            live,
        }
    }

    pub(crate) fn live(&self) -> &LiveTranscript {
        &self.live
    }

    /// Keep `link` and its room unless the meeting has a link; returns the
    /// link it keeps and whether that is `link`. The room is stored first,
    /// so deleting the meeting in a later launch still empties it, and a
    /// room the store did not take is never handed out.
    pub(crate) fn keep_link(
        &self,
        link: String,
        room: &LiveRoom,
    ) -> Result<(String, bool), StoreError> {
        let mut share = self.live.share();
        if let Some(kept) = &share.link {
            return Ok((kept.clone(), false));
        }
        self.store.keep_live_room(self.session_id, room)?;
        share.link = Some(link.clone());
        Ok((link, true))
    }

    /// Word of this meeting's delete, for its room's feed.
    pub(crate) fn watch_delete(&self) -> DeleteWatch {
        self.service.shared_rooms().watch(self.session_id)
    }

    /// The state teammates see in `phase`, as JSON.
    pub(crate) fn state(&self, phase: SharePhase) -> Result<Vec<u8>, serde_json::Error> {
        if phase == SharePhase::Withdrawn {
            return Ok(REMOVED.to_vec());
        }
        let title = self
            .store
            .session_snapshot(self.session_id)
            .map(|snapshot| snapshot.title)
            .unwrap_or_default();
        let card = self.live.live_card(self.session_id);
        let mut state = State {
            v: 1,
            title: &title,
            live: phase == SharePhase::Live,
            heard_ns: card.heard_through_ns,
            points: self
                .live
                .doc_points()
                .into_iter()
                .map(|point| Point {
                    id: point.id,
                    text: point.text,
                    said_ns: point.start_offset_ns,
                    edited: point.edited,
                })
                .collect(),
            cards: card
                .promises
                .into_iter()
                .map(|promise| Card {
                    id: promise.id,
                    what: promise.what,
                    owner: promise.owner_name,
                    due_at_ms: promise.due_at_ms,
                    due_words: promise.due_words,
                    said_ns: promise.start_offset_ns,
                    edited: promise.edited,
                })
                .collect(),
            lines: joined(
                self.live
                    .snapshot(self.session_id)
                    .segments
                    .into_iter()
                    .filter(|segment| {
                        segment.source_kind != SourceKind::Microphone
                            || live_doc::voice_ask(&segment.text).is_none()
                    }),
            ),
            changes: self.live.live_changes(),
        };
        loop {
            let json = serde_json::to_vec(&state)?;
            if json.len() <= MAX_STATE_BYTES || state.lines.is_empty() {
                return Ok(json);
            }
            let over = state.lines.len().div_ceil(10);
            state.lines.drain(..over);
        }
    }

    /// A teammate's edit while the meeting records, kept, saved and told to
    /// the screens. Returns whether it changed the doc or the board.
    pub(crate) fn apply(&self, edit: &[u8]) -> bool {
        let Ok(edit) = serde_json::from_slice::<Edit>(edit) else {
            return false;
        };
        let name = edit.name.split_whitespace().collect::<Vec<_>>().join(" ");
        let fits = edit.v == 1
            && EDIT_ID_CHARS.contains(&edit.id.len())
            && edit
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            && (1..=MAX_NAME_CHARS).contains(&name.chars().count())
            && !name.chars().any(char::is_control)
            && !RESERVED_NAMES
                .iter()
                .any(|reserved| name.eq_ignore_ascii_case(reserved));
        if !fits || self.live.share_phase() != SharePhase::Live || self.live.has_change(&edit.id) {
            return false;
        }
        let author = Author::Teammate {
            name,
            edit_id: edit.id,
        };
        match edit.target {
            MeetingLiveTarget::Point => {
                if self
                    .live
                    .edit_doc(&author, edit.target_id.as_deref(), &edit.text)
                    .is_err()
                {
                    return false;
                }
                live_doc::save(&self.store, self.session_id, &self.live);
            }
            MeetingLiveTarget::Card => {
                let Some(promise_id) = edit.target_id else {
                    return false;
                };
                let Some(due_at_ms) = self
                    .live
                    .live_promises()
                    .into_iter()
                    .find(|promise| promise.id == promise_id)
                    .map(|promise| promise.due_at_ms)
                else {
                    return false;
                };
                if self
                    .live
                    .edit_live_promise(&author, &promise_id, &edit.text, due_at_ms)
                    .is_none()
                {
                    return false;
                }
                live_changes::save(&self.store, self.session_id, &self.live);
                self.service.emit_current(
                    &self.store,
                    "meeting:live-card-changed",
                    self.session_id,
                );
            }
        }
        self.service
            .emit_current(&self.store, "meeting:live-doc-changed", self.session_id);
        true
    }
}
