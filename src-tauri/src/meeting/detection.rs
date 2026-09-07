//! Automatic meeting detection: signals in, one prompt decision out.
//!
//! # Layering
//!
//! | Layer | Owner | Invariant it owns |
//! |---|---|---|
//! | Signals | `apps`, `calendar`, `input_device` | What the platform actually reports, including what it cannot report |
//! | Decision | `machine` | Whether a prompt is warranted, and which one |
//! | Delivery | `notify` | How a prompt reaches the operator, and what a click means |
//! | Capture | `MeetingSessionManager` | Consent, the capture lease, and persistence |
//!
//! The runtime here is glue between those four and owns nothing else. In
//! particular it never starts a capture: the strongest thing an affirmative
//! click does is create a preflight and put the existing consent screen in
//! front of the operator, which is byte-for-byte the tray's own
//! `start_meeting_notes` path. `MeetingSessionManager::start` persists a
//! per-attempt consent receipt naming the microphone and system-audio
//! acknowledgements, and detection has no authority to forge one.
//!
//! # What it does not observe
//!
//! * **Per-process microphone use.** CoreAudio's device-in-use property is
//!   device-global. Sona's own dictation raises it, which is why
//!   `self_holds_input_device` exists and why an active Sona microphone
//!   suppresses the ad-hoc path outright. The property also lags a close, so
//!   `self_mic_just_closed` covers the interval after Sona's own stream ends
//!   — see `input_device::SELF_MIC_COOLDOWN`. Both are written from the
//!   stream, not from the recording state: the stream is what holds the
//!   device, and it outlives the dictation that opened it.
//! * **Bluetooth microphones.** They under-report through that property, so
//!   AirPods-style headsets are a known false negative on the ad-hoc path.

pub mod apps;
pub mod calendar;
pub mod input_device;
pub mod machine;
pub mod notify;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::AppHandle;
use tauri_specta::Event as _;
use uuid::Uuid;

use crate::meeting::consent_panel::ConsentPanelLayout;
use crate::meeting::people_types::PersonBriefingRow;
use crate::meeting::session::{MeetingSessionManager, MeetingTitleSetRequest};
use crate::meeting::types::{
    MeetingArtifactState, MeetingNavigationDestination, MeetingOperationId, MeetingPhase,
    MeetingSessionId, MeetingSessionSnapshot, SourceKind,
};
use crate::meeting::workflow_types::WorkflowEventKind;
use crate::settings::AppSettings;

use apps::{BrowserTitleReader, RunningApp, RunningAppsSource};
use calendar::{CalendarAccess, CalendarSource};
use input_device::{
    InputDeviceLevel, InputDeviceObserver, InputDeviceState, SelfInputDeviceLease,
    SELF_MIC_COOLDOWN,
};
use machine::{
    evaluate, evaluate_stop, CalendarEventSummary, CalendarSignal, DetectionInputs,
    DetectionOutcome, DetectionPolicy, MicSignal, OutputSignal, PromptKind, RecentCapture,
    StopInputs, SuppressReason,
};
use notify::{
    ConsentPromptSurface, NotificationAccess, PanelCommand, PanelSlot, PromptResponder,
    PromptResponse,
};

/// Schema marker on both detection events, matching the meeting events' shape.
pub const DETECTION_EVENT_SCHEMA_VERSION: u32 = 2;

/// Tick interval. Chosen so the T-60s calendar prompt lands between T-60 and
/// T-45: tight enough to be useful, loose enough that the idle path costs a
/// settings read, an atomic load, and one in-process application list — and no
/// database read at all.
const TICK: Duration = Duration::from_secs(15);
const PANEL_ACK_WINDOW: Duration = Duration::from_millis(750);
const WRAP_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// A wall-clock jump this much larger than the monotonic clock's advance means
/// the host slept. Both clocks are read on the same tick, so the only source of
/// a gap this size is suspend.
const SLEEP_DETECTION_SLACK: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DetectionPromptDelivery {
    Panel,
    Notification,
    InAppOnly,
}

/// Emitted when detection wants an answer. The frontend renders localized copy
/// from these fields; the native notification carries §5.4's English pattern.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DetectionPromptEvent {
    pub event_schema_version: u32,
    /// Opaque handle. Echo it back through `detection_prompt_respond`.
    pub prompt_id: String,
    pub prompt: PromptKind,
    /// §5.4's English copy, exactly as the notification shows it.
    pub notification_title: String,
    /// The surface that owns this delivery. Only `in_app_only` should produce a
    /// toast in the main window; `panel` and `notification` are already visible.
    pub delivery: DetectionPromptDelivery,
    /// One short explanation rendered only after the panel has acknowledged its
    /// first successful prompt delivery.
    pub show_introduction: bool,
    /// What this prompt's series remembers about announcing itself, which is the
    /// state the panel's announce checkbox opens in. False for a meeting with no
    /// series behind it: there is nothing to remember and nothing remembered.
    pub announce_in_chat: bool,
}

