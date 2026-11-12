use crate::fs_util::write_private_file;
use crate::meeting::store::validated_png;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Serialize;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The user's own photo, as base64 PNG, or none.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct ProfilePhoto {
    pub png: Option<String>,
}

pub(crate) fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("profile-photo.png")
}

pub(crate) fn read(data_dir: &Path) -> Result<ProfilePhoto, String> {
    match std::fs::read(path(data_dir)) {
        Ok(bytes) => Ok(ProfilePhoto {
            png: Some(STANDARD.encode(bytes)),
        }),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(ProfilePhoto { png: None }),
        Err(error) => Err(format!("Sona could not read your photo: {error}")),
    }
}

/// Saves a square PNG of at most 512 pixels a side, or removes the photo.
pub(crate) fn write(data_dir: &Path, png: Option<&str>) -> Result<ProfilePhoto, String> {
    let path = path(data_dir);
    match png {
        Some(encoded) => {
            let bytes = validated_png(encoded)
                .map_err(|_| "Use a square PNG photo of at most 512 pixels a side.".to_owned())?;
            write_private_file(&path, &bytes)
                .map_err(|error| format!("Sona could not save your photo: {error}"))?;
        }
        None => match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Sona could not remove your photo: {error}")),
        },
    }
    read(data_dir)
}
