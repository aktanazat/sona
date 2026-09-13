//! D20: one evening notification, on days that had one.
//!
//! The whole feature is a clock, a dedupe key, and a sentence. A thread wakes
//! every minute, and once the configured local hour has passed on a local day
//! nothing has summarized yet, it raises one `DailyDigestDue` workflow event.
//! The event's dedupe key is that local day, and the event's one successful
//! run is what settles it: the second attempt of the evening — or the first
//! after a restart at 21:00 — finds that run on record and posts nothing.
//! An attempt that fails is not the last word, and the next tick of the same
//! day tries again.
//!
//! Three gates stand between the clock and a notification, and they are
//! deliberately in different places:
//!
//! - `meeting_digest_enabled` is read here, before the event exists. A digest
//!   that is off should leave no receipts behind, not receipts that say it did
//!   nothing.
//! - The local day's dedupe key is the store's, and it is what makes "one per
//!   day" true across restarts rather than only within one process.
//! - Whether the day is worth saying anything about is [`digest_body`]'s, which
//!   is a pure function of three numbers and the only thing that decides the
//!   words.
//!
//! The notification is delivered by the OS from a Rust string and cannot reach
//! the frontend's i18next catalog, which is why the copy below is English —
//! the same constraint `PromptKind::notification_title` documents.

use super::detection::notify::{DigestOpener, PromptPresenter};
use super::learning::{no_inputs, AppLearningInputs};
use super::session::MeetingSessionManager;
use super::store::digest::MeetingDigestCounts;
use super::workflow_types::{NewWorkflowEvent, WorkflowEventKind, WorkflowId, WorkflowRunStatus};
use crate::analytics::local_days_start_utc_ms;
use chrono::{DateTime, Duration, Local, Timelike};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use std::thread;
use std::time::Duration as StdDuration;
use tauri::AppHandle;

/// One minute. The digest is due "at 18:00" to the minute, and a coarser tick
/// would make that a lie; a finer one would buy nothing a person can perceive.
const TICK: StdDuration = StdDuration::from_secs(60);

/// The notification's title. The body carries the day.
const DIGEST_TITLE: &str = "Today in Sona";

/// Asks the shell to show Capture.
///
/// Capture is where a digest lands because it is where the day already is: the
/// workflow receipt cards and the pending-suggestion card both live there. The
/// event carries no payload — there is one Capture, and it is the whole
/// request.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct CaptureRequestedEvent;

impl tauri_specta::Event for CaptureRequestedEvent {
    const NAME: &'static str = "sona:capture-requested";
}

/// The local day the digest is about, and the window that bounds it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DigestDay {
    pub local_day: String,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
}

/// The day `now` falls in, as a dedupe key and a half-open UTC window.
///
/// Both bounds go through `local_days_start_utc_ms`, which is the app's one
/// answer to "where does a local day begin" and already handles the days DST
/// gives no midnight. Asking it twice — once for today, once for tomorrow — is
/// what keeps a 23-hour day 23 hours long.
pub(crate) fn digest_day(now: DateTime<Local>) -> Option<DigestDay> {
    let start_utc_ms = local_days_start_utc_ms(now, 1).ok()?;
    let tomorrow = now.checked_add_signed(Duration::days(1))?;
    let end_utc_ms = local_days_start_utc_ms(tomorrow, 1).ok()?;
    (end_utc_ms > start_utc_ms).then(|| DigestDay {
        local_day: now.format("%Y-%m-%d").to_string(),
        start_utc_ms,
        end_utc_ms,
    })
}

/// The day to summarize, or `None` when there is nothing to do yet.
///
/// `last_raised_local_day` is this process's memory of its own work. The
/// store's dedupe key is the real guarantee; this only keeps a switched-on
/// digest from opening a transaction every minute from 18:00 until midnight.
pub(crate) fn due_digest_day(
    now: DateTime<Local>,
    enabled: bool,
    minute_of_day: u32,
    last_raised_local_day: Option<&str>,
) -> Option<DigestDay> {
    if !enabled {
        return None;
    }
    let now_minute = now.hour() * 60 + now.minute();
    if now_minute < minute_of_day {
        return None;
    }
    let day = digest_day(now)?;
    (last_raised_local_day != Some(day.local_day.as_str())).then_some(day)
}