/// Registers the payload and its event name with the specta builder. Runtime
/// emits use the typed `tauri_specta::Event` method, so construction and the
/// wire name stay together.
impl tauri_specta::Event for DetectionPromptEvent {
    const NAME: &'static str = "detection-prompt";
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MeetingPrepParticipant {
    pub name: String,
    pub meetings_count: u64,
    pub organization: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MeetingPrepCard {
    pub event_key: String,
    pub series_key: String,
    pub title: String,
    pub start_utc_ms: i64,
    pub last_meeting_id: MeetingSessionId,
    pub headline: String,
    pub mine_open_loops: Vec<String>,
    pub mine_open_loop_count: u64,
    pub waiting_on_count: u64,
    pub participants: Vec<MeetingPrepParticipant>,
    pub can_record_when_starts: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MeetingWrapCard {
    pub session_id: MeetingSessionId,
    pub title: String,
    pub headline: String,
    pub unresolved_speaker_count: Option<u64>,
    pub follow_up_count: u64,
    pub waiting_on_count: u64,
    pub waiting_on_names: Vec<String>,
}

/// Shown while a call Sona started by itself is recording, so an auto-start is
/// never something the operator has to discover afterwards.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MeetingRecordingCard {
    pub session_id: MeetingSessionId,
    pub bundle_id: String,
    pub app_name: String,
    pub started_at_utc_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", content = "card", rename_all = "snake_case")]
pub enum MeetingRitual {
    Prep(MeetingPrepCard),
    Wrap(MeetingWrapCard),
    Recording(MeetingRecordingCard),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingRitualAction {
    PrepRecordWhenStarts,
    PrepOpenBrief,
    PrepDismiss,
    WrapOpenNotes,
    WrapFollowUpCopied,
    WrapDone,
    RecordingStop,
    /// Stop, and take this application off the auto-record list. One gesture,
    /// because an operator who wants the recording to end also wants the reason
    /// it started to end.
    RecordingForgetApp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MeetingRitualEvent {
    pub event_schema_version: u32,
    pub ritual_id: String,
    pub ritual: MeetingRitual,
    pub notification_title: String,
    pub delivery: DetectionPromptDelivery,
}

impl tauri_specta::Event for MeetingRitualEvent {
    const NAME: &'static str = "meeting-ritual";
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MeetingRitualRetractedEvent {
    pub event_schema_version: u32,
    pub ritual_id: String,
}

impl tauri_specta::Event for MeetingRitualRetractedEvent {
    const NAME: &'static str = "meeting-ritual-retracted";
}

/// The countdown half of §5.3 case 1, and everything the pre-meeting card
/// renders about the event it is counting down to.
///
/// The event is carried whole rather than flattened into a copy of two of its
/// fields: the card shows the calendar's own facts, and a second copy of them
/// here would be a second place for them to go stale.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DetectionCountdown {
    pub event: CalendarEventSummary,
    pub seconds_to_start: i64,
    pub briefing: Vec<PersonBriefingRow>,
}

/// The operator-editable half of detection, read and written as one unit.
///
/// One value rather than six independent setters: these fields only make sense
/// together — turning the calendar path on while detection itself is off is not
/// a state the UI should be able to produce halfway through.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DetectionSettings {
    pub enabled: bool,
    pub calendar_enabled: bool,
    pub any_mic_activity: bool,
    pub auto_start_on_open_pane: bool,
    pub meeting_apps: Vec<String>,
    /// Bundle IDs that record without a prompt. A subset of `meeting_apps` in
    /// practice: an entry detection does not watch grants nothing.
    pub auto_record_apps: Vec<String>,
}

impl DetectionSettings {
    fn from_app_settings(settings: &AppSettings) -> Self {
        Self {
            enabled: settings.detection_enabled,
            calendar_enabled: settings.detection_calendar_enabled,
            any_mic_activity: settings.detection_any_mic_activity,
            auto_start_on_open_pane: settings.detection_auto_start_on_open_pane,
            meeting_apps: settings.detection_meeting_apps.clone(),
            auto_record_apps: settings.detection_auto_record_apps.clone(),
        }
    }
}

/// Everything the operator can see about what detection is doing. Emitted on
/// change, and readable on demand through `detection_status_get`.
///
/// This exists because silent detection is indistinguishable from broken
/// detection. Every suppression reason and every unavailable signal is named
/// here so the failure modes above are visible rather than inferred.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DetectionStatus {
    pub event_schema_version: u32,
    pub settings: DetectionSettings,
    pub calendar_access: CalendarAccess,
    pub notification_access: NotificationAccess,
    /// True when some process holds the default input device.
    pub input_device_active: bool,
    /// True when that process is Sona itself.
    pub sona_holds_input_device: bool,
    /// Why detection is quiet, when it is.
    pub suppress_reason: Option<SuppressReason>,
    pub countdown: Option<DetectionCountdown>,
    /// The call a hand-started capture adopted as its stop trigger.
    pub adopted_call: Option<machine::AdoptedCall>,
    /// Allowlisted bundle IDs whose application is running right now. Empty is a
    /// legitimate answer and the settings UI shows it as such.
    pub running_meeting_apps: Vec<String>,
    /// True when the Bluetooth-microphone false negative applies: nothing is
    /// reported as holding the input device while a meeting app is frontmost.
    pub input_device_reporting_suspect: bool,
}

impl tauri_specta::Event for DetectionStatus {
    const NAME: &'static str = "detection-status";
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DetectionPromptRetractionReason {
    TriggerAppQuit,
    EventEnded,
    MicEpisodeEnded,
    /// A call prompt's call is no longer live. Its episode is the call, not
    /// the microphone: the call it offered to record may never have raised
    /// the input device at all.
    CallEnded,
    Resolved,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DetectionPromptRetractedEvent {
    pub event_schema_version: u32,
    pub prompt_id: String,
    pub reason: DetectionPromptRetractionReason,
}

impl tauri_specta::Event for DetectionPromptRetractedEvent {
    const NAME: &'static str = "detection-prompt-retracted";
}

/// A prompt awaiting an answer.
#[derive(Clone, Debug)]
struct PendingPrompt {
    prompt: PromptKind,
    /// The calendar event on the table when this prompt was raised, for the
    /// facts a capture started from it remembers.
    calendar_event: Option<CalendarEventSummary>,
    show_introduction: bool,
    /// What this prompt's series already decided about announcing itself.
    announce_in_chat: bool,
}

impl PendingPrompt {
    /// The scheduled end this prompt, and a capture started from it, lives
    /// until. Only a calendar prompt has one, and only from its own event: a
    /// Zoom prompt raised while some block sits on the calendar has nothing to
    /// do with that block, and must not be retracted or stopped when it ends.
    fn linked_event_end(&self) -> Option<i64> {
        let PromptKind::CalendarEvent { event_key, .. } = &self.prompt else {
            return None;
        };
        self.calendar_event
            .as_ref()
            .filter(|event| event.event_key == *event_key)
            .map(|event| event.end_utc_ms)
    }
}

#[derive(Clone, Debug)]
struct PendingRitual {
    ritual: MeetingRitual,
    notification_title: String,
    idle_generation: u64,
}

#[derive(Clone, Debug)]
enum PendingPanel {
    Prompt(PendingPrompt),
    Ritual(PendingRitual),
}

impl PendingPanel {
    const fn is_prompt(&self) -> bool {
        matches!(self, Self::Prompt(_))
    }
}
/// A capture detection is watching, and what may stop it.
#[derive(Clone, Debug)]
struct TrackedCapture {
    session_id: MeetingSessionId,
    kind: TrackedCaptureKind,
}

#[derive(Clone, Debug)]
enum TrackedCaptureKind {
    Detection {
        trigger_bundle_id: Option<String>,
        event_end_utc_ms: Option<i64>,
        /// The lanes the session actually started, as its snapshot reported them.
        /// A stop rule about a device applies only to a capture listening to it.
        sources: Vec<SourceKind>,
        /// What the default output device has done since a call app's trigger tied
        /// this capture to a call. Only that trigger arms it, and only at start.
        call_output: Option<machine::CallOutputWatch>,
    },
    Operator(machine::OperatorCapture),
}
struct CaptureTick<'a> {
    running: &'a [RunningApp],
    mic: MicSignal,
    output: OutputSignal,
    sona_holds: bool,
    call: &'a machine::CallSignal,
    call_live: bool,
}
#[derive(Debug, Eq, PartialEq)]
enum CaptureTransition {
    Continue,
    Adopted(machine::AdoptedCall),
    Stop(machine::StopTrigger),
}

impl TrackedCapture {
    fn detection(
        snapshot: &MeetingSessionSnapshot,
        trigger_bundle_id: Option<String>,
        event_end_utc_ms: Option<i64>,
    ) -> Self {
        let call_output = trigger_bundle_id
            .as_deref()
            .is_some_and(apps::is_call_app_bundle_id)
            .then(machine::CallOutputWatch::default);
        Self {
            session_id: snapshot.session_id,
            kind: TrackedCaptureKind::Detection {
                trigger_bundle_id,
                event_end_utc_ms,
                sources: snapshot
                    .sources
                    .iter()
                    .map(|source| source.source_kind)
                    .collect(),
                call_output,
            },
        }
    }

    fn operator(snapshot: &MeetingSessionSnapshot) -> Self {
        Self {
            session_id: snapshot.session_id,
            kind: TrackedCaptureKind::Operator(machine::OperatorCapture::Waiting),
        }
    }

    fn keeps_runtime_active(&self) -> bool {
        matches!(
            self.kind,
            TrackedCaptureKind::Detection { .. }
                | TrackedCaptureKind::Operator(machine::OperatorCapture::Adopted { .. })
        )
    }

    fn observe_call_output(
        &mut self,
        output: OutputSignal,
        now_utc_ms: i64,
    ) -> Option<machine::CallOutputWatch> {
        let watch = match &mut self.kind {
            TrackedCaptureKind::Detection { call_output, .. } => call_output.as_mut()?,
            TrackedCaptureKind::Operator(machine::OperatorCapture::Waiting) => return None,
            TrackedCaptureKind::Operator(machine::OperatorCapture::Adopted { watch, .. }) => watch,
        };
        watch.observe(output, now_utc_ms);
        Some(*watch)
    }

    fn microphone_lane(&self) -> bool {
        match &self.kind {
            TrackedCaptureKind::Detection { sources, .. } => {
                sources.contains(&SourceKind::Microphone)
            }
            TrackedCaptureKind::Operator(_) => false,
        }
    }
}

/// Above every prompt. The recording card is raised while a capture holds the
/// panel, and nothing raised during that capture may take the panel from it.
const RECORDING_CARD_PRIORITY: u8 = 14;

#[derive(Default)]
struct RuntimeState {
    panel: PanelSlot<PendingPanel>,
    /// Calendar event keys already prompted for. Without this the 15s tick would
    /// re-notify for the same event every tick until it ended — the most likely
    /// new failure this subsystem introduces, and the cheapest to block.
    prompted_events: HashSet<String>,
    /// Calendar events whose durable briefing workflow has been dispatched.
    briefing_events: HashSet<String>,
    /// Bundle IDs already prompted for during the current input-device episode.
    /// Cleared when the device goes idle, so a second meeting in the same app
    /// prompts again.
    prompted_apps: HashSet<String>,
    /// Allowlisted apps the operator has been in front of since the input
    /// device went active. Presence is not participation: this is what lets
    /// `apps::app_signal` tell a meeting app in use from one merely open.
    /// Cleared with `prompted_apps`, at the same two places.
    apps_used: HashSet<String>,
    /// Bundle IDs the call path has already acted on for the call in progress.
    /// Cleared when no call is live, which is a different boundary from
    /// `prompted_apps`: a call detected on the output signal alone never raises
    /// the input device, so the microphone episode that clears that set never
    /// begins and an auto-start would otherwise re-fire every tick.
    acted_calls: HashSet<String>,
    /// Last emitted status, so the event fires on change rather than on a timer.
    last_status: Option<DetectionStatus>,
    tracked: Option<TrackedCapture>,
    recent: Option<RecentCapture>,
    /// Set when a tick observes a sleep boundary.
    slept: bool,
}

impl RuntimeState {
    /// Claims the call in progress for `bundle_id`, reporting whether this is
    /// the first claim. False means a later tick inside the same call, which
    /// must not act again.
    fn claim_call(&mut self, bundle_id: &str) -> bool {
        self.acted_calls.insert(bundle_id.to_string())
    }

    /// Re-arms the call path when no call evidence is present. A live call
    /// keeps every claim so later ticks cannot act again.
    fn rearm_call_claims(&mut self, call_evidence: bool) {
        if !call_evidence {
            self.acted_calls.clear();
        }
    }

    /// The input-device episode is over. The next meeting in the same app is
    /// a fresh decision, and which apps the operator used starts from nothing.
    fn end_input_episode(&mut self) {
        self.prompted_apps.clear();
        self.apps_used.clear();
    }

    /// Folds this tick's output level into the tracked call capture and hands
    /// back the watch for the stop rule to read. `None` when `session_id` is
    /// not the tracked capture or it has not been tied to a call.
    fn observe_call_output(
        &mut self,
        session_id: MeetingSessionId,
        output: OutputSignal,
        now_utc_ms: i64,
    ) -> Option<machine::CallOutputWatch> {
        let tracked = self
            .tracked
            .as_mut()
            .filter(|tracked| tracked.session_id == session_id)?;
        tracked.observe_call_output(output, now_utc_ms)
    }

    fn adopt_operator_call(
        &mut self,
        session_id: MeetingSessionId,
        call: &machine::CallSignal,
        call_live: bool,
    ) -> Option<machine::AdoptedCall> {
        let tracked = self
            .tracked
            .as_mut()
            .filter(|tracked| tracked.session_id == session_id)?;
        let TrackedCaptureKind::Operator(capture) = &mut tracked.kind else {
            return None;
        };
        let adopted = machine::adopt_operator_call(capture, call, call_live)?;
        *capture = machine::OperatorCapture::Adopted {
            call: adopted.clone(),
            watch: machine::CallOutputWatch::default(),
        };
        self.slept = false;
        Some(adopted)
    }

    fn adopted_call(&self) -> Option<machine::AdoptedCall> {
        match self.tracked.as_ref().map(|tracked| &tracked.kind) {
            Some(TrackedCaptureKind::Operator(machine::OperatorCapture::Adopted {
                call, ..
            })) => Some(call.clone()),
            _ => None,
        }
    }

    /// Raises the recording card for the tracked capture into the panel slot.
    /// `None` when the capture the card names is no longer the tracked one —
    /// a start that lost a race against its own stop, which must show nothing.
    ///
    /// Raised as panel-eligible although a capture is running: the capture
    /// owns the panel, and this card is the capture's. Every prompt in the
    /// slot was made ineligible by `begin_capture`, so the card never displaces
    /// one; the priority only says that nothing may displace the card.
    fn raise_recording_card(
        &mut self,
        ritual_id: String,
        card: MeetingRecordingCard,
    ) -> Option<Vec<PanelCommand<PendingPanel>>> {
        self.tracked
            .as_ref()
            .filter(|tracked| tracked.session_id == card.session_id)?;
        let pending = PendingPanel::Ritual(PendingRitual {
            notification_title: format!("{} call — recording", card.app_name),
            ritual: MeetingRitual::Recording(card),
            idle_generation: 0,
        });
        Some(
            self.panel
                .raise(ritual_id, pending, RECORDING_CARD_PRIORITY, true),
        )
    }

    /// The pending recording card that names `session_id`, by ritual id.
    fn recording_card_id(&self, session_id: MeetingSessionId) -> Option<String> {
        self.panel
            .iter()
            .find_map(|(ritual_id, pending)| match pending {
                PendingPanel::Ritual(PendingRitual {
                    ritual: MeetingRitual::Recording(card),
                    ..
                }) if card.session_id == session_id => Some(ritual_id.to_string()),
                _ => None,
            })
    }

    /// Starts tracking a capture, and remembers it for the cross-link window.
    ///
    /// A sleep boundary is per capture, not per process: `slept` is set by
    /// any loop iteration that crosses one, so without this reset a Mac that
    /// slept while nothing was tracked would stop the next capture on its
    /// first tick with `SleepBoundary`.
    fn begin_tracked(&mut self, tracked: TrackedCapture, recent: RecentCapture) {
        self.tracked = Some(tracked);
        self.recent = Some(recent);
        self.slept = false;
    }

    /// Stops tracking `session_id` and hands back what was tracked, or `None`
    /// when that was not the tracked capture. A stop for some other session is
    /// how a stale receipt arrives, and it must change nothing.
    fn end_tracked(&mut self, session_id: MeetingSessionId) -> Option<TrackedCapture> {
        let tracked = self
            .tracked
            .take_if(|tracked| tracked.session_id == session_id)?;
        self.slept = false;
        Some(tracked)
    }
}

fn evaluate_running_capture_transition(
    state: &mut RuntimeState,
    tracked: &TrackedCapture,
    tick: CaptureTick<'_>,
    now_utc_ms: i64,
) -> CaptureTransition {
    match &tracked.kind {
        TrackedCaptureKind::Detection {
            trigger_bundle_id,
            event_end_utc_ms,
            ..
        } => {
            let call_output =
                state.observe_call_output(tracked.session_id, tick.output, now_utc_ms);
            let trigger = evaluate_stop(&StopInputs {
                now_utc_ms,
                linked_event_end_utc_ms: *event_end_utc_ms,
                self_holds_input_device: tick.sona_holds,
                device_running_somewhere: tick.mic == MicSignal::Active,
                microphone_lane: tracked.microphone_lane(),
                call_output,
                trigger_app_running: trigger_bundle_id
                    .as_deref()
                    .is_none_or(|bundle_id| apps::is_app_running(tick.running, bundle_id)),
                slept_since_start: state.slept,
            });
            trigger.map_or(CaptureTransition::Continue, CaptureTransition::Stop)
        }
        TrackedCaptureKind::Operator(machine::OperatorCapture::Waiting) => {
            let Some(adopted) =
                state.adopt_operator_call(tracked.session_id, tick.call, tick.call_live)
            else {
                return CaptureTransition::Continue;
            };
            CaptureTransition::Adopted(adopted)
        }
        TrackedCaptureKind::Operator(machine::OperatorCapture::Adopted {
            call: adopted, ..
        }) => {
            let call_output =
                state.observe_call_output(tracked.session_id, tick.output, now_utc_ms);
            let trigger = evaluate_stop(&StopInputs {
                now_utc_ms,
                linked_event_end_utc_ms: None,
                self_holds_input_device: tick.sona_holds,
                device_running_somewhere: tick.mic == MicSignal::Active,
                microphone_lane: false,
                call_output,
                trigger_app_running: apps::is_app_running(tick.running, &adopted.bundle_id),
                slept_since_start: state.slept,
            });
            trigger.map_or(CaptureTransition::Continue, CaptureTransition::Stop)
        }
    }
}

/// Wakes the tick thread early. The ad-hoc path must fire on the input-device
/// transition itself, not on the next scheduled tick.
#[derive(Debug, Default)]
struct Wakeup {
    flagged: Mutex<bool>,
    signal: Condvar,
}

impl Wakeup {
    fn wake(&self) {
        let mut flagged = self
            .flagged
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *flagged = true;
        self.signal.notify_all();
    }

    /// Waits for a wake, optionally bounded by the next scheduled tick.
    ///
    /// The pre-check is load-bearing, not an optimization: an input-device edge
    /// arriving while the loop is inside `tick` sets the flag with nobody
    /// waiting. Without it, that edge is lost until the next scheduled tick.
    fn wait(&self, timeout: Option<Duration>) {
        let mut flagged = self
            .flagged
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *flagged {
            *flagged = false;
            return;
        }
        flagged = match timeout {
            Some(timeout) => {
                self.signal
                    .wait_timeout(flagged, timeout)
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .0
            }
            None => self
                .signal
                .wait(flagged)
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        };
        *flagged = false;
    }
}

fn tick_interval(enabled: bool, tracking_capture: bool) -> Option<Duration> {
    (enabled || tracking_capture).then_some(TICK)
}

/// Owns the detection loop and the platform observers it drives.
pub struct DetectionRuntime {
    app: AppHandle,
    meetings: Arc<MeetingSessionManager>,
    self_lease: Arc<SelfInputDeviceLease>,
    calendar: Arc<dyn CalendarSource>,
    running_apps: Arc<dyn RunningAppsSource>,
    input: Arc<dyn InputDeviceState>,
    prompts: Arc<dyn ConsentPromptSurface>,
    /// Reads the frontmost browser's window on demand, through the same
    /// observer whose activation edges fill the offer store.
    browser_titles: Arc<dyn BrowserTitleReader>,
    state: Arc<Mutex<RuntimeState>>,
    wakeup: Arc<Wakeup>,
    stop: Arc<AtomicBool>,
    enabled: AtomicBool,
}

impl DetectionRuntime {
    /// Assembles a runtime from explicit collaborators. The platform-backed
    /// wiring lives in `start`.
    ///
    /// The long argument list is the point: every platform dependency is named
    /// and substitutable, which is what lets the decision table be tested
    /// without a calendar, a microphone, or a notification center.
    #[allow(clippy::too_many_arguments)]
    pub fn with_parts(
        app: AppHandle,
        meetings: Arc<MeetingSessionManager>,
        self_lease: Arc<SelfInputDeviceLease>,
        calendar: Arc<dyn CalendarSource>,
        running_apps: Arc<dyn RunningAppsSource>,
        input: Arc<dyn InputDeviceState>,
        prompts: Arc<dyn ConsentPromptSurface>,
        browser_titles: Arc<dyn BrowserTitleReader>,
    ) -> Self {
        // The lease is created by whoever opens the microphone stream, which
        // happens before this thread exists. Handing it the wakeup here is what
        // turns a release into a tick instead of a fact the loop notices up to
        // `TICK` later — and a stale ad-hoc prompt is on screen for that whole
        // interval.
        let wakeup = Arc::new(Wakeup::default());
        self_lease.attach_wakeup(Arc::clone(&wakeup));
        Self {
            app,
            meetings,
            self_lease,
            calendar,
            running_apps,
            input,
            prompts,
            browser_titles,
            state: Arc::new(Mutex::new(RuntimeState::default())),
            wakeup,
            stop: Arc::new(AtomicBool::new(false)),
            enabled: AtomicBool::new(false),
        }
    }

    /// Starts the tick thread. Returns immediately; nothing here blocks startup,
    /// and nothing here touches a platform framework.
    ///
    /// `level` is handed over rather than read back off `self`, which stores it
    /// coerced to `Arc<dyn InputDeviceState>`: the CoreAudio monitor needs the
    /// concrete type, and a trait object cannot be downcast to it. The thread
    /// owns the monitor for exactly as long as the loop runs.
    pub fn spawn_loop(self: &Arc<Self>, level: Arc<InputDeviceLevel>) {
        self.enabled.store(
            crate::settings::get_settings(&self.app).detection_enabled,
            Ordering::Release,
        );
        let runtime = Arc::clone(self);
        thread::Builder::new()
            .name("sona-meeting-detection".to_string())
            .spawn(move || runtime.run(level))
            .map(|_| ())
            .unwrap_or_else(|error| {
                log::warn!("Meeting detection loop is unavailable: {error}");
            });
    }

    /// Applies the master toggle to the loop lifecycle. Turning detection off
    /// wakes the thread so it can drop the CoreAudio observer and park.
    pub(crate) fn set_enabled(&self, enabled: bool) {
        if self.enabled.swap(enabled, Ordering::AcqRel) != enabled {
            self.wakeup.wake();
        }
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        self.wakeup.wake();
    }

    pub fn app_handle(&self) -> &AppHandle {
        &self.app
    }

    /// The observer handed to the CoreAudio monitor.
    pub fn input_observer(&self) -> Arc<dyn InputDeviceObserver> {
        Arc::new(WakeOnInputChange {
            wakeup: Arc::clone(&self.wakeup),
            state: Arc::clone(&self.state),
        })
    }

    /// The responder handed to the notification presenter.
    pub fn prompt_responder(self: &Arc<Self>) -> Arc<dyn PromptResponder> {
        Arc::new(RuntimeResponder {
            runtime: Arc::clone(self),
        })
    }

    fn run(self: Arc<Self>, level: Arc<InputDeviceLevel>) {
        let mut monitor = None;
        let mut observing = false;
        let mut previous_wall = utc_now_ms();
        let mut previous_monotonic = Instant::now();

        while !self.stop.load(Ordering::Acquire) {
            let tracking_capture = self
                .lock()
                .tracked
                .as_ref()
                .is_some_and(TrackedCapture::keeps_runtime_active);
            let interval = tick_interval(self.enabled.load(Ordering::Acquire), tracking_capture);
            let Some(interval) = interval else {
                // Dropping the monitor unregisters its CoreAudio listener. The
                // condition wait has no deadline, so disabled detection has no
                // platform observer and no periodic tick.
                monitor = None;
                observing = false;
                self.wakeup.wait(None);
                continue;
            };

            if !observing {
                monitor = self.start_input_monitor(&level);
                previous_wall = utc_now_ms();
                previous_monotonic = Instant::now();
                observing = true;
            }

            self.wakeup.wait(Some(interval));
            if self.stop.load(Ordering::Acquire) {
                return;
            }
            let tracking_capture = self
                .lock()
                .tracked
                .as_ref()
                .is_some_and(TrackedCapture::keeps_runtime_active);
            if tick_interval(self.enabled.load(Ordering::Acquire), tracking_capture).is_none() {
                continue;
            }

            let wall = utc_now_ms();
            let monotonic = Instant::now();
            if slept_between(previous_wall, wall, previous_monotonic, monotonic) {
                self.lock().slept = true;
            }
            previous_wall = wall;
            previous_monotonic = monotonic;
            // The property listener is per-device, so switching from the
            // built-in microphone to a USB interface would silently end the
            // ad-hoc path. Re-registering on the new device is what keeps the
            // microphone dimension alive across a device change.
            self.refresh_input_monitor(&mut monitor, &level);
            let started = Instant::now();
            self.tick(wall);
            log::debug!("Meeting detection tick finished in {:?}", started.elapsed());
        }
    }

    /// Registers the CoreAudio listener, or reports why the microphone
    /// dimension is unavailable. A missing monitor degrades detection to the
    /// calendar path and manual start rather than failing the loop.
    #[cfg(target_os = "macos")]
    fn start_input_monitor(
        self: &Arc<Self>,
        level: &Arc<InputDeviceLevel>,
    ) -> Option<input_device::CoreAudioInputMonitor> {
        match input_device::CoreAudioInputMonitor::start(Arc::clone(level), self.input_observer()) {
            Ok(monitor) => Some(monitor),
            Err(error) => {
                log::warn!(
                    "Meeting detection has no microphone signal: {error:?}. The calendar path \
                     and manual start are unaffected."
                );
                None
            }
        }
    }

    /// Re-registers the listener when the default input device changed.
    ///
    /// Owned by the loop rather than by `app_handle.manage`, which is what held
    /// the monitor before: state managed by the app handle lives until process
    /// exit and its `Drop` may never run, so the listener was never removed.
    /// Thread-owned gives deterministic teardown on `shutdown`.
    #[cfg(target_os = "macos")]
    fn refresh_input_monitor(
        self: &Arc<Self>,
        monitor: &mut Option<input_device::CoreAudioInputMonitor>,
        level: &Arc<InputDeviceLevel>,
    ) {
        let changed = monitor
            .as_ref()
            .is_some_and(input_device::CoreAudioInputMonitor::device_changed);
        if !changed {
            return;
        }
        // Dropped before the replacement is created: `Drop` removes the listener
        // from the old device, and registering the new one first would briefly
        // leave two listeners writing the same level.
        *monitor = None;
        *monitor = self.start_input_monitor(level);
    }

    #[cfg(not(target_os = "macos"))]
    fn start_input_monitor(self: &Arc<Self>, _level: &Arc<InputDeviceLevel>) -> Option<()> {
        Some(())
    }

    #[cfg(not(target_os = "macos"))]
    fn refresh_input_monitor(
        self: &Arc<Self>,
        _monitor: &mut Option<()>,
        _level: &Arc<InputDeviceLevel>,
    ) {
    }

    fn tick(self: &Arc<Self>, now_utc_ms: i64) {
        let settings = crate::settings::get_settings(&self.app);
        let policy = policy_from_settings(&settings);
        let mic = self.input.mic_signal();
        let output = self.input.output_signal();
        let sona_holds = self.self_lease.is_held();
        // The device may still be reporting a stream Sona already closed. Read
        // once here so the decision table and the retraction rules cannot
        // disagree about it inside one tick.
        let sona_mic_cooling = self.self_lease.released_within(SELF_MIC_COOLDOWN);

        // The calendar query only runs when the sub-toggle is on, so an operator
        // who never enabled it pays nothing and is never prompted for access.
        let calendar = if policy.enabled && policy.calendar_enabled {
            calendar::calendar_signal(
                self.calendar
                    .next_event(now_utc_ms, calendar::lookahead_ms()),
                now_utc_ms,
            )
        } else {
            CalendarSignal::Absent
        };

        let tracked = self.lock().tracked.clone();
        // With the master toggle off there is no calendar query, application
        // enumeration, or store read. Detection-started captures and operator
        // captures that already adopted a call stay alive so their stop triggers
        // still fire; a waiting operator capture parks with the disabled loop.
        let keeps_runtime_active = tracked
            .as_ref()
            .is_some_and(TrackedCapture::keeps_runtime_active);
        let running = if policy.enabled || keeps_runtime_active {
            self.running_apps.running_apps()
        } else {
            Vec::new()
        };
        let allowlist = apps::normalize_allowlist(&settings.detection_meeting_apps);
        let running_allowlisted = running
            .iter()
            .filter(|app| {
                allowlist
                    .iter()
                    .any(|bundle_id| bundle_id == &app.bundle_id)
            })
            .map(|app| app.bundle_id.clone())
            .collect::<Vec<_>>();
        // Which apps the operator is using is remembered per input-device
        // episode, so an app they switched away from mid-meeting keeps
        // explaining the microphone and one they never touched does not.
        let app_signal = {
            let mut state = self.lock();
            if mic == MicSignal::Active {
                if let Some(bundle_id) = apps::frontmost_allowlisted(&running, &allowlist) {
                    state.apps_used.insert(bundle_id.to_string());
                }
            }
            apps::app_signal(&running, &allowlist, &state.apps_used)
        };
        let call = apps::call_signal(&running, &allowlist);
        // Whether a call is happening, decided once per tick: it decides
        // whether the tick is inert, and the decision table asks the same
        // function from the same inputs. The once-per-call claim and a pending
        // call prompt live inside the call's *evidence* instead, which does
        // not need the app in front: the operator switching away mid-call must
        // not re-arm the claim or retract the prompt.
        let call_live = machine::call_is_live(&call, &app_signal, mic, output);
        let call_evidence = machine::call_evidence(&call, mic, output);
        self.lock().rearm_call_claims(call_evidence);

        // Nothing to decide: skip the store reads entirely — the capture
        // snapshot, the suggestion list, and the decision table. This is the
        // overwhelmingly common path, and it is the SQLCipher reads that cost
        // something, not the in-process application list.
        //
        // The application list is enumerated *before* this return on purpose.
        // `running_meeting_apps` is how the operator checks that a bundle ID
        // they typed is real; reporting an empty list here because detection
        // had nothing to decide would tell an operator with a meeting app open
        // and an idle microphone that their allowlist entry is dead. "I did not
        // look" is not "nothing is running", and this field is load-bearing
        // precisely on the idle path — as is
        // `input_device_reporting_suspect`, which is derived from it and was
        // therefore never able to fire here either.
        self.retract_stale_prompts(
            now_utc_ms,
            &running,
            mic,
            call_evidence,
            sona_holds || sona_mic_cooling,
        );

        let inert = mic == MicSignal::Idle && calendar == CalendarSignal::Absent && !call_live;
        if inert && tracked.is_none() {
            self.publish_status(&settings, mic, sona_holds, None, None, running_allowlisted);
            return;
        }

        // The tick thread's one entry into the async runtime, and detection's
        // only one. `active_capture` is `async`, so a caller that already runs
        // on the runtime — a notification click, a command — awaits it instead
        // of nesting a `block_on` and hanging the very path the operator asked
        // for. This thread is not a runtime thread, so blocking here is a plain
        // wait. One read serves both the stop evaluation and the decision
        // table, so the two cannot disagree about whether a capture is live.
        let active = tauri::async_runtime::block_on(self.active_capture());

        if let Some(tracked) = tracked {
            self.evaluate_running_capture(
                &tracked,
                active.as_ref(),
                CaptureTick {
                    running: &running,
                    mic,
                    output,
                    sona_holds,
                    call: &call,
                    call_live,
                },
                now_utc_ms,
            );
        }

        // The activation observer fires only on app switches and its offer
        // lives two minutes, so a call joined after the switch is invisible to
        // it. Reading the focused window now, on the ticks where the title can
        // decide anything — a browser in front, the microphone live, and no
        // capture or dictation already holding the decision — is what sees it.
        // Nothing here holds the state lock: the read calls back into the
        // suggestion store on this thread.
        let browser_title = match &app_signal {
            machine::AppSignal::Browser { bundle_id, .. }
                if mic == MicSignal::Active && !sona_holds && active.is_none() =>
            {
                let read = self.browser_titles.refresh_frontmost(bundle_id);
                apps::browser_title_evidence(
                    read,
                    &self
                        .meetings
                        .suggestions_list(crate::meeting::clock::host_monotonic_now_ns()),
                    bundle_id,
                )
            }
            _ => machine::BrowserTitleEvidence::NoMatch,
        };
        let calendar_event = match &calendar {
            CalendarSignal::Upcoming { event, .. } | CalendarSignal::Started { event }
                if event.attendee_count >= machine::ATTENDEE_FLOOR =>
            {
                Some(event.clone())
            }
            _ => None,
        };
        let briefing_event = calendar_event.clone();
        let briefing = Vec::new();
        let standing_series_consent = match &calendar {
            CalendarSignal::Started { event } if mic == MicSignal::Active => {
                tauri::async_runtime::block_on(self.meetings.live_series_consent(&event.series_key))
                    .ok()
                    .flatten()
                    .is_some()
            }
            _ => false,
        };
        let standing_app_consent = match &call {
            machine::CallSignal::Running { bundle_id, .. } => {
                apps::grants_auto_record(&settings, bundle_id)
            }
            machine::CallSignal::Absent => false,
        };

        let inputs = DetectionInputs {
            now_utc_ms,
            calendar,
            app: app_signal,
            call,
            mic,
            output,
            browser_title,
            standing_series_consent,
            standing_app_consent,
            recent_capture: self.lock().recent.clone(),
            self_holds_input_device: sona_holds,
            self_mic_just_closed: sona_mic_cooling,
            capture_active: active.is_some(),
        };

        let outcome = evaluate(&inputs, &policy);
        let (suppress_reason, countdown) = self.apply(outcome, calendar_event, briefing);
        self.publish_status(
            &settings,
            mic,
            sona_holds,
            suppress_reason,
            countdown,
            running_allowlisted,
        );
        if let Some(event) = briefing_event {
            self.schedule_calendar_briefing(event, now_utc_ms);
        }
    }

    /// Turns one outcome into the action it names. Returns what the status event
    /// should report.
    fn apply(
        self: &Arc<Self>,
        outcome: DetectionOutcome,
        calendar_event: Option<CalendarEventSummary>,
        briefing: Vec<PersonBriefingRow>,
    ) -> (Option<SuppressReason>, Option<DetectionCountdown>) {
        match outcome {
            DetectionOutcome::Suppress(reason) => (Some(reason), None),
            DetectionOutcome::Countdown {
                event,
                seconds_to_start,
            } => (
                None,
                Some(DetectionCountdown {
                    event,
                    seconds_to_start,
                    briefing,
                }),
            ),
            DetectionOutcome::AutoStart {
                event_key,
                event_title,
            } => {
                if self.claim_event(&event_key) {
                    let runtime = Arc::clone(self);
                    tauri::async_runtime::spawn(async move {
                        let Some(event) = calendar_event else {
                            return;
                        };
                        let Ok(Some(standing)) = runtime
                            .meetings
                            .live_series_consent(&event.series_key)
                            .await
                        else {
                            return;
                        };
                        let context = crate::meeting::session::MeetingDetectionStartContext {
                            prompt_id: format!("auto:{event_key}"),
                            title: event_title,
                            trigger_bundle_id: None,
                            event_end_utc_ms: Some(event.end_utc_ms),
                            calendar_event: Some(event),
                        };
                        match runtime
                            .meetings
                            .start_from_standing_series(&context, standing)
                            .await
                        {
                            Ok(result)
                                if result.snapshot.phase == MeetingPhase::CapturingRecording =>
                            {
                                runtime.track_started(&context, &result.snapshot);
                                runtime
                                    .meetings
                                    .record_auto_record_started(
                                        &event_key,
                                        result.snapshot.session_id,
                                    )
                                    .await;
                            }
                            Ok(_) => {}
                            Err(error) => {
                                log::warn!("Standing-series recording could not start: {error:?}");
                            }
                        }
                    });
                }
                (None, None)
            }
            DetectionOutcome::AutoStartCall {
                bundle_id,
                app_name,
            } => {
                if self.claim_call(&bundle_id) {
                    self.start_call_recording(bundle_id, app_name);
                }
                (None, None)
            }
            DetectionOutcome::Prompt(prompt) => {
                if self.claim_prompt(&prompt) {
                    self.raise(prompt, calendar_event);
                }
                (None, None)
            }
            DetectionOutcome::CrossLink { session_id } => {
                log::info!(
                    "Meeting detection attached new activity to session {session_id} \
                     inside the cross-link window"
                );
                (None, None)
            }
        }
    }

    /// One prompt per calendar event, one per app per input-device episode, and
    /// one per call. A 15s tick without this becomes a notification storm.
    ///
    /// A call prompt claims the call boundary rather than the microphone
    /// episode: a call detected on the output signal alone never raises the
    /// input device, so the episode that clears `prompted_apps` never starts.
    fn claim_prompt(&self, prompt: &PromptKind) -> bool {
        match prompt {
            PromptKind::CalendarEvent { event_key, .. } => self.claim_event(event_key),
            PromptKind::AppCall { bundle_id, .. } => self.claim_call(bundle_id),
            PromptKind::AppMeeting { bundle_id, .. }
            | PromptKind::AppHuddle { bundle_id, .. }
            | PromptKind::BrowserCall { bundle_id, .. } => {
                self.lock().prompted_apps.insert(bundle_id.clone())
            }
            PromptKind::UnknownMicSource => {
                self.lock().prompted_apps.insert("__unknown__".to_string())
            }
        }
    }

    fn claim_event(&self, event_key: &str) -> bool {
        self.lock().prompted_events.insert(event_key.to_string())
    }

    /// One decision per call, not per tick. Released by the tick that observes
    /// the call is no longer live.
    fn claim_call(&self, bundle_id: &str) -> bool {
        self.lock().claim_call(bundle_id)
    }

    /// The standing-app half of the auto-start path. The grant is re-read from
    /// settings here, on the async side of the claim, because the tick that
    /// decided may be up to a tick old by the time this runs.
    fn start_call_recording(self: &Arc<Self>, bundle_id: String, app_name: String) {
        let runtime = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let granted =
                apps::grants_auto_record(&crate::settings::get_settings(&runtime.app), &bundle_id);
            if !granted {
                return;
            }
            let context = crate::meeting::session::MeetingDetectionStartContext {
                prompt_id: format!("auto-call:{bundle_id}"),
                title: machine::call_meeting_title(&app_name, chrono::Local::now()),
                trigger_bundle_id: Some(bundle_id.clone()),
                event_end_utc_ms: None,
                calendar_event: None,
            };
            match runtime
                .meetings
                .start_from_standing_app(&context, bundle_id.clone())
                .await
            {
                Ok(result) if result.snapshot.phase == MeetingPhase::CapturingRecording => {
                    runtime.track_started(&context, &result.snapshot);
                    runtime.present_recording_card(&result.snapshot, bundle_id, app_name);
                    runtime
                        .meetings
                        .record_auto_record_started(&context.prompt_id, result.snapshot.session_id)
                        .await;
                }
                Ok(_) => {}
                Err(error) => {
                    log::warn!("Standing-app call recording could not start: {error:?}");
                }
            }
        });
    }

    fn schedule_calendar_briefing(self: &Arc<Self>, event: CalendarEventSummary, now_utc_ms: i64) {
        if !self.lock().briefing_events.insert(event.event_key.clone()) {
            return;
        }
        let runtime = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let event_key = event.event_key.clone();
            let briefing = runtime
                .meetings
                .calendar_briefing(event.clone(), now_utc_ms)
                .await;
            runtime.publish_calendar_briefing(&event_key, briefing);
            runtime.present_prep(event, now_utc_ms).await;
        });
    }

