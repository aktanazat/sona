use crate::actions;
use crate::managers::audio::AudioRecordingManager;
use crate::modes::{RunPlan, TranscriptionIntent};
use log::{debug, error, warn};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

const DEBOUNCE: Duration = Duration::from_millis(30);
const RELEASE_GRACE: Duration = Duration::from_millis(50);

/// A physical tap runs 80-150 ms from key-down to key-up, while deliberate
/// hold-to-talk stays down well past this before releasing. A release this
/// soon after its press is therefore a tap, and a tap has to latch the
/// recording open: stopping at key-up captures 30-60 ms of audio, which the
/// no-speech path then discards.
const TAP_LATCH_THRESHOLD: Duration = Duration::from_millis(400);

/// What a push-to-talk edge does to the release-grace machinery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PttAction {
    /// Not a grace-window decision: the stage logic in
    /// [`CoordinatorState::on_input`] decides.
    Passthrough,
    /// Arm the grace window; the outcome is what its expiry means.
    Defer(GraceOutcome),
    CancelRelease,
    /// The key is up, but this release cannot end the recording.
    IgnoreRelease,
}

/// What an expired grace window does to the recording it was armed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraceOutcome {
    /// The chord was held past [`TAP_LATCH_THRESHOLD`]: hold-to-talk, so the
    /// release ends the recording.
    Stop,
    /// The chord was tapped: keep recording and let the next press end it.
    Latch,
}

/// The recording in flight, as the edge being classified sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveRecording {
    /// Nothing is recording this edge's intent: idle, busy, or another intent.
    Unrelated,
    /// Recording this intent with releases disarmed; the next press stops it.
    Latched,
    /// Recording this intent while its press is still logically down.
    Held {
        /// The press edge is younger than [`TAP_LATCH_THRESHOLD`], so a
        /// release now is a tap rather than the end of a hold.
        tapped: bool,
    },
}

struct PendingRelease {
    intent: TranscriptionIntent,
    shortcut_label: String,
    deadline: Instant,
    outcome: GraceOutcome,
}

/// A press that arrived while the pipeline was still processing the previous
/// transcription. Toggle-style triggers (signals, CLI flags, some pedals) flip
/// state on every edge, so dropping a busy press desyncs the parity: the next
/// edge then starts a recording nobody will ever stop.
struct PendingPress {
    intent: TranscriptionIntent,
    shortcut_label: String,
    /// Whether the start it eventually produces latches; see [`start_effect`].
    latched: bool,
}

/// What to do with an input that arrives while the pipeline is busy.
/// `remembered` is whether a press for the same intent is already waiting for
/// the pipeline to drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BusyAction {
    Ignore,
    /// Remember the press; start recording when the pipeline finishes.
    Remember,
    /// Cancel a previously remembered press. Toggle: two presses during one
    /// busy window net to no-op. Push-to-talk: the key is already back up, so
    /// the remembered press must not fire.
    Forget,
}

fn classify_busy_input(is_pressed: bool, push_to_talk: bool, remembered: bool) -> BusyAction {
    match (push_to_talk, is_pressed) {
        // Toggle: presses alternate remember/forget to preserve parity.
        (false, true) if remembered => BusyAction::Forget,
        (false, true) => BusyAction::Remember,
        // Toggle mode ignores releases.
        (false, false) => BusyAction::Ignore,
        // Push-to-talk: a press while busy means the key is being held, so
        // start as soon as the pipeline drains.
        (true, true) => BusyAction::Remember,
        (true, false) if remembered => BusyAction::Forget,
        (true, false) => BusyAction::Ignore,
    }
}

/// A keyboard, signal, or CLI edge for one transcription intent.
struct InputEvent {
    intent: TranscriptionIntent,
    shortcut_label: String,
    is_pressed: bool,
    push_to_talk: bool,
    /// Signals and CLI toggles rather than physical keys. They fire on every
    /// edge by design and must never be debounced: dropping one desyncs toggle
    /// parity and leaves recording wedged on.
    external: bool,
}

/// Commands processed sequentially by the coordinator thread.
enum Command {
    Input(InputEvent),
    /// End whatever is recording; see [`CoordinatorState::on_finish`].
    Finish {
        source: String,
    },
    Cancel {
        recording_was_active: bool,
    },
    ProcessingFinished,
}

enum Stage {
    Idle,
    Recording {
        intent: TranscriptionIntent,
        run_plan: Box<RunPlan>,
        /// When the press that opened this recording was processed.
        pressed_at: Instant,
        /// Releases no longer stop this recording; the next press does.
        latched: bool,
    },
    Processing,
}

/// A side effect decided by [`CoordinatorState`]. The coordinator thread is
/// the only executor, so every decision stays testable without an `AppHandle`.
enum Effect {
    Start {
        intent: TranscriptionIntent,
        shortcut_label: String,
        /// Press provenance for the recording this opens, threaded back
        /// through [`CoordinatorState::on_started`] so a recording always
        /// knows whether a release can stop it.
        pressed_at: Instant,
        latched: bool,
    },
    Stop {
        intent: TranscriptionIntent,
        shortcut_label: String,
        run_plan: Box<RunPlan>,
    },
}

