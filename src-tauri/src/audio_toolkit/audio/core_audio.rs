//! CoreAudio property reads, shared by the recorder and meeting detection.

use objc2_core_audio::{
    kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioDevicePropertyTransportType,
    kAudioDeviceTransportTypeBluetooth, kAudioDeviceTransportTypeBluetoothLE,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
    kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioProcessPropertyBundleID, kAudioProcessPropertyIsRunningOutput, kAudioProcessPropertyPID,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress,
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
            }) && read_string(device_id, kAudioObjectPropertyName).as_deref() == Some(name)
        })
}

pub(crate) fn input_configuration_identity(name: &str) -> Option<[u64; 6]> {
    let mut matching = device_ids()?
        .into_iter()
        .filter(|id| read_string(*id, kAudioObjectPropertyName).as_deref() == Some(name));
    let device_id = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    let mut property = AudioObjectPropertyAddress {
        mSelector: objc2_core_audio::kAudioDevicePropertyStreamFormat,
        mScope: objc2_core_audio::kAudioObjectPropertyScopeInput,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut format = objc2_core_audio_types::AudioStreamBasicDescription {
        mSampleRate: 0.0,
        mFormatID: 0,
        mFormatFlags: 0,
        mBytesPerPacket: 0,
        mFramesPerPacket: 0,
        mBytesPerFrame: 0,
        mChannelsPerFrame: 0,
        mBitsPerChannel: 0,
        mReserved: 0,
    };
    let mut size = u32::try_from(std::mem::size_of_val(&format)).ok()?;
    // SAFETY: the typed format buffer and its exact byte length remain live for this call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            NonNull::from(&mut property),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut format).cast(),
        )
    };
    if status != 0 {
        return None;
    }
    let mut buffers = objc2_core_audio_types::AudioBufferList {
        mNumberBuffers: 0,
        mBuffers: [objc2_core_audio_types::AudioBuffer {
            mNumberChannels: 0,
            mDataByteSize: 0,
            mData: std::ptr::null_mut(),
        }],
    };
    property.mSelector = objc2_core_audio::kAudioDevicePropertyStreamConfiguration;
    let mut size = u32::try_from(std::mem::size_of_val(&buffers)).ok()?;
    // SAFETY: CoreAudio receives the actual capacity of this one-buffer list and refuses a larger list.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            NonNull::from(&mut property),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut buffers).cast(),
        )
    };
    if status != 0
        || buffers.mNumberBuffers != 1
        || buffers.mBuffers[0].mNumberChannels != format.mChannelsPerFrame
    {
        return None;
    }
    Some([
        u64::from(device_id),
        format.mSampleRate.to_bits(),
        u64::from(format.mChannelsPerFrame),
        u64::from(format.mFormatID),
        u64::from(format.mFormatFlags),
        u64::from(format.mBitsPerChannel),
    ])
}

/// Channels across a device's input streams, from its stream configuration
/// alone. cpal's input checks build an input audio unit, which makes macOS ask
/// for the microphone; this reads a property. `None` when CoreAudio refuses.
pub(crate) fn input_channel_count(device_id: AudioObjectID) -> Option<u32> {
    use objc2_core_audio_types::{AudioBuffer, AudioBufferList};

    let mut property = AudioObjectPropertyAddress {
        mSelector: objc2_core_audio::kAudioDevicePropertyStreamConfiguration,
        mScope: objc2_core_audio::kAudioObjectPropertyScopeInput,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size: u32 = 0;
    // SAFETY: `property` and `size` are live stack slots for this call.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            device_id,
            NonNull::from(&mut property),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 {
        return None;
    }
    let buffer_bytes = std::mem::size_of::<AudioBuffer>();
    // Whole `AudioBuffer` slots give the variable-length list its alignment.
    let empty = AudioBuffer {
        mNumberChannels: 0,
        mDataByteSize: 0,
        mData: std::ptr::null_mut(),
    };
    let mut storage = vec![empty; usize::try_from(size).ok()?.div_ceil(buffer_bytes)];
    let list = storage.as_mut_ptr().cast::<AudioBufferList>();
    // `size` states the storage's byte length, and CoreAudio rewrites it with
    // the bytes it filled.
    // SAFETY: `storage` owns at least `size` bytes, aligned for the list.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            NonNull::from(&mut property),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(list)?.cast(),
        )
    };
    if status != 0 {
        return None;
    }
    let filled = usize::try_from(size).ok()?;
    let header = std::mem::offset_of!(AudioBufferList, mBuffers);
    if filled < header {
        return None;
    }
    // SAFETY: CoreAudio filled the list's header.
    let count = usize::try_from(unsafe { (*list).mNumberBuffers }).ok()?;
    if header + count * buffer_bytes > filled {
        return None;
    }
    // SAFETY: all `count` buffers lie within the bytes CoreAudio filled.
    let buffers = unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!((*list).mBuffers).cast::<AudioBuffer>(),
            count,
        )
    };
    buffers.iter().try_fold(0_u32, |total, buffer| {
        total.checked_add(buffer.mNumberChannels)
    })
}

