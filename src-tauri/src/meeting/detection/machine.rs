//! The whole decision authority for automatic meeting detection.
//!
//! Every rule from the detection brief's §5.3 decision table, §5.4 prompt copy,
//! and §5.5 auto-stop heuristic lives here as pure functions over owned inputs.
//! Nothing in this module reads the clock, the calendar, the audio device, or
//! settings; the runtime in the parent module collects those and hands them in.
//! That split is deliberate: the platform observers are untestable in CI, and
//! the policy they feed is the part that decides whether Sona records a user's
//! meeting, so the policy is the part that must be exhaustively tested.
//!
//! Two invariants this module owns, and no other layer may re-decide:
//!
//! 1. **A detection outcome is never a capture grant.** The strongest outcome is
//!    `AutoStart`, which still routes through the preflight consent screen. The
//!    session layer owns the consent receipt; detection has no authority to
//!    forge one.
//! 2. **Sona's own microphone is never meeting evidence.** The CoreAudio
//!    device-in-use property is device-global, so dictation and Sona's own
//!    meeting capture both raise it. Reading that as a meeting would make every
//!    push-to-talk run prompt "meeting detected".

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use specta::Type;

/// Calendar lead time for the pre-meeting prompt. The brief fixes this at T-60s.
pub const CALENDAR_LEAD_SECONDS: i64 = 60;

/// Attendee floor for the calendar path. An event with fewer participants is a
/// personal blocked-time entry, not a meeting.
pub const ATTENDEE_FLOOR: usize = 2;

/// How long a just-started capture keeps absorbing new ad-hoc activity instead
/// of spawning a second note (§5.3 case 8).
pub const CROSS_LINK_WINDOW_MS: i64 = 15 * 60 * 1000;

/// How one participant answered the invitation, as EventKit reports it.
///
/// `EKParticipantStatus` also carries `Delegated`, `Completed` and
/// `InProcess`, which describe reminders and task assignments rather than an
/// answer to a meeting invitation. They collapse into `Unknown` here: the card
/// renders "no answer", which is the true statement, instead of inventing an
/// attendance claim from a task state.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ParticipationStatus {
    /// EventKit has no answer for this participant.
    #[default]
    Unknown,
    Pending,
    Accepted,
    Declined,
    Tentative,
}

/// One named participant on a calendar event.
///
/// Only participants EventKit names reach this type. An unnamed participant is
/// a row that could say nothing but "someone", so it is dropped at the
/// EventKit boundary and counted in `attendee_count` alone.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CalendarAttendee {
    pub name: String,
    pub status: ParticipationStatus,
    /// Normalized address from EventKit's `mailto:` participant URL.
    #[serde(default)]
    pub email: Option<String>,
    /// True for the account that owns the calendar this event lives on.
    pub is_self: bool,
}

/// One calendar event, reduced to the fields the pre-meeting surfaces read.
///
/// A qualifying detection durably records these facts in the encrypted
/// workflow-event store before its briefing runs. Accepting the event also
/// copies the same facts onto the meeting session.
/// The decision table reads only `attendee_count` and the two instants.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEventSummary {
    /// Identity for this occurrence. Recurring events share `series_key`, but
    /// every start instant gets its own claim and prompt.
    pub event_key: String,
    /// EventKit's calendar-item identifier, used only for standing series
    /// consent and continuity priming.
    #[serde(default)]
    pub series_key: String,
    pub title: String,
    /// Participant count including the organizer, and including participants
    /// EventKit refused to name. Zero means the event carries no attendee list
    /// at all, which §5.3 case 9 treats the same as solo.
    pub attendee_count: usize,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    /// The participants EventKit named. Shorter than `attendee_count` when the
    /// event carries anonymous participants, and empty when it names none.
    #[serde(default)]
    pub attendees: Vec<CalendarAttendee>,
    /// The event's own notes, trimmed. `None` when the event carries none.
    #[serde(default)]
    pub notes: Option<String>,
    /// Title of the calendar the event sits on. `None` when EventKit does not
    /// report one.
    #[serde(default)]
    pub calendar_name: Option<String>,
    /// The URL attached to the event, which for a scheduled call is the join
    /// link. `None` when the event carries none.
    #[serde(default)]
    pub url: Option<String>,
}

/// Where the nearest calendar event sits relative to now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CalendarSignal {
    /// No event is near, or the calendar path is disabled or unauthorized.
    Absent,
    /// The event has not started yet.
    Upcoming {
        event: CalendarEventSummary,
        seconds_to_start: i64,
    },
    /// The event's start instant has passed and its end has not.
    Started { event: CalendarEventSummary },
}

/// Which application, if any, could own the current microphone activity.
///
/// Presence is not participation. A meeting app that is merely open explains
/// nothing about a live microphone — Zoom sits in the menu bar all day — so
/// `Known` names an app the operator is *using* this input-device episode,
/// and `Present` names one that is only running.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppSignal {
    /// Neither an allowlisted meeting app nor a browser is a candidate.
    Absent,
    /// An allowlisted meeting application the operator has been in front of
    /// since the microphone went active: frontmost now, or at some tick of
    /// this episode.
    Known {
        bundle_id: String,
        display_name: String,
        /// True when this app currently receives key events. Not required by any
        /// rule; carried so the prompt can name the app the user is looking at.
        frontmost: bool,
    },
    /// A browser is frontmost and no allowlisted native meeting app is in use.
    Browser {
        bundle_id: String,
        display_name: String,
    },
    /// An allowlisted meeting application is running, but the operator has not
    /// been in front of it since the microphone went active, and no browser is
    /// in front either.
    Present {
        bundle_id: String,
        display_name: String,
    },
}

/// A running application whose meetings are calls: FaceTime and Phone.
///
/// Separate from `AppSignal` because the two dimensions answer different
/// questions and must not shadow each other. A call app never carries a
/// calendar event, its microphone is often an AirPods-class Bluetooth device
/// that the input-device property under-reports, and it is the only dimension
/// with a standing per-application grant. Folding it into `AppSignal` would
/// mean one of Zoom-is-open and FaceTime-is-open silently losing to the other.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallSignal {
    /// No allowlisted call application is running.
    Absent,
    Running {
        bundle_id: String,
        display_name: String,
        /// True when this app currently receives key events. Load-bearing:
        /// neither clause of `call_is_live` holds without it.
        frontmost: bool,
    },
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AdoptedCall {
    pub bundle_id: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperatorCapture {
    Waiting,
    Adopted {
        call: AdoptedCall,
        watch: CallOutputWatch,
    },
}

/// State of the default input device, as reported by CoreAudio's
/// `kAudioDevicePropertyDeviceIsRunningSomewhere`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MicSignal {
    Idle,
    Active,
}

/// State of the default *output* device, as reported by the same CoreAudio
/// `kAudioDevicePropertyDeviceIsRunningSomewhere` the microphone dimension
/// reads. Only the call path consults it: a live call plays the other side
/// through the default output, and an idle FaceTime plays nothing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputSignal {
    Idle,
    Active,
}

/// What a browser window title says about the tab in front. `Unreadable` is the
/// honest state when Accessibility is not trusted: the reader needs it to ask
/// the browser for its focused window at all, and reports so rather than
/// guessing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserTitleEvidence {
    MeetingMatch,
    NoMatch,
    Unreadable,
}

/// A capture Sona started recently. Feeds §5.3 case 8 cross-linking.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentCapture {
    pub session_id: String,
    pub started_utc_ms: i64,
}

/// Everything the decision table reads, collected at one instant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetectionInputs {
    pub now_utc_ms: i64,
    pub calendar: CalendarSignal,
    pub app: AppSignal,
    /// The call dimension, evaluated before the calendar and the ad-hoc paths.
    pub call: CallSignal,
    pub mic: MicSignal,
    pub output: OutputSignal,
    pub browser_title: BrowserTitleEvidence,
    /// True only when the session layer found a live standing grant for this
    /// event's series. A visible countdown is context, never consent.
    pub standing_series_consent: bool,
    /// True only when the operator's auto-record list names the call app in
    /// `call`. The grant is a stored setting, not something detection decides.
    pub standing_app_consent: bool,
    pub recent_capture: Option<RecentCapture>,
    /// True when Sona itself holds the default input device — its own dictation
    /// run or the microphone lane of its own meeting capture.
    pub self_holds_input_device: bool,
    /// True while Sona's own microphone stream closed recently enough that
    /// CoreAudio's device-global property may still be reporting it. Only the
    /// ad-hoc opt-in reads this: every other path carries evidence that is not
    /// the device reading.
    pub self_mic_just_closed: bool,
    /// True when a Sona meeting capture is already running.
    pub capture_active: bool,
}

/// Operator-controlled detection behavior. Timing constants live here rather
/// than being read from the consts directly so tests can state them explicitly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DetectionPolicy {
    pub enabled: bool,
    pub calendar_enabled: bool,
    pub any_mic_activity: bool,
    pub auto_start_on_open_pane: bool,
    pub lead_seconds: i64,
    pub attendee_floor: usize,
    pub cross_link_window_ms: i64,
}

impl Default for DetectionPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            calendar_enabled: false,
            any_mic_activity: false,
            auto_start_on_open_pane: false,
            lead_seconds: CALENDAR_LEAD_SECONDS,
            attendee_floor: ATTENDEE_FLOOR,
            cross_link_window_ms: CROSS_LINK_WINDOW_MS,
        }
    }
}

