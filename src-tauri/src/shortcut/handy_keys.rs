//! Handy-keys based keyboard shortcut implementation
//!
//! This module provides an alternative to Tauri's global-shortcut plugin
//! using the handy-keys library for more control over keyboard events.
//!
//! ## Architecture
//!
//! One event thread owns the `KeyboardListener` and matches its key events
//! against a registry that callers edit directly:
//!
//! ```text
//! ┌─────────────────┐   lock registry   ┌──────────────────────┐
//! │ Callers         │ ────────────────▶ │ Registry             │
//! │ - register()    │                   │ - bindings, pressed  │
//! │ - unregister()  │                   │ - blocking set       │
//! └─────────────────┘                   └──────────────────────┘
//!                                                  ▲
//!                                                  │ match each key event
//!                                       ┌──────────────────────┐
//!                                       │ Event thread         │
//!                                       │ - blocks on recv()   │
//!                                       │ - dispatches actions │
//!                                       └──────────────────────┘
//! ```
//!
//! The listener's tap thread parks in its run loop and the event thread
//! blocks on the listener's channel with no deadline, so an idle keyboard
//! wakes neither. The event thread releases the registry before dispatching,
//! so an action may register or unregister shortcuts itself.
//!
//! ## Recording Mode
//!
//! For UI key capture, a separate `KeyboardListener` is created on-demand and
//! polled from a dedicated recording thread. Events are emitted to the frontend
//! via Tauri's event system.

use handy_keys::{BlockingHotkeys, Hotkey, KeyEvent, KeyboardListener};
use log::{debug, error, info};
use serde::Serialize;
use specta::Type;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

use crate::settings::{self, get_settings, ShortcutBinding};

use super::handler::handle_shortcut_event;

/// The stored spelling of a double-tap binding: `double:option_right` fires
/// on the second of two quick taps of Right Option.
pub(crate) const DOUBLE_TAP_PREFIX: &str = "double:";

/// Longest a first tap may be held and still count as a tap.
const DOUBLE_TAP_MAX_HOLD: Duration = Duration::from_millis(350);
/// Longest gap between letting go of the first tap and pressing the second.
const DOUBLE_TAP_MAX_GAP: Duration = Duration::from_millis(400);

/// A stored binding string, split into its chord and how the chord fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParsedShortcut {
    pub hotkey: Hotkey,
    pub double_tap: bool,
}

pub(crate) fn parse_shortcut(raw: &str) -> Result<ParsedShortcut, String> {
    let (chord, double_tap) = match raw.trim().strip_prefix(DOUBLE_TAP_PREFIX) {
        Some(chord) => (chord, true),
        None => (raw, false),
    };
    let hotkey = chord
        .parse::<Hotkey>()
        .map_err(|e| format!("Failed to parse hotkey '{raw}': {e}"))?;
    Ok(ParsedShortcut { hotkey, double_tap })
}

/// Where a double-tap binding is between its two taps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TapState {
    Idle,
    FirstDown(Instant),
    FirstUp(Instant),
    /// The second tap fired the binding; its release ends it.
    SecondDown,
}

/// A registered binding and whether its chord is held right now.
struct RegisteredHotkey {
    hotkey: Hotkey,
    shortcut: String,
    pressed: bool,
    /// `Some` for a double-tap binding.
    taps: Option<TapState>,
}

/// Registered bindings, matched against every key event the listener reports.
///
/// `blocking` is the set the listener's tap consults to swallow a chord. It
/// holds exactly the registered hotkeys, so it changes only beside `bindings`.
struct Registry {
    bindings: HashMap<String, RegisteredHotkey>,
    blocking: BlockingHotkeys,
}

/// A binding whose chord went down or came up.
struct Transition {
    binding_id: String,
    shortcut: String,
    is_pressed: bool,
}

