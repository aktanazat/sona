//! CoreAudio property reads, shared by the recorder and meeting detection.

use objc2_core_audio::{
    kAudioDevicePropertyTransportType, kAudioDeviceTransportTypeBluetooth,
    kAudioDeviceTransportTypeBluetoothLE, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectSystemObject, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress,
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

/// Every device CoreAudio lists, or `None` when it refuses the query.
fn device_ids() -> Option<Vec<AudioObjectID>> {
    let system = u32::try_from(kAudioObjectSystemObject).ok()?;
    let mut property = address(kAudioHardwarePropertyDevices);
    let mut size: u32 = 0;
    // SAFETY: `property` and `size` are live stack slots for this call.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            system,
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
    let mut device_ids: Vec<AudioObjectID> = vec![0; usize::try_from(size).ok()? / id_size];
    // `size` states the buffer's byte length, and CoreAudio rewrites it with
    // the bytes it filled: fewer when a device left between the two calls.
    // SAFETY: `device_ids` owns `size` bytes of initialized storage.
    let status = unsafe {
        AudioObjectGetPropertyData(
            system,
            NonNull::from(&mut property),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(device_ids.as_mut_ptr())?.cast(),
        )
    };
    if status != 0 {
        return None;
    }
    device_ids.truncate(usize::try_from(size).ok()? / id_size);
    Some(device_ids)
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
