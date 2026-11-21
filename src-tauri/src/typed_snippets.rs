//! Shortcuts expand where they are typed, in any app: the trigger and the
//! space after it are erased and the expansion is typed in their place.
//!
//! A listen-only event tap reads key downs; it never holds or changes them.
//! Sona's own keystrokes come back through the tap and are skipped by their
//! source process. While macOS secure input is on (a password field), the tap
//! receives nothing, so nothing typed there is read.

use crate::input::EnigoState;
use crate::settings::get_settings;
use crate::snippets::{fill_placeholders, Keystroke, TypedExpansion, TypedTriggers};
use enigo::{Direction, Key, Keyboard};
use objc2_core_foundation::{kCFRunLoopCommonModes, CFMachPort, CFRetained, CFRunLoop};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType, CGPreflightListenEventAccess,
};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use tauri::{AppHandle, Manager};

static STARTED: AtomicBool = AtomicBool::new(false);
/// Read on every key: while off, the tap passes keys through unread.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Key codes that move the caret or leave the field: what was typed before
/// them no longer sits in front of the caret.
const CARET_KEYS: [i64; 13] = [36, 48, 53, 76, 115, 116, 117, 119, 121, 123, 124, 125, 126];
const DELETE_KEY: i64 = 51;

/// Follows the setting: starts the tap once when it is on and macOS already
/// lets Sona read the keyboard, and stops reading keys when it is off. It
/// never asks for that permission itself.
pub(crate) fn start_if_enabled(app: &AppHandle) {
    let enabled = get_settings(app).typed_snippets_enabled;
    ENABLED.store(enabled, Ordering::SeqCst);
    if !enabled {
        return;
    }
    if !CGPreflightListenEventAccess() {
        log::warn!("Typed shortcuts are on, but Sona may not read the keyboard yet");
        return;
    }
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let (sender, receiver) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("typed-shortcuts-tap".into())
        .spawn(move || run_tap(sender));
    if let Err(error) = spawned {
        log::warn!("Typed shortcuts did not start: {error}");
        STARTED.store(false, Ordering::SeqCst);
        return;
    }
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("typed-shortcuts".into())
        .spawn(move || expand(&app, &receiver));
}

struct TapContext {
    sender: Sender<Keystroke>,
    tap: Option<CFRetained<CFMachPort>>,
    own_pid: i64,
}

fn run_tap(sender: Sender<Keystroke>) {
    let mask: CGEventMask = [
        CGEventType::KeyDown,
        CGEventType::LeftMouseDown,
        CGEventType::RightMouseDown,
        CGEventType::OtherMouseDown,
    ]
    .iter()
    .fold(0, |mask, kind| mask | (1 << kind.0));
    // The tap and its context live as long as the process: the thread parks
    // in the run loop below and never returns.
    let context = Box::leak(Box::new(TapContext {
        sender,
        tap: None,
        own_pid: i64::from(std::process::id()),
    }));
    // SAFETY: `on_event` matches the tap callback signature and `context`
    // outlives the tap.
    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            mask,
            Some(on_event),
            std::ptr::from_mut(context).cast(),
        )
    };
    let Some(tap) = tap else {
        log::warn!("Typed shortcuts: macOS refused the keyboard tap");
        STARTED.store(false, Ordering::SeqCst);
        return;
    };
    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&tap), 0) else {
        STARTED.store(false, Ordering::SeqCst);
        return;
    };
    let Some(run_loop) = CFRunLoop::current() else {
        STARTED.store(false, Ordering::SeqCst);
        return;
    };
    // SAFETY: a framework constant, read only.
    run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
    CGEvent::tap_enable(&tap, true);
    context.tap = Some(tap);
    CFRunLoop::run();
}

unsafe extern "C-unwind" fn on_event(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    context: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: `context` is the leaked `TapContext` handed to `tap_create`.
    let context = unsafe { &*context.cast::<TapContext>() };
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        if let Some(tap) = &context.tap {
            CGEvent::tap_enable(tap, true);
        }
        return event.as_ptr();
    }
    if !ENABLED.load(Ordering::SeqCst) {
        return event.as_ptr();
    }
    // SAFETY: the tap hands a live event for the duration of the callback.
    let event_ref = unsafe { event.as_ref() };
    if kind != CGEventType::KeyDown {
        let _ = context.sender.send(Keystroke::Reset);
        return event.as_ptr();
    }
    let source = CGEvent::integer_value_field(Some(event_ref), CGEventField::EventSourceUnixProcessID);
    if source == context.own_pid {
        return event.as_ptr();
    }
    for keystroke in keystrokes(event_ref) {
        let _ = context.sender.send(keystroke);
    }
    event.as_ptr()
}

fn keystrokes(event: &CGEvent) -> Vec<Keystroke> {
    let flags = CGEvent::flags(Some(event));
    if flags.intersects(CGEventFlags::MaskCommand | CGEventFlags::MaskControl) {
        return vec![Keystroke::Reset];
    }
    let code = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode);
    if code == DELETE_KEY {
        return vec![Keystroke::Erase];
    }
    if CARET_KEYS.contains(&code) {
        return vec![Keystroke::Reset];
    }
    let mut units = [0u16; 8];
    let mut length = 0;
    // SAFETY: `units` holds `units.len()` code units and `length` is written
    // with how many were filled.
    unsafe {
        CGEvent::keyboard_get_unicode_string(
            Some(event),
            units.len() as _,
            &mut length,
            units.as_mut_ptr(),
        );
    }
    let typed: Vec<Keystroke> = char::decode_utf16(units.iter().copied().take(length as usize))
        .map(|unit| match unit {
            Ok(character) if !character.is_control() => Keystroke::Text(character),
            _ => Keystroke::Reset,
        })
        .collect();
    if typed.is_empty() {
        vec![Keystroke::Reset]
    } else {
        typed
    }
}

fn expand(app: &AppHandle, keystrokes: &Receiver<Keystroke>) {
    let mut triggers = TypedTriggers::default();
    let mut snippets = Vec::new();
    for keystroke in keystrokes {
        if keystroke == Keystroke::Text(' ') {
            let settings = get_settings(app);
            if !settings.typed_snippets_enabled || !settings.snippets_enabled {
                triggers.key(Keystroke::Reset, &[]);
                continue;
            }
            snippets = settings.snippets;
        }
        if let Some(found) = triggers.key(keystroke, &snippets) {
            if let Err(error) = type_expansion(app, &found) {
                log::warn!("Typed shortcut not expanded: {error}");
            }
        }
    }
}

fn type_expansion(app: &AppHandle, found: &TypedExpansion) -> Result<(), String> {
    let filled = fill_placeholders(
        &found.expansion,
        &chrono::Local::now(),
        crate::clipboard::current_text,
    );
    let state = app
        .try_state::<EnigoState>()
        .ok_or("Sona may not type yet")?;
    let mut enigo = state.0.lock().map_err(|error| error.to_string())?;
    for _ in 0..found.erase {
        enigo
            .key(Key::Backspace, Direction::Click)
            .map_err(|error| error.to_string())?;
    }
    let text = if filled.caret_back == 0 {
        format!("{} ", filled.text)
    } else {
        filled.text
    };
    enigo.text(&text).map_err(|error| error.to_string())?;
    for _ in 0..filled.caret_back {
        enigo
            .key(Key::LeftArrow, Direction::Click)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}
