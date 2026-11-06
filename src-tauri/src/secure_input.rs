//! macOS Secure Event Input detection, monitoring, and fallback.
//!
//! When any process enables secure event input (password fields, Terminal's
//! "Secure Keyboard Entry", a stuck `loginwindow`), CGEventTaps stop receiving
//! KeyDown/KeyUp events while FlagsChanged still flows. The handy-keys
//! implementation is tap-based, so keyed shortcuts (e.g. Option+Space) die
//! silently while modifier-only shortcuts keep working. See issue #1578.
//!
//! This module:
//! - polls `IsSecureEventInputEnabled()` and tracks state transitions
//! - looks up the holding process (best effort — Apple documents no reliable
//!   API; the IORegistry PID is frequently wrong or absent)
//! - while secure input is sustained, shadow-registers vulnerable *keyed*
//!   bindings through the Carbon-backed Tauri global-shortcut path, which is
//!   not affected by secure input (modifier-only bindings need no fallback)
//! - dynamically shadows the cancel keys the open recording owns, so they
//!   remain available under secure input
//! - exposes a count-only keyboard diagnostic for the debug window. Only
//!   event *kinds* are counted — key identity is never logged or returned.

use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone, Serialize, Type)]
pub struct SecureInputStatus {
    /// Secure input is currently enabled (live check)
    pub enabled: bool,
    /// Enabled continuously long enough to be considered stuck (not just a
    /// password field gaining momentary focus)
    pub sustained: bool,
    pub culprit_pid: Option<i32>,
    pub culprit_name: Option<String>,
    /// Carbon fallback registrations are currently active
    pub fallback_active: bool,
    /// Binding ids shadow-registered with identical semantics
    pub covered_bindings: Vec<String>,
    /// Side-specific binding ids widened to match either side while shadowed
    pub degraded_bindings: Vec<String>,
    /// Binding ids that cannot fire at all (e.g. fn+key, registration failure)
    pub uncovered_bindings: Vec<String>,
    /// The user tried to record a shortcut while secure input was active.
    /// Treated as user impact even when every binding is covered, so the
    /// warning banner appears and explains why recording refused.
    pub recorder_blocked: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct KeyboardDiagnosticReport {
    pub secure_input_enabled: bool,
    pub culprit_pid: Option<i32>,
    pub culprit_name: Option<String>,
    /// Counts only — key identity is deliberately never captured.
    pub key_down: u32,
    pub key_up: u32,
    pub flags_changed: u32,
    pub mouse: u32,
    pub duration_ms: u32,
}

#[tauri::command]
#[specta::specta]
pub fn get_secure_input_status(app: AppHandle) -> SecureInputStatus {
    imp::status(&app)
}

#[tauri::command]
#[specta::specta]
pub async fn run_keyboard_diagnostic(
    duration_secs: Option<u32>,
) -> Result<KeyboardDiagnosticReport, String> {
    imp::run_diagnostic(duration_secs.unwrap_or(10).clamp(3, 30)).await
}

/// True if secure input is enabled right now (live check, macOS only).
pub fn is_enabled_now() -> bool {
    imp::is_enabled()
}

/// Record that a shortcut-recording attempt was refused because secure input
/// is active. Flips the warning state so the banner/tray explain the refusal
/// even when every registered binding is covered by the fallback.
pub fn note_recorder_blocked(app: &AppHandle) {
    imp::note_recorder_blocked(app)
}

/// Re-shadow the cancel keys after the open recording's set changed.
pub fn cancel_keys_changed(app: &AppHandle) {
    imp::cancel_keys_changed(app)
}

/// Rebuild Carbon fallback registrations from the current settings and
/// lifecycle state. This is called after shortcut-related settings change.
pub fn reconcile_fallback(app: &AppHandle) {
    imp::reconcile_fallback(app)
}

/// Managed state + monitor startup. On non-macOS platforms the state exists
/// but the monitor never runs and everything reports disabled.
pub fn init(app: &AppHandle) {
    app.manage(imp::SecureInputState::new());
    imp::start_monitor(app);
}

/// Whether the tray should show the warning badge / menu entry.
///
/// Only when the user is actually impacted: a binding is degraded or dead.
/// When every affected binding is covered transparently by the fallback (or
/// none are affected), the experience is seamless and nothing is shown.
pub fn tray_warning_active(app: &AppHandle) -> bool {
    app.try_state::<imp::SecureInputState>()
        .map(|s| s.warning_active())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use crate::settings::{self, KeyboardImplementation, ShortcutBinding};
    use log::{debug, error, info, warn};
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    /// How often the monitor thread polls.
    const POLL_INTERVAL: Duration = Duration::from_secs(1);
    /// Secure input must be held this long before we treat it as stuck.
    /// Momentary activation (a password field gaining focus) is normal.
    const SUSTAIN_THRESHOLD: Duration = Duration::from_secs(3);

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        // Carbon HIToolbox; Boolean is an unsigned char
        fn IsSecureEventInputEnabled() -> u8;
    }