/// The input channels of the first device with this name that has any: the
/// device a by-name lookup in the input list resolves to.
pub(crate) fn input_channel_count_named(name: &str) -> Option<u32> {
    device_ids()?
        .into_iter()
        .filter(|id| read_string(*id, kAudioObjectPropertyName).as_deref() == Some(name))
        .find_map(|id| input_channel_count(id).filter(|channels| *channels > 0))
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

/// Whether the app `bundle_id` names is playing sound through its own
/// processes, including the helpers a browser or an Electron app plays from,
/// whose bundle ids extend the app's. `None` when CoreAudio refuses the
/// per-process list, as releases before macOS 14.2 do, or lists no process
/// of the app.
pub(crate) fn app_is_playing(bundle_id: &str) -> Option<bool> {
    let system = u32::try_from(kAudioObjectSystemObject).ok()?;
    let mut listed = false;
    let mut playing = false;
    for process in object_list(system, kAudioHardwarePropertyProcessObjectList)? {
        let owned = read_string(process, kAudioProcessPropertyBundleID)
            .is_some_and(|process_bundle_id| is_app_or_helper(&process_bundle_id, bundle_id));
        if !owned {
            continue;
        }
        listed = true;
        playing |= read_u32(process, kAudioProcessPropertyIsRunningOutput)
            .is_some_and(|running| running != 0);
    }
    listed.then_some(playing)
}

/// True when `process_bundle_id` is `bundle_id`'s own or one of its
/// helpers'. Compared without case: CoreAudio spells an id as its bundle
/// does, and the allowlist stores ids lowercased.
fn is_app_or_helper(process_bundle_id: &str, bundle_id: &str) -> bool {
    process_bundle_id
        .get(..bundle_id.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(bundle_id))
        && matches!(
            process_bundle_id.as_bytes().get(bundle_id.len()),
            None | Some(b'.')
        )
}

/// Every device CoreAudio lists, or `None` when it refuses the query.
pub(crate) fn device_ids() -> Option<Vec<AudioObjectID>> {
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

fn read_string(object_id: AudioObjectID, selector: u32) -> Option<String> {
    let mut property = address(selector);
    let mut string: *const CFString = std::ptr::null();
    let mut size = u32::try_from(std::mem::size_of::<*const CFString>()).ok()?;
    // `size` states `string`'s exact byte length, as CoreAudio requires.
    // SAFETY: `property` and `string` are live stack slots for this call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            NonNull::from(&mut property),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut string).cast(),
        )
    };
    if status != 0 {
        return None;
    }
    // CoreAudio hands the caller a retained CFString it must release.
    // SAFETY: `string` is that owned reference; `CFRetained` releases it on drop.
    let string = unsafe { CFRetained::from_raw(NonNull::new(string.cast_mut())?) };
    Some(string.to_string())
}

#[cfg(test)]
mod tests {
    use super::is_app_or_helper;

    /* A browser call plays from a helper whose bundle id extends the
     * browser's, spelled as its bundle spells it while the allowlist is
     * lowercase. An id that only starts with the same letters is another
     * app, and counting its sound as the meeting's would keep alive a
     * meeting that ended. */
    #[test]
    fn an_app_owns_its_helpers_and_nothing_that_only_shares_its_prefix() {
        assert!(is_app_or_helper("us.zoom.xos", "us.zoom.xos"));
        assert!(is_app_or_helper(
            "com.google.Chrome.helper",
            "com.google.chrome"
        ));
        assert!(!is_app_or_helper("com.google.chromex", "com.google.chrome"));
        assert!(!is_app_or_helper(
            "com.apple.WebKit.GPU",
            "com.apple.safari"
        ));
        assert!(!is_app_or_helper("com.google", "com.google.chrome"));
    }
}