/// Which prompt to raise, carrying the fields the copy pattern interpolates.
/// The frontend localizes from these fields; the native notification uses the
/// English copy pattern from §5.4 verbatim.
///
/// Exported to TypeScript as `DetectionPromptKind`: bare `PromptKind` would
/// land in bindings.ts beside `PromptPreset` and read as an LLM prompt, which
/// this is not. The rename is specta-only — serde never sees it, so it cannot
/// reach the wire.
///
/// The `rename_all` sits on each variant rather than on the enum. On an enum,
/// a container-level `rename_all` renames the *variants* and leaves their
/// fields alone — so writing it there emitted `{"kind":"appMeeting",
/// "app_name":…}` while the frontend read `{"kind":"AppMeeting","appName":…}`,
/// matched no arm, and rendered a prompt card with no title on it. Per-variant
/// is the placement that renames fields, which is what the rest of this
/// module's camelCase wire shape needs. Pinned by
/// `the_prompt_wire_shape_names_variants_and_camelcases_fields`; specta reads
/// the same per-variant attribute, so the generated union carries the same
/// camelCase fields the wire does.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind")]
#[specta(rename = "DetectionPromptKind")]
pub enum PromptKind {
    /// §5.3 case 3 — "{Event title} starting".
    #[serde(rename_all = "camelCase")]
    CalendarEvent {
        event_key: String,
        event_title: String,
    },
    /// §5.3 case 5 — "{App} meeting detected".
    #[serde(rename_all = "camelCase")]
    AppMeeting { bundle_id: String, app_name: String },
    /// §5.3 case 5, Slack flavor — "{App} huddle detected".
    #[serde(rename_all = "camelCase")]
    AppHuddle { bundle_id: String, app_name: String },
    /// §5.3 case 7 — "Call detected in {Browser}".
    #[serde(rename_all = "camelCase")]
    BrowserCall { bundle_id: String, app_name: String },
    /// A call app is in call — "{App} call detected".
    #[serde(rename_all = "camelCase")]
    AppCall { bundle_id: String, app_name: String },
    /// §5.3 case 6, behind the opt-in toggle — no app identity to name.
    UnknownMicSource,
}

impl PromptKind {
    /// The §5.4 copy pattern, in English. Native notifications are delivered by
    /// the OS from a Rust string, so they cannot go through the frontend's
    /// i18next catalog; the in-app pane renders localized copy from the fields.
    pub fn notification_title(&self) -> String {
        match self {
            Self::CalendarEvent { event_title, .. } => format!("{event_title} starting"),
            Self::AppMeeting { app_name, .. } => format!("{app_name} meeting detected"),
            Self::AppHuddle { app_name, .. } => format!("{app_name} huddle detected"),
            Self::BrowserCall { app_name, .. } => format!("Call detected in {app_name}"),
            Self::AppCall { app_name, .. } => format!("{app_name} call detected"),
            Self::UnknownMicSource => "Microphone activity detected".to_string(),
        }
    }

    /// Title proposed for the meeting the prompt would create.
    ///
    /// `now_local` is passed in rather than read here: this module decides, it
    /// never observes. A call is stamped with the time it started because a
    /// person has several a day and "FaceTime call" alone does not tell two of
    /// them apart, while every other kind already carries a distinguishing
    /// name.
    pub fn proposed_meeting_title(&self, now_local: DateTime<Local>) -> String {
        match self {
            Self::CalendarEvent { event_title, .. } => event_title.clone(),
            Self::AppMeeting { app_name, .. } | Self::AppHuddle { app_name, .. } => {
                format!("{app_name} meeting")
            }
            Self::AppCall { app_name, .. } => call_meeting_title(app_name, now_local),
            Self::BrowserCall { app_name, .. } => format!("Call in {app_name}"),
            Self::UnknownMicSource => crate::meeting::types::MANUAL_DEFAULT_TITLE.to_string(),
        }
    }

    /// Bundle ID of the app the prompt is about, when there is one.
    pub fn bundle_id(&self) -> Option<&str> {
        match self {
            Self::AppMeeting { bundle_id, .. }
            | Self::AppHuddle { bundle_id, .. }
            | Self::AppCall { bundle_id, .. }
            | Self::BrowserCall { bundle_id, .. } => Some(bundle_id),
            Self::CalendarEvent { .. } | Self::UnknownMicSource => None,
        }
    }
}

/// The name an auto-recorded call carries: "FaceTime call, 3:15 PM".
///
/// English, like `MANUAL_DEFAULT_TITLE`, because this is stored meeting data
/// rather than rendered copy — the operator can rename it, and the i18next
/// catalog is not reachable from the store.
pub fn call_meeting_title(app_name: &str, started_local: DateTime<Local>) -> String {
    format!("{app_name} call, {}", started_local.format("%-I:%M %p"))
}

/// Why detection stayed quiet. Carried into the status event so the operator can
/// see what detection is doing instead of guessing at silence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SuppressReason {
    /// Master toggle is off.
    DetectionDisabled,
    /// Sona's own microphone is what went active.
    SonaHoldsInputDevice,
    /// A capture is already running and nothing needs cross-linking.
    CaptureAlreadyActive,
    /// §5.3 case 4 — an app is open but nothing is listening yet.
    NoQualifyingSignal,
    /// §5.3 case 9 — solo focus block, or an event with no attendee list.
    AttendeeFloorNotMet,
    /// §5.3 case 6 — mic active with no identifiable meeting app.
    UnknownMicSource,
    /// §5.3 case 7b — a browser is in front but its title cannot be read.
    BrowserTitleUnreadable,
    /// §5.3 case 7 — a browser is in front and its title is not a meeting.
    BrowserTitleNotMeeting,
    /// A meeting app is running, but the operator has not been in front of it
    /// since the microphone went active. Presence is not participation.
    AppPresentNotInUse,
    /// The microphone Sona itself just used still reads as in use. Distinct
    /// from `SonaHoldsInputDevice`, which is the stream being open: this is
    /// the interval after it closed where the device has not caught up.
    SonaMicJustClosed,
}

/// The decision table's output. Exactly one of these per evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DetectionOutcome {
    Suppress(SuppressReason),
    /// §5.3 case 1 — countdown only, no capture and no notification.
    ///
    /// Carries the whole event rather than a copy of two of its fields: the
    /// pre-meeting card renders every fact the calendar supplied, and a
    /// flattened copy here would be a second place for those facts to drift.
    Countdown {
        event: CalendarEventSummary,
        seconds_to_start: i64,
    },
    /// §5.3 case 2 — a live standing series grant authorizes this occurrence.
    /// The session layer revalidates that grant atomically with the start.
    AutoStart {
        event_key: String,
        event_title: String,
    },
    /// A call app the operator's auto-record list names went in call. The
    /// session layer re-reads the list when it writes the receipt.
    AutoStartCall {
        bundle_id: String,
        app_name: String,
    },
    /// §5.3 cases 3, 5, 6, 7 — raise a notification and wait for a click.
    Prompt(PromptKind),
    /// §5.3 case 8 — attach this activity to the capture already open.
    CrossLink {
        session_id: String,
    },
}

/// The one entry point. Applies §5.3 in the table's own order.
pub fn evaluate(inputs: &DetectionInputs, policy: &DetectionPolicy) -> DetectionOutcome {
    if !policy.enabled {
        return DetectionOutcome::Suppress(SuppressReason::DetectionDisabled);
    }

    // Invariant 2. A capture in progress legitimately holds the device, so this
    // only fires for a dictation run or a stream Sona is keeping warm — never
    // mistake either for someone else's meeting.
    if inputs.self_holds_input_device && !inputs.capture_active {
        return DetectionOutcome::Suppress(SuppressReason::SonaHoldsInputDevice);
    }

    if inputs.capture_active {
        return cross_link_or_suppress(inputs, policy);
    }

    // The call path runs before the calendar because a call that is provably
    // live outranks an event that may not be happening, and because a call app
    // never carries a calendar event for it to displace. It falls through when
    // a call app is merely open, so a Zoom meeting beside an idle FaceTime is
    // still case 5.
    if let Some(outcome) = call_path(inputs) {
        return outcome;
    }

    // The calendar path runs before the ad-hoc one because a scheduled event is
    // stronger evidence than an app being open, and because case 1's countdown
    // must win over any ad-hoc prompt for the same moment.
    let calendar_reason = match calendar_path(inputs, policy) {
        CalendarPath::Decided(outcome) => return outcome,
        CalendarPath::Ineligible(reason) => reason,
    };

    let ad_hoc = ad_hoc_path(inputs, policy);
    // When the calendar actively declined and the ad-hoc path also stayed quiet,
    // report the calendar's reason. It is the one the operator cannot infer: an
    // event is what they expect detection to catch, and "solo event" explains the
    // silence where a generic ad-hoc reason does not.
    match (calendar_reason, ad_hoc) {
        (Some(reason), DetectionOutcome::Suppress(_)) => DetectionOutcome::Suppress(reason),
        (_, outcome) => outcome,
    }
}