    pub fn is_enabled() -> bool {
        // Carbon's query takes no pointers and retains no state.
        // SAFETY: it is documented to be callable while the application runs.
        unsafe { IsSecureEventInputEnabled() != 0 }
    }

    #[derive(Debug, Clone)]
    struct Culprit {
        pid: i32,
        name: String,
    }

    #[derive(Default)]
    struct FallbackState {
        /// Bindings shadow-registered through the Tauri/Carbon path (possibly
        /// with widened modifiers), kept so deactivation unregisters the
        /// exact strings we registered.
        registered: Vec<ShortcutBinding>,
        /// Shadowed with identical semantics
        covered: Vec<String>,
        /// Shadowed, but side-specific modifiers widened to either side
        degraded: Vec<String>,
        /// Cannot fire at all while secure input is held
        uncovered: Vec<String>,
    }
    /// The time-only portion of Secure Input monitoring. The monitor supplies
    /// a monotonic duration, which keeps the threshold boundary independent of
    /// its polling and Carbon I/O.
    #[derive(Default)]
    struct SustainedInput {
        enabled_since: Option<Duration>,
        active: bool,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FallbackTransition {
        Register,
        Unregister,
    }

    impl SustainedInput {
        fn observe(&mut self, enabled: bool, now: Duration) -> Option<FallbackTransition> {
            if enabled {
                let enabled_since = self.enabled_since.get_or_insert(now);
                if !self.active && now.saturating_sub(*enabled_since) >= SUSTAIN_THRESHOLD {
                    self.active = true;
                    return Some(FallbackTransition::Register);
                }
                return None;
            }

            self.enabled_since = None;
            if std::mem::replace(&mut self.active, false) {
                Some(FallbackTransition::Unregister)
            } else {
                None
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum SecureInputStateLock {
        SustainedInput,
        Culprit,
        Fallback,
        FallbackOperation,
    }

    fn state_lock<'a, T>(
        mutex: &'a Mutex<T>,
        kind: SecureInputStateLock,
    ) -> Result<MutexGuard<'a, T>, SecureInputStateLock> {
        mutex.lock().map_err(|_| kind)
    }

    pub struct SecureInputState {
        enabled: AtomicBool,
        sustained: AtomicBool,
        clock_origin: Instant,
        sustained_input: Mutex<SustainedInput>,
        culprit: Mutex<Option<Culprit>>,
        fallback: Mutex<FallbackState>,
        /// Serializes fallback registration changes without requiring the
        /// fallback state lock to be held across global-shortcut plugin calls.
        fallback_operation: Mutex<()>,
        recorder_blocked: AtomicBool,
        monitor_started: AtomicBool,
    }

    impl SecureInputState {
        pub fn new() -> Self {
            Self {
                enabled: AtomicBool::new(false),
                sustained: AtomicBool::new(false),
                clock_origin: Instant::now(),
                sustained_input: Mutex::new(SustainedInput::default()),
                culprit: Mutex::new(None),
                fallback: Mutex::new(FallbackState::default()),
                fallback_operation: Mutex::new(()),
                recorder_blocked: AtomicBool::new(false),
                monitor_started: AtomicBool::new(false),
            }
        }

