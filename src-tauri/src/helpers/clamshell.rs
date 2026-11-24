pub(crate) const LID_CLOSED_MICROPHONE: &str =
    "The lid is closed. Open it or choose another microphone.";

#[cfg(target_os = "macos")]
use std::process::Command;

/// Whether macOS reports the laptop lid closed.
#[cfg(target_os = "macos")]
pub fn is_clamshell() -> Result<bool, String> {
    use objc2_core_foundation::{CFBoolean, CFRetained, CFString, CFType};
    use objc2_foundation::ns_string;
    use std::ffi::{c_char, c_void};
    use std::ptr::{self, NonNull};

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
        fn IORegistryEntryCreateCFProperty(
            entry: u32,
            key: *const CFString,
            allocator: *const c_void,
            options: u32,
        ) -> *const CFType;
        fn IOObjectRelease(object: u32) -> i32;
    }

    // SAFETY: the class name is a static, NUL-terminated C string.
    let matching = unsafe { IOServiceMatching(c"IOPMrootDomain".as_ptr()) };
    if matching.is_null() {
        return Err("macOS could not read the lid state.".to_owned());
    }
    // SAFETY: this consumes the matching dictionary, including on failure.
    let root = unsafe { IOServiceGetMatchingService(0, matching) };
    if root == 0 {
        return Ok(false);
    }
    // SAFETY: the key is a toll-free CFString; Create transfers ownership of the property.
    let raw = unsafe {
        let property = IORegistryEntryCreateCFProperty(
            root,
            ptr::from_ref(ns_string!("AppleClamshellState")).cast(),
            ptr::null(),
            0,
        );
        IOObjectRelease(root);
        property
    };
    let Some(raw) = NonNull::new(raw.cast_mut()) else {
        return Ok(false);
    };
    // SAFETY: the Create call transferred one retain to this function.
    let property = unsafe { CFRetained::from_raw(raw) };
    property
        .downcast_ref::<CFBoolean>()
        .map(CFBoolean::value)
        .ok_or_else(|| "macOS returned an unreadable lid state.".to_owned())
}

/// Checks if the Mac is a laptop by detecting battery presence
///
/// This uses pmset to check for battery information.
/// Returns true if a battery is detected (laptop), false otherwise (desktop)
#[cfg(target_os = "macos")]
#[tauri::command]
#[specta::specta]
pub fn is_laptop() -> Result<bool, String> {
    let output = Command::new("pmset")
        .arg("-g")
        .arg("batt")
        .output()
        .map_err(|e| e.to_string())?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Check if InternalBattery is present (laptops have batteries, desktops typically don't)
    Ok(stdout.contains("InternalBattery"))
}

/// Stub implementation for non-macOS platforms
/// Always returns false since clamshell mode is macOS-specific
#[cfg(not(target_os = "macos"))]
pub fn is_clamshell() -> Result<bool, String> {
    Ok(false)
}

/// Stub implementation for non-macOS platforms
/// Always returns false since laptop detection is macOS-specific
#[cfg(not(target_os = "macos"))]
#[tauri::command]
#[specta::specta]
pub fn is_laptop() -> Result<bool, String> {
    Ok(false)
}