/// Turn a press edge into the start it triggers. Signals and CLI toggles have
/// no key to release, so the recording they open latches at once: only their
/// next edge can stop it.
fn start_effect(input: InputEvent, pressed_at: Instant) -> Effect {
    Effect::Start {
        intent: input.intent,
        shortcut_label: input.shortcut_label,
        pressed_at,
        latched: input.external,
    }
}

fn classify_ptt_event(
    pending_release_intent: Option<&TranscriptionIntent>,
    is_pressed: bool,
    push_to_talk: bool,
    intent: &TranscriptionIntent,
    recording: ActiveRecording,
) -> PttAction {
    if !push_to_talk {
        return PttAction::Passthrough;
    }

    if is_pressed {
        if pending_release_intent == Some(intent) {
            PttAction::CancelRelease
        } else {
            PttAction::Passthrough
        }
    } else {
        match recording {
            ActiveRecording::Unrelated => PttAction::Passthrough,
            // A latched recording outlives its key.
            ActiveRecording::Latched => PttAction::IgnoreRelease,
            // The armed window already owns this release; letting a repeat of
            // it through would stop a recording the window is about to latch.
            ActiveRecording::Held { .. } if pending_release_intent == Some(intent) => {
                PttAction::IgnoreRelease
            }
            ActiveRecording::Held { tapped: true } => PttAction::Defer(GraceOutcome::Latch),
            ActiveRecording::Held { tapped: false } => PttAction::Defer(GraceOutcome::Stop),
        }
    }
}

/// Pure lifecycle state machine: it owns every transition decision (push-to-talk
/// grace, debounce, busy-pipeline remember/forget, cancel, drain) and produces
/// [`Effect`]s instead of touching the app, so tests exercise the production
/// logic rather than a copy of it.
struct CoordinatorState {
    stage: Stage,
    last_press: Option<Instant>,
    pending_release: Option<PendingRelease>,
    pending_press: Option<PendingPress>,
}

impl CoordinatorState {
    fn new() -> Self {
        Self {
            stage: Stage::Idle,
            last_press: None,
            pending_release: None,
            pending_press: None,
        }
    }

    /// Deadline of the deferred release, if any. Drives `recv_timeout`.
    fn grace_deadline(&self) -> Option<Instant> {
        self.pending_release
            .as_ref()
            .map(|pending| pending.deadline)
    }

    /// How the recording in flight relates to `intent` at `now`.
    fn active_recording(&self, intent: &TranscriptionIntent, now: Instant) -> ActiveRecording {
        match &self.stage {
            Stage::Recording {
                intent: active,
                pressed_at,
                latched,
                ..
            } if active == intent => {
                if *latched {
                    ActiveRecording::Latched
                } else {
                    ActiveRecording::Held {
                        tapped: now.duration_since(*pressed_at) < TAP_LATCH_THRESHOLD,
                    }
                }
            }
            _ => ActiveRecording::Unrelated,
        }
    }

    fn on_input(&mut self, input: InputEvent, now: Instant) -> Option<Effect> {
        let pending_release_intent = self.pending_release.as_ref().map(|pending| &pending.intent);
        let recording = self.active_recording(&input.intent, now);

        match classify_ptt_event(
            pending_release_intent,
            input.is_pressed,
            input.push_to_talk,
            &input.intent,
            recording,
        ) {
            PttAction::CancelRelease => {
                self.pending_release = None;
                return None;
            }
            PttAction::Defer(outcome) => {
                self.pending_release = Some(PendingRelease {
                    intent: input.intent,
                    shortcut_label: input.shortcut_label,
                    deadline: now + RELEASE_GRACE,
                    outcome,
                });
                return None;
            }
            PttAction::IgnoreRelease => return None,
            PttAction::Passthrough => {}
        }

        // Debounce rapid-fire press events (key repeat / double-tap).
        // Push-to-talk releases may be deferred above to absorb X11 auto-repeat.
        // External triggers are exempt: each one is a deliberate edge from the
        // user's own integration, and dropping it desyncs toggle parity.
        if input.is_pressed && !input.external {
            if self
                .last_press
                .is_some_and(|then| now.duration_since(then) < DEBOUNCE)
            {
                debug!("Debounced transcription intent: {:?}", input.intent);
                return None;
            }
            self.last_press = Some(now);
        }

        if matches!(self.stage, Stage::Processing) {
            self.remember_or_forget(input);
            return None;
        }

        match (input.push_to_talk, input.is_pressed) {
            // Push-to-talk: hold to talk, or tap to latch and press again to stop.
            (true, true) => match recording {
                // The tap that latched this recording is over; this press ends it.
                ActiveRecording::Latched => {
                    self.stop_recording(&input.intent, input.shortcut_label)
                }
                // The chord is still down, so the OS is repeating it, not the user.
                ActiveRecording::Held { .. } => None,
                ActiveRecording::Unrelated if matches!(self.stage, Stage::Idle) => {
                    Some(start_effect(input, now))
                }
                ActiveRecording::Unrelated => None,
            },
            // Every push-to-talk release is decided by the grace window above.
            (true, false) => None,
            (false, true) => match recording {
                // Toggle: pressing the recording intent again stops it.
                ActiveRecording::Latched | ActiveRecording::Held { .. } => {
                    self.stop_recording(&input.intent, input.shortcut_label)
                }
                ActiveRecording::Unrelated if matches!(self.stage, Stage::Idle) => {
                    Some(start_effect(input, now))
                }
                ActiveRecording::Unrelated => {
                    debug!(
                        "Ignoring transcription intent {:?}: another intent is recording",
                        input.intent
                    );
                    None
                }
            },
            // Toggle mode ignores releases.
            (false, false) => None,
        }
    }

