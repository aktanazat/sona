//! The calendar dimension: EventKit against the on-device Calendar store.
//!
//! Reading events requires **full access**, not read-only: Apple's own docs are
//! explicit that a read-only grant does not exist and that the write-only grant
//! can write but never read back. That is a heavy ask for a note-taking app, so
//! the request is lazy — it happens the first time the operator turns the
//! calendar sub-toggle on, never at launch and never as a side effect of the
//! master detection toggle.
//!
//! No event content is persisted, and only the one nearest event is read in
//! full. Every event in the lookahead window is reduced to a key, a title, a
//! participant count and two instants; the event the tick actually selects is
//! then read a second time for the facts the pre-meeting card shows — named
//! attendees with their answers, the event's notes, its calendar's title, and
//! the URL attached to it. Those live in the in-memory detection status until
//! the event passes. Nothing on this path writes to disk, and nothing writes
//! back to the Calendar store: Sona records meetings, it does not answer
//! invitations.

use super::machine::{
    CalendarAttendee, CalendarEventSummary, CalendarSignal, ParticipationStatus, ATTENDEE_FLOOR,
    CALENDAR_LEAD_SECONDS,
};

/// How far ahead to look for the next event. Wide enough that a tick can never
/// step over an event's start, narrow enough that the query stays trivial.
const LOOKAHEAD_MS: i64 = 2 * 60 * 60 * 1000;

/// One event in the lookahead window, as the selection sees it: the summary
/// the decision table reads, plus the one fact the summary does not carry and
/// only the calendar can answer — how the operator answered the invitation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventCandidate {
    pub summary: CalendarEventSummary,
    /// `Unknown` when the operator is not on the attendee list at all, which
    /// is every event they own with no invitees.
    pub self_status: ParticipationStatus,
}

/// Picks the one event the decision table hears about, by index into
/// `candidates`.
///
/// Events that have ended are out, and so are events the operator declined:
/// their own answer is the best evidence there is that they are not in that
/// meeting, and a live microphone beside a declined event is a voice memo, not
/// the meeting. Among the rest, three tiers, nearest start within each:
///
/// 1. A meeting about to start, inside the countdown lead. The countdown is
///    the calendar path's one visible promise, and a meeting running over
///    must not hide it.
/// 2. A meeting under way. It outranks anything merely scheduled, so a solo
///    block starting in five minutes cannot hide the meeting the operator is
///    already in.
/// 3. Everything else, nearest start first, which is how a solo block still
///    reaches the table to be declined there for its own reason.
///
/// "Meeting" here means the attendee floor is met; the table applies the
/// floor again on whatever wins, so a tier is a preference, never a decision.
pub fn select_event<'a>(
    candidates: impl IntoIterator<Item = &'a EventCandidate>,
    now_utc_ms: i64,
) -> Option<usize> {
    candidates
        .into_iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.summary.end_utc_ms > now_utc_ms
                && candidate.self_status != ParticipationStatus::Declined
        })
        .min_by_key(|(_, candidate)| {
            let summary = &candidate.summary;
            let meeting = summary.attendee_count >= ATTENDEE_FLOOR;
            let to_start = summary.start_utc_ms - now_utc_ms;
            let tier = match (meeting, to_start <= 0) {
                (true, false) if to_start <= CALENDAR_LEAD_SECONDS * 1_000 => 0,
                (true, true) => 1,
                _ => 2,
            };
            (tier, to_start.abs())
        })
        .map(|(index, _)| index)
}

/// Authorization for reading events, as the operator would recognize it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum CalendarAccess {
    /// Never asked. The sub-toggle has not been turned on.
    NotDetermined,
    /// Full access granted: events are readable.
    Authorized,
    /// Denied, restricted, or downgraded to write-only. Detection continues on
    /// the ad-hoc path alone and says so in its status.
    Denied,
    /// No EventKit on this platform.
    Unavailable,
}

