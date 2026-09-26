//! Pictures of a call's shared screen. Each picture is two encrypted files in
//! the meeting's own folder, the full image and a small copy that also carries
//! the picture's record, so they go to the trash, come back, and are purged
//! with the meeting's audio. The row only says when the picture was taken.

use super::{
    ensure_private_directory, from_i64, id, parse_uuid, session_row, set_private_file_permissions,
    sync_parent_directory, to_i64, MeetingStore, StoreError,
};
use crate::meeting::snapshots::{
    MeetingSnapshotId, MeetingSnapshotSize, MeetingSnapshotSummary, MeetingSnapshotTrigger,
    AUTOMATIC_SNAPSHOT_LIMIT, SNAPSHOT_LIMIT,
};
use crate::meeting::types::MeetingSessionId;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use rusqlite::{params, Connection};
use sha2::Sha256;
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;
use zeroize::Zeroizing;

const SNAPSHOT_DIRECTORY: &str = "snapshots";
const SNAPSHOT_MAGIC: &[u8; 4] = b"SNP1";
const SNAPSHOT_KEY_SALT: &[u8] = b"sona-meeting-snapshot-v1";
const NONCE_BYTES: usize = 12;
const MAXIMUM_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAXIMUM_THUMBNAIL_BYTES: usize = 2 * 1024 * 1024;
const MAXIMUM_RECORD_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy)]
enum Part {
    Image,
    Thumbnail,
}

impl Part {
    fn file_name(self, snapshot: Uuid) -> String {
        match self {
            Self::Image => format!("{snapshot}.snap"),
            Self::Thumbnail => format!("{snapshot}.thumb"),
        }
    }

    /// Bound into the authenticated data, so the small copy can never be
    /// opened as the full image or the other way round.
    fn tag(self) -> u8 {
        match self {
            Self::Image => 1,
            Self::Thumbnail => 2,
        }
    }
}

fn trigger_text(trigger: MeetingSnapshotTrigger) -> &'static str {
    match trigger {
        MeetingSnapshotTrigger::Manual => "manual",
        MeetingSnapshotTrigger::Automatic => "automatic",
    }
}

fn trigger_from_text(value: &str) -> Result<MeetingSnapshotTrigger, StoreError> {
    match value {
        "manual" => Ok(MeetingSnapshotTrigger::Manual),
        "automatic" => Ok(MeetingSnapshotTrigger::Automatic),
        _ => Err(StoreError::Corrupt),
    }
}

