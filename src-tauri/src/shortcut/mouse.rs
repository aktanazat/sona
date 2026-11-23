use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};

use objc2_application_services::AXIsProcessTrusted;
use objc2_core_foundation::{
    kCFRunLoopCommonModes, CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use tauri::{AppHandle, Manager};

use crate::modes::TranscriptionIntent;
use crate::settings::{
    get_settings, try_update_settings_committed, update_settings, MouseDictationButton,
};
use crate::transcription_coordinator::TranscriptionCoordinator;

use super::handler::handle_transcription_shortcut;

const LABEL: &str = "Mouse button";
static SUSPENDED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static TAP: RefCell<Option<MouseTap>> = const { RefCell::new(None) };
}

impl MouseDictationButton {
    fn event_number(self) -> Option<i64> {
        match self {
            Self::Off => None,
            Self::Middle => Some(2),
            Self::Button4 => Some(3),
            Self::Button5 => Some(4),
        }
    }
}

enum HeldButton {
    Armed { push_to_talk: bool },
    Interrupted,
}

struct Context {
    app: AppHandle,
    tap: Option<CFRetained<CFMachPort>>,
    selected: MouseDictationButton,
    held: Option<HeldButton>,
    own_pid: i64,
}

struct MouseTap {
    source: CFRetained<CFRunLoopSource>,
    context: Box<Context>,
}

impl MouseTap {
    fn new(app: &AppHandle) -> Result<Self, String> {
        // SAFETY: This checks existing permission without showing a system prompt.
        if !unsafe { AXIsProcessTrusted() } {
            return Err("Allow Sona in System Settings → Privacy & Security → Accessibility to use a mouse shortcut.".into());
        }
        let mut context = Box::new(Context {
            app: app.clone(),
            tap: None,
            selected: MouseDictationButton::Off,
            held: None,
            own_pid: i64::from(std::process::id()),
        });
        let mask = (1 << CGEventType::OtherMouseDown.0)
            | (1 << CGEventType::OtherMouseUp.0)
            | (1 << CGEventType::OtherMouseDragged.0);
        // SAFETY: The context's allocation is stable until Drop invalidates both
        // the tap and its source on this same run loop.
        let tap = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                mask,
                Some(on_event),
                ptr::from_mut(context.as_mut()).cast(),
            )
        }
        .ok_or_else(|| "macOS did not allow the mouse shortcut.".to_owned())?;
        let source = CFMachPort::new_run_loop_source(None, Some(&tap), 0)
            .ok_or_else(|| "macOS could not start the mouse shortcut.".to_owned())?;
        let Some(run_loop) = CFRunLoop::current() else {
            return Err("macOS could not start the mouse shortcut.".into());
        };
        CGEvent::tap_enable(&tap, true);
        context.tap = Some(tap);
        // SAFETY: The Apple framework supplies this immutable mode constant.
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
        Ok(Self { source, context })
    }
}

impl Drop for MouseTap {
    fn drop(&mut self) {
        self.source.invalidate();
        if let Some(tap) = &self.context.tap {
            tap.invalidate();
        }
    }
}

impl Context {
    fn input(&self, pressed: bool, push_to_talk: bool) {
        handle_transcription_shortcut(
            &self.app,
            TranscriptionIntent::ActiveModeMouse,
            LABEL,
            pressed,
            push_to_talk,
        );
    }
}

unsafe extern "C-unwind" fn on_event(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    context: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: Only this run loop calls the callback, and the tap owns the context.
    let context = unsafe { &mut *context.cast::<Context>() };
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        context.held = context.held.take().map(|_| HeldButton::Interrupted);
        interrupt(&context.app);
        if let Some(tap) = &context.tap {
            CGEvent::tap_enable(tap, true);
        }
        return event.as_ptr();
    }
    let Some(selected) = context.selected.event_number() else {
        return event.as_ptr();
    };
    // SAFETY: The tap supplies a valid event for the duration of this call.
    let event_ref = Some(unsafe { event.as_ref() });
    if CGEvent::integer_value_field(event_ref, CGEventField::MouseEventButtonNumber) != selected
        || CGEvent::integer_value_field(event_ref, CGEventField::EventSourceUnixProcessID)
            == context.own_pid
    {
        return event.as_ptr();
    }
    match kind {
        CGEventType::OtherMouseDown => {
            if !matches!(context.held, Some(HeldButton::Armed { .. })) {
                if SUSPENDED.load(Ordering::Relaxed) || crate::secure_input::is_enabled_now() {
                    context.held = None;
                    return event.as_ptr();
                }
                let push_to_talk = get_settings(&context.app).push_to_talk;
                context.held = Some(HeldButton::Armed { push_to_talk });
                context.input(true, push_to_talk);
            }
        }
        CGEventType::OtherMouseUp => {
            let Some(held) = context.held.take() else {
                return event.as_ptr();
            };
            if let HeldButton::Armed { push_to_talk } = held {
                context.input(false, push_to_talk);
            }
        }
        CGEventType::OtherMouseDragged if context.held.is_some() => {}
        _ => return event.as_ptr(),
    }
    ptr::null_mut()
}

fn interrupt(app: &AppHandle) {
    if let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() {
        coordinator.interrupt_mouse_shortcut();
    }
}

pub(super) fn init(app: &AppHandle, button: MouseDictationButton) {
    if button != MouseDictationButton::Off {
        if let Err(error) = configure(app, button, false) {
            log::warn!("{error}");
        }
    }
}

pub(super) fn set_suspended(suspended: bool) {
    SUSPENDED.store(suspended, Ordering::Relaxed);
}

pub(crate) fn change_button(app: &AppHandle, button: MouseDictationButton) -> Result<(), String> {
    configure(app, button, true)
}

fn configure(app: &AppHandle, button: MouseDictationButton, persist: bool) -> Result<(), String> {
    TAP.with_borrow_mut(|slot| {
        let current = slot
            .as_ref()
            .map_or(MouseDictationButton::Off, |tap| tap.context.selected);
        if current != button
            && slot
                .as_ref()
                .is_some_and(|tap| matches!(tap.context.held, Some(HeldButton::Armed { .. })))
        {
            return Err("Release the mouse shortcut before changing it.".into());
        }
        let candidate = if button != MouseDictationButton::Off && slot.is_none() {
            Some(MouseTap::new(app)?)
        } else {
            None
        };
        if persist {
            let (previous, persisted) = try_update_settings_committed(app, |settings| {
                Ok::<_, String>(std::mem::replace(
                    &mut settings.mouse_dictation_button,
                    button,
                ))
            })?;
            if let Err(error) = persisted {
                let _ = update_settings(app, |settings| settings.mouse_dictation_button = previous);
                return Err(error.to_string());
            }
        }
        if current != button && current != MouseDictationButton::Off {
            interrupt(app);
        }
        if button == MouseDictationButton::Off {
            *slot = None;
        } else {
            if let Some(candidate) = candidate {
                *slot = Some(candidate);
            }
            if let Some(tap) = slot {
                if current != button {
                    tap.context.held = None;
                }
                tap.context.selected = button;
            }
        }
        Ok(())
    })
}