        pub fn is_sustained(&self) -> bool {
            self.sustained.load(Ordering::SeqCst)
        }

        /// User-visible impact exists: some binding is degraded or dead, or
        /// the user ran into the blocked shortcut recorder.
        pub fn warning_active(&self) -> bool {
            if self.recorder_blocked.load(Ordering::SeqCst) {
                return true;
            }
            if !self.is_sustained() {
                return false;
            }
            let fallback = match state_lock(&self.fallback, SecureInputStateLock::Fallback) {
                Ok(fallback) => fallback,
                Err(lock) => {
                    error!("SecureInput {lock:?} lock poisoned; showing the warning");
                    return true;
                }
            };
            !fallback.degraded.is_empty() || !fallback.uncovered.is_empty()
        }
    }
    fn sample_sustained_input(
        state: &SecureInputState,
        enabled: bool,
    ) -> Option<FallbackTransition> {
        let now = state.clock_origin.elapsed();
        match state_lock(&state.sustained_input, SecureInputStateLock::SustainedInput) {
            Ok(mut sustained_input) => {
                let transition = sustained_input.observe(enabled, now);
                // Mirror the authority after every sample, not only when a
                // transition is emitted. The tray and `warning_active` read
                // this atomic, and tying the copy to which transitions exist
                // is what would let the two drift apart without a signal.
                state
                    .sustained
                    .store(sustained_input.active, Ordering::SeqCst);
                transition
            }
            Err(lock) => {
                error!("SecureInput {lock:?} lock poisoned while sampling duration");
                if !enabled && state.sustained.swap(false, Ordering::SeqCst) {
                    Some(FallbackTransition::Unregister)
                } else {
                    None
                }
            }
        }
    }