    /// A busy pipeline cannot change lifecycle now, so classify the input
    /// against any already-remembered press instead of dropping it silently.
    fn remember_or_forget(&mut self, input: InputEvent) {
        // Only one press can be remembered. Once an intent has claimed it,
        // inputs for a different intent are ignored, the same rule as a
        // different intent pressed while recording, rather than replacing the
        // remembered press and breaking its parity.
        if let Some(pending) = &self.pending_press {
            if pending.intent != input.intent {
                debug!(
                    "Ignoring transcription intent {:?}: {:?} is already pending",
                    input.intent, pending.intent
                );
                return;
            }
        }

        match classify_busy_input(
            input.is_pressed,
            input.push_to_talk,
            self.pending_press.is_some(),
        ) {
            BusyAction::Remember => {
                debug!("Remembering press for {:?}: pipeline busy", input.intent);
                self.pending_press = Some(PendingPress {
                    intent: input.intent,
                    shortcut_label: input.shortcut_label,
                    latched: input.external,
                });
            }
            BusyAction::Forget => {
                debug!("Forgetting remembered press for {:?}", input.intent);
                self.pending_press = None;
            }
            BusyAction::Ignore => {
                debug!(
                    "Ignoring transcription intent {:?}: pipeline busy",
                    input.intent
                )
            }
        }
    }

    /// The `RELEASE_GRACE` window elapsed with no cancelling press, so the
    /// release was genuine rather than auto-repeat: end a hold, or hold a tap
    /// open until the next press.
    fn on_grace_expired(&mut self) -> Option<Effect> {
        let pending = self.pending_release.take()?;
        match pending.outcome {
            GraceOutcome::Stop => self.stop_recording(&pending.intent, pending.shortcut_label),
            GraceOutcome::Latch => {
                self.latch(&pending.intent);
                None
            }
        }
    }

    /// Keep `intent`'s recording open past its key-up, so a 100 ms chord still
    /// captures a whole utterance.
    fn latch(&mut self, intent: &TranscriptionIntent) {
        match &mut self.stage {
            Stage::Recording {
                intent: active,
                latched,
                ..
            } if active == intent => {
                debug!("Tap latch: recording continues until next press");
                *latched = true;
            }
            _ => {}
        }
    }

    fn on_cancel(&mut self, recording_was_active: bool) {
        self.pending_release = None;
        // An explicit cancel abandons a remembered start too: the user asked
        // for silence, not for a deferred recording.
        self.pending_press = None;
        // Don't reset during processing — wait for the pipeline to finish.
        if !matches!(self.stage, Stage::Processing)
            && (recording_was_active || matches!(self.stage, Stage::Recording { .. }))
        {
            self.stage = Stage::Idle;
        }
    }

    fn on_processing_finished(&mut self, now: Instant) -> Option<Effect> {
        self.stage = Stage::Idle;
        let pending = self.pending_press.take()?;
        debug!(
            "Pipeline drained; starting the press remembered for {:?}",
            pending.intent
        );
        Some(Effect::Start {
            intent: pending.intent,
            shortcut_label: pending.shortcut_label,
            // The drain is this recording's press edge: the key has been down
            // since before the pipeline finished, so the tap window opens now.
            pressed_at: now,
            latched: pending.latched,
        })
    }

    /// Reconcile with the executor: recording began only if it produced a plan.
    fn on_started(
        &mut self,
        intent: TranscriptionIntent,
        pressed_at: Instant,
        latched: bool,
        run_plan: Option<Box<RunPlan>>,
    ) {
        if let Some(run_plan) = run_plan {
            self.stage = Stage::Recording {
                intent,
                run_plan,
                pressed_at,
                latched,
            };
        }
    }

    /// A Stop button rather than a shortcut edge: it ends the recording in
    /// flight whatever intent opened it, and it is never remembered. While
    /// the pipeline is busy or idle it does nothing, so a stop can never
    /// turn into the start of the next recording the way a toggle press does.
    fn on_finish(&mut self, source: String) -> Option<Effect> {
        if !matches!(self.stage, Stage::Recording { .. }) {
            return None;
        }
        self.pending_release = None;
        self.hand_over_recording(source)
    }

    /// Hand the active recording's plan to the executor. Returns None when
    /// `intent` is not the intent currently recording.
    fn stop_recording(
        &mut self,
        intent: &TranscriptionIntent,
        shortcut_label: String,
    ) -> Option<Effect> {
        if !matches!(&self.stage, Stage::Recording { intent: active, .. } if active == intent) {
            return None;
        }
        self.hand_over_recording(shortcut_label)
    }

    fn hand_over_recording(&mut self, shortcut_label: String) -> Option<Effect> {
        match std::mem::replace(&mut self.stage, Stage::Processing) {
            Stage::Recording {
                intent, run_plan, ..
            } => Some(Effect::Stop {
                intent,
                shortcut_label,
                run_plan,
            }),
            other => {
                self.stage = other;
                None
            }
        }
    }
}

/// Serialises all transcription lifecycle events through a single thread
/// to eliminate race conditions between keyboard shortcuts, signals, and
/// the async transcribe-paste pipeline.
pub struct TranscriptionCoordinator {
    tx: Sender<Command>,
}

