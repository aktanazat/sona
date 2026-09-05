use crate::settings;
use log::warn;
use reqwest::header::{HeaderValue, ACCEPT, USER_AGENT};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::time::Duration;
use tauri::AppHandle;

const RELEASES_URL: &str = "https://api.github.com/repos/aktanazat/sona/releases/latest";
const RELEASES_URL_ENV: &str = "SONA_UPDATE_CHECK_URL";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const UPDATE_USER_AGENT: &str = concat!("Sona/", env!("CARGO_PKG_VERSION"));
const MAX_NOTES_BYTES: usize = 2000;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum UpdateCheckStatus {
    UpToDate,
    UpdateAvailable,
    CheckFailed,
    /// The user turned update checks off, so nothing was requested.
    Disabled,
}

/// The result of one manual update check. A failed check is reported in
/// `status` and `error` rather than as a command error, so the UI can show the
/// current version either way.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub url: Option<String>,
    pub notes_excerpt: Option<String>,
    pub published_at_utc_ms: Option<i64>,
    pub status: UpdateCheckStatus,
    pub error: Option<String>,
}

impl UpdateCheckResult {
    fn quiet(
        status: UpdateCheckStatus,
        latest_version: Option<String>,
        error: Option<String>,
    ) -> Self {
        Self {
            current_version: current_version().to_string(),
            latest_version,
            update_available: false,
            url: None,
            notes_excerpt: None,
            published_at_utc_ms: None,
            status,
            error,
        }
    }

    fn up_to_date(latest_version: Option<String>) -> Self {
        Self::quiet(UpdateCheckStatus::UpToDate, latest_version, None)
    }

    fn failed(error: String) -> Self {
        Self::quiet(UpdateCheckStatus::CheckFailed, None, Some(error))
    }

    fn disabled() -> Self {
        Self::quiet(UpdateCheckStatus::Disabled, None, None)
    }
}

#[derive(Deserialize, Debug)]
struct GithubRelease {
    tag_name: Option<String>,
    name: Option<String>,
    html_url: Option<String>,
    body: Option<String>,
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Numeric release components, ignoring a leading `v` and any pre-release or
/// build suffix. `None` means the tag is not a version this app can compare.
fn version_parts(version: &str) -> Option<Vec<u64>> {
    let trimmed = version.trim();
    let trimmed = trimmed.strip_prefix(['v', 'V']).unwrap_or(trimmed);
    let core = trimmed
        .split(['-', '+'])
        .next()
        .filter(|core| !core.is_empty())?;
    let parts = core
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<u64>>>()?;
    (!parts.is_empty()).then_some(parts)
}

/// True when `latest` names a strictly newer release than `current`. An
/// unparseable tag is never treated as newer: the user keeps the build they
/// have instead of being sent to a release page that may not apply.
fn is_newer_version(latest: &str, current: &str) -> bool {
    let (Some(latest), Some(current)) = (version_parts(latest), version_parts(current)) else {
        return false;
    };
    let length = latest.len().max(current.len());
    for index in 0..length {
        let latest_part = latest.get(index).copied().unwrap_or(0);
        let current_part = current.get(index).copied().unwrap_or(0);
        if latest_part != current_part {
            return latest_part > current_part;
        }
    }
    false
}

fn notes_excerpt(body: Option<String>) -> Option<String> {
    let body = body?;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= MAX_NOTES_BYTES {
        return Some(trimmed.to_string());
    }
    let cut = (0..=MAX_NOTES_BYTES)
        .rev()
        .find(|index| trimmed.is_char_boundary(*index))
        .unwrap_or(0);
    Some(trimmed[..cut].trim_end().to_string())
}

fn published_at_utc_ms(published_at: Option<String>) -> Option<i64> {
    let published_at = published_at?;
    chrono::DateTime::parse_from_rfc3339(&published_at)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

fn result_for_release(release: GithubRelease) -> UpdateCheckResult {
    let Some(tag) = release
        .tag_name
        .or(release.name)
        .filter(|tag| !tag.trim().is_empty())
    else {
        return UpdateCheckResult::up_to_date(None);
    };
    if release.draft || release.prerelease || !is_newer_version(&tag, current_version()) {
        return UpdateCheckResult::up_to_date(Some(tag));
    }

    UpdateCheckResult {
        current_version: current_version().to_string(),
        latest_version: Some(tag),
        update_available: true,
        url: release.html_url,
        notes_excerpt: notes_excerpt(release.body),
        published_at_utc_ms: published_at_utc_ms(release.published_at),
        status: UpdateCheckStatus::UpdateAvailable,
        error: None,
    }
}

/// Ask GitHub for the latest published release. This never downloads or
/// installs anything, a repository with no releases yet is reported as up to
/// date rather than as a failure, and nothing leaves the device while the user
/// keeps update checks turned off.
#[tauri::command]
#[specta::specta]
pub async fn check_for_updates(app: AppHandle) -> Result<UpdateCheckResult, String> {
    check_for_updates_for_runtime(app).await
}

async fn check_for_updates_for_runtime<R: tauri::Runtime>(
    app: AppHandle<R>,
) -> Result<UpdateCheckResult, String> {
    let _span = crate::launch_trace::update_check_span();
    if !settings::get_settings(&app).update_check_enabled {
        return Ok(UpdateCheckResult::disabled());
    }

    let client = match reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() {
        Ok(client) => client,
        Err(error) => {
            warn!("Update check could not build an HTTP client: {error}");
            return Ok(UpdateCheckResult::failed(
                "Could not start the update check".to_string(),
            ));
        }
    };

    // The override is a launch-measurement hook for the local delayed/offline
    // cohorts; ordinary builds keep the pinned GitHub endpoint.
    let releases_url = std::env::var(RELEASES_URL_ENV).ok();
    let response = client
        .get(releases_url.as_deref().unwrap_or(RELEASES_URL))
        .header(USER_AGENT, HeaderValue::from_static(UPDATE_USER_AGENT))
        .header(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        )
        .send()
        .await;

    let response = match response {
        Ok(response) => response,
        Err(error) => {
            warn!("Update check request failed: {error}");
            return Ok(UpdateCheckResult::failed(
                "Could not reach the update service".to_string(),
            ));
        }
    };

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(UpdateCheckResult::up_to_date(None));
    }
    if !response.status().is_success() {
        let status = response.status().as_u16();
        warn!("Update check returned HTTP {status}");
        return Ok(UpdateCheckResult::failed(format!(
            "The update service returned HTTP {status}"
        )));
    }

    match response.json::<GithubRelease>().await {
        Ok(release) => Ok(result_for_release(release)),
        Err(error) => {
            warn!("Update check response could not be read: {error}");
            Ok(UpdateCheckResult::failed(
                "The update service returned an unreadable response".to_string(),
            ))
        }
    }
}

#[tauri::command]
#[specta::specta]
pub fn change_update_check_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::update_settings(&app, |settings| {
        settings.update_check_enabled = enabled;
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsString, sync::Mutex};
    use tauri_plugin_store::StoreExt;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
        time::{timeout, Instant},
    };