/// One occurrence returned by a ranged listing, plus the one fact only the
/// calendar itself can answer: whether the event it came from repeats.
///
/// `is_recurring` is not derivable from `CalendarEventSummary`. Every event has
/// a `series_key` — it is EventKit's calendar-item identifier, which a one-off
/// carries too — so "this is a series" has to come from the recurrence rules,
/// and it rides beside the summary rather than inside it: the decision table
/// never asks, and widening a type six call sites construct to serve one
/// surface is how shared types rot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CalendarOccurrence {
    pub summary: CalendarEventSummary,
    pub is_recurring: bool,
}

/// The calendar store, behind a trait so the runtime and the decision table can
/// both be exercised without a Calendar database or a TCC prompt.
pub trait CalendarSource: Send + Sync {
    fn access(&self) -> CalendarAccess;

    /// Requests full access. Blocks until the operator answers, so this is only
    /// ever called from the command that handles the sub-toggle being switched
    /// on, never from the detection tick.
    fn request_access(&self) -> CalendarAccess;

    /// The single event the decision table should hear about now, chosen by
    /// `select_event`, or `None`. Returning one event rather than a list is
    /// deliberate: the decision table only ever asks about the current moment,
    /// and a list would invite callers to invent their own precedence.
    fn next_event(&self, now_utc_ms: i64, lookahead_ms: i64) -> Option<CalendarEventSummary>;

    /// Every event that overlaps `[start_utc_ms, end_utc_ms)`, oldest first.
    ///
    /// D28's Upcoming section is the only caller, and it is user-triggered
    /// rather than ticked, which is what makes a list affordable here when the
    /// detection path deliberately refuses one. Rows are enriched with the
    /// facts a row renders — named attendees, the calendar's title, the join
    /// URL — and deliberately *not* with the event's notes: an agenda pasted
    /// into a recurring event is kilobytes nothing on this surface shows.
    fn events_between(&self, start_utc_ms: i64, end_utc_ms: i64) -> Vec<CalendarOccurrence>;
}

/// Used on non-macOS targets and whenever the sub-toggle is off.
pub struct NoCalendar;

impl CalendarSource for NoCalendar {
    fn access(&self) -> CalendarAccess {
        CalendarAccess::Unavailable
    }

    fn request_access(&self) -> CalendarAccess {
        CalendarAccess::Unavailable
    }

    fn next_event(&self, _now_utc_ms: i64, _lookahead_ms: i64) -> Option<CalendarEventSummary> {
        None
    }

    fn events_between(&self, _start_utc_ms: i64, _end_utc_ms: i64) -> Vec<CalendarOccurrence> {
        Vec::new()
    }
}