fn authenticated_data(part: Part, session: Uuid, snapshot: Uuid) -> [u8; 37] {
    let mut data = [0_u8; 37];
    data[..4].copy_from_slice(SNAPSHOT_MAGIC);
    data[4] = part.tag();
    data[5..21].copy_from_slice(session.as_bytes());
    data[21..].copy_from_slice(snapshot.as_bytes());
    data
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(StoreError::Invalid)?;
    let partial = path.with_file_name(format!("{file_name}.partial"));
    {
        let mut file = File::create(&partial)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    set_private_file_permissions(&partial)?;
    fs::rename(&partial, path)?;
    sync_parent_directory(path)
}

fn remove_if_present(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// The small image, then the picture's record, then the record's length as
/// four little-endian bytes.
fn thumbnail_plaintext(
    thumbnail: &[u8],
    summary: &MeetingSnapshotSummary,
) -> Result<Vec<u8>, StoreError> {
    let record = serde_json::to_vec(summary).map_err(|_| StoreError::Invalid)?;
    let record_length = u32::try_from(record.len()).map_err(|_| StoreError::Invalid)?;
    let mut plaintext = Vec::with_capacity(thumbnail.len() + record.len() + 4);
    plaintext.extend_from_slice(thumbnail);
    plaintext.extend_from_slice(&record);
    plaintext.extend_from_slice(&record_length.to_le_bytes());
    Ok(plaintext)
}

fn split_thumbnail(plaintext: &[u8]) -> Result<(&[u8], MeetingSnapshotSummary), StoreError> {
    let length_start = plaintext.len().checked_sub(4).ok_or(StoreError::Corrupt)?;
    let mut length = [0_u8; 4];
    length.copy_from_slice(&plaintext[length_start..]);
    let record_length = usize::try_from(u32::from_le_bytes(length)).map_err(|_| StoreError::Corrupt)?;
    if record_length > MAXIMUM_RECORD_BYTES {
        return Err(StoreError::Corrupt);
    }
    let record_start = length_start
        .checked_sub(record_length)
        .ok_or(StoreError::Corrupt)?;
    let summary = serde_json::from_slice(&plaintext[record_start..length_start])
        .map_err(|_| StoreError::Corrupt)?;
    Ok((&plaintext[..record_start], summary))
}

pub(super) fn snapshots_for_session(
    connection: &Connection,
    session_id: MeetingSessionId,
) -> Result<Vec<MeetingSnapshotSummary>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT snapshot_id, offset_ns, captured_at_utc_ms, width, height, capture_trigger, app_bundle_id
         FROM meeting_snapshots WHERE session_id = ?1
         ORDER BY offset_ns, captured_at_utc_ms, snapshot_id",
    )?;
    let rows = statement.query_map(params![id(session_id)], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<String>>(6)?,
        ))
    })?;
    let mut snapshots = Vec::new();
    for row in rows {
        let (snapshot_id, offset_ns, captured_at_utc_ms, width, height, trigger, app_bundle_id) =
            row?;
        snapshots.push(MeetingSnapshotSummary {
            snapshot_id: MeetingSnapshotId::from_uuid(parse_uuid(&snapshot_id)?),
            session_id,
            offset_ns: from_i64(offset_ns)?,
            captured_at_utc_ms,
            width: u32::try_from(width).map_err(|_| StoreError::Corrupt)?,
            height: u32::try_from(height).map_err(|_| StoreError::Corrupt)?,
            trigger: trigger_from_text(&trigger)?,
            app_bundle_id,
        });
    }
    Ok(snapshots)
}

fn count_snapshots(
    connection: &Connection,
    session_id: MeetingSessionId,
    trigger: Option<MeetingSnapshotTrigger>,
) -> Result<u32, StoreError> {
    let count: i64 = match trigger {
        None => connection.query_row(
            "SELECT COUNT(*) FROM meeting_snapshots WHERE session_id = ?1",
            params![id(session_id)],
            |row| row.get(0),
        )?,
        Some(trigger) => connection.query_row(
            "SELECT COUNT(*) FROM meeting_snapshots WHERE session_id = ?1 AND capture_trigger = ?2",
            params![id(session_id), trigger_text(trigger)],
            |row| row.get(0),
        )?,
    };
    u32::try_from(count).map_err(|_| StoreError::Corrupt)
}

fn insert_row(
    connection: &Connection,
    summary: &MeetingSnapshotSummary,
    byte_length: u64,
) -> Result<usize, StoreError> {
    Ok(connection.execute(
        "INSERT OR IGNORE INTO meeting_snapshots
         (snapshot_id, session_id, offset_ns, captured_at_utc_ms, width, height, capture_trigger, app_bundle_id, byte_length)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            summary.snapshot_id.uuid().to_string(),
            id(summary.session_id),
            to_i64(summary.offset_ns)?,
            summary.captured_at_utc_ms,
            i64::from(summary.width),
            i64::from(summary.height),
            trigger_text(summary.trigger),
            summary.app_bundle_id,
            to_i64(byte_length)?,
        ],
    )?)
}

impl MeetingStore {
    fn snapshot_key(
        &self,
        session: Uuid,
        snapshot: Uuid,
    ) -> Result<Zeroizing<[u8; 32]>, StoreError> {
        let derivation = Hkdf::<Sha256>::new(Some(SNAPSHOT_KEY_SALT), self.master_key.as_bytes());
        let mut info = [0_u8; 32];
        info[..16].copy_from_slice(session.as_bytes());
        info[16..].copy_from_slice(snapshot.as_bytes());
        let mut key = Zeroizing::new([0_u8; 32]);
        derivation
            .expand(&info, key.as_mut())
            .map_err(|_| StoreError::EncryptionUnavailable)?;
        Ok(key)
    }