/// Whether a call app is in a call right now.
///
/// **The rule.** A call app is in call when it is running **and frontmost**,
/// and either
///
/// * the default *input* device reports `IsRunningSomewhere` **and no other
///   allowlisted meeting app is running**, or
/// * the default *output* device reports it.
///
/// Both clauses are qualified because both properties are device-global. They
/// say that some process is holding a device; they never say which, and this
/// module does not guess.
///
/// **The input clause** is the same signal every other app uses, and it is the
/// one that under-reports: AirPods-class Bluetooth microphones frequently do
/// not raise it, which is why call apps need a second signal at all. Its own
/// qualifier is attribution — a running Zoom is a perfectly good explanation
/// for a live microphone, and preferring a backgrounded FaceTime over it would
/// trade a meeting Sona can detect for a call nobody is on. That qualifier
/// only names what the allowlist names, though: a Meet tab in a frontmost
/// browser, Discord, Voice Memos, and every other process that holds the
/// microphone is not an `AppSignal::Known`, and on the qualifier alone their
/// microphone would be handed to whichever call app happened to be open
/// behind them — a standing grant for FaceTime recording a call FaceTime
/// never carried. Frontmost is what closes that: a call app the operator is
/// not looking at explains nothing.
///
/// **The output clause's false-positive envelope** is music, a video, or any
/// other playback while the call app happens to be open — an idle FaceTime
/// plays nothing, but Spotify behind it plays plenty. The frontmost qualifier
/// is what bounds it: background audio while the operator works in another
/// window never reaches this rule, so the residue is "the operator is looking
/// at FaceTime, with no call, while audio plays". A prompt is the outcome there
/// unless they also granted standing consent for that app, and the recording
/// card offers Stop and a one-click revocation of the grant.
///
/// **Why frontmost rather than "recently launched".** Both bound the same
/// clauses. Frontmost is the stronger of the two for the case this exists for:
/// an inbound call — including an iPhone call relayed to the Mac — rings
/// through the output device while the call app sits in the background, and
/// answering it is what activates the app. Frontmost therefore declines to
/// record a ringing phone and starts the moment it is answered, while
/// "launched less than a tick ago" would record the ring and would also admit
/// the worst false positive on offer: an app launched at login with music
/// playing. Frontmost also needs no launch timestamp, which the app dimension
/// does not carry. What it costs is the call the operator answered and then
/// left in the background before a tick saw it; manual start stays primary.
///
/// Reads the signals rather than `DetectionInputs` because the tick needs
/// this answer before it has collected the rest of the inputs, and the two
/// callers must not derive it two ways.
pub fn call_is_live(
    call: &CallSignal,
    app: &AppSignal,
    mic: MicSignal,
    output: OutputSignal,
) -> bool {
    let CallSignal::Running {
        frontmost: true, ..
    } = call
    else {
        return false;
    };
    // A meeting app that is only open still explains the microphone here:
    // attribution to the call app is a guess either way, and a guess must not
    // become a recording under that app's standing grant.
    let other_meeting_app_running =
        matches!(app, AppSignal::Known { .. } | AppSignal::Present { .. });
    (mic == MicSignal::Active && !other_meeting_app_running) || output == OutputSignal::Active
}

/// The first live call seen by a hand-started capture. An adopted call is
/// stable for the rest of that capture, even when another call becomes live.
pub fn adopt_operator_call(
    capture: &OperatorCapture,
    call: &CallSignal,
    call_live: bool,
) -> Option<AdoptedCall> {
    if !matches!(capture, OperatorCapture::Waiting) || !call_live {
        return None;
    }
    let CallSignal::Running {
        bundle_id,
        display_name,
        ..
    } = call
    else {
        return None;
    };
    Some(AdoptedCall {
        bundle_id: bundle_id.clone(),
        display_name: display_name.clone(),
    })
}

/// Whether the evidence for a call that was already attributed is still
/// there. The boundary a claimed call and a pending call prompt live inside.
///
/// Looser than `call_is_live` on purpose. Attribution needs the call app in
/// front, because a backgrounded one explains nothing about a microphone the
/// allowlist cannot name. But once a call *has* been attributed, the operator
/// switching to Notes must not end it: the prompt would retract, the claim
/// would re-arm, and switching back would raise a second prompt for the same
/// call. So a held microphone keeps the boundary open whoever holds it, and
/// playback keeps it open only while the app is in front, since music behind
/// a backgrounded call app is not the call continuing.
pub fn call_evidence(call: &CallSignal, mic: MicSignal, output: OutputSignal) -> bool {
    let CallSignal::Running { frontmost, .. } = call else {
        return false;
    };
    mic == MicSignal::Active || (output == OutputSignal::Active && *frontmost)
}

/// The call dimension. `None` means it had nothing to say and the rest of the
/// table still applies.
fn call_path(inputs: &DetectionInputs) -> Option<DetectionOutcome> {
    let CallSignal::Running {
        bundle_id,
        display_name,
        ..
    } = &inputs.call
    else {
        return None;
    };
    if !call_is_live(&inputs.call, &inputs.app, inputs.mic, inputs.output) {
        return None;
    }
    if inputs.standing_app_consent {
        return Some(DetectionOutcome::AutoStartCall {
            bundle_id: bundle_id.clone(),
            app_name: display_name.clone(),
        });
    }
    Some(DetectionOutcome::Prompt(PromptKind::AppCall {
        bundle_id: bundle_id.clone(),
        app_name: display_name.clone(),
    }))
}

/// Outcome of consulting the calendar half of the table.
enum CalendarPath {
    Decided(DetectionOutcome),
    /// The calendar had nothing to say. `Some(reason)` when it actively declined
    /// and that reason is more informative than the ad-hoc path's silence.
    Ineligible(Option<SuppressReason>),
}

/// §5.3 cases 1, 2, 3, and 9.
fn calendar_path(inputs: &DetectionInputs, policy: &DetectionPolicy) -> CalendarPath {
    if !policy.calendar_enabled {
        return CalendarPath::Ineligible(None);
    }

    let (event, seconds_to_start) = match &inputs.calendar {
        CalendarSignal::Absent => return CalendarPath::Ineligible(None),
        CalendarSignal::Upcoming {
            event,
            seconds_to_start,
        } => (event, Some(*seconds_to_start)),
        CalendarSignal::Started { event } => (event, None),
    };

    // Case 9. A solo block never prompts from the calendar path, but it must not
    // shadow the ad-hoc path: a one-attendee event with Zoom open and the mic
    // live is still a meeting case 5 should catch.
    if event.attendee_count < policy.attendee_floor {
        return CalendarPath::Ineligible(Some(SuppressReason::AttendeeFloorNotMet));
    }

    match seconds_to_start {
        // Case 1. Countdown only, no capture.
        Some(seconds) if seconds <= policy.lead_seconds => {
            CalendarPath::Decided(DetectionOutcome::Countdown {
                event: event.clone(),
                seconds_to_start: seconds,
            })
        }
        // Further out than the lead window: nothing to show yet.
        Some(_) => CalendarPath::Ineligible(None),
        None => CalendarPath::Decided(started_event_outcome(inputs, policy, event)),
    }
}

/// §5.3 cases 2 and 3, for an event whose start instant has passed.
fn started_event_outcome(
    inputs: &DetectionInputs,
    _policy: &DetectionPolicy,
    event: &CalendarEventSummary,
) -> DetectionOutcome {
    if inputs.mic != MicSignal::Active {
        // Scheduled but nobody is talking yet. The countdown already told the
        // operator; a second prompt for an event that may not happen is noise.
        return DetectionOutcome::Suppress(SuppressReason::NoQualifyingSignal);
    }

    // Case 2. Detection can cite a standing series grant, but cannot construct
    // consent itself. The session manager revalidates the grant when it writes
    // the per-attempt receipt.
    if inputs.standing_series_consent {
        return DetectionOutcome::AutoStart {
            event_key: event.event_key.clone(),
            event_title: event.title.clone(),
        };
    }

    // Case 3. No standing grant: prompt, never silent-start.
    DetectionOutcome::Prompt(PromptKind::CalendarEvent {
        event_key: event.event_key.clone(),
        event_title: event.title.clone(),
    })
}

/// §5.3 cases 4, 5, 6, 7, and 7b, for activity with no matching event.
fn ad_hoc_path(inputs: &DetectionInputs, policy: &DetectionPolicy) -> DetectionOutcome {
    // Case 4. An open meeting app is not evidence on its own — otherwise every
    // trip to Zoom's settings raises a prompt.
    if inputs.mic != MicSignal::Active {
        return DetectionOutcome::Suppress(SuppressReason::NoQualifyingSignal);
    }

    match &inputs.app {
        // Case 5.
        AppSignal::Known {
            bundle_id,
            display_name,
            ..
        } => DetectionOutcome::Prompt(known_app_prompt(bundle_id, display_name)),
        // Cases 7 and 7b.
        AppSignal::Browser {
            bundle_id,
            display_name,
        } => browser_outcome(inputs, policy, bundle_id, display_name),
        // Case 6.
        AppSignal::Absent => any_mic_outcome(inputs, policy, SuppressReason::UnknownMicSource),
        // Presence is not participation: an app the operator has not touched
        // since the microphone went active explains nothing about it, so this
        // degrades to case 6's opt-in exactly as a browser's non-match does.
        AppSignal::Present { .. } => {
            any_mic_outcome(inputs, policy, SuppressReason::AppPresentNotInUse)
        }
    }
}

/// Slack's huddle is a mode inside the app rather than a separate process, so
/// the copy follows the bundle ID rather than any huddle-specific API.
fn known_app_prompt(bundle_id: &str, display_name: &str) -> PromptKind {
    if bundle_id.eq_ignore_ascii_case(SLACK_BUNDLE_ID) {
        return PromptKind::AppHuddle {
            bundle_id: bundle_id.to_string(),
            app_name: display_name.to_string(),
        };
    }
    PromptKind::AppMeeting {
        bundle_id: bundle_id.to_string(),
        app_name: display_name.to_string(),
    }
}

pub(crate) const SLACK_BUNDLE_ID: &str = "com.tinyspeck.slackmacgap";

