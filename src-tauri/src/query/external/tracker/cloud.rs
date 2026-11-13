use super::preferences::{read, ExternalSettings};
use super::{Tracker, WorkAnswer, WriteArgs};
use crate::cloud_sync::{
    meeting_shares_in, meeting_status_in, overview_in, read_share_bundle, CloudRuntimeError,
};
use crate::commands::cloud_sync::service_status;
use crate::meeting::cloud_bundle::CloudMeetingBundleV1;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::{MeetingListFilter, MeetingSessionId};
use crate::settings::CloudSyncSettings;
use crate::query::external::{
    kept_local, meeting_refusal, session_id, store, visible_meeting, ExternalError,
    ExternalErrorCode, ExternalScope, READ_SCOPES, WRITE_SCOPES,
};
use crate::query::{QueryError, QUERY_SCHEMA_VERSION};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

const IMPORT: &str = "meeting-import";
const IMPORT_LEDGER: &str = "cli_meeting_import";
const SYNC_LIST_LIMIT: usize = 100;

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum CloudVerb {
    /// Where cloud sync stands: on or off, waiting uploads, the service, and
    /// each recent meeting's sync state; with --meeting, one meeting's state
    /// and its shares.
    Sync(SyncArgs),
    /// Import a meeting someone shared as a .sona file.
    MeetingImport(MeetingImportArgs),
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SyncArgs {
    /// One meeting's sync state and shares, by its id.
    #[arg(long, value_name = "MEETING_ID")]
    pub meeting: Option<String>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct MeetingImportArgs {
    /// The .sona file.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
    /// Print the meeting the file carries and import nothing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub write: WriteArgs,
}

impl CloudVerb {
    pub(super) const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self {
            Self::Sync(_) | Self::MeetingImport(MeetingImportArgs { dry_run: true, .. }) => {
                READ_SCOPES
            }
            Self::MeetingImport(_) => WRITE_SCOPES,
        }
    }
}

#[derive(Serialize)]
struct SyncedMeeting {
    title: String,
    #[serde(flatten)]
    status: crate::cloud_sync::types::CloudMeetingStatus,
}

#[derive(Serialize)]
struct SharedMeeting {
    meeting_id: String,
    title: String,
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
    audio_included: bool,
    transcript_segments: usize,
    notes: usize,
    artifacts: usize,
}

impl SharedMeeting {
    fn of(bundle: &CloudMeetingBundleV1) -> Self {
        Self {
            meeting_id: bundle.session.session_id.uuid().to_string(),
            title: bundle.session.title.clone(),
            started_at_ms: bundle.session.started_at_utc_ms,
            ended_at_ms: bundle.session.ended_at_utc_ms,
            audio_included: bundle.audio_included,
            transcript_segments: bundle.transcript_segments.len(),
            notes: bundle.notes.len(),
            artifacts: bundle.artifacts.len(),
        }
    }
}

fn sync_failed(error: &CloudRuntimeError) -> ExternalError {
    ExternalError::new(ExternalErrorCode::Failed, format!("Sona could not read it: {error}."))
}

fn unreadable_file() -> ExternalError {
    ExternalError::invalid(
        "That file is not a .sona meeting Sona can open: it needs the .sona extension and must be whole.",
    )
}

async fn sync_overview(
    meetings: &Arc<MeetingSessionManager>,
    settings: &CloudSyncSettings,
    portable_mode: bool,
) -> Result<Value, ExternalError> {
    let store = store(meetings).await?;
    let hidden = kept_local(&store)?;
    let overview =
        overview_in(&store, settings, portable_mode, None).map_err(|error| sync_failed(&error))?;
    let page = meetings
        .list(None, SYNC_LIST_LIMIT, MeetingListFilter::default())
        .await
        .map_err(QueryError::from)?;
    let synced = page
        .entries
        .into_iter()
        .filter(|entry| !hidden.hides(entry.session_id))
        .map(|entry| {
            meeting_status_in(&store, settings.paused, entry.session_id).map(|status| SyncedMeeting {
                title: entry.title,
                status,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sync_failed(&error))?;
    Ok(json!({
        "overview": overview,
        "service": service_status(settings, portable_mode, None),
        "meetings": synced,
    }))
}

async fn sync_meeting(
    meetings: &Arc<MeetingSessionManager>,
    paused: bool,
    meeting: &str,
) -> Result<Value, ExternalError> {
    let session = session_id(meeting)?;
    let store = store(meetings).await?;
    visible_meeting(&store, session)?;
    store
        .session_snapshot(session)
        .map_err(|error| meeting_refusal(session, error))?;
    let status = meeting_status_in(&store, paused, session).map_err(|error| sync_failed(&error))?;
    let shares = meeting_shares_in(&store, session).map_err(|error| sync_failed(&error))?;
    Ok(json!({ "meeting": status, "shares": shares }))
}

fn already_here(store: &MeetingStore, session: MeetingSessionId) -> Result<bool, ExternalError> {
    visible_meeting(store, session)?;
    match store.session_snapshot(session) {
        Ok(_) => Ok(true),
        Err(StoreError::NotFound) => Ok(false),
        Err(error) => Err(QueryError::from(error).into()),
    }
}

impl Tracker<'_> {
    pub(super) async fn cloud(&self, verb: &CloudVerb) -> Result<WorkAnswer, ExternalError> {
        let settings = crate::settings::get_settings(self.app).cloud_sync;
        match verb {
            CloudVerb::Sync(SyncArgs { meeting: None }) => Ok(read(
                "sync",
                &sync_overview(self.meetings, &settings, crate::portable::is_portable()).await?,
            )),
            CloudVerb::Sync(SyncArgs {
                meeting: Some(meeting),
            }) => Ok(read(
                "sync",
                &sync_meeting(self.meetings, settings.paused, meeting).await?,
            )),
            CloudVerb::MeetingImport(args) => self.meeting_import(args).await,
        }
    }

    async fn meeting_import(&self, args: &MeetingImportArgs) -> Result<WorkAnswer, ExternalError> {
        let path = std::path::absolute(&args.file).map_err(|_| unreadable_file())?;
        let request = json!({ "path": path }).to_string();
        let operation_id = args.write.operation_id.filter(|_| !args.dry_run);
        if let Some(operation_id) = operation_id {
            if let Some(earlier) =
                self.settings_replay(&operation_id.to_string(), IMPORT_LEDGER, &request)?
            {
                return Ok(WorkAnswer::Settings(Box::new(earlier)));
            }
        }
        let bundle = read_share_bundle(&path).map_err(|_| unreadable_file())?;
        let shared = SharedMeeting::of(&bundle);
        let session = bundle.session.session_id;
        let store = store(self.meetings).await?;
        let already_here = already_here(&store, session)?;
        if args.dry_run {
            return Ok(WorkAnswer::Settings(Box::new(ExternalSettings {
                schema_version: QUERY_SCHEMA_VERSION,
                what: IMPORT.to_owned(),
                dry_run: true,
                operation_id: args.write.operation_id.map(|id| id.to_string()),
                replayed: false,
                unchanged: false,
                agent: args.write.agent.as_ref().map(|_| args.write.agent()),
                before: None,
                value: json!({ "path": path, "meeting": shared, "already_here": already_here }),
                live: None,
            })));
        }
        if already_here {
            return Err(ExternalError::invalid(format!(
                "Meeting {} is already in Sona, so the file was not imported.",
                session.uuid()
            )));
        }
        self.applies("importing meetings")?;
        let operation = args.write.fresh_operation_id();
        self.meetings
            .import_cloud_bundle(bundle)
            .await
            .map_err(QueryError::from)?;
        let answer = ExternalSettings {
            schema_version: QUERY_SCHEMA_VERSION,
            what: IMPORT.to_owned(),
            dry_run: false,
            operation_id: Some(operation.clone()),
            replayed: false,
            unchanged: false,
            agent: Some(args.write.agent()),
            before: None,
            value: json!({ "path": path, "meeting": shared }),
            live: None,
        };
        self.settings_record(&operation, IMPORT_LEDGER, &request, &answer)?;
        Ok(WorkAnswer::Settings(Box::new(answer)))
    }
}

#[cfg(test)]
mod tests {
    use super::{already_here, sync_meeting, sync_overview};
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::session::{MeetingSessionManager, NoCaptureSources};
    use crate::meeting::store::workflow_core_tests::reviewable_meeting;
    use crate::meeting::types::{MeetingOperationId, MeetingSessionId};
    use crate::query::external::ExternalErrorCode;
    use crate::secrets::{MemorySecretBackend, SecretManager};
    use crate::settings::CloudSyncSettings;
    use std::sync::Arc;
    use tempfile::TempDir;

    const NOW: i64 = 1_767_000_000_000;

    #[test]
    fn a_meeting_kept_on_this_mac_stays_out_of_sync_reads() {
        tauri::async_runtime::block_on(async {
            let directory = TempDir::new().unwrap();
            let secrets = Arc::new(SecretManager::with_backend(Arc::new(
                MemorySecretBackend::new(),
            )));
            let meetings = Arc::new(
                MeetingSessionManager::with_parts(
                    None,
                    Some(directory.path().join("meetings")),
                    secrets,
                    Arc::new(NoCaptureSources),
                )
                .reading_only(),
            );
            let store = meetings.store().await.unwrap();
            let kept = reviewable_meeting(&store, "Board prep", NOW);
            let shared = reviewable_meeting(&store, "Pricing sync", NOW + 1);
            store
                .remember_calendar_facts(
                    kept,
                    &CalendarEventSummary {
                        event_key: format!("board-prep#{NOW}"),
                        series_key: "board-prep".to_owned(),
                        title: "Board prep".to_owned(),
                        attendee_count: 2,
                        start_utc_ms: NOW,
                        end_utc_ms: NOW + 30 * 60 * 1_000,
                        attendees: Vec::new(),
                        notes: None,
                        calendar_name: None,
                        location: None,
                        url: None,
                        place: None,
                    },
                )
                .unwrap();
            store
                .set_series_remote_opt_out(
                    &MeetingSeriesRemoteOptOutSetRequest {
                        operation_id: MeetingOperationId::new(),
                        series_key: "board-prep".to_owned(),
                        remote_intelligence_opt_out: true,
                        expected_revision: 0,
                    },
                    NOW,
                )
                .unwrap();

            let overview = sync_overview(&meetings, &CloudSyncSettings::default(), false)
                .await
                .unwrap();
            let listed: Vec<&str> = overview["meetings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|meeting| meeting["session_id"].as_str().unwrap())
                .collect();
            assert_eq!(listed, [shared.uuid().to_string()]);

            let refused = sync_meeting(&meetings, false, &kept.uuid().to_string())
                .await
                .unwrap_err();
            assert_eq!(refused.error, ExternalErrorCode::NotFound);
            assert!(sync_meeting(&meetings, false, &shared.uuid().to_string())
                .await
                .is_ok());

            let unknown = MeetingSessionId::new();
            let missing = sync_meeting(&meetings, false, &unknown.uuid().to_string())
                .await
                .unwrap_err();
            assert_eq!(missing.error, ExternalErrorCode::NotFound);
            assert_eq!(missing.message, format!("No meeting {} in this corpus.", unknown.uuid()));

            let kept_import = already_here(&store, kept).unwrap_err();
            assert_eq!(kept_import.error, ExternalErrorCode::NotFound);
            assert!(already_here(&store, shared).unwrap());
            assert!(!already_here(&store, unknown).unwrap());
        });
    }
}
