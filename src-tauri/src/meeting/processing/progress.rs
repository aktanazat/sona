//! What a running pipeline tells the screen: which of its five stages it is
//! in, how far through that stage, and how long the rest should take.
//!
//! Time left is read only from this Mac's own finished runs. A stage that has
//! never been measured here says nothing about time rather than guessing, and
//! once a finish time has been shown it moves later at most once a stage: a
//! countdown that keeps climbing back up is worse than none.

use crate::meeting::store::processing_speed::{ProcessingWork, StageSpeed};
use crate::meeting::types::{
    MeetingArtifactId, MeetingTrackSnapshot, ProcessingProgress, ProcessingReceipt,
    ProcessingRoute, ProcessingStage,
};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// The least time between two updates inside one stage.
const UPDATE_INTERVAL: Duration = Duration::from_millis(250);
/// The least a finish time has to move later before the move is shown.
const LATER_FLOOR: Duration = Duration::from_secs(3);

pub(super) struct RunProgress {
    started_at_utc_ms: i64,
    run_started: Instant,
    run_paused: Duration,
    work: ProcessingWork,
    speeds: BTreeMap<ProcessingStage, StageSpeed>,
    stage: ProcessingStage,
    stage_started: Instant,
    stage_paused: Duration,
    /// Whether the stage did the work its pace is measured on. A stage that
    /// returned early, such as diarization with no model on disk, finished
    /// fast because it skipped the work, and would teach the estimate that.
    stage_worked: bool,
    done_permille: u16,
    last_update: Instant,
    finish_at: Option<Instant>,
    moved_later: bool,
    measured: Vec<(ProcessingStage, StageSpeed)>,
    notes: Option<(MeetingArtifactId, ProcessingRoute)>,
}

/// What a run leaves behind once it is over.
pub(super) struct FinishedRun {
    pub(super) samples: Vec<(ProcessingStage, StageSpeed)>,
    pub(super) receipt: Option<(MeetingArtifactId, ProcessingReceipt)>,
}

/// Where one track's records fall in a stage that reads several tracks in
/// turn: after the audio of the tracks read before it.
#[derive(Clone, Copy)]
pub(super) struct TrackSpan {
    before_ns: u64,
    first_offset_ns: u64,
    span_ns: u64,
}

impl TrackSpan {
    pub(super) fn of(track: &MeetingTrackSnapshot, before_ns: u64) -> Self {
        let first_offset_ns = track.first_offset_ns.unwrap_or(0);
        let span_ns = track
            .first_offset_ns
            .zip(track.last_offset_ns)
            .map_or(0, |(first, last)| last.saturating_sub(first));
        Self {
            before_ns,
            first_offset_ns,
            span_ns,
        }
    }

    /// The stage's audio read once the record ending at `record_end_ns` is.
    pub(super) fn done_ns(self, record_end_ns: u64) -> u64 {
        let within = record_end_ns
            .saturating_sub(self.first_offset_ns)
            .min(self.span_ns);
        self.before_ns.saturating_add(within)
    }

    /// The stage's audio read once the whole track is.
    pub(super) fn end_ns(self) -> u64 {
        self.before_ns.saturating_add(self.span_ns)
    }
}

impl RunProgress {
    pub(super) fn start(
        work: ProcessingWork,
        speeds: BTreeMap<ProcessingStage, StageSpeed>,
        started_at_utc_ms: i64,
        now: Instant,
    ) -> Self {
        Self {
            started_at_utc_ms,
            run_started: now,
            run_paused: Duration::ZERO,
            work,
            speeds,
            stage: ProcessingStage::Audio,
            stage_started: now,
            stage_paused: Duration::ZERO,
            stage_worked: true,
            done_permille: 0,
            last_update: now,
            finish_at: None,
            moved_later: false,
            measured: Vec::new(),
            notes: None,
        }
    }

    pub(super) fn current(&mut self, now: Instant) -> ProcessingProgress {
        self.update(now)
    }

    /// Close the stage under way and open `stage`. Always reported, however
    /// soon after the last update: a stage boundary is the news.
    pub(super) fn enter(&mut self, stage: ProcessingStage, now: Instant) -> ProcessingProgress {
        if stage != self.stage {
            self.close_stage(now);
            self.stage = stage;
            self.stage_started = now;
            self.stage_paused = Duration::ZERO;
            self.stage_worked = false;
            self.done_permille = 0;
            self.moved_later = false;
        }
        self.update(now)
    }

    /// `done_ns` of the stage's audio has been read. Reported only once
    /// `UPDATE_INTERVAL` has passed since the last update.
    pub(super) fn advance(&mut self, done_ns: u64, now: Instant) -> Option<ProcessingProgress> {
        self.stage_worked = true;
        if let Some(total) = self.work_for(self.stage) {
            let permille = u128::from(done_ns.min(total)) * 1000 / u128::from(total);
            let permille = u16::try_from(permille).unwrap_or(1000);
            self.done_permille = self.done_permille.max(permille);
        }
        (now.saturating_duration_since(self.last_update) >= UPDATE_INTERVAL)
            .then(|| self.update(now))
    }

