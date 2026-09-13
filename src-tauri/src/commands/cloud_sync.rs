use crate::cloud_sync::types::{
    CloudBrowserShareCreateRequest, CloudBrowserShareResult, CloudConflictResolveRequest,
    CloudMeetingStatus, CloudPairingAcceptRequest, CloudPairingApproveRequest, CloudPairingOffer,
    CloudPairingOfferRequest, CloudShareCreateRequest, CloudShareImportRequest,
    CloudShareImportResult, CloudShareListRequest, CloudShareResult, CloudShareRevokeRequest,
    CloudShareSummary, CloudSyncBootstrapRequest, CloudSyncBootstrapResult, CloudSyncOverview,
    CloudSyncRecoveryRequest,
};
use crate::cloud_sync::{CloudSyncErrorKind, CloudSyncRuntime};
use crate::meeting::types::MeetingSessionId;
use crate::settings::{is_loopback_host, CloudSyncSettings};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, State};

#[tauri::command]
#[specta::specta]
pub async fn cloud_sync_overview_get(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
) -> Result<CloudSyncOverview, CloudSyncErrorKind> {
    runtime.overview().await.map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_sync_meeting_status_get(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
    session_id: MeetingSessionId,
) -> Result<CloudMeetingStatus, CloudSyncErrorKind> {
    runtime
        .meeting_status(session_id)
        .await
        .map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_sync_meeting_status_list(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
) -> Result<Vec<CloudMeetingStatus>, CloudSyncErrorKind> {
    runtime
        .meeting_status_list()
        .await
        .map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_sync_bootstrap(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
    request: CloudSyncBootstrapRequest,
) -> Result<CloudSyncBootstrapResult, CloudSyncErrorKind> {
    runtime
        .bootstrap(request)
        .await
        .map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_sync_recover(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
    request: CloudSyncRecoveryRequest,
) -> Result<CloudSyncOverview, CloudSyncErrorKind> {
    runtime.recover(request).await.map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_sync_pairing_offer(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
    request: CloudPairingOfferRequest,
) -> Result<CloudPairingOffer, CloudSyncErrorKind> {
    runtime
        .pairing_offer(request)
        .await
        .map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_sync_pairing_approve(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
    request: CloudPairingApproveRequest,
) -> Result<CloudSyncOverview, CloudSyncErrorKind> {
    runtime
        .pairing_approve(request)
        .await
        .map_err(|error| error.kind())
}

/// The fingerprint of an offer that arrived from another device, derived on
/// this Mac so the operator compares a value this Mac read out of the offer
/// rather than one the offer told it to show. Reads nothing and changes
/// nothing: the answer is a function of the offer alone.
#[tauri::command]
#[specta::specta]
pub fn cloud_sync_pairing_fingerprint(
    offer: CloudPairingOffer,
) -> Result<String, CloudSyncErrorKind> {
    crate::cloud_sync::pairing_offer_fingerprint(&offer).map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_sync_pairing_accept(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
    request: CloudPairingAcceptRequest,
) -> Result<CloudSyncOverview, CloudSyncErrorKind> {
    runtime
        .pairing_accept(request)
        .await
        .map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_sync_pause(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
) -> Result<CloudSyncOverview, CloudSyncErrorKind> {
    runtime.pause().await.map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_sync_resume(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
) -> Result<CloudSyncOverview, CloudSyncErrorKind> {
    runtime.resume().await.map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_sync_retry(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
    session_id: MeetingSessionId,
) -> Result<CloudMeetingStatus, CloudSyncErrorKind> {
    runtime
        .retry(session_id)
        .await
        .map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_sync_conflict_resolve(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
    request: CloudConflictResolveRequest,
) -> Result<CloudMeetingStatus, CloudSyncErrorKind> {
    runtime
        .conflict_resolve(request)
        .await
        .map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_share_create(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
    request: CloudShareCreateRequest,
) -> Result<CloudShareResult, CloudSyncErrorKind> {
    runtime
        .share_create(request)
        .await
        .map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_browser_share_create(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
    request: CloudBrowserShareCreateRequest,
) -> Result<CloudBrowserShareResult, CloudSyncErrorKind> {
    runtime
        .browser_share_create(request)
        .await
        .map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_share_revoke(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
    request: CloudShareRevokeRequest,
) -> Result<CloudSyncOverview, CloudSyncErrorKind> {
    runtime
        .share_revoke(request)
        .await
        .map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_share_list(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
    request: CloudShareListRequest,
) -> Result<Vec<CloudShareSummary>, CloudSyncErrorKind> {
    runtime
        .share_list(request)
        .await
        .map_err(|error| error.kind())
}

#[tauri::command]
#[specta::specta]
pub async fn cloud_share_import_file(
    runtime: State<'_, Arc<CloudSyncRuntime>>,
    request: CloudShareImportRequest,
) -> Result<CloudShareImportResult, CloudSyncErrorKind> {
    runtime
        .share_import(request)
        .await
        .map_err(|error| error.kind())
}

/// What the privacy page says about cloud sync on this device. Derived from
/// stored settings and the runtime's last access result: reads no network and
/// acquires no credentials.
///
/// `configured` answers provisioning alone, meaning setup finished, the current
/// disclosure accepted, and a remote endpoint stored. `error` is the only
/// failure signal. A provisioned vault that cannot be opened right now stays
/// configured and reports the failure in `error`, so a locked keychain never
/// reads as a device that was never set up, which is the state destructive
/// setup actions key off.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct CloudSyncServiceStatus {
    pub configured: bool,
    pub endpoint: Option<String>,
    pub error: Option<CloudSyncErrorKind>,
    pub reason: String,
}

fn service_status(
    settings: &CloudSyncSettings,
    portable_mode: bool,
    operational_error: Option<CloudSyncErrorKind>,
) -> CloudSyncServiceStatus {
    let unconfigured = |endpoint: Option<String>, reason: &str| CloudSyncServiceStatus {
        configured: false,
        endpoint,
        error: None,
        reason: reason.to_string(),
    };

    if portable_mode {
        return unconfigured(None, "Cloud sync is unavailable in portable mode");
    }
    let endpoint = match settings.endpoint() {
        Ok(Some(endpoint)) => endpoint,
        Ok(None) => return unconfigured(None, "No cloud sync endpoint is configured"),
        Err(error) => return unconfigured(None, &error.to_string()),
    };
    let host_is_local = Url::parse(&endpoint)
        .ok()
        .and_then(|url| url.host_str().map(is_loopback_host))
        .unwrap_or(false);
    if host_is_local {
        return unconfigured(
            Some(endpoint),
            "The configured endpoint is on this machine, so there is no cloud service to sync with",
        );
    }
    if !settings.enabled {
        return unconfigured(
            Some(endpoint),
            "Cloud sync setup has not finished on this device",
        );
    }
    if !settings.has_current_consent() {
        return unconfigured(
            Some(endpoint),
            "Cloud sync is waiting for the current transfer disclosure to be accepted",
        );
    }
    // Provisioning is settled by here. Whether a failure is worth showing is
    // `CloudSyncRuntime::status_error_for`'s call, and it hands over `None`
    // while sync is paused, off, unconsented, portable or mid-capture, so
    // `Some` means a provisioned vault is failing right now. It never takes
    // `configured` back.
    let reason = if operational_error.is_some() {
        "Cloud sync needs attention on this device"
    } else {
        "Cloud sync is set up for this device"
    };
    CloudSyncServiceStatus {
        configured: true,
        endpoint: Some(endpoint),
        error: operational_error,
        reason: reason.to_string(),
    }
}

/// `async` because `status_error_for` takes the meeting actor mutex, which a
/// starting meeting holds across a system-audio device open. On the main
/// thread that would stall the whole UI while Settings > Privacy opens.
#[tauri::command(async)]
#[specta::specta]
pub fn cloud_sync_service_status(
    app: AppHandle,
    runtime: State<'_, Arc<CloudSyncRuntime>>,
) -> CloudSyncServiceStatus {
    let settings = crate::settings::get_settings(&app).cloud_sync;
    let portable_mode = crate::portable::is_portable();
    let error = runtime.status_error_for(&settings, portable_mode);
    service_status(&settings, portable_mode, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::CLOUD_SYNC_CONSENT_VERSION;

    fn bootstrapped(endpoint: &str) -> CloudSyncSettings {
        CloudSyncSettings {
            enabled: true,
            paused: false,
            consent_version: Some(CLOUD_SYNC_CONSENT_VERSION),
            endpoint: Some(endpoint.to_string()),
        }
    }

    #[test]
    fn a_bootstrapped_remote_endpoint_is_configured() {
        let status = service_status(&bootstrapped("https://sync.example.test/v1"), false, None);

        assert!(status.configured);
        assert_eq!(
            status.endpoint.as_deref(),
            Some("https://sync.example.test/v1")
        );
    }

    #[test]
    fn a_store_without_an_endpoint_is_not_configured() {
        let status = service_status(&CloudSyncSettings::default(), false, None);

        assert!(!status.configured);
        assert!(status.endpoint.is_none());
        assert!(status.reason.contains("No cloud sync endpoint"));
    }

    #[test]
    fn a_loopback_endpoint_is_not_a_cloud_service() {
        for endpoint in ["https://localhost/v1", "https://127.0.0.1/v1"] {
            let status = service_status(&bootstrapped(endpoint), false, None);

            assert!(!status.configured, "{endpoint} must not count as cloud");
            assert_eq!(status.endpoint.as_deref(), Some(endpoint));
        }
    }

    #[test]
    fn an_unfinished_bootstrap_is_not_configured() {
        let mut settings = bootstrapped("https://sync.example.test/v1");
        settings.enabled = false;

        let status = service_status(&settings, false, None);

        assert!(!status.configured);
        assert!(status.reason.contains("setup has not finished"));
    }

    #[test]
    fn a_stale_consent_is_not_configured() {
        let mut settings = bootstrapped("https://sync.example.test/v1");
        settings.consent_version = None;

        let status = service_status(&settings, false, None);

        assert!(!status.configured);
        assert!(status.reason.contains("disclosure"));
    }

    #[test]
    fn an_unusable_endpoint_reports_why() {
        let mut settings = bootstrapped("http://sync.example.test/v1");
        settings.enabled = true;

        let status = service_status(&settings, false, None);

        assert!(!status.configured);
        assert!(status.endpoint.is_none());
        assert!(status.reason.contains("HTTPS"));
    }

    #[test]
    fn portable_installs_have_no_cloud_service() {
        let status = service_status(&bootstrapped("https://sync.example.test/v1"), true, None);

        assert!(!status.configured);
        assert!(status.reason.contains("portable"));
    }

    /// `configured` is provisioning, not health: the UI hides destructive setup
    /// actions behind it, so a vault whose keychain is locked right now must not
    /// read as one that was never set up.
    #[test]
    fn a_failing_provisioned_vault_stays_configured_and_names_the_error() {
        for error in [
            CloudSyncErrorKind::SecretUnavailable,
            CloudSyncErrorKind::IntegrityFailure,
        ] {
            let status = service_status(
                &bootstrapped("https://sync.example.test/v1"),
                false,
                Some(error),
            );

            assert!(
                status.configured,
                "{error:?} must not unprovision the vault"
            );
            assert_eq!(status.error, Some(error));
            assert_eq!(
                status.endpoint.as_deref(),
                Some("https://sync.example.test/v1")
            );
        }
    }
}