impl Registry {
    /// Refuses a chord that is already registered, even to this binding or
    /// as the other of a single press and a double tap, since every tap of a
    /// double tap is also a press; a binding registered again with a new
    /// chord gives up its old one.
    ///
    /// A double tap is never blocked: its first tap cannot be told from an
    /// ordinary press until the second arrives, so swallowing it would take
    /// the key away from every app.
    fn register(
        &mut self,
        binding_id: &str,
        shortcut: &str,
        parsed: ParsedShortcut,
    ) -> Result<(), String> {
        let hotkey = parsed.hotkey;
        if self
            .bindings
            .values()
            .any(|registered| registered.hotkey == hotkey)
        {
            return Err(format!("Hotkey already registered: {hotkey}"));
        }
        let previous = self.bindings.insert(
            binding_id.to_string(),
            RegisteredHotkey {
                hotkey,
                shortcut: shortcut.to_string(),
                pressed: false,
                taps: parsed.double_tap.then_some(TapState::Idle),
            },
        );
        let mut blocking = lock_recover(&self.blocking);
        if let Some(previous) = previous {
            blocking.remove(&previous.hotkey);
        }
        if !parsed.double_tap {
            blocking.insert(hotkey);
        }
        Ok(())
    }

    /// Whether the binding was registered.
    fn unregister(&mut self, binding_id: &str) -> bool {
        let Some(registered) = self.bindings.remove(binding_id) else {
            return false;
        };
        lock_recover(&self.blocking).remove(&registered.hotkey);
        true
    }

    /// The transitions one key event at `at` causes, with the semantics of
    /// handy-keys' own manager: a chord presses when its key goes down with
    /// matching modifiers, and releases when its key comes up or, for a
    /// modifier event, when the held modifiers stop matching. A modifier event
    /// that still matches (tapping Shift while a Cmd-only chord is held)
    /// releases nothing.
    ///
    /// A double-tap binding presses only on the second of two taps of its
    /// chord, the first tap short and the gap between them short, and
    /// releases when that second tap comes up. Any other key struck between
    /// the taps starts the count again.
    fn transitions(&mut self, event: &KeyEvent, at: Instant) -> Vec<Transition> {
        let mut transitions = Vec::new();
        for (binding_id, registered) in &mut self.bindings {
            let hotkey = registered.hotkey;
            let changes = if event.is_key_down {
                !registered.pressed
                    && hotkey.key == event.key
                    && hotkey.modifiers.matches(event.modifiers)
            } else {
                registered.pressed
                    && match event.key {
                        Some(_) => hotkey.key == event.key,
                        None => !hotkey.modifiers.matches(event.modifiers),
                    }
            };
            if changes {
                registered.pressed = event.is_key_down;
            }
            let fires = match registered.taps.as_mut() {
                None => changes,
                Some(taps) => double_tap_fires(taps, hotkey, event, changes, at),
            };
            if fires {
                transitions.push(Transition {
                    binding_id: binding_id.clone(),
                    shortcut: registered.shortcut.clone(),
                    is_pressed: event.is_key_down,
                });
            }
        }
        transitions
    }
}

/// Advances a double-tap binding by one key event at `at`; whether the
/// binding presses or releases on it. `changes` is whether the event pressed
/// or released the binding's chord.
fn double_tap_fires(
    taps: &mut TapState,
    hotkey: Hotkey,
    event: &KeyEvent,
    changes: bool,
    at: Instant,
) -> bool {
    let building_chord = event.key.is_none()
        && !event.modifiers.is_empty()
        && hotkey.modifiers.contains(event.modifiers);
    let (next, fires) = match (*taps, changes, event.is_key_down) {
        (TapState::SecondDown, true, false) => (TapState::Idle, true),
        (TapState::SecondDown, _, _) => (TapState::SecondDown, false),
        (TapState::FirstUp(up), true, true) if at.duration_since(up) <= DOUBLE_TAP_MAX_GAP => {
            (TapState::SecondDown, true)
        }
        (_, true, true) => (TapState::FirstDown(at), false),
        (TapState::FirstDown(down), true, false)
            if at.duration_since(down) <= DOUBLE_TAP_MAX_HOLD =>
        {
            (TapState::FirstUp(at), false)
        }
        (TapState::FirstUp(up), false, true) if building_chord => (TapState::FirstUp(up), false),
        (_, true, false) | (_, false, true) => (TapState::Idle, false),
        (state, false, false) => (state, false),
    };
    *taps = next;
    fires
}

