//! The room on the companion that a recording meeting's link opens. The
//! first ask for the link makes the room; from then on the meeting's doc,
//! board and transcript go to it sealed, at most once a second and only
//! when they changed, and teammates' sealed edits come back from it.
//! Deleting the meeting, during the call or after it, in this launch or a
//! later one, empties the room. The companion never holds the key: it
//! travels in the link's fragment.

use super::*;
use crate::cloud_sync::client::LiveRoomPlan;
use crate::cloud_sync::crypto::{derive_live_room_keys, open_live_frame, seal_live_frame};
use crate::meeting::processing::live_share::{
    DeleteWatch, SharePhase, SharedMeeting, SharedRooms, REMOVED,
};
use crate::meeting::store::LiveRoom;
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use tokio_tungstenite::tungstenite::Message;
use zeroize::Zeroizing;

const PUBLISH_EVERY: Duration = Duration::from_secs(1);
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LAST_RETRY: Duration = Duration::from_secs(30);
/// How long a call that ended keeps trying to hand the room its last state.
const FINAL_TRIES_FOR: Duration = Duration::from_secs(120);
/// The close code a room gives a host socket another one replaced.
const REPLACED: u16 = 4000;
/// The close code a socket gets once its room is gone.
const GONE: u16 = 4404;
/// The frame count the deleted state goes under: the highest a page reads,
/// so the room takes it over any state it holds, even one an earlier
/// launch sent.
const DELETED_SEQ: u64 = (1 << 53) - 1;