    static UPDATE_CHECK_URL_LOCK: Mutex<()> = Mutex::new(());

    /// Exclusive ownership of the process-wide endpoint override.
    /// `RELEASES_URL_ENV` is one variable for the whole test binary, so a test
    /// that installed the override without the lock would hand its own local
    /// server to a neighbour. Taking the lock in the constructor and setting
    /// the variable only through the guard is what makes that unwritable
    /// rather than merely discouraged.
    struct UpdateCheckEndpoint {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<OsString>,
    }

    impl UpdateCheckEndpoint {
        /// Acquire before binding a server: `serve_response` starts a bounded
        /// accept window, and a server bound while a neighbour still owns the
        /// endpoint can run that window out before this test gets to use it.
        fn acquire() -> Self {
            let lock = UPDATE_CHECK_URL_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            Self {
                _lock: lock,
                previous: std::env::var_os(RELEASES_URL_ENV),
            }
        }

        fn point_at(&self, url: &str) {
            std::env::set_var(RELEASES_URL_ENV, url);
        }
    }

    impl Drop for UpdateCheckEndpoint {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(previous) => std::env::set_var(RELEASES_URL_ENV, previous),
                None => std::env::remove_var(RELEASES_URL_ENV),
            }
        }
    }

    /// One enabled update check against a local server that answers `status`
    /// with `body`, then waits for that server to finish.
    async fn check_against(status: &str, body: &str) -> UpdateCheckResult {
        let endpoint = UpdateCheckEndpoint::acquire();
        let (url, server) = serve_response(status, body).await;
        endpoint.point_at(&url);
        let (_data_dir, app) = update_test_app(true);

        let result = check_for_updates_for_runtime(app.handle().clone())
            .await
            .expect("update check returns a result");
        server.await.expect("update response server completed");
        result
    }

    fn update_test_app(
        update_check_enabled: bool,
    ) -> (tempfile::TempDir, tauri::App<tauri::test::MockRuntime>) {
        let data_dir = tempfile::tempdir().expect("create update test data directory");
        // An absolute identifier keeps Tauri's app-data store under this temporary directory.
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().identifier = data_dir.path().to_string_lossy().into_owned();
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_store::Builder::default().build())
            .build(context)
            .expect("build update test app");
        app.handle()
            .store(settings::SETTINGS_STORE_PATH)
            .expect("open update test settings store")
            .set(
                "settings",
                serde_json::json!({ "update_check_enabled": update_check_enabled }),
            );
        (data_dir, app)
    }

    async fn serve_response(status: &str, body: &str) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind update response server");
        let address = listener.local_addr().expect("read update server address");
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let task = tokio::spawn(async move {
            let (mut stream, _) = timeout(Duration::from_secs(2), listener.accept())
                .await
                .expect("update client did not contact the local server")
                .expect("accept update request");
            let mut request = [0_u8; 2048];
            assert!(
                stream
                    .read(&mut request)
                    .await
                    .expect("read update request")
                    > 0,
                "update request was empty"
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write update response");
        });
        (format!("http://{address}"), task)
    }

    async fn serve_hanging_response() -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind hanging update server");
        let address = listener.local_addr().expect("read hanging server address");
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept update request");
            let mut request = [0_u8; 2048];
            assert!(
                stream
                    .read(&mut request)
                    .await
                    .expect("read update request")
                    > 0,
                "update request was empty"
            );
            let _ = stream.read(&mut [0_u8; 1]).await;
        });
        (format!("http://{address}"), task)
    }

    #[tokio::test]
    async fn disabled_update_check_never_contacts_the_server() {
        let endpoint = UpdateCheckEndpoint::acquire();
        let disabled_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind disabled update server");
        let disabled_url = format!(
            "http://{}",
            disabled_listener
                .local_addr()
                .expect("read disabled server address")
        );
        endpoint.point_at(&disabled_url);
        let (_data_dir, app) = update_test_app(false);

        let result = tokio::select! {
            result = check_for_updates_for_runtime(app.handle().clone()) => {
                result.expect("disabled update check returns a result")
            }
            _ = disabled_listener.accept() => panic!("disabled update check contacted the server"),
        };

        assert_eq!(result.status, UpdateCheckStatus::Disabled);
        assert_eq!(result.current_version, current_version());
        assert!(!result.update_available);
        assert!(result.latest_version.is_none());
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn current_release_is_reported_as_up_to_date() {
        let result = check_against(
            "200 OK",
            &format!(r#"{{"tag_name":"{}"}}"#, current_version()),
        )
        .await;

        assert_eq!(result.status, UpdateCheckStatus::UpToDate);
        assert_eq!(result.latest_version.as_deref(), Some(current_version()));
        assert!(!result.update_available);
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn newer_release_is_reported_as_available() {
        let result = check_against(
            "200 OK",
            r#"{"tag_name":"v99.0.0","html_url":"https://example.test/sona-v99"}"#,
        )
        .await;

        assert_eq!(result.status, UpdateCheckStatus::UpdateAvailable);
        assert_eq!(result.latest_version.as_deref(), Some("v99.0.0"));
        assert!(result.update_available);
        assert_eq!(result.url.as_deref(), Some("https://example.test/sona-v99"));
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn malformed_release_response_fails_the_check() {
        let result = check_against("200 OK", "not json").await;

        assert_eq!(result.status, UpdateCheckStatus::CheckFailed);
        assert_eq!(result.current_version, current_version());
        assert_eq!(
            result.error.as_deref(),
            Some("The update service returned an unreadable response")
        );
        assert!(!result.update_available);
    }

    #[tokio::test]
    async fn draft_release_is_not_an_update() {
        let result = check_against("200 OK", r#"{"tag_name":"v99.0.0","draft":true}"#).await;

        assert_eq!(result.status, UpdateCheckStatus::UpToDate);
        assert_eq!(result.latest_version.as_deref(), Some("v99.0.0"));
        assert!(!result.update_available);
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn prerelease_is_not_an_update() {
        let result = check_against("200 OK", r#"{"tag_name":"v99.0.0","prerelease":true}"#).await;

        assert_eq!(result.status, UpdateCheckStatus::UpToDate);
        assert_eq!(result.latest_version.as_deref(), Some("v99.0.0"));
        assert!(!result.update_available);
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn http_error_fails_the_update_check() {
        let result = check_against("500 Internal Server Error", "{}").await;

        assert_eq!(result.status, UpdateCheckStatus::CheckFailed);
        assert_eq!(
            result.error.as_deref(),
            Some("The update service returned HTTP 500")
        );
        assert!(!result.update_available);
    }

    #[tokio::test]
    async fn hanging_update_response_times_out() {
        let endpoint = UpdateCheckEndpoint::acquire();
        let (_data_dir, app) = update_test_app(true);
        let (url, server) = serve_hanging_response().await;
        endpoint.point_at(&url);
        let started = Instant::now();

        let result = timeout(
            REQUEST_TIMEOUT + Duration::from_secs(2),
            check_for_updates_for_runtime(app.handle().clone()),
        )
        .await
        .expect("update request exceeded its timeout bound")
        .expect("timed-out update check returns a result");

        assert!(
            started.elapsed() >= REQUEST_TIMEOUT,
            "update check failed before its request timeout: {:?}",
            started.elapsed()
        );
        assert_eq!(result.status, UpdateCheckStatus::CheckFailed);
        assert_eq!(
            result.error.as_deref(),
            Some("Could not reach the update service")
        );
        assert!(!result.update_available);
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn newer_releases_are_detected() {
        assert!(is_newer_version("v0.2.0", "0.1.9"));
        assert!(is_newer_version("0.1.10", "0.1.9"));
        assert!(is_newer_version("1.0.0", "0.9.9"));
        assert!(is_newer_version("v1.2", "1.1.9"));
    }

    #[test]
    fn same_or_older_releases_are_not_updates() {
        assert!(!is_newer_version("v0.1.9", "0.1.9"));
        assert!(!is_newer_version("0.1.9", "v0.1.9"));
        assert!(!is_newer_version("0.1.8", "0.1.9"));
        assert!(!is_newer_version("1.0", "1.0.0"));
        assert!(!is_newer_version("1.0.0", "1.0"));
    }

    #[test]
    fn pre_release_and_build_suffixes_compare_by_release_numbers() {
        assert!(is_newer_version("0.2.0-beta.1", "0.1.9"));
        assert!(!is_newer_version("0.1.9-beta.1", "0.1.9"));
        assert!(!is_newer_version("0.1.9+build.7", "0.1.9"));
    }

    #[test]
    fn unparseable_tags_never_claim_an_update() {
        assert!(!is_newer_version("nightly", "0.1.9"));
        assert!(!is_newer_version("", "0.1.9"));
        assert!(!is_newer_version("v", "0.1.9"));
        assert!(!is_newer_version("0.1.x", "0.1.9"));
        assert!(!is_newer_version("0.2.0", "not-a-version"));
    }

    #[test]
    fn drafts_and_pre_releases_are_reported_as_up_to_date() {
        let release = GithubRelease {
            tag_name: Some("v99.0.0".to_string()),
            name: None,
            html_url: Some("https://example.test/release".to_string()),
            body: Some("notes".to_string()),
            published_at: Some("2026-01-02T03:04:05Z".to_string()),
            draft: false,
            prerelease: true,
        };

        let result = result_for_release(release);

        assert_eq!(result.status, UpdateCheckStatus::UpToDate);
        assert!(!result.update_available);
        assert_eq!(result.latest_version.as_deref(), Some("v99.0.0"));
        assert!(result.url.is_none());
    }

    #[test]
    fn a_newer_release_carries_its_url_notes_and_date() {
        let release = GithubRelease {
            tag_name: Some("v99.0.0".to_string()),
            name: None,
            html_url: Some("https://example.test/release".to_string()),
            body: Some("  notes  ".to_string()),
            published_at: Some("2026-01-02T03:04:05Z".to_string()),
            draft: false,
            prerelease: false,
        };

        let result = result_for_release(release);

        assert_eq!(result.status, UpdateCheckStatus::UpdateAvailable);
        assert!(result.update_available);
        assert_eq!(result.current_version, current_version());
        assert_eq!(result.url.as_deref(), Some("https://example.test/release"));
        assert_eq!(result.notes_excerpt.as_deref(), Some("notes"));
        assert_eq!(result.published_at_utc_ms, Some(1_767_323_045_000));
        assert!(result.error.is_none());
    }

    #[test]
    fn long_notes_are_cut_on_a_character_boundary() {
        let body = "é".repeat(MAX_NOTES_BYTES);

        let excerpt = notes_excerpt(Some(body)).expect("notes present");

        assert!(excerpt.len() <= MAX_NOTES_BYTES);
        assert!(excerpt.chars().all(|character| character == 'é'));
    }

    #[test]
    fn a_failed_check_still_reports_the_current_version() {
        let result = UpdateCheckResult::failed("offline".to_string());

        assert_eq!(result.status, UpdateCheckStatus::CheckFailed);
        assert_eq!(result.current_version, current_version());
        assert_eq!(result.error.as_deref(), Some("offline"));
        assert!(!result.update_available);
    }

    #[test]
    fn a_disabled_check_reports_nothing_but_the_current_version() {
        let result = UpdateCheckResult::disabled();

        assert_eq!(result.status, UpdateCheckStatus::Disabled);
        assert_eq!(result.current_version, current_version());
        assert!(result.latest_version.is_none());
        assert!(result.error.is_none());
        assert!(!result.update_available);
    }
}