    fn publish_calendar_briefing(&self, event_key: &str, briefing: Vec<PersonBriefingRow>) {
        let status = {
            let mut state = self.lock();
            let Some(status) = state.last_status.as_mut() else {
                return;
            };
            let Some(countdown) = status.countdown.as_mut() else {
                return;
            };
            if countdown.event.event_key != event_key || countdown.briefing == briefing {
                return;
            }
            countdown.briefing = briefing;
            status.clone()
        };
        let _ = status.emit(&self.app);
    }

    async fn prep_card(
        &self,
        event: &CalendarEventSummary,
        now_utc_ms: i64,
    ) -> Option<MeetingPrepCard> {
        if event.series_key.trim().is_empty() || now_utc_ms >= event.start_utc_ms {
            return None;
        }
        let store = self.meetings.store().await.ok()?;
        let previous = store
            .previous_series_brief(&event.series_key, event.start_utc_ms)
            .ok()
            .flatten()?;
        let loops = store.meeting_loops(previous.session_id).ok()?;
        let mut mine_open_loop_count = 0_u64;
        let mut waiting_on_count = 0_u64;
        let mut mine_open_loops = Vec::with_capacity(2);
        for row in &loops.rows {
            if row.is_open() && row.is_mine() {
                mine_open_loop_count += 1;
                if mine_open_loops.len() < 2 {
                    mine_open_loops.push(row.text.clone());
                }
            } else if row.is_open() && row.is_waiting_on() {
                waiting_on_count += 1;
            }
        }

        let emails = event
            .attendees
            .iter()
            .filter(|attendee| !attendee.is_self)
            .filter_map(|attendee| attendee.email.clone())
            .collect::<Vec<_>>();
        let ids_by_email = store.person_ids_for_calendar_emails(&emails).ok()?;
        let person_ids = ids_by_email
            .values()
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let context = store.person_context(&person_ids).ok()?;
        let organizations = store.organizations_for_person_ids(&person_ids).ok()?;
        let context_by_id = context
            .rows
            .into_iter()
            .map(|row| (row.person_id, (row.display_name, row.meetings_count)))
            .collect::<HashMap<_, _>>();
        let participants = event
            .attendees
            .iter()
            .filter(|attendee| !attendee.is_self)
            .filter_map(|attendee| {
                let fallback_name = attendee.name.trim();
                if fallback_name.is_empty() {
                    return None;
                }
                let person_id = attendee
                    .email
                    .as_deref()
                    .and_then(|email| ids_by_email.get(&email.trim().to_lowercase()));
                let (name, meetings_count) = person_id
                    .and_then(|person_id| context_by_id.get(person_id))
                    .map(|(name, meetings_count)| (name.clone(), *meetings_count))
                    .unwrap_or_else(|| (fallback_name.to_string(), 0));
                Some(MeetingPrepParticipant {
                    name,
                    meetings_count,
                    organization: person_id
                        .and_then(|person_id| organizations.get(person_id))
                        .cloned(),
                })
            })
            .collect();
        let can_record_when_starts = store
            .live_series_consent(&event.series_key)
            .ok()
            .flatten()
            .is_some();
        Some(MeetingPrepCard {
            event_key: event.event_key.clone(),
            series_key: event.series_key.clone(),
            title: event.title.clone(),
            start_utc_ms: event.start_utc_ms,
            last_meeting_id: previous.session_id,
            headline: previous.headline,
            mine_open_loops,
            mine_open_loop_count,
            waiting_on_count,
            participants,
            can_record_when_starts,
        })
    }

    async fn present_prep(self: &Arc<Self>, event: CalendarEventSummary, now_utc_ms: i64) {
        let Some(card) = self.prep_card(&event, now_utc_ms).await else {
            return;
        };
        if self.active_capture().await.is_some() {
            return;
        }
        let ritual_id = format!("prep:{}", card.event_key);
        if !self
            .meetings
            .record_ritual_activity(
                WorkflowEventKind::MeetingPrepPresented,
                &ritual_id,
                card.last_meeting_id,
                &card.event_key,
            )
            .await
        {
            return;
        }
        if self.active_capture().await.is_some() {
            return;
        }
        let minutes = event
            .start_utc_ms
            .saturating_sub(now_utc_ms)
            .saturating_add(59_999)
            / 60_000;
        let minutes = minutes.max(1);
        let pending = PendingPanel::Ritual(PendingRitual {
            notification_title: format!("{} — in {minutes} minutes", event.title),
            ritual: MeetingRitual::Prep(card),
            idle_generation: 0,
        });
        let mut state = self.lock();
        let panel_available = state.tracked.is_none();
        let commands = state.panel.raise(ritual_id, pending, 2, panel_available);
        self.apply_panel_commands(&mut state, commands);
    }

