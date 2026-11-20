//! Tells the person dictating when the network is holding up a cloud
//! transcription or a cloud cleanup. Local models never report here: only the
//! cloud transcription worker and requests to a remote cleanup endpoint do.

use serde::Serialize;
use std::future::Future;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub const SLOW_CONNECTION_EVENT: &str = "cloud-connection-slow";

/// Opening the provider's session; a healthy one takes a few hundred
/// milliseconds.
#[cfg(feature = "cloud-realtime")]
pub const SLOW_CONNECT: Duration = Duration::from_secs(2);
/// Speech sent with no word back from the provider. Live providers answer
/// within about a second of speech.
#[cfg(feature = "cloud-realtime")]
const SLOW_REPLY_AUDIO: Duration = Duration::from_secs(4);
/// Waiting for the provider's final words after the recording ends.
#[cfg(feature = "cloud-realtime")]
const SLOW_FINAL: Duration = Duration::from_millis(2500);
/// A cleanup request to a remote endpoint. Cleanup asks for no reasoning and
/// usually answers within three seconds.
pub const SLOW_CLEANUP: Duration = Duration::from_secs(6);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SlowStage {
    #[cfg(feature = "cloud-realtime")]
    Transcription,
    Cleanup,
}

#[derive(Clone, Debug, Serialize)]
struct SlowConnectionEvent {
    stage: SlowStage,
}

pub fn announce(app: &AppHandle, stage: SlowStage) {
    log::warn!("A slow connection is delaying cloud {stage:?}");
    let _ = app.emit(SLOW_CONNECTION_EVENT, SlowConnectionEvent { stage });
}

/// One cloud transcription session. Each method returns true only the first
/// time the session turns slow, so a dictation is warned once.
#[cfg(feature = "cloud-realtime")]
#[derive(Default)]
pub struct TranscriptionLatency {
    warned: bool,
    audio_since_reply: Duration,
}

#[cfg(feature = "cloud-realtime")]
impl TranscriptionLatency {
    pub fn connected(&mut self, took: Duration) -> bool {
        self.warn_if(took >= SLOW_CONNECT)
    }

    pub fn sent(&mut self, audio: Duration) -> bool {
        self.audio_since_reply += audio;
        self.warn_if(self.audio_since_reply >= SLOW_REPLY_AUDIO)
    }

    pub fn replied(&mut self) {
        self.audio_since_reply = Duration::ZERO;
    }

    pub fn awaiting_final(&mut self, waited: Duration) -> bool {
        self.warn_if(waited >= SLOW_FINAL)
    }

    /// The network failed or the final words never came.
    pub fn timed_out(&mut self) -> bool {
        self.warn_if(true)
    }

    fn warn_if(&mut self, slow: bool) -> bool {
        let first = slow && !self.warned;
        self.warned |= slow;
        first
    }
}

/// Await `request`, calling `on_slow` once if it is still waiting after
/// `after`. The request itself is never cut short.
pub async fn warn_when_slow<F: Future>(
    request: F,
    after: Duration,
    on_slow: impl FnOnce(),
) -> F::Output {
    tokio::pin!(request);
    tokio::select! {
        output = &mut request => return output,
        () = tokio::time::sleep(after) => on_slow(),
    }
    request.await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[cfg(feature = "cloud-realtime")]
    const FRAME: Duration = Duration::from_millis(30);

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn a_quick_session_never_warns() {
        let mut latency = TranscriptionLatency::default();
        assert!(!latency.connected(Duration::from_millis(400)));
        for frame in 0..400 {
            assert!(!latency.sent(FRAME));
            if frame % 20 == 0 {
                latency.replied();
            }
        }
        assert!(!latency.awaiting_final(Duration::from_millis(900)));
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn speech_without_replies_warns_once() {
        let mut latency = TranscriptionLatency::default();
        let warnings = (0..300).filter(|_| latency.sent(FRAME)).count();
        assert_eq!(warnings, 1);
        assert!(!latency.awaiting_final(Duration::from_secs(5)));
        assert!(!latency.timed_out());
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn each_slow_stage_warns() {
        assert!(TranscriptionLatency::default().connected(SLOW_CONNECT));
        assert!(TranscriptionLatency::default().awaiting_final(SLOW_FINAL));
        assert!(TranscriptionLatency::default().timed_out());
        assert!(!TranscriptionLatency::default().awaiting_final(SLOW_FINAL - FRAME));
    }

    #[tokio::test]
    async fn a_request_past_the_threshold_warns_and_still_answers() {
        let threshold = Duration::from_millis(40);
        let warned = Cell::new(0);
        let slow = async {
            tokio::time::sleep(threshold * 3).await;
            "slow"
        };
        let answer = warn_when_slow(slow, threshold, || warned.set(warned.get() + 1)).await;
        assert_eq!((answer, warned.get()), ("slow", 1));

        let quick = async { "quick" };
        let answer = warn_when_slow(quick, threshold, || warned.set(warned.get() + 1)).await;
        assert_eq!((answer, warned.get()), ("quick", 1));
    }
}
