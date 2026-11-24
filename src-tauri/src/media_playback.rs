//! Other apps' playback while Sona records: the song or video macOS lists as
//! now playing is paused and played again afterwards, or the output volume is
//! lowered and put back.
//!
//! MediaRemote answers only Apple-signed processes since macOS 15.4, so the
//! calls run inside `osascript`'s JavaScript bridge rather than in the core.

use std::process::Command;

const MEDIA_REMOTE: &str = "/System/Library/PrivateFrameworks/MediaRemote.framework/";
/// `kMRPlay` and `kMRPause` in MediaRemote's command enumeration.
const PLAY: u32 = 0;
const PAUSE: u32 = 1;

/// Playback Sona changed and must put back once the recording ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackChange {
    Paused,
    Lowered { from: u8, to: u8 },
}

impl PlaybackChange {
    /// Pauses what macOS lists as now playing. `None` when nothing was
    /// playing, so the end of the recording starts nothing new.
    pub(crate) fn pause() -> Option<Self> {
        (run_media_command(PAUSE, true).as_deref() == Some("sent")).then_some(Self::Paused)
    }

    /// Lowers the output volume to a quarter. `None` when the volume cannot
    /// be read or is already silent.
    pub(crate) fn lower() -> Option<Self> {
        let from = output_volume()?;
        let to = lowered_volume(from)?;
        set_output_volume(to);
        Some(Self::Lowered { from, to })
    }

    /// Puts playback back. A pause is undone only while nothing plays, so a
    /// song he started himself in the meantime keeps playing; a lowered
    /// volume is restored only while it is still the one Sona set.
    pub(crate) fn restore(self) {
        match self {
            Self::Paused => {
                let _ = run_media_command(PLAY, false);
            }
            Self::Lowered { from, to } => {
                if let Some(volume) = restored_volume(from, to, output_volume()) {
                    set_output_volume(volume);
                }
            }
        }
    }
}

/// The volume a lowering sets: a quarter of `current`, at least one step so
/// the sound stays audible. `None` when the output is already silent.
fn lowered_volume(current: u8) -> Option<u8> {
    (current > 0).then(|| (current / 4).max(1))
}

/// The volume to restore after lowering `from` to `to`, given what the
/// output reads now. A volume he changed during the recording stays his.
fn restored_volume(from: u8, to: u8, now: Option<u8>) -> Option<u8> {
    (now == Some(to)).then_some(from)
}

/// Sends `command` to the now-playing app when its playing state is
/// `when_playing`, and prints `sent`; otherwise prints `skipped`.
fn run_media_command(command: u32, when_playing: bool) -> Option<String> {
    let script = format!(
        "ObjC.import('Foundation');\
         $.NSBundle.bundleWithPath('{MEDIA_REMOTE}').load;\
         const request = $.NSClassFromString('MRNowPlayingRequest');\
         ObjC.bindFunction('MRMediaRemoteSendCommand', ['bool', ['unsigned int', 'id']]);\
         (!request.isNil() && request.localIsPlaying === {when_playing} \
           && $.MRMediaRemoteSendCommand({command}, $())) ? 'sent' : 'skipped'"
    );
    osascript(&["-l", "JavaScript", "-e", &script])
}

fn output_volume() -> Option<u8> {
    osascript(&["-e", "output volume of (get volume settings)"])?
        .parse()
        .ok()
}

fn set_output_volume(volume: u8) {
    let _ = osascript(&["-e", &format!("set volume output volume {volume}")]);
}

fn osascript(args: &[&str]) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = Command::new("osascript").args(args).output().ok()?;
    if !output.status.success() {
        log::debug!(
            "osascript failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowering_keeps_some_sound_and_skips_silence() {
        assert_eq!(lowered_volume(80), Some(20));
        assert_eq!(lowered_volume(3), Some(1));
        assert_eq!(lowered_volume(0), None);
    }

    #[test]
    fn a_volume_changed_during_the_recording_is_left_alone() {
        assert_eq!(restored_volume(80, 20, Some(20)), Some(80));
        assert_eq!(restored_volume(80, 20, Some(55)), None);
        assert_eq!(restored_volume(80, 20, None), None);
    }
}