/// The sentence, or `None` when the day does not deserve one.
///
/// "The day had activity" means something happened today: a meeting was
/// captured, or a loop was closed. Waiting suggestions and overdue handoffs
/// are backlogs rather than events, so they can join the sentence but never
/// start one — a queue nobody has emptied would otherwise raise the same
/// notification every evening until they did.
pub(crate) fn digest_body(counts: MeetingDigestCounts) -> Option<String> {
    if counts.meetings == 0 && counts.loops_closed == 0 {
        return None;
    }
    let mut clauses = Vec::with_capacity(4);
    if counts.meetings > 0 {
        clauses.push(plural(counts.meetings, "meeting", "meetings"));
    }
    if counts.loops_closed > 0 {
        clauses.push(format!(
            "{} closed",
            plural(counts.loops_closed, "loop", "loops")
        ));
    }
    if counts.suggestions_waiting > 0 {
        clauses.push(format!(
            "{} waiting",
            plural(counts.suggestions_waiting, "suggestion", "suggestions")
        ));
    }
    if counts.waiting_on_stale > 0 {
        clauses.push(format!(
            "{} overdue with others",
            plural(counts.waiting_on_stale, "loop", "loops")
        ));
    }
    Some(clauses.join(", "))
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// What one attempt to raise the digest did. Returned so the scheduler can
/// remember the day it handled, and so a test can assert on the decision
/// without a notification centre.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DigestOutcome {
    /// The day's run was already on record, or nothing was switched on to
    /// run; nothing ran now.
    AlreadyRaised,
    /// The day was counted and had nothing worth saying.
    Quiet,
    /// A notification was handed to the OS.
    Notified,
    /// A notification was shaped but the OS would not take it — no bundle, or
    /// no authorization.
    Undelivered,
}

impl MeetingSessionManager {
    /// Starts the once-a-minute clock.
    ///
    /// Its own thread, like the retention sweeper: every tick that is not the
    /// digest's minute is a settings read and two integer comparisons, and the
    /// one that is blocks on a database transaction that must not sit on the
    /// async runtime.
    pub(crate) fn start_digest_scheduler(
        self: &Arc<Self>,
        app: AppHandle,
        prompts: Arc<dyn PromptPresenter>,
    ) {
        let manager = Arc::clone(self);
        thread::spawn(move || {
            // This process's memory of a day it finished with. Filled only by
            // a completed attempt: one that failed leaves it empty, so the
            // next tick of the same day tries again, and what the store
            // already holds for that day is not run twice.
            let mut last_raised = None::<String>;
            loop {
                thread::sleep(TICK);
                let settings = crate::settings::get_settings(&app);
                let Some(day) = due_digest_day(
                    Local::now(),
                    settings.meeting_digest_enabled,
                    settings.meeting_digest_minute_of_day,
                    last_raised.as_deref(),
                ) else {
                    continue;
                };
                match manager.raise_digest(&app, prompts.as_ref(), &day) {
                    Ok(outcome) => {
                        log::info!("Evening digest for {}: {outcome:?}", day.local_day.as_str());
                        last_raised = Some(day.local_day);
                    }
                    Err(error) => log::warn!("Evening digest could not run: {error:?}"),
                }
            }
        });
    }

    /// Records the day's event, runs it, and posts the sentence it produced.
    ///
    /// The event on record is the day's claim, not its digest: the run after
    /// the insert can fail, and the process can stop between the two. So the
    /// run is what settles the day. A day whose run already succeeded has no
    /// work left and hands back no receipt, which is the one-per-day
    /// guarantee across restarts; a failed run is not terminal for this kind,
    /// so the next call runs it again.
    ///
    /// The counts come back out of the run's own receipt rather than from a
    /// second query. The receipt is what a reader — or an agent — can go and
    /// check afterwards, and a notification whose numbers came from somewhere
    /// else would be a second, unverifiable answer.
    pub(crate) fn raise_digest(
        &self,
        app: &AppHandle,
        prompts: &dyn PromptPresenter,
        day: &DigestDay,
    ) -> Result<DigestOutcome, super::store::StoreError> {
        let store = tauri::async_runtime::block_on(self.store())
            .map_err(|_| super::store::StoreError::Unavailable)?;
        let dispatch = store.record_workflow_event(NewWorkflowEvent {
            kind: WorkflowEventKind::DailyDigestDue,
            payload: serde_json::json!({
                "local_day": &day.local_day,
                "day_start_utc_ms": day.start_utc_ms,
                "day_end_utc_ms": day.end_utc_ms,
            }),
            occurred_at_utc_ms: chrono::Utc::now().timestamp_millis(),
            source: "meeting_digest",
            dedupe_key: format!("daily-digest:{}", day.local_day),
        })?;
        let inputs = AppLearningInputs::resolve(Some(app));
        let receipts = match inputs {
            Some(inputs) => store.run_workflow_event(dispatch.event_id, false, &inputs),
            None => store.run_workflow_event(dispatch.event_id, false, &no_inputs()),
        }?;
        let Some(receipt) = receipts
            .iter()
            .find(|receipt| receipt.workflow_id == WorkflowId::DailyDigest)
        else {
            return Ok(DigestOutcome::AlreadyRaised);
        };
        if receipt.status != WorkflowRunStatus::Ok {
            return Err(super::store::StoreError::Unavailable);
        }
        let counts = MeetingDigestCounts {
            meetings: receipt.outcome_counts.meetings,
            loops_closed: receipt.outcome_counts.loops_closed,
            suggestions_waiting: receipt.outcome_counts.suggestions_waiting,
            waiting_on_stale: receipt.outcome_counts.waiting_on_stale,
        };
        let Some(body) = digest_body(counts) else {
            return Ok(DigestOutcome::Quiet);
        };
        Ok(if prompts.present_digest(DIGEST_TITLE, &body) {
            DigestOutcome::Notified
        } else {
            DigestOutcome::Undelivered
        })
    }
}

