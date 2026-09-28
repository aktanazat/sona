//! CoreAudio property reads, shared by the recorder and meeting detection.

use objc2_core_audio::{
    kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioDevicePropertyTransportType,
    kAudioDeviceTransportTypeBluetooth, kAudioDeviceTransportTypeBluetoothLE,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
    kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioProcessPropertyIsRunningOutput, kAudioProcessPropertyPID, AudioObjectGetPropertyData,
    AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
};
use objc2_core_foundation::{CFRetained, CFString};
use std::ptr::NonNull;

pub(crate) fn address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

pub(crate) fn read_u32(object_id: AudioObjectID, selector: u32) -> Option<u32> {
    let mut property = address(selector);
    let mut value: u32 = 0;
    let mut size = u32::try_from(std::mem::size_of::<u32>()).ok()?;
    // `size` states `value`'s exact byte length, as CoreAudio requires.
    // SAFETY: `property` and `value` are live stack slots for this call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            NonNull::from(&mut property),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    (status == 0).then_some(value)
}

/// True when a Bluetooth device carries this name. Matched by name because a
/// name is the one device identity cpal exposes, read from the same property.
pub(crate) fn is_bluetooth_device_named(name: &str) -> bool {
    device_ids()
        .unwrap_or_default()
        .into_iter()
        .any(|device_id| {
            read_u32(device_id, kAudioDevicePropertyTransportType).is_some_and(|transport| {
                transport == kAudioDeviceTransportTypeBluetooth
                    || transport == kAudioDeviceTransportTypeBluetoothLE
            }) && device_name(device_id).as_deref() == Some(name)
        })
}

/// Whether a process outside Sona is playing sound right now.
///
/// macOS 14.2 and later list the processes doing audio, each with its own
/// output flag. Exclude the core and its verified native shell, whose library
/// player runs in a separate process. Earlier releases refuse that list,
/// and the default output device's running flag stands in;
/// it is device-wide, so a stream this process only just closed can still
/// count until CoreAudio recomputes it. `None` when CoreAudio cannot say.
pub(crate) fn other_process_is_playing() -> Option<bool> {
    let system = u32::try_from(kAudioObjectSystemObject).ok()?;
    if let Some(processes) = object_list(system, kAudioHardwarePropertyProcessObjectList) {
        // `pid_t` is four bytes, read here as the bit pattern `process::id` uses.
        let own_pid = std::process::id();
        let shell_pid = crate::native_bridge::native_shell_pid();
        return Some(processes.into_iter().any(|process| {
            if let Some(pid) = read_u32(process, kAudioProcessPropertyPID) {
                if pid == own_pid || Some(pid) == shell_pid {
                    return false;
                }
            }
            read_u32(process, kAudioProcessPropertyIsRunningOutput)
                .is_some_and(|running| running != 0)
        }));
    }
    let device = read_u32(system, kAudioHardwarePropertyDefaultOutputDevice)?;
    // Zero is CoreAudio's "no device" sentinel, not a valid object.
    if device == 0 {
        return None;
    }
    read_u32(device, kAudioDevicePropertyDeviceIsRunningSomewhere).map(|running| running != 0)
}

/// Every device CoreAudio lists, or `None` when it refuses the query.
fn device_ids() -> Option<Vec<AudioObjectID>> {
    let system = u32::try_from(kAudioObjectSystemObject).ok()?;
    object_list(system, kAudioHardwarePropertyDevices)
}

/// The object ids `selector` lists on `object_id`, or `None` when CoreAudio
/// refuses the query.
fn object_list(object_id: AudioObjectID, selector: u32) -> Option<Vec<AudioObjectID>> {
    let mut property = address(selector);
    let mut size: u32 = 0;
    // SAFETY: `property` and `size` are live stack slots for this call.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object_id,
            NonNull::from(&mut property),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 {
        return None;
    }
    let id_size = std::mem::size_of::<AudioObjectID>();
    let count = usize::try_from(size).ok()? / id_size;
    if count == 0 {
        return Some(Vec::new());
    }
    let mut object_ids: Vec<AudioObjectID> = vec![0; count];
    // `size` states the buffer's byte length, and CoreAudio rewrites it with
    // the bytes it filled: fewer when an object left between the two calls.
    // SAFETY: `object_ids` owns `size` bytes of initialized storage.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            NonNull::from(&mut property),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(object_ids.as_mut_ptr())?.cast(),
        )
    };
    if status != 0 {
        return None;
    }
    object_ids.truncate(usize::try_from(size).ok()? / id_size);
    Some(object_ids)
}

fn device_name(device_id: AudioObjectID) -> Option<String> {
    let mut property = address(kAudioObjectPropertyName);
    let mut name: *const CFString = std::ptr::null();
    let mut size = u32::try_from(std::mem::size_of::<*const CFString>()).ok()?;
    // `size` states `name`'s exact byte length, as CoreAudio requires.
    // SAFETY: `property` and `name` are live stack slots for this call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            NonNull::from(&mut property),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut name).cast(),
        )
    };
    if status != 0 {
        return None;
    }
    // CoreAudio hands the caller a retained CFString it must release.
    // SAFETY: `name` is that owned reference; `CFRetained` releases it on drop.
    let name = unsafe { CFRetained::from_raw(NonNull::new(name.cast_mut())?) };
    Some(name.to_string())
}