impl TranscriptionCoordinator {
    pub fn new(app: AppHandle) -> Self {
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut state = CoordinatorState::new();

                loop {
                    let cmd = match state.grace_deadline() {
                        Some(deadline) => {
                            match rx
                                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                            {
                                Ok(cmd) => cmd,
                                Err(mpsc::RecvTimeoutError::Timeout) => {
                                    if let Some(effect) = state.on_grace_expired() {
                                        execute(&app, &mut state, effect);
                                    }
                                    continue;
                                }
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                        None => match rx.recv() {
                            Ok(cmd) => cmd,
                            Err(_) => break,
                        },
                    };

                    let effect = match cmd {
                        Command::Input(input) => state.on_input(input, Instant::now()),
                        Command::Finish { source } => state.on_finish(source),
                        Command::Cancel {
                            recording_was_active,
                        } => {
                            state.on_cancel(recording_was_active);
                            None
                        }
                        Command::ProcessingFinished => state.on_processing_finished(Instant::now()),
                    };

                    if let Some(effect) = effect {
                        execute(&app, &mut state, effect);
                    }
                }
                debug!("Transcription coordinator exited");
            }));
            if let Err(e) = result {
                error!("Transcription coordinator panicked: {e:?}");
            }
        });

        Self { tx }
    }

    /// Route a keyboard event that has already resolved to a typed
    /// transcription intent.
    pub fn send_shortcut_input(
        &self,
        intent: TranscriptionIntent,
        shortcut_label: &str,
        is_pressed: bool,
        push_to_talk: bool,
    ) {
        self.send(InputEvent {
            intent,
            shortcut_label: shortcut_label.to_string(),
            is_pressed,
            push_to_talk,
            external: false,
        });
    }

    /// Signals, the tray, and CLI toggles are semantic inputs, not hidden
    /// shortcut IDs. They arrive already debounced by whatever produced them,
    /// and each edge flips the toggle, so none of them may be dropped.
    pub fn send_intent(&self, intent: TranscriptionIntent, source: &str) {
        self.send(InputEvent {
            intent,
            shortcut_label: source.to_string(),
            is_pressed: true,
            push_to_talk: false,
            external: true,
        });
    }

    fn send(&self, input: InputEvent) {
        if self.tx.send(Command::Input(input)).is_err() {
            warn!("Transcription coordinator channel closed");
        }
    }

    /// A Stop button. Ends the recording in flight whatever intent opened
    /// it, and does nothing at any other stage.
    pub fn send_finish(&self, source: &str) {
        if self
            .tx
            .send(Command::Finish {
                source: source.to_string(),
            })
            .is_err()
        {
            warn!("Transcription coordinator channel closed");
        }
    }

    pub fn notify_cancel(&self, recording_was_active: bool) {
        if self
            .tx
            .send(Command::Cancel {
                recording_was_active,
            })
            .is_err()
        {
            warn!("Transcription coordinator channel closed");
        }
    }

    pub fn notify_processing_finished(&self) {
        if self.tx.send(Command::ProcessingFinished).is_err() {
            warn!("Transcription coordinator channel closed");
        }
    }
}

/// Run one decision from [`CoordinatorState`] and report a start back, so the
/// machine only ever holds a recording the microphone actually opened.
fn execute(app: &AppHandle, state: &mut CoordinatorState, effect: Effect) {
    match effect {
        Effect::Start {
            intent,
            shortcut_label,
            pressed_at,
            latched,
        } => {
            let run_plan = start(app, &intent, &shortcut_label);
            state.on_started(intent, pressed_at, latched, run_plan);
        }
        Effect::Stop {
            intent,
            shortcut_label,
            run_plan,
        } => {
            actions::stop_transcription(app, &intent.recording_id(), &shortcut_label, *run_plan);
        }
    }
}