impl CloudSyncRuntime {
    /// The link teammates open to follow and edit a recording meeting's doc
    /// and board. The first ask makes the room and starts feeding it; later
    /// asks get the same link.
    pub(crate) async fn live_share_link(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<String, CloudRuntimeError> {
        let meeting = self
            .meetings
            .shared_meeting(session_id)
            .await
            .map_err(|_| CloudRuntimeError::Conflict)?;
        if let Some(link) = meeting.live().share_link() {
            return Ok(link);
        }
        self.reject_portable()?;
        let access = self
            .vault_access()
            .await?
            .ok_or(CloudRuntimeError::SetupRequired)?;
        let room_id = random_opaque_id()?;
        let root = Zeroizing::new(random_array::<32>()?);
        let host_token = Zeroizing::new(random_array::<32>()?);
        let keys = derive_live_room_keys(root.as_ref(), &room_id)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let frame_key = Zeroizing::new(keys.frame_key);
        let join_token = Zeroizing::new(keys.join_token);
        let created = access
            .client
            .create_live_room(
                &credentials(&access)?,
                &idempotency_key(&["live_room", &room_id])?,
                &LiveRoomPlan {
                    version: 1,
                    room_id: room_id.clone(),
                    host_verifier: sha256_base64url(host_token.as_ref()),
                    join_verifier: sha256_base64url(join_token.as_ref()),
                },
            )
            .await
            .map_err(CloudRuntimeError::Client)?;
        if created.room_id != room_id {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let page = room_page(&created.page_url, &room_id)?;
        let room = LiveRoom {
            socket: room_socket(&access.state.endpoint, &room_id, host_token.as_ref())?,
            id: room_id,
            frame_key,
            expires_at_utc_ms: i64::try_from(created.expires_at)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        };
        let link = format!("{page}#v=1&k={}", base64_url_encode(root.as_ref()));
        let (link, kept) = meeting.keep_link(link, &room).map_err(map_store_error)?;
        if kept {
            log::info!("Made a live room for meeting {session_id:?}");
            tauri::async_runtime::spawn(feed(Arc::clone(&access.store), Arc::new(meeting), room));
        }
        Ok(link)
    }

    /// Watch again for the deletes of meetings whose rooms earlier launches
    /// made, and empty the rooms of those deleted while no launch watched.
    pub(super) async fn rewatch_live_rooms(&self) {
        let Ok(store) = self.meetings.cloud_store().await else {
            return;
        };
        if let Err(error) = rewatch(&store, self.meetings.shared_rooms()) {
            log::warn!("The live rooms of earlier launches are not watched: {error:?}");
        }
    }
}

/// The room's page from the companion's answer: an https address whose
/// path is the room's and that carries nothing else.
fn room_page(page: &str, room_id: &str) -> Result<Url, CloudRuntimeError> {
    let url = Url::parse(page).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    if url.scheme() != "https"
        || url.path() != format!("/l/{room_id}")
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    Ok(url)
}

/// Where the Mac feeds the room: the companion's own address, over a
/// secure socket, with the token only the Mac holds.
fn room_socket(endpoint: &str, room_id: &str, host_token: &[u8]) -> Result<String, CloudRuntimeError> {
    let mut url = Url::parse(endpoint).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    url.set_scheme("wss")
        .map_err(|()| CloudRuntimeError::IntegrityFailure)?;
    url.path_segments_mut()
        .map_err(|()| CloudRuntimeError::IntegrityFailure)?
        .pop_if_empty()
        .extend(["v1", "live", room_id, "socket"]);
    url.set_fragment(None);
    url.query_pairs_mut()
        .clear()
        .append_pair("t", &base64_url_encode(host_token));
    Ok(url.into())
}

/// What the room has been sent so far; the frame count runs on across
/// sockets, so no two of the meeting's states share one.
struct Fed {
    seq: u64,
    /// The state last sent on this socket, and whether it was the last.
    sent: Option<(Vec<u8>, bool)>,
}

enum Served {
    /// The room has the last state.
    Delivered,
    /// Another Mac feeds the room, the room is gone, or the state could
    /// not be made.
    Over,
    Dropped,
}

/// Feed the room until it has the meeting's last state. Unless the room
/// was emptied during the call, wait for the meeting's delete after it and
/// empty the room then.
async fn feed(store: Arc<MeetingStore>, meeting: Arc<SharedMeeting>, room: LiveRoom) {
    let watch = meeting.watch_delete();
    let mut fed = Fed { seq: 0, sent: None };
    let delivered = deliver(Some(&meeting), &watch, &room, &mut fed).await;
    if meeting.live().share_phase() == SharePhase::Withdrawn {
        if delivered {
            forget(&store, &room);
        }
        return;
    }
    drop(meeting);
    watch.deleted().await;
    empty(&store, &watch, &room).await;
}

/// Watch for the deletes of the meetings whose rooms `store` keeps, and
/// empty each room once its meeting is deleted.
fn rewatch(store: &Arc<MeetingStore>, rooms: &SharedRooms) -> Result<(), StoreError> {
    for kept in store.kept_live_rooms(utc_now_ms())? {
        let watch = rooms.watch(kept.session_id);
        let store = Arc::clone(store);
        tauri::async_runtime::spawn(async move {
            if !kept.deleted {
                watch.deleted().await;
            }
            empty(&store, &watch, &kept.room).await;
        });
    }
    Ok(())
}

/// Tell the room its meeting was deleted, and let the room go once it
/// knows. A room that never hears it before the app quits is told at the
/// next launch.
async fn empty(store: &MeetingStore, watch: &DeleteWatch, room: &LiveRoom) {
    if deliver(None, watch, room, &mut Fed { seq: 0, sent: None }).await {
        forget(store, room);
    }
}

fn forget(store: &MeetingStore, room: &LiveRoom) {
    if let Err(error) = store.forget_live_room(&room.id) {
        log::warn!("A live room that knows its meeting was deleted stays kept: {error:?}");
    }
}

/// Hand the room states until it has the last one: the meeting's, or with
/// no meeting, that it was deleted. Returns whether the room got it.
async fn deliver(
    meeting: Option<&Arc<SharedMeeting>>,
    watch: &DeleteWatch,
    room: &LiveRoom,
    fed: &mut Fed,
) -> bool {
    let mut retry = FIRST_RETRY;
    let mut over_since = None;
    loop {
        match tokio_tungstenite::connect_async(room.socket.as_str()).await {
            Ok((socket, _)) => {
                retry = FIRST_RETRY;
                over_since = None;
                match serve(meeting, watch, room, fed, socket).await {
                    Served::Delivered => return true,
                    Served::Over => return false,
                    Served::Dropped => {}
                }
            }
            Err(error) => log::debug!("The live room could not be reached: {error}"),
        }
        let phase = meeting.map_or(SharePhase::Withdrawn, |meeting| meeting.live().share_phase());
        if !tries_again(phase, &mut over_since, room) {
            log::warn!("The live room never got the meeting's last state");
            return false;
        }
        tokio::time::sleep(retry).await;
        retry = (retry * 2).min(LAST_RETRY);
    }
}

/// Whether to try the room again: always during the call, for
/// [`FINAL_TRIES_FOR`] once the call's last state first failed to go out,
/// and until the room expires once the meeting is deleted, so a delete made
/// offline still empties the room when the Mac is back online.
fn tries_again(
    phase: SharePhase,
    over_since: &mut Option<std::time::Instant>,
    room: &LiveRoom,
) -> bool {
    match phase {
        SharePhase::Live => true,
        SharePhase::Closing | SharePhase::Ended => {
            over_since.get_or_insert_with(std::time::Instant::now).elapsed() <= FINAL_TRIES_FOR
        }
        SharePhase::Withdrawn => utc_now_ms() < room.expires_at_utc_ms,
    }
}

async fn serve<S>(
    meeting: Option<&Arc<SharedMeeting>>,
    watch: &DeleteWatch,
    room: &LiveRoom,
    fed: &mut Fed,
    socket: S,
) -> Served
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    let (mut sink, mut stream) = socket.split();
    fed.sent = None;
    let mut tick = tokio::time::interval(PUBLISH_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let Some((state, last)) = next_state(meeting).await else {
                    return Served::Over;
                };
                let sent = fed.sent.as_ref().map(|(sent, was_last)| (sent.as_slice(), *was_last));
                if sent != Some((state.as_slice(), last)) {
                    let seq = if meeting.is_some() { fed.seq + 1 } else { DELETED_SEQ };
                    let Ok(frame) = state_frame(room, seq, &state, last) else {
                        return Served::Over;
                    };
                    if sink.send(Message::text(frame)).await.is_err() {
                        return Served::Dropped;
                    }
                    fed.seq = seq;
                    fed.sent = Some((state, last));
                }
                if last {
                    if let Err(error) = sink.close().await {
                        log::debug!("The live room socket did not close cleanly: {error}");
                    }
                    return Served::Delivered;
                }
            }
            () = watch.deleted(), if meeting.is_some() => {
                if let Some(meeting) = meeting {
                    meeting.live().withdraw_share();
                }
            }
            message = stream.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    if let Some(meeting) = meeting {
                        hear(meeting, room, text.as_str()).await;
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    if frame.is_some_and(|frame| matches!(u16::from(frame.code), REPLACED | GONE)) {
                        return Served::Over;
                    }
                    return Served::Dropped;
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return Served::Dropped,
            },
        }
    }
}