    async fn wrap_card(&self, session_id: MeetingSessionId) -> Option<MeetingWrapCard> {
        let store = self.meetings.store().await.ok()?;
        let review = store.review_snapshot(session_id).ok()?;
        if review.session.phase != MeetingPhase::ReviewReady {
            return None;
        }
        let headline = review
            .artifacts
            .iter()
            .filter(|artifact| artifact.state == MeetingArtifactState::Current)
            .find_map(|artifact| artifact.content.as_ref())
            .and_then(|content| content.headline())
            .map(str::trim)
            .filter(|headline| !headline.is_empty())?
            .to_string();
        let unresolved_speaker_count = match store.unresolved_active_voice_speaker_ids(session_id) {
            Ok(speaker_ids) => u64::try_from(speaker_ids.len()).ok(),
            Err(error) => {
                log::warn!(
                    "Meeting wrap speaker count unavailable for session {session_id:?}: {error:?}"
                );
                None
            }
        };
        let loops = store.meeting_loops(session_id).ok()?;
        let mut follow_up_count = 0_u64;
        let mut waiting_on_count = 0_u64;
        let mut waiting_on_names = Vec::new();
        let mut seen_names = HashSet::new();
        for row in &loops.rows {
            if row.is_open() && row.is_mine() {
                follow_up_count += 1;
            } else if row.is_open() && row.is_waiting_on() {
                waiting_on_count += 1;
                let name = row
                    .owner_display_name
                    .as_deref()
                    .or(row.owner_text.as_deref())
                    .map(str::trim)
                    .filter(|name| !name.is_empty());
                if let Some(name) = name {
                    let key = name.to_lowercase();
                    if seen_names.insert(key) {
                        waiting_on_names.push(name.to_string());
                    }
                }
            }
        }
        Some(MeetingWrapCard {
            session_id,
            title: review.session.title,
            headline,
            unresolved_speaker_count,
            follow_up_count,
            waiting_on_count,
            waiting_on_names,
        })
    }

    pub(crate) async fn present_wrap(self: &Arc<Self>, session_id: MeetingSessionId) {
        let Some(card) = self.wrap_card(session_id).await else {
            return;
        };
        if self.active_capture().await.is_some() {
            return;
        }
        let ritual_id = format!("wrap:{}", session_id.uuid());
        if !self
            .meetings
            .record_ritual_activity(
                WorkflowEventKind::MeetingWrapPresented,
                &ritual_id,
                session_id,
                &session_id.uuid().to_string(),
            )
            .await
        {
            return;
        }
        if self.active_capture().await.is_some() {
            return;
        }
        let pending = PendingPanel::Ritual(PendingRitual {
            notification_title: format!("{} — saved", card.title),
            ritual: MeetingRitual::Wrap(card),
            idle_generation: 0,
        });
        let mut state = self.lock();
        let panel_available = state.tracked.is_none();
        let commands = state
            .panel
            .raise(ritual_id.clone(), pending, 1, panel_available);
        self.apply_panel_commands(&mut state, commands);
        drop(state);
        self.schedule_wrap_timeout(ritual_id, 0);
    }