    /// Best-effort culprit lookup via the IORegistry session property.
    /// Apple documents no reliable API for this; the PID may be missing
    /// (an app quit while holding secure input) or point at the wrong
    /// process (often the responsible parent, or `loginwindow`).
    fn lookup_culprit() -> Option<Culprit> {
        let out = Command::new("ioreg")
            .args(["-l", "-w", "0"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let pid: i32 = text
            .lines()
            .find_map(|l| l.split("\"kCGSSessionSecureInputPID\"=").nth(1))?
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .ok()?;

        // `ps -o comm=` returns the full executable path; show just the
        // binary name ("Terminal", not ".../Terminal.app/Contents/MacOS/Terminal")
        let name = Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
            .ok()
            .and_then(|o| {
                let raw = String::from_utf8_lossy(&o.stdout);
                let trimmed = raw.trim();
                (!trimmed.is_empty())
                    .then(|| trimmed.rsplit('/').next().unwrap_or(trimmed).to_string())
            })
            .unwrap_or_else(|| "(process no longer running)".to_string());

        Some(Culprit { pid, name })
    }

    pub fn status(app: &AppHandle) -> SecureInputStatus {
        let enabled = is_enabled();
        let state = app.state::<SecureInputState>();

        // Culprit discovery shells out to ioreg and is intentionally performed
        // only by the monitor (or the blocking diagnostic), never by this
        // synchronous Tauri command.
        let culprit = match state_lock(&state.culprit, SecureInputStateLock::Culprit) {
            Ok(culprit) => culprit.clone(),
            Err(lock) => {
                error!("SecureInput {lock:?} lock poisoned while reading status");
                None
            }
        };
        let (fallback_active, covered_bindings, degraded_bindings, uncovered_bindings) =
            match state_lock(&state.fallback, SecureInputStateLock::Fallback) {
                Ok(fallback) => (
                    !fallback.registered.is_empty(),
                    fallback.covered.clone(),
                    fallback.degraded.clone(),
                    fallback.uncovered.clone(),
                ),
                Err(lock) => {
                    error!("SecureInput {lock:?} lock poisoned while reading status");
                    (false, Vec::new(), Vec::new(), Vec::new())
                }
            };
        SecureInputStatus {
            enabled,
            sustained: state.sustained.load(Ordering::SeqCst),
            culprit_pid: culprit.as_ref().map(|culprit| culprit.pid),
            culprit_name: culprit.map(|culprit| culprit.name),
            fallback_active,
            covered_bindings,
            degraded_bindings,
            uncovered_bindings,
            recorder_blocked: state.recorder_blocked.load(Ordering::SeqCst),
        }
    }

    pub fn note_recorder_blocked(app: &AppHandle) {
        let state = app.state::<SecureInputState>();
        if !state.recorder_blocked.swap(true, Ordering::SeqCst) {
            warn!("SecureInput: shortcut recording attempt blocked — surfacing warning");
            refresh_tray(app);
            emit_status(app);
        }
    }

    fn emit_status(app: &AppHandle) {
        let payload = status(app);
        if let Err(e) = app.emit("secure-input-changed", &payload) {
            error!("Failed to emit secure-input-changed: {e}");
        }
    }

    fn refresh_tray(app: &AppHandle) {
        // No-op before the tray is built; otherwise a diffed, coalesced,
        // main-thread apply that never blocks this thread.
        crate::tray::refresh_tray_icon(app);
    }

    pub fn start_monitor(app: &AppHandle) {
        let state = app.state::<SecureInputState>();
        if state.monitor_started.swap(true, Ordering::SeqCst) {
            return;
        }

        let app = app.clone();
        std::thread::spawn(move || {
            info!("secure-input monitor started");
            loop {
                std::thread::sleep(POLL_INTERVAL);
                let state = app.state::<SecureInputState>();
                let now_enabled = is_enabled();
                let was_enabled = state.enabled.swap(now_enabled, Ordering::SeqCst);
                let fallback_transition = sample_sustained_input(&state, now_enabled);

                if now_enabled && !was_enabled {
                    let culprit = lookup_culprit();
                    match &culprit {
                        Some(c) => {
                            info!("SecureInput ENABLED (held by pid {} '{}')", c.pid, c.name)
                        }
                        None => info!("SecureInput ENABLED (no visible holder)"),
                    }
                    match state_lock(&state.culprit, SecureInputStateLock::Culprit) {
                        Ok(mut state_culprit) => *state_culprit = culprit,
                        Err(lock) => error!("SecureInput {lock:?} lock poisoned while enabling"),
                    }
                }

                if !now_enabled {
                    // Clear recorder impact on every disabled sample. A short
                    // Secure Input episode can otherwise occur entirely
                    // between polls and leave this flag latched indefinitely.
                    let was_blocked = state.recorder_blocked.swap(false, Ordering::SeqCst);
                    if was_enabled {
                        info!("SecureInput DISABLED");
                        match state_lock(&state.culprit, SecureInputStateLock::Culprit) {
                            Ok(mut state_culprit) => *state_culprit = None,
                            Err(lock) => {
                                error!("SecureInput {lock:?} lock poisoned while disabling")
                            }
                        }
                    }

                    if matches!(fallback_transition, Some(FallbackTransition::Unregister)) {
                        reconcile_fallback(&app);
                    } else if was_enabled || was_blocked {
                        refresh_tray(&app);
                        emit_status(&app);
                    }
                    continue;
                }

                if matches!(fallback_transition, Some(FallbackTransition::Register)) {
                    warn!(
                        "SecureInput held for {}s — keyed shortcuts are blocked; activating fallback",
                        SUSTAIN_THRESHOLD.as_secs()
                    );
                    reconcile_fallback(&app);
                }
            }
        });
    }

    fn is_mouse_key(key: &handy_keys::Key) -> bool {
        key.to_string().to_lowercase().starts_with("mouse")
    }

    /// Build the Carbon-registrable equivalent of a keyed hotkey.
    ///
    /// Carbon has no concept of left/right modifiers, so side-specific
    /// modifiers widen to the whole group — returned as `degraded: true` so
    /// the UI can call out the changed matching. The fn key cannot be
    /// expressed at all (`None`).
    fn carbon_equivalent(hotkey: &handy_keys::Hotkey) -> Option<(String, bool)> {
        use handy_keys::Modifiers as M;

        if hotkey.modifiers.contains(M::FN) {
            return None;
        }

        let mut widened = M::empty();
        let mut degraded = false;
        for group in [M::CTRL, M::OPT, M::SHIFT, M::CMD] {
            if hotkey.modifiers.intersects(group) {
                widened |= group;
                if !hotkey.modifiers.contains(group) {
                    // Only one side was specified — matching gets wider
                    degraded = true;
                }
            }
        }

        let carbon_hotkey = handy_keys::Hotkey::new(widened, hotkey.key).ok()?;
        Some((carbon_hotkey.to_handy_string(), degraded))
    }

    /// Register one vulnerable binding through Carbon. `fallback` is local
    /// reconciliation state, never the mutex-protected shared state.
    fn register_fallback_binding(
        app: &AppHandle,
        id: &str,
        binding: &ShortcutBinding,
        fallback: &mut FallbackState,
    ) -> bool {
        let Ok(hotkey) = binding.current_binding.parse::<handy_keys::Hotkey>() else {
            warn!(
                "SecureInput fallback: '{}' has unparseable binding '{}', skipping",
                id, binding.current_binding
            );
            fallback.uncovered.push(id.to_string());
            return false;
        };

        match &hotkey.key {
            None => {
                debug!(
                    "SecureInput fallback: '{}' ('{}') is modifier-only — immune, no shadow needed",
                    id, binding.current_binding
                );
                return true;
            }
            Some(k) if is_mouse_key(k) => {
                debug!(
                    "SecureInput fallback: '{}' ('{}') is mouse-based — immune, no shadow needed",
                    id, binding.current_binding
                );
                return true;
            }
            Some(_) => {}
        }

        let Some((carbon_binding, degraded)) = carbon_equivalent(&hotkey) else {
            warn!(
                "SecureInput fallback: '{}' ('{}') cannot be expressed via Carbon",
                id, binding.current_binding
            );
            fallback.uncovered.push(id.to_string());
            return false;
        };

        let mut shadow = binding.clone();
        shadow.current_binding = carbon_binding;

        match crate::shortcut::tauri_impl::register_shortcut(app, shadow.clone()) {
            Ok(()) => {
                info!(
                    "SecureInput fallback: '{}' registered via Carbon as '{}'{}",
                    id,
                    shadow.current_binding,
                    if degraded {
                        " (widened to either side)"
                    } else {
                        ""
                    }
                );
                fallback.registered.push(shadow);
                if degraded {
                    fallback.degraded.push(id.to_string());
                } else {
                    fallback.covered.push(id.to_string());
                }
            }
            Err(e) => {
                warn!(
                    "SecureInput fallback: could not cover '{}' ('{}'): {}",
                    id, shadow.current_binding, e
                );
                fallback.uncovered.push(id.to_string());
            }
        }

        false
    }

    /// Rebuild the fallback from current state. The operation mutex serializes
    /// reconciliations, while the fallback mutex is released before every
    /// global-shortcut plugin call to avoid lock-order inversion with callbacks.
    pub fn reconcile_fallback(app: &AppHandle) {
        let state = app.state::<SecureInputState>();
        let operation = match state_lock(
            &state.fallback_operation,
            SecureInputStateLock::FallbackOperation,
        ) {
            Ok(operation) => operation,
            Err(lock) => {
                error!("SecureInput {lock:?} lock poisoned; skipping fallback reconciliation");
                return;
            }
        };

        let previous = {
            let mut fallback = match state_lock(&state.fallback, SecureInputStateLock::Fallback) {
                Ok(fallback) => fallback,
                Err(lock) => {
                    error!("SecureInput {lock:?} lock poisoned; skipping fallback reconciliation");
                    return;
                }
            };
            std::mem::take(&mut *fallback)
        };

        if !previous.registered.is_empty() {
            info!(
                "SecureInput fallback reconciling: removing {} Carbon shadow(s)",
                previous.registered.len()
            );
        }
        for binding in previous.registered {
            if let Err(e) = crate::shortcut::tauri_impl::unregister_shortcut(app, binding.clone()) {
                warn!(
                    "SecureInput fallback: failed to unregister '{}': {}",
                    binding.current_binding, e
                );
            }
        }

        let settings = settings::get_settings(app);
        let eligible = state.is_sustained()
            && app
                .try_state::<crate::commands::ShortcutsInitialized>()
                .is_some()
            && settings.keyboard_implementation == KeyboardImplementation::HandyKeys;

        let mut next = FallbackState::default();
        let mut immune = 0usize;
        if eligible {
            let cancel_bindings = crate::shortcut::active_cancel_bindings(&settings);
            let bindings = settings
                .bindings
                .iter()
                .filter(|(id, _)| id.as_str() != "cancel")
                .map(|(id, binding)| (id.as_str(), binding))
                .chain(
                    cancel_bindings
                        .iter()
                        .map(|binding| (binding.id.as_str(), binding)),
                );
            for (id, binding) in bindings {
                if register_fallback_binding(app, id, binding, &mut next) {
                    immune += 1;
                }
            }

            info!(
                "SecureInput fallback active: {} covered, {} degraded, {} uncovered, {} immune (user impact: {})",
                next.covered.len(),
                next.degraded.len(),
                next.uncovered.len(),
                immune,
                !next.degraded.is_empty() || !next.uncovered.is_empty()
            );
        } else if state.is_sustained()
            && app
                .try_state::<crate::commands::ShortcutsInitialized>()
                .is_none()
        {
            debug!("SecureInput fallback deferred until shortcuts are initialized");
        }

        match state_lock(&state.fallback, SecureInputStateLock::Fallback) {
            Ok(mut fallback) => *fallback = next,
            Err(lock) => error!("SecureInput {lock:?} lock poisoned while replacing fallback"),
        }
        drop(operation);

        // The tray sync diffs against what is displayed, so this is free when
        // the warning state did not change. Lock is released first: the sync
        // reads app state and must not nest under the operation mutex.
        refresh_tray(app);
        emit_status(app);
    }

    fn schedule_reconcile(app: &AppHandle) {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            reconcile_fallback(&app);
        });
    }

