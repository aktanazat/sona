//! Clipboard recency, without a clipboard-content poller.
//!
//! macOS does not give a pasteboard item's timestamp. The closest reliable
//! primitive is its monotonically changing `changeCount`, so this module samples
//! that cheap generation number once per second while a Full-context mode exists.
//! A text value is read only by the run that needs it, and only when a generation
//! change is known to have happened inside that run's pre-roll window.

use super::{ContextSourceStatus, SourceOutcome};
use std::sync::{Condvar, LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How long before record-start a copy still counts as part of the dictation,
/// unless the user has narrowed or widened it. Three seconds is short enough
/// that an unrelated copy from earlier in the session never reaches a prompt.
pub const DEFAULT_CLIPBOARD_PREROLL_MS: u64 = 3_000;
/// A changed count is fresh only when the observer checked often enough to put
/// an honest upper bound on its age. This lets scheduling hiccups degrade to
/// `Stale` rather than claiming an old clipboard is recent.
const MAX_OBSERVATION_GAP_MS: u64 = 2_500;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Generation {
    count: Option<i64>,
    changed_at_ms: Option<u64>,
}

impl Generation {
    /// Whether the last observed change is provably inside `preroll_ms` of
    /// `now_ms`. An unknown change time is never treated as recent.
    pub(crate) fn is_fresh(self, now_ms: u64, preroll_ms: u64) -> bool {
        self.count.is_some()
            && self
                .changed_at_ms
                .is_some_and(|changed_at| now_ms.saturating_sub(changed_at) <= preroll_ms)
    }

    fn matches(self, current: Option<i64>) -> bool {
        self.count.is_some_and(|count| current == Some(count))
    }
}
/// Reads content only after proving the observed generation is fresh and still
/// current. The second generation check closes the race with a later copy.
pub(crate) fn read_if_fresh(
    generation: Generation,
    now_ms: u64,
    preroll_ms: u64,
    mut current_generation: impl FnMut() -> Option<i64>,
    read_text: impl FnOnce() -> Option<String>,
) -> SourceOutcome {
    if !generation.is_fresh(now_ms, preroll_ms) || !generation.matches(current_generation()) {
        return SourceOutcome::Unavailable(ContextSourceStatus::Stale);
    }

    let text = read_text();
    if !generation.matches(current_generation()) {
        return SourceOutcome::Unavailable(ContextSourceStatus::Stale);
    }

    SourceOutcome::read(text)
}
#[cfg(test)]
pub(super) fn generation_for_test(count: i64, changed_at_ms: u64) -> Generation {
    Generation {
        count: Some(count),
        changed_at_ms: Some(changed_at_ms),
    }
}

#[derive(Debug, Default)]
struct State {
    enabled: bool,
    worker_started: bool,
    last_count: Option<i64>,
    last_observed_at_ms: Option<u64>,
    changed_at_ms: Option<u64>,
}

static RECENCY: LazyLock<(Mutex<State>, Condvar)> =
    LazyLock::new(|| (Mutex::new(State::default()), Condvar::new()));

/// Starts or stops generation-only polling. The worker is created at most once
/// and sleeps on a condition variable while no stored mode can use clipboard
/// context, so the default (all non-Full modes) has no periodic work.
pub fn set_clipboard_watch_enabled(enabled: bool) {
    let (lock, wake) = &*RECENCY;
    let start_worker = match lock.lock() {
        Ok(mut state) => {
            state.enabled = enabled;
            if !enabled {
                // A later enable needs a new baseline; stale old knowledge must
                // not make an untouched clipboard look fresh.
                state.last_count = None;
                state.last_observed_at_ms = None;
                state.changed_at_ms = None;
            }
            let start = enabled && !state.worker_started;
            if start {
                state.worker_started = true;
            }
            start
        }
        Err(_) => return,
    };
    wake.notify_all();

    if start_worker {
        let _ = std::thread::Builder::new()
            .name("sona-clipboard-recency".to_string())
            .spawn(watch_generations);
    }
}

pub(crate) fn watch_enabled() -> bool {
    RECENCY.0.lock().map(|state| state.enabled).unwrap_or(false)
}

/// Takes a run-start generation snapshot. It does not read clipboard content.
pub(crate) fn observe_clipboard_generation() -> Generation {
    let (lock, _) = &*RECENCY;
    let Ok(mut state) = lock.lock() else {
        return Generation::default();
    };
    observe_if_enabled(&mut state, now_ms(), platform_generation)
}

fn watch_generations() {
    loop {
        let (lock, wake) = &*RECENCY;
        let mut state = match lock.lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        while !state.enabled {
            state = match wake.wait(state) {
                Ok(state) => state,
                Err(_) => return,
            };
        }

        let now = now_ms();
        let _ = observe_if_enabled(&mut state, now, platform_generation);
        if wake.wait_timeout(state, POLL_INTERVAL).is_err() {
            return;
        }
    }
}

fn observe_if_enabled(
    state: &mut State,
    now: u64,
    read_generation: impl FnOnce() -> Option<i64>,
) -> Generation {
    if !state.enabled {
        return Generation::default();
    }
    observe_locked(state, now, read_generation());
    Generation {
        count: state.last_count,
        changed_at_ms: state.changed_at_ms,
    }
}

fn observe_locked(state: &mut State, now: u64, count: Option<i64>) {
    let prior_count = state.last_count;
    let prior_observation = state.last_observed_at_ms;
    let observation_gap = prior_observation.map(|then| now.saturating_sub(then));

    if count != prior_count {
        state.changed_at_ms = match (prior_count, count, observation_gap) {
            // The first sample only establishes a baseline. We do not know when
            // that content was copied, so we must not call it fresh.
            (None, Some(_), _) => None,
            // We observed a different generation recently enough to prove its
            // change happened inside the recency window.
            (Some(_), Some(_), Some(gap)) if gap <= MAX_OBSERVATION_GAP_MS => Some(now),
            // The pasteboard disappeared or we missed too much time. Lose
            // freshness rather than stretching an old observation.
            _ => None,
        };
        state.last_count = count;
    }
    state.last_observed_at_ms = Some(now);
}

#[cfg(target_os = "macos")]
fn platform_generation() -> Option<i64> {
    use objc2_app_kit::NSPasteboard;

    // `changeCount` is a scalar property of the general pasteboard; unlike
    // reading data, this never copies the user's contents into Sona.
    //
    // The pool is for `generalPasteboard()`: this runs on the long-lived watcher
    // thread and on the run-start caller, neither of which has a pool, so an
    // autoreleased return here would be held for the thread's whole life.
    objc2::rc::autoreleasepool(|_| {
        i64::try_from(NSPasteboard::generalPasteboard().changeCount()).ok()
    })
}

#[cfg(not(target_os = "macos"))]
fn platform_generation() -> Option<i64> {
    None
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_watch_performs_no_platform_read() {
        let mut state = State::default();
        let mut reads = 0;
        let generation = observe_if_enabled(&mut state, 1_000, || {
            reads += 1;
            Some(8)
        });

        assert_eq!(generation, Generation::default());
        assert_eq!(reads, 0);
    }

    #[test]
    fn first_observation_is_a_baseline_not_a_claim_of_freshness() {
        let mut state = State::default();
        observe_locked(&mut state, 1_000, Some(8));
        assert_eq!(state.changed_at_ms, None);
    }

    #[test]
    fn closely_observed_change_is_fresh_inside_the_preroll() {
        let mut state = State {
            last_count: Some(8),
            last_observed_at_ms: Some(1_000),
            ..State::default()
        };
        observe_locked(&mut state, 2_000, Some(9));
        let generation = Generation {
            count: state.last_count,
            changed_at_ms: state.changed_at_ms,
        };
        assert!(generation.is_fresh(4_999, DEFAULT_CLIPBOARD_PREROLL_MS));
        assert!(!generation.is_fresh(5_001, DEFAULT_CLIPBOARD_PREROLL_MS));
    }

    /// The window is the run's frozen setting, not a module constant: a copy
    /// older than the configured pre-roll is excluded even when it would have
    /// passed the default.
    #[test]
    fn a_copy_older_than_the_configured_preroll_is_excluded() {
        let mut state = State {
            last_count: Some(8),
            last_observed_at_ms: Some(1_000),
            ..State::default()
        };
        observe_locked(&mut state, 2_000, Some(9));
        let generation = Generation {
            count: state.last_count,
            changed_at_ms: state.changed_at_ms,
        };
        assert!(generation.is_fresh(2_500, 1_000));
        assert!(!generation.is_fresh(3_500, 1_000));
        assert!(generation.is_fresh(3_500, 5_000));
    }

    #[test]
    fn delayed_observation_does_not_invent_a_recent_copy() {
        let mut state = State {
            last_count: Some(8),
            last_observed_at_ms: Some(1_000),
            ..State::default()
        };
        observe_locked(&mut state, 4_000, Some(9));
        assert_eq!(state.changed_at_ms, None);
    }

    #[test]
    fn equal_generation_keeps_the_original_change_time() {
        let mut state = State {
            last_count: Some(8),
            last_observed_at_ms: Some(1_000),
            changed_at_ms: Some(900),
            ..State::default()
        };
        observe_locked(&mut state, 2_000, Some(8));
        assert_eq!(state.changed_at_ms, Some(900));
    }

    #[test]
    fn disappeared_pasteboard_clears_freshness() {
        let mut state = State {
            last_count: Some(8),
            last_observed_at_ms: Some(1_000),
            changed_at_ms: Some(900),
            ..State::default()
        };
        observe_locked(&mut state, 2_000, None);
        assert_eq!(state.changed_at_ms, None);
        assert_eq!(state.last_count, None);
    }
    #[test]
    fn fresh_preroll_reads_content_between_stable_generation_checks() {
        let generation = Generation {
            count: Some(9),
            changed_at_ms: Some(2_000),
        };
        let events = std::cell::RefCell::new(Vec::new());

        let outcome = read_if_fresh(
            generation,
            5_000,
            DEFAULT_CLIPBOARD_PREROLL_MS,
            || {
                events.borrow_mut().push("generation");
                Some(9)
            },
            || {
                events.borrow_mut().push("content");
                Some("copied inside the frozen pre-roll".to_string())
            },
        );

        assert_eq!(
            outcome,
            SourceOutcome::Captured("copied inside the frozen pre-roll".to_string())
        );
        assert_eq!(
            events.into_inner(),
            ["generation", "content", "generation"],
            "the content read stays between the two generation checks"
        );
    }

    #[test]
    fn stale_or_mismatched_generation_never_reads_content() {
        let stale = Generation {
            count: Some(9),
            changed_at_ms: Some(2_000),
        };
        let mut generation_reads = 0;
        let mut content_reads = 0;
        let outcome = read_if_fresh(
            stale,
            5_001,
            DEFAULT_CLIPBOARD_PREROLL_MS,
            || {
                generation_reads += 1;
                Some(9)
            },
            || {
                content_reads += 1;
                Some("old clipboard".to_string())
            },
        );
        assert_eq!(
            outcome,
            SourceOutcome::Unavailable(ContextSourceStatus::Stale)
        );
        assert_eq!(
            generation_reads, 0,
            "a stale observation does not query the pasteboard"
        );
        assert_eq!(
            content_reads, 0,
            "a stale observation does not read content"
        );

        let fresh = Generation {
            count: Some(9),
            changed_at_ms: Some(2_000),
        };
        let outcome = read_if_fresh(
            fresh,
            5_000,
            DEFAULT_CLIPBOARD_PREROLL_MS,
            || Some(10),
            || {
                content_reads += 1;
                Some("different copy".to_string())
            },
        );
        assert_eq!(
            outcome,
            SourceOutcome::Unavailable(ContextSourceStatus::Stale)
        );
        assert_eq!(
            content_reads, 0,
            "a generation mismatch fails before content is read"
        );
    }

    #[test]
    fn a_copy_racing_the_content_read_fails_closed() {
        let generation = Generation {
            count: Some(9),
            changed_at_ms: Some(2_000),
        };
        let mut observed = [Some(9), Some(10)].into_iter();
        let mut content_reads = 0;

        let outcome = read_if_fresh(
            generation,
            5_000,
            DEFAULT_CLIPBOARD_PREROLL_MS,
            || observed.next().expect("two generation observations"),
            || {
                content_reads += 1;
                Some("copy that lost the race".to_string())
            },
        );

        assert_eq!(
            outcome,
            SourceOutcome::Unavailable(ContextSourceStatus::Stale)
        );
        assert_eq!(
            content_reads, 1,
            "the second generation check discards a raced content read"
        );
    }
}