/// State for the handy-keys shortcut manager
pub struct HandyKeysState {
    /// The bindings the event thread matches, or why the keyboard listener
    /// could not start (on macOS, without accessibility permission). The event
    /// thread holds only a weak reference, so dropping the state ends the
    /// thread at its next key event, which drops the listener.
    registry: Result<Arc<Mutex<Registry>>, String>,
    /// Recording listener for UI key capture (only active during recording)
    recording_listener: Mutex<Option<KeyboardListener>>,
    /// Flag indicating if we're in recording mode
    is_recording: AtomicBool,
    /// The binding ID being recorded (if any)
    recording_binding_id: Mutex<Option<String>>,
    /// Flag to stop recording loop
    recording_running: Arc<AtomicBool>,
}

/// Key event sent to frontend during recording mode
#[derive(Debug, Clone, Serialize, Type)]
pub struct FrontendKeyEvent {
    /// Currently pressed modifier keys
    pub modifiers: Vec<String>,
    /// The key that was pressed (if any)
    pub key: Option<String>,
    /// Whether this is a key down event
    pub is_key_down: bool,
    /// Full hotkey chord emitted by the listener.
    #[serde(rename = "hotkey_string")]
    #[specta(rename = "hotkey_string")]
    pub shortcut: String,
}

impl HandyKeysState {
    /// Create a new HandyKeysState
    ///
    /// A listener that cannot start fails each registration rather than the
    /// state: an error here makes the caller switch to Tauri shortcuts for
    /// good, which cannot hold a modifier-only chord once permission arrives.
    pub fn new(app: AppHandle) -> Result<Self, String> {
        let blocking: BlockingHotkeys = Arc::new(Mutex::new(HashSet::new()));
        let registry = match KeyboardListener::new_with_blocking(Arc::clone(&blocking)) {
            Ok(listener) => {
                let registry = Arc::new(Mutex::new(Registry {
                    bindings: HashMap::new(),
                    blocking,
                }));
                let events = Arc::downgrade(&registry);
                thread::Builder::new()
                    .name("sona-shortcut-keys".to_string())
                    .spawn(move || Self::dispatch_key_events(listener, events, app))
                    .map_err(|e| format!("Failed to start the shortcut event thread: {e}"))?;
                info!("handy-keys event thread started");
                Ok(registry)
            }
            Err(e) => {
                let reason = format!("Failed to create keyboard listener: {e}");
                error!("{reason}");
                Err(reason)
            }
        };

        Ok(Self {
            registry,
            recording_listener: Mutex::new(None),
            is_recording: AtomicBool::new(false),
            recording_binding_id: Mutex::new(None),
            recording_running: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Dispatches every chord transition until the state is dropped. `recv`
    /// has no deadline, so this thread wakes only for a key event.
    fn dispatch_key_events(
        listener: KeyboardListener,
        registry: Weak<Mutex<Registry>>,
        app: AppHandle,
    ) {
        while let Ok(event) = listener.recv() {
            let Some(registry) = registry.upgrade() else {
                break;
            };
            let transitions = lock_recover(&registry).transitions(&event, Instant::now());
            drop(registry);
            for transition in transitions {
                debug!(
                    "handy-keys event: binding={}, hotkey={}, pressed={}",
                    transition.binding_id, transition.shortcut, transition.is_pressed
                );
                let _ = handle_shortcut_event(
                    &app,
                    &transition.binding_id,
                    &transition.shortcut,
                    transition.is_pressed,
                );
            }
        }
        info!("handy-keys event thread stopped");
    }

    /// Register a shortcut binding
    pub fn register(&self, binding: &ShortcutBinding) -> Result<(), String> {
        let registry = self.registry.as_ref().map_err(Clone::clone)?;
        let parsed = parse_shortcut(&binding.current_binding)?;
        lock_recover(registry).register(&binding.id, &binding.current_binding, parsed)?;
        debug!(
            "Registered handy-keys shortcut: {} -> {:?}",
            binding.id, parsed
        );
        Ok(())
    }

    /// Unregister a shortcut binding
    pub fn unregister(&self, binding: &ShortcutBinding) -> Result<(), String> {
        let registry = self.registry.as_ref().map_err(Clone::clone)?;
        if lock_recover(registry).unregister(&binding.id) {
            debug!("Unregistered handy-keys shortcut: {}", binding.id);
        }
        Ok(())
    }

    /// Start recording mode for a specific binding
    pub fn start_recording(&self, app: &AppHandle, binding_id: String) -> Result<(), String> {
        if self.is_recording.load(Ordering::SeqCst) {
            return Err("Already recording".into());
        }

        // Create a new keyboard listener for recording
        let listener = KeyboardListener::new()
            .map_err(|e| format!("Failed to create keyboard listener: {}", e))?;

        {
            let mut recording = self
                .recording_listener
                .lock()
                .map_err(|_| "Failed to lock recording_listener")?;
            *recording = Some(listener);
        }
        {
            let mut binding = self
                .recording_binding_id
                .lock()
                .map_err(|_| "Failed to lock recording_binding_id")?;
            *binding = Some(binding_id);
        }

        self.is_recording.store(true, Ordering::SeqCst);
        self.recording_running.store(true, Ordering::SeqCst);

        // Start a thread to emit key events to the frontend
        let app_clone = app.clone();
        let recording_running = Arc::clone(&self.recording_running);
        thread::spawn(move || {
            Self::recording_loop(app_clone, recording_running);
        });

        debug!("Started handy-keys recording mode");
        Ok(())
    }

    /// Recording loop - emits key events to frontend during recording
    fn recording_loop(app: AppHandle, running: Arc<AtomicBool>) {
        while running.load(Ordering::SeqCst) {
            let event = {
                let state = match app.try_state::<HandyKeysState>() {
                    Some(s) => s,
                    None => break,
                };
                let listener = state.recording_listener.lock().ok();
                listener.as_ref().and_then(|l| l.as_ref()?.try_recv())
            };

            if let Some(key_event) = event {
                // Convert to frontend-friendly format
                let frontend_event = FrontendKeyEvent {
                    modifiers: modifiers_to_strings(key_event.modifiers),
                    key: key_event.key.map(|k| k.to_string().to_lowercase()),
                    is_key_down: key_event.is_key_down,
                    shortcut: key_event
                        .as_hotkey()
                        .map(|h| h.to_handy_string())
                        .unwrap_or_default(),
                };

                // Emit to frontend
                if let Err(e) = app.emit("handy-keys-event", &frontend_event) {
                    error!("Failed to emit key event: {}", e);
                }
            } else {
                thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        debug!("Recording loop ended");
    }

    /// Stop recording mode
    pub fn stop_recording(&self) -> Result<(), String> {
        self.is_recording.store(false, Ordering::SeqCst);
        self.recording_running.store(false, Ordering::SeqCst);

        {
            let mut recording = self
                .recording_listener
                .lock()
                .map_err(|_| "Failed to lock recording_listener")?;
            *recording = None;
        }
        {
            let mut binding = self
                .recording_binding_id
                .lock()
                .map_err(|_| "Failed to lock recording_binding_id")?;
            *binding = None;
        }

        debug!("Stopped handy-keys recording mode");
        Ok(())
    }
}

impl Drop for HandyKeysState {
    fn drop(&mut self) {
        // Signal recording to stop
        self.recording_running.store(false, Ordering::SeqCst);
        self.is_recording.store(false, Ordering::SeqCst);
    }
}

pub(super) fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Convert handy-keys Modifiers to a list of strings
fn modifiers_to_strings(modifiers: handy_keys::Modifiers) -> Vec<String> {
    let mut result = Vec::new();

    if modifiers.contains(handy_keys::Modifiers::CTRL) {
        result.push("ctrl".to_string());
    }
    if modifiers.contains(handy_keys::Modifiers::OPT) {
        #[cfg(target_os = "macos")]
        result.push("option".to_string());
        #[cfg(not(target_os = "macos"))]
        result.push("alt".to_string());
    }
    if modifiers.contains(handy_keys::Modifiers::SHIFT) {
        result.push("shift".to_string());
    }
    if modifiers.contains(handy_keys::Modifiers::CMD) {
        #[cfg(target_os = "macos")]
        result.push("command".to_string());
        #[cfg(not(target_os = "macos"))]
        result.push("super".to_string());
    }
    if modifiers.contains(handy_keys::Modifiers::FN) {
        result.push("fn".to_string());
    }

    result
}

/// Validate a shortcut string for the HandyKeys implementation.
/// HandyKeys is more permissive: allows modifier-only combos and the fn key.
pub fn validate_shortcut(raw: &str) -> Result<(), String> {
    if raw.trim().is_empty() {
        return Err("Shortcut cannot be empty".into());
    }
    // HandyKeys accepts modifier-only, key-only, and modifier+key combos,
    // each pressed once or double-tapped.
    parse_shortcut(raw)
        .map(|_| ())
        .map_err(|e| format!("Invalid shortcut for HandyKeys: {}", e))
}

/// Initialize handy-keys shortcuts
pub fn init_shortcuts(app: &AppHandle) -> Result<(), String> {
    let state = HandyKeysState::new(app.clone())?;
    let user_settings = settings::load_or_create_app_settings(app);

    // Both backends consume the same persisted registration set.
    for binding in super::bindings_for_registration(&user_settings) {
        let id = binding.id.clone();
        if let Err(e) = state.register(&binding) {
            error!("Failed to register handy-keys shortcut {id} during init: {e}");
        }
    }

    app.manage(state);
    info!("handy-keys shortcuts initialized");
    Ok(())
}

/// Register a shortcut
pub fn register_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let state = app
        .try_state::<HandyKeysState>()
        .ok_or("HandyKeysState not initialized")?;
    state.register(&binding)
}

/// Unregister a shortcut
pub fn unregister_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let state = app
        .try_state::<HandyKeysState>()
        .ok_or("HandyKeysState not initialized")?;
    state.unregister(&binding)
}

/// Start key recording mode
#[tauri::command]
#[specta::specta]
pub fn start_handy_keys_recording(app: AppHandle, binding_id: String) -> Result<(), String> {
    let settings = get_settings(&app);
    if settings.keyboard_implementation != settings::KeyboardImplementation::HandyKeys {
        return Err("handy-keys is not the active keyboard implementation".into());
    }

    // While Secure Input is active the tap receives no KeyDown/KeyUp, so the
    // recorder would silently capture just the modifier and overwrite the
    // binding with it (issue #1578). Refuse instead; the frontend maps this
    // marker to a localized explanation, and the noted impact makes the
    // warning banner appear with the full story.
    if crate::secure_input::is_enabled_now() {
        crate::secure_input::note_recorder_blocked(&app);
        return Err("secure-input-active".into());
    }

    let state = app
        .try_state::<HandyKeysState>()
        .ok_or("HandyKeysState not initialized")?;

    // Suspend every registered shortcut so a combo that overlaps an existing
    // binding can't fire it (or have its keys swallowed) mid-capture.
    super::suspend_all_shortcuts(&app);

    let result = state.start_recording(&app, binding_id);
    if result.is_err() {
        super::resume_all_shortcuts(&app);
    }
    result
}

/// Stop key recording mode
#[tauri::command]
#[specta::specta]
pub fn stop_handy_keys_recording(app: AppHandle) -> Result<(), String> {
    let settings = get_settings(&app);
    if settings.keyboard_implementation != settings::KeyboardImplementation::HandyKeys {
        return Err("handy-keys is not the active keyboard implementation".into());
    }

    let state = app
        .try_state::<HandyKeysState>()
        .ok_or("HandyKeysState not initialized")?;

    // Restore shortcuts from settings regardless of how recording ended.
    // A commit has already registered the new binding via change_binding;
    // re-registering it here fails cleanly and is ignored.
    let result = state.stop_recording();
    super::resume_all_shortcuts(&app);
    result
}

#[cfg(test)]
mod tests {
    use super::{lock_recover, parse_shortcut, FrontendKeyEvent, Registry};
    use handy_keys::{Hotkey, Key, KeyEvent, Modifiers};
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn chord(shortcut: &str) -> Result<Hotkey, String> {
        shortcut
            .parse()
            .map_err(|error| format!("'{shortcut}' is not a chord: {error}"))
    }

    fn registry(bindings: &[(&str, &str)]) -> Result<Registry, String> {
        let mut registry = Registry {
            bindings: HashMap::new(),
            blocking: Arc::new(Mutex::new(HashSet::new())),
        };
        for (binding_id, shortcut) in bindings {
            registry.register(binding_id, shortcut, parse_shortcut(shortcut)?)?;
        }
        Ok(registry)
    }

    fn event(modifiers: Modifiers, key: Option<Key>, is_key_down: bool) -> KeyEvent {
        KeyEvent {
            modifiers,
            key,
            is_key_down,
            changed_modifier: None,
        }
    }

    /// The press (`true`) and release (`false`) transitions each event causes.
    fn replay(registry: &mut Registry, events: &[KeyEvent]) -> Vec<Vec<bool>> {
        let start = Instant::now();
        events
            .iter()
            .map(|event| {
                registry
                    .transitions(event, start)
                    .iter()
                    .map(|transition| transition.is_pressed)
                    .collect()
            })
            .collect()
    }

    /// Every binding that pressed or released, over events each stamped with
    /// its millisecond offset from the first.
    fn replay_timed(registry: &mut Registry, events: &[(u64, KeyEvent)]) -> Vec<(String, bool)> {
        let start = Instant::now();
        events
            .iter()
            .flat_map(|(ms, event)| registry.transitions(event, start + Duration::from_millis(*ms)))
            .map(|transition| (transition.binding_id, transition.is_pressed))
            .collect()
    }

    fn right_option(is_key_down: bool) -> KeyEvent {
        let held = if is_key_down {
            Modifiers::OPT_RIGHT
        } else {
            Modifiers::empty()
        };
        event(held, None, is_key_down)
    }

    fn left_option(is_key_down: bool) -> KeyEvent {
        let held = if is_key_down {
            Modifiers::OPT_LEFT
        } else {
            Modifiers::empty()
        };
        event(held, None, is_key_down)
    }

    fn fired(binding_id: &str, is_pressed: bool) -> (String, bool) {
        (binding_id.to_string(), is_pressed)
    }

    #[test]
    fn a_double_tap_presses_on_the_second_quick_tap_and_releases_with_it() -> Result<(), String> {
        let mut registry = registry(&[("transcribe", "double:option_right")])?;
        let transitions = replay_timed(
            &mut registry,
            &[
                (0, right_option(true)),
                (90, right_option(false)),
                (300, right_option(true)),
                (380, right_option(false)),
            ],
        );
        assert_eq!(
            transitions,
            [fired("transcribe", true), fired("transcribe", false)]
        );
        Ok(())
    }

    #[test]
    fn a_modifier_plus_key_double_tap_fires_with_full_release_between_taps() -> Result<(), String> {
        let mut registry = registry(&[("transcribe", "double:option_right+space")])?;
        let transitions = replay_timed(
            &mut registry,
            &[
                (0, right_option(true)),
                (20, event(Modifiers::OPT_RIGHT, Some(Key::Space), true)),
                (80, event(Modifiers::OPT_RIGHT, Some(Key::Space), false)),
                (100, right_option(false)),
                (220, right_option(true)),
                (240, event(Modifiers::OPT_RIGHT, Some(Key::Space), true)),
                (300, event(Modifiers::OPT_RIGHT, Some(Key::Space), false)),
                (320, right_option(false)),
            ],
        );
        assert_eq!(
            transitions,
            [fired("transcribe", true), fired("transcribe", false)]
        );
        Ok(())
    }

    #[test]
    fn a_slow_or_interrupted_double_tap_presses_nothing() -> Result<(), String> {
        let double_tap = || registry(&[("transcribe", "double:option_right")]);
        let no_presses: [(String, bool); 0] = [];

        let held_first_tap = replay_timed(
            &mut double_tap()?,
            &[
                (0, right_option(true)),
                (600, right_option(false)),
                (700, right_option(true)),
                (760, right_option(false)),
            ],
        );
        assert_eq!(
            held_first_tap, no_presses,
            "a first tap held 600 ms is a hold"
        );

        let long_gap = replay_timed(
            &mut double_tap()?,
            &[
                (0, right_option(true)),
                (80, right_option(false)),
                (700, right_option(true)),
                (760, right_option(false)),
            ],
        );
        assert_eq!(
            long_gap, no_presses,
            "taps 620 ms apart are two single taps"
        );

        let typed_between = replay_timed(
            &mut double_tap()?,
            &[
                (0, right_option(true)),
                (80, right_option(false)),
                (150, event(Modifiers::empty(), Some(Key::E), true)),
                (180, event(Modifiers::empty(), Some(Key::E), false)),
                (250, right_option(true)),
                (320, right_option(false)),
            ],
        );
        assert_eq!(
            typed_between, no_presses,
            "a key typed between the taps resets them"
        );
        Ok(())
    }

    /// Push-to-talk on Left Option beside a double tap on Right Option: the
    /// hold presses and releases only push-to-talk, quick taps of the left
    /// key stay push-to-talk taps, and only the right key's double tap fires
    /// its binding. Neither key's taps are swallowed beyond the held chord.
    #[test]
    fn a_double_tap_leaves_push_to_talk_on_the_other_side_alone() -> Result<(), String> {
        let mut registry = registry(&[
            ("transcribe", "option_left"),
            ("mode/email/transcribe", "double:option_right"),
        ])?;
        let transitions = replay_timed(
            &mut registry,
            &[
                (0, left_option(true)),
                (2_000, left_option(false)),
                (2_100, left_option(true)),
                (2_180, left_option(false)),
                (2_300, left_option(true)),
                (2_380, left_option(false)),
                (3_000, right_option(true)),
                (3_080, right_option(false)),
                (3_200, right_option(true)),
                (3_270, right_option(false)),
            ],
        );
        assert_eq!(
            transitions,
            [
                fired("transcribe", true),
                fired("transcribe", false),
                fired("transcribe", true),
                fired("transcribe", false),
                fired("transcribe", true),
                fired("transcribe", false),
                fired("mode/email/transcribe", true),
                fired("mode/email/transcribe", false),
            ]
        );
        assert_eq!(
            *lock_recover(&registry.blocking),
            HashSet::from([chord("option_left")?]),
            "the double tap's key reaches other apps"
        );
        Ok(())
    }

    #[test]
    fn a_double_tap_of_a_chord_bound_to_a_single_press_is_refused() -> Result<(), String> {
        let mut registry = registry(&[("transcribe", "option_right")])?;
        let refused = registry.register(
            "mode/email/transcribe",
            "double:option_right",
            parse_shortcut("double:option_right")?,
        );
        assert!(
            refused.is_err(),
            "every tap would also press the single binding"
        );
        Ok(())
    }

    #[test]
    fn a_held_modifier_chord_survives_a_shift_tap_and_releases_when_lifted() -> Result<(), String> {
        let mut registry = registry(&[("transcribe", "option")])?;
        let transitions = replay(
            &mut registry,
            &[
                event(Modifiers::OPT_LEFT, None, true),
                event(Modifiers::OPT_LEFT | Modifiers::SHIFT_LEFT, None, true),
                event(Modifiers::OPT_LEFT, None, false),
                event(Modifiers::empty(), None, false),
            ],
        );
        assert_eq!(transitions, [vec![true], vec![], vec![], vec![false]]);
        Ok(())
    }

    #[test]
    fn a_key_chord_presses_once_through_key_repeat_and_releases_on_key_up() -> Result<(), String> {
        let mut registry = registry(&[("transcribe", "option+space")])?;
        let transitions = replay(
            &mut registry,
            &[
                event(Modifiers::OPT_LEFT, None, true),
                event(Modifiers::OPT_LEFT, Some(Key::Space), true),
                event(Modifiers::OPT_LEFT, Some(Key::Space), true),
                event(Modifiers::OPT_LEFT, Some(Key::Space), false),
            ],
        );
        assert_eq!(transitions, [vec![], vec![true], vec![], vec![false]]);
        Ok(())
    }

    #[test]
    fn lifting_the_modifier_first_releases_a_key_chord() -> Result<(), String> {
        let mut registry = registry(&[("transcribe", "option+space")])?;
        let transitions = replay(
            &mut registry,
            &[
                event(Modifiers::OPT_LEFT, Some(Key::Space), true),
                event(Modifiers::empty(), None, false),
                event(Modifiers::empty(), Some(Key::Space), false),
            ],
        );
        assert_eq!(transitions, [vec![true], vec![false], vec![]]);
        Ok(())
    }

    #[test]
    fn the_tap_blocks_exactly_the_registered_chords() -> Result<(), String> {
        let mut registry = registry(&[("transcribe", "option+space")])?;

        registry.register("transcribe", "option+k", parse_shortcut("option+k")?)?;
        assert_eq!(
            *lock_recover(&registry.blocking),
            HashSet::from([chord("option+k")?]),
            "a re-registered binding moves its block to the new chord"
        );

        assert!(registry.unregister("transcribe"));
        assert!(lock_recover(&registry.blocking).is_empty());
        Ok(())
    }

    #[test]
    fn a_chord_another_binding_holds_is_refused_and_keeps_firing_the_first() -> Result<(), String> {
        let mut registry = registry(&[("transcribe", "option+space")])?;

        let refused = registry.register("cancel", "option+space", parse_shortcut("option+space")?);
        assert!(refused.is_err(), "the duplicate chord was accepted");

        let fired = registry.transitions(
            &event(Modifiers::OPT_LEFT, Some(Key::Space), true),
            Instant::now(),
        );
        let fired: Vec<&str> = fired
            .iter()
            .map(|transition| transition.binding_id.as_str())
            .collect();
        assert_eq!(fired, ["transcribe"]);
        Ok(())
    }

    #[test]
    fn frontend_key_event_preserves_the_hotkey_string_wire_field() -> Result<(), String> {
        let event = FrontendKeyEvent {
            modifiers: vec!["option".to_string()],
            key: Some("space".to_string()),
            is_key_down: true,
            shortcut: "option+space".to_string(),
        };
        let payload = serde_json::to_value(event)
            .map_err(|error| format!("failed to serialize frontend key event: {error}"))?;
        let shortcut = payload
            .get("hotkey_string")
            .and_then(serde_json::Value::as_str)
            .ok_or("frontend key event lost hotkey_string")?;

        assert_eq!(shortcut, "option+space");
        assert!(payload.get("shortcut").is_none());
        Ok(())
    }
}