    pub fn cancel_keys_changed(app: &AppHandle) {
        let state = app.state::<SecureInputState>();
        // Without sustained Secure Input there are no Carbon shadows to
        // update. The monitor performs reconciliation when sustained mode is
        // entered or left, so spawning here would only race the normal tray
        // state transition for every recording start.
        if state.is_sustained() {
            schedule_reconcile(app);
        }
    }

    /// Count-only capture test for the debug window. Opens a short-lived
    /// keyboard listener and tallies event kinds; key identity is never
    /// inspected beyond the mouse/keyboard distinction, and nothing about
    /// individual events is logged or returned.
    pub async fn run_diagnostic(duration_secs: u32) -> Result<KeyboardDiagnosticReport, String> {
        tauri::async_runtime::spawn_blocking(move || {
            let listener = handy_keys::KeyboardListener::new()
                .map_err(|e| format!("Failed to create keyboard listener: {e}"))?;

            let enabled_at_start = is_enabled();
            let start = Instant::now();
            let deadline = start + Duration::from_secs(u64::from(duration_secs));
            let (mut key_down, mut key_up, mut flags_changed, mut mouse) = (0u32, 0u32, 0u32, 0u32);

            while Instant::now() < deadline {
                match listener.try_recv() {
                    Some(event) => match &event.key {
                        Some(k) if is_mouse_key(k) => mouse += 1,
                        Some(_) if event.is_key_down => key_down += 1,
                        Some(_) => key_up += 1,
                        None => flags_changed += 1,
                    },
                    None => std::thread::sleep(Duration::from_millis(10)),
                }
            }

            let enabled = enabled_at_start || is_enabled();
            let culprit = if enabled { lookup_culprit() } else { None };
            info!(
                "keyboard diagnostic: secure_input={} key_down={} key_up={} flags_changed={} mouse={}",
                enabled, key_down, key_up, flags_changed, mouse
            );
            let duration_ms = u32::try_from(start.elapsed().as_millis())
                .map_err(|_| "Keyboard diagnostic duration exceeds u32 milliseconds".to_string())?;

            Ok(KeyboardDiagnosticReport {
                secure_input_enabled: enabled,
                culprit_pid: culprit.as_ref().map(|c| c.pid),
                culprit_name: culprit.map(|c| c.name),
                key_down,
                key_up,
                flags_changed,
                mouse,
                duration_ms,
            })
        })
        .await
        .map_err(|e| format!("Diagnostic task failed: {e}"))?
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        #[derive(Default)]
        struct RecordingFallbackOwner {
            active: bool,
            registrations: usize,
            unregistrations: usize,
        }

        impl RecordingFallbackOwner {
            fn apply(&mut self, transition: Option<FallbackTransition>) {
                match transition {
                    Some(FallbackTransition::Register) => {
                        self.active = true;
                        self.registrations += 1;
                    }
                    Some(FallbackTransition::Unregister) => {
                        self.active = false;
                        self.unregistrations += 1;
                    }
                    None => {}
                }
            }
        }

        fn sample(
            sustained_input: &mut SustainedInput,
            owner: &mut RecordingFallbackOwner,
            enabled: bool,
            now: Duration,
        ) {
            owner.apply(sustained_input.observe(enabled, now));
        }

        #[test]
        fn sustained_secure_input_registers_once_at_the_exact_threshold_and_releases_once() {
            let mut sustained_input = SustainedInput::default();
            let mut fallback = RecordingFallbackOwner::default();
            let started_at = Duration::from_secs(10);

            sample(&mut sustained_input, &mut fallback, true, started_at);
            sample(
                &mut sustained_input,
                &mut fallback,
                true,
                started_at + SUSTAIN_THRESHOLD.saturating_sub(Duration::from_millis(1)),
            );
            assert_eq!(
                fallback.registrations, 0,
                "a short secure-input episode keeps normal shortcuts"
            );
            assert!(!fallback.active);

            sample(
                &mut sustained_input,
                &mut fallback,
                true,
                started_at + SUSTAIN_THRESHOLD,
            );
            assert_eq!(
                fallback.registrations, 1,
                "the exact threshold registers the fallback"
            );
            assert!(fallback.active);

            sample(
                &mut sustained_input,
                &mut fallback,
                true,
                started_at + SUSTAIN_THRESHOLD + Duration::from_secs(30),
            );
            assert_eq!(
                fallback.registrations, 1,
                "continued secure input does not duplicate fallback ownership"
            );

            sample(
                &mut sustained_input,
                &mut fallback,
                false,
                started_at + SUSTAIN_THRESHOLD + Duration::from_secs(31),
            );
            assert_eq!(
                fallback.unregistrations, 1,
                "release unregisters the fallback once"
            );
            assert!(
                !fallback.active,
                "release returns shortcut ownership to the normal backend"
            );

            sample(
                &mut sustained_input,
                &mut fallback,
                false,
                started_at + SUSTAIN_THRESHOLD + Duration::from_secs(32),
            );
            assert_eq!(
                fallback.unregistrations, 1,
                "a second disabled sample is idempotent"
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub struct SecureInputState;

    impl SecureInputState {
        pub fn new() -> Self {
            Self
        }
        pub fn warning_active(&self) -> bool {
            false
        }
    }

    pub fn is_enabled() -> bool {
        false
    }

    pub fn start_monitor(_app: &AppHandle) {}

    pub fn status(_app: &AppHandle) -> SecureInputStatus {
        SecureInputStatus {
            enabled: false,
            sustained: false,
            culprit_pid: None,
            culprit_name: None,
            fallback_active: false,
            covered_bindings: Vec::new(),
            degraded_bindings: Vec::new(),
            uncovered_bindings: Vec::new(),
            recorder_blocked: false,
        }
    }

    pub fn note_recorder_blocked(_app: &AppHandle) {}

    pub fn cancel_keys_changed(_app: &AppHandle) {}

    pub fn reconcile_fallback(_app: &AppHandle) {}

    pub async fn run_diagnostic(_duration_secs: u32) -> Result<KeyboardDiagnosticReport, String> {
        Err("The keyboard diagnostic is only supported on macOS".to_string())
    }
}
