use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

struct LaunchClock {
    started: Instant,
}

static CLOCK: OnceLock<LaunchClock> = OnceLock::new();
static KEYRING: AtomicU8 = AtomicU8::new(0);
static UPDATE_CHECK: AtomicU8 = AtomicU8::new(0);

pub fn start() {
    let _ = CLOCK.set(LaunchClock {
        started: Instant::now(),
    });
}

pub fn span(name: &'static str) -> LaunchSpan {
    LaunchSpan::new(name, None)
}

pub fn keyring_span() -> LaunchSpan {
    LaunchSpan::new("keyring", Some(&KEYRING))
}

pub fn update_check_span() -> LaunchSpan {
    LaunchSpan::new("update_check", Some(&UPDATE_CHECK))
}

fn elapsed_ms() -> f64 {
    CLOCK
        .get()
        .map(|clock| clock.started.elapsed().as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

fn log_span(name: &'static str, start_ms: f64, end_ms: f64) {
    log::info!(
        target: "sona::launch",
        "launch_span name={name} start_ms={start_ms:.3} end_ms={end_ms:.3} duration_ms={:.3}",
        end_ms - start_ms,
    );
}

pub struct LaunchSpan {
    name: &'static str,
    started: Option<(Instant, f64)>,
    once: Option<&'static AtomicU8>,
}

impl LaunchSpan {
    fn new(name: &'static str, once: Option<&'static AtomicU8>) -> Self {
        let enabled = CLOCK.get().is_some()
            && once.is_none_or(|state| {
                state
                    .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            });
        Self {
            name,
            started: enabled.then(|| (Instant::now(), elapsed_ms())),
            once,
        }
    }
}

impl Drop for LaunchSpan {
    fn drop(&mut self) {
        let Some((started, start_ms)) = self.started else {
            return;
        };
        log_span(
            self.name,
            start_ms,
            start_ms + started.elapsed().as_secs_f64() * 1000.0,
        );
        if let Some(state) = self.once {
            state.store(2, Ordering::Release);
        }
    }
}