    fn schedule_wrap_timeout(self: &Arc<Self>, ritual_id: String, generation: u64) {
        let runtime = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(WRAP_IDLE_TIMEOUT).await;
            let should_finish = {
                let state = runtime.lock();
                matches!(
                    state.panel.get(&ritual_id),
                    Some(PendingPanel::Ritual(PendingRitual {
                        ritual: MeetingRitual::Wrap(_),
                        idle_generation,
                        ..
                    })) if *idle_generation == generation
                )
            };
            if should_finish {
                runtime.finish_ritual(&ritual_id);
            }
        });
    }

    fn retract_stale_prompts(
        self: &Arc<Self>,
        now_utc_ms: i64,
        running: &[RunningApp],
        mic: MicSignal,
        call_evidence: bool,
        sona_mic: bool,
    ) {
        let pending_kinds = self
            .lock()
            .panel
            .iter()
            .filter_map(|(prompt_id, pending)| match pending {
                PendingPanel::Prompt(pending) => Some(format!("{prompt_id}={:?}", pending.prompt)),
                PendingPanel::Ritual(_) => None,
            })
            .collect::<Vec<_>>();
        // A prompt nothing withdraws is invisible in the log otherwise: the
        // panel window logs nothing, and the operator's only other evidence is
        // a window that will not go away. This names what is pending and the
        // three facts that decide whether it should be.
        if !pending_kinds.is_empty() {
            log::debug!(
                "Meeting detection holds {} pending prompt(s) [{}] with mic={mic:?} \
                 sona_mic={sona_mic} call_evidence={call_evidence}",
                pending_kinds.len(),
                pending_kinds.join(", "),
            );
        }
        let retract = self
            .lock()
            .panel
            .iter()
            .filter_map(|(prompt_id, pending)| match pending {
                PendingPanel::Prompt(pending) => {
                    prompt_retraction(pending, now_utc_ms, running, mic, call_evidence, sona_mic)
                        .map(|reason| (prompt_id.to_string(), Some(reason)))
                }
                PendingPanel::Ritual(pending) if ritual_is_stale(&pending.ritual, now_utc_ms) => {
                    Some((prompt_id.to_string(), None))
                }
                PendingPanel::Ritual(_) => None,
            })
            .collect::<Vec<_>>();
        for (prompt_id, reason) in retract {
            match reason {
                Some(reason) => {
                    self.finish_prompt(&prompt_id, reason);
                }
                None => {
                    self.finish_ritual(&prompt_id);
                }
            }
        }
    }

    pub fn calendar_event_for_start(&self, event_key: &str) -> Option<CalendarEventSummary> {
        let state = self.lock();
        let event = state
            .last_status
            .as_ref()
            .and_then(|status| status.countdown.as_ref())
            .map(|countdown| &countdown.event)
            .into_iter()
            .chain(state.panel.iter().filter_map(|(_, pending)| match pending {
                PendingPanel::Prompt(pending) => pending.calendar_event.as_ref(),
                PendingPanel::Ritual(_) => None,
            }))
            .find(|event| event.event_key == event_key)
            .cloned();
        event
    }

    fn raise(self: &Arc<Self>, prompt: PromptKind, calendar_event: Option<CalendarEventSummary>) {
        let prompt_id = Uuid::new_v4().to_string();
        // The panel window logs nothing and sets NSWindowSharingType::None, so
        // it cannot be screenshotted either. Without this line the only trace a
        // prompt ever existed is the window itself.
        log::info!("Meeting detection raised prompt {prompt_id}: {prompt:?}");
        let show_introduction =
            tauri::async_runtime::block_on(self.meetings.consent_panel_introduction_needed());
        let announce_in_chat = calendar_event.as_ref().is_some_and(|event| {
            tauri::async_runtime::block_on(
                self.meetings.series_announces_in_chat(&event.series_key),
            )
        });
        let priority = prompt_priority(&prompt);
        let pending = PendingPanel::Prompt(PendingPrompt {
            prompt,
            calendar_event,
            show_introduction,
            announce_in_chat,
        });
        let mut state = self.lock();
        let panel_available = state.tracked.is_none();
        let commands = state
            .panel
            .raise(prompt_id, pending, priority, panel_available);
        self.apply_panel_commands(&mut state, commands);
    }

    fn apply_panel_commands(
        self: &Arc<Self>,
        state: &mut RuntimeState,
        commands: Vec<PanelCommand<PendingPanel>>,
    ) {
        let mut commands = VecDeque::from(commands);
        while let Some(command) = commands.pop_front() {
            match command {
                PanelCommand::ShowPanel => {
                    let _ = self.prompts.show_panel(ConsentPanelLayout::Recording {
                        // The refused-disclosure row is not known yet: the
                        // announcement is attempted from the panel, which
                        // asks for the height when it draws the note.
                        disclosure_note: false,
                    });
                }
                PanelCommand::HidePanel => self.prompts.hide_panel(),
                PanelCommand::WithdrawPrompt { prompt_id } => {
                    self.prompts.withdraw(&prompt_id);
                }
                PanelCommand::PresentPanel { prompt_id, prompt } => {
                    self.prompts.withdraw(&prompt_id);
                    let layout = match &prompt {
                        PendingPanel::Prompt(prompt) => prompt_panel_layout(
                            prompt,
                            state
                                .last_status
                                .as_ref()
                                .and_then(|status| status.countdown.as_ref()),
                        ),
                        PendingPanel::Ritual(ritual) => ritual_panel_layout(&ritual.ritual),
                    };
                    if self.prompts.show_panel(layout) {
                        self.emit_panel(&prompt_id, &prompt, DetectionPromptDelivery::Panel);
                        self.schedule_panel_ack_timeout(prompt_id);
                    } else {
                        commands.extend(state.panel.fallback_if_unacknowledged(&prompt_id));
                    }
                }
                PanelCommand::PresentFallback { prompt_id, prompt } => {
                    self.prompts.withdraw(&prompt_id);
                    let delivery = match &prompt {
                        PendingPanel::Prompt(prompt) => {
                            self.prompts.present_fallback(&prompt_id, &prompt.prompt)
                        }
                        PendingPanel::Ritual(ritual) => self
                            .prompts
                            .present_ritual_fallback(&prompt_id, &ritual.notification_title),
                    };
                    self.emit_panel(&prompt_id, &prompt, delivery);
                }
                PanelCommand::Acknowledged {
                    prompt_id: _,
                    prompt,
                } => {
                    if let PendingPanel::Prompt(prompt) = prompt {
                        if prompt.show_introduction {
                            let meetings = Arc::clone(&self.meetings);
                            tauri::async_runtime::spawn(async move {
                                meetings.mark_consent_panel_introduction_shown().await;
                            });
                        }
                    }
                }
            }
        }
    }

    fn emit_panel(
        &self,
        prompt_id: &str,
        pending: &PendingPanel,
        delivery: DetectionPromptDelivery,
    ) {
        match pending {
            PendingPanel::Prompt(prompt) => self.emit_prompt(prompt_id, prompt, delivery),
            PendingPanel::Ritual(ritual) => self.emit_ritual(prompt_id, ritual, delivery),
        }
    }

    fn emit_prompt(
        &self,
        prompt_id: &str,
        pending: &PendingPrompt,
        delivery: DetectionPromptDelivery,
    ) {
        let _ = DetectionPromptEvent {
            event_schema_version: DETECTION_EVENT_SCHEMA_VERSION,
            prompt_id: prompt_id.to_string(),
            notification_title: pending.prompt.notification_title(),
            prompt: pending.prompt.clone(),
            delivery,
            show_introduction: pending.show_introduction,
            announce_in_chat: pending.announce_in_chat,
        }
        .emit(&self.app);
    }

    fn emit_ritual(
        &self,
        ritual_id: &str,
        pending: &PendingRitual,
        delivery: DetectionPromptDelivery,
    ) {
        let _ = MeetingRitualEvent {
            event_schema_version: DETECTION_EVENT_SCHEMA_VERSION,
            ritual_id: ritual_id.to_string(),
            ritual: pending.ritual.clone(),
            notification_title: pending.notification_title.clone(),
            delivery,
        }
        .emit(&self.app);
    }

    fn schedule_panel_ack_timeout(self: &Arc<Self>, prompt_id: String) {
        let runtime = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(PANEL_ACK_WINDOW).await;
            runtime.panel_ack_timed_out(&prompt_id);
        });
    }

    fn panel_ack_timed_out(self: &Arc<Self>, prompt_id: &str) {
        let mut state = self.lock();
        let commands = state.panel.fallback_if_unacknowledged(prompt_id);
        self.apply_panel_commands(&mut state, commands);
    }

    pub fn acknowledge_panel(self: &Arc<Self>, prompt_id: &str) {
        let mut state = self.lock();
        let commands = state.panel.acknowledge(prompt_id);
        self.apply_panel_commands(&mut state, commands);
    }

    pub fn take_for_panel_start(
        self: &Arc<Self>,
        prompt_id: &str,
    ) -> Option<crate::meeting::session::MeetingDetectionStartContext> {
        let pending = self.finish_prompt(prompt_id, DetectionPromptRetractionReason::Resolved)?;
        Some(crate::meeting::session::MeetingDetectionStartContext {
            prompt_id: prompt_id.to_string(),
            title: pending.prompt.proposed_meeting_title(chrono::Local::now()),
            trigger_bundle_id: pending.prompt.bundle_id().map(str::to_string),
            event_end_utc_ms: pending.linked_event_end(),
            calendar_event: pending.calendar_event,
        })
    }

    /// Resolves an answered native or in-app fallback prompt. Accepting keeps
    /// the historical notification contract: it opens preflight and never
    /// starts capture. The consent panel uses the composed meeting command.
    pub fn respond(self: &Arc<Self>, prompt_id: &str, accepted: bool) {
        let Some(pending) =
            self.finish_prompt(prompt_id, DetectionPromptRetractionReason::Resolved)
        else {
            log::info!(
                "Meeting detection received a response receipt for already-drained prompt \
                 {prompt_id}; no action was taken"
            );
            return;
        };
        if !accepted {
            let meetings = Arc::clone(&self.meetings);
            let prompt_id = prompt_id.to_string();
            tauri::async_runtime::spawn(async move {
                meetings.record_prompt_ignored(prompt_id).await;
            });
            return;
        }
        let runtime = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            runtime
                .open_capture(
                    &pending.prompt,
                    pending.linked_event_end(),
                    utc_now_ms(),
                    pending.calendar_event,
                )
                .await;
        });
    }

    pub async fn respond_ritual(
        self: &Arc<Self>,
        ritual_id: &str,
        action: MeetingRitualAction,
    ) -> bool {
        let pending = {
            let state = self.lock();
            match state.panel.get(ritual_id) {
                Some(PendingPanel::Ritual(pending)) => pending.clone(),
                _ => return false,
            }
        };
        match (&pending.ritual, action) {
            (MeetingRitual::Prep(card), MeetingRitualAction::PrepRecordWhenStarts) => {
                if !card.can_record_when_starts
                    || self
                        .meetings
                        .live_series_consent(&card.series_key)
                        .await
                        .ok()
                        .flatten()
                        .is_none()
                {
                    return false;
                }
                if !self
                    .meetings
                    .record_ritual_activity(
                        WorkflowEventKind::MeetingPrepRecordArmed,
                        ritual_id,
                        card.last_meeting_id,
                        ritual_id,
                    )
                    .await
                {
                    return false;
                }
                self.finish_ritual(ritual_id);
                true
            }
            (MeetingRitual::Prep(card), MeetingRitualAction::PrepOpenBrief) => {
                if !self
                    .meetings
                    .record_ritual_activity(
                        WorkflowEventKind::MeetingPrepBriefOpened,
                        ritual_id,
                        card.last_meeting_id,
                        ritual_id,
                    )
                    .await
                {
                    return false;
                }
                let opened = crate::dispatch_deep_link(
                    &self.app,
                    &crate::query::meeting_link(card.last_meeting_id),
                );
                self.finish_ritual(ritual_id);
                opened
            }
            (MeetingRitual::Prep(card), MeetingRitualAction::PrepDismiss) => {
                if !self
                    .meetings
                    .record_ritual_activity(
                        WorkflowEventKind::MeetingPrepDismissed,
                        ritual_id,
                        card.last_meeting_id,
                        ritual_id,
                    )
                    .await
                {
                    return false;
                }
                self.finish_ritual(ritual_id);
                true
            }
            (MeetingRitual::Wrap(card), MeetingRitualAction::WrapOpenNotes) => {
                if !self
                    .meetings
                    .record_ritual_activity(
                        WorkflowEventKind::MeetingWrapNotesOpened,
                        ritual_id,
                        card.session_id,
                        ritual_id,
                    )
                    .await
                {
                    return false;
                }
                let opened = crate::dispatch_deep_link(
                    &self.app,
                    &crate::query::meeting_link(card.session_id),
                );
                self.finish_ritual(ritual_id);
                opened
            }
            (MeetingRitual::Wrap(card), MeetingRitualAction::WrapFollowUpCopied) => {
                if !self
                    .meetings
                    .record_ritual_activity(
                        WorkflowEventKind::MeetingWrapFollowUpCopied,
                        ritual_id,
                        card.session_id,
                        ritual_id,
                    )
                    .await
                {
                    return false;
                }
                let generation = {
                    let mut state = self.lock();
                    match state.panel.get_mut(ritual_id) {
                        Some(PendingPanel::Ritual(pending)) => {
                            pending.idle_generation += 1;
                            pending.idle_generation
                        }
                        _ => return false,
                    }
                };
                self.schedule_wrap_timeout(ritual_id.to_string(), generation);
                true
            }
            (MeetingRitual::Wrap(card), MeetingRitualAction::WrapDone) => {
                if !self
                    .meetings
                    .record_ritual_activity(
                        WorkflowEventKind::MeetingWrapDone,
                        ritual_id,
                        card.session_id,
                        ritual_id,
                    )
                    .await
                {
                    return false;
                }
                self.finish_ritual(ritual_id);
                true
            }
            (
                MeetingRitual::Recording(card),
                MeetingRitualAction::RecordingStop | MeetingRitualAction::RecordingForgetApp,
            ) => self.respond_recording(card.clone(), action).await,
            _ => false,
        }
    }

    /// Both actions on the recording card end the capture; the second also
    /// takes back the standing grant that started it. Revoking first means an
    /// operator whose stop fails still does not get auto-recorded again.
    async fn respond_recording(
        self: &Arc<Self>,
        card: MeetingRecordingCard,
        action: MeetingRitualAction,
    ) -> bool {
        match action {
            MeetingRitualAction::RecordingStop => {}
            MeetingRitualAction::RecordingForgetApp => {
                // Nothing in this function's shape can report a store failure; the settings seam logs it.
                let _ = crate::settings::update_settings(&self.app, |settings| {
                    apps::revoke_auto_record(settings, &card.bundle_id);
                });
                self.wakeup.wake();
            }
            _ => return false,
        }
        // The card names one capture. Whatever is active now may be a later
        // one the operator started from the tray after this card's capture
        // ended by a route that never reached `track_ended`; the click must
        // not stop that.
        let Some(active) = self
            .active_capture()
            .await
            .filter(|active| active.session_id == card.session_id)
        else {
            // Already stopped by some other route; the card has nothing left to
            // end, and `track_ended` retracts it.
            self.track_ended(card.session_id);
            return true;
        };
        let stopped = self
            .meetings
            .stop(
                crate::meeting::session::MeetingMutationRequest {
                    operation_id: MeetingOperationId::new(),
                    session_id: active.session_id,
                    expected_revision: active.revision,
                },
                crate::meeting::session::MeetingStopCause::RecordingCard,
            )
            .await;
        match stopped {
            Ok(_) => {
                // No auto-stop event: a click on this card is a manual stop,
                // and `StopTrigger` deliberately has no variant for one.
                self.track_ended(active.session_id);
                true
            }
            Err(error) => {
                log::warn!("The recording card could not stop the capture: {error:?}");
                false
            }
        }
    }

    fn finish_prompt(
        self: &Arc<Self>,
        prompt_id: &str,
        reason: DetectionPromptRetractionReason,
    ) -> Option<PendingPrompt> {
        let mut state = self.lock();
        let finish = state.panel.finish(prompt_id);
        let PendingPanel::Prompt(pending) = finish.removed? else {
            return None;
        };
        let _ = DetectionPromptRetractedEvent {
            event_schema_version: DETECTION_EVENT_SCHEMA_VERSION,
            prompt_id: prompt_id.to_string(),
            reason,
        }
        .emit(&self.app);
        self.apply_panel_commands(&mut state, finish.commands);
        Some(pending)
    }

    fn finish_ritual(self: &Arc<Self>, ritual_id: &str) -> Option<PendingRitual> {
        let mut state = self.lock();
        let finish = state.panel.finish(ritual_id);
        let PendingPanel::Ritual(pending) = finish.removed? else {
            return None;
        };
        let _ = MeetingRitualRetractedEvent {
            event_schema_version: DETECTION_EVENT_SCHEMA_VERSION,
            ritual_id: ritual_id.to_string(),
        }
        .emit(&self.app);
        self.apply_panel_commands(&mut state, finish.commands);
        Some(pending)
    }

    pub fn track_started(
        self: &Arc<Self>,
        context: &crate::meeting::session::MeetingDetectionStartContext,
        snapshot: &MeetingSessionSnapshot,
    ) {
        let trigger_bundle_id = context.trigger_bundle_id.clone();
        let mut state = self.lock();
        state.begin_tracked(
            TrackedCapture::detection(snapshot, trigger_bundle_id, context.event_end_utc_ms),
            RecentCapture {
                session_id: snapshot.session_id.uuid().to_string(),
                started_utc_ms: utc_now_ms(),
            },
        );
        // A capture that ended by a route `track_ended` never heard about is
        // still tracked here, and its recording card is still in the slot.
        // `begin_capture` discards every ritual, that card included, and the
        // retraction below is what takes it off the panel.
        let capture = state.panel.begin_capture(PendingPanel::is_prompt);
        for (ritual_id, pending) in capture.discarded {
            if matches!(pending, PendingPanel::Ritual(_)) {
                let _ = MeetingRitualRetractedEvent {
                    event_schema_version: DETECTION_EVENT_SCHEMA_VERSION,
                    ritual_id,
                }
                .emit(&self.app);
            }
        }
        self.apply_panel_commands(&mut state, capture.commands);
    }

    pub fn track_started_by_operator(self: &Arc<Self>, snapshot: &MeetingSessionSnapshot) {
        let settings = crate::settings::get_settings(&self.app);
        if !policy_from_settings(&settings).enabled {
            return;
        }
        self.lock().begin_tracked(
            TrackedCapture::operator(snapshot),
            RecentCapture {
                session_id: snapshot.session_id.uuid().to_string(),
                started_utc_ms: utc_now_ms(),
            },
        );
    }

    /// Raises the auto-record card into the panel `begin_capture` just took
    /// for the capture.
    ///
    /// Through `PanelSlot` like every other presentation, so the card gets the
    /// same delivery pipeline: `show_panel`'s result is checked, an
    /// unacknowledged panel times out into a native notification, and a panel
    /// that never showed falls back at once. A recording nobody asked for out
    /// loud is the one presentation that must never go undelivered.
    /// `track_ended` finishes it; a capture that replaces this one discards
    /// it through `begin_capture`.
    fn present_recording_card(
        self: &Arc<Self>,
        snapshot: &MeetingSessionSnapshot,
        bundle_id: String,
        app_name: String,
    ) {
        let card = MeetingRecordingCard {
            session_id: snapshot.session_id,
            bundle_id,
            app_name,
            started_at_utc_ms: snapshot.started_at_utc_ms.unwrap_or_else(utc_now_ms),
        };
        let mut state = self.lock();
        let Some(commands) = state.raise_recording_card(Uuid::new_v4().to_string(), card) else {
            return;
        };
        self.apply_panel_commands(&mut state, commands);
    }

    pub fn track_ended(self: &Arc<Self>, session_id: MeetingSessionId) {
        let mut state = self.lock();
        let Some(tracked) = state.end_tracked(session_id) else {
            return;
        };
        if matches!(tracked.kind, TrackedCaptureKind::Detection { .. }) {
            let commands = state.panel.end_capture();
            self.apply_panel_commands(&mut state, commands);
        }
        let card = state.recording_card_id(session_id);
        drop(state);
        if let Some(ritual_id) = card {
            self.finish_ritual(&ritual_id);
        }
        self.publish_adopted_call_status();
    }

    /// The one place detection touches capture, and it touches only the entry
    /// point the tray already uses: create a preflight, name it, put the consent
    /// screen in front of the operator. `MeetingSessionManager::start` is never
    /// called from here.
    ///
    /// `async` rather than blocking: this is reached from a notification click,
    /// which lands on the async runtime. A `block_on` there would be a nested
    /// runtime entry — a hang on the one path the operator explicitly asked for.
    async fn open_capture(
        &self,
        prompt: &PromptKind,
        event_end_utc_ms: Option<i64>,
        now_utc_ms: i64,
        calendar_event: Option<CalendarEventSummary>,
    ) {
        let title = prompt.proposed_meeting_title(chrono::Local::now());
        let trigger_bundle_id = prompt.bundle_id().map(str::to_string);
        // Re-read rather than trusting the tick's snapshot: an operator can take
        // a while to answer a notification, and opening a meeting for an app that
        // has since quit is a note nobody asked for.
        let trigger_running = trigger_bundle_id.as_deref().is_none_or(|bundle_id| {
            apps::is_app_running(&self.running_apps.running_apps(), bundle_id)
        });
        if !trigger_running {
            log::info!("Meeting detection dropped a prompt whose application had already quit");
            return;
        }
        let snapshot = match self.meetings.create_manual_preflight_from_tray().await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                log::warn!("Meeting detection could not open a preflight: {error:?}");
                return;
            }
        };
        // A failed rename leaves the meeting under the tray's default title,
        // which is a cosmetic loss, not a reason to abandon the capture.
        let snapshot = match self
            .meetings
            .title_set(MeetingTitleSetRequest {
                operation_id: MeetingOperationId::new(),
                session_id: snapshot.session_id,
                expected_revision: snapshot.revision,
                title,
            })
            .await
        {
            Ok(result) => result.snapshot,
            Err(error) => {
                log::info!("Meeting detection kept the default meeting title: {error:?}");
                snapshot
            }
        };
        if let Some(calendar_event) = calendar_event {
            if let Err(error) = self
                .meetings
                .remember_calendar_facts(snapshot.session_id, calendar_event)
                .await
            {
                log::warn!("Meeting calendar facts could not be saved: {error:?}");
            }
        }
        self.lock().begin_tracked(
            TrackedCapture::detection(&snapshot, trigger_bundle_id, event_end_utc_ms),
            RecentCapture {
                session_id: snapshot.session_id.uuid().to_string(),
                started_utc_ms: now_utc_ms,
            },
        );
        crate::tray::set_meeting_tray_snapshot(&self.app, Some(&snapshot));
        crate::show_meeting_destination(
            &self.app,
            MeetingNavigationDestination::Preflight,
            Some(&snapshot),
        );
    }

    /// §5.5, for every tracked capture. Manual stop stays primary: this only
    /// fires on triggers the platform actually reports.
    fn evaluate_running_capture(
        self: &Arc<Self>,
        tracked: &TrackedCapture,
        active: Option<&MeetingSessionSnapshot>,
        tick: CaptureTick<'_>,
        now_utc_ms: i64,
    ) {
        let Some(active) = active else {
            // The capture ended by some other route. Stop tracking it, but keep
            // recent so the cross-link window still applies.
            self.track_ended(tracked.session_id);
            return;
        };
        if active.session_id != tracked.session_id {
            self.track_ended(tracked.session_id);
            return;
        }
        let transition = {
            let mut state = self.lock();
            evaluate_running_capture_transition(&mut state, tracked, tick, now_utc_ms)
        };
        let trigger = match transition {
            CaptureTransition::Continue => return,
            CaptureTransition::Adopted(adopted) => {
                log::info!(
                    "Meeting capture {} adopted the {} call as its stop trigger",
                    tracked.session_id.uuid(),
                    adopted.display_name
                );
                self.publish_adopted_call_status();
                return;
            }
            CaptureTransition::Stop(trigger) => trigger,
        };
        // The session stop logs the committed stop and its cause. This names the
        // trigger before the spawned task carries it out and can fail.
        log::info!("Meeting detection is stopping capture on {trigger:?}");
        let runtime = Arc::clone(self);
        let meetings = Arc::clone(&self.meetings);
        let request = crate::meeting::session::MeetingMutationRequest {
            operation_id: MeetingOperationId::new(),
            session_id: active.session_id,
            expected_revision: active.revision,
        };
        let session_id = active.session_id;
        tauri::async_runtime::spawn(async move {
            match meetings
                .stop(
                    request,
                    crate::meeting::session::MeetingStopCause::Detection(trigger),
                )
                .await
            {
                Ok(_) => {
                    meetings
                        .record_auto_record_stopped(session_id, trigger)
                        .await;
                    runtime.track_ended(session_id);
                }
                Err(error) => {
                    log::warn!("Meeting detection could not stop the capture: {error:?}");
                }
            }
        });
    }

    /// The active capture, or `None`. Reads through the session manager because
    /// the capture lease is its invariant, not detection's.
    ///
    /// `async` on purpose: a sync wrapper around `block_on` is a landmine for
    /// the next caller that happens to be a notification click or a command,
    /// both of which already run on the async runtime. Nesting a runtime entry
    /// there deadlocks. The tick thread does the one permitted blocking read,
    /// at the boundary in `tick` that says so.
    async fn active_capture(&self) -> Option<MeetingSessionSnapshot> {
        self.meetings
            .tray_snapshot()
            .await
            .ok()
            .flatten()
            .filter(|snapshot| {
                matches!(
                    snapshot.phase,
                    MeetingPhase::CapturingRecording
                        | MeetingPhase::CapturingPausing
                        | MeetingPhase::CapturingPaused
                        | MeetingPhase::CapturingResuming
                )
            })
    }

    fn publish_adopted_call_status(&self) {
        let status = {
            let mut state = self.lock();
            let adopted_call = state.adopted_call();
            let Some(status) = state.last_status.as_mut() else {
                return;
            };
            if status.adopted_call == adopted_call {
                return;
            }
            status.adopted_call = adopted_call;
            status.clone()
        };
        let _ = status.emit(&self.app);
    }

    fn publish_status(
        &self,
        settings: &AppSettings,
        mic: MicSignal,
        sona_holds: bool,
        suppress_reason: Option<SuppressReason>,
        countdown: Option<DetectionCountdown>,
        running_meeting_apps: Vec<String>,
    ) {
        let input_device_active = mic == MicSignal::Active;
        // A meeting app is running and nothing claims the input device. That is
        // either nobody talking or the Bluetooth false negative; the operator
        // deserves to see which possibilities are live rather than assume
        // detection is broken.
        //
        // Call apps are excluded because the caveat would be false for them:
        // the output-device signal is the answer to that exact false negative,
        // so a quiet microphone beside an open FaceTime is not a blind spot.
        // Left in, it would fire for anyone who leaves FaceTime running, which
        // is a permanent warning about a gap that is covered.
        let input_device_reporting_suspect = !input_device_active
            && running_meeting_apps
                .iter()
                .any(|bundle_id| !apps::is_call_app_bundle_id(bundle_id));
        let adopted_call = self.lock().adopted_call();
        let status = DetectionStatus {
            event_schema_version: DETECTION_EVENT_SCHEMA_VERSION,
            settings: DetectionSettings::from_app_settings(settings),
            calendar_access: self.calendar.access(),
            notification_access: self.prompts.access(),
            input_device_active,
            sona_holds_input_device: sona_holds,
            suppress_reason,
            countdown,
            adopted_call,
            running_meeting_apps,
            input_device_reporting_suspect,
        };
        {
            let mut state = self.lock();
            if state.last_status.as_ref() == Some(&status) {
                return;
            }
            // An input-device episode ending clears the per-app prompt claims
            // and the apps in use, so the next meeting in the same app prompts
            // again on its own evidence.
            if !input_device_active {
                state.end_input_episode();
            }
            state.last_status = Some(status.clone());
        }
        let _ = status.emit(&self.app);
    }

    /// The status the frontend reads on mount, before any tick has fired.
    pub fn status(&self) -> DetectionStatus {
        let settings = crate::settings::get_settings(&self.app);
        let (last_status, adopted_call) = {
            let state = self.lock();
            (state.last_status.clone(), state.adopted_call())
        };
        let mut status = last_status.unwrap_or_else(|| DetectionStatus {
            event_schema_version: DETECTION_EVENT_SCHEMA_VERSION,
            settings: DetectionSettings::from_app_settings(&settings),
            calendar_access: self.calendar.access(),
            notification_access: self.prompts.access(),
            input_device_active: self.input.mic_signal() == MicSignal::Active,
            sona_holds_input_device: self.self_lease.is_held(),
            suppress_reason: None,
            countdown: None,
            adopted_call: None,
            running_meeting_apps: Vec::new(),
            input_device_reporting_suspect: false,
        });
        status.adopted_call = adopted_call;
        status
    }

    /// Writes the operator's detection policy and returns the status it produces.
    ///
    /// The allowlist is normalized on the way in — lowercased, trimmed,
    /// deduplicated — because a typo in a settings-editable list is otherwise a
    /// silently dead entry.
    pub fn write_settings(&self, requested: DetectionSettings) -> DetectionStatus {
        // Nothing in this function's shape can report a store failure; the settings seam logs it.
        let _ = crate::settings::update_settings(&self.app, |settings| {
            settings.detection_enabled = requested.enabled;
            settings.detection_calendar_enabled = requested.calendar_enabled;
            settings.detection_any_mic_activity = requested.any_mic_activity;
            settings.detection_auto_start_on_open_pane = requested.auto_start_on_open_pane;
            settings.detection_meeting_apps = apps::normalize_allowlist(&requested.meeting_apps);
            // Normalized against the same rule as the allowlist, so a grant and
            // the entry it depends on can never differ by case or whitespace,
            // and narrowed to the apps a standing grant is ever read for: an
            // entry the table cannot reach is consent the picker draws no
            // switch for and the operator cannot withdraw.
            settings.detection_auto_record_apps =
                apps::normalize_auto_record(&requested.auto_record_apps);
        });
        // Wake the loop so the next status reflects the write immediately rather
        // than at the end of the current interval.
        self.wakeup.wake();
        self.status()
    }

    /// Requests calendar full access. Only reached from the settings sub-toggle,
    /// never from the tick — the whole point of the lazy request.
    pub fn request_calendar_access(&self) -> CalendarAccess {
        let access = self.calendar.request_access();
        self.wakeup.wake();
        access
    }

    /// Whether events are readable right now, without waking the loop.
    ///
    /// D28's Upcoming section reads this rather than the whole `DetectionStatus`
    /// because a calendar grant and a detection policy are different questions:
    /// listing the week ahead needs the grant, and needs nothing detection
    /// decides.
    pub fn calendar_access(&self) -> CalendarAccess {
        self.calendar.access()
    }

    /// Every event overlapping the half-open window, oldest first. Empty
    /// whenever the calendar cannot be read, which is the same answer an empty
    /// week gives — the caller distinguishes them with `calendar_access`.
    pub fn calendar_events_between(
        &self,
        start_utc_ms: i64,
        end_utc_ms: i64,
    ) -> Vec<calendar::CalendarOccurrence> {
        self.calendar.events_between(start_utc_ms, end_utc_ms)
    }

    pub async fn request_notification_access(&self) -> NotificationAccess {
        let access = self.prompts.request_access().await;
        self.wakeup.wake();
        access
    }

    /// Allowlisted bundle IDs whose application is running, so the settings UI
    /// can show an operator that an entry they typed is or is not real.
    pub fn running_meeting_apps(&self) -> Vec<String> {
        let settings = crate::settings::get_settings(&self.app);
        let allowlist = apps::normalize_allowlist(&settings.detection_meeting_apps);
        self.running_apps
            .running_apps()
            .into_iter()
            .filter(|app| {
                allowlist
                    .iter()
                    .any(|bundle_id| bundle_id == &app.bundle_id)
            })
            .map(|app| app.bundle_id)
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RuntimeState> {
        lock_state(&self.state)
    }
}
fn prompt_priority(prompt: &PromptKind) -> u8 {
    match prompt {
        PromptKind::CalendarEvent { .. } => 13,
        PromptKind::AppMeeting { .. } | PromptKind::AppHuddle { .. } => 12,
        // Level with a native meeting app: `PanelSlot::raise` keeps the
        // incumbent on a tie, so whichever of the two arrived first holds the
        // panel. The two cannot coexist today anyway, because
        // `retract_stale_prompts` runs before `evaluate` on every tick.
        PromptKind::AppCall { .. } => 12,
        PromptKind::BrowserCall { .. } => 11,
        PromptKind::UnknownMicSource => 10,
    }
}