/// This platform's calendar, or the one that answers nothing where there is
/// none.
///
/// One factory rather than a `cfg` at each callsite: the detection loop builds
/// one at startup and the headless `--upcoming` read builds one per
/// invocation, and a second `cfg` pick would be a second answer to which
/// calendar this build reads. Cheap either way — the EventKit store is created
/// on first use, not here.
pub fn platform_calendar() -> std::sync::Arc<dyn CalendarSource> {
    #[cfg(target_os = "macos")]
    {
        std::sync::Arc::new(EventKitCalendar::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::sync::Arc::new(NoCalendar)
    }
}

/// Places the nearest event relative to `now`. Pure, so the T-60s boundary and
/// the started/ended transitions are testable without a calendar.
pub fn calendar_signal(event: Option<CalendarEventSummary>, now_utc_ms: i64) -> CalendarSignal {
    let Some(event) = event else {
        return CalendarSignal::Absent;
    };
    if now_utc_ms >= event.end_utc_ms {
        return CalendarSignal::Absent;
    }
    if now_utc_ms >= event.start_utc_ms {
        return CalendarSignal::Started { event };
    }
    // Rounds up, so an event 60.4s out reports 61 and stays outside the lead
    // window rather than prompting early.
    let seconds_to_start = (event.start_utc_ms - now_utc_ms + 999) / 1_000;
    CalendarSignal::Upcoming {
        event,
        seconds_to_start,
    }
}

pub fn lookahead_ms() -> i64 {
    LOOKAHEAD_MS
}
/// Derives the identity used for one prompt/auto-start claim. EventKit exposes
/// a calendar-item identifier shared by recurring occurrences, so the start
/// instant is part of the occurrence key while the bare identifier remains the
/// series key used by standing consent.
pub fn occurrence_key(series_key: &str, start_utc_ms: i64) -> String {
    format!("{series_key}#{start_utc_ms}")
}

/// The start instant an `occurrence_key` carries, or `None` for a key this
/// module did not mint.
pub fn occurrence_start(event_key: &str) -> Option<i64> {
    event_key.rsplit_once('#')?.1.parse().ok()
}

/// Maps `EKParticipantStatus`'s raw value onto the answer a person recognizes.
///
/// Keyed on the raw integer, and living outside the macOS module, so the one
/// rule that turns a framework constant into something a card renders is
/// testable without a Calendar database or a TCC prompt.
pub fn participation_status(raw: isize) -> ParticipationStatus {
    match raw {
        1 => ParticipationStatus::Pending,
        2 => ParticipationStatus::Accepted,
        3 => ParticipationStatus::Declined,
        4 => ParticipationStatus::Tentative,
        // 0 is Unknown. 5 Delegated, 6 Completed and 7 InProcess describe
        // reminders and task assignments, not an answer to an invitation, so
        // they report "no answer" rather than a fabricated attendance claim.
        _ => ParticipationStatus::Unknown,
    }
}

/// Reduces one raw participant to an attendee a card can name, or nothing.
///
/// A participant EventKit will not name is a chip that could say only
/// "someone". It is dropped here and survives in `attendee_count` alone, which
/// is the honest split: the event has N participants, and these are the ones
/// with names.
pub fn named_attendee(
    name: Option<&str>,
    status_raw: isize,
    is_self: bool,
) -> Option<CalendarAttendee> {
    named_attendee_with_email(name, status_raw, is_self, None)
}

fn named_attendee_with_email(
    name: Option<&str>,
    status_raw: isize,
    is_self: bool,
    participant_url: Option<&str>,
) -> Option<CalendarAttendee> {
    let name = name?.trim();
    if name.is_empty() {
        return None;
    }
    Some(CalendarAttendee {
        name: name.to_string(),
        status: participation_status(status_raw),
        email: participant_email(participant_url),
        is_self,
    })
}

fn participant_email(participant_url: Option<&str>) -> Option<String> {
    let value = participant_url?.trim();
    let email = value
        .strip_prefix("mailto:")
        .or_else(|| value.strip_prefix("MAILTO:"))?
        .split('?')
        .next()?
        .trim()
        .to_lowercase();
    (!email.is_empty()).then_some(email)
}

/// Trims one optional event string and treats an empty result as absent, so a
/// row is omitted rather than rendered blank.
pub fn event_text(value: Option<String>) -> Option<String> {
    let trimmed = value?.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(target_os = "macos")]
pub use macos::EventKitCalendar;

#[cfg(target_os = "macos")]
mod macos {
    use super::{
        event_text, named_attendee_with_email, occurrence_key, participation_status, select_event,
        CalendarAccess, CalendarEventSummary, CalendarOccurrence, CalendarSource, EventCandidate,
        ParticipationStatus,
    };
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2_event_kit::{EKAuthorizationStatus, EKEntityType, EKEvent, EKEventStore};
    use objc2_foundation::{NSDate, NSError};
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::time::Duration;

    /// How long to wait for the operator to answer the TCC prompt before giving
    /// up on the reply. The grant still lands in TCC if they answer later; the
    /// next `access()` call sees it. This bound exists because the command is
    /// awaited by the settings toggle, and a toggle that never returns is worse
    /// than one that reports "not determined".
    const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(120);

    /// Owns the one `EKEventStore`, created on first query rather than in `new`.
    ///
    /// `new` runs inside Tauri's `setup`, before the window exists, and
    /// constructing a store connects to CalendarAgent. Worse, it did so
    /// unconditionally: `detection_calendar_enabled` defaults to false, so the
    /// overwhelming majority of operators paid a calendar connection at every
    /// launch for a feature they had not turned on — while this module's own
    /// permission matrix promised the calendar was touched lazily and never at
    /// launch. Deferring it makes that promise true.
    pub struct EventKitCalendar {
        store: Mutex<Option<Retained<EKEventStore>>>,
    }

    // SAFETY: every use of `store` goes through the mutex, and EKEventStore's
    // event queries are documented as callable off the main thread.
    unsafe impl Send for EventKitCalendar {}
    unsafe impl Sync for EventKitCalendar {}

    impl EventKitCalendar {
        pub fn new() -> Self {
            Self {
                store: Mutex::new(None),
            }
        }

        /// Runs `body` against the one store, creating it on first use.
        ///
        /// The lock is held for the whole body, which is what the previous
        /// guard-returning accessor did too: EventKit's query APIs are callable
        /// off the main thread but not concurrently on one store, and both the
        /// tick thread and the settings command can arrive here.
        fn with_store<T>(&self, body: impl FnOnce(&EKEventStore) -> T) -> T {
            let mut store = self
                .store
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // SAFETY: `EKEventStore::new` is a plain allocation and init; it
            // requests nothing and prompts for nothing.
            let store = store.get_or_insert_with(|| unsafe { EKEventStore::new() });
            body(store)
        }
    }

    impl Default for EventKitCalendar {
        fn default() -> Self {
            Self::new()
        }
    }

    fn access_from_status(status: EKAuthorizationStatus) -> CalendarAccess {
        match status {
            EKAuthorizationStatus::NotDetermined => CalendarAccess::NotDetermined,
            EKAuthorizationStatus::FullAccess => CalendarAccess::Authorized,
            // Denied, restricted, and write-only all mean the same thing here:
            // events cannot be read.
            _ => CalendarAccess::Denied,
        }
    }

    impl CalendarSource for EventKitCalendar {
        fn access(&self) -> CalendarAccess {
            // SAFETY: a class method reading TCC state; no arguments to violate.
            let status =
                unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
            access_from_status(status)
        }

        fn request_access(&self) -> CalendarAccess {
            if self.access() == CalendarAccess::Authorized {
                return CalendarAccess::Authorized;
            }
            let (sender, receiver) = mpsc::channel::<bool>();
            let completion = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
                let _ = sender.send(granted.as_bool());
            });
            // SAFETY: EventKit retains the block for the request's duration, and
            // the block only sends into a channel this scope owns.
            self.with_store(|store| unsafe {
                store.requestFullAccessToEventsWithCompletion(RcBlock::as_ptr(&completion));
            });
            match receiver.recv_timeout(AUTHORIZATION_TIMEOUT) {
                // Re-read TCC rather than trusting the boolean: a write-only
                // grant reports success but still cannot read events.
                Ok(_) => self.access(),
                Err(_) => CalendarAccess::NotDetermined,
            }
        }

        fn next_event(&self, now_utc_ms: i64, lookahead_ms: i64) -> Option<CalendarEventSummary> {
            if self.access() != CalendarAccess::Authorized {
                return None;
            }
            objc2::rc::autoreleasepool(|_| {
                self.with_store(|store| {
                    // Start the window in the past so an event already under way
                    // is still found; `calendar_signal` sorts out where it sits.
                    let start = NSDate::dateWithTimeIntervalSince1970(
                        (now_utc_ms - lookahead_ms) as f64 / 1_000.0,
                    );
                    let end = NSDate::dateWithTimeIntervalSince1970(
                        (now_utc_ms + lookahead_ms) as f64 / 1_000.0,
                    );
                    // `None` calendars means "every calendar the store sees".
                    // SAFETY: both dates are live for this construction call.
                    let predicate = unsafe {
                        store.predicateForEventsWithStartDate_endDate_calendars(&start, &end, None)
                    };
                    // SAFETY: the predicate came from this same store.
                    let events = unsafe { store.eventsMatchingPredicate(&predicate) };
                    // Two passes on purpose. The cheap summary runs over every
                    // event in the window; the content read — notes, attendee
                    // names, calendar title, URL — runs once, on the one event
                    // that won. An agenda pasted into a recurring event is
                    // kilobytes of string, and reading it for a dozen events
                    // every fifteen seconds to throw eleven away is the kind of
                    // waste that never shows up in a profile and never stops
                    // costing. The operator's own answer is read in the first
                    // pass because the selection needs it: it is one status
                    // per event, not the attendee list.
                    let mut candidates = events
                        .iter()
                        .filter_map(|event| {
                            let summary = summarize(&event)?;
                            let self_status = self_participation(&event);
                            Some((
                                event,
                                EventCandidate {
                                    summary,
                                    self_status,
                                },
                            ))
                        })
                        .collect::<Vec<_>>();
                    let index = select_event(
                        candidates.iter().map(|(_, candidate)| candidate),
                        now_utc_ms,
                    )?;
                    let (event, candidate) = candidates.swap_remove(index);
                    let mut summary = candidate.summary;
                    enrich(&event, &mut summary);
                    Some(summary)
                })
            })
        }

        fn events_between(&self, start_utc_ms: i64, end_utc_ms: i64) -> Vec<CalendarOccurrence> {
            if self.access() != CalendarAccess::Authorized || end_utc_ms <= start_utc_ms {
                return Vec::new();
            }
            objc2::rc::autoreleasepool(|_| {
                self.with_store(|store| {
                    let start =
                        NSDate::dateWithTimeIntervalSince1970(start_utc_ms as f64 / 1_000.0);
                    let end = NSDate::dateWithTimeIntervalSince1970(end_utc_ms as f64 / 1_000.0);
                    // `None` calendars means "every calendar the store sees",
                    // which is exactly what D28 promises: the Google, iCloud and
                    // Outlook accounts already signed in to macOS Calendar.
                    // SAFETY: both dates are live for this construction call.
                    let predicate = unsafe {
                        store.predicateForEventsWithStartDate_endDate_calendars(&start, &end, None)
                    };
                    // SAFETY: the predicate came from this same store.
                    let events = unsafe { store.eventsMatchingPredicate(&predicate) };
                    let mut occurrences = events
                        .iter()
                        .filter_map(|event| {
                            let mut summary = summarize(&event)?;
                            // The predicate matches anything overlapping the
                            // window, including something that began yesterday
                            // and runs into it. A row that has already ended is
                            // not upcoming.
                            if summary.end_utc_ms <= start_utc_ms {
                                return None;
                            }
                            enrich_participants(&event, &mut summary);
                            // SAFETY: a plain property read on a live event.
                            let is_recurring = unsafe { event.hasRecurrenceRules() };
                            Some(CalendarOccurrence {
                                summary,
                                is_recurring,
                            })
                        })
                        .collect::<Vec<_>>();
                    // EventKit does not promise an order. The title breaks a
                    // tie so two events on the same minute do not swap places
                    // between reads of the same unchanged calendar.
                    occurrences.sort_by(|left, right| {
                        left.summary
                            .start_utc_ms
                            .cmp(&right.summary.start_utc_ms)
                            .then_with(|| left.summary.title.cmp(&right.summary.title))
                            .then_with(|| left.summary.event_key.cmp(&right.summary.event_key))
                    });
                    occurrences
                })
            })
        }
    }

    /// Reduces an `EKEvent` to the fields the decision table reads, and nothing
    /// more. `enrich` adds the rest, for the one event that is selected.
    fn summarize(event: &EKEvent) -> Option<CalendarEventSummary> {
        // SAFETY: plain property reads on a live event.
        let (start, end, all_day) =
            unsafe { (event.startDate(), event.endDate(), event.isAllDay()) };
        // An all-day event is a label on the day, not something that starts. It
        // would otherwise fire a countdown at every midnight.
        if all_day {
            return None;
        }
        let start_utc_ms = instant_ms(&start);
        let end_utc_ms = instant_ms(&end);
        if end_utc_ms <= start_utc_ms {
            return None;
        }
        // SAFETY: plain property reads on a live event.
        let (identifier, title, attendees) = unsafe {
            (
                event.calendarItemIdentifier(),
                event.title(),
                event.attendees(),
            )
        };
        let series_key = identifier.to_string();
        if series_key.is_empty() {
            return None;
        }
        Some(CalendarEventSummary {
            event_key: occurrence_key(&series_key, start_utc_ms),
            series_key,
            title: title.to_string(),
            // A nil attendee list counts as zero, which §5.3 case 9 treats the
            // same as a solo block. Under-counting here suppresses a prompt;
            // over-counting would raise one for a personal reminder.
            attendee_count: attendees.map_or(0, |attendees| attendees.len()),
            start_utc_ms,
            end_utc_ms,
            // Filled by `enrich`, for the selected event only.
            attendees: Vec::new(),
            notes: None,
            calendar_name: None,
            url: None,
        })
    }

    /// How the operator answered this event's invitation. `Unknown` when they
    /// are not on its attendee list, which is every event they own with no
    /// invitees: there was no invitation to answer.
    fn self_participation(event: &EKEvent) -> ParticipationStatus {
        // SAFETY: a plain property read on a live event.
        let Some(attendees) = (unsafe { event.attendees() }) else {
            return ParticipationStatus::Unknown;
        };
        attendees
            .iter()
            // SAFETY: plain property reads on a live participant.
            .find(|participant| unsafe { participant.isCurrentUser() })
            .map(|participant| participation_status(unsafe { participant.participantStatus() }.0))
            .unwrap_or_default()
    }

    /// Adds the facts the pre-meeting card renders to an already-selected
    /// event. Every one of them stays absent when EventKit reports nothing, so
    /// a card omits the row rather than showing an empty one.
    fn enrich(event: &EKEvent, summary: &mut CalendarEventSummary) {
        enrich_participants(event, summary);
        // SAFETY: a plain property read on a live event.
        let notes = unsafe { event.notes() };
        summary.notes = event_text(notes.map(|notes| notes.to_string()));
    }

    /// Everything a row can show about who is coming and where the event lives.
    ///
    /// Split out from `enrich` so the ranged listing can have it without the
    /// notes read: an agenda pasted into a recurring event is kilobytes of
    /// string, no D28 row shows one, and paying for thirty of them on every
    /// Meetings-home mount is exactly the waste this module already refuses on
    /// the tick path.
    fn enrich_participants(event: &EKEvent, summary: &mut CalendarEventSummary) {
        // SAFETY: plain property reads on a live event.
        let (attendees, calendar, url) =
            unsafe { (event.attendees(), event.calendar(), event.URL()) };
        if let Some(attendees) = attendees {
            summary.attendees = attendees
                .iter()
                .filter_map(|participant| {
                    // SAFETY: plain property reads on a live participant.
                    let (name, status, is_self, participant_url) = unsafe {
                        let participant_url = participant
                            .URL()
                            .absoluteString()
                            .map(|url| url.to_string());
                        (
                            participant.name(),
                            participant.participantStatus(),
                            participant.isCurrentUser(),
                            participant_url,
                        )
                    };
                    let name = name.map(|name| name.to_string());
                    named_attendee_with_email(
                        name.as_deref(),
                        status.0,
                        is_self,
                        participant_url.as_deref(),
                    )
                })
                .collect();
        }
        // SAFETY: `title` is a plain property read on a live calendar.
        summary.calendar_name =
            event_text(calendar.map(|calendar| unsafe { calendar.title() }.to_string()));
        summary.url = event_text(
            url.and_then(|url| url.absoluteString())
                .map(|absolute| absolute.to_string()),
        );
    }

    fn instant_ms(date: &NSDate) -> i64 {
        (date.timeIntervalSince1970() * 1_000.0) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    fn event(start_offset_ms: i64, duration_ms: i64) -> CalendarEventSummary {
        CalendarEventSummary {
            event_key: "event-1".to_string(),
            series_key: "series-1".to_string(),
            title: "Quarterly planning".to_string(),
            attendee_count: 3,
            start_utc_ms: NOW + start_offset_ms,
            end_utc_ms: NOW + start_offset_ms + duration_ms,
            attendees: Vec::new(),
            notes: None,
            calendar_name: None,
            url: None,
        }
    }

    const MINUTE_MS: i64 = 60_000;

    fn candidate(
        start_offset_ms: i64,
        attendee_count: usize,
        self_status: ParticipationStatus,
    ) -> EventCandidate {
        EventCandidate {
            summary: CalendarEventSummary {
                attendee_count,
                ..event(start_offset_ms, 60 * MINUTE_MS)
            },
            self_status,
        }
    }

    /* FP5 in the detection map: a started event with enough attendees used to
     * prompt over the operator's own "no". Their answer is the best evidence
     * there is that a live microphone beside it is not that meeting. */
    #[test]
    fn an_event_the_operator_declined_is_never_selected() {
        let declined_now = candidate(-10 * MINUTE_MS, 3, ParticipationStatus::Declined);
        let accepted_later = candidate(30 * MINUTE_MS, 3, ParticipationStatus::Accepted);

        assert_eq!(
            select_event([&declined_now, &accepted_later], NOW),
            Some(1),
            "the declined meeting under way must not hide the accepted one"
        );
        assert_eq!(select_event([&declined_now], NOW), None);
    }

    /* FN10 in the detection map: nearest-by-start picked the solo block five
     * minutes out over the meeting the operator had been in for ten. */
    #[test]
    fn a_meeting_under_way_beats_a_nearer_solo_block() {
        let meeting_under_way = candidate(-10 * MINUTE_MS, 3, ParticipationStatus::Accepted);
        let solo_block_soon = candidate(5 * MINUTE_MS, 1, ParticipationStatus::Unknown);

        assert_eq!(
            select_event([&solo_block_soon, &meeting_under_way], NOW),
            Some(1)
        );
    }

    /* Back-to-back meetings, the first running over. The countdown for the
     * second is the calendar path's one visible promise and must show. */
    #[test]
    fn a_countdown_about_to_show_beats_a_meeting_running_over() {
        let running_over = candidate(-50 * MINUTE_MS, 3, ParticipationStatus::Accepted);
        let next_in_45s = candidate(45_000, 3, ParticipationStatus::Accepted);
        let next_in_5m = candidate(5 * MINUTE_MS, 3, ParticipationStatus::Accepted);

        assert_eq!(select_event([&running_over, &next_in_45s], NOW), Some(1));
        assert_eq!(
            select_event([&running_over, &next_in_5m], NOW),
            Some(0),
            "outside the lead window the meeting under way still wins"
        );
    }

    #[test]
    fn among_scheduled_events_the_nearest_start_still_wins() {
        let ended = candidate(-90 * MINUTE_MS, 3, ParticipationStatus::Accepted);
        let solo_soon = candidate(5 * MINUTE_MS, 1, ParticipationStatus::Unknown);
        let meeting_later = candidate(40 * MINUTE_MS, 4, ParticipationStatus::Accepted);

        assert_eq!(
            select_event([&ended, &meeting_later, &solo_soon], NOW),
            Some(2),
            "a solo block reaches the table so it can be declined there for its own reason"
        );
        assert_eq!(select_event([&ended], NOW), None);
    }

    #[test]
    fn recurring_occurrences_have_distinct_claim_keys_and_one_series_key() {
        let first = occurrence_key("event-kit-series", NOW);
        let second = occurrence_key("event-kit-series", NOW + 7 * 24 * 60 * 60_000);

        assert_ne!(first, second);
        assert!(first.starts_with("event-kit-series#"));
        assert!(second.starts_with("event-kit-series#"));
    }

    #[test]
    fn no_event_reads_as_an_absent_signal() {
        assert_eq!(calendar_signal(None, NOW), CalendarSignal::Absent);
    }

    #[test]
    fn an_event_under_way_reads_as_started() {
        let signal = calendar_signal(Some(event(-60_000, 30 * 60_000)), NOW);

        assert!(matches!(signal, CalendarSignal::Started { .. }));
    }

    #[test]
    fn a_finished_event_reads_as_absent() {
        let signal = calendar_signal(Some(event(-60 * 60_000, 30 * 60_000)), NOW);

        assert_eq!(signal, CalendarSignal::Absent);
    }

    #[test]
    fn the_lead_countdown_rounds_away_from_prompting_early() {
        let CalendarSignal::Upcoming {
            seconds_to_start, ..
        } = calendar_signal(Some(event(60_400, 30 * 60_000)), NOW)
        else {
            panic!("an event 60.4s out is still upcoming");
        };

        assert_eq!(
            seconds_to_start, 61,
            "rounding up keeps a 60.4s event outside the T-60s window"
        );
    }

    #[test]
    fn an_event_exactly_at_the_lead_boundary_reports_sixty() {
        let CalendarSignal::Upcoming {
            seconds_to_start, ..
        } = calendar_signal(Some(event(60_000, 30 * 60_000)), NOW)
        else {
            panic!("an event 60s out is still upcoming");
        };

        assert_eq!(seconds_to_start, 60);
    }

    #[test]
    fn the_start_instant_flips_upcoming_to_started() {
        let signal = calendar_signal(Some(event(0, 30 * 60_000)), NOW);

        assert!(matches!(signal, CalendarSignal::Started { .. }));
    }

    #[test]
    fn an_absent_calendar_never_produces_an_event() {
        assert_eq!(NoCalendar.access(), CalendarAccess::Unavailable);
        assert_eq!(NoCalendar.next_event(NOW, lookahead_ms()), None);
        assert_eq!(
            NoCalendar.events_between(NOW, NOW + 8 * 24 * 60 * 60_000),
            Vec::new()
        );
    }

    /* EKParticipantStatus's raw values, as the framework defines them. Written
     * out rather than imported so the test still fails if the mapping is
     * silently re-pointed at some other integer. */
    const RAW_UNKNOWN: isize = 0;
    const RAW_PENDING: isize = 1;
    const RAW_ACCEPTED: isize = 2;
    const RAW_DECLINED: isize = 3;
    const RAW_TENTATIVE: isize = 4;
    const RAW_DELEGATED: isize = 5;
    const RAW_COMPLETED: isize = 6;
    const RAW_IN_PROCESS: isize = 7;

    #[test]
    fn every_answer_a_person_can_give_maps_to_its_own_state() {
        assert_eq!(
            participation_status(RAW_PENDING),
            ParticipationStatus::Pending
        );
        assert_eq!(
            participation_status(RAW_ACCEPTED),
            ParticipationStatus::Accepted
        );
        assert_eq!(
            participation_status(RAW_DECLINED),
            ParticipationStatus::Declined
        );
        assert_eq!(
            participation_status(RAW_TENTATIVE),
            ParticipationStatus::Tentative
        );
    }

    #[test]
    fn task_states_report_no_answer_rather_than_an_invented_one() {
        for raw in [RAW_UNKNOWN, RAW_DELEGATED, RAW_COMPLETED, RAW_IN_PROCESS] {
            assert_eq!(
                participation_status(raw),
                ParticipationStatus::Unknown,
                "raw status {raw} is not an answer to an invitation"
            );
        }
    }

    #[test]
    fn an_unknown_future_status_reports_no_answer() {
        assert_eq!(participation_status(99), ParticipationStatus::Unknown);
    }

    #[test]
    fn a_named_participant_carries_their_answer_and_whether_they_are_you() {
        assert_eq!(
            named_attendee(Some("  Aktan Azat  "), RAW_ACCEPTED, true),
            Some(CalendarAttendee {
                name: "Aktan Azat".to_string(),
                status: ParticipationStatus::Accepted,
                is_self: true,
                email: None,
            })
        );
    }

    #[test]
    fn a_mailto_participant_url_preserves_the_calendar_email() {
        assert_eq!(
            named_attendee_with_email(
                Some("Stephen Wolfram"),
                RAW_ACCEPTED,
                false,
                Some("mailto:STEPHEN@example.com?subject=meeting"),
            ),
            Some(CalendarAttendee {
                name: "Stephen Wolfram".to_string(),
                email: Some("stephen@example.com".to_string()),
                status: ParticipationStatus::Accepted,
                is_self: false,
            })
        );
    }

    #[test]
    fn a_participant_eventkit_will_not_name_is_dropped() {
        assert_eq!(named_attendee(None, RAW_ACCEPTED, false), None);
        assert_eq!(named_attendee(Some("   "), RAW_ACCEPTED, false), None);
    }

    #[test]
    fn blank_event_text_reads_as_absent_so_its_row_is_omitted() {
        assert_eq!(event_text(None), None);
        assert_eq!(event_text(Some("   \n ".to_string())), None);
        assert_eq!(
            event_text(Some("  Agenda: ship the card  ".to_string())),
            Some("Agenda: ship the card".to_string())
        );
    }
}