/// The state to send next and whether it is the last: the meeting's, or
/// with no meeting, that it was deleted. The room is told the call is over
/// only once the stop's last readings are in.
async fn next_state(meeting: Option<&Arc<SharedMeeting>>) -> Option<(Vec<u8>, bool)> {
    let Some(meeting) = meeting else {
        return Some((REMOVED.to_vec(), true));
    };
    let phase = meeting.live().share_phase();
    let reading = Arc::clone(meeting);
    let state = tauri::async_runtime::spawn_blocking(move || reading.state(phase))
        .await
        .ok()?
        .ok()?;
    Some((
        state,
        matches!(phase, SharePhase::Ended | SharePhase::Withdrawn),
    ))
}

#[derive(Serialize)]
struct StateFrame<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    seq: u64,
    #[serde(rename = "box")]
    sealed: &'a str,
    #[serde(rename = "final")]
    last: bool,
}

#[derive(Deserialize)]
struct Incoming {
    #[serde(rename = "type")]
    kind: String,
    seq: Option<u64>,
    #[serde(rename = "box")]
    sealed: Option<String>,
}

fn state_frame(
    room: &LiveRoom,
    seq: u64,
    state: &[u8],
    last: bool,
) -> Result<String, CloudRuntimeError> {
    let nonce = random_array::<12>()?;
    let sealed = seal_live_frame(&room.frame_key, &room.id, "state", seq, &nonce, state)
        .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    serde_json::to_string(&StateFrame {
        kind: "state",
        seq,
        sealed: &base64_url_encode(&sealed),
        last,
    })
    .map_err(|_| CloudRuntimeError::IntegrityFailure)
}