/// §5.3 cases 7 and 7b. Both non-matching outcomes fall through to case 6's
/// opt-in toggle rather than hard-suppressing, which is what 7b prescribes: the
/// browser case degrades to "any mic activity" instead of blocking on a
/// permission request the core feature must never depend on.
///
/// The one permission the title reader needs is Accessibility, and the
/// evidence already says whether it had it: `Unreadable` is a reader that
/// could not look. Screen Recording is not consulted here at all. It gates
/// capturing system audio, not reading a window title, and a missing grant is
/// the consent panel's degraded-start caveat rather than a reason to stay
/// silent about a call that is plainly on screen.
fn browser_outcome(
    inputs: &DetectionInputs,
    policy: &DetectionPolicy,
    bundle_id: &str,
    display_name: &str,
) -> DetectionOutcome {
    match inputs.browser_title {
        BrowserTitleEvidence::MeetingMatch => DetectionOutcome::Prompt(PromptKind::BrowserCall {
            bundle_id: bundle_id.to_string(),
            app_name: display_name.to_string(),
        }),
        BrowserTitleEvidence::NoMatch => {
            any_mic_outcome(inputs, policy, SuppressReason::BrowserTitleNotMeeting)
        }
        BrowserTitleEvidence::Unreadable => {
            any_mic_outcome(inputs, policy, SuppressReason::BrowserTitleUnreadable)
        }
    }
}

/// The opt-in escape hatch. Off by default because voice memos, music
/// production, and every other audio app land here.
///
/// This is the one outcome in the table whose entire evidence is the device
/// reading, so it is the one the cooldown gates. A microphone Sona itself
/// just released is not an unknown source; it is a known one the property has
/// not caught up with. Every other arm above got here with something else to
/// go on — an event, an app in use, a matched tab, an output device — and
/// decides on that.
fn any_mic_outcome(
    inputs: &DetectionInputs,
    policy: &DetectionPolicy,
    reason: SuppressReason,
) -> DetectionOutcome {
    if inputs.self_mic_just_closed {
        return DetectionOutcome::Suppress(SuppressReason::SonaMicJustClosed);
    }
    if policy.any_mic_activity {
        return DetectionOutcome::Prompt(PromptKind::UnknownMicSource);
    }
    DetectionOutcome::Suppress(reason)
}

/// §5.3 case 8. New activity during a still-open capture joins that capture
/// rather than starting a second note.
fn cross_link_or_suppress(inputs: &DetectionInputs, policy: &DetectionPolicy) -> DetectionOutcome {
    if inputs.mic != MicSignal::Active {
        return DetectionOutcome::Suppress(SuppressReason::CaptureAlreadyActive);
    }
    let Some(recent) = &inputs.recent_capture else {
        return DetectionOutcome::Suppress(SuppressReason::CaptureAlreadyActive);
    };
    let elapsed = inputs.now_utc_ms.saturating_sub(recent.started_utc_ms);
    if !(0..=policy.cross_link_window_ms).contains(&elapsed) {
        return DetectionOutcome::Suppress(SuppressReason::CaptureAlreadyActive);
    }
    DetectionOutcome::CrossLink {
        session_id: recent.session_id.clone(),
    }
}

/// Everything §5.5's auto-stop heuristic reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StopInputs {
    pub now_utc_ms: i64,
    /// Scheduled end of the calendar event this capture is linked to.
    pub linked_event_end_utc_ms: Option<i64>,
    /// True while Sona's own capture holds the default input device. When it is
    /// true, the device-in-use property says nothing about the meeting app: Sona
    /// is the process keeping it raised.
    pub self_holds_input_device: bool,
    pub device_running_somewhere: bool,
    /// True when this capture records the default input device itself. The
    /// idle rule is about that device, so it applies only to a capture whose
    /// own inputs include it: a system-audio-only capture is not listening to
    /// the microphone, and nothing holding it says nothing about the meeting.
    pub microphone_lane: bool,
    /// What the default *output* device has done during this capture. Present
    /// only for a capture a call app triggered, where it is the one liveness
    /// signal Sona's own microphone does not pollute.
    pub call_output: Option<CallOutputWatch>,
    /// True while the process that triggered this capture is still running.
    pub trigger_app_running: bool,
    /// Set once the host crossed a sleep boundary during this capture.
    pub slept_since_start: bool,
}

/// How long the default output device must stay idle, after having played
/// during a call capture, before the silence reads as a hangup.
///
/// A hangup releases the device for good. A default-output change — AirPods
/// connecting mid-call — releases it for as long as the call's stream takes to
/// move to the new device, and the monitor re-registers on that device and
/// seeds the level from it before the stream has necessarily arrived. One
/// tick's reading is therefore not a hangup; ten seconds of readings is well
/// past any device swap and still short next to a tick.
pub const CALL_HANGUP_GRACE_MS: i64 = 10_000;

/// What the default output device has done since a call capture started,
/// folded in one tick at a time.
///
/// The level alone is not a hangup, for two reasons this carries. The output
/// is evidence only once it has played during this capture: a listener that
/// failed to register reads idle forever (`input_device` treats that as
/// degradation, not failure), and a call routed to a device other than the
/// default never touches it, so a capture the microphone clause admitted
/// would otherwise be stopped by a signal that was never watching it. And an
/// idle reading is a hangup only once it has lasted `CALL_HANGUP_GRACE_MS`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CallOutputWatch {
    played: bool,
    idle_since_utc_ms: Option<i64>,
}

impl CallOutputWatch {
    pub fn observe(&mut self, output: OutputSignal, now_utc_ms: i64) {
        match output {
            OutputSignal::Active => {
                self.played = true;
                self.idle_since_utc_ms = None;
            }
            OutputSignal::Idle => {
                if self.played && self.idle_since_utc_ms.is_none() {
                    self.idle_since_utc_ms = Some(now_utc_ms);
                }
            }
        }
    }

    /// True once the output has played and then stayed idle for the grace.
    pub fn hung_up(&self, now_utc_ms: i64) -> bool {
        self.idle_since_utc_ms
            .is_some_and(|since| now_utc_ms.saturating_sub(since) >= CALL_HANGUP_GRACE_MS)
    }
}

/// Why a capture ended by itself. Manual stop is not in this list: it stays the
/// primary path and does not go through the heuristic at all.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum StopTrigger {
    /// The host crossed a sleep boundary.
    SleepBoundary,
    /// The linked calendar event ended.
    EventEnd,
    /// The application that triggered this capture quit.
    TriggerAppExited,
    /// The default input device became idle while Sona was not holding it.
    InputDeviceIdle,
    /// A call capture's default output stayed idle after it had played.
    CallEnded,
}

impl StopTrigger {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SleepBoundary => "sleep_boundary",
            Self::EventEnd => "event_end",
            Self::TriggerAppExited => "trigger_app_exited",
            Self::InputDeviceIdle => "input_device_idle",
            Self::CallEnded => "call_ended",
        }
    }
}