/// Which rows the consent panel will draw for this prompt.
///
/// The window is sized before the webview renders, so the presenter has to
/// predict the two conditionals in ConsentPanel.tsx: the always-record
/// checkbox belongs to calendar prompts, and the series brief needs a
/// countdown for this same event that has someone to brief about.
fn prompt_panel_layout(
    pending: &PendingPrompt,
    countdown: Option<&DetectionCountdown>,
) -> ConsentPanelLayout {
    let event_key = match &pending.prompt {
        PromptKind::CalendarEvent { event_key, .. } => Some(event_key.as_str()),
        PromptKind::AppMeeting { .. }
        | PromptKind::AppHuddle { .. }
        | PromptKind::AppCall { .. }
        | PromptKind::BrowserCall { .. }
        | PromptKind::UnknownMicSource => None,
    };
    ConsentPanelLayout::Prompt {
        always_record_checkbox: event_key.is_some(),
        series_brief: event_key.is_some_and(|key| {
            countdown.is_some_and(|countdown| {
                countdown.event.event_key == key && !countdown.briefing.is_empty()
            })
        }),
    }
}

fn ritual_panel_layout(ritual: &MeetingRitual) -> ConsentPanelLayout {
    match ritual {
        MeetingRitual::Prep(card) => ConsentPanelLayout::Prep {
            loop_rows: u8::try_from(card.mine_open_loops.len())
                .unwrap_or(u8::MAX)
                .min(2),
            waiting_on: card.waiting_on_count != 0,
            participants: !card.participants.is_empty(),
        },
        MeetingRitual::Wrap(card) => ConsentPanelLayout::Wrap {
            loop_delta: card.follow_up_count != 0 || card.waiting_on_count != 0,
        },
        // The pill this card replaces is the same three rows at the same width.
        MeetingRitual::Recording(_) => ConsentPanelLayout::Recording {
            disclosure_note: false,
        },
    }
}

/// Why a pending prompt no longer applies, or `None` while it still does.
/// Pure, so the retraction rules are testable without a panel.
///
/// `sona_mic` is true while the input device reading belongs to Sona itself,
/// live or just closed. It retracts exactly one prompt kind, for the reason
/// that kind exists: see the arm below.
fn prompt_retraction(
    pending: &PendingPrompt,
    now_utc_ms: i64,
    running: &[RunningApp],
    mic: MicSignal,
    call_evidence: bool,
    sona_mic: bool,
) -> Option<DetectionPromptRetractionReason> {
    if pending
        .linked_event_end()
        .is_some_and(|end| now_utc_ms >= end)
    {
        return Some(DetectionPromptRetractionReason::EventEnded);
    }
    if pending
        .prompt
        .bundle_id()
        .is_some_and(|bundle_id| !apps::is_app_running(running, bundle_id))
    {
        return Some(DetectionPromptRetractionReason::TriggerAppQuit);
    }
    match pending.prompt {
        // A call prompt's episode is the call, not the microphone: the call it
        // offers to record may never raise the input device at all, so the
        // microphone rule below would retract it on the tick that raised it.
        PromptKind::AppCall { .. } => {
            (!call_evidence).then_some(DetectionPromptRetractionReason::CallEnded)
        }
        PromptKind::CalendarEvent { .. } => None,
        PromptKind::AppMeeting { .. }
        | PromptKind::AppHuddle { .. }
        | PromptKind::BrowserCall { .. } => {
            (mic == MicSignal::Idle).then_some(DetectionPromptRetractionReason::MicEpisodeEnded)
        }
        // The unknown-source prompt's whole evidence is "the device reads as
        // in use and nothing explains it". Sona's own stream explains it, so
        // the prompt has to go the instant the lease is held — and it cannot
        // wait for the microphone to report idle, because Sona keeping the
        // stream warm means it may never report idle at all. That is how one
        // of these stayed on screen for ninety seconds after the app that
        // raised it had quit. The three kinds above are unaffected: each
        // names an app the operator is using, which is evidence of its own.
        PromptKind::UnknownMicSource => (mic == MicSignal::Idle || sona_mic)
            .then_some(DetectionPromptRetractionReason::MicEpisodeEnded),
    }
}

fn ritual_is_stale(ritual: &MeetingRitual, now_utc_ms: i64) -> bool {
    matches!(ritual, MeetingRitual::Prep(card) if now_utc_ms >= card.start_utc_ms)
}

/// Reads the operator's settings into the decision table's policy. The timing
/// constants are fixed by the brief and are not settings.
pub fn policy_from_settings(settings: &AppSettings) -> DetectionPolicy {
    DetectionPolicy {
        enabled: settings.detection_enabled,
        calendar_enabled: settings.detection_calendar_enabled,
        any_mic_activity: settings.detection_any_mic_activity,
        auto_start_on_open_pane: settings.detection_auto_start_on_open_pane,
        lead_seconds: machine::CALENDAR_LEAD_SECONDS,
        attendee_floor: machine::ATTENDEE_FLOOR,
        cross_link_window_ms: machine::CROSS_LINK_WINDOW_MS,
    }
}

/// True when the wall clock advanced far more than the monotonic clock, which on
/// Apple platforms means the host suspended: `Instant` excludes sleep.
fn slept_between(
    previous_wall_ms: i64,
    wall_ms: i64,
    previous_monotonic: Instant,
    monotonic: Instant,
) -> bool {
    let wall_delta = wall_ms.saturating_sub(previous_wall_ms);
    if wall_delta <= 0 {
        return false;
    }
    let monotonic_delta = monotonic
        .saturating_duration_since(previous_monotonic)
        .as_millis();
    let Ok(monotonic_delta) = i64::try_from(monotonic_delta) else {
        return false;
    };
    let slack = i64::try_from(SLEEP_DETECTION_SLACK.as_millis()).unwrap_or(i64::MAX);
    wall_delta.saturating_sub(monotonic_delta) > slack
}

pub fn utc_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// Recovers a poisoned lock instead of propagating the panic: this state is
/// the tick's memory of the last one, and a detection thread that stops
/// deciding is a worse failure than a decision taken from stale memory.
fn lock_state(state: &Mutex<RuntimeState>) -> std::sync::MutexGuard<'_, RuntimeState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Bridges the CoreAudio callback into the tick loop. It holds the tick state
/// and the wakeup rather than the whole runtime: ending an input episode and
/// scheduling a tick is everything a device edge is allowed to do.
struct WakeOnInputChange {
    wakeup: Arc<Wakeup>,
    state: Arc<Mutex<RuntimeState>>,
}

impl InputDeviceObserver for WakeOnInputChange {
    fn input_device_changed(&self, signal: MicSignal) {
        if signal == MicSignal::Idle {
            // The episode is over: forget which apps were prompted for and
            // which were in use, so the next meeting is a fresh decision.
            lock_state(&self.state).end_input_episode();
        }
        self.wakeup.wake();
    }

    /// The output edge can be the first time FaceTime or Phone is observable.
    /// It must schedule a tick without consulting the previous tick's status;
    /// the tick still owns all call attribution and recording decisions.
    fn output_device_changed(&self) {
        self.wakeup.wake();
    }
}

/// Bridges a notification action click into the runtime.
struct RuntimeResponder {
    runtime: Arc<DetectionRuntime>,
}

impl PromptResponder for RuntimeResponder {
    fn prompt_answered(&self, response: PromptResponse) {
        match response {
            PromptResponse::Start { prompt_id } => self.runtime.respond(&prompt_id, true),
            PromptResponse::Dismiss { prompt_id } => self.runtime.respond(&prompt_id, false),
            // Sorted out by `ResponderCell` before it reaches the runtime; the
            // digest is not detection's, and the arm exists so that stays true
            // by construction rather than by comment.
            PromptResponse::DigestOpened => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    #[test]
    fn a_normal_tick_is_never_read_as_sleep() {
        let start = Instant::now();
        let slept = slept_between(1_000, 16_000, start, start + Duration::from_secs(15));

        assert!(!slept);
    }

    #[test]
    fn a_wall_clock_jump_the_monotonic_clock_did_not_see_is_sleep() {
        let start = Instant::now();
        // Ten minutes of wall time, fifteen seconds of uptime: the host suspended.
        let slept = slept_between(
            1_000,
            1_000 + 10 * 60_000,
            start,
            start + Duration::from_secs(15),
        );

        assert!(slept);
    }

    #[test]
    fn a_backwards_wall_clock_is_not_sleep() {
        let start = Instant::now();
        let slept = slept_between(60_000, 1_000, start, start + Duration::from_secs(15));

        assert!(!slept);
    }

    #[test]
    fn an_early_wake_clears_the_flag_without_blocking_the_next_wait() {
        let wakeup = Wakeup::default();
        wakeup.wake();

        let started = Instant::now();
        wakeup.wait(Some(Duration::from_secs(30)));

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a flagged wakeup must return immediately"
        );
    }

    /* The output edge used to be gated on the previous tick having already
     * seen a call app, so the first FaceTime call after a launch — nothing
     * published, nothing tracked — waited up to a full tick to be noticed.
     * The observer driven here is the one the CoreAudio monitor is handed. */
    #[test]
    fn an_unseen_call_output_edge_wakes_the_detection_loop() {
        let wakeup = Arc::new(Wakeup::default());
        let state = Arc::new(Mutex::new(RuntimeState::default()));
        let observer = WakeOnInputChange {
            wakeup: Arc::clone(&wakeup),
            state: Arc::clone(&state),
        };
        {
            let tick_state = lock_state(&state);
            assert!(tick_state.last_status.is_none(), "nothing published yet");
            assert!(tick_state.tracked.is_none(), "nothing tracked yet");
        }

        let started = Instant::now();
        observer.output_device_changed();
        wakeup.wait(Some(Duration::from_secs(3)));

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "an output edge must wake even before a FaceTime or Phone process was observed"
        );
    }

    #[test]
    fn disabled_detection_has_no_tick_deadline() {
        assert_eq!(tick_interval(false, false), None);
        assert_eq!(tick_interval(true, false), Some(TICK));
        assert_eq!(tick_interval(false, true), Some(TICK));
    }

    fn pending(prompt: PromptKind, show_introduction: bool) -> PendingPrompt {
        PendingPrompt {
            prompt,
            calendar_event: None,
            show_introduction,
            announce_in_chat: false,
        }
    }

    fn countdown(event_key: &str, briefed: bool) -> DetectionCountdown {
        DetectionCountdown {
            event: CalendarEventSummary {
                event_key: event_key.to_string(),
                series_key: "series-1".to_string(),
                title: "Weekly sync".to_string(),
                attendee_count: 4,
                start_utc_ms: 1_700_000_000_000,
                end_utc_ms: 1_700_000_000_000 + 30 * 60_000,
                attendees: Vec::new(),
                notes: None,
                calendar_name: None,
                url: None,
            },
            seconds_to_start: 48,
            briefing: if briefed {
                vec![PersonBriefingRow {
                    person_id: crate::meeting::people_types::PersonId::new(),
                    display_name: "Morgan Ellis".to_string(),
                    meetings_count: 3,
                    last: None,
                    open_loops: Vec::new(),
                    commitments: Vec::new(),
                }]
            } else {
                Vec::new()
            },
        }
    }

    #[test]
    fn an_app_prompt_is_sized_without_the_calendar_only_rows() {
        let prompt = pending(
            PromptKind::AppMeeting {
                bundle_id: "us.zoom.xos".to_string(),
                app_name: "Zoom".to_string(),
            },
            false,
        );

        assert_eq!(
            prompt_panel_layout(&prompt, Some(&countdown("event-1", true))),
            ConsentPanelLayout::Prompt {
                always_record_checkbox: false,
                series_brief: false,
            }
        );
    }

    #[test]
    fn a_calendar_prompt_counts_the_brief_only_for_its_own_briefed_event() {
        let prompt = pending(
            PromptKind::CalendarEvent {
                event_key: "event-1".to_string(),
                event_title: "Weekly sync".to_string(),
            },
            true,
        );
        let layout =
            |countdown: Option<&DetectionCountdown>| prompt_panel_layout(&prompt, countdown);
        let brief_shown = |countdown: Option<&DetectionCountdown>| {
            layout(countdown)
                == ConsentPanelLayout::Prompt {
                    always_record_checkbox: true,
                    series_brief: true,
                }
        };

        assert!(brief_shown(Some(&countdown("event-1", true))));
        assert!(!brief_shown(Some(&countdown("event-2", true))));
        assert!(!brief_shown(Some(&countdown("event-1", false))));
        assert!(!brief_shown(None));
    }

    /* FS2 in the detection map: a Zoom prompt raised while some event sat on
     * the calendar carried that event's end, so the capture it started
     * stopped when the event ended and the pending prompt was retracted at
     * that instant. Only a calendar prompt has an event to end with. */
    #[test]
    fn an_app_prompt_raised_beside_a_calendar_event_does_not_end_with_it() {
        let block = countdown("event-1", false).event;
        let zoom_running = [RunningApp {
            bundle_id: "us.zoom.xos".to_string(),
            display_name: "Zoom".to_string(),
            frontmost: true,
        }];
        let zoom_prompt = PendingPrompt {
            calendar_event: Some(block.clone()),
            ..pending(
                PromptKind::AppMeeting {
                    bundle_id: "us.zoom.xos".to_string(),
                    app_name: "Zoom".to_string(),
                },
                false,
            )
        };

        assert_eq!(zoom_prompt.linked_event_end(), None);
        assert_eq!(
            prompt_retraction(
                &zoom_prompt,
                block.end_utc_ms,
                &zoom_running,
                MicSignal::Active,
                false,
                false
            ),
            None,
            "the block ending is not the Zoom meeting ending"
        );

        let calendar_prompt = PendingPrompt {
            calendar_event: Some(block.clone()),
            ..pending(
                PromptKind::CalendarEvent {
                    event_key: block.event_key.clone(),
                    event_title: block.title.clone(),
                },
                false,
            )
        };

        assert_eq!(calendar_prompt.linked_event_end(), Some(block.end_utc_ms));
        assert_eq!(
            prompt_retraction(
                &calendar_prompt,
                block.end_utc_ms,
                &[],
                MicSignal::Active,
                false,
                false
            ),
            Some(DetectionPromptRetractionReason::EventEnded)
        );
    }

