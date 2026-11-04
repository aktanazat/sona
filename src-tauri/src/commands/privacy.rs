use crate::cloud_sync::{phone_snapshot_published_at, sync_requested};
use crate::managers::history::HistoryManager;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::privacy_inventory::MeetingHoldings;
use crate::settings::{get_settings, CloudSyncSettings};
use serde::Serialize;
use specta::Type;
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// What Sona holds on this Mac, counted where it is stored. Each part that
/// could not be read says so instead of reading as zero.
#[derive(Clone, Debug, Serialize, Type)]
pub struct PrivacyInventory {
    pub measured_at_utc_ms: i64,
    pub meetings: Holding<MeetingHoldings>,
    pub dictations: Holding<DictationHoldings>,
    pub notes: Holding<NoteHoldings>,
    /// Dictation history and notes. Meeting data needs no flag: the meeting
    /// store refuses to open without its key and cipher.
    pub history_encrypted: bool,
    pub cloud_backup: CloudBackup,
    pub phone_snapshot: Holding<PhoneSnapshotHoldings>,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Holding<T> {
    Available(T),
    Unavailable { reason: HoldingGap },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum HoldingGap {
    /// The data is encrypted and its key is not available right now.
    Locked,
    /// The store could not be opened or read. The meeting store reports a
    /// missing key, a missing folder and a failed open alike, so its failures
    /// all land here.
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct DictationHoldings {
    pub count: u64,
    pub duration_ms: u64,
    /// Kept audio files, measured on disk.
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct NoteHoldings {
    pub count: u64,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct PhoneSnapshotHoldings {
    pub last_published_at_utc_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CloudBackup {
    Off,
    Paused,
    On,
}

#[tauri::command]
#[specta::specta]
pub async fn privacy_inventory(app: AppHandle) -> PrivacyInventory {
    let now = chrono::Utc::now().timestamp_millis();
    let history = Arc::clone(&*app.state::<Arc<HistoryManager>>());
    let meetings = meeting_holdings(&app, now).await;
    let dictations = dictation_holdings(&history).await;
    let notes = blocking(Arc::clone(&history), |history| {
        crate::scratchpad::note_statistics(history)
            .map(|statistics| NoteHoldings {
                count: statistics.notes,
            })
            .map_err(|_| history_gap(history))
    })
    .await;
    let phone_app = app.clone();
    let phone_snapshot =
        tauri::async_runtime::spawn_blocking(move || phone_snapshot_published_at(&phone_app))
            .await
            .ok()
            .and_then(Result::ok)
            .map_or(
                Holding::Unavailable {
                    reason: HoldingGap::Unavailable,
                },
                |last_published_at_utc_ms| {
                    Holding::Available(PhoneSnapshotHoldings {
                        last_published_at_utc_ms,
                    })
                },
            );
    PrivacyInventory {
        measured_at_utc_ms: now,
        meetings,
        dictations,
        notes,
        history_encrypted: history.storage_status().encrypted,
        cloud_backup: cloud_backup(
            &get_settings(&app).cloud_sync,
            crate::portable::is_portable(),
        ),
        phone_snapshot,
    }
}

async fn meeting_holdings(app: &AppHandle, now: i64) -> Holding<MeetingHoldings> {
    let Ok(store) = app.state::<Arc<MeetingSessionManager>>().store().await else {
        return Holding::Unavailable {
            reason: HoldingGap::Unavailable,
        };
    };
    match tauri::async_runtime::spawn_blocking(move || store.holdings(now)).await {
        Ok(Ok(holdings)) => Holding::Available(holdings),
        _ => Holding::Unavailable {
            reason: HoldingGap::Unavailable,
        },
    }
}

async fn dictation_holdings(history: &Arc<HistoryManager>) -> Holding<DictationHoldings> {
    let stats = match history.get_history_stats().await {
        Ok(stats) => stats,
        Err(_) => {
            return Holding::Unavailable {
                reason: history_gap(history),
            }
        }
    };
    let bytes = blocking(Arc::clone(history), |history| {
        directory_bytes(history.recordings_dir()).map_err(|_| HoldingGap::Unavailable)
    })
    .await;
    match bytes {
        Holding::Available(bytes) => Holding::Available(DictationHoldings {
            count: stats.entries,
            duration_ms: stats.total_duration_ms,
            bytes,
        }),
        Holding::Unavailable { reason } => Holding::Unavailable { reason },
    }
}

async fn blocking<T: Send + 'static>(
    history: Arc<HistoryManager>,
    read: impl FnOnce(&HistoryManager) -> Result<T, HoldingGap> + Send + 'static,
) -> Holding<T> {
    match tauri::async_runtime::spawn_blocking(move || read(&history)).await {
        Ok(Ok(value)) => Holding::Available(value),
        Ok(Err(reason)) => Holding::Unavailable { reason },
        Err(_) => Holding::Unavailable {
            reason: HoldingGap::Unavailable,
        },
    }
}

fn history_gap(history: &HistoryManager) -> HoldingGap {
    let status = history.storage_status();
    if status.encrypted && status.reason.is_some() {
        HoldingGap::Locked
    } else {
        HoldingGap::Unavailable
    }
}

fn cloud_backup(settings: &CloudSyncSettings, portable_mode: bool) -> CloudBackup {
    if sync_requested(settings, portable_mode) {
        return CloudBackup::On;
    }
    let resumed = CloudSyncSettings {
        paused: false,
        ..settings.clone()
    };
    if settings.paused && sync_requested(&resumed, portable_mode) {
        CloudBackup::Paused
    } else {
        CloudBackup::Off
    }
}

fn directory_bytes(directory: &Path) -> std::io::Result<u64> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut total = 0_u64;
    for entry in entries {
        let metadata = entry?.metadata()?;
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}