    fn seal_snapshot_part(
        &self,
        part: Part,
        session: Uuid,
        snapshot: Uuid,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, StoreError> {
        let key = self.snapshot_key(session, snapshot)?;
        let cipher =
            Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| StoreError::EncryptionUnavailable)?;
        let mut nonce = [0_u8; NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|_| StoreError::Unavailable)?;
        let aad = authenticated_data(part, session, snapshot);
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad: &aad })
            .map_err(|_| StoreError::EncryptionUnavailable)?;
        let mut sealed = Vec::with_capacity(SNAPSHOT_MAGIC.len() + NONCE_BYTES + ciphertext.len());
        sealed.extend_from_slice(SNAPSHOT_MAGIC);
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&ciphertext);
        Ok(sealed)
    }

    fn open_snapshot_part(
        &self,
        part: Part,
        session: Uuid,
        snapshot: Uuid,
        sealed: &[u8],
    ) -> Result<Vec<u8>, StoreError> {
        let body_start = SNAPSHOT_MAGIC.len() + NONCE_BYTES;
        if sealed.len() <= body_start || &sealed[..SNAPSHOT_MAGIC.len()] != SNAPSHOT_MAGIC {
            return Err(StoreError::Corrupt);
        }
        let key = self.snapshot_key(session, snapshot)?;
        let cipher =
            Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| StoreError::EncryptionUnavailable)?;
        let aad = authenticated_data(part, session, snapshot);
        cipher
            .decrypt(
                Nonce::from_slice(&sealed[SNAPSHOT_MAGIC.len()..body_start]),
                Payload { msg: &sealed[body_start..], aad: &aad },
            )
            .map_err(|_| StoreError::Corrupt)
    }

    /// The private picture folder. This helper does not take the connection
    /// lock; inserts call it while holding that lock and the retention fence.
    fn snapshot_directory(&self, session_id: MeetingSessionId) -> Result<PathBuf, StoreError> {
        let directory = self
            .ensure_session_directory(session_id)?
            .join(SNAPSHOT_DIRECTORY);
        ensure_private_directory(&directory)?;
        Ok(directory)
    }

    fn snapshot_exists(
        &self,
        session_id: MeetingSessionId,
        snapshot_id: MeetingSnapshotId,
    ) -> Result<bool, StoreError> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM meeting_snapshots WHERE snapshot_id = ?1 AND session_id = ?2)",
            params![snapshot_id.uuid().to_string(), id(session_id)],
            |row| row.get(0),
        )?)
    }

    /// Keeps one picture. A meeting holds at most `SNAPSHOT_LIMIT` pictures,
    /// and at most `AUTOMATIC_SNAPSHOT_LIMIT` of them taken on their own;
    /// past either the answer is `Conflict` and nothing is written.
    pub fn insert_meeting_snapshot(
        &self,
        summary: &MeetingSnapshotSummary,
        png: &[u8],
        thumbnail: &[u8],
    ) -> Result<(), StoreError> {
        if png.is_empty()
            || png.len() > MAXIMUM_IMAGE_BYTES
            || thumbnail.is_empty()
            || thumbnail.len() > MAXIMUM_THUMBNAIL_BYTES
            || summary.width == 0
            || summary.height == 0
        {
            return Err(StoreError::Invalid);
        }
        let session = parse_uuid(&id(summary.session_id))?;
        let snapshot = summary.snapshot_id.uuid();
        let sealed_image = self.seal_snapshot_part(Part::Image, session, snapshot, png)?;
        let sealed_thumbnail = self.seal_snapshot_part(
            Part::Thumbnail,
            session,
            snapshot,
            &thumbnail_plaintext(thumbnail, summary)?,
        )?;

        let connection = self.connection()?;
        let current = session_row(&connection, summary.session_id)?;
        if current.transcript_purged_at_utc_ms.is_some() {
            return Err(StoreError::TranscriptDeleted);
        }
        if current.phase == crate::meeting::types::MeetingPhase::Deleting {
            return Err(StoreError::NotFound);
        }
        let already_present: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM meeting_snapshots WHERE snapshot_id = ?1)",
            params![snapshot.to_string()],
            |row| row.get(0),
        )?;
        if already_present {
            return Err(StoreError::Conflict);
        }
        let stored_bytes: i64 = connection.query_row(
            "SELECT COALESCE(SUM(byte_length), 0) FROM meeting_snapshots WHERE session_id = ?1",
            params![id(summary.session_id)],
            |row| row.get(0),
        )?;
        let byte_length = u64::try_from(sealed_image.len() + sealed_thumbnail.len())
            .map_err(|_| StoreError::Invalid)?;
        if from_i64(stored_bytes)?.saturating_add(byte_length) > 512 * 1024 * 1024 {
            return Err(StoreError::Conflict);
        }
        if count_snapshots(&connection, summary.session_id, None)? >= SNAPSHOT_LIMIT
            || (summary.trigger == MeetingSnapshotTrigger::Automatic
                && count_snapshots(
                    &connection,
                    summary.session_id,
                    Some(MeetingSnapshotTrigger::Automatic),
                )? >= AUTOMATIC_SNAPSHOT_LIMIT)
        {
            return Err(StoreError::Conflict);
        }
        let directory = self.snapshot_directory(summary.session_id)?;
        let image_path = directory.join(Part::Image.file_name(snapshot));
        let thumbnail_path = directory.join(Part::Thumbnail.file_name(snapshot));
        let written = write_private_file(&image_path, &sealed_image)
            .and_then(|()| write_private_file(&thumbnail_path, &sealed_thumbnail))
            .and_then(|()| match insert_row(&connection, summary, byte_length)? {
                1 => Ok(()),
                _ => Err(StoreError::Conflict),
            });
        if written.is_err() {
            // A picture without its row would come back on the next restore
            // as one the person never saw; take the files away with it.
            let _ = remove_if_present(&image_path);
            let _ = remove_if_present(&thumbnail_path);
        }
        written
    }

    pub fn meeting_snapshots(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<MeetingSnapshotSummary>, StoreError> {
        let connection = self.connection()?;
        snapshots_for_session(&connection, session_id)
    }

    pub fn count_meeting_snapshots(
        &self,
        session_id: MeetingSessionId,
        trigger: Option<MeetingSnapshotTrigger>,
    ) -> Result<u32, StoreError> {
        let connection = self.connection()?;
        count_snapshots(&connection, session_id, trigger)
    }

    /// The picture as PNG bytes, full size or the small copy.
    pub fn meeting_snapshot_png(
        &self,
        session_id: MeetingSessionId,
        snapshot_id: MeetingSnapshotId,
        size: MeetingSnapshotSize,
    ) -> Result<Vec<u8>, StoreError> {
        if !self.snapshot_exists(session_id, snapshot_id)? {
            return Err(StoreError::NotFound);
        }
        let session = parse_uuid(&id(session_id))?;
        let snapshot = snapshot_id.uuid();
        let part = match size {
            MeetingSnapshotSize::Thumbnail => Part::Thumbnail,
            MeetingSnapshotSize::Full => Part::Image,
        };
        let path = self.snapshot_directory(session_id)?.join(part.file_name(snapshot));
        let sealed = fs::read(&path).map_err(|error| match error.kind() {
            ErrorKind::NotFound => StoreError::NotFound,
            _ => StoreError::from(error),
        })?;
        let plaintext = self.open_snapshot_part(part, session, snapshot, &sealed)?;
        match part {
            Part::Image => Ok(plaintext),
            Part::Thumbnail => Ok(split_thumbnail(&plaintext)?.0.to_vec()),
        }
    }

    pub fn delete_meeting_snapshot(
        &self,
        session_id: MeetingSessionId,
        snapshot_id: MeetingSnapshotId,
    ) -> Result<(), StoreError> {
        if !self.snapshot_exists(session_id, snapshot_id)? {
            return Err(StoreError::NotFound);
        }
        let connection = self.connection()?;
        let directory = self.root.join(id(session_id)).join(SNAPSHOT_DIRECTORY);
        let snapshot = snapshot_id.uuid();
        let image_path = directory.join(Part::Image.file_name(snapshot));
        // The restore record goes first. A crash must not bring back a
        // picture whose row was removed but whose thumbnail was left behind.
        remove_if_present(&directory.join(Part::Thumbnail.file_name(snapshot)))?;
        remove_if_present(&image_path)?;
        sync_parent_directory(&image_path)?;
        let changed = connection.execute(
            "DELETE FROM meeting_snapshots WHERE snapshot_id = ?1 AND session_id = ?2",
            params![snapshot.to_string(), id(session_id)],
        )?;
        if changed == 1 { Ok(()) } else { Err(StoreError::NotFound) }
    }

    /// Rebuilds the rows of a meeting brought back from the trash. The cloud
    /// bundle a restore reads carries no pictures, but the pictures' files
    /// came back with the meeting's folder, and each small copy carries its
    /// own record. A file that does not open is left where it is and not
    /// listed: it cannot be shown either way.
    pub(super) fn repair_session_snapshots(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<(), StoreError> {
        let session = parse_uuid(&id(session_id))?;
        let directory = self.snapshot_directory(session_id)?;
        let mut recovered = Vec::new();
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("thumb") {
                continue;
            }
            let Some(snapshot) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| Uuid::parse_str(stem).ok())
            else {
                continue;
            };
            let Ok(sealed) = fs::read(&path) else {
                continue;
            };
            let Ok(plaintext) = self.open_snapshot_part(Part::Thumbnail, session, snapshot, &sealed)
            else {
                continue;
            };
            let Ok((_, summary)) = split_thumbnail(&plaintext) else {
                continue;
            };
            if summary.session_id == session_id
                && summary.snapshot_id.uuid() == snapshot
                && directory.join(Part::Image.file_name(snapshot)).is_file()
            {
                let byte_length = u64::try_from(sealed.len())
                    .map_err(|_| StoreError::Corrupt)?
                    .checked_add(fs::metadata(directory.join(Part::Image.file_name(snapshot)))?.len())
                    .ok_or(StoreError::Corrupt)?;
                recovered.push((summary, byte_length));
            }
        }
        let connection = self.connection()?;
        if session_row(&connection, session_id)?.transcript_purged_at_utc_ms.is_some() {
            return Ok(());
        }
        for (summary, byte_length) in &recovered {
            insert_row(&connection, summary, *byte_length)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::workflow_core_tests::{reviewable_meeting, store};
    use super::*;
    use crate::meeting::types::{DeletionCause, MeetingOperationId};

    // Contract: deleting a meeting leaves none of its pictures behind, neither
    // the rows that list them nor the encrypted files in its live folder.
    #[test]
    fn deleting_a_meeting_deletes_its_snapshots() {
        let (_directory, store) = store();
        let session_id = reviewable_meeting(&store, "Quarterly review", 1_000);
        let summary = MeetingSnapshotSummary {
            snapshot_id: MeetingSnapshotId::new(),
            session_id,
            offset_ns: 65_000_000_000,
            captured_at_utc_ms: 66_000,
            width: 1920,
            height: 1080,
            trigger: MeetingSnapshotTrigger::Automatic,
            app_bundle_id: Some("us.zoom.xos".to_owned()),
        };
        store
            .insert_meeting_snapshot(&summary, b"full picture", b"small picture")
            .unwrap();
        let image_path = store
            .snapshot_directory(session_id)
            .unwrap()
            .join(Part::Image.file_name(summary.snapshot_id.uuid()));
        assert_eq!(store.meeting_snapshots(session_id).unwrap(), vec![summary.clone()]);
        assert_eq!(
            store
                .meeting_snapshot_png(session_id, summary.snapshot_id, MeetingSnapshotSize::Full)
                .unwrap(),
            b"full picture"
        );

        let revision = store.session_snapshot(session_id).unwrap().revision;
        let (_, job_id) = store
            .reserve_deletion(
                MeetingOperationId::new(),
                70_000,
                session_id,
                revision,
                DeletionCause::User,
            )
            .unwrap();
        store.finish_deletion(job_id).unwrap();

        let rows: i64 = store
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM meeting_snapshots WHERE session_id = ?1",
                params![id(session_id)],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
        assert!(!image_path.exists());
    }
}
