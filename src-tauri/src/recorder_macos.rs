use crate::recorder::RecorderNativeStatus;
use serde::Deserialize;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;
use std::sync::Arc;

const BRIDGE_OK: c_int = 0;

type StatusCallback = unsafe extern "C" fn(*mut c_void, c_int, u64);

extern "C" {
    fn sona_recorder_preflight_json() -> *mut c_char;
    fn sona_recorder_free_string(value: *mut c_char);
    fn sona_recorder_preview_start(
        camera_id: *const c_char,
        microphone_id: *const c_char,
        camera_enabled: c_int,
        microphone_enabled: c_int,
        status_callback: Option<StatusCallback>,
        callback_context: *mut c_void,
        out_handle: *mut *mut c_void,
    ) -> c_int;
    fn sona_recorder_start(handle: *mut c_void, output_path: *const c_char) -> c_int;
    fn sona_recorder_pause(handle: *mut c_void) -> c_int;
    fn sona_recorder_resume(handle: *mut c_void) -> c_int;
    fn sona_recorder_stop(
        handle: *mut c_void,
        out_width: *mut i32,
        out_height: *mut i32,
        out_duration_ms: *mut u64,
    ) -> c_int;
    fn sona_recorder_cancel(handle: *mut c_void) -> c_int;
    fn sona_recorder_destroy(handle: *mut c_void);
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeRecorderPreflight {
    pub availability: String,
    pub camera_devices: Vec<NativeRecorderDevice>,
    pub microphone_devices: Vec<NativeRecorderDevice>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct NativeRecorderDevice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeRecorderError {
    Unsupported,
    CameraPermissionDenied,
    MicrophonePermissionDenied,
    SourceSelectionCancelled,
    SourceUnavailable,
    CameraUnavailable,
    MicrophoneUnavailable,
    StreamFailed,
    TimestampDiscontinuity,
    WriterFailed,
    OutputFinalizeFailed,
    InvalidState,
}

impl NativeRecorderError {
    fn from_bridge(result: c_int) -> Self {
        match result {
            1 => Self::Unsupported,
            3 => Self::CameraPermissionDenied,
            4 => Self::MicrophonePermissionDenied,
            5 => Self::SourceSelectionCancelled,
            6 => Self::SourceUnavailable,
            7 => Self::CameraUnavailable,
            8 => Self::MicrophoneUnavailable,
            9 => Self::StreamFailed,
            10 => Self::TimestampDiscontinuity,
            11 => Self::WriterFailed,
            12 => Self::OutputFinalizeFailed,
            _ => Self::InvalidState,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NativeRecorderStopReport {
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
}

struct RecorderCallbackState {
    status: Arc<RecorderNativeStatus>,
}

/// Owns the retained Swift handle and the Rust callback context it calls.
pub(crate) struct NativeRecorder {
    handle: Option<*mut c_void>,
    callback_state: Option<Box<RecorderCallbackState>>,
    status: Arc<RecorderNativeStatus>,
}

// SAFETY: every FFI operation runs under ScreenRecorderManager's mutex. Swift calls the status
// callback only while holding its own state lock with callbacks enabled, and `cancel` clears
// that flag under the same lock before returning, so once `destroy` returns no callback is
// running and none can start. That edge is what lets `callback_state` be dropped here; the
// producer and encoder queue drains inside the bridge protect the writer, not this box.
unsafe impl Send for NativeRecorder {}

impl NativeRecorder {
    pub(crate) fn preflight() -> Result<NativeRecorderPreflight, NativeRecorderError> {
        // SAFETY: the bridge returns either null or an owned UTF-8 C string released below.
        let raw = unsafe { sona_recorder_preflight_json() };
        if raw.is_null() {
            return Err(NativeRecorderError::Unsupported);
        }
        // SAFETY: raw was checked for null and stays valid until sona_recorder_free_string.
        let json = unsafe { CStr::from_ptr(raw) }.to_bytes().to_vec();
        // SAFETY: raw is the bridge allocation described above and is released exactly once.
        unsafe { sona_recorder_free_string(raw) };
        serde_json::from_slice(&json).map_err(|_| NativeRecorderError::Unsupported)
    }

    pub(crate) fn preview_start(
        camera_id: Option<&str>,
        microphone_id: Option<&str>,
        camera_enabled: bool,
        microphone_enabled: bool,
    ) -> Result<Self, NativeRecorderError> {
        let camera_id = c_string(camera_id)?;
        let microphone_id = c_string(microphone_id)?;
        let status = Arc::new(RecorderNativeStatus::default());
        let mut callback_state = Box::new(RecorderCallbackState {
            status: Arc::clone(&status),
        });
        let callback_context = ptr::from_mut(&mut *callback_state).cast::<c_void>();
        let mut handle = ptr::null_mut();
        // SAFETY: call inputs live through the call and the callback state stays owned until destroy drains callbacks.
        let result = unsafe {
            sona_recorder_preview_start(
                camera_id
                    .as_ref()
                    .map_or(ptr::null(), |value| value.as_ptr()),
                microphone_id
                    .as_ref()
                    .map_or(ptr::null(), |value| value.as_ptr()),
                c_int::from(camera_enabled),
                c_int::from(microphone_enabled),
                Some(recorder_status_callback),
                callback_context,
                &mut handle,
            )
        };
        if result != BRIDGE_OK || handle.is_null() {
            return Err(NativeRecorderError::from_bridge(result));
        }
        Ok(Self {
            handle: Some(handle),
            callback_state: Some(callback_state),
            status,
        })
    }

    pub(crate) fn start(&mut self, output_path: &str) -> Result<(), NativeRecorderError> {
        let output_path =
            CString::new(output_path).map_err(|_| NativeRecorderError::WriterFailed)?;
        let handle = self.handle.ok_or(NativeRecorderError::InvalidState)?;
        // SAFETY: handle remains retained by Self and output_path remains valid for the call.
        let result = unsafe { sona_recorder_start(handle, output_path.as_ptr()) };
        bridge_result(result)
    }

    pub(crate) fn pause(&mut self) -> Result<(), NativeRecorderError> {
        let handle = self.handle.ok_or(NativeRecorderError::InvalidState)?;
        // SAFETY: handle remains retained by Self for this synchronous control call.
        bridge_result(unsafe { sona_recorder_pause(handle) })
    }

    pub(crate) fn resume(&mut self) -> Result<(), NativeRecorderError> {
        let handle = self.handle.ok_or(NativeRecorderError::InvalidState)?;
        // SAFETY: handle remains retained by Self for this synchronous control call.
        bridge_result(unsafe { sona_recorder_resume(handle) })
    }

    pub(crate) fn stop(&mut self) -> Result<NativeRecorderStopReport, NativeRecorderError> {
        let handle = self.handle.ok_or(NativeRecorderError::InvalidState)?;
        let mut width = 0;
        let mut height = 0;
        let mut duration_ms = 0;
        // SAFETY: handle and all out-pointers remain valid for this synchronous finalization call.
        let result =
            unsafe { sona_recorder_stop(handle, &mut width, &mut height, &mut duration_ms) };
        bridge_result(result)?;
        let width = u32::try_from(width).map_err(|_| NativeRecorderError::OutputFinalizeFailed)?;
        let height =
            u32::try_from(height).map_err(|_| NativeRecorderError::OutputFinalizeFailed)?;
        Ok(NativeRecorderStopReport {
            width,
            height,
            duration_ms,
        })
    }

    pub(crate) fn dropped_video_frames(&self) -> u64 {
        self.status.dropped_video_frames()
    }

    pub(crate) fn take_failure(&self) -> Option<NativeRecorderError> {
        // Status codes carry the same numbers as call results, so one table decodes both.
        self.status
            .take_failure()
            .map(NativeRecorderError::from_bridge)
    }

    pub(crate) fn cancel_and_destroy(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        // SAFETY: handle was retained by Self and Swift disables/drains callbacks in destroy.
        unsafe {
            let _ = sona_recorder_cancel(handle);
            sona_recorder_destroy(handle);
        }
        self.callback_state.take();
    }
}

impl Drop for NativeRecorder {
    fn drop(&mut self) {
        self.cancel_and_destroy();
    }
}

fn c_string(value: Option<&str>) -> Result<Option<CString>, NativeRecorderError> {
    value
        .map(CString::new)
        .transpose()
        .map_err(|_| NativeRecorderError::InvalidState)
}

fn bridge_result(result: c_int) -> Result<(), NativeRecorderError> {
    (result == BRIDGE_OK)
        .then_some(())
        .ok_or_else(|| NativeRecorderError::from_bridge(result))
}

unsafe extern "C" fn recorder_status_callback(context: *mut c_void, status: c_int, dropped: u64) {
    if context.is_null() {
        return;
    }
    // SAFETY: NativeRecorder owns this Box until Swift's destroy drains every callback queue.
    let callback_state = unsafe { &*(context.cast::<RecorderCallbackState>()) };
    callback_state.status.record(status, dropped);
}

#[cfg(test)]
mod tests {
    use super::NativeRecorderError;

    /// The Swift bridge reports asynchronous failures with the same numbers it returns from a
    /// call, so this table has to decode both. If the two enums drift apart, a stream failure
    /// starts arriving as a permission denial and the UI asks for the wrong thing.
    #[test]
    fn status_codes_decode_through_the_shared_bridge_table() {
        assert_eq!(
            NativeRecorderError::from_bridge(9),
            NativeRecorderError::StreamFailed
        );
        assert_eq!(
            NativeRecorderError::from_bridge(10),
            NativeRecorderError::TimestampDiscontinuity
        );
        assert_eq!(
            NativeRecorderError::from_bridge(11),
            NativeRecorderError::WriterFailed
        );
        assert_eq!(
            NativeRecorderError::from_bridge(6),
            NativeRecorderError::SourceUnavailable
        );
    }
}
