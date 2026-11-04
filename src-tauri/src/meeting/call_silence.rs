//! Whether a call's sound has stopped reaching the recording while the
//! person is still talking: the sign a call app has dropped out of the
//! system-audio capture. Each track's level is read as its packets go to
//! disk. It only ever warns; the recording carries on, and a quiet call is
//! not a partial one.

use super::types::SourceKind;
use serde::Serialize;
use specta::Type;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const WINDOW_NS: u64 = 500_000_000;
const NS_PER_SECOND: u64 = 1_000_000_000;
/// Call audio at -80 dBFS: under the line and room noise of any real call,
/// so only a track that carries no sound at all stays below it.
const CALL_SILENT_RMS: f64 = 1.0e-4;
/// A microphone window is speech when it stands this far over the quietest
/// window of the last twenty seconds, which is the room the person is in.
const VOICE_OVER_FLOOR: f64 = 4.0;
/// And never below -50 dBFS, so a dead-quiet room does not read every
/// breath as speech.
const VOICE_MIN_RMS: f64 = 3.0e-3;
const FLOOR_WINDOWS: usize = 40;
/// How long the call stays silent, with the person heard inside the same
/// span, before the screen says so.
pub(crate) const ALARM_NS: u64 = 30_000_000_000;

/// `meeting_call_silence`: how long the running call has had no sound
/// while the person talked, in milliseconds, or `None` when it has sound,
/// when the person is quiet too, or when this meeting is not capturing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
pub struct MeetingCallSilence {
    pub silent_ms: Option<u64>,
}

/// One capture's two levels, shared between its track workers and the
/// screen that asks.
pub(crate) struct CaptureAudibility {
    /// Call audio in a row that held no sound, counted from the first window
    /// that had some: a call track silent from the start is no call at all.
    /// Zero once a window has sound again.
    call_silent_ns: AtomicU64,
    /// Microphone audio since its last window of speech. `u64::MAX` until
    /// the first.
    mic_quiet_ns: AtomicU64,
}

impl CaptureAudibility {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            call_silent_ns: AtomicU64::new(0),
            mic_quiet_ns: AtomicU64::new(u64::MAX),
        })
    }

    pub(crate) fn meter(self: &Arc<Self>, source: SourceKind) -> TrackMeter {
        TrackMeter {
            shared: Arc::clone(self),
            source,
            sum_squares: 0.0,
            samples: 0,
            window_ns: 0,
            recent: VecDeque::with_capacity(FLOOR_WINDOWS),
            heard: false,
        }
    }

    /// How long the call has had no sound, once that is thirty seconds or
    /// more and the microphone heard speech within the last thirty. Both
    /// sides quiet is a pause in the conversation, not a lost call.
    pub(crate) fn call_silent_ns(&self) -> Option<u64> {
        let silent = self.call_silent_ns.load(Ordering::Relaxed);
        let mic_quiet = self.mic_quiet_ns.load(Ordering::Relaxed);
        (silent >= ALARM_NS && mic_quiet <= ALARM_NS).then_some(silent)
    }
}

/// One track's level, read in half-second windows.
pub(crate) struct TrackMeter {
    shared: Arc<CaptureAudibility>,
    source: SourceKind,
    sum_squares: f64,
    samples: u32,
    window_ns: u64,
    recent: VecDeque<f64>,
    heard: bool,
}

impl TrackMeter {
    /// One packet of interleaved samples, as the track writer took it.
    pub(crate) fn feed(&mut self, samples: &[f32], sample_rate_hz: u32, channels: u16) {
        if sample_rate_hz == 0 || samples.is_empty() {
            return;
        }
        let frames = samples.len() / usize::from(channels.max(1));
        let duration_ns = u64::try_from(frames)
            .unwrap_or(u64::MAX)
            .saturating_mul(NS_PER_SECOND)
            / u64::from(sample_rate_hz);
        self.sum_squares += samples
            .iter()
            .map(|sample| f64::from(*sample) * f64::from(*sample))
            .sum::<f64>();
        self.samples = self
            .samples
            .saturating_add(u32::try_from(samples.len()).unwrap_or(u32::MAX));
        self.window_ns += duration_ns;
        if self.window_ns >= WINDOW_NS {
            let rms = (self.sum_squares / f64::from(self.samples)).sqrt();
            self.close(rms, self.window_ns);
            self.sum_squares = 0.0;
            self.samples = 0;
            self.window_ns = 0;
        }
    }