    /// The stage under way did its work.
    pub(super) fn stage_did_work(&mut self) {
        self.stage_worked = true;
    }

    /// The notes this run wrote, which its receipt is kept on.
    pub(super) fn wrote_notes(&mut self, artifact_id: MeetingArtifactId, route: ProcessingRoute) {
        self.stage_worked = true;
        self.notes = Some((artifact_id, route));
    }

    /// Time spent waiting for a capture to finish is not processing time.
    pub(super) fn paused(&mut self, waited: Duration) {
        self.stage_paused = self.stage_paused.saturating_add(waited);
        self.run_paused = self.run_paused.saturating_add(waited);
    }

    /// The update while the run waits for a recording to finish. It shows no
    /// time left, and the finish time shown before is let go: the wait moves
    /// it by however long the recording runs.
    pub(super) fn waiting(&mut self, now: Instant) -> ProcessingProgress {
        self.last_update = now;
        self.finish_at = None;
        self.moved_later = false;
        ProcessingProgress {
            waiting_for_capture: true,
            ..self.progress(None)
        }
    }

    /// End the run. A run that did not complete keeps the stages it finished
    /// and measures neither the stage it stopped in nor the run as a whole.
    pub(super) fn finish(mut self, completed: bool, now: Instant) -> FinishedRun {
        if completed {
            self.close_stage(now);
        }
        let wall = now
            .saturating_duration_since(self.run_started)
            .saturating_sub(self.run_paused);
        let wall_ms = u64::try_from(wall.as_millis()).unwrap_or(u64::MAX).max(1);
        let receipt = self
            .notes
            .filter(|_| completed)
            .zip(self.work.audio_ns)
            .map(|((artifact_id, route), audio_ns)| {
                (
                    artifact_id,
                    ProcessingReceipt {
                        wall_ms,
                        audio_ns,
                        route,
                    },
                )
            });
        FinishedRun {
            samples: self.measured,
            receipt,
        }
    }

    fn close_stage(&mut self, now: Instant) {
        if !self.stage_worked {
            return;
        }
        let wall_ns = u64::try_from(self.stage_elapsed(now).as_nanos()).unwrap_or(u64::MAX);
        if let Some(audio_ns) = self.work_for(self.stage).filter(|_| wall_ns > 0) {
            self.measured
                .push((self.stage, StageSpeed { audio_ns, wall_ns }));
        }
    }

    fn update(&mut self, now: Instant) -> ProcessingProgress {
        self.last_update = now;
        let eta_ms = self.shown_left(now);
        self.progress(eta_ms)
    }

    fn progress(&self, eta_ms: Option<u64>) -> ProcessingProgress {
        ProcessingProgress {
            stage: self.stage,
            stage_fraction_permille: self.done_permille,
            audio_ns_total: self.work.audio_ns,
            started_at_utc_ms: self.started_at_utc_ms,
            eta_ms,
            waiting_for_capture: false,
        }
    }

    /// Milliseconds left as shown: the estimate, held to the finish time
    /// already on screen unless it moves earlier, or later for the first
    /// time this stage by enough to matter.
    fn shown_left(&mut self, now: Instant) -> Option<u64> {
        let Some(left) = self.left(now) else {
            self.finish_at = None;
            return None;
        };
        let proposed = now + left;
        let finish_at = match self.finish_at {
            Some(shown) if proposed > shown => {
                if !self.moved_later && proposed - shown >= LATER_FLOOR.max(left / 10) {
                    self.moved_later = true;
                    proposed
                } else {
                    shown
                }
            }
            _ => proposed,
        };
        self.finish_at = Some(finish_at);
        Some(
            u64::try_from(finish_at.saturating_duration_since(now).as_millis()).unwrap_or(u64::MAX),
        )
    }

    /// The whole run's time left: this stage's, which has to have been
    /// measured, and every later stage's that has been.
    fn left(&self, now: Instant) -> Option<Duration> {
        let current = self.left_in_stage(now)?;
        let later = ProcessingStage::ALL
            .into_iter()
            .filter(|stage| *stage > self.stage)
            .filter_map(|stage| self.predicted_ns(stage))
            .map(u128::from)
            .sum::<u128>();
        let left = current.saturating_add(later);
        Some(Duration::from_nanos(
            u64::try_from(left).unwrap_or(u64::MAX),
        ))
    }

    /// Before the stage reports any progress, its measured pace counts down.
    /// After, the pace observed so far takes over as the stage goes on:
    /// the expected stage length is `(1 − f²)·predicted + f·elapsed`, which
    /// is the prediction when the run keeps to it.
    fn left_in_stage(&self, now: Instant) -> Option<u128> {
        let predicted = u128::from(self.predicted_ns(self.stage)?);
        let elapsed = self.stage_elapsed(now).as_nanos();
        let done = u128::from(self.done_permille.min(1000));
        if done == 0 {
            return Some(predicted.saturating_sub(elapsed));
        }
        let expected = predicted * (1_000_000 - done * done) / 1_000_000 + elapsed * done / 1000;
        Some(expected * (1000 - done) / 1000)
    }

