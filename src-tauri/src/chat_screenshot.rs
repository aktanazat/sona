use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use image::{ImageFormat, ImageReader, Limits};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::fmt;
use std::io::Cursor;
use std::sync::Arc;

pub(crate) const MAX_SCREENSHOT_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_SCREENSHOT_BASE64_BYTES: usize = MAX_SCREENSHOT_BYTES.div_ceil(3) * 4;
const MAX_SCREENSHOT_EDGE: u32 = 2048;

/// One explicitly shared PNG. Validation happens before a turn can own it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(try_from = "String")]
pub struct ChatScreenshot(Arc<String>);

impl TryFrom<String> for ChatScreenshot {
    type Error = &'static str;

    fn try_from(encoded: String) -> Result<Self, Self::Error> {
        if encoded.len() > MAX_SCREENSHOT_BASE64_BYTES {
            return Err("The screenshot must be no larger than 2 MiB.");
        }
        let bytes = STANDARD
            .decode(&encoded)
            .map_err(|_| "The screenshot is not valid base64.")?;
        if bytes.len() > MAX_SCREENSHOT_BYTES {
            return Err("The screenshot must be no larger than 2 MiB.");
        }
        let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_SCREENSHOT_EDGE);
        limits.max_image_height = Some(MAX_SCREENSHOT_EDGE);
        limits.max_alloc = Some(32 * 1024 * 1024);
        reader.limits(limits);
        reader
            .decode()
            .map_err(|_| "Use a valid PNG screenshot no larger than 2048 by 2048 pixels.")?;
        Ok(Self(Arc::new(encoded)))
    }
}

impl fmt::Debug for ChatScreenshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatScreenshot")
            .field("encoded_bytes", &self.0.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(width, height)
            .write_to(&mut bytes, ImageFormat::Png)
            .expect("encode PNG fixture");
        bytes.into_inner()
    }

    #[test]
    fn oversized_dimensions_are_refused_before_a_turn_exists() {
        let encoded = STANDARD.encode(png(2049, 1));
        let result = serde_json::from_value::<ChatScreenshot>(serde_json::Value::String(encoded));
        assert!(result.is_err());
    }

    #[test]
    fn truncated_pixels_are_refused_even_with_a_valid_png_header() {
        let mut bytes = png(8, 8);
        let pixels = bytes.windows(4).position(|part| part == b"IDAT")
            .expect("PNG fixture has image data");
        bytes.truncate(pixels + 8);
        let result = serde_json::from_value::<ChatScreenshot>(
            serde_json::Value::String(STANDARD.encode(bytes)),
        );
        assert!(result.is_err());
    }
}
