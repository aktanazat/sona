use super::{
    bump_people_revision_in, people_revision_in, person_by_id_in, require_people_revision_in,
    SCHEMA_VERSION,
};
use crate::meeting::people_types::{
    PersonAvatar, PersonAvatarResult, PersonAvatarSetRequest, PersonAvatarSourceResult, PersonId,
    PersonPhotoProvenance,
};
use crate::meeting::store::{MeetingStore, StoreError};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use image::ImageFormat;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::io::Cursor;

const MAX_AVATAR_BYTES: usize = 1024 * 1024;
const MAX_AVATAR_EDGE: u32 = 512;
const MAX_ORIGINAL_BYTES: usize = 10 * 1024 * 1024;
const MAX_ORIGINAL_EDGE: u32 = 32_768;
const MAX_ORIGINAL_PIXELS: u64 = 100_000_000;

impl MeetingStore {
    pub(crate) fn person_avatar(
        &self,
        person_id: PersonId,
    ) -> Result<PersonAvatarResult, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        person_by_id_in(&transaction, person_id)?;
        let result = avatar_result_in(&transaction, person_id)?;
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn person_avatar_source(
        &self,
        person_id: PersonId,
    ) -> Result<PersonAvatarSourceResult, StoreError> {
        let connection = self.connection()?;
        person_by_id_in(&connection, person_id)?;
        let row = connection
            .query_row(
                "SELECT original, provenance_json FROM person_avatars WHERE person_id = ?1",
                [person_id.uuid().to_string()],
                |row| {
                    Ok((
                        row.get::<_, Option<Vec<u8>>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()?;
        match row {
            Some((original, provenance)) => Ok(PersonAvatarSourceResult {
                original_base64: original.map(|bytes| STANDARD.encode(bytes)),
                provenance: provenance
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|_| StoreError::Invalid)?,
            }),
            None => Ok(PersonAvatarSourceResult {
                original_base64: None,
                provenance: None,
            }),
        }
    }

    pub(crate) fn set_person_avatar(
        &self,
        request: PersonAvatarSetRequest,
        now_utc_ms: i64,
    ) -> Result<PersonAvatarResult, StoreError> {
        let png = request
            .png_base64
            .as_deref()
            .map(validated_png)
            .transpose()?;
        let source = match (&png, &request.provenance, &request.original_base64) {
            (Some(_), Some(provenance), Some(original)) => {
                validate_provenance(provenance, now_utc_ms)?;
                Some((
                    serde_json::to_string(provenance).map_err(|_| StoreError::Invalid)?,
                    validated_original(original)?,
                ))
            }
            (None, None, None) => None,
            _ => return Err(StoreError::Invalid),
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_people_revision_in(&transaction, request.expected_revision)?;
        person_by_id_in(&transaction, request.person_id)?;
        match (png, source) {
            (Some(png), Some((provenance, original))) => {
                transaction.execute(
                    "INSERT INTO person_avatars (person_id, png, updated_at_utc_ms, provenance_json, original)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(person_id) DO UPDATE SET
                         png = excluded.png, updated_at_utc_ms = excluded.updated_at_utc_ms,
                         provenance_json = excluded.provenance_json, original = excluded.original",
                    params![request.person_id.uuid().to_string(), png, now_utc_ms, provenance, original],
                )?;
            }
            (None, None) => {
                transaction.execute(
                    "DELETE FROM person_avatars WHERE person_id = ?1",
                    [request.person_id.uuid().to_string()],
                )?;
            }
            _ => return Err(StoreError::Invalid),
        }
        transaction.execute(
            "UPDATE persons SET updated_at_utc_ms = ?1 WHERE id = ?2",
            params![now_utc_ms, request.person_id.uuid().to_string()],
        )?;
        bump_people_revision_in(&transaction)?;
        let result = avatar_result_in(&transaction, request.person_id)?;
        transaction.commit()?;
        Ok(result)
    }
}

fn avatar_result_in(
    connection: &Connection,
    person_id: PersonId,
) -> Result<PersonAvatarResult, StoreError> {
    let row = connection
        .query_row(
            "SELECT png, updated_at_utc_ms, provenance_json FROM person_avatars WHERE person_id = ?1",
            [person_id.uuid().to_string()],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?, row.get::<_, Option<String>>(2)?)),
        )
        .optional()?;
    let avatar = row
        .map(|(png, updated_at_utc_ms, provenance)| {
            Ok::<PersonAvatar, StoreError>(PersonAvatar {
                png_base64: STANDARD.encode(png),
                updated_at_utc_ms,
                provenance: provenance
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|_| StoreError::Invalid)?,
            })
        })
        .transpose()?;
    Ok(PersonAvatarResult {
        schema_version: SCHEMA_VERSION,
        revision: people_revision_in(connection)?,
        avatar,
    })
}

fn validated_png(encoded: &str) -> Result<Vec<u8>, StoreError> {
    if encoded.len() > MAX_AVATAR_BYTES.div_ceil(3) * 4 {
        return Err(StoreError::Invalid);
    }
    let bytes = STANDARD.decode(encoded).map_err(|_| StoreError::Invalid)?;
    if bytes.is_empty() || bytes.len() > MAX_AVATAR_BYTES {
        return Err(StoreError::Invalid);
    }
    let reader = image::ImageReader::with_format(Cursor::new(&bytes), ImageFormat::Png);
    let (width, height) = reader.into_dimensions().map_err(|_| StoreError::Invalid)?;
    if width == 0 || width != height || width > MAX_AVATAR_EDGE {
        return Err(StoreError::Invalid);
    }
    let image = image::load_from_memory_with_format(&bytes, ImageFormat::Png)
        .map_err(|_| StoreError::Invalid)?;
    let mut normalized = Cursor::new(Vec::new());
    image
        .write_to(&mut normalized, ImageFormat::Png)
        .map_err(|_| StoreError::Invalid)?;
    let normalized = normalized.into_inner();
    if normalized.len() > MAX_AVATAR_BYTES {
        return Err(StoreError::Invalid);
    }
    Ok(normalized)
}

fn validated_original(encoded: &str) -> Result<Vec<u8>, StoreError> {
    if encoded.len() > MAX_ORIGINAL_BYTES.div_ceil(3) * 4 {
        return Err(StoreError::Invalid);
    }
    let bytes = STANDARD.decode(encoded).map_err(|_| StoreError::Invalid)?;
    if bytes.is_empty() || bytes.len() > MAX_ORIGINAL_BYTES {
        return Err(StoreError::Invalid);
    }
    let format = image::guess_format(&bytes).map_err(|_| StoreError::Invalid)?;
    if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg) {
        return Err(StoreError::Invalid);
    }
    let (width, height) = image::ImageReader::with_format(Cursor::new(&bytes), format)
        .into_dimensions()
        .map_err(|_| StoreError::Invalid)?;
    if width == 0
        || height == 0
        || width > MAX_ORIGINAL_EDGE
        || height > MAX_ORIGINAL_EDGE
        || u64::from(width) * u64::from(height) > MAX_ORIGINAL_PIXELS
    {
        return Err(StoreError::Invalid);
    }
    Ok(bytes)
}

