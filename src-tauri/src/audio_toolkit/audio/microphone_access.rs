/// Whether the user has already allowed microphone access. Reading the answer
/// never asks for it; opening an input stream is what asks.
#[cfg(target_os = "macos")]
pub(crate) fn microphone_authorized() -> bool {
    unsafe extern "C" {
        fn sona_microphone_authorized() -> bool;
    }
    // SAFETY: the Swift bridge takes no arguments and only reads AVFoundation's status.
    unsafe { sona_microphone_authorized() }
}

/// The device queries this guards show a permission dialog on macOS only.
#[cfg(not(target_os = "macos"))]
pub(crate) fn microphone_authorized() -> bool {
    true
}