    /* Measured on the running 1.1.0 build: an ad-hoc prompt raised for Voice
     * Memos was still on screen ninety seconds after Voice Memos quit. The
     * unknown-source prompt carries no bundle ID, so the trigger-app rule
     * above can never fire for it, and its only other rule is a microphone
     * that goes idle — which never happens while Sona's own stream keeps the
     * device reading as in use. Nothing was left to withdraw it. */
    #[test]
    fn an_unknown_mic_prompt_is_withdrawn_once_sonas_own_stream_explains_the_device() {
        let orphaned = pending(PromptKind::UnknownMicSource, false);

        assert_eq!(
            prompt_retraction(&orphaned, NOW, &[], MicSignal::Active, false, true),
            Some(DetectionPromptRetractionReason::MicEpisodeEnded),
            "the device reads as in use because Sona holds it, so the prompt has \
             no unknown source left to offer"
        );
    }

    #[test]
    fn a_mic_idle_edge_withdraws_a_pending_ad_hoc_prompt() {
        let orphaned = pending(PromptKind::UnknownMicSource, false);

        assert_eq!(
            prompt_retraction(&orphaned, NOW, &[], MicSignal::Idle, false, false),
            Some(DetectionPromptRetractionReason::MicEpisodeEnded)
        );
    }

    /// Sona's microphone is not a reason to drop a prompt that named an app.
    /// A dictation run beside a live Zoom meeting says nothing about the Zoom
    /// meeting, and retracting there would lose the prompt the operator was
    /// about to answer.
    #[test]
    fn a_named_app_prompt_outlives_sonas_own_microphone() {
        let zoom_running = [RunningApp {
            bundle_id: "us.zoom.xos".to_string(),
            display_name: "Zoom".to_string(),
            frontmost: true,
        }];
        let zoom_prompt = pending(
            PromptKind::AppMeeting {
                bundle_id: "us.zoom.xos".to_string(),
                app_name: "Zoom".to_string(),
            },
            false,
        );

        assert_eq!(
            prompt_retraction(
                &zoom_prompt,
                NOW,
                &zoom_running,
                MicSignal::Active,
                false,
                true
            ),
            None
        );
    }