    fn predicted_ns(&self, stage: ProcessingStage) -> Option<u64> {
        Some(self.speeds.get(&stage)?.wall_for(self.work_for(stage)?))
    }

    fn work_for(&self, stage: ProcessingStage) -> Option<u64> {
        match stage {
            ProcessingStage::Speech => self.work.track_ns,
            _ => self.work.audio_ns,
        }
    }

    fn stage_elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.stage_started)
            .saturating_sub(self.stage_paused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HUNDRED_SECONDS_NS: u64 = 100_000_000_000;

    fn work() -> ProcessingWork {
        ProcessingWork {
            audio_ns: Some(HUNDRED_SECONDS_NS),
            track_ns: Some(HUNDRED_SECONDS_NS),
        }
    }

    /// This Mac has read these stages in real time before.
    fn real_time(stages: &[ProcessingStage]) -> BTreeMap<ProcessingStage, StageSpeed> {
        stages
            .iter()
            .map(|stage| {
                (
                    *stage,
                    StageSpeed {
                        audio_ns: 1,
                        wall_ns: 1,
                    },
                )
            })
            .collect()
    }

    /// A stage running slow pushes the finish time back once; after that the
    /// countdown holds until it can move earlier, and the next stage gets its
    /// own one move.
    #[test]
    fn the_finish_time_moves_later_at_most_once_a_stage() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let eta = |update: Option<ProcessingProgress>| update.expect("an update").eta_ms;
        let mut progress = RunProgress::start(
            work(),
            real_time(&[ProcessingStage::Speech, ProcessingStage::Speakers]),
            0,
            t0,
        );

        assert_eq!(
            progress.enter(ProcessingStage::Speech, t0).eta_ms,
            Some(200_000)
        );
        assert_eq!(
            eta(progress.advance(10_000_000_000, at(50_000))),
            Some(193_600),
            "the first slip is shown"
        );
        assert_eq!(
            eta(progress.advance(15_000_000_000, at(100_000))),
            Some(143_600),
            "a second slip in the same stage is held"
        );
        assert_eq!(
            progress
                .enter(ProcessingStage::Speakers, at(101_000))
                .eta_ms,
            Some(100_000),
            "an earlier finish is always shown"
        );
        assert_eq!(
            eta(progress.advance(10_000_000_000, at(151_000))),
            Some(93_600),
            "the next stage may slip once"
        );
    }

    /// Inside a stage an update waits a quarter second after the last one; a
    /// new stage is news and is never held back.
    #[test]
    fn updates_inside_a_stage_come_at_most_four_a_second() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut progress = RunProgress::start(work(), BTreeMap::new(), 0, t0);
        progress.enter(ProcessingStage::Speech, t0);

        assert_eq!(progress.advance(1_000_000_000, at(100)), None);
        assert_eq!(progress.advance(2_000_000_000, at(249)), None);
        let update = progress
            .advance(3_000_000_000, at(250))
            .expect("a quarter second on");
        assert_eq!(update.stage_fraction_permille, 30);
        assert_eq!(
            update.eta_ms, None,
            "a stage never measured here says nothing about time"
        );
        let boundary = progress.enter(ProcessingStage::Speakers, at(260));
        assert_eq!(
            (boundary.stage, boundary.stage_fraction_permille),
            (ProcessingStage::Speakers, 0)
        );
    }

    /// A run that stops keeps the pace of the stages it finished, and nothing
    /// from the stage it stopped in or from a stage that skipped its work.
    #[test]
    fn a_stopped_run_measures_only_the_work_it_finished() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut progress = RunProgress::start(work(), BTreeMap::new(), 0, t0);
        progress.enter(ProcessingStage::Speech, at(1_000));
        progress.advance(HUNDRED_SECONDS_NS, at(5_000));
        progress.enter(ProcessingStage::Speakers, at(6_000));
        progress.enter(ProcessingStage::Notes, at(6_500));
        progress.wrote_notes(MeetingArtifactId::new(), ProcessingRoute::Local);

        let finished = progress.finish(false, at(9_000));
        assert_eq!(
            finished.samples,
            vec![
                (
                    ProcessingStage::Audio,
                    StageSpeed {
                        audio_ns: HUNDRED_SECONDS_NS,
                        wall_ns: 1_000_000_000,
                    },
                ),
                (
                    ProcessingStage::Speech,
                    StageSpeed {
                        audio_ns: HUNDRED_SECONDS_NS,
                        wall_ns: 5_000_000_000,
                    },
                ),
            ]
        );
        assert!(
            finished.receipt.is_none(),
            "a run that stopped leaves no receipt"
        );
    }
}
