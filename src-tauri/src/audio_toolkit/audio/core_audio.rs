//! CoreAudio property reads, shared by the recorder and meeting detection.

use objc2_core_audio::{
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, AudioObjectGetPropertyData,
    AudioObjectID, AudioObjectPropertyAddress,
};
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