/// Where a digest click lands.
pub(crate) struct CaptureOpener {
    app: AppHandle,
}

impl CaptureOpener {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl DigestOpener for CaptureOpener {
    fn digest_opened(&self) {
        crate::show_main_window(&self.app);
        <CaptureRequestedEvent as tauri_specta::Event>::emit(&CaptureRequestedEvent, &self.app)
            .unwrap_or_else(|error| {
                log::warn!("The digest could not ask for Capture: {error}");
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn counts(
        meetings: u64,
        loops_closed: u64,
        suggestions_waiting: u64,
        waiting_on_stale: u64,
    ) -> MeetingDigestCounts {
        MeetingDigestCounts {
            meetings,
            loops_closed,
            suggestions_waiting,
            waiting_on_stale,
        }
    }

    fn at(hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 8, 31, hour, minute, 0)
            .single()
            .expect("a real local instant")
    }

    #[test]
    fn the_sentence_is_the_counts_the_day_actually_had() {
        assert_eq!(
            digest_body(counts(2, 3, 1, 4)).as_deref(),
            Some("2 meetings, 3 loops closed, 1 suggestion waiting, 4 loops overdue with others")
        );
    }

    #[test]
    fn one_of_a_thing_is_singular() {
        assert_eq!(
            digest_body(counts(1, 1, 1, 1)).as_deref(),
            Some("1 meeting, 1 loop closed, 1 suggestion waiting, 1 loop overdue with others")
        );
    }

    #[test]
    fn a_clause_worth_zero_is_left_out_rather_than_printed() {
        assert_eq!(
            digest_body(counts(2, 0, 0, 0)).as_deref(),
            Some("2 meetings")
        );
        assert_eq!(
            digest_body(counts(0, 4, 0, 0)).as_deref(),
            Some("4 loops closed")
        );
    }

    /// A backlog is not news. Overdue handoffs join a sentence the day earned
    /// and never raise one on their own, or the same notification would arrive
    /// every evening until somebody else answered their email.
    #[test]
    fn overdue_handoffs_join_a_sentence_but_never_start_one() {
        assert_eq!(digest_body(counts(0, 0, 0, 9)), None);
        assert_eq!(
            digest_body(counts(1, 0, 0, 9)).as_deref(),
            Some("1 meeting, 9 loops overdue with others")
        );
    }

    #[test]
    fn a_day_with_nothing_in_it_has_no_sentence() {
        assert_eq!(digest_body(counts(0, 0, 0, 0)), None);
    }

    /// A suggestion queue nobody has emptied is a backlog, not today's news.
    /// Letting it raise a notification would nag every evening until it was
    /// cleared, which is the behavior this product does not have.
    #[test]
    fn waiting_suggestions_join_a_sentence_but_never_start_one() {
        assert_eq!(digest_body(counts(0, 0, 7, 0)), None);
        assert_eq!(
            digest_body(counts(1, 0, 7, 0)).as_deref(),
            Some("1 meeting, 7 suggestions waiting")
        );
    }

    #[test]
    fn the_digest_is_not_due_before_its_minute() {
        assert_eq!(due_digest_day(at(17, 59), true, 18 * 60, None), None);
        assert!(due_digest_day(at(18, 0), true, 18 * 60, None).is_some());
        assert!(due_digest_day(at(23, 30), true, 18 * 60, None).is_some());
    }

    #[test]
    fn a_switched_off_digest_is_never_due() {
        assert_eq!(due_digest_day(at(21, 0), false, 18 * 60, None), None);
    }

    #[test]
    fn the_day_already_raised_is_not_raised_again() {
        let day = due_digest_day(at(18, 0), true, 18 * 60, None).expect("due at the hour");
        assert_eq!(day.local_day, "2026-08-31");
        assert_eq!(
            due_digest_day(at(19, 0), true, 18 * 60, Some(&day.local_day)),
            None
        );
        // A different day is a different digest, memory or not.
        assert!(due_digest_day(at(19, 0), true, 18 * 60, Some("2026-08-30")).is_some());
    }

    #[test]
    fn the_window_is_one_local_day_wide() {
        let day = digest_day(at(18, 0)).expect("a real day");
        assert_eq!(day.end_utc_ms - day.start_utc_ms, 24 * 60 * 60 * 1_000);
        assert!(day.start_utc_ms <= at(18, 0).timestamp_millis());
        assert!(day.end_utc_ms > at(23, 59).timestamp_millis());
    }
}
