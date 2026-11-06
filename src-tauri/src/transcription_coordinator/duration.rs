use std::time::{Duration, Instant};

const SUGGEST_MEETING_AFTER: Duration = Duration::from_secs(3 * 60);
const WARNING_AFTER: Duration = Duration::from_secs(19 * 60);
const STOP_AFTER: Duration = Duration::from_secs(20 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DurationAction {
    Warn,
    Finish,
}

/// Owned by the recording stage, so stopping, cancelling, or a failed start
/// cannot leave a timer attached to the next dictation or to processing.
pub(super) struct RecordingDeadline {
    started_at: Instant,
    warned: bool,
}

impl RecordingDeadline {
    pub(super) fn new(started_at: Instant) -> Self {
        Self {
            started_at,
            warned: false,
        }
    }

    pub(super) fn next_deadline(&self) -> Instant {
        self.started_at
            + if self.warned {
                STOP_AFTER
            } else {
                WARNING_AFTER
            }
    }

    pub(super) fn action(&mut self, now: Instant) -> Option<DurationAction> {
        if now >= self.started_at + STOP_AFTER {
            Some(DurationAction::Finish)
        } else if !self.warned && now >= self.started_at + WARNING_AFTER {
            self.warned = true;
            Some(DurationAction::Warn)
        } else {
            None
        }
    }
}

/// The point a dictation has run long enough that meeting mode suits it
/// better. It comes due once per recording, and like the deadline it belongs
/// to the recording stage.
pub(super) struct MeetingSuggestion {
    due: Option<Instant>,
}

impl MeetingSuggestion {
    pub(super) fn new(started_at: Instant) -> Self {
        Self {
            due: Some(started_at + SUGGEST_MEETING_AFTER),
        }
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.due
    }

    /// The seconds recorded when the suggestion first comes due; `None`
    /// before then and every time after.
    pub(super) fn take(&mut self, now: Instant) -> Option<u32> {
        if self.due.is_some_and(|due| now >= due) {
            self.due = None;
            Some(u32::try_from(SUGGEST_MEETING_AFTER.as_secs()).unwrap_or(u32::MAX))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_is_due_at_nineteen_minutes_not_before() {
        let start = Instant::now();
        let mut deadline = RecordingDeadline::new(start);
        assert_eq!(deadline.next_deadline(), start + Duration::from_secs(1140));
        assert_eq!(deadline.action(start + Duration::from_secs(1139)), None);
        assert_eq!(
            deadline.action(start + Duration::from_secs(1140)),
            Some(DurationAction::Warn)
        );
        assert_eq!(deadline.action(start + Duration::from_secs(1141)), None);
        assert_eq!(deadline.next_deadline(), start + Duration::from_secs(1200));
    }

    #[test]
    fn finish_is_due_at_twenty_minutes_not_before() {
        let start = Instant::now();
        let mut deadline = RecordingDeadline::new(start);
        assert_eq!(
            deadline.action(start + Duration::from_secs(1140)),
            Some(DurationAction::Warn)
        );
        assert_eq!(
            deadline.action(start + Duration::from_secs(1200) - Duration::from_nanos(1)),
            None
        );
        assert_eq!(
            deadline.action(start + Duration::from_secs(1200)),
            Some(DurationAction::Finish)
        );
    }

    #[test]
    fn a_late_wakeup_finishes_without_an_outdated_warning() {
        let start = Instant::now();
        let mut deadline = RecordingDeadline::new(start);
        assert_eq!(
            deadline.action(start + Duration::from_secs(1201)),
            Some(DurationAction::Finish)
        );
    }

    #[test]
    fn another_recording_gets_its_own_full_twenty_minutes() {
        let start = Instant::now();
        let mut first = RecordingDeadline::new(start);
        assert_eq!(
            first.action(start + Duration::from_secs(1200)),
            Some(DurationAction::Finish)
        );
        let restarted = start + Duration::from_secs(1210);
        let mut second = RecordingDeadline::new(restarted);
        assert_eq!(second.action(restarted), None);
        assert_eq!(
            second.next_deadline(),
            restarted + Duration::from_secs(1140)
        );
    }
}