    /// The withdrawal has to reach the operator, not just the decision table.
    /// Without the wake the loop learns about the release on its next
    /// scheduled tick, up to `TICK` away.
    #[test]
    fn releasing_the_self_mic_lease_wakes_the_loop_inside_a_second() {
        let wakeup = Arc::new(Wakeup::default());
        let lease = SelfInputDeviceLease::default();
        lease.attach_wakeup(Arc::clone(&wakeup));
        lease.acquire();

        let started = Instant::now();
        lease.release();
        wakeup.wait(Some(Duration::from_secs(3)));

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the wait returned on the release, not on its own deadline"
        );
    }

    fn prep_card(start_utc_ms: i64) -> MeetingPrepCard {
        MeetingPrepCard {
            event_key: "event-1".to_string(),
            series_key: "series-1".to_string(),
            title: "Weekly sync".to_string(),
            start_utc_ms,
            last_meeting_id: MeetingSessionId::from_uuid(Uuid::nil()),
            headline: "Pricing stayed open.".to_string(),
            mine_open_loops: vec!["One".to_string(), "Two".to_string()],
            mine_open_loop_count: 2,
            waiting_on_count: 1,
            participants: vec![MeetingPrepParticipant {
                name: "Morgan".to_string(),
                meetings_count: 3,
                organization: Some("Northstar".to_string()),
            }],
            can_record_when_starts: true,
        }
    }

    #[test]
    fn ritual_layout_tracks_only_rows_the_cards_render() {
        assert_eq!(
            ritual_panel_layout(&MeetingRitual::Prep(prep_card(1_000))),
            ConsentPanelLayout::Prep {
                loop_rows: 2,
                waiting_on: true,
                participants: true,
            }
        );
        assert_eq!(
            ritual_panel_layout(&MeetingRitual::Wrap(MeetingWrapCard {
                session_id: MeetingSessionId::from_uuid(Uuid::nil()),
                title: "Weekly sync".to_string(),
                headline: "Saved.".to_string(),
                unresolved_speaker_count: Some(0),
                follow_up_count: 0,
                waiting_on_count: 0,
                waiting_on_names: Vec::new(),
            })),
            ConsentPanelLayout::Wrap { loop_delta: false }
        );
    }

    #[test]
    fn wrap_card_serializes_an_unavailable_speaker_count_as_null() {
        let card = MeetingWrapCard {
            session_id: MeetingSessionId::from_uuid(Uuid::nil()),
            title: "Weekly sync".to_string(),
            headline: "Saved.".to_string(),
            unresolved_speaker_count: None,
            follow_up_count: 0,
            waiting_on_count: 0,
            waiting_on_names: Vec::new(),
        };

        let payload = serde_json::to_value(card).expect("wrap card serializes");
        assert_eq!(payload["unresolvedSpeakerCount"], serde_json::Value::Null);
    }

    #[test]
    fn prep_expires_at_the_event_start_but_wrap_waits_for_its_idle_timer() {
        assert!(!ritual_is_stale(
            &MeetingRitual::Prep(prep_card(1_000)),
            999,
        ));
        assert!(ritual_is_stale(
            &MeetingRitual::Prep(prep_card(1_000)),
            1_000,
        ));
        assert!(!ritual_is_stale(
            &MeetingRitual::Wrap(MeetingWrapCard {
                session_id: MeetingSessionId::from_uuid(Uuid::nil()),
                title: "Weekly sync".to_string(),
                headline: "Saved.".to_string(),
                unresolved_speaker_count: Some(0),
                follow_up_count: 1,
                waiting_on_count: 0,
                waiting_on_names: Vec::new(),
            }),
            i64::MAX,
        ));
    }

    /// A capturing session as the store snapshots it, with the lanes it
    /// started.
    fn capturing(session_id: MeetingSessionId, sources: &[SourceKind]) -> MeetingSessionSnapshot {
        use crate::meeting::types::{
            CaptureCompleteness, MeetingSourceSnapshot, ProcessingStatus, SourceAvailability,
            SourceHealth, StorageAvailability,
        };
        MeetingSessionSnapshot {
            session_id,
            phase: MeetingPhase::CapturingRecording,
            revision: 1,
            title: "FaceTime call, 3:15 PM".to_string(),
            started_at_utc_ms: Some(1_700_000_000_000),
            elapsed_offset_ns: None,
            sources: sources
                .iter()
                .map(|source_kind| MeetingSourceSnapshot {
                    track_id: None,
                    source_kind: *source_kind,
                    required: true,
                    availability: SourceAvailability::Available,
                    health: SourceHealth::NotStarted,
                    format: None,
                    last_durable_offset_ns: None,
                    gap_count: 0,
                })
                .collect(),
            open_capture_window_started_at_ns: None,
            capture_completeness: CaptureCompleteness::NotStarted,
            storage: StorageAvailability::Available,
            processing_status: ProcessingStatus::Pending,
            preflight_local_processing: None,
            retention_deadline_utc_ms: None,
            allowed_actions: Vec::new(),
        }
    }

    fn tracked_call(session_id: MeetingSessionId) -> TrackedCapture {
        TrackedCapture::detection(
            &capturing(session_id, &SourceKind::ALL),
            Some("com.apple.facetime".to_string()),
            None,
        )
    }

    fn facetime(frontmost: bool) -> machine::CallSignal {
        machine::CallSignal::Running {
            bundle_id: "com.apple.facetime".to_string(),
            display_name: "FaceTime".to_string(),
            frontmost,
        }
    }

    /* FS5 in the detection map: a capture whose microphone lane was toggled
     * off is not listening to the input device, so the device going idle says
     * nothing about its meeting. The lanes come from the session's snapshot. */
    #[test]
    fn a_capture_records_which_lanes_it_started() {
        let session_id = MeetingSessionId::new();

        let system_audio_only = TrackedCapture::detection(
            &capturing(session_id, &[SourceKind::SystemAudio]),
            Some("com.google.chrome".to_string()),
            None,
        );
        assert!(!system_audio_only.microphone_lane());
        assert!(tracked_call(session_id).microphone_lane());
    }

    /* Only a call app's own trigger arms the output watch, and a tick for
     * another session reads nothing, so a stale stop cannot end the wrong
     * capture. Zoom is a meeting app rather than a call app: its capture is
     * tied to the process, and `TriggerAppExited` is what ends it. */
    #[test]
    fn only_a_call_capture_watches_the_output_device() {
        let session_id = MeetingSessionId::new();
        let mut state = RuntimeState::default();
        state.tracked = Some(TrackedCapture::detection(
            &capturing(session_id, &SourceKind::ALL),
            Some("us.zoom.xos".to_string()),
            None,
        ));

        assert!(state
            .observe_call_output(session_id, OutputSignal::Active, NOW)
            .is_none());

        state.tracked = Some(tracked_call(session_id));

        assert!(state
            .observe_call_output(MeetingSessionId::new(), OutputSignal::Active, NOW)
            .is_none());
        let playing = state
            .observe_call_output(session_id, OutputSignal::Active, NOW)
            .expect("a call capture watches its output");
        assert!(!playing.hung_up(NOW + machine::CALL_HANGUP_GRACE_MS));
        let hung_up = state
            .observe_call_output(session_id, OutputSignal::Idle, NOW + 1_000)
            .expect("a call capture watches its output");
        assert_eq!(
            evaluate_stop(&StopInputs {
                now_utc_ms: NOW + 1_000 + machine::CALL_HANGUP_GRACE_MS,
                linked_event_end_utc_ms: None,
                self_holds_input_device: true,
                device_running_somewhere: true,
                microphone_lane: true,
                call_output: Some(hung_up),
                trigger_app_running: true,
                slept_since_start: false,
            }),
            Some(machine::StopTrigger::CallEnded)
        );
    }

    /* The adoption path this replaced: a capture with no call-app trigger
     * armed the watch as soon as `call_evidence` held. During a live capture
     * that evidence is always available with a call app in front — Sona's own
     * stream is what keeps the input device active — and its other half reads
     * the default output device, which is device-wide rather than
     * per-application. Any other application's playback ending would then
     * have stopped a live ad-hoc capture ten seconds later. */
    #[test]
    fn a_capture_no_call_app_started_never_adopts_a_call_output_stop() {
        let session_id = MeetingSessionId::new();
        assert!(
            machine::call_evidence(&facetime(true), MicSignal::Active, OutputSignal::Active),
            "the evidence the removed adoption path read is present here"
        );

        // A manual note, then a calendar-linked capture: neither was started
        // by a call app, so neither may be handed the output device.
        for event_end_utc_ms in [None, Some(NOW + 2 * machine::CALL_HANGUP_GRACE_MS)] {
            let mut state = RuntimeState::default();
            state.tracked = Some(TrackedCapture::detection(
                &capturing(session_id, &SourceKind::ALL),
                None,
                event_end_utc_ms,
            ));

            assert!(
                state
                    .observe_call_output(session_id, OutputSignal::Active, NOW)
                    .is_none(),
                "playback through the default output is not this capture's call"
            );
            let call_output = state.observe_call_output(session_id, OutputSignal::Idle, NOW + 1);
            assert_eq!(
                evaluate_stop(&StopInputs {
                    now_utc_ms: NOW + 1 + machine::CALL_HANGUP_GRACE_MS,
                    linked_event_end_utc_ms: event_end_utc_ms,
                    self_holds_input_device: true,
                    device_running_somewhere: true,
                    microphone_lane: true,
                    call_output,
                    trigger_app_running: true,
                    slept_since_start: false,
                }),
                None,
                "that playback ending must not stop the capture"
            );
        }
    }

    /* FS1 in the detection map: `slept` was set by any loop iteration and
     * reset only when the tracked capture ended, so a Mac that slept while
     * nothing was tracked stopped the next capture on its first tick. Sleep,
     * then start, then tick: no `SleepBoundary`. */
    #[test]
    fn a_sleep_before_the_capture_is_not_a_sleep_during_it() {
        let session_id = MeetingSessionId::new();
        let mut state = RuntimeState::default();
        state.slept = true;

        state.begin_tracked(
            tracked_call(session_id),
            RecentCapture {
                session_id: session_id.uuid().to_string(),
                started_utc_ms: 1_700_000_000_000,
            },
        );

        assert!(!state.slept);
        assert_eq!(
            evaluate_stop(&StopInputs {
                now_utc_ms: 1_700_000_000_000,
                linked_event_end_utc_ms: None,
                self_holds_input_device: false,
                device_running_somewhere: true,
                microphone_lane: true,
                call_output: None,
                trigger_app_running: true,
                slept_since_start: state.slept,
            },),
            None
        );

        // A boundary crossed during the capture still ends it.
        state.slept = true;
        assert!(state.end_tracked(session_id).is_some());
        assert!(!state.slept, "ending the capture clears its boundary");
    }

    fn recording_card(session_id: MeetingSessionId) -> MeetingRecordingCard {
        MeetingRecordingCard {
            session_id,
            bundle_id: "com.apple.facetime".to_string(),
            app_name: "FaceTime".to_string(),
            started_at_utc_ms: 1_700_000_000_000,
        }
    }

    fn transition_at(
        state: &mut RuntimeState,
        tracked: &TrackedCapture,
        now_utc_ms: i64,
        running: &[RunningApp],
        mic: MicSignal,
        output: OutputSignal,
        sona_holds: bool,
        call: &machine::CallSignal,
        call_live: bool,
    ) -> CaptureTransition {
        evaluate_running_capture_transition(
            state,
            tracked,
            CaptureTick {
                running,
                mic,
                output,
                sona_holds,
                call,
                call_live,
            },
            now_utc_ms,
        )
    }

    fn call_inputs(
        call: machine::CallSignal,
        mic: MicSignal,
        output: OutputSignal,
    ) -> DetectionInputs {
        DetectionInputs {
            now_utc_ms: NOW,
            calendar: CalendarSignal::Absent,
            app: machine::AppSignal::Absent,
            call,
            mic,
            output,
            browser_title: machine::BrowserTitleEvidence::NoMatch,
            standing_series_consent: false,
            standing_app_consent: true,
            recent_capture: None,
            self_holds_input_device: false,
            self_mic_just_closed: false,
            capture_active: false,
        }
    }

    /* The 15s tick is what makes this load-bearing: without the claim, a call
     * on the auto-record list would start a recording on every tick it stays
     * live. `prompted_apps` cannot carry it, because a call detected on the
     * output signal alone never opens a microphone episode to end. */
    #[test]
    fn a_call_is_acted_on_once_and_re_arms_only_when_it_ends() {
        let mut state = RuntimeState::default();
        let policy = DetectionPolicy::default();
        let live = call_inputs(facetime(true), MicSignal::Active, OutputSignal::Idle);
        // Each tick re-arms from its own evidence before it decides, in the
        // order `tick` does, so a live call must keep the claim it made.
        state.rearm_call_claims(machine::call_evidence(&live.call, live.mic, live.output));
        let first = evaluate(&live, &policy);
        let bundle_id = match first {
            DetectionOutcome::AutoStartCall { bundle_id, .. } => bundle_id,
            other => panic!("expected an auto-start call outcome, got {other:?}"),
        };

        assert!(state.claim_call(&bundle_id));
        state.rearm_call_claims(machine::call_evidence(&live.call, live.mic, live.output));
        let later = evaluate(&live, &policy);
        let later_bundle_id = match later {
            DetectionOutcome::AutoStartCall { bundle_id, .. } => bundle_id,
            other => panic!("expected the live call on the next tick, got {other:?}"),
        };
        assert!(
            !state.claim_call(&later_bundle_id),
            "a later tick inside the same call must not start a second recording"
        );

        let ended = call_inputs(
            machine::CallSignal::Absent,
            MicSignal::Idle,
            OutputSignal::Idle,
        );
        state.rearm_call_claims(machine::call_evidence(&ended.call, ended.mic, ended.output));

        assert!(
            state.claim_call(&bundle_id),
            "the next call is a fresh decision"
        );
    }

    #[test]
    fn a_call_capture_stops_after_output_silence_grace_and_resets_before_it() {
        let session_id = MeetingSessionId::new();
        let tracked = tracked_call(session_id);
        let mut state = RuntimeState::default();
        state.tracked = Some(tracked.clone());
        let call = facetime(true);
        let running = [RunningApp {
            bundle_id: "com.apple.facetime".to_string(),
            display_name: "FaceTime".to_string(),
            frontmost: true,
        }];

        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW,
                &running,
                MicSignal::Active,
                OutputSignal::Active,
                true,
                &call,
                true,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW + 1_000,
                &running,
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &call,
                true,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW + 1_000 + machine::CALL_HANGUP_GRACE_MS - 1,
                &running,
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &call,
                true,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW + 1_000 + machine::CALL_HANGUP_GRACE_MS,
                &running,
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &call,
                true,
            ),
            CaptureTransition::Stop(machine::StopTrigger::CallEnded)
        );

        let resumed_id = MeetingSessionId::new();
        let resumed = tracked_call(resumed_id);
        let mut resumed_state = RuntimeState::default();
        resumed_state.tracked = Some(resumed.clone());
        assert_eq!(
            transition_at(
                &mut resumed_state,
                &resumed,
                NOW,
                &running,
                MicSignal::Active,
                OutputSignal::Active,
                true,
                &call,
                true,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut resumed_state,
                &resumed,
                NOW + 1_000,
                &running,
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &call,
                true,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut resumed_state,
                &resumed,
                NOW + 3_000,
                &running,
                MicSignal::Active,
                OutputSignal::Active,
                true,
                &call,
                true,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut resumed_state,
                &resumed,
                NOW + 4_000,
                &running,
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &call,
                true,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut resumed_state,
                &resumed,
                NOW + 4_000 + machine::CALL_HANGUP_GRACE_MS - 1,
                &running,
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &call,
                true,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut resumed_state,
                &resumed,
                NOW + 4_000 + machine::CALL_HANGUP_GRACE_MS,
                &running,
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &call,
                true,
            ),
            CaptureTransition::Stop(machine::StopTrigger::CallEnded)
        );
    }

    #[test]
    fn a_tracked_capture_stops_when_its_triggering_app_quits() {
        let call_id = MeetingSessionId::new();
        let call_capture = tracked_call(call_id);
        let mut call_state = RuntimeState::default();
        call_state.tracked = Some(call_capture.clone());
        let call = facetime(true);
        let call_running = [RunningApp {
            bundle_id: "com.apple.facetime".to_string(),
            display_name: "FaceTime".to_string(),
            frontmost: true,
        }];

        assert_eq!(
            transition_at(
                &mut call_state,
                &call_capture,
                NOW,
                &call_running,
                MicSignal::Active,
                OutputSignal::Active,
                true,
                &call,
                true,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut call_state,
                &call_capture,
                NOW + 1_000,
                &[],
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &machine::CallSignal::Absent,
                false,
            ),
            CaptureTransition::Stop(machine::StopTrigger::TriggerAppExited)
        );

        let zoom_id = MeetingSessionId::new();
        let zoom_capture = TrackedCapture::detection(
            &capturing(zoom_id, &SourceKind::ALL),
            Some("us.zoom.xos".to_string()),
            None,
        );
        let mut zoom_state = RuntimeState::default();
        zoom_state.tracked = Some(zoom_capture.clone());
        let zoom_running = [RunningApp {
            bundle_id: "us.zoom.xos".to_string(),
            display_name: "Zoom".to_string(),
            frontmost: true,
        }];
        assert_eq!(
            transition_at(
                &mut zoom_state,
                &zoom_capture,
                NOW,
                &zoom_running,
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &machine::CallSignal::Absent,
                false,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut zoom_state,
                &zoom_capture,
                NOW + 1_000,
                &[],
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &machine::CallSignal::Absent,
                false,
            ),
            CaptureTransition::Stop(machine::StopTrigger::TriggerAppExited)
        );
    }

    #[test]
    fn a_microphone_capture_stops_on_idle_but_held_and_system_audio_do_not() {
        let session_id = MeetingSessionId::new();
        let tracked =
            TrackedCapture::detection(&capturing(session_id, &SourceKind::ALL), None, None);
        let mut state = RuntimeState::default();
        state.tracked = Some(tracked.clone());
        let call = machine::CallSignal::Absent;

        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW,
                &[],
                MicSignal::Active,
                OutputSignal::Idle,
                false,
                &call,
                false,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW + 1_000,
                &[],
                MicSignal::Idle,
                OutputSignal::Idle,
                false,
                &call,
                false,
            ),
            CaptureTransition::Stop(machine::StopTrigger::InputDeviceIdle)
        );

        let held_id = MeetingSessionId::new();
        let held = TrackedCapture::detection(&capturing(held_id, &SourceKind::ALL), None, None);
        let mut held_state = RuntimeState::default();
        held_state.tracked = Some(held.clone());
        assert_eq!(
            transition_at(
                &mut held_state,
                &held,
                NOW,
                &[],
                MicSignal::Idle,
                OutputSignal::Idle,
                true,
                &call,
                false,
            ),
            CaptureTransition::Continue
        );

        let system_audio_id = MeetingSessionId::new();
        let system_audio = TrackedCapture::detection(
            &capturing(system_audio_id, &[SourceKind::SystemAudio]),
            None,
            None,
        );
        let mut system_audio_state = RuntimeState::default();
        system_audio_state.tracked = Some(system_audio.clone());
        assert_eq!(
            transition_at(
                &mut system_audio_state,
                &system_audio,
                NOW,
                &[],
                MicSignal::Idle,
                OutputSignal::Idle,
                false,
                &call,
                false,
            ),
            CaptureTransition::Continue
        );
    }

    #[test]
    fn an_operator_capture_adopts_once_then_uses_the_adopted_call_rules() {
        let session_id = MeetingSessionId::new();
        let mut tracked = TrackedCapture::operator(&capturing(session_id, &SourceKind::ALL));
        let mut state = RuntimeState::default();
        state.tracked = Some(tracked.clone());
        let no_call = machine::CallSignal::Absent;
        let zoom = [RunningApp {
            bundle_id: "us.zoom.xos".to_string(),
            display_name: "Zoom".to_string(),
            frontmost: true,
        }];

        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW,
                &zoom,
                MicSignal::Idle,
                OutputSignal::Idle,
                false,
                &no_call,
                false,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW + 1_000,
                &[],
                MicSignal::Idle,
                OutputSignal::Idle,
                false,
                &no_call,
                false,
            ),
            CaptureTransition::Continue
        );

        let facetime_call = facetime(true);
        let facetime_running = [RunningApp {
            bundle_id: "com.apple.facetime".to_string(),
            display_name: "FaceTime".to_string(),
            frontmost: true,
        }];
        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW + 1_500,
                &facetime_running,
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &facetime(false),
                false,
            ),
            CaptureTransition::Continue
        );
        assert!(state.adopted_call().is_none());
        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW + 2_000,
                &facetime_running,
                MicSignal::Active,
                OutputSignal::Active,
                true,
                &facetime_call,
                true,
            ),
            CaptureTransition::Adopted(machine::AdoptedCall {
                bundle_id: "com.apple.facetime".to_string(),
                display_name: "FaceTime".to_string(),
            })
        );
        tracked = state
            .tracked
            .clone()
            .expect("adoption keeps the capture tracked");
        assert_eq!(
            state.adopted_call(),
            Some(machine::AdoptedCall {
                bundle_id: "com.apple.facetime".to_string(),
                display_name: "FaceTime".to_string(),
            })
        );

        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW + 2_500,
                &facetime_running,
                MicSignal::Idle,
                OutputSignal::Active,
                false,
                &facetime_call,
                true,
            ),
            CaptureTransition::Continue
        );
        let phone = machine::CallSignal::Running {
            bundle_id: "com.apple.mobilephone".to_string(),
            display_name: "Phone".to_string(),
            frontmost: true,
        };
        let both_calls = [
            facetime_running[0].clone(),
            RunningApp {
                bundle_id: "com.apple.mobilephone".to_string(),
                display_name: "Phone".to_string(),
                frontmost: true,
            },
        ];
        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW + 3_000,
                &both_calls,
                MicSignal::Active,
                OutputSignal::Active,
                true,
                &phone,
                true,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            state
                .adopted_call()
                .expect("adopted call remains stable")
                .bundle_id,
            "com.apple.facetime"
        );

        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW + 4_000,
                &facetime_running,
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &facetime_call,
                true,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                NOW + 4_000 + machine::CALL_HANGUP_GRACE_MS,
                &facetime_running,
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &facetime_call,
                true,
            ),
            CaptureTransition::Stop(machine::StopTrigger::CallEnded)
        );

        let quit_id = MeetingSessionId::new();
        let mut quit_tracked = TrackedCapture::operator(&capturing(quit_id, &SourceKind::ALL));
        let mut quit_state = RuntimeState::default();
        quit_state.tracked = Some(quit_tracked.clone());
        assert!(matches!(
            transition_at(
                &mut quit_state,
                &quit_tracked,
                NOW,
                &facetime_running,
                MicSignal::Active,
                OutputSignal::Active,
                true,
                &facetime_call,
                true,
            ),
            CaptureTransition::Adopted(_)
        ));
        quit_tracked = quit_state
            .tracked
            .clone()
            .expect("adopted capture remains tracked");
        assert_eq!(
            transition_at(
                &mut quit_state,
                &quit_tracked,
                NOW + 1_000,
                &[],
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &machine::CallSignal::Absent,
                false,
            ),
            CaptureTransition::Stop(machine::StopTrigger::TriggerAppExited)
        );
    }

    #[test]
    fn a_sleep_boundary_stops_every_tracked_capture_on_the_next_tick() {
        let detection_id = MeetingSessionId::new();
        let detection =
            TrackedCapture::detection(&capturing(detection_id, &SourceKind::ALL), None, None);
        let mut detection_state = RuntimeState::default();
        detection_state.tracked = Some(detection.clone());

        assert_eq!(
            transition_at(
                &mut detection_state,
                &detection,
                NOW,
                &[],
                MicSignal::Active,
                OutputSignal::Active,
                true,
                &machine::CallSignal::Absent,
                false,
            ),
            CaptureTransition::Continue
        );
        detection_state.slept = true;
        assert_eq!(
            transition_at(
                &mut detection_state,
                &detection,
                NOW + 1_000,
                &[],
                MicSignal::Active,
                OutputSignal::Active,
                true,
                &machine::CallSignal::Absent,
                false,
            ),
            CaptureTransition::Stop(machine::StopTrigger::SleepBoundary)
        );

        let adopted_id = MeetingSessionId::new();
        let mut adopted = TrackedCapture::operator(&capturing(adopted_id, &SourceKind::ALL));
        let mut adopted_state = RuntimeState::default();
        adopted_state.tracked = Some(adopted.clone());
        let adopted_call = facetime(true);
        let adopted_running = [RunningApp {
            bundle_id: "com.apple.facetime".to_string(),
            display_name: "FaceTime".to_string(),
            frontmost: true,
        }];
        assert_eq!(
            transition_at(
                &mut adopted_state,
                &adopted,
                NOW,
                &adopted_running,
                MicSignal::Active,
                OutputSignal::Active,
                true,
                &adopted_call,
                true,
            ),
            CaptureTransition::Adopted(machine::AdoptedCall {
                bundle_id: "com.apple.facetime".to_string(),
                display_name: "FaceTime".to_string(),
            })
        );
        adopted = adopted_state
            .tracked
            .clone()
            .expect("adopted capture remains tracked");
        assert_eq!(
            transition_at(
                &mut adopted_state,
                &adopted,
                NOW + 1_000,
                &adopted_running,
                MicSignal::Active,
                OutputSignal::Active,
                true,
                &adopted_call,
                true,
            ),
            CaptureTransition::Continue
        );
        adopted_state.slept = true;
        assert_eq!(
            transition_at(
                &mut adopted_state,
                &adopted,
                NOW + 2_000,
                &adopted_running,
                MicSignal::Active,
                OutputSignal::Active,
                true,
                &adopted_call,
                true,
            ),
            CaptureTransition::Stop(machine::StopTrigger::SleepBoundary)
        );
    }

    #[test]
    fn a_linked_capture_stops_at_event_end_not_before_it() {
        let session_id = MeetingSessionId::new();
        let event_end = NOW + 1_000;
        let tracked = TrackedCapture::detection(
            &capturing(session_id, &SourceKind::ALL),
            None,
            Some(event_end),
        );
        let mut state = RuntimeState::default();
        state.tracked = Some(tracked.clone());
        let call = machine::CallSignal::Absent;

        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                event_end - 1,
                &[],
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &call,
                false,
            ),
            CaptureTransition::Continue
        );
        assert_eq!(
            transition_at(
                &mut state,
                &tracked,
                event_end,
                &[],
                MicSignal::Active,
                OutputSignal::Idle,
                true,
                &call,
                false,
            ),
            CaptureTransition::Stop(machine::StopTrigger::EventEnd)
        );
    }

    /* Which apps the operator used is the episode's memory, and the episode
     * ends with the microphone: both per-episode sets clear at that boundary,
     * so the next meeting in Zoom is judged on its own evidence. */
    #[test]
    fn the_microphone_going_idle_forgets_which_apps_were_in_use() {
        let mut state = RuntimeState::default();
        state.apps_used.insert("us.zoom.xos".to_string());
        state.prompted_apps.insert("us.zoom.xos".to_string());

        state.end_input_episode();

        assert!(state.apps_used.is_empty());
        assert!(state.prompted_apps.is_empty());
    }

    /* The card is raised while the capture holds the panel, so it must be
     * presented rather than sent to fallback, and it must be findable by the
     * session it names so `track_ended` can finish it. */
    #[test]
    fn the_recording_card_is_presented_on_the_panel_and_found_by_its_session() {
        let session_id = MeetingSessionId::new();
        let mut state = RuntimeState::default();
        state.tracked = Some(tracked_call(session_id));
        state.panel.begin_capture(PendingPanel::is_prompt);

        let commands = state
            .raise_recording_card("ritual-1".to_string(), recording_card(session_id))
            .expect("the tracked capture takes its card");

        assert!(
            matches!(
                commands.as_slice(),
                [PanelCommand::PresentPanel { prompt_id, .. }] if prompt_id == "ritual-1"
            ),
            "the card must be presented, not sent to fallback: {commands:?}"
        );
        assert_eq!(
            state.recording_card_id(session_id),
            Some("ritual-1".to_string())
        );
        assert_eq!(state.recording_card_id(MeetingSessionId::new()), None);
    }

    /* A start that lost the race against its own stop: the card would otherwise
     * be shown for a capture that is already gone, with nothing left to retract
     * it. */
    #[test]
    fn a_card_for_an_untracked_capture_is_refused() {
        let session_id = MeetingSessionId::new();
        let mut state = RuntimeState::default();

        assert!(state
            .raise_recording_card("ritual-1".to_string(), recording_card(session_id))
            .is_none());

        state.tracked = Some(tracked_call(MeetingSessionId::new()));

        assert!(state
            .raise_recording_card("ritual-1".to_string(), recording_card(session_id))
            .is_none());
        assert_eq!(state.recording_card_id(session_id), None);
    }

    /* A capture that ended by a route `track_ended` never heard about leaves
     * its card in the slot; the next capture's `begin_capture` is what takes
     * it out, as a discarded ritual to retract. */
    #[test]
    fn a_replacing_capture_discards_the_previous_card() {
        let session_id = MeetingSessionId::new();
        let mut state = RuntimeState::default();
        state.tracked = Some(tracked_call(session_id));
        state.panel.begin_capture(PendingPanel::is_prompt);
        state
            .raise_recording_card("ritual-1".to_string(), recording_card(session_id))
            .expect("the tracked capture takes its card");

        state.tracked = Some(tracked_call(MeetingSessionId::new()));
        let capture = state.panel.begin_capture(PendingPanel::is_prompt);

        assert!(
            capture
                .discarded
                .iter()
                .any(|(ritual_id, _)| ritual_id == "ritual-1"),
            "the stale card must be discarded for retraction"
        );
        assert_eq!(state.recording_card_id(session_id), None);
    }

    #[test]
    fn a_stop_for_some_other_session_changes_nothing() {
        let session_id = MeetingSessionId::new();
        let mut state = RuntimeState::default();
        state.tracked = Some(tracked_call(session_id));

        assert!(state.end_tracked(MeetingSessionId::new()).is_none());
        assert!(state.tracked.is_some());
    }

    #[test]
    fn the_recording_card_reuses_the_in_session_pill_geometry() {
        assert_eq!(
            ritual_panel_layout(&MeetingRitual::Recording(recording_card(
                MeetingSessionId::from_uuid(Uuid::nil())
            ))),
            ConsentPanelLayout::Recording {
                disclosure_note: false
            }
        );
    }
}
