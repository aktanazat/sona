//! The room on the companion that a recording meeting's link opens. The
//! first ask for the link makes the room; from then on the meeting's doc,
//! board and transcript go to it sealed, at most once a second and only
//! when they changed, and teammates' sealed edits come back from it.
//! Deleting the meeting, during the call or after it, empties the room.
//! The companion never holds the key: it travels in the link's fragment.

use super::*;
use crate::cloud_sync::client::LiveRoomPlan;
use crate::cloud_sync::crypto::{derive_live_room_keys, open_live_frame, seal_live_frame};
use crate::meeting::processing::live_share::{DeleteWatch, SharePhase, SharedMeeting, REMOVED};
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use tokio_tungstenite::tungstenite::Message;
use zeroize::Zeroizing;

const PUBLISH_EVERY: Duration = Duration::from_secs(1);
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LAST_RETRY: Duration = Duration::from_secs(30);
/// How long a meeting that is over keeps trying to hand the room its last
/// state.
const FINAL_TRIES_FOR: Duration = Duration::from_secs(120);
/// The close code a room gives a host socket another one replaced.
const REPLACED: u16 = 4000;

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
        let socket = room_socket(&access.state.endpoint, &room_id, host_token.as_ref())?;
        let link = format!("{page}#v=1&k={}", base64_url_encode(root.as_ref()));
        let (link, kept) = meeting.live().keep_share_link(link);
        if kept {
            log::info!("Made a live room for meeting {session_id:?}");
            tauri::async_runtime::spawn(feed(
                Arc::new(meeting),
                Room {
                    id: room_id,
                    socket,
                    frame_key,
                },
            ));
        }
        Ok(link)
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
fn room_socket(endpoint: &str, room_id: &str, host_token: &[u8]) -> Result<Url, CloudRuntimeError> {
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
    Ok(url)
}

struct Room {
    id: String,
    socket: Url,
    frame_key: Zeroizing<[u8; 32]>,
}

/// What the room has been sent so far; the frame count runs on across
/// sockets, so no two frames share one.
struct Fed {
    seq: u64,
    /// The state last sent on this socket, and whether it was the last.
    sent: Option<(Vec<u8>, bool)>,
}

enum Served {
    /// The room has the last state.
    Delivered,
    /// Another Mac feeds the room, or the state could not be made.
    Over,
    Dropped,
}

/// Feed the room until it has the meeting's last state. After the call,
/// wait for the meeting's delete and empty the room then.
async fn feed(meeting: Arc<SharedMeeting>, room: Room) {
    let watch = meeting.watch_delete();
    let mut fed = Fed { seq: 0, sent: None };
    if !deliver(Some(&meeting), &watch, &room, &mut fed).await
        || meeting.live().share_phase() != SharePhase::Ended
    {
        return;
    }
    drop(meeting);
    watch.deleted().await;
    deliver(None, &watch, &room, &mut fed).await;
}

/// Hand the room states until it has the last one: the meeting's, or with
/// no meeting, that it was deleted. Returns whether the room got it.
async fn deliver(
    meeting: Option<&Arc<SharedMeeting>>,
    watch: &DeleteWatch,
    room: &Room,
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
        if meeting.is_none_or(|meeting| meeting.live().share_phase() != SharePhase::Live) {
            let since = *over_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() > FINAL_TRIES_FOR {
                log::warn!("The live room never got the meeting's last state");
                return false;
            }
        }
        tokio::time::sleep(retry).await;
        retry = (retry * 2).min(LAST_RETRY);
    }
}

async fn serve<S>(
    meeting: Option<&Arc<SharedMeeting>>,
    watch: &DeleteWatch,
    room: &Room,
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
                    let Ok(frame) = state_frame(room, fed.seq + 1, &state, last) else {
                        return Served::Over;
                    };
                    if sink.send(Message::text(frame)).await.is_err() {
                        return Served::Dropped;
                    }
                    fed.seq += 1;
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
                    if frame.is_some_and(|frame| u16::from(frame.code) == REPLACED) {
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
    room: &Room,
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
async fn hear(meeting: &Arc<SharedMeeting>, room: &Room, text: &str) {
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