/// Begin recording for `intent`. Returns the plan it started with, or None
/// when no plan could be built or the microphone never opened.
fn start(
    app: &AppHandle,
    intent: &TranscriptionIntent,
    shortcut_label: &str,
) -> Option<Box<RunPlan>> {
    let settings = crate::settings::get_settings(app);
    let run_plan = match RunPlan::for_intent(&settings, intent) {
        Ok(plan) => plan,
        Err(error) => {
            warn!("Could not build run plan for {intent:?}: {error}");
            // A refusal the user caused deliberately deserves an answer rather
            // than a silent no-op; every other rejection stays a log line.
            crate::command_mode::report_refused_run(app, intent, &error);
            return None;
        }
    };
    let recording_id = intent.recording_id();
    actions::start_transcription(app, &recording_id, shortcut_label, &run_plan);
    if app
        .try_state::<Arc<AudioRecordingManager>>()
        .is_some_and(|manager| manager.is_recording())
    {
        Some(Box::new(run_plan))
    } else {
        debug!("Start for {intent:?} did not begin recording; staying idle");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_mode() -> TranscriptionIntent {
        TranscriptionIntent::ActiveMode
    }

    #[test]
    fn push_to_talk_release_while_recording_defers_release() {
        let intent = active_mode();
        assert_eq!(
            classify_ptt_event(
                None,
                false,
                true,
                &intent,
                ActiveRecording::Held { tapped: false }
            ),
            PttAction::Defer(GraceOutcome::Stop)
        );
    }

    #[test]
    fn push_to_talk_release_classification_covers_every_recording_state() {
        let intent = active_mode();
        // A tap defers into a latch, so the recording survives its key-up.
        assert_eq!(
            classify_ptt_event(
                None,
                false,
                true,
                &intent,
                ActiveRecording::Held { tapped: true }
            ),
            PttAction::Defer(GraceOutcome::Latch)
        );
        // A latched recording ignores releases; only the next press ends it.
        assert_eq!(
            classify_ptt_event(None, false, true, &intent, ActiveRecording::Latched),
            PttAction::IgnoreRelease
        );
        // An armed window owns the release: a duplicate must not shortcut it.
        assert_eq!(
            classify_ptt_event(
                Some(&intent),
                false,
                true,
                &intent,
                ActiveRecording::Held { tapped: true }
            ),
            PttAction::IgnoreRelease
        );
        // Nothing is recording this intent: the stage logic decides.
        assert_eq!(
            classify_ptt_event(None, false, true, &intent, ActiveRecording::Unrelated),
            PttAction::Passthrough
        );
    }

    #[test]
    fn push_to_talk_press_matching_pending_release_cancels_release() {
        let intent = active_mode();
        assert_eq!(
            classify_ptt_event(
                Some(&intent),
                true,
                true,
                &intent,
                ActiveRecording::Held { tapped: false }
            ),
            PttAction::CancelRelease
        );
    }

    #[test]
    fn toggle_mode_press_and_release_pass_through() {
        let intent = active_mode();
        assert_eq!(
            classify_ptt_event(
                Some(&intent),
                true,
                false,
                &intent,
                ActiveRecording::Held { tapped: false }
            ),
            PttAction::Passthrough
        );
        assert_eq!(
            classify_ptt_event(None, false, false, &intent, ActiveRecording::Latched),
            PttAction::Passthrough
        );
    }

    #[test]
    fn different_semantic_intent_does_not_cancel_a_pending_release() {
        let active = active_mode();
        let post_process = TranscriptionIntent::ActiveModeWithPostProcess;
        assert_eq!(
            classify_ptt_event(
                Some(&active),
                true,
                true,
                &post_process,
                ActiveRecording::Unrelated
            ),
            PttAction::Passthrough
        );
    }

    #[test]
    fn legacy_binding_id_is_not_a_transcription_binding() {
        assert!(
            TranscriptionIntent::from_binding(crate::modes::LEGACY_POST_PROCESS_BINDING_ID)
                .is_none()
        );
        assert_eq!(
            TranscriptionIntent::from_binding("transcribe"),
            Some(TranscriptionIntent::ActiveMode)
        );
    }

    /// The command chord is a first-class intent, so the hybrid tap/hold rules
    /// have to apply to it byte-for-byte. A command that stopped at key-up
    /// would capture 30-60 ms of audio and refuse for no speech.
    #[test]
    fn the_command_intent_follows_the_same_tap_and_hold_rules() {
        let command = TranscriptionIntent::Command;
        assert_eq!(
            classify_ptt_event(
                None,
                false,
                true,
                &command,
                ActiveRecording::Held { tapped: true }
            ),
            PttAction::Defer(GraceOutcome::Latch)
        );
        assert_eq!(
            classify_ptt_event(
                None,
                false,
                true,
                &command,
                ActiveRecording::Held { tapped: false }
            ),
            PttAction::Defer(GraceOutcome::Stop)
        );
        assert_eq!(
            classify_ptt_event(None, false, true, &command, ActiveRecording::Latched),
            PttAction::IgnoreRelease
        );
    }

    /// Dictation and command are separate recordings. Releasing one must never
    /// end the other, or the command chord would stop a dictation mid-sentence.
    #[test]
    fn a_command_edge_never_touches_a_dictation_recording() {
        let command = TranscriptionIntent::Command;
        assert_eq!(
            classify_ptt_event(
                Some(&active_mode()),
                true,
                true,
                &command,
                ActiveRecording::Unrelated
            ),
            PttAction::Passthrough
        );

        let mut harness = Harness::new();
        press_and_hold(&mut harness);
        harness.advance(Duration::from_millis(800));
        harness.input(command, false, true);
        assert!(
            harness.state.grace_deadline().is_none(),
            "a command release must not arm the dictation's release"
        );
        assert_eq!((harness.starts, harness.stops), (1, 0));
        assert!(harness.is_recording());
    }

    #[test]
    fn a_command_hold_records_and_stops_on_release() {
        let mut harness = Harness::new();
        harness.input(TranscriptionIntent::Command, true, true);
        assert!(harness.is_recording());

        harness.advance(Duration::from_millis(800));
        harness.input(TranscriptionIntent::Command, false, true);
        assert_eq!(harness.stops, 0, "the grace window owns the stop");

        harness.advance(RELEASE_GRACE);
        harness.grace_expired();
        assert_eq!((harness.starts, harness.stops), (1, 1));
        assert!(harness.is_processing());
    }

    /// Drives the production state machine. The executor is stubbed by
    /// `started`: `Effect::Start` becomes a recording with a real run plan
    /// unless the test says the microphone refused.
    struct Harness {
        state: CoordinatorState,
        clock: Instant,
        starts: u32,
        stops: u32,
        microphone_opens: bool,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                state: CoordinatorState::new(),
                clock: Instant::now(),
                starts: 0,
                stops: 0,
                microphone_opens: true,
            }
        }

        fn run_plan() -> Box<RunPlan> {
            let settings = crate::settings::get_default_settings();
            Box::new(
                RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
                    .expect("default settings always resolve the active mode"),
            )
        }

        fn apply(&mut self, effect: Option<Effect>) {
            match effect {
                Some(Effect::Start {
                    intent,
                    pressed_at,
                    latched,
                    ..
                }) => {
                    self.starts += 1;
                    let plan = self.microphone_opens.then(Self::run_plan);
                    self.state.on_started(intent, pressed_at, latched, plan);
                }
                Some(Effect::Stop { .. }) => self.stops += 1,
                None => {}
            }
        }

        fn input(&mut self, intent: TranscriptionIntent, is_pressed: bool, push_to_talk: bool) {
            self.advance(Duration::from_millis(5));
            let effect = self.state.on_input(
                InputEvent {
                    intent,
                    shortcut_label: "test".to_string(),
                    is_pressed,
                    push_to_talk,
                    external: false,
                },
                self.clock,
            );
            self.apply(effect);
        }

        fn external_press(&mut self, intent: TranscriptionIntent, push_to_talk: bool) {
            self.advance(Duration::from_millis(5));
            let effect = self.state.on_input(
                InputEvent {
                    intent,
                    shortcut_label: "signal".to_string(),
                    is_pressed: true,
                    push_to_talk,
                    external: true,
                },
                self.clock,
            );
            self.apply(effect);
        }

        fn grace_expired(&mut self) {
            let effect = self.state.on_grace_expired();
            self.apply(effect);
        }

        fn processing_finished(&mut self) {
            let effect = self.state.on_processing_finished(self.clock);
            self.apply(effect);
        }

        fn finish(&mut self) {
            let effect = self.state.on_finish("button".to_string());
            self.apply(effect);
        }

        fn advance(&mut self, by: Duration) {
            self.clock += by;
        }

        fn is_recording(&self) -> bool {
            matches!(self.state.stage, Stage::Recording { .. })
        }

        fn is_processing(&self) -> bool {
            matches!(self.state.stage, Stage::Processing)
        }

        fn is_idle(&self) -> bool {
            matches!(self.state.stage, Stage::Idle)
        }
    }

    fn press_and_hold(harness: &mut Harness) {
        harness.input(active_mode(), true, true);
    }

    /// X11 auto-repeat delivers release/press pairs while the key is held.
    fn autorepeat_burst(harness: &mut Harness) {
        press_and_hold(harness);
        for _ in 0..6 {
            harness.input(active_mode(), false, true);
            harness.input(active_mode(), true, true);
        }
    }

    #[test]
    fn x11_autorepeat_burst_does_not_toggle_recording() {
        let mut harness = Harness::new();
        autorepeat_burst(&mut harness);
        assert_eq!((harness.starts, harness.stops), (1, 0));
        assert!(harness.is_recording());
        // Each repeat press cancels the release before its grace window
        // expires, so the burst never latches. A latch here would hand the
        // stop to the very next repeat press.
        assert_eq!(
            harness
                .state
                .active_recording(&active_mode(), harness.clock),
            ActiveRecording::Held { tapped: true }
        );
    }

    #[test]
    fn genuine_release_after_grace_stops_recording_once() {
        let mut harness = Harness::new();
        autorepeat_burst(&mut harness);
        // A deliberate hold outlives the tap window, so its release stops.
        harness.advance(TAP_LATCH_THRESHOLD);
        harness.input(active_mode(), false, true);
        harness.grace_expired();
        assert_eq!((harness.starts, harness.stops), (1, 1));
        assert!(harness.is_processing());
    }

    #[test]
    fn push_to_talk_tap_latches_the_recording_open() {
        let mut harness = Harness::new();
        press_and_hold(&mut harness);
        harness.advance(Duration::from_millis(100));
        harness.input(active_mode(), false, true);
        assert!(
            harness.is_recording(),
            "a tap must not stop the recording at key-up"
        );

        harness.advance(RELEASE_GRACE);
        harness.grace_expired();
        assert_eq!((harness.starts, harness.stops), (1, 0));
        assert!(harness.is_recording(), "the tap latched the recording open");

        // The latch hands the stop to the next press.
        harness.input(active_mode(), true, true);
        assert_eq!((harness.starts, harness.stops), (1, 1));
        assert!(harness.is_processing());
    }

    #[test]
    fn push_to_talk_hold_release_stops_at_grace_expiry() {
        let mut harness = Harness::new();
        press_and_hold(&mut harness);
        harness.advance(Duration::from_millis(800));
        harness.input(active_mode(), false, true);
        assert!(
            harness.state.grace_deadline().is_some(),
            "a hold-release waits out the grace window before stopping"
        );
        assert_eq!(harness.stops, 0);

        harness.advance(RELEASE_GRACE);
        harness.grace_expired();
        assert_eq!((harness.starts, harness.stops), (1, 1));
        assert!(harness.is_processing());
    }

    #[test]
    fn a_release_at_the_latch_threshold_is_a_hold() {
        let mut harness = Harness::new();
        press_and_hold(&mut harness);
        let pressed_at = harness.clock;
        assert_eq!(
            harness
                .state
                .active_recording(&active_mode(), pressed_at + TAP_LATCH_THRESHOLD),
            ActiveRecording::Held { tapped: false }
        );
        assert_eq!(
            harness.state.active_recording(
                &active_mode(),
                pressed_at + TAP_LATCH_THRESHOLD - Duration::from_millis(1)
            ),
            ActiveRecording::Held { tapped: true }
        );
    }

    #[test]
    fn repeat_presses_of_a_held_chord_produce_no_effect() {
        let mut harness = Harness::new();
        press_and_hold(&mut harness);
        // macOS repeats key-down with no intervening key-up, far enough apart
        // to clear DEBOUNCE.
        for _ in 0..4 {
            harness.advance(DEBOUNCE);
            harness.input(active_mode(), true, true);
        }
        assert_eq!((harness.starts, harness.stops), (1, 0));
        assert!(harness.is_recording());
        assert!(
            harness.state.grace_deadline().is_none(),
            "a repeat press must not arm a release"
        );
    }

    /// Every other test here delivers a press and its release as a pair,
    /// because the harness builds both. The real key source can lose a
    /// release: macOS Secure Input stops event-tap delivery while a password
    /// field holds focus, and an episode shorter than
    /// `secure_input::SUSTAIN_THRESHOLD` registers no fallback, so a key-up
    /// struck inside it is simply gone. handy-keys then still believes the
    /// chord is down and emits no further press for it.
    ///
    /// This pins what the machine does with that state, and the asymmetry it
    /// exposes: toggle recovers on the user's next press, push-to-talk reads
    /// the same press as auto-repeat and keeps the microphone open. So the
    /// exit is cancel, the tray, or an external edge — and cancel only
    /// reaches a held chord because the cancel key is registered under each
    /// modifier prefix a recording can be held by, see
    /// `shortcut::tests::every_recording_chord_leaves_the_cancel_key_reachable`.
    #[test]
    fn a_dropped_release_leaves_push_to_talk_with_no_keyboard_exit() {
        let mut push_to_talk = Harness::new();
        press_and_hold(&mut push_to_talk);
        assert!(push_to_talk.is_recording());

        // The release is lost. Well past the tap window — so no repeat rate
        // explains it — the user presses again to stop the recording.
        push_to_talk.advance(TAP_LATCH_THRESHOLD * 4);
        push_to_talk.input(active_mode(), true, true);
        assert_eq!((push_to_talk.starts, push_to_talk.stops), (1, 0));
        assert!(
            push_to_talk.is_recording(),
            "the recovery press is swallowed as auto-repeat"
        );

        // The identical loss under toggle: the next press stops it.
        let mut toggle = Harness::new();
        toggle.input(active_mode(), true, false);
        assert!(toggle.is_recording());
        toggle.advance(TAP_LATCH_THRESHOLD * 4);
        toggle.input(active_mode(), true, false);
        assert_eq!((toggle.starts, toggle.stops), (1, 1));
        assert!(toggle.is_processing());
    }

    #[test]
    fn a_release_after_a_tap_stop_is_ignored_by_the_busy_pipeline() {
        let mut harness = Harness::new();
        press_and_hold(&mut harness);
        harness.advance(Duration::from_millis(100));
        harness.input(active_mode(), false, true);
        harness.advance(RELEASE_GRACE);
        harness.grace_expired();
        harness.input(active_mode(), true, true);
        assert!(harness.is_processing());

        // The key-up of the press that stopped the tap lands mid-pipeline.
        harness.input(active_mode(), false, true);
        assert_eq!((harness.starts, harness.stops), (1, 1));
        assert!(harness.is_processing());

        harness.processing_finished();
        assert_eq!(harness.starts, 1, "a bare release must not arm a start");
        assert!(harness.is_idle());
    }

    #[test]
    fn external_edges_toggle_under_push_to_talk() {
        let mut harness = Harness::new();
        harness.external_press(active_mode(), true);
        assert!(harness.is_recording());

        // A signal has no key to release, so it latches on the press edge and
        // the next edge stops it, debounce window or not.
        harness.external_press(active_mode(), true);
        assert_eq!((harness.starts, harness.stops), (1, 1));
        assert!(harness.is_processing());
    }

    #[test]
    fn a_failed_start_leaves_the_machine_idle() {
        let mut harness = Harness::new();
        harness.microphone_opens = false;
        harness.input(active_mode(), true, false);
        assert_eq!(harness.starts, 1);
        assert!(
            harness.is_idle(),
            "a start that never recorded must not arm a stop"
        );
    }

    #[test]
    fn busy_toggle_press_is_remembered_and_starts_when_the_pipeline_drains() {
        let mut harness = Harness::new();
        harness.input(active_mode(), true, false);
        harness.advance(DEBOUNCE);
        harness.input(active_mode(), true, false);
        assert!(harness.is_processing());

        // The press that lands mid-pipeline is the user's next recording.
        harness.advance(DEBOUNCE);
        harness.input(active_mode(), true, false);
        assert_eq!(
            harness.starts, 1,
            "nothing starts while the pipeline is busy"
        );

        harness.processing_finished();
        assert_eq!(harness.starts, 2);
        assert!(harness.is_recording());
    }

    #[test]
    fn two_busy_toggle_presses_cancel_out() {
        let mut harness = Harness::new();
        harness.input(active_mode(), true, false);
        harness.advance(DEBOUNCE);
        harness.input(active_mode(), true, false);

        harness.advance(DEBOUNCE);
        harness.input(active_mode(), true, false);
        harness.advance(DEBOUNCE);
        harness.input(active_mode(), true, false);

        harness.processing_finished();
        assert_eq!(
            harness.starts, 1,
            "an even number of busy presses is a no-op"
        );
        assert!(harness.is_idle());
    }

    /// The Stop button is not a toggle edge. Pressed while the pipeline is
    /// still working it is dropped, never remembered: it must not open the
    /// microphone again the moment the previous dictation lands.
    #[test]
    fn a_finish_during_processing_starts_nothing_when_the_pipeline_drains() {
        let mut harness = Harness::new();
        harness.input(active_mode(), true, false);
        harness.advance(DEBOUNCE);
        harness.finish();
        assert!(harness.is_processing());

        harness.finish();
        harness.processing_finished();
        assert_eq!(harness.starts, 1, "a stop is never a deferred start");
        assert!(harness.is_idle());
    }

    /// The button ends whatever is recording, including a recording that a
    /// command shortcut opened, which the dictation toggle would ignore.
    #[test]
    fn a_finish_ends_a_recording_another_intent_opened() {
        let mut harness = Harness::new();
        harness.input(TranscriptionIntent::Command, true, true);
        assert!(harness.is_recording());

        harness.advance(DEBOUNCE);
        harness.input(active_mode(), true, false);
        assert_eq!(
            harness.stops, 0,
            "the dictation toggle belongs to its own intent"
        );

        harness.finish();
        assert_eq!((harness.starts, harness.stops), (1, 1));
        assert!(harness.is_processing());
    }

    #[test]
    fn a_push_to_talk_release_during_the_busy_window_forgets_the_press() {
        let mut harness = Harness::new();
        press_and_hold(&mut harness);
        // A hold, so its release stops the recording instead of latching it.
        harness.advance(TAP_LATCH_THRESHOLD);
        harness.input(active_mode(), false, true);
        harness.grace_expired();
        assert!(harness.is_processing());

        // The user tapped again while the pipeline was busy, then let go before
        // it drained: the tap is over, so nothing should start.
        harness.advance(DEBOUNCE);
        harness.input(active_mode(), true, true);
        harness.input(active_mode(), false, true);

        harness.processing_finished();
        assert_eq!(harness.starts, 1);
        assert!(harness.is_idle());
    }

    #[test]
    fn a_second_intent_never_steals_a_pending_busy_press() {
        let mut harness = Harness::new();
        harness.input(active_mode(), true, false);
        harness.advance(DEBOUNCE);
        harness.input(active_mode(), true, false);

        harness.advance(DEBOUNCE);
        harness.input(active_mode(), true, false);
        harness.advance(DEBOUNCE);
        harness.input(TranscriptionIntent::ActiveModeWithPostProcess, true, false);

        harness.processing_finished();
        assert_eq!(harness.starts, 2);
        assert!(matches!(
            &harness.state.stage,
            Stage::Recording { intent, .. } if intent == &active_mode()
        ));
    }

    #[test]
    fn cancel_drops_a_remembered_press() {
        let mut harness = Harness::new();
        harness.input(active_mode(), true, false);
        harness.advance(DEBOUNCE);
        harness.input(active_mode(), true, false);
        harness.advance(DEBOUNCE);
        harness.input(active_mode(), true, false);

        harness.state.on_cancel(false);
        harness.processing_finished();
        assert_eq!(
            harness.starts, 1,
            "cancel means silence, not a deferred start"
        );
        assert!(harness.is_idle());
    }

    #[test]
    fn external_toggles_keep_parity_inside_the_debounce_window() {
        let mut harness = Harness::new();
        harness.external_press(active_mode(), false);
        assert!(harness.is_recording());

        // 5 ms later, well inside DEBOUNCE: a keyboard repeat would be dropped,
        // but a signal or CLI edge is deliberate and must still toggle.
        harness.external_press(active_mode(), false);
        assert_eq!((harness.starts, harness.stops), (1, 1));
        assert!(harness.is_processing());
    }

    #[test]
    fn keyboard_presses_inside_the_debounce_window_are_dropped() {
        let mut harness = Harness::new();
        harness.input(active_mode(), true, false);
        harness.input(active_mode(), true, false);
        assert_eq!((harness.starts, harness.stops), (1, 0));
        assert!(harness.is_recording());
    }

    #[test]
    fn busy_classification_covers_every_edge() {
        // Toggle: alternate remember/forget, ignore releases.
        assert_eq!(
            classify_busy_input(true, false, false),
            BusyAction::Remember
        );
        assert_eq!(classify_busy_input(true, false, true), BusyAction::Forget);
        assert_eq!(classify_busy_input(false, false, false), BusyAction::Ignore);
        assert_eq!(classify_busy_input(false, false, true), BusyAction::Ignore);
        // Push-to-talk: a held key starts on drain, a release cancels it.
        assert_eq!(classify_busy_input(true, true, false), BusyAction::Remember);
        assert_eq!(classify_busy_input(true, true, true), BusyAction::Remember);
        assert_eq!(classify_busy_input(false, true, true), BusyAction::Forget);
        assert_eq!(classify_busy_input(false, true, false), BusyAction::Ignore);
    }
}