/// Observable auto-stop causes, in precedence order.
pub fn evaluate_stop(inputs: &StopInputs) -> Option<StopTrigger> {
    if inputs.slept_since_start {
        return Some(StopTrigger::SleepBoundary);
    }
    if inputs
        .linked_event_end_utc_ms
        .is_some_and(|end| inputs.now_utc_ms >= end)
    {
        return Some(StopTrigger::EventEnd);
    }
    if !inputs.trigger_app_running {
        return Some(StopTrigger::TriggerAppExited);
    }
    // Before the input-device rule, which cannot fire during a Sona capture:
    // Sona's own microphone is what keeps that device raised. The output
    // device is the call's own, and hanging up releases it.
    if inputs
        .call_output
        .is_some_and(|output| output.hung_up(inputs.now_utc_ms))
    {
        return Some(StopTrigger::CallEnded);
    }
    // §5.5 condition 3 proper, scoped to a capture that is listening to the
    // device at all. Sona's own microphone lane keeps it raised, so a false
    // reading here means that lane is gone; a capture with no such lane has
    // nothing to read.
    if inputs.microphone_lane && !inputs.self_holds_input_device && !inputs.device_running_somewhere
    {
        return Some(StopTrigger::InputDeviceIdle);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const NOW: i64 = 1_700_000_000_000;

    fn event(attendee_count: usize) -> CalendarEventSummary {
        CalendarEventSummary {
            event_key: "event-1".to_string(),
            series_key: "series-1".to_string(),
            title: "Quarterly planning".to_string(),
            attendee_count,
            start_utc_ms: NOW,
            end_utc_ms: NOW + 30 * 60_000,
            attendees: Vec::new(),
            notes: None,
            calendar_name: None,
            url: None,
        }
    }

    fn inputs() -> DetectionInputs {
        DetectionInputs {
            now_utc_ms: NOW,
            calendar: CalendarSignal::Absent,
            app: AppSignal::Absent,
            call: CallSignal::Absent,
            mic: MicSignal::Idle,
            output: OutputSignal::Idle,
            browser_title: BrowserTitleEvidence::Unreadable,
            standing_series_consent: false,
            standing_app_consent: false,
            recent_capture: None,
            self_holds_input_device: false,
            self_mic_just_closed: false,
            capture_active: false,
        }
    }

    fn facetime(frontmost: bool) -> CallSignal {
        CallSignal::Running {
            bundle_id: "com.apple.facetime".to_string(),
            display_name: "FaceTime".to_string(),
            frontmost,
        }
    }

    /// A live FaceTime call the operator granted standing consent to, detected
    /// the way an AirPods call actually is: the microphone property never rises,
    /// only the output device does.
    fn granted_call_on_output_only() -> DetectionInputs {
        DetectionInputs {
            call: facetime(true),
            output: OutputSignal::Active,
            standing_app_consent: true,
            ..inputs()
        }
    }

    fn calendar_policy() -> DetectionPolicy {
        DetectionPolicy {
            calendar_enabled: true,
            ..DetectionPolicy::default()
        }
    }

    fn zoom() -> AppSignal {
        AppSignal::Known {
            bundle_id: "us.zoom.xos".to_string(),
            display_name: "Zoom".to_string(),
            frontmost: true,
        }
    }

    /// Zoom running, and not in front since the microphone went active.
    fn zoom_present() -> AppSignal {
        AppSignal::Present {
            bundle_id: "us.zoom.xos".to_string(),
            display_name: "Zoom".to_string(),
        }
    }

    fn chrome() -> AppSignal {
        AppSignal::Browser {
            bundle_id: "com.google.chrome".to_string(),
            display_name: "Chrome".to_string(),
        }
    }

    /* The frontend mirrors this enum by hand in
     * src/components/settings/meetings/detectionStore.ts, because the prompt
     * leaves through a raw `app.emit` rather than a tauri_specta Event and so
     * never reaches bindings.ts. That hand mirror is only as good as this
     * shape, and when the two drifted the pane rendered prompt cards with no
     * title on them — a card offering to record something it could not name.
     * Pinning the bytes is what makes the next drift a test failure. */
    #[test]
    fn the_prompt_wire_shape_names_variants_and_camelcases_fields() {
        let wire = |prompt: &PromptKind| serde_json::to_value(prompt).expect("prompt serializes");

        assert_eq!(
            wire(&PromptKind::CalendarEvent {
                event_key: "event-1".to_string(),
                event_title: "Quarterly planning".to_string(),
            }),
            serde_json::json!({
                "kind": "CalendarEvent",
                "eventKey": "event-1",
                "eventTitle": "Quarterly planning",
            })
        );
        assert_eq!(
            wire(&PromptKind::AppMeeting {
                bundle_id: "us.zoom.xos".to_string(),
                app_name: "Zoom".to_string(),
            }),
            serde_json::json!({
                "kind": "AppMeeting",
                "bundleId": "us.zoom.xos",
                "appName": "Zoom",
            })
        );
        assert_eq!(
            wire(&PromptKind::AppHuddle {
                bundle_id: "com.tinyspeck.slackmacgap".to_string(),
                app_name: "Slack".to_string(),
            }),
            serde_json::json!({
                "kind": "AppHuddle",
                "bundleId": "com.tinyspeck.slackmacgap",
                "appName": "Slack",
            })
        );
        assert_eq!(
            wire(&PromptKind::BrowserCall {
                bundle_id: "com.google.chrome".to_string(),
                app_name: "Chrome".to_string(),
            }),
            serde_json::json!({
                "kind": "BrowserCall",
                "bundleId": "com.google.chrome",
                "appName": "Chrome",
            })
        );
        assert_eq!(
            wire(&PromptKind::UnknownMicSource),
            serde_json::json!({ "kind": "UnknownMicSource" })
        );
    }

    /* §5.3 case 1 — event within T-60s, >=2 attendees: countdown, no capture. */
    #[test]
    fn case_1_upcoming_event_shows_countdown_without_capture() {
        let outcome = evaluate(
            &DetectionInputs {
                calendar: CalendarSignal::Upcoming {
                    event: event(4),
                    seconds_to_start: 45,
                },
                ..inputs()
            },
            &calendar_policy(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Countdown {
                event: event(4),
                seconds_to_start: 45,
            }
        );
    }

    #[test]
    fn case_1_event_beyond_lead_window_stays_quiet() {
        let outcome = evaluate(
            &DetectionInputs {
                calendar: CalendarSignal::Upcoming {
                    event: event(4),
                    seconds_to_start: 61,
                },
                ..inputs()
            },
            &calendar_policy(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Suppress(SuppressReason::NoQualifyingSignal)
        );
    }

    /* §5.3 case 2 — a live standing grant, not a visible pane or setting,
     * authorizes the recurring occurrence. */
    #[test]
    fn case_2_standing_series_consent_auto_starts() {
        let outcome = evaluate(
            &DetectionInputs {
                calendar: CalendarSignal::Started { event: event(3) },
                mic: MicSignal::Active,
                standing_series_consent: true,
                ..inputs()
            },
            &calendar_policy(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::AutoStart {
                event_key: "event-1".to_string(),
                event_title: "Quarterly planning".to_string(),
            }
        );
    }

    #[test]
    fn open_pane_setting_without_standing_consent_still_prompts() {
        let outcome = evaluate(
            &DetectionInputs {
                calendar: CalendarSignal::Started { event: event(3) },
                mic: MicSignal::Active,
                ..inputs()
            },
            &DetectionPolicy {
                auto_start_on_open_pane: true,
                ..calendar_policy()
            },
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Prompt(PromptKind::CalendarEvent {
                event_key: "event-1".to_string(),
                event_title: "Quarterly planning".to_string(),
            })
        );
    }

    /* §5.3 case 3 — start reached, pane not open, mic live: prompt. */
    #[test]
    fn case_3_started_event_without_open_pane_prompts() {
        let outcome = evaluate(
            &DetectionInputs {
                calendar: CalendarSignal::Started { event: event(2) },
                app: zoom(),
                mic: MicSignal::Active,
                ..inputs()
            },
            &calendar_policy(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Prompt(PromptKind::CalendarEvent {
                event_key: "event-1".to_string(),
                event_title: "Quarterly planning".to_string(),
            })
        );
    }

    #[test]
    fn case_3_prompt_title_uses_the_event_copy_pattern() {
        let prompt = PromptKind::CalendarEvent {
            event_key: "event-1".to_string(),
            event_title: "Quarterly planning".to_string(),
        };

        assert_eq!(prompt.notification_title(), "Quarterly planning starting");
    }

    /* §5.3 case 4 — meeting app open, microphone idle: nothing. */
    #[test]
    fn case_4_app_open_without_mic_activity_never_prompts() {
        let outcome = evaluate(
            &DetectionInputs {
                app: zoom(),
                ..inputs()
            },
            &DetectionPolicy::default(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Suppress(SuppressReason::NoQualifyingSignal)
        );
    }

    /* §5.3 case 5 — allowlisted app plus live mic, no event: ad-hoc prompt. */
    #[test]
    fn case_5_known_app_with_live_mic_prompts_with_app_copy() {
        let outcome = evaluate(
            &DetectionInputs {
                app: zoom(),
                mic: MicSignal::Active,
                ..inputs()
            },
            &DetectionPolicy::default(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Prompt(PromptKind::AppMeeting {
                bundle_id: "us.zoom.xos".to_string(),
                app_name: "Zoom".to_string(),
            })
        );
        assert_eq!(
            PromptKind::AppMeeting {
                bundle_id: "us.zoom.xos".to_string(),
                app_name: "Zoom".to_string(),
            }
            .notification_title(),
            "Zoom meeting detected"
        );
    }

    #[test]
    fn case_5_slack_takes_the_huddle_copy() {
        let outcome = evaluate(
            &DetectionInputs {
                app: AppSignal::Known {
                    bundle_id: SLACK_BUNDLE_ID.to_string(),
                    display_name: "Slack".to_string(),
                    frontmost: true,
                },
                mic: MicSignal::Active,
                ..inputs()
            },
            &DetectionPolicy::default(),
        );

        let DetectionOutcome::Prompt(prompt) = outcome else {
            panic!("Slack with a live mic should prompt");
        };
        assert_eq!(prompt.notification_title(), "Slack huddle detected");
    }

    /* Presence is not participation. Zoom in the menu bar while a voice memo
     * records used to be "Zoom meeting detected"; now it is a named silence,
     * and the any-mic opt-in is the only way through, as for case 6. */
    #[test]
    fn case_5_a_meeting_app_that_is_only_open_never_prompts() {
        let voice_memo = DetectionInputs {
            app: zoom_present(),
            mic: MicSignal::Active,
            ..inputs()
        };
        let any_mic = DetectionPolicy {
            any_mic_activity: true,
            ..DetectionPolicy::default()
        };

        assert_eq!(
            evaluate(&voice_memo, &DetectionPolicy::default()),
            DetectionOutcome::Suppress(SuppressReason::AppPresentNotInUse)
        );
        assert_eq!(
            evaluate(&voice_memo, &any_mic),
            DetectionOutcome::Prompt(PromptKind::UnknownMicSource)
        );
    }

    /* The call path's microphone clause treats an open meeting app as the
     * likelier holder, used or not: a FaceTime grant recording a Zoom meeting
     * nobody consented to is the failure that clause exists to block. */
    #[test]
    fn a_meeting_app_that_is_only_open_still_explains_the_microphone_to_the_call_path() {
        let facetime_in_front = DetectionInputs {
            call: facetime(true),
            app: zoom_present(),
            mic: MicSignal::Active,
            standing_app_consent: true,
            ..inputs()
        };

        assert_eq!(
            evaluate(&facetime_in_front, &DetectionPolicy::default()),
            DetectionOutcome::Suppress(SuppressReason::AppPresentNotInUse)
        );
    }

    /* §5.3 case 6 — live mic, no identifiable app: suppressed unless opted in. */
    #[test]
    fn case_6_unknown_mic_source_is_suppressed_by_default() {
        assert!(!DetectionPolicy::default().any_mic_activity);

        let outcome = evaluate(
            &DetectionInputs {
                mic: MicSignal::Active,
                ..inputs()
            },
            &DetectionPolicy::default(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Suppress(SuppressReason::UnknownMicSource)
        );
    }

    #[test]
    fn case_6_any_mic_activity_toggle_opens_the_unknown_path() {
        let outcome = evaluate(
            &DetectionInputs {
                mic: MicSignal::Active,
                ..inputs()
            },
            &DetectionPolicy {
                any_mic_activity: true,
                ..DetectionPolicy::default()
            },
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Prompt(PromptKind::UnknownMicSource)
        );
    }

    /* The bug the cooldown exists for. The operator dictates with a shortcut,
     * lets go, and the stream closes; CoreAudio's device-global property has
     * not caught up, so the microphone still reads as in use with nothing to
     * explain it. On the opt-in path that was a "microphone activity
     * detected" prompt for Sona's own dictation. */
    #[test]
    fn a_dictation_that_just_ended_does_not_prompt_for_its_own_microphone() {
        let just_stopped = DetectionInputs {
            mic: MicSignal::Active,
            self_mic_just_closed: true,
            ..inputs()
        };

        assert_eq!(
            evaluate(
                &just_stopped,
                &DetectionPolicy {
                    any_mic_activity: true,
                    ..DetectionPolicy::default()
                }
            ),
            DetectionOutcome::Suppress(SuppressReason::SonaMicJustClosed)
        );
    }

    /* The cooldown silences one path: the opt-in escape hatch, whose only
     * evidence is the device reading it can no longer trust. Everything with
     * evidence of its own decides on that evidence, cooldown or not. */
    #[test]
    fn the_cooldown_leaves_every_path_that_has_its_own_evidence_alone() {
        let cooling = DetectionInputs {
            mic: MicSignal::Active,
            self_mic_just_closed: true,
            ..inputs()
        };
        let policy = DetectionPolicy {
            any_mic_activity: true,
            ..calendar_policy()
        };

        assert_eq!(
            evaluate(
                &DetectionInputs {
                    calendar: CalendarSignal::Started { event: event(3) },
                    ..cooling.clone()
                },
                &policy
            ),
            DetectionOutcome::Prompt(PromptKind::CalendarEvent {
                event_key: "event-1".to_string(),
                event_title: "Quarterly planning".to_string(),
            }),
            "a scheduled event is evidence the device reading is not"
        );

        assert_eq!(
            evaluate(
                &DetectionInputs {
                    app: zoom(),
                    ..cooling.clone()
                },
                &policy
            ),
            DetectionOutcome::Prompt(PromptKind::AppMeeting {
                bundle_id: "us.zoom.xos".to_string(),
                app_name: "Zoom".to_string(),
            }),
            "a meeting app the operator is using explains the microphone"
        );

        assert_eq!(
            evaluate(
                &DetectionInputs {
                    call: facetime(true),
                    output: OutputSignal::Active,
                    ..cooling.clone()
                },
                &policy
            ),
            DetectionOutcome::Prompt(PromptKind::AppCall {
                bundle_id: "com.apple.facetime".to_string(),
                app_name: "FaceTime".to_string(),
            }),
            "a call carries the output device, which Sona's capture never raises"
        );

        assert_eq!(
            evaluate(
                &DetectionInputs {
                    app: chrome(),
                    browser_title: BrowserTitleEvidence::MeetingMatch,
                    ..cooling
                },
                &policy
            ),
            DetectionOutcome::Prompt(PromptKind::BrowserCall {
                bundle_id: "com.google.chrome".to_string(),
                app_name: "Chrome".to_string(),
            }),
            "a matched tab title names the call on screen"
        );
    }

    /* §5.3 case 7 — browser plus a readable meeting title. FN3 in the
     * detection map: the reader needs Accessibility, and this used to be
     * discarded whenever Screen Recording was missing, a grant that has nothing
     * to do with reading a window title. No Screen Recording input exists any
     * more, so a match is a prompt, full stop. */
    #[test]
    fn case_7_a_meeting_title_prompts_whatever_screen_recording_says() {
        let outcome = evaluate(
            &DetectionInputs {
                app: chrome(),
                mic: MicSignal::Active,
                browser_title: BrowserTitleEvidence::MeetingMatch,
                ..inputs()
            },
            &DetectionPolicy::default(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Prompt(PromptKind::BrowserCall {
                bundle_id: "com.google.chrome".to_string(),
                app_name: "Chrome".to_string(),
            })
        );
        assert_eq!(
            PromptKind::BrowserCall {
                bundle_id: "com.google.chrome".to_string(),
                app_name: "Chrome".to_string(),
            }
            .notification_title(),
            "Call detected in Chrome"
        );
    }

    #[test]
    fn case_7_browser_without_a_meeting_title_is_suppressed() {
        let outcome = evaluate(
            &DetectionInputs {
                app: chrome(),
                mic: MicSignal::Active,
                browser_title: BrowserTitleEvidence::NoMatch,
                ..inputs()
            },
            &DetectionPolicy::default(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Suppress(SuppressReason::BrowserTitleNotMeeting)
        );
    }

    /* §5.3 case 7b — same as 7 with Accessibility not trusted. */
    #[test]
    fn case_7b_unreadable_browser_title_suppresses_instead_of_guessing() {
        let outcome = evaluate(
            &DetectionInputs {
                app: chrome(),
                mic: MicSignal::Active,
                browser_title: BrowserTitleEvidence::Unreadable,
                ..inputs()
            },
            &DetectionPolicy::default(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Suppress(SuppressReason::BrowserTitleUnreadable)
        );
    }

    #[test]
    fn case_7b_falls_back_to_the_any_mic_toggle_rather_than_a_permission_request() {
        let outcome = evaluate(
            &DetectionInputs {
                app: chrome(),
                mic: MicSignal::Active,
                browser_title: BrowserTitleEvidence::Unreadable,
                ..inputs()
            },
            &DetectionPolicy {
                any_mic_activity: true,
                ..DetectionPolicy::default()
            },
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Prompt(PromptKind::UnknownMicSource)
        );
    }

    /* §5.3 case 8 — new activity inside the cross-link window joins the open note. */
    #[test]
    fn case_8_activity_during_an_open_capture_cross_links() {
        let outcome = evaluate(
            &DetectionInputs {
                mic: MicSignal::Active,
                capture_active: true,
                self_holds_input_device: true,
                recent_capture: Some(RecentCapture {
                    session_id: "session-1".to_string(),
                    started_utc_ms: NOW - 5 * 60_000,
                }),
                ..inputs()
            },
            &DetectionPolicy::default(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::CrossLink {
                session_id: "session-1".to_string(),
            }
        );
    }

    #[test]
    fn case_8_expires_after_the_cross_link_window() {
        let outcome = evaluate(
            &DetectionInputs {
                mic: MicSignal::Active,
                capture_active: true,
                recent_capture: Some(RecentCapture {
                    session_id: "session-1".to_string(),
                    started_utc_ms: NOW - CROSS_LINK_WINDOW_MS - 1,
                }),
                ..inputs()
            },
            &DetectionPolicy::default(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Suppress(SuppressReason::CaptureAlreadyActive)
        );
    }

    /* §5.3 case 9 — the attendee floor. */
    #[test]
    fn case_9_solo_event_never_prompts_from_the_calendar_path() {
        for attendee_count in [0, 1] {
            let outcome = evaluate(
                &DetectionInputs {
                    calendar: CalendarSignal::Started {
                        event: event(attendee_count),
                    },
                    mic: MicSignal::Active,
                    ..inputs()
                },
                &calendar_policy(),
            );

            assert_eq!(
                outcome,
                DetectionOutcome::Suppress(SuppressReason::AttendeeFloorNotMet),
                "an event with {attendee_count} attendees must not prompt"
            );
        }
    }

    #[test]
    fn case_9_solo_event_does_not_shadow_the_ad_hoc_path() {
        let outcome = evaluate(
            &DetectionInputs {
                calendar: CalendarSignal::Started { event: event(1) },
                app: zoom(),
                mic: MicSignal::Active,
                ..inputs()
            },
            &calendar_policy(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Prompt(PromptKind::AppMeeting {
                bundle_id: "us.zoom.xos".to_string(),
                app_name: "Zoom".to_string(),
            })
        );
    }

    #[test]
    fn case_9_upcoming_solo_event_shows_no_countdown() {
        let outcome = evaluate(
            &DetectionInputs {
                calendar: CalendarSignal::Upcoming {
                    event: event(1),
                    seconds_to_start: 30,
                },
                ..inputs()
            },
            &calendar_policy(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Suppress(SuppressReason::AttendeeFloorNotMet)
        );
    }

    /* §5.3 case 10 — Screen Recording must not gate cases 1-5. It is no longer
     * an input to this table at all, so nothing here can depend on it. */

    /* Suppress paths that are not decision-table rows. */
    #[test]
    fn master_toggle_off_suppresses_every_signal() {
        let outcome = evaluate(
            &DetectionInputs {
                calendar: CalendarSignal::Started { event: event(9) },
                app: zoom(),
                mic: MicSignal::Active,
                browser_title: BrowserTitleEvidence::MeetingMatch,
                ..inputs()
            },
            &DetectionPolicy {
                enabled: false,
                ..calendar_policy()
            },
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Suppress(SuppressReason::DetectionDisabled)
        );
    }

    #[test]
    fn sonas_own_dictation_is_never_read_as_a_meeting() {
        let outcome = evaluate(
            &DetectionInputs {
                app: zoom(),
                mic: MicSignal::Active,
                self_holds_input_device: true,
                ..inputs()
            },
            &DetectionPolicy {
                any_mic_activity: true,
                ..DetectionPolicy::default()
            },
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Suppress(SuppressReason::SonaHoldsInputDevice)
        );
    }

    #[test]
    fn calendar_sub_toggle_off_ignores_calendar_evidence() {
        let outcome = evaluate(
            &DetectionInputs {
                calendar: CalendarSignal::Upcoming {
                    event: event(5),
                    seconds_to_start: 10,
                },
                ..inputs()
            },
            &DetectionPolicy::default(),
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Suppress(SuppressReason::NoQualifyingSignal)
        );
    }

    #[test]
    fn an_open_capture_without_recent_context_stays_quiet() {
        let outcome = evaluate(
            &DetectionInputs {
                mic: MicSignal::Active,
                capture_active: true,
                ..inputs()
            },
            &DetectionPolicy {
                any_mic_activity: true,
                ..DetectionPolicy::default()
            },
        );

        assert_eq!(
            outcome,
            DetectionOutcome::Suppress(SuppressReason::CaptureAlreadyActive)
        );
    }

    /* §5.5 auto-stop. */
    fn stop_inputs() -> StopInputs {
        StopInputs {
            now_utc_ms: NOW,
            linked_event_end_utc_ms: None,
            self_holds_input_device: true,
            device_running_somewhere: true,
            microphone_lane: true,
            call_output: None,
            trigger_app_running: true,
            slept_since_start: false,
        }
    }

    #[test]
    fn a_healthy_capture_has_no_stop_trigger() {
        assert_eq!(evaluate_stop(&stop_inputs()), None);
    }

    #[test]
    fn scheduled_event_end_stops_a_linked_capture() {
        let trigger = evaluate_stop(&StopInputs {
            linked_event_end_utc_ms: Some(NOW),
            ..stop_inputs()
        });

        assert_eq!(trigger, Some(StopTrigger::EventEnd));
    }

    #[test]
    fn the_device_going_idle_only_counts_when_sona_is_not_holding_it() {
        assert_eq!(
            evaluate_stop(&StopInputs {
                self_holds_input_device: true,
                device_running_somewhere: false,
                ..stop_inputs()
            }),
            None
        );
        assert_eq!(
            evaluate_stop(&StopInputs {
                self_holds_input_device: false,
                device_running_somewhere: false,
                ..stop_inputs()
            }),
            Some(StopTrigger::InputDeviceIdle)
        );
    }

    #[test]
    fn a_system_audio_only_capture_survives_an_idle_input_device() {
        assert_eq!(
            evaluate_stop(&StopInputs {
                microphone_lane: false,
                self_holds_input_device: false,
                device_running_somewhere: false,
                ..stop_inputs()
            }),
            None
        );
    }

    #[test]
    fn the_triggering_app_quitting_stops_the_capture() {
        let trigger = evaluate_stop(&StopInputs {
            trigger_app_running: false,
            ..stop_inputs()
        });

        assert_eq!(trigger, Some(StopTrigger::TriggerAppExited));
    }

    #[test]
    fn a_sleep_boundary_outranks_every_other_trigger() {
        let trigger = evaluate_stop(&StopInputs {
            slept_since_start: true,
            linked_event_end_utc_ms: Some(NOW),
            trigger_app_running: false,
            ..stop_inputs()
        });

        assert_eq!(trigger, Some(StopTrigger::SleepBoundary));
    }

    #[test]
    fn operator_capture_adopts_a_live_call_on_first_tick() {
        let call = facetime(true);
        let call_live = call_is_live(
            &call,
            &AppSignal::Absent,
            MicSignal::Idle,
            OutputSignal::Active,
        );

        assert_eq!(
            adopt_operator_call(&OperatorCapture::Waiting, &call, call_live),
            Some(AdoptedCall {
                bundle_id: "com.apple.facetime".to_string(),
                display_name: "FaceTime".to_string(),
            })
        );
    }

    #[test]
    fn operator_capture_adopts_first_later_call_once() {
        let mut capture = OperatorCapture::Waiting;
        assert_eq!(
            adopt_operator_call(&capture, &CallSignal::Absent, false),
            None
        );

        capture = OperatorCapture::Adopted {
            call: adopt_operator_call(&capture, &facetime(true), true)
                .expect("the first live call is adopted"),
            watch: CallOutputWatch::default(),
        };
        let later_call = CallSignal::Running {
            bundle_id: "com.apple.mobilephone".to_string(),
            display_name: "Phone".to_string(),
            frontmost: true,
        };

        assert_eq!(adopt_operator_call(&capture, &later_call, true), None);
        assert!(matches!(
            capture,
            OperatorCapture::Adopted {
                call: AdoptedCall { ref bundle_id, .. },
                ..
            } if bundle_id == "com.apple.facetime"
        ));
    }

    #[test]
    fn evaluate_stop_precedence_favors_the_earliest_trigger() {
        let mut watch = CallOutputWatch::default();
        watch.observe(OutputSignal::Active, NOW);
        watch.observe(OutputSignal::Idle, NOW + 1_000);
        let hung_up = StopInputs {
            now_utc_ms: NOW + 1_000 + CALL_HANGUP_GRACE_MS,
            linked_event_end_utc_ms: None,
            self_holds_input_device: false,
            device_running_somewhere: false,
            microphone_lane: false,
            call_output: Some(watch),
            trigger_app_running: true,
            slept_since_start: false,
        };

        assert_eq!(
            evaluate_stop(&StopInputs {
                slept_since_start: true,
                linked_event_end_utc_ms: Some(NOW),
                trigger_app_running: false,
                ..hung_up
            }),
            Some(StopTrigger::SleepBoundary)
        );
        assert_eq!(
            evaluate_stop(&StopInputs {
                linked_event_end_utc_ms: Some(NOW),
                trigger_app_running: false,
                ..hung_up
            }),
            Some(StopTrigger::EventEnd)
        );
        assert_eq!(
            evaluate_stop(&StopInputs {
                trigger_app_running: false,
                ..hung_up
            }),
            Some(StopTrigger::TriggerAppExited)
        );
        assert_eq!(evaluate_stop(&hung_up), Some(StopTrigger::CallEnded));
    }

    #[test]
    fn operator_capture_does_not_adopt_a_background_call() {
        let call = facetime(false);
        let call_live = call_is_live(
            &call,
            &AppSignal::Absent,
            MicSignal::Idle,
            OutputSignal::Active,
        );

        assert!(!call_live);
        assert_eq!(
            adopt_operator_call(&OperatorCapture::Waiting, &call, call_live),
            None
        );
    }

    #[test]
    fn a_call_is_live_on_the_output_signal_alone() {
        let outcome = evaluate(&granted_call_on_output_only(), &DetectionPolicy::default());

        assert_eq!(
            outcome,
            DetectionOutcome::AutoStartCall {
                bundle_id: "com.apple.facetime".to_string(),
                app_name: "FaceTime".to_string(),
            }
        );
    }

    /* The output clause's whole false-positive envelope, and its bound: music
     * behind a call app is audio on the same device, so the only thing keeping
     * Spotify from starting a recording is that FaceTime is not in front. */
    #[test]
    fn playback_behind_a_backgrounded_call_app_is_not_a_call() {
        let background = DetectionInputs {
            call: facetime(false),
            ..granted_call_on_output_only()
        };

        assert_eq!(
            evaluate(&background, &DetectionPolicy::default()),
            DetectionOutcome::Suppress(SuppressReason::NoQualifyingSignal)
        );
    }

    #[test]
    fn a_call_app_with_neither_signal_is_not_a_call() {
        let idle = DetectionInputs {
            call: facetime(true),
            standing_app_consent: true,
            ..inputs()
        };

        assert_eq!(
            evaluate(&idle, &DetectionPolicy::default()),
            DetectionOutcome::Suppress(SuppressReason::NoQualifyingSignal)
        );
    }

    /* The wired case: the microphone property rises, the output has not been
     * seen yet, and the operator is looking at the call. */
    #[test]
    fn a_bluetooth_headset_still_leaves_the_microphone_clause_working() {
        let wired = DetectionInputs {
            call: facetime(true),
            mic: MicSignal::Active,
            standing_app_consent: true,
            ..inputs()
        };

        assert_eq!(
            evaluate(&wired, &DetectionPolicy::default()),
            DetectionOutcome::AutoStartCall {
                bundle_id: "com.apple.facetime".to_string(),
                app_name: "FaceTime".to_string(),
            }
        );
    }

    /* The input property is device-global and the allowlist cannot name every
     * process that raises it. A Meet tab in a frontmost browser does, with
     * FaceTime idle behind it; a standing grant for FaceTime must not turn
     * that into a recording of somebody else's call. */
    #[test]
    fn a_backgrounded_call_app_does_not_claim_a_browser_calls_microphone() {
        let meet_in_chrome = DetectionInputs {
            call: facetime(false),
            app: chrome(),
            mic: MicSignal::Active,
            browser_title: BrowserTitleEvidence::MeetingMatch,
            standing_app_consent: true,
            ..inputs()
        };

        assert_eq!(
            evaluate(&meet_in_chrome, &DetectionPolicy::default()),
            DetectionOutcome::Prompt(PromptKind::BrowserCall {
                bundle_id: "com.google.chrome".to_string(),
                app_name: "Chrome".to_string(),
            })
        );
    }

    /* Discord, Voice Memos, a DAW: case 6 owns an unattributed microphone, and
     * its opt-in must keep owning it with a call app open in the background.
     * Off, the episode is suppressed; on, it prompts as the unknown source it
     * is, never as a call. */
    #[test]
    fn a_backgrounded_call_app_does_not_claim_an_unattributed_microphone() {
        let voice_memo = DetectionInputs {
            call: facetime(false),
            mic: MicSignal::Active,
            standing_app_consent: true,
            ..inputs()
        };
        let any_mic = DetectionPolicy {
            any_mic_activity: true,
            ..DetectionPolicy::default()
        };

        assert_eq!(
            evaluate(&voice_memo, &DetectionPolicy::default()),
            DetectionOutcome::Suppress(SuppressReason::UnknownMicSource)
        );
        assert_eq!(
            evaluate(&voice_memo, &any_mic),
            DetectionOutcome::Prompt(PromptKind::UnknownMicSource)
        );
    }

    /* The call path runs ahead of the calendar path, so a backgrounded call
     * app that counted as live would consume a started event's decision too:
     * a series with standing consent would record nothing. */
    #[test]
    fn a_backgrounded_call_app_does_not_displace_a_started_event() {
        let scheduled = DetectionInputs {
            calendar: CalendarSignal::Started { event: event(3) },
            call: facetime(false),
            mic: MicSignal::Active,
            standing_series_consent: true,
            standing_app_consent: true,
            ..inputs()
        };

        assert_eq!(
            evaluate(&scheduled, &calendar_policy()),
            DetectionOutcome::AutoStart {
                event_key: "event-1".to_string(),
                event_title: "Quarterly planning".to_string(),
            }
        );
    }

    /* Attribution needs the app in front; the boundary of a call already
     * attributed does not. Switching to Notes mid-call keeps the claim and the
     * pending prompt; music behind a backgrounded call app does not. */
    #[test]
    fn a_claimed_call_survives_the_operator_switching_away() {
        assert!(!call_is_live(
            &facetime(false),
            &AppSignal::Absent,
            MicSignal::Active,
            OutputSignal::Idle,
        ));
        assert!(call_evidence(
            &facetime(false),
            MicSignal::Active,
            OutputSignal::Idle
        ));
        assert!(!call_evidence(
            &facetime(false),
            MicSignal::Idle,
            OutputSignal::Active
        ));
        assert!(call_evidence(
            &facetime(true),
            MicSignal::Idle,
            OutputSignal::Active
        ));
        assert!(!call_evidence(
            &CallSignal::Absent,
            MicSignal::Active,
            OutputSignal::Active
        ));
    }

    /* Safety rule: an app the operator never put on the auto-record list is
     * offered, never taken. */
    #[test]
    fn a_call_app_without_a_standing_grant_prompts_instead_of_starting() {
        let ungranted = DetectionInputs {
            standing_app_consent: false,
            ..granted_call_on_output_only()
        };

        assert_eq!(
            evaluate(&ungranted, &DetectionPolicy::default()),
            DetectionOutcome::Prompt(PromptKind::AppCall {
                bundle_id: "com.apple.facetime".to_string(),
                app_name: "FaceTime".to_string(),
            })
        );
    }

    /* Safety rule: the master toggle. */
    #[test]
    fn disabled_detection_never_auto_records_a_call() {
        let policy = DetectionPolicy {
            enabled: false,
            ..DetectionPolicy::default()
        };

        assert_eq!(
            evaluate(&granted_call_on_output_only(), &policy),
            DetectionOutcome::Suppress(SuppressReason::DetectionDisabled)
        );
    }

    /* Safety rule: a dictation run holds the same device a call would, and the
     * output clause cannot tell a call from Sona's own start chime. */
    #[test]
    fn sonas_own_microphone_never_auto_records_a_call() {
        let dictating = DetectionInputs {
            mic: MicSignal::Active,
            self_holds_input_device: true,
            ..granted_call_on_output_only()
        };

        assert_eq!(
            evaluate(&dictating, &DetectionPolicy::default()),
            DetectionOutcome::Suppress(SuppressReason::SonaHoldsInputDevice)
        );
    }

    /* Safety rule: a capture already running is never joined by a second one. */
    #[test]
    fn a_live_capture_never_auto_records_a_call_beside_it() {
        let capturing = DetectionInputs {
            capture_active: true,
            ..granted_call_on_output_only()
        };

        assert_eq!(
            evaluate(&capturing, &DetectionPolicy::default()),
            DetectionOutcome::Suppress(SuppressReason::CaptureAlreadyActive)
        );
    }

    /* A call app that is merely open must not consume the decision, and it must
     * not claim a microphone another running meeting app explains just as well:
     * the property is device-global, so a backgrounded FaceTime is a guess and
     * Zoom is a reading. */
    #[test]
    fn an_idle_call_app_does_not_shadow_the_ad_hoc_path() {
        let both = DetectionInputs {
            call: facetime(false),
            app: zoom(),
            mic: MicSignal::Active,
            output: OutputSignal::Active,
            standing_app_consent: true,
            ..inputs()
        };

        assert_eq!(
            evaluate(&both, &DetectionPolicy::default()),
            DetectionOutcome::Prompt(PromptKind::AppMeeting {
                bundle_id: "us.zoom.xos".to_string(),
                app_name: "Zoom".to_string(),
            })
        );
    }

    /* Bringing the call app to the front resolves the same ambiguity the other
     * way: now the operator is looking at the call. */
    #[test]
    fn a_frontmost_call_app_wins_over_a_running_meeting_app() {
        let both = DetectionInputs {
            call: facetime(true),
            app: zoom(),
            mic: MicSignal::Active,
            output: OutputSignal::Active,
            standing_app_consent: true,
            ..inputs()
        };

        assert_eq!(
            evaluate(&both, &DetectionPolicy::default()),
            DetectionOutcome::AutoStartCall {
                bundle_id: "com.apple.facetime".to_string(),
                app_name: "FaceTime".to_string(),
            }
        );
    }

    /* `InputDeviceIdle` cannot fire during a capture — Sona's own microphone is
     * what keeps that device raised — and a call app stays open after a hangup,
     * so neither existing trigger can end a call. */
    fn call_recording() -> StopInputs {
        StopInputs {
            now_utc_ms: NOW,
            linked_event_end_utc_ms: None,
            self_holds_input_device: false,
            device_running_somewhere: true,
            microphone_lane: true,
            call_output: Some(CallOutputWatch::default()),
            trigger_app_running: true,
            slept_since_start: false,
        }
    }

    #[test]
    fn a_call_capture_ends_when_the_output_device_goes_quiet() {
        let mut output = CallOutputWatch::default();
        output.observe(OutputSignal::Active, NOW);
        let playing = StopInputs {
            call_output: Some(output),
            ..call_recording()
        };

        assert_eq!(evaluate_stop(&playing), None);

        output.observe(OutputSignal::Idle, NOW + 1_000);
        let hung_up = StopInputs {
            now_utc_ms: NOW + 1_000 + CALL_HANGUP_GRACE_MS,
            call_output: Some(output),
            ..call_recording()
        };

        assert_eq!(evaluate_stop(&hung_up), Some(StopTrigger::CallEnded));
    }

    #[test]
    fn an_output_gap_shorter_than_the_grace_is_not_a_hangup() {
        let mut output = CallOutputWatch::default();
        output.observe(OutputSignal::Active, NOW);
        output.observe(OutputSignal::Idle, NOW + 1_000);
        let gap = StopInputs {
            now_utc_ms: NOW + CALL_HANGUP_GRACE_MS,
            call_output: Some(output),
            ..call_recording()
        };

        assert_eq!(evaluate_stop(&gap), None);

        output.observe(OutputSignal::Active, NOW + 3_000);
        let moved = StopInputs {
            now_utc_ms: NOW + 60_000,
            call_output: Some(output),
            ..call_recording()
        };

        assert_eq!(evaluate_stop(&moved), None);
    }

    #[test]
    fn a_call_that_never_played_through_the_default_output_is_not_ended_by_it() {
        assert!(call_is_live(
            &facetime(true),
            &AppSignal::Absent,
            MicSignal::Active,
            OutputSignal::Idle,
        ));

        let mut output = CallOutputWatch::default();
        output.observe(OutputSignal::Idle, NOW);
        let silent = StopInputs {
            now_utc_ms: NOW + 10 * CALL_HANGUP_GRACE_MS,
            call_output: Some(output),
            ..call_recording()
        };

        assert_eq!(evaluate_stop(&silent), None);
    }

    #[test]
    fn a_quiet_output_device_never_stops_a_capture_no_call_started() {
        let meeting = StopInputs {
            call_output: None,
            ..call_recording()
        };

        assert_eq!(evaluate_stop(&meeting), None);
    }
    #[test]
    fn a_call_is_titled_with_the_app_and_the_time_it_started() {
        let started = Local
            .with_ymd_and_hms(2026, 3, 14, 15, 15, 0)
            .single()
            .expect("an unambiguous local instant");

        assert_eq!(
            PromptKind::AppCall {
                bundle_id: "com.apple.facetime".to_string(),
                app_name: "FaceTime".to_string(),
            }
            .proposed_meeting_title(started),
            "FaceTime call, 3:15 PM"
        );
    }

    #[test]
    fn the_call_prompt_wire_shape_matches_the_rest_of_the_union() {
        assert_eq!(
            serde_json::to_value(PromptKind::AppCall {
                bundle_id: "com.apple.mobilephone".to_string(),
                app_name: "Phone".to_string(),
            })
            .expect("prompt serializes"),
            serde_json::json!({
                "kind": "AppCall",
                "bundleId": "com.apple.mobilephone",
                "appName": "Phone",
            })
        );
    }
}