fn validate_provenance(
    provenance: &PersonPhotoProvenance,
    now_utc_ms: i64,
) -> Result<(), StoreError> {
    let label_valid = |text: &str| {
        !text.trim().is_empty() && text.len() <= 512 && !text.chars().any(char::is_control)
    };
    let confirmed_at = match provenance {
        PersonPhotoProvenance::LocalFile {
            file_name,
            confirmed_at_utc_ms,
        } => {
            if !label_valid(file_name) || file_name.contains(['/', '\\']) {
                return Err(StoreError::Invalid);
            }
            *confirmed_at_utc_ms
        }
        PersonPhotoProvenance::LocalContact {
            contact_name,
            confirmed_at_utc_ms,
        } => {
            if !label_valid(contact_name) {
                return Err(StoreError::Invalid);
            }
            *confirmed_at_utc_ms
        }
        PersonPhotoProvenance::PublicImage {
            source_url,
            image_url,
            attribution,
            confirmed_at_utc_ms,
        } => {
            if !label_valid(attribution) {
                return Err(StoreError::Invalid);
            }
            for raw in [source_url, image_url] {
                let url = url::Url::parse(raw).map_err(|_| StoreError::Invalid)?;
                if raw.len() > 4096
                    || url.scheme() != "https"
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                {
                    return Err(StoreError::Invalid);
                }
            }
            *confirmed_at_utc_ms
        }
    };
    if confirmed_at <= 0 || confirmed_at > now_utc_ms.saturating_add(60_000) {
        return Err(StoreError::Invalid);
    }
    Ok(())
}