/// A teammate's edit from the room, opened and applied. Anything else the
/// room says, or an edit that does not open, is let go.
async fn hear(meeting: &Arc<SharedMeeting>, room: &LiveRoom, text: &str) {
    let Ok(Incoming {
        kind,
        seq: Some(seq),
        sealed: Some(sealed),
    }) = serde_json::from_str(text)
    else {
        return;
    };
    if kind != "edit" {
        return;
    }
    let Ok(sealed) = base64_url_decode(&sealed) else {
        return;
    };
    let Ok(edit) = open_live_frame(&room.frame_key, &room.id, "edit", seq, &sealed) else {
        log::debug!("A live room edit did not open");
        return;
    };
    let applying = Arc::clone(meeting);
    if let Err(error) = tauri::async_runtime::spawn_blocking(move || applying.apply(&edit)).await {
        log::warn!("A live room edit could not be applied: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::store::workflow_core_tests;
    use crate::meeting::types::{DeletionCause, MeetingOperationId};
    use tokio::net::TcpListener;

    /// A room an earlier launch made for the meeting, fed through a local
    /// socket that stands in for the companion.
    fn kept_room(store: &MeetingStore, session_id: MeetingSessionId, listener: &TcpListener) -> LiveRoom {
        let room = LiveRoom {
            id: "harbor-room".to_owned(),
            socket: format!(
                "ws://{}/v1/live/harbor-room/socket?t=host",
                listener.local_addr().unwrap()
            ),
            frame_key: Zeroizing::new([7; 32]),
            expires_at_utc_ms: utc_now_ms() + 60_000,
        };
        store.keep_live_room(session_id, &room).unwrap();
        room
    }

    /// The first state the room is sent, opened: its count, whether it is
    /// the last, and what it says.
    async fn first_state(listener: &TcpListener, room: &LiveRoom) -> (u64, bool, Vec<u8>) {
        let (socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .expect("the room is fed")
            .unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        let Some(Ok(Message::Text(text))) = socket.next().await else {
            panic!("the room is sent a state first");
        };
        while socket.next().await.is_some() {}
        let frame: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
        let seq = frame["seq"].as_u64().unwrap();
        let sealed = base64_url_decode(frame["box"].as_str().unwrap()).unwrap();
        (
            seq,
            frame["final"].as_bool().unwrap(),
            open_live_frame(&room.frame_key, &room.id, "state", seq, &sealed).unwrap(),
        )
    }

    async fn assert_rooms_let_go(store: &MeetingStore) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !store.kept_live_rooms(utc_now_ms()).unwrap().is_empty() {
            assert!(tokio::time::Instant::now() < deadline, "the emptied room stays kept");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[test]
    fn deleting_a_meeting_shared_in_an_earlier_launch_empties_its_room() {
        let (_directory, store) = workflow_core_tests::store();
        let session_id =
            workflow_core_tests::reviewable_meeting(&store, "Harbor map sync", utc_now_ms());
        tauri::async_runtime::block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let room = kept_room(&store, session_id, &listener);
            let rooms = SharedRooms::default();
            rewatch(&store, &rooms).unwrap();
            rooms.withdraw(session_id);
            assert_eq!(
                first_state(&listener, &room).await,
                (9_007_199_254_740_991, true, REMOVED.to_vec())
            );
            assert_rooms_let_go(&store).await;
        });
    }

    #[test]
    fn a_meeting_deleted_while_no_launch_watched_has_its_room_emptied_at_the_next() {
        let (_directory, store) = workflow_core_tests::store();
        let session_id =
            workflow_core_tests::reviewable_meeting(&store, "Harbor map sync", utc_now_ms());
        tauri::async_runtime::block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let room = kept_room(&store, session_id, &listener);
            let revision = store.session_snapshot(session_id).unwrap().revision;
            store
                .reserve_deletion(
                    MeetingOperationId::new(),
                    utc_now_ms(),
                    session_id,
                    revision,
                    DeletionCause::User,
                )
                .unwrap();
            rewatch(&store, &SharedRooms::default()).unwrap();
            assert_eq!(
                first_state(&listener, &room).await,
                (9_007_199_254_740_991, true, REMOVED.to_vec())
            );
            assert_rooms_let_go(&store).await;
        });
    }

    #[test]
    fn a_meeting_deleted_offline_keeps_trying_its_room_until_the_room_expires() {
        let room = LiveRoom {
            id: "harbor-room".to_owned(),
            socket: String::new(),
            frame_key: Zeroizing::new([7; 32]),
            expires_at_utc_ms: utc_now_ms() + 60_000,
        };
        let failing_since = std::time::Instant::now()
            .checked_sub(FINAL_TRIES_FOR + Duration::from_secs(1))
            .unwrap();
        assert!(!tries_again(SharePhase::Ended, &mut Some(failing_since), &room));
        assert!(tries_again(SharePhase::Withdrawn, &mut Some(failing_since), &room));
        let expired = LiveRoom {
            expires_at_utc_ms: utc_now_ms() - 1,
            ..room
        };
        assert!(!tries_again(SharePhase::Withdrawn, &mut None, &expired));
    }
}
