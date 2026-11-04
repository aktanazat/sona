use cpal::traits::{DeviceTrait, HostTrait};

pub struct CpalDeviceInfo {
    pub index: String,
    pub name: String,
    pub is_default: bool,
    pub device: cpal::Device,
}

pub fn list_input_devices() -> Result<Vec<CpalDeviceInfo>, Box<dyn std::error::Error>> {
    let host = crate::audio_toolkit::get_cpal_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());

    let mut out = Vec::<CpalDeviceInfo>::new();

    for (index, device) in input_devices(&host)?.enumerate() {
        let name = device.name().unwrap_or_else(|_| "Unknown".into());

        let is_default = Some(name.clone()) == default_name;

        out.push(CpalDeviceInfo {
            index: index.to_string(),
            name,
            is_default,
            device,
        });
    }

    Ok(out)
}

/// cpal's own input filter builds an input audio unit per device, which makes
/// macOS ask for the microphone. The stream configuration property answers
/// the same question without one.
#[cfg(target_os = "macos")]
fn input_devices(
    host: &cpal::Host,
) -> Result<impl Iterator<Item = cpal::Device>, Box<dyn std::error::Error>> {
    use super::core_audio::{device_ids, input_channel_count};

    let ids = device_ids().ok_or("CoreAudio refused the device list")?;
    let devices: Vec<cpal::Device> = host.devices()?.collect();
    // cpal reads this same list but hides each device's id, so the two pair by
    // position, which holds only while the list did not change in between.
    if devices.len() != ids.len() || device_ids().as_ref() != Some(&ids) {
        return Err("the audio device list changed while it was read".into());
    }
    Ok(devices
        .into_iter()
        .zip(ids)
        .filter(|(_, id)| input_channel_count(*id).is_some_and(|channels| channels > 0))
        .map(|(device, _)| device))
}

#[cfg(not(target_os = "macos"))]
fn input_devices(
    host: &cpal::Host,
) -> Result<impl Iterator<Item = cpal::Device>, Box<dyn std::error::Error>> {
    Ok(host.input_devices()?)
}

pub fn list_output_devices() -> Result<Vec<CpalDeviceInfo>, Box<dyn std::error::Error>> {
    let host = crate::audio_toolkit::get_cpal_host();
    let default_name = host.default_output_device().and_then(|d| d.name().ok());

    let mut out = Vec::<CpalDeviceInfo>::new();

    for (index, device) in host.output_devices()?.enumerate() {
        let name = device.name().unwrap_or_else(|_| "Unknown".into());

        let is_default = Some(name.clone()) == default_name;

        out.push(CpalDeviceInfo {
            index: index.to_string(),
            name,
            is_default,
            device,
        });
    }

    Ok(out)
}
