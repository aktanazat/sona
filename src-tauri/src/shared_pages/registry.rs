//! What Sona keeps about shares on the Mac: site settings, the list of
//! shares, drafts, and the lock every write holds. The remote tree is the
//! truth for what is live; this is metadata.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use specta::Type;

use super::content::SpeakerMode;
use super::{Code, SharedPagesError};

const LOCK_WAIT: Duration = Duration::from_secs(10);
const LOCK_RETRY: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SiteSettings {
    pub remote: String,
    pub base_url: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ShareState {
    Publishing,
    Live,
    Failed,
    Removing,
    Removed,
    Expired,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ShareRecord {
    pub slug: String,
    pub url: Option<String>,
    pub meeting_id: String,
    pub title: String,
    /// The person the page was for, when it was a one-person share.
    #[serde(default)]
    pub for_person: Option<String>,
    /// The first sentence of the summary the page carried, for copying with
    /// the link. Lives only in this file; the page never carries it back.
    #[serde(default)]
    pub blurb: Option<String>,
    pub include: Vec<String>,
    pub speakers: SpeakerMode,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub state: ShareState,
    pub state_at: String,
    pub bytes: u64,
    pub error: Option<String>,
}

impl ShareRecord {
    /// The row as he sees it at `now`: a live share past its expiry reads as
    /// expired, and a share that is gone carries no link.
    pub(crate) fn seen_at(mut self, now: DateTime<Utc>) -> Self {
        if self.state == ShareState::Live && self.has_expired(now) {
            self.state = ShareState::Expired;
        }
        if matches!(self.state, ShareState::Removed | ShareState::Expired) {
            self.url = None;
        }
        self
    }

    pub(crate) fn has_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at
            .as_deref()
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
            .is_some_and(|at| at <= now)
    }
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct DraftEntry {
    pub meeting_id: String,
    pub title: String,
    pub prepared_by: Option<String>,
    pub saved_at: String,
}

pub(crate) fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `<app data>/shared-pages` and the files in it.
#[derive(Clone, Debug)]
pub(crate) struct Paths {
    root: PathBuf,
}

impl Paths {
    pub(crate) fn new(app_data: &Path) -> Self {
        Self {
            root: app_data.join("shared-pages"),
        }
    }

    fn site_json(&self) -> PathBuf {
        self.root.join("site.json")
    }

    fn shares_json(&self) -> PathBuf {
        self.root.join("shares.json")
    }

    pub(crate) fn clone_dir(&self) -> PathBuf {
        self.root.join("site")
    }

    fn drafts_dir(&self) -> PathBuf {
        self.root.join("drafts")
    }

    /// A draft's folder. Only a meeting id names one, so no draft path can
    /// leave the drafts folder.
    pub(crate) fn draft_dir(&self, meeting_id: &str) -> Result<PathBuf, SharedPagesError> {
        let id = uuid::Uuid::parse_str(meeting_id).map_err(|_| {
            SharedPagesError::new(Code::InvalidDraft, "The meeting id is not a meeting id.")
        })?;
        Ok(self.drafts_dir().join(id.hyphenated().to_string()))
    }

    pub(crate) fn site(&self) -> Result<Option<SiteSettings>, SharedPagesError> {
        read_json(&self.site_json())
    }

    pub(crate) fn require_site(&self) -> Result<SiteSettings, SharedPagesError> {
        self.site()?.ok_or_else(|| {
            SharedPagesError::new(
                Code::SiteNotSetUp,
                "No site is set up for shared pages. Set one with share-site --remote URL --base-url URL.",
            )
        })
    }

    pub(crate) fn set_site(&self, settings: &SiteSettings) -> Result<(), SharedPagesError> {
        write_json(&self.site_json(), settings)
    }

    pub(crate) fn shares(&self) -> Result<Vec<ShareRecord>, SharedPagesError> {
        Ok(read_json(&self.shares_json())?.unwrap_or_default())
    }

    pub(crate) fn save_shares(&self, shares: &[ShareRecord]) -> Result<(), SharedPagesError> {
        write_json(&self.shares_json(), &shares)
    }

    pub(crate) fn draft<T: DeserializeOwned>(
        &self,
        meeting_id: &str,
    ) -> Result<Option<T>, SharedPagesError> {
        read_json(&self.draft_dir(meeting_id)?.join("draft.json"))
    }

    pub(crate) fn save_draft<T: Serialize>(
        &self,
        meeting_id: &str,
        draft: &T,
    ) -> Result<(), SharedPagesError> {
        let dir = self.draft_dir(meeting_id)?;
        fs::create_dir_all(&dir).map_err(io_failed)?;
        write_json(&dir.join("draft.json"), draft)
    }

    pub(crate) fn discard_draft(&self, meeting_id: &str) -> Result<(), SharedPagesError> {
        match fs::remove_dir_all(self.draft_dir(meeting_id)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_failed(error)),
        }
    }

    /// Every saved draft, newest first.
    pub(crate) fn drafts<T: DeserializeOwned>(
        &self,
    ) -> Result<Vec<(T, DateTime<Utc>)>, SharedPagesError> {
        let entries = match fs::read_dir(self.drafts_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_failed(error)),
        };
        let mut drafts = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path().join("draft.json");
            let Ok(saved_at) = fs::metadata(&path).and_then(|meta| meta.modified()) else {
                continue;
            };
            if let Some(draft) = read_json(&path)? {
                drafts.push((draft, DateTime::<Utc>::from(saved_at)));
            }
        }
        drafts.sort_by_key(|draft| std::cmp::Reverse(draft.1));
        Ok(drafts)
    }

    /// Holds `.lock` exclusively; the GUI core and the CLI are separate
    /// processes. Waits briefly, then refuses with `locked`.
    pub(crate) fn lock(&self) -> Result<Lock, SharedPagesError> {
        fs::create_dir_all(&self.root).map_err(io_failed)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.root.join(".lock"))
            .map_err(io_failed)?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Lock { _file: file }),
                Err(TryLockError::WouldBlock) if started.elapsed() < LOCK_WAIT => {
                    std::thread::sleep(LOCK_RETRY);
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(SharedPagesError::new(
                        Code::Locked,
                        "Another share or removal is running. Try again in a moment.",
                    ))
                }
                Err(TryLockError::Error(error)) => return Err(io_failed(error)),
            }
        }
    }
}

/// Released when dropped.
pub(crate) struct Lock {
    _file: File,
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, SharedPagesError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|_| {
            SharedPagesError::new(
                Code::Failed,
                format!("{} could not be read.", path.display()),
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_failed(error)),
    }
}

/// Writes through a private temporary file and a rename, so a reader sees
/// the old file or the new one, never half of one.
fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<(), SharedPagesError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| SharedPagesError::new(Code::Failed, "The record could not be encoded."))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(io_failed)?;
    let temporary = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(io_failed)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(io_failed)?;
    fs::rename(&temporary, path).map_err(io_failed)
}

pub(crate) fn io_failed(error: std::io::Error) -> SharedPagesError {
    SharedPagesError::new(
        Code::Failed,
        format!("A shared pages file could not be written: {error}"),
    )
}