    fn close(&mut self, rms: f64, window_ns: u64) {
        match self.source {
            SourceKind::SystemAudio => {
                if rms >= CALL_SILENT_RMS {
                    self.heard = true;
                    self.shared.call_silent_ns.store(0, Ordering::Relaxed);
                } else if self.heard {
                    self.shared
                        .call_silent_ns
                        .fetch_add(window_ns, Ordering::Relaxed);
                }
            }
            SourceKind::Microphone => {
                let floor = self.recent.iter().copied().fold(f64::INFINITY, f64::min);
                if self.recent.len() == FLOOR_WINDOWS {
                    self.recent.pop_front();
                }
                self.recent.push_back(rms);
                let quiet = &self.shared.mic_quiet_ns;
                if rms >= VOICE_MIN_RMS && rms >= floor * VOICE_OVER_FLOOR {
                    quiet.store(0, Ordering::Relaxed);
                } else {
                    let since = quiet.load(Ordering::Relaxed).saturating_add(window_ns);
                    quiet.store(since, Ordering::Relaxed);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;
    const SECOND_NS: u64 = 1_000_000_000;

    /// `tenths` tenths of a second of one steady level, in 100 ms packets.
    fn feed(meter: &mut TrackMeter, level: f32, tenths: u32) {
        let packet = vec![level; RATE as usize / 10];
        for _ in 0..tenths {
            meter.feed(&packet, RATE, 1);
        }
    }

    /// One second of a call with no sound beside a person who talks for
    /// half of it over a quiet room.
    fn talking_into_silence(call: &mut TrackMeter, mic: &mut TrackMeter) {
        feed(call, 0.0, 10);
        feed(mic, 0.05, 5);
        feed(mic, 0.002, 5);
    }

    #[test]
    fn a_silent_call_warns_only_while_the_person_is_heard() {
        let audibility = CaptureAudibility::new();
        let mut call = audibility.meter(SourceKind::SystemAudio);
        let mut mic = audibility.meter(SourceKind::Microphone);
        feed(&mut call, 0.05, 50);
        feed(&mut mic, 0.002, 50);

        for _ in 0..29 {
            talking_into_silence(&mut call, &mut mic);
        }
        assert_eq!(audibility.call_silent_ns(), None);
        for _ in 0..2 {
            talking_into_silence(&mut call, &mut mic);
        }
        assert_eq!(audibility.call_silent_ns(), Some(31 * SECOND_NS));

        // Both sides quiet for longer than the window is a pause.
        feed(&mut call, 0.0, 310);
        feed(&mut mic, 0.002, 310);
        assert_eq!(audibility.call_silent_ns(), None);

        // Sound from the call starts the count again.
        feed(&mut call, 0.05, 10);
        for _ in 0..10 {
            talking_into_silence(&mut call, &mut mic);
        }
        assert_eq!(audibility.call_silent_ns(), None);
    }

    #[test]
    fn a_call_track_that_never_had_sound_never_warns() {
        let audibility = CaptureAudibility::new();
        let mut call = audibility.meter(SourceKind::SystemAudio);
        let mut mic = audibility.meter(SourceKind::Microphone);
        feed(&mut mic, 0.002, 50);
        for _ in 0..40 {
            talking_into_silence(&mut call, &mut mic);
        }
        assert_eq!(audibility.call_silent_ns(), None);
    }
}
