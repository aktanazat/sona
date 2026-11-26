//! The words in a picture, and what it shows, read on this Mac through the
//! Vision bridge. A meeting's pictures serve search and the transcript's
//! "On screen" marks only: no engine that writes notes or answers chat reads
//! them. A phone's photo thought is read so the Mac can file it.

use serde::Deserialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScreenTextError {
    /// The bytes are not a picture, so they never will be read.
    Unreadable,
    /// Vision did not read the picture this time.
    Failed,
}

/// The words one picture shows.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ScreenText {
    /// The tallest line, the first of several as tall, such as a slide's
    /// title. Empty when no line has two letters or digits.
    pub title: String,
    /// Every line in reading order, one per line.
    pub text: String,
}

#[derive(Deserialize)]
struct Line {
    text: String,
    /// The line's height as a share of the picture's.
    height: f64,
}

pub(crate) fn read(png: &[u8]) -> Result<ScreenText, ScreenTextError> {
    Ok(screen_text(platform::read_lines(png)?))
}

/// What a picture shows, as Vision's scene labels, most confident first.
pub(crate) fn labels(image: &[u8]) -> Result<Vec<String>, ScreenTextError> {
    platform::labels(image)
}

fn screen_text(lines: Vec<Line>) -> ScreenText {
    let lines: Vec<Line> = lines
        .into_iter()
        .filter_map(|line| {
            let text = line.text.trim();
            (!text.is_empty()).then(|| Line {
                text: text.to_owned(),
                height: line.height,
            })
        })
        .collect();
    let mut title: Option<&Line> = None;
    for line in &lines {
        let worded = line
            .text
            .chars()
            .filter(|c| c.is_alphanumeric())
            .nth(1)
            .is_some();
        if worded && title.is_none_or(|title| line.height > title.height) {
            title = Some(line);
        }
    }
    ScreenText {
        title: title.map(|line| line.text.clone()).unwrap_or_default(),
        text: lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Line, ScreenTextError};
    use serde::de::DeserializeOwned;
    use std::ffi::{c_char, c_void, CStr};

    type Bridge = unsafe extern "C" fn(*const u8, usize, *mut *mut c_char) -> i32;

    unsafe extern "C" {
        fn sona_screen_text_read(
            bytes: *const u8,
            length: usize,
            out_json: *mut *mut c_char,
        ) -> i32;
        fn sona_image_labels(bytes: *const u8, length: usize, out_json: *mut *mut c_char) -> i32;
        fn sona_screen_text_free(pointer: *mut c_void);
    }

    pub(super) fn read_lines(png: &[u8]) -> Result<Vec<Line>, ScreenTextError> {
        call(sona_screen_text_read, png)
    }

    pub(super) fn labels(image: &[u8]) -> Result<Vec<String>, ScreenTextError> {
        call(sona_image_labels, image)
    }

    fn call<T: DeserializeOwned>(bridge: Bridge, image: &[u8]) -> Result<T, ScreenTextError> {
        let mut json: *mut c_char = std::ptr::null_mut();
        // SAFETY: the bridge reads `image.len()` bytes and writes one pointer into the local.
        let result = unsafe { bridge(image.as_ptr(), image.len(), &mut json) };
        match result {
            0 if !json.is_null() => {}
            2 => return Err(ScreenTextError::Unreadable),
            _ => return Err(ScreenTextError::Failed),
        }
        // SAFETY: on success the bridge hands over a NUL-terminated string it
        // allocated; it is copied once here.
        let text = unsafe { CStr::from_ptr(json) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: json is the bridge's live allocation and nothing borrows it now.
        unsafe { sona_screen_text_free(json.cast()) };
        serde_json::from_str(&text).map_err(|_| ScreenTextError::Failed)
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{Line, ScreenTextError};

    pub(super) fn read_lines(_png: &[u8]) -> Result<Vec<Line>, ScreenTextError> {
        Err(ScreenTextError::Failed)
    }

    pub(super) fn labels(_image: &[u8]) -> Result<Vec<String>, ScreenTextError> {
        Err(ScreenTextError::Failed)
    }
}
