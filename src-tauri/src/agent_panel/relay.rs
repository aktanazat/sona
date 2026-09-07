use super::protocol::{
    AgentPanelWorkspaceV1, PanelTurnV1, SonaAgentResponseV1, SonaModelCatalogV1, SonaSubmissionV1,
    MAX_CHAT_SUBMISSION_BYTES, MAX_PROPOSAL_BYTES, SONA_MODEL_ALIAS,
};
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE, RETRY_AFTER};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, Runtime};
use url::Url;

const BRIDGE_VERSION: &str = "bridge-v1";
const HEADER_KEY: &str = "X-Bridge-Key";
const HEADER_TIMESTAMP: &str = "X-Bridge-Ts";
const HEADER_NONCE: &str = "X-Bridge-Nonce";
const HEADER_DIRECTION: &str = "X-Bridge-Dir";
const HEADER_STATUS: &str = "X-Bridge-Status";
const HEADER_REQUEST_NONCE: &str = "X-Bridge-Req-Nonce";
const HEADER_SIGNATURE: &str = "X-Bridge-Sig";
const MAX_SKEW_SECONDS: u64 = 300;
/// Room in a job row for the fields that are neither the submission nor the
/// result: the two identifiers, the state, the workspace, the granted
/// capabilities and the empty tool list.
const JOB_ENVELOPE_BYTES: usize = 8 * 1024;
/// How many times a job row carries the submission. Once under `payload`, and
/// once more because the relay (`RelayDB._row`) hoists every payload key to
/// the top of the row — `request` included. That hoisting is also where this
/// client's `kind`, `workspace_id`, `model_alias`, `capabilities` and `tools`
/// come from, so it cannot go on the relay without `RelayJobWire` changing.
const SUBMISSION_COPIES_IN_A_JOB_ROW: usize = 2;
/// How much longer the relay's JSON can be than this client's for the same
/// value. aiohttp's `json_response` keeps Python's `ensure_ascii`, so a
/// two-byte character here is a six-byte `\uXXXX` on the wire, an astral one
/// twelve, and every separator carries a space. Three bounds any character.
const RELAY_JSON_INFLATION: usize = 3;
/// The largest response body this client will read, in bytes.
///
/// Not a number of its own: the relay answers a submit, a poll and a cancel
/// with the whole job row, and a job row carries the submission back beside
/// the result — twice, escaped. A ceiling that models that row wrongly turns
/// a pack the relay accepted into a reply this client refuses to read, and
/// that shipped: sized for one unescaped copy, this refused the 283 173-byte
/// row that answered a 141 310-byte English pack, and every meeting
/// regenerate failed as `EngineFailure` while the relay's job succeeded.
/// Derived from the row's real shape so the two cannot come apart again.
const MAX_RESPONSE_BYTES: usize = RELAY_JSON_INFLATION
    * (SUBMISSION_COPIES_IN_A_JOB_ROW * MAX_CHAT_SUBMISSION_BYTES + MAX_PROPOSAL_BYTES)
    + JOB_ENVELOPE_BYTES;
const RESPONSE_NONCE_TTL: Duration = Duration::from_secs(MAX_SKEW_SECONDS * 2);
const MAX_RATE_LIMIT_RETRIES: usize = 3;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayError {
    Disabled,
    Unpaired,
    InvalidConfiguration,
    CleartextRejected,
    SecretUnavailable,
    RandomUnavailable,
    RequestFailed,
    RateLimited(Option<Duration>),
    ResponseTooLarge,
    ResponseSignatureInvalid,
    ResponseMalformed,
    RemoteRejected,
    /// The relay would not accept this client at all: an unrecognised bridge
    /// key, or a request signature it refused. Both go out as a 401 with no
    /// signature of the relay's own, which is why the status has to be read
    /// before the signature is checked.
    Unauthorized,
    OwnershipRejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RelayJobStateV1 {
    Queued,
    Leased,
    Running,
    WaitingUser,
    WaitingApproval,
    Succeeded,
    Failed,
    Canceled,
    UnverifiedExternal,
}

impl RelayJobStateV1 {
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Canceled | Self::UnverifiedExternal
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayJobFailure {
    Refused,
    /// The worker refused the answer because the turn declared
    /// `reply_is_json` and the message was not a JSON object.
    ///
    /// A `Refused` with one thing extra: which rule was broken. The panel
    /// treats it as any other refusal, because a reader is told the same
    /// thing either way; the meeting engine names it in the log, because it
    /// is the one refusal whose cause is a shape rather than content.
    ReplyNotStructured,
    Failed,
}

#[derive(Clone, Debug)]
pub(crate) struct RelayJob {
    pub(crate) id: String,
    pub(crate) state: RelayJobStateV1,
    pub(crate) response: Option<SonaAgentResponseV1>,
    pub(crate) failure: Option<RelayJobFailure>,
}

/// The routing facts a job is checked against on the way back in. A reply is
/// only this client's reply if it came from the workspace this client asked,
/// under the capability it asked for: the panel now speaks to two workspaces,
/// so what used to be a pair of constants is carried per job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RelayJobExpectation<'a> {
    pub(crate) workspace: AgentPanelWorkspaceV1,
    pub(crate) model_alias: &'a str,
    pub(crate) job_id: Option<&'a str>,
    pub(crate) idempotency_key: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub(crate) struct RelayEvent {
    pub(crate) id: u64,
    pub(crate) event_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AgentPanelPublicIdentityV1 {
    pub key_id: String,
    pub public_key: String,
}

#[derive(Default)]
pub(crate) struct ResponseNonceCache {
    seen: Mutex<HashMap<String, Instant>>,
}

impl ResponseNonceCache {
    fn check_and_store(&self, nonce: &str) -> bool {
        let now = Instant::now();
        let mut seen = match self.seen.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        seen.retain(|_, stored| now.duration_since(*stored) < RESPONSE_NONCE_TTL);
        if seen.contains_key(nonce) {
            return false;
        }
        seen.insert(nonce.to_string(), now);
        true
    }
}

pub(crate) struct RelayClient {
    base_url: Url,
    client_key_id: String,
    signing_key: SigningKey,
    relay_key_id: String,
    relay_verifying_key: VerifyingKey,
    nonce_cache: Arc<ResponseNonceCache>,
    http: reqwest::Client,
}
struct SignatureContext<'a> {
    method: &'a str,
    path: &'a str,
    body: &'a [u8],
    timestamp: i64,
    nonce: &'a str,
    direction: &'a str,
    status: Option<StatusCode>,
    request_nonce: Option<&'a str>,
}

impl<'a> SignatureContext<'a> {
    fn request(
        method: &'a str,
        path: &'a str,
        body: &'a [u8],
        timestamp: i64,
        nonce: &'a str,
    ) -> Self {
        Self {
            method,
            path,
            body,
            timestamp,
            nonce,
            direction: "request",
            status: None,
            request_nonce: None,
        }
    }

    fn response(
        method: &'a str,
        path: &'a str,
        body: &'a [u8],
        timestamp: i64,
        nonce: &'a str,
        status: StatusCode,
        request_nonce: &'a str,
    ) -> Self {
        Self {
            method,
            path,
            body,
            timestamp,
            nonce,
            direction: "response",
            status: Some(status),
            request_nonce: Some(request_nonce),
        }
    }
}

struct ResponseVerification<'a> {
    method: &'a str,
    path: &'a str,
    body: &'a [u8],
    headers: &'a HeaderMap,
    status: StatusCode,
    request_nonce: &'a str,
}

impl RelayClient {
    pub(crate) async fn from_settings<R: Runtime>(
        app: &AppHandle<R>,
        nonce_cache: Arc<ResponseNonceCache>,
    ) -> Result<Self, RelayError> {
        let settings = crate::settings::get_settings(app);
        if !settings.agent_panel_enabled {
            return Err(RelayError::Disabled);
        }
        if !settings.agent_panel_paired {
            return Err(RelayError::Unpaired);
        }
        let relay_url = settings
            .agent_panel_relay_url
            .as_deref()
            .ok_or(RelayError::InvalidConfiguration)?;
        let base_url = validate_relay_url(relay_url)?;
        let relay_key_id = settings
            .agent_panel_relay_key_id
            .filter(|value| is_key_identifier(value))
            .ok_or(RelayError::InvalidConfiguration)?;
        let pinned_key = settings
            .agent_panel_relay_public_key
            .as_deref()
            .ok_or(RelayError::InvalidConfiguration)?;
        let relay_verifying_key = verifying_key_from_base64(pinned_key)?;
        let secret_manager = app
            .try_state::<Arc<crate::secrets::SecretManager>>()
            .ok_or(RelayError::SecretUnavailable)?;
        let seed = secret_manager
            .agent_panel_signing_seed()
            .await
            .map_err(|_| RelayError::SecretUnavailable)?;
        let signing_key = SigningKey::from_bytes(seed.as_bytes());
        let client_key_id = public_identity_for_key(&signing_key).key_id;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| RelayError::RequestFailed)?;
        Ok(Self {
            base_url,
            client_key_id,
            signing_key,
            relay_key_id,
            relay_verifying_key,
            nonce_cache,
            http,
        })
    }

    pub(crate) async fn submit_turn(
        &self,
        idempotency_key: &str,
        turn: &PanelTurnV1,
        model_alias: &str,
    ) -> Result<RelayJob, RelayError> {
        let workspace = turn.workspace();
        let body = SonaSubmissionV1 {
            workspace_id: workspace.id(),
            model: model_alias,
            capability: workspace.capability(),
            idempotency_key,
            request: turn,
        };
        let response: SubmissionResponse = self
            .request(Method::POST, "/v1/jobs/submit", Some(&body))
            .await?;
        response.job.into_job(
            &self.client_key_id,
            RelayJobExpectation {
                workspace,
                model_alias,
                job_id: None,
                idempotency_key: Some(idempotency_key),
            },
        )
    }

    pub(crate) async fn get_job(
        &self,
        job_id: &str,
        workspace: AgentPanelWorkspaceV1,
        model_alias: &str,
    ) -> Result<RelayJob, RelayError> {
        if !is_job_identifier(job_id) {
            return Err(RelayError::OwnershipRejected);
        }
        let path = format!("/v1/jobs/{job_id}");
        let response: JobResponse = self.request(Method::GET, &path, None::<&()>).await?;
        response.job.into_job(
            &self.client_key_id,
            RelayJobExpectation {
                workspace,
                model_alias,
                job_id: Some(job_id),
                idempotency_key: None,
            },
        )
    }

    /// The smallest signed round-trip the relay offers, for the pairing screen
    /// to prove a relay URL and a pinned key actually reach each other. It
    /// reads one event because reading nothing is not a test: the reply has to
    /// carry a body this client can verify and parse.
    pub(crate) async fn test_connection(&self) -> Result<(), RelayError> {
        let _: EventsResponse = self
            .request(Method::GET, "/v1/events?limit=1", None::<&()>)
            .await?;
        Ok(())
    }

    pub(crate) async fn preferred_model_alias(&self) -> String {
        self.request::<SonaModelCatalogV1, _>(Method::GET, "/v1/models", None::<&()>)
            .await
            .ok()
            .and_then(|catalog| choose_model_alias(&catalog))
            .unwrap_or_else(|| SONA_MODEL_ALIAS.to_string())
    }

    pub(crate) async fn get_events(
        &self,
        job_id: &str,
        after_id: u64,
    ) -> Result<Vec<RelayEvent>, RelayError> {
        if !is_job_identifier(job_id) {
            return Err(RelayError::OwnershipRejected);
        }
        let path = format!("/v1/events?after_id={after_id}&job_id={job_id}&limit=50");
        let response: EventsResponse = self.request(Method::GET, &path, None::<&()>).await?;
        let mut events = Vec::with_capacity(response.events.len());
        let mut last_id = after_id;
        for event in response.events {
            let event = event.into_event(job_id)?;
            if event.id <= last_id {
                return Err(RelayError::ResponseMalformed);
            }
            last_id = event.id;
            events.push(event);
        }
        Ok(events)
    }

    pub(crate) async fn cancel_job(
        &self,
        job_id: &str,
        workspace: AgentPanelWorkspaceV1,
        model_alias: &str,
    ) -> Result<RelayJob, RelayError> {
        if !is_job_identifier(job_id) {
            return Err(RelayError::OwnershipRejected);
        }
        let path = format!("/v1/jobs/{job_id}/cancel");
        let response: CancelResponse = self.request(Method::POST, &path, None::<&()>).await?;
        response.job.into_job(
            &self.client_key_id,
            RelayJobExpectation {
                workspace,
                model_alias,
                job_id: Some(job_id),
                idempotency_key: None,
            },
        )
    }

    async fn request<T, B>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, RelayError>
    where
        T: for<'de> Deserialize<'de>,
        B: Serialize + ?Sized,
    {
        let body_bytes = match body {
            Some(body) => serde_json::to_vec(body).map_err(|_| RelayError::RequestFailed)?,
            None => Vec::new(),
        };
        let request_nonce = new_nonce()?;
        let timestamp = chrono::Utc::now().timestamp();
        let signature_context = SignatureContext::request(
            method.as_str(),
            path,
            &body_bytes,
            timestamp,
            &request_nonce,
        );
        let signatures = sign_headers(&self.signing_key, &self.client_key_id, &signature_context)?;
        let url = self
            .base_url
            .join(path)
            .map_err(|_| RelayError::InvalidConfiguration)?;
        let mut request = self.http.request(method.clone(), url).body(body_bytes);
        if body.is_some() {
            request = request.header(CONTENT_TYPE, "application/json");
        }
        for (name, value) in signatures {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|_| RelayError::RequestFailed)?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > u64::try_from(MAX_RESPONSE_BYTES).unwrap_or(u64::MAX))
        {
            return Err(RelayError::ResponseTooLarge);
        }
        let headers = response.headers().clone();
        let response_bytes = read_limited_response(response).await?;
        /* The status first, because the relay signs only what it accepted.
         * `signed_v1_middleware` signs a response when the request that asked
         * for it verified — `if verified and isinstance(response,
         * web.Response)`, `relay/app.py:172` — plus the single oversized-body
         * refusal it signs before verifying, `relay/app.py:132-146`. Its two
         * rejections of the envelope itself therefore go out bare, both 401:
         * an unrecognised bridge key at `relay/app.py:127-128`, and a request
         * signature it would not take at `relay/app.py:168-169`. So does a
         * handler that panicked, and so does whatever answers when the relay
         * is not running at all.
         *
         * Verifying first reported every one of those as
         * `ResponseSignatureInvalid`, which named this client's own trust
         * check as the fault when the fact on the wire was a 401 or a 503,
         * and left the panel saying the reply was not signed by the paired
         * server for what was an outage or a pairing the relay had forgotten.
         *
         * Nothing is read out of a response that is not an answer — the
         * status is the whole diagnosis and the body is dropped — so an
         * unsigned one has nothing to lie its way into. The 2xx this client
         * does parse is still verified first. */
        if let Some(failure) = failure_for_status(status, &headers) {
            return Err(failure);
        }
        let response_verification = ResponseVerification {
            method: method.as_str(),
            path,
            body: &response_bytes,
            headers: &headers,
            status,
            request_nonce: &request_nonce,
        };
        verify_response(
            &self.relay_verifying_key,
            &self.relay_key_id,
            &self.nonce_cache,
            &response_verification,
        )?;
        serde_json::from_slice(&response_bytes).map_err(|_| RelayError::ResponseMalformed)
    }
}
fn choose_model_alias(catalog: &SonaModelCatalogV1) -> Option<String> {
    let first = catalog.models.first()?;
    let selected = catalog
        .models
        .iter()
        .find(|model| model.default)
        .or_else(|| {
            catalog
                .models
                .iter()
                .find(|model| model.alias == SONA_MODEL_ALIAS)
        })
        .unwrap_or(first);
    (!selected.alias.is_empty()).then(|| selected.alias.clone())
}

pub(crate) const fn rate_limit_delay(
    attempt: usize,
    retry_after: Option<Duration>,
) -> Option<Duration> {
    if attempt >= MAX_RATE_LIMIT_RETRIES {
        return None;
    }
    if let Some(retry_after) = retry_after {
        if retry_after.as_secs() > MAX_RETRY_AFTER.as_secs() {
            return None;
        }
        let backoff = Duration::from_secs(1 << attempt);
        return Some(if retry_after.as_secs() > backoff.as_secs() {
            retry_after
        } else {
            backoff
        });
    }
    Some(Duration::from_secs(1 << attempt))
}

pub(crate) async fn retry_rate_limited<
    T,
    Operation,
    OperationFuture,
    Delay,
    DelayFuture,
    Notify,
    ShouldRetry,
>(
    mut operation: Operation,
    mut delay: Delay,
    mut notify: Notify,
    mut should_retry: ShouldRetry,
) -> Result<T, RelayError>
where
    Operation: FnMut() -> OperationFuture,
    OperationFuture: Future<Output = Result<T, RelayError>>,
    Delay: FnMut(Duration) -> DelayFuture,
    DelayFuture: Future<Output = ()>,
    Notify: FnMut(Duration),
    ShouldRetry: FnMut() -> bool,
{
    let mut attempt = 0;
    loop {
        match operation().await {
            Err(RelayError::RateLimited(retry_after)) => {
                let Some(wait) = rate_limit_delay(attempt, retry_after) else {
                    return Err(RelayError::RateLimited(retry_after));
                };
                if !should_retry() {
                    return Err(RelayError::RateLimited(retry_after));
                }
                notify(wait);
                delay(wait).await;
                if !should_retry() {
                    return Err(RelayError::RateLimited(retry_after));
                }
                attempt += 1;
            }
            result => return result,
        }
    }
}
pub(crate) async fn public_identity(
    enabled: bool,
    secrets: &crate::secrets::SecretManager,
) -> Result<AgentPanelPublicIdentityV1, RelayError> {
    if !enabled {
        return Err(RelayError::Disabled);
    }
    let seed = secrets
        .agent_panel_signing_seed()
        .await
        .map_err(|_| RelayError::SecretUnavailable)?;
    Ok(public_identity_for_key(&SigningKey::from_bytes(
        seed.as_bytes(),
    )))
}

pub(crate) fn new_idempotency_key() -> Result<String, RelayError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| RelayError::RandomUnavailable)?;
    Ok(hex::encode(bytes))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmissionResponse {
    job: RelayJobWire,
    #[serde(rename = "created")]
    _created: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobResponse {
    job: RelayJobWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelResponse {
    job: RelayJobWire,
    #[serde(rename = "control")]
    _control: serde_json::Value,
    #[serde(rename = "created")]
    _created: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsResponse {
    events: Vec<RelayEventWire>,
}

#[derive(Deserialize)]
struct RelayJobWire {
    id: String,
    state: String,
    kind: String,
    workspace_id: String,
    model_alias: String,
    capabilities: Vec<String>,
    tools: Vec<serde_json::Value>,
    submitter_key_id: String,
    external_ref: String,
    #[serde(default)]
    result: Option<serde_json::Value>,
}

impl RelayJobWire {
    fn into_job(
        self,
        expected_submitter_key_id: &str,
        expected: RelayJobExpectation<'_>,
    ) -> Result<RelayJob, RelayError> {
        if !is_job_identifier(&self.id) {
            return Err(RelayError::ResponseMalformed);
        }
        if expected.job_id.is_some_and(|expected| expected != self.id)
            || self.submitter_key_id != expected_submitter_key_id
        {
            return Err(RelayError::OwnershipRejected);
        }
        let capability = expected.workspace.capability();
        if expected
            .idempotency_key
            .is_some_and(|expected| expected != self.external_ref)
            || self.kind != capability
            || self.workspace_id != expected.workspace.id()
            || self.model_alias != expected.model_alias
            || self.capabilities.len() != 1
            || self
                .capabilities
                .first()
                .is_none_or(|granted| granted != capability)
            || !self.tools.is_empty()
        {
            return Err(RelayError::ResponseMalformed);
        }
        let state = parse_state(&self.state)?;
        let result = self.result;
        let failure = match state {
            RelayJobStateV1::Failed => Some(failed_job_reason(result.as_ref())),
            RelayJobStateV1::UnverifiedExternal => Some(RelayJobFailure::Failed),
            _ => None,
        };
        if let Some(failure) = failure {
            /* The only place all three facts are in hand at once. Two of
             * these refusals are one word by the time a reader sees them -
             * deliberately, because a reader is told the same thing either
             * way - so without this line the log cannot tell a model that
             * declined from a model that answered in the wrong shape. The
             * code is printed as the relay sent it, including one this build
             * has no name for. */
            log::warn!(
                "Relay job {} came back {:?} as {:?}, relay error_code {}",
                self.id,
                state,
                failure,
                failed_job_error_code(result.as_ref()).unwrap_or("(none sent)")
            );
        }
        let response = if state == RelayJobStateV1::Succeeded {
            let result = result.ok_or(RelayError::ResponseMalformed)?;
            let serialized =
                serde_json::to_vec(&result).map_err(|_| RelayError::ResponseMalformed)?;
            if serialized.len() > MAX_PROPOSAL_BYTES {
                return Err(RelayError::ResponseTooLarge);
            }
            Some(serde_json::from_value(result).map_err(|_| RelayError::ResponseMalformed)?)
        } else {
            None
        };
        Ok(RelayJob {
            id: self.id,
            state,
            response,
            failure,
        })
    }
}

/// Which failure a `FAILED` job carries, from the code the worker set.
///
/// Only codes a caller can act on are named. `sona_reply_not_structured` is
/// set by `rejection_result` in `omp_bridge/worker/vps_sona.py` and is the one
/// refusal that used to arrive as a success: the relay recorded `SUCCEEDED`,
/// the message held prose, and the parse failed here with nothing written down
/// anywhere about why. Everything else stays the blanket refusal it was.
fn failed_job_reason(result: Option<&serde_json::Value>) -> RelayJobFailure {
    match failed_job_error_code(result) {
        Some("sona_reply_not_structured") => RelayJobFailure::ReplyNotStructured,
        Some("sona_response_rejected") => RelayJobFailure::Refused,
        _ => RelayJobFailure::Failed,
    }
}

/// The relay's own word for why, verbatim, if it sent one.
///
/// Read twice on a failing job: once to type it, once to log it. A code this
/// build has no meaning for still belongs in the log, because it is the only
/// thing on either side naming the cause and the mapping above turns three of
/// them into the same word.
fn failed_job_error_code(result: Option<&serde_json::Value>) -> Option<&str> {
    result
        .and_then(|value| value.get("error_code"))
        .and_then(serde_json::Value::as_str)
}

#[derive(Deserialize)]
struct RelayEventWire {
    id: u64,
    job_id: String,
    event_type: String,
}

impl RelayEventWire {
    fn into_event(self, expected_job_id: &str) -> Result<RelayEvent, RelayError> {
        if !is_job_identifier(&self.job_id) || self.job_id != expected_job_id {
            return Err(RelayError::OwnershipRejected);
        }
        if !is_event_type(&self.event_type) {
            return Err(RelayError::ResponseMalformed);
        }
        Ok(RelayEvent {
            id: self.id,
            event_type: self.event_type,
        })
    }
}

fn parse_state(state: &str) -> Result<RelayJobStateV1, RelayError> {
    match state {
        "QUEUED" => Ok(RelayJobStateV1::Queued),
        "LEASED" => Ok(RelayJobStateV1::Leased),
        "RUNNING" => Ok(RelayJobStateV1::Running),
        "WAITING_USER" => Ok(RelayJobStateV1::WaitingUser),
        "WAITING_APPROVAL" => Ok(RelayJobStateV1::WaitingApproval),
        "SUCCEEDED" => Ok(RelayJobStateV1::Succeeded),
        "FAILED" => Ok(RelayJobStateV1::Failed),
        "CANCELED" => Ok(RelayJobStateV1::Canceled),
        "UNVERIFIED_EXTERNAL" => Ok(RelayJobStateV1::UnverifiedExternal),
        _ => Err(RelayError::ResponseMalformed),
    }
}

async fn read_limited_response(response: reqwest::Response) -> Result<Vec<u8>, RelayError> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| RelayError::RequestFailed)?;
        let next = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or(RelayError::ResponseTooLarge)?;
        if next > MAX_RESPONSE_BYTES {
            return Err(RelayError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn validate_relay_url(value: &str) -> Result<Url, RelayError> {
    let mut url = Url::parse(value).map_err(|_| RelayError::InvalidConfiguration)?;
    if url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(RelayError::InvalidConfiguration);
    }
    if !matches!(url.scheme(), "https" | "http") {
        return Err(RelayError::InvalidConfiguration);
    }
    if !crate::net_policy::is_private_relay_host(url.host_str()) {
        return Err(RelayError::CleartextRejected);
    }
    if url.path().is_empty() {
        url.set_path("/");
    }
    if url.path() != "/" {
        return Err(RelayError::InvalidConfiguration);
    }
    Ok(url)
}

fn verifying_key_from_base64(value: &str) -> Result<VerifyingKey, RelayError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| RelayError::InvalidConfiguration)?;
    let key_bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| RelayError::InvalidConfiguration)?;
    VerifyingKey::from_bytes(&key_bytes).map_err(|_| RelayError::InvalidConfiguration)
}

/// A pairing the client would accept. Normalising here rather than at the
/// command means the rules a request is checked against are the same rules
/// `from_settings` will apply on the next turn: a pairing that saves is a
/// pairing that connects, or the difference is a fault on the wire and not a
/// second opinion in the settings layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ValidatedPairingV1 {
    pub(crate) relay_url: String,
    pub(crate) relay_key_id: String,
    pub(crate) relay_public_key: String,
}

pub(crate) fn validate_pairing(
    relay_url: &str,
    relay_key_id: &str,
    relay_public_key: &str,
) -> Result<ValidatedPairingV1, RelayError> {
    let url = validate_relay_url(relay_url.trim())?;
    let relay_key_id = relay_key_id.trim();
    if !is_key_identifier(relay_key_id) {
        return Err(RelayError::InvalidConfiguration);
    }
    let relay_public_key = relay_public_key.trim();
    let verifying_key = verifying_key_from_base64(relay_public_key)?;
    Ok(ValidatedPairingV1 {
        relay_url: url.to_string(),
        relay_key_id: relay_key_id.to_string(),
        /* Re-encoded from the parsed key, so the stored form is the canonical
         * 32-byte encoding rather than whatever padding the paste carried. */
        relay_public_key: base64::engine::general_purpose::STANDARD
            .encode(verifying_key.to_bytes()),
    })
}

fn public_identity_for_key(signing_key: &SigningKey) -> AgentPanelPublicIdentityV1 {
    let public_bytes = signing_key.verifying_key().to_bytes();
    let mut digest = Sha256::new();
    digest.update(public_bytes);
    let digest = digest.finalize();
    let key_id = format!("sona-{}", hex::encode(&digest[..12]));
    AgentPanelPublicIdentityV1 {
        key_id,
        public_key: base64::engine::general_purpose::STANDARD.encode(public_bytes),
    }
}

fn new_nonce() -> Result<String, RelayError> {
    let mut bytes = [0_u8; 24];
    getrandom::fill(&mut bytes).map_err(|_| RelayError::RandomUnavailable)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn sign_headers(
    signing_key: &SigningKey,
    key_id: &str,
    context: &SignatureContext<'_>,
) -> Result<Vec<(HeaderName, HeaderValue)>, RelayError> {
    let canonical = canonical_bytes(context)?;
    let signature = signing_key.sign(&canonical);
    let mut headers = vec![
        header(HEADER_KEY, key_id)?,
        header(HEADER_TIMESTAMP, &context.timestamp.to_string())?,
        header(HEADER_NONCE, context.nonce)?,
        header(HEADER_DIRECTION, context.direction)?,
        header(
            HEADER_SIGNATURE,
            &base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
        )?,
    ];
    if let Some(status) = context.status {
        let request_nonce = context
            .request_nonce
            .ok_or(RelayError::InvalidConfiguration)?;
        headers.push(header(HEADER_STATUS, &status.as_u16().to_string())?);
        headers.push(header(HEADER_REQUEST_NONCE, request_nonce)?);
    }
    Ok(headers)
}

/// The typed cause a response that is not an answer carries, or `None` for the
/// 2xx this client goes on to parse.
///
/// A 401 asks for pairing rather than a retry. 502, 503 and 504 are the outage
/// `RequestFailed` already represents. A 429 carries its optional delay so
/// the caller can retry it within one shared budget. Other relay refusals stay
/// terminal.
fn failure_for_status(status: StatusCode, headers: &HeaderMap) -> Option<RelayError> {
    if status.is_success() {
        return None;
    }
    Some(match status.as_u16() {
        401 => RelayError::Unauthorized,
        403 | 404 => RelayError::OwnershipRejected,
        429 => RelayError::RateLimited(parse_retry_after(headers)),
        502..=504 => RelayError::RequestFailed,
        _ if status.is_client_error() || status.is_server_error() => RelayError::RemoteRejected,
        /* Neither an answer nor a refusal: a redirect this client does not
         * follow, or a 1xx. Nothing the relay sends, and nothing to name it
         * with beyond "not the shape a reply has". */
        _ => RelayError::ResponseMalformed,
    })
}

fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    const HTTP_DATE_FORMATS: [&str; 3] = [
        "%a, %d %b %Y %H:%M:%S GMT",
        "%A, %d-%b-%y %H:%M:%S GMT",
        "%a %b %e %H:%M:%S %Y",
    ];
    let retry_at = HTTP_DATE_FORMATS
        .iter()
        .find_map(|format| chrono::NaiveDateTime::parse_from_str(value, format).ok())?;
    let seconds = retry_at
        .and_utc()
        .timestamp()
        .saturating_sub(chrono::Utc::now().timestamp());
    Some(Duration::from_secs(u64::try_from(seconds).unwrap_or(0)))
}

fn verify_response(
    verifying_key: &VerifyingKey,
    expected_key_id: &str,
    nonce_cache: &ResponseNonceCache,
    response: &ResponseVerification<'_>,
) -> Result<(), RelayError> {
    let key_id = required_header(response.headers, HEADER_KEY)?;
    if key_id != expected_key_id {
        return Err(RelayError::ResponseSignatureInvalid);
    }
    let timestamp = required_header(response.headers, HEADER_TIMESTAMP)?
        .parse::<i64>()
        .map_err(|_| RelayError::ResponseSignatureInvalid)?;
    let now = chrono::Utc::now().timestamp();
    if now.saturating_sub(timestamp).unsigned_abs() > MAX_SKEW_SECONDS {
        return Err(RelayError::ResponseSignatureInvalid);
    }
    let nonce = required_header(response.headers, HEADER_NONCE)?;
    if nonce.is_empty()
        || nonce.len() > 128
        || !nonce.bytes().all(|byte| (33..=126).contains(&byte))
    {
        return Err(RelayError::ResponseSignatureInvalid);
    }
    if required_header(response.headers, HEADER_DIRECTION)? != "response"
        || required_header(response.headers, HEADER_STATUS)? != response.status.as_u16().to_string()
        || required_header(response.headers, HEADER_REQUEST_NONCE)? != response.request_nonce
    {
        return Err(RelayError::ResponseSignatureInvalid);
    }
    let signature = base64::engine::general_purpose::STANDARD
        .decode(required_header(response.headers, HEADER_SIGNATURE)?)
        .map_err(|_| RelayError::ResponseSignatureInvalid)?;
    let signature =
        Signature::from_slice(&signature).map_err(|_| RelayError::ResponseSignatureInvalid)?;
    let context = SignatureContext::response(
        response.method,
        response.path,
        response.body,
        timestamp,
        nonce,
        response.status,
        response.request_nonce,
    );
    verifying_key
        .verify_strict(&canonical_bytes(&context)?, &signature)
        .map_err(|_| RelayError::ResponseSignatureInvalid)?;
    if !nonce_cache.check_and_store(nonce) {
        return Err(RelayError::ResponseSignatureInvalid);
    }
    Ok(())
}

fn required_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, RelayError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .ok_or(RelayError::ResponseSignatureInvalid)
}

fn header(name: &str, value: &str) -> Result<(HeaderName, HeaderValue), RelayError> {
    let name =
        HeaderName::from_bytes(name.as_bytes()).map_err(|_| RelayError::InvalidConfiguration)?;
    let value = HeaderValue::from_str(value).map_err(|_| RelayError::InvalidConfiguration)?;
    Ok((name, value))
}

fn canonical_bytes(context: &SignatureContext<'_>) -> Result<Vec<u8>, RelayError> {
    let path = canonical_path_query(context.path)?;
    Ok(format!(
        "{BRIDGE_VERSION}\n{}\n{}\n{path}\n{}\n{}\n{}\n{}\n{}",
        context.direction,
        context.method.to_ascii_uppercase(),
        context
            .status
            .map(|status| status.as_u16())
            .map(|status| status.to_string())
            .unwrap_or_default(),
        context.request_nonce.unwrap_or_default(),
        context.timestamp,
        context.nonce,
        body_sha256(context.body),
    )
    .into_bytes())
}

fn canonical_path_query(path_query: &str) -> Result<String, RelayError> {
    let without_fragment = path_query.split('#').next().unwrap_or(path_query);
    let (path, raw_query) = without_fragment
        .split_once('?')
        .map_or((without_fragment, ""), |(path, query)| (path, query));
    let path = if path.is_empty() { "/" } else { path };
    let mut pairs = url::form_urlencoded::parse(raw_query.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    pairs.sort();
    let mut canonical = percent_encode_path(path);
    if !pairs.is_empty() {
        let query = pairs
            .iter()
            .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(value)))
            .collect::<Vec<_>>()
            .join("&");
        canonical.push('?');
        canonical.push_str(&query);
    }
    Ok(canonical)
}

fn percent_encode_path(path: &str) -> String {
    let mut output = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'/' | b'%' | b':' | b'@' | b'-' | b'_' | b'.' | b'~')
        {
            output.push(char::from(byte));
        } else {
            append_percent_encoded(&mut output, byte);
        }
    }
    output
}

fn form_encode(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(char::from(byte));
        } else if byte == b' ' {
            output.push('+');
        } else {
            append_percent_encoded(&mut output, byte);
        }
    }
    output
}

fn append_percent_encoded(output: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    output.push('%');
    output.push(char::from(HEX[usize::from(byte >> 4)]));
    output.push(char::from(HEX[usize::from(byte & 0x0f)]));
}

fn body_sha256(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

fn is_key_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn is_job_identifier(value: &str) -> bool {
    is_key_identifier(value)
}

fn is_event_type(value: &str) -> bool {
    is_key_identifier(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_panel::protocol::{
        AgentPanelTurnFailureV1, AgentPanelWorkspaceV1, DeviceNames, PanelTurnV1,
        SonaAgentChatOutcomeV1, SonaAgentChatRoleV1, SonaAgentChatTurnV1, SonaAgentResponseV1,
        SonaAgentTurnV1, SonaAllowedValuesV1, SonaChatActionV1, SonaChatTurnV2,
        SonaConfigProposalV1, SonaModelCatalogEntryV1, SonaSettingChangeV1,
        SONA_AGENT_TURN_VERSION, SONA_CHAT_TURN_VERSION, SONA_CONFIG_PROPOSAL_VERSION,
    };
    use crate::agent_panel::{
        accept_job_in_state, config, ActiveTurn, AgentPanelActionStateV1, AgentPanelManager,
        AgentPanelProposalStateV1, AgentPanelRelayStatusV1, AgentPanelTurnStateV1, PanelState,
        Reversal, StoredActionState,
    };
    use crate::meeting::loop_types::{MeetingLoopRow, MeetingLoopStatus};
    use crate::meeting::session::{MeetingSessionManager, NoCaptureSources};
    use crate::meeting::store::{workflow_core_tests, MeetingStore};
    use crate::meeting::types::{
        MeetingCommandKind, MeetingOperationId, MeetingSessionId, OperationResult,
    };
    use crate::secrets::{MemorySecretBackend, SecretManager};
    use crate::settings::Theme;
    use std::{
        collections::BTreeMap,
        net::SocketAddr,
        sync::{
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
            Arc,
        },
    };
    use tauri_plugin_store::StoreExt;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };
    use uuid::Uuid;
    fn paired_panel_app(
        endpoint: &str,
        relay_key: &SigningKey,
    ) -> (tempfile::TempDir, tauri::App<tauri::test::MockRuntime>) {
        let data_dir = tempfile::tempdir().expect("temporary app data");
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().identifier = data_dir.path().to_string_lossy().into_owned();
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_store::Builder::default().build())
            .build(context)
            .expect("test app with a store plugin");
        let store = app
            .handle()
            .store(crate::portable::store_path(
                crate::settings::SETTINGS_STORE_PATH,
            ))
            .expect("settings store");
        let mut settings = crate::settings::get_default_settings();
        settings.agent_panel_enabled = true;
        settings.agent_panel_paired = true;
        settings.agent_panel_relay_url = Some(endpoint.to_string());
        settings.agent_panel_relay_key_id = Some("relay-test".to_string());
        settings.agent_panel_relay_public_key = Some(
            base64::engine::general_purpose::STANDARD.encode(relay_key.verifying_key().to_bytes()),
        );
        store.set(
            "settings",
            serde_json::to_value(&settings).expect("serialize paired settings"),
        );
        (data_dir, app)
    }

    fn active_chat_state_for_manager(turn_id: &str, message: &str) -> PanelState {
        let conversation_id = format!("conversation-{turn_id}");
        let mut state = active_panel_state(
            chat_turn(turn_id),
            SonaAllowedValuesV1::default(),
            &format!("{turn_id}-key"),
        );
        state.conversation_id = Some(conversation_id);
        state.conversation = vec![SonaAgentChatTurnV1 {
            role: SonaAgentChatRoleV1::User,
            message: message.to_string(),
            outcome: None,
        }];
        state
            .turn
            .as_mut()
            .expect("active manager test turn")
            .submitting = false;
        state
    }

    #[derive(Debug)]
    struct TestRequest {
        line: String,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    }

    fn memory_secrets() -> Arc<SecretManager> {
        Arc::new(SecretManager::with_backend(Arc::new(
            MemorySecretBackend::new(),
        )))
    }

    async fn relay_client(
        secrets: &SecretManager,
        endpoint: &str,
        relay_key: &SigningKey,
    ) -> (RelayClient, AgentPanelPublicIdentityV1) {
        let seed = secrets
            .agent_panel_signing_seed()
            .await
            .expect("create in-memory signing seed");
        let signing_key = SigningKey::from_bytes(seed.as_bytes());
        let identity = public_identity_for_key(&signing_key);
        let client = RelayClient {
            base_url: validate_relay_url(endpoint).expect("loopback relay URL"),
            client_key_id: identity.key_id.clone(),
            signing_key,
            relay_key_id: "relay-test".to_string(),
            relay_verifying_key: relay_key.verifying_key(),
            nonce_cache: Arc::new(ResponseNonceCache::default()),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(15))
                .build()
                .expect("relay HTTP client"),
        };
        (client, identity)
    }

    fn active_panel_state(
        turn: PanelTurnV1,
        allowed: SonaAllowedValuesV1,
        idempotency_key: &str,
    ) -> PanelState {
        let turn_id = turn.turn_id().to_string();
        let workspace = turn.workspace();
        let base_pack = turn.context_pack().map(str::to_string);
        let mut state = PanelState::default();
        state.relay_status = AgentPanelRelayStatusV1::Ready;
        state.turn = Some(ActiveTurn {
            turn_id,
            workspace,
            idempotency_key: idempotency_key.to_string(),
            model_alias: SONA_MODEL_ALIAS.to_string(),
            request: turn,
            allowed,
            job_id: None,
            state: AgentPanelTurnStateV1::Submitting,
            event_cursor: 0,
            submitting: true,
            cancel_requested: false,
            last_progress: Instant::now(),
            started_at_utc_ms: chrono::Utc::now().timestamp_millis(),
            completed_at_utc_ms: None,
            failure: None,
            steps: Vec::new(),
            actions: Vec::new(),
            tool_rounds: 0,
            pending_calls: Vec::new(),
            base_pack,
        });
        state
    }

    async fn read_request(stream: &mut TcpStream) -> TestRequest {
        let mut received = Vec::new();
        let mut buffer = [0_u8; 1024];
        let headers_end = loop {
            let count = stream.read(&mut buffer).await.expect("read request");
            assert_ne!(count, 0, "request closed before headers");
            received.extend_from_slice(&buffer[..count]);
            if let Some(position) = received.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let text = std::str::from_utf8(&received[..headers_end]).expect("request headers");
        let mut lines = text.split("\r\n");
        let line = lines.next().expect("request line").to_string();
        let mut headers = BTreeMap::new();
        for header in lines.take_while(|line| !line.is_empty()) {
            let (name, value) = header.split_once(':').expect("header separator");
            headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
        }
        let body_len = headers
            .get("content-length")
            .map(|value| value.parse::<usize>().expect("numeric content length"))
            .unwrap_or(0);
        while received.len() < headers_end + body_len {
            let count = stream.read(&mut buffer).await.expect("read request body");
            assert_ne!(count, 0, "request closed before body");
            received.extend_from_slice(&buffer[..count]);
        }
        TestRequest {
            line,
            headers,
            body: received[headers_end..headers_end + body_len].to_vec(),
        }
    }

    fn request_header<'a>(request: &'a TestRequest, name: &str) -> &'a str {
        request
            .headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
            .expect("signed request header")
    }

    fn assert_request_signature(
        request: &TestRequest,
        client: &AgentPanelPublicIdentityV1,
        method: &str,
        path: &str,
    ) {
        assert_eq!(request.line, format!("{method} {path} HTTP/1.1"));
        assert_eq!(request_header(request, HEADER_KEY), client.key_id);
        assert_eq!(request_header(request, HEADER_DIRECTION), "request");
        let timestamp = request_header(request, HEADER_TIMESTAMP)
            .parse::<i64>()
            .expect("request timestamp");
        let nonce = request_header(request, HEADER_NONCE);
        let signature = base64::engine::general_purpose::STANDARD
            .decode(request_header(request, HEADER_SIGNATURE))
            .expect("request signature encoding");
        let signature = Signature::from_slice(&signature).expect("request signature");
        let client_key = verifying_key_from_base64(&client.public_key).expect("client public key");
        let context = SignatureContext::request(method, path, &request.body, timestamp, nonce);
        client_key
            .verify_strict(
                &canonical_bytes(&context).expect("canonical request"),
                &signature,
            )
            .expect("valid request signature");
    }

    fn endpoint(listener: &TcpListener) -> String {
        let address: SocketAddr = listener.local_addr().expect("listener address");
        format!("http://{address}")
    }

    fn signed_submission_server(
        listener: TcpListener,
        signing_key: SigningKey,
        client: AgentPanelPublicIdentityV1,
        response: SonaAgentResponseV1,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept relay request");
            let request = read_request(&mut stream).await;
            assert_request_signature(&request, &client, "POST", "/v1/jobs/submit");
            let submission: serde_json::Value =
                serde_json::from_slice(&request.body).expect("submission JSON");
            let workspace = submission["workspace_id"]
                .as_str()
                .expect("submission workspace");
            let capability = submission["capability"]
                .as_str()
                .expect("submission capability");
            let idempotency_key = submission["idempotency_key"]
                .as_str()
                .expect("submission idempotency key");
            let body = serde_json::to_vec(&serde_json::json!({
                "job": {
                    "id": "job-e2e",
                    "state": "SUCCEEDED",
                    "kind": capability,
                    "workspace_id": workspace,
                    "model_alias": SONA_MODEL_ALIAS,
                    "capabilities": [capability],
                    "tools": [],
                    "submitter_key_id": client.key_id,
                    "external_ref": idempotency_key,
                    "result": response,
                },
                "created": true,
            }))
            .expect("relay response JSON");
            let request_nonce = request_header(&request, HEADER_NONCE);
            let response_nonce = format!("relay-{}", Uuid::new_v4());
            let context = SignatureContext::response(
                "POST",
                "/v1/jobs/submit",
                &body,
                chrono::Utc::now().timestamp(),
                &response_nonce,
                StatusCode::OK,
                request_nonce,
            );
            let headers =
                sign_headers(&signing_key, "relay-test", &context).expect("relay response headers");
            let mut head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (name, value) in headers {
                head.push_str(name.as_str());
                head.push_str(": ");
                head.push_str(value.to_str().expect("response header value"));
                head.push_str("\r\n");
            }
            head.push_str("\r\n");
            stream
                .write_all(head.as_bytes())
                .await
                .expect("write relay headers");
            stream.write_all(&body).await.expect("write relay body");
        })
    }

    /* The relay's own refusal in the parts that decide this: a body, and no
     * `X-Bridge-*` header over it, because nothing signs a response written
     * before the envelope verified. It answers off the request head alone,
     * like the refusal it stands in for: the unknown-bridge-key return sits
     * ahead of `await request.read()`, `relay/app.py:127-131`. */
    fn bare_error_server(
        listener: TcpListener,
        status_line: &'static str,
        body: &'static str,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept relay request");
            let mut head_bytes = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !head_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).await.expect("read request");
                assert_ne!(count, 0, "request closed before headers");
                head_bytes.extend_from_slice(&buffer[..count]);
            }
            let head = format!(
                "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(head.as_bytes())
                .await
                .expect("write relay headers");
            stream
                .write_all(body.as_bytes())
                .await
                .expect("write relay body");
        })
    }

    enum ScriptedReply {
        RateLimited { retry_after: Option<&'static str> },
        NotFound,
        Signed(serde_json::Value),
    }

    fn scripted_server(
        listener: TcpListener,
        signing_key: SigningKey,
        replies: Vec<ScriptedReply>,
    ) -> JoinHandle<Vec<TestRequest>> {
        tokio::spawn(async move {
            let mut requests = Vec::with_capacity(replies.len());
            for reply in replies {
                let (mut stream, _) = listener.accept().await.expect("accept relay request");
                let request = read_request(&mut stream).await;
                let (status_line, body, retry_after, signed) = match reply {
                    ScriptedReply::RateLimited { retry_after } => {
                        ("429 Too Many Requests", Vec::new(), retry_after, false)
                    }
                    ScriptedReply::NotFound => ("404 Not Found", Vec::new(), None, false),
                    ScriptedReply::Signed(body) => (
                        "200 OK",
                        serde_json::to_vec(&body).expect("relay response JSON"),
                        None,
                        true,
                    ),
                };
                let mut head = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                if let Some(retry_after) = retry_after {
                    head.push_str("Retry-After: ");
                    head.push_str(retry_after);
                    head.push_str("\r\n");
                }
                if signed {
                    let mut parts = request.line.split_whitespace();
                    let method = parts.next().expect("request method");
                    let path = parts.next().expect("request path");
                    let response_nonce = format!("relay-{}", Uuid::new_v4());
                    let context = SignatureContext::response(
                        method,
                        path,
                        &body,
                        chrono::Utc::now().timestamp(),
                        &response_nonce,
                        StatusCode::OK,
                        request_header(&request, HEADER_NONCE),
                    );
                    for (name, value) in sign_headers(&signing_key, "relay-test", &context)
                        .expect("relay response headers")
                    {
                        head.push_str(name.as_str());
                        head.push_str(": ");
                        head.push_str(value.to_str().expect("response header value"));
                        head.push_str("\r\n");
                    }
                }
                head.push_str("\r\n");
                stream
                    .write_all(head.as_bytes())
                    .await
                    .expect("write scripted response headers");
                stream
                    .write_all(&body)
                    .await
                    .expect("write scripted response body");
                requests.push(request);
            }
            requests
        })
    }

    fn successful_job(
        client: &AgentPanelPublicIdentityV1,
        idempotency_key: &str,
        model_alias: &str,
    ) -> ScriptedReply {
        ScriptedReply::Signed(serde_json::json!({
            "job": {
                "id": "job-e2e",
                "state": "SUCCEEDED",
                "kind": "sona-chat",
                "workspace_id": "sona-chat",
                "model_alias": model_alias,
                "capabilities": ["sona-chat"],
                "tools": [],
                "submitter_key_id": client.key_id,
                "external_ref": idempotency_key,
                "result": {"kind":"text","message":"Done."},
            },
            "created": true,
        }))
    }

    fn chat_turn(turn_id: &str) -> PanelTurnV1 {
        PanelTurnV1::Chat(SonaChatTurnV2 {
            protocol_version: SONA_CHAT_TURN_VERSION.to_string(),
            conversation_id: format!("conversation-{turn_id}"),
            turn_id: turn_id.to_string(),
            user_message: "What did we decide?".to_string(),
            recent_turns: Vec::new(),
            context_pack: None,
            tools_allowed: false,
            locale: "en".to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            reply_is_json: false,
        })
    }

    async fn seed_open_loop(
        manager: &MeetingSessionManager,
    ) -> (Arc<MeetingStore>, MeetingSessionId, MeetingLoopRow) {
        let store = manager.store().await.expect("open encrypted meeting store");
        let session_id = workflow_core_tests::reviewable_meeting(
            &store,
            "Agent panel loop",
            1_700_000_000_000_i64,
        );
        let artifacts = serde_json::json!({
            "summary": {"text": "The deck still needs an owner.", "citations": []},
            "summary_trace": [],
            "outline": [],
            "decisions": [],
            "action_items": [],
            "key_questions": [],
            "risks": [],
            "follow_up_draft": {"text": "", "citations": []},
            "ledger": {
                "headline": "The deck owner is still open.",
                "threads": [],
                "open_loops": [{
                    "question": "Who will send the deck?",
                    "instead": "The meeting moved on without an owner.",
                    "at_ms": 12_000,
                    "citations": []
                }],
                "commitments": [],
                "stances": [],
                "caveats": [],
                "receipts": {"status": "verified"}
            }
        });
        workflow_core_tests::current_artifact(&store, session_id, &artifacts, 1);
        let loops = manager
            .loops_list(session_id)
            .await
            .expect("list seeded loops");
        assert_eq!(loops.rows.len(), 1, "one actual open loop");
        let row = loops.rows.into_iter().next().expect("seeded loop row");
        assert_eq!(row.status, MeetingLoopStatus::Open);
        (store, session_id, row)
    }
    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7_u8; 32])
    }

    #[test]
    fn canonicalizes_path_query_like_the_relay() {
        assert_eq!(
            canonical_path_query("/v1/a b?z=two+words&a=x%20y&a=").expect("canonical path"),
            "/v1/a%20b?a=&a=x+y&z=two+words"
        );
    }

    #[test]
    fn rejects_non_private_relay_hosts() {
        assert_eq!(
            validate_relay_url("http://relay.example.com"),
            Err(RelayError::CleartextRejected)
        );
        assert_eq!(
            validate_relay_url("https://relay.example.com"),
            Err(RelayError::CleartextRejected)
        );
        assert!(validate_relay_url("http://100.64.1.2").is_ok());
        assert!(validate_relay_url("http://localhost:8317").is_ok());
        assert!(validate_relay_url("https://[fd7a:115c:a1e0::1]").is_ok());
    }

    #[test]
    fn response_verification_rejects_tampered_boundaries_and_replays() {
        let signing_key = signing_key();
        let peer_key_id = "relay-key";
        let request_nonce = "request-nonce";
        let body = b"{\"job\":{}}";
        let timestamp = chrono::Utc::now().timestamp();
        let context = SignatureContext::response(
            "GET",
            "/v1/jobs/job-1",
            body,
            timestamp,
            "response-nonce",
            StatusCode::OK,
            request_nonce,
        );
        let headers = sign_headers(&signing_key, peer_key_id, &context).expect("sign response");
        let mut response_headers = HeaderMap::new();
        for (name, value) in headers {
            response_headers.insert(name, value);
        }
        let cache = ResponseNonceCache::default();
        let response = ResponseVerification {
            method: "GET",
            path: "/v1/jobs/job-1",
            body,
            headers: &response_headers,
            status: StatusCode::OK,
            request_nonce,
        };
        assert_eq!(
            verify_response(&signing_key.verifying_key(), peer_key_id, &cache, &response,),
            Ok(())
        );
        let tampered_response = ResponseVerification {
            method: "POST",
            path: "/v1/jobs/job-1",
            body,
            headers: &response_headers,
            status: StatusCode::OK,
            request_nonce,
        };
        assert_eq!(
            verify_response(
                &signing_key.verifying_key(),
                peer_key_id,
                &cache,
                &tampered_response,
            ),
            Err(RelayError::ResponseSignatureInvalid)
        );
    }

    #[test]
    fn identity_is_stable_and_does_not_include_the_seed() {
        let identity = public_identity_for_key(&signing_key());
        assert!(identity.key_id.starts_with("sona-"));
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(identity.public_key)
                .expect("public key encoding")
                .len(),
            32
        );
    }
    #[test]
    fn disabled_panel_identity_never_touches_the_secret_backend() {
        let backend = Arc::new(crate::secrets::MemorySecretBackend::new());
        let secrets = crate::secrets::SecretManager::with_backend(backend.clone());
        assert_eq!(backend.operation_count(), 0);
        assert_eq!(
            tauri::async_runtime::block_on(public_identity(false, &secrets)),
            Err(RelayError::Disabled)
        );
        assert_eq!(backend.operation_count(), 0);

        let first = tauri::async_runtime::block_on(public_identity(true, &secrets))
            .expect("explicit identity");
        assert!(backend.has("agent_panel/signing-seed-v1"));
        let operations_after_create = backend.operation_count();
        let second = tauri::async_runtime::block_on(public_identity(true, &secrets))
            .expect("stable explicit identity");
        assert_eq!(first, second);
        assert_eq!(backend.operation_count(), operations_after_create + 1);
    }

    #[test]
    fn pairing_only_accepts_a_tailnet_relay_and_a_real_ed25519_key() {
        let public_key = base64::engine::general_purpose::STANDARD
            .encode(signing_key().verifying_key().to_bytes());
        let paired = validate_pairing("  http://100.99.192.40:8650  ", " relay-01 ", &public_key)
            .expect("a tailnet relay with a 32-byte key pairs");
        assert_eq!(paired.relay_url, "http://100.99.192.40:8650/");
        assert_eq!(paired.relay_key_id, "relay-01");
        assert_eq!(paired.relay_public_key, public_key);

        assert_eq!(
            validate_pairing("https://relay.example.com", "relay-01", &public_key),
            Err(RelayError::CleartextRejected)
        );
        assert_eq!(
            validate_pairing("http://100.99.192.40/v1", "relay-01", &public_key),
            Err(RelayError::InvalidConfiguration)
        );
        assert_eq!(
            validate_pairing("http://100.99.192.40", "relay 01", &public_key),
            Err(RelayError::InvalidConfiguration)
        );
        /* A 31-byte key is well-formed base64 and not a key. */
        let short = base64::engine::general_purpose::STANDARD.encode([7_u8; 31]);
        assert_eq!(
            validate_pairing("http://100.99.192.40", "relay-01", &short),
            Err(RelayError::InvalidConfiguration)
        );
    }

    #[test]
    fn a_job_from_the_other_workspace_is_not_this_turns_job() {
        let wire = || RelayJobWire {
            id: "job-1".to_string(),
            state: "SUCCEEDED".to_string(),
            kind: "sona-chat".to_string(),
            workspace_id: "sona-chat".to_string(),
            model_alias: SONA_MODEL_ALIAS.to_string(),
            capabilities: vec!["sona-chat".to_string()],
            tools: Vec::new(),
            submitter_key_id: "sona-me".to_string(),
            external_ref: "abcd".to_string(),
            result: Some(serde_json::json!({"kind":"text","message":"Found it."})),
        };
        let expectation = |workspace| RelayJobExpectation {
            workspace,
            model_alias: SONA_MODEL_ALIAS,
            job_id: Some("job-1"),
            idempotency_key: None,
        };
        let job = wire()
            .into_job("sona-me", expectation(AgentPanelWorkspaceV1::SonaChat))
            .expect("a chat job answers a chat turn");
        assert!(matches!(
            job.response,
            Some(super::SonaAgentResponseV1::Text { .. })
        ));
        assert_eq!(
            wire()
                .into_job("sona-me", expectation(AgentPanelWorkspaceV1::SonaConfig))
                .err(),
            Some(RelayError::ResponseMalformed)
        );
    }

    #[test]
    fn a_row_echoing_a_maximal_cyrillic_pack_twice_fits_the_response_ceiling() {
        /* The relay hands the job row back through Python's `json.dumps`
         * with `ensure_ascii`: a character outside ASCII becomes a `\uXXXX`
         * escape, two of them past the BMP. The row holds the submission
         * twice — under `payload` and hoisted beside it — and the result
         * once. The literal two is the relay's shape, not the constant that
         * models it, so shrinking that constant back to one goes red here.
         * The English row that shipped the failure was 283 173 bytes for a
         * 141 310-byte pack; a Cyrillic pack costs three for every one. */
        let wire_len = |utf8: &str| -> usize {
            utf8.chars()
                .map(|character| match character.len_utf8() {
                    1 => 1,
                    4 => 12,
                    _ => 6,
                })
                .sum()
        };
        let cyrillic = |bytes: usize| "б".repeat(bytes / "б".len());
        let row = 2 * wire_len(&cyrillic(MAX_CHAT_SUBMISSION_BYTES))
            + wire_len(&cyrillic(MAX_PROPOSAL_BYTES))
            + JOB_ENVELOPE_BYTES;
        assert!(
            row <= MAX_RESPONSE_BYTES,
            "a job row answering a maximal Cyrillic pack is {row} bytes on the wire, over the {MAX_RESPONSE_BYTES}-byte ceiling: a pack the relay accepted would come back unreadable"
        );
    }

    #[test]
    fn failed_jobs_keep_a_typed_reason_without_exposing_relay_error_text() {
        let failure = |result| {
            RelayJobWire {
                id: "job-1".to_string(),
                state: "FAILED".to_string(),
                kind: "sona-chat".to_string(),
                workspace_id: "sona-chat".to_string(),
                model_alias: SONA_MODEL_ALIAS.to_string(),
                capabilities: vec!["sona-chat".to_string()],
                tools: Vec::new(),
                submitter_key_id: "sona-me".to_string(),
                external_ref: "abcd".to_string(),
                result: Some(result),
            }
            .into_job(
                "sona-me",
                RelayJobExpectation {
                    workspace: AgentPanelWorkspaceV1::SonaChat,
                    model_alias: SONA_MODEL_ALIAS,
                    job_id: Some("job-1"),
                    idempotency_key: None,
                },
            )
            .expect("a terminal job belongs to this turn")
            .failure
        };

        assert_eq!(
            failure(serde_json::json!({
                "error": "the model answer did not match the contract",
                "error_code": "sona_response_rejected"
            })),
            Some(RelayJobFailure::Refused)
        );
        assert_eq!(
            failure(serde_json::json!({
                "error": "omp exited with status 1",
                "error_code": "omp_exit_status"
            })),
            Some(RelayJobFailure::Failed)
        );
        /* The bytes below are the ones the box actually produced, copied from
         * relay job 14e6b661: `rejection_result` in
         * `omp_bridge/worker/vps_sona.py` builds the sentence from the
         * contract's own refusal and sets this code. It is asserted verbatim
         * because it is the only thing tying the two hosts together — the code
         * is a string on a wire, nothing on either side would fail to compile
         * if it drifted, and this failure arriving as the blanket `Refused`
         * would be silent again. */
        assert_eq!(
            failure(serde_json::json!({
                "error": "sona-chat response rejected: Sona chat turn declared reply_is_json \
                          and the message is not a JSON object",
                "error_code": "sona_reply_not_structured"
            })),
            Some(RelayJobFailure::ReplyNotStructured),
            "a prose answer to a structured request is the one refusal a caller acts on"
        );
        /* And a `FAILED` job with no code at all is still a plain failure,
         * because the worker on the other host sets no codes. */
        assert_eq!(
            failure(serde_json::json!({ "error": "worker task canceled" })),
            Some(RelayJobFailure::Failed)
        );
    }

    /* A relay that refuses the envelope answers without signing it: the
     * unknown-key 401 at `relay/app.py:127-128` and the bad-signature 401 at
     * `:168-169` are both written while `verified` is still false, and a 503
     * from a unit that is down was never near the signing key at all.
     *
     * Verifying before reading the status turned all three into
     * `ResponseSignatureInvalid` - this client accusing the relay of forgery
     * over a pairing the relay had forgotten, or over an outage, and telling
     * the reader the reply was not signed by the paired server either way. */
    #[test]
    fn an_unsigned_error_is_reported_as_its_status_and_not_as_a_forgery() {
        tauri::async_runtime::block_on(async {
            for (status_line, body, expected) in [
                (
                    "401 Unauthorized",
                    r#"{"error": {"code": "unauthorized", "message": "unknown bridge key"}}"#,
                    RelayError::Unauthorized,
                ),
                (
                    "503 Service Unavailable",
                    "<html><body><h1>503 Service Unavailable</h1></body></html>",
                    RelayError::RequestFailed,
                ),
            ] {
                let secrets = memory_secrets();
                let listener = TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind relay listener");
                let endpoint = endpoint(&listener);
                let server = bare_error_server(listener, status_line, body);
                let (client, _) = relay_client(&secrets, &endpoint, &signing_key()).await;
                assert_eq!(
                    client
                        .cancel_job("job-e2e", AgentPanelWorkspaceV1::SonaChat, SONA_MODEL_ALIAS,)
                        .await
                        .err(),
                    Some(expected),
                    "unsigned {status_line}"
                );
                server.await.expect("relay server task");
            }
        });
    }

    #[test]
    fn rate_limit_schedule_is_bounded_and_parses_retry_after() {
        assert_eq!(
            (0..=3)
                .map(|attempt| rate_limit_delay(attempt, None))
                .collect::<Vec<_>>(),
            vec![
                Some(Duration::from_secs(1)),
                Some(Duration::from_secs(2)),
                Some(Duration::from_secs(4)),
                None,
            ]
        );
        assert_eq!(
            (0..=3)
                .map(|attempt| rate_limit_delay(attempt, Some(Duration::from_secs(3))))
                .collect::<Vec<_>>(),
            vec![
                Some(Duration::from_secs(3)),
                Some(Duration::from_secs(3)),
                Some(Duration::from_secs(4)),
                None,
            ]
        );
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("later"));
        assert_eq!(
            failure_for_status(StatusCode::TOO_MANY_REQUESTS, &headers),
            Some(RelayError::RateLimited(None))
        );
    }

    #[test]
    fn submit_retries_a_rate_limit_without_sleeping() {
        tauri::async_runtime::block_on(async {
            let secrets = memory_secrets();
            let relay_key = signing_key();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind relay listener");
            let (client, identity) = relay_client(&secrets, &endpoint(&listener), &relay_key).await;
            let turn = chat_turn("retry-submit");
            let server = scripted_server(
                listener,
                relay_key,
                vec![
                    ScriptedReply::RateLimited {
                        retry_after: Some("2"),
                    },
                    successful_job(&identity, "retry-submit-key", SONA_MODEL_ALIAS),
                ],
            );
            let waits = Arc::new(Mutex::new(Vec::new()));
            let observed = Arc::clone(&waits);

            let job = retry_rate_limited(
                || client.submit_turn("retry-submit-key", &turn, SONA_MODEL_ALIAS),
                move |duration| {
                    observed.lock().expect("wait observations").push(duration);
                    std::future::ready(())
                },
                |_| {},
                || true,
            )
            .await
            .expect("second submission succeeds");
            let requests = server.await.expect("scripted relay task");

            assert_eq!(job.state, RelayJobStateV1::Succeeded);
            assert_eq!(requests.len(), 2);
            assert_eq!(
                *waits.lock().expect("wait observations"),
                vec![Duration::from_secs(2)]
            );
            for request in &requests {
                assert_request_signature(request, &identity, "POST", "/v1/jobs/submit");
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).expect("submission JSON");
                assert_eq!(body["model"], SONA_MODEL_ALIAS);
            }
        });
    }

    #[test]
    fn four_rate_limits_exhaust_the_retry_budget() {
        tauri::async_runtime::block_on(async {
            let secrets = memory_secrets();
            let relay_key = signing_key();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind relay listener");
            let endpoint = endpoint(&listener);
            let server = scripted_server(
                listener,
                relay_key,
                (0..4)
                    .map(|_| ScriptedReply::RateLimited { retry_after: None })
                    .collect(),
            );
            let (client, _) = relay_client(&secrets, &endpoint, &signing_key()).await;
            let turn = chat_turn("retry-exhausted");
            let waits = Arc::new(Mutex::new(Vec::new()));
            let observed = Arc::clone(&waits);

            let result = retry_rate_limited(
                || client.submit_turn("retry-exhausted-key", &turn, SONA_MODEL_ALIAS),
                move |duration| {
                    observed.lock().expect("wait observations").push(duration);
                    std::future::ready(())
                },
                |_| {},
                || true,
            )
            .await;
            let requests = server.await.expect("scripted relay task");

            assert_eq!(result.err(), Some(RelayError::RateLimited(None)));
            assert_eq!(requests.len(), 4);
            assert_eq!(
                *waits.lock().expect("wait observations"),
                vec![
                    Duration::from_secs(1),
                    Duration::from_secs(2),
                    Duration::from_secs(4),
                ]
            );
        });
    }

    #[test]
    fn retry_after_over_the_cap_fails_without_waiting() {
        tauri::async_runtime::block_on(async {
            let secrets = memory_secrets();
            let relay_key = signing_key();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind relay listener");
            let endpoint = endpoint(&listener);
            let server = scripted_server(
                listener,
                relay_key,
                vec![ScriptedReply::RateLimited {
                    retry_after: Some("45"),
                }],
            );
            let (client, _) = relay_client(&secrets, &endpoint, &signing_key()).await;
            let turn = chat_turn("retry-capped");
            let waits = Arc::new(Mutex::new(Vec::new()));
            let observed = Arc::clone(&waits);

            let result = retry_rate_limited(
                || client.submit_turn("retry-capped-key", &turn, SONA_MODEL_ALIAS),
                move |duration| {
                    observed.lock().expect("wait observations").push(duration);
                    std::future::ready(())
                },
                |_| {},
                || true,
            )
            .await;
            let requests = server.await.expect("scripted relay task");

            assert_eq!(
                result.err(),
                Some(RelayError::RateLimited(Some(Duration::from_secs(45))))
            );
            assert_eq!(requests.len(), 1);
            assert!(waits.lock().expect("wait observations").is_empty());
        });
    }

    #[test]
    fn retry_rate_limit_checks_cancellation_before_wait() {
        tauri::async_runtime::block_on(async {
            let calls = Arc::new(AtomicUsize::new(0));
            let waits = Arc::new(AtomicUsize::new(0));
            let observed_calls = Arc::clone(&calls);
            let observed_waits = Arc::clone(&waits);
            let result = retry_rate_limited(
                move || {
                    observed_calls.fetch_add(1, Ordering::SeqCst);
                    async { Err::<(), RelayError>(RelayError::RateLimited(None)) }
                },
                move |_| {
                    observed_waits.fetch_add(1, Ordering::SeqCst);
                    std::future::ready(())
                },
                |_| {},
                || false,
            )
            .await;

            assert_eq!(result, Err(RelayError::RateLimited(None)));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(waits.load(Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn retry_rate_limit_checks_cancellation_after_wait() {
        tauri::async_runtime::block_on(async {
            let calls = Arc::new(AtomicUsize::new(0));
            let waits = Arc::new(AtomicUsize::new(0));
            let canceled = Arc::new(AtomicBool::new(false));
            let observed_calls = Arc::clone(&calls);
            let observed_waits = Arc::clone(&waits);
            let canceled_during_wait = Arc::clone(&canceled);
            let cancellation_check = Arc::clone(&canceled);
            let result = retry_rate_limited(
                move || {
                    observed_calls.fetch_add(1, Ordering::SeqCst);
                    async { Err::<(), RelayError>(RelayError::RateLimited(None)) }
                },
                move |_| {
                    observed_waits.fetch_add(1, Ordering::SeqCst);
                    canceled_during_wait.store(true, Ordering::SeqCst);
                    std::future::ready(())
                },
                |_| {},
                move || !cancellation_check.load(Ordering::SeqCst),
            )
            .await;

            assert_eq!(result, Err(RelayError::RateLimited(None)));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(waits.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn retry_rate_limit_stops_after_poll_generation_changes() {
        tauri::async_runtime::block_on(async {
            let generation = Arc::new(AtomicU64::new(7));
            let calls = Arc::new(AtomicUsize::new(0));
            let generation_during_wait = Arc::clone(&generation);
            let current_generation = Arc::clone(&generation);
            let observed_calls = Arc::clone(&calls);
            let result = retry_rate_limited(
                move || {
                    observed_calls.fetch_add(1, Ordering::SeqCst);
                    async { Err::<(), RelayError>(RelayError::RateLimited(None)) }
                },
                move |_| {
                    generation_during_wait.store(8, Ordering::SeqCst);
                    std::future::ready(())
                },
                |_| {},
                move || current_generation.load(Ordering::SeqCst) == 7,
            )
            .await;

            assert_eq!(result, Err(RelayError::RateLimited(None)));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(generation.load(Ordering::SeqCst), 8);
        });
    }

    #[test]
    fn polling_retries_a_rate_limit_without_sleeping() {
        tauri::async_runtime::block_on(async {
            let secrets = memory_secrets();
            let relay_key = signing_key();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind relay listener");
            let (client, identity) = relay_client(&secrets, &endpoint(&listener), &relay_key).await;
            let ScriptedReply::Signed(submission) =
                successful_job(&identity, "original-key", SONA_MODEL_ALIAS)
            else {
                unreachable!("a successful job helper always returns a signed response")
            };
            let server = scripted_server(
                listener,
                relay_key,
                vec![
                    ScriptedReply::RateLimited { retry_after: None },
                    ScriptedReply::Signed(serde_json::json!({
                        "job": submission["job"].clone()
                    })),
                ],
            );
            let waits = Arc::new(Mutex::new(Vec::new()));
            let observed = Arc::clone(&waits);

            let job = retry_rate_limited(
                || client.get_job("job-e2e", AgentPanelWorkspaceV1::SonaChat, SONA_MODEL_ALIAS),
                move |duration| {
                    observed.lock().expect("wait observations").push(duration);
                    std::future::ready(())
                },
                |_| {},
                || true,
            )
            .await
            .expect("second poll succeeds");
            let requests = server.await.expect("scripted relay task");

            assert_eq!(job.state, RelayJobStateV1::Succeeded);
            assert_eq!(requests.len(), 2);
            assert_eq!(
                *waits.lock().expect("wait observations"),
                vec![Duration::from_secs(1)]
            );
            for request in &requests {
                assert_request_signature(request, &identity, "GET", "/v1/jobs/job-e2e");
            }
        });
    }

    #[test]
    fn catalog_selects_the_default_model_for_submission() {
        tauri::async_runtime::block_on(async {
            let secrets = memory_secrets();
            let relay_key = signing_key();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind relay listener");
            let (client, identity) = relay_client(&secrets, &endpoint(&listener), &relay_key).await;
            let server = scripted_server(
                listener,
                relay_key,
                vec![
                    ScriptedReply::Signed(serde_json::json!({
                        "models": [
                            {"alias": "fast", "default": true},
                            {"alias": "ultra", "default": false}
                        ]
                    })),
                    successful_job(&identity, "catalog-key", "fast"),
                ],
            );

            let alias = client.preferred_model_alias().await;
            let job = client
                .submit_turn("catalog-key", &chat_turn("catalog"), &alias)
                .await
                .expect("catalog model job");
            let requests = server.await.expect("scripted relay task");

            assert_eq!(alias, "fast");
            assert_eq!(job.state, RelayJobStateV1::Succeeded);
            assert_request_signature(&requests[0], &identity, "GET", "/v1/models");
            let submission: serde_json::Value =
                serde_json::from_slice(&requests[1].body).expect("submission JSON");
            assert_eq!(submission["model"], "fast");
        });
    }

    #[test]
    fn model_catalog_cache_respects_force_refresh_and_pairing_rotation() {
        tauri::async_runtime::block_on(async {
            let relay_key = signing_key();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind relay listener");
            let relay_endpoint = endpoint(&listener);
            let (_data_dir, app) = paired_panel_app(&relay_endpoint, &relay_key);
            let manager = AgentPanelManager::new(app.handle());
            let secrets = memory_secrets();
            let (client, identity) =
                relay_client(secrets.as_ref(), &relay_endpoint, &relay_key).await;
            let server = scripted_server(
                listener,
                relay_key,
                vec![
                    ScriptedReply::Signed(serde_json::json!({
                        "models": [{"alias": "fast", "default": true}]
                    })),
                    ScriptedReply::Signed(serde_json::json!({
                        "models": [{"alias": "ultra", "default": true}]
                    })),
                    ScriptedReply::Signed(serde_json::json!({
                        "models": [{"alias": "first", "default": true}]
                    })),
                ],
            );

            let first = manager.model_alias(&client, false).await;
            let cached = manager.model_alias(&client, false).await;
            let forced = manager.model_alias(&client, true).await;

            let store = app
                .handle()
                .store(crate::portable::store_path(
                    crate::settings::SETTINGS_STORE_PATH,
                ))
                .expect("settings store");
            let mut settings = crate::settings::get_settings(app.handle());
            settings.agent_panel_relay_key_id = Some("relay-test-rotated".to_string());
            store.set(
                "settings",
                serde_json::to_value(&settings).expect("serialize rotated settings"),
            );
            let rotated = manager.model_alias(&client, false).await;
            assert_eq!(first, "fast");
            assert_eq!(cached, "fast");
            assert_eq!(forced, "ultra");
            assert_eq!(rotated, "first");
            let requests = server.await.expect("scripted relay task");
            assert_eq!(requests.len(), 3);
            for request in &requests {
                assert_request_signature(&request, &identity, "GET", "/v1/models");
            }
        });
    }

    #[test]
    fn submit_retry_stops_when_canceled_during_wait() {
        tauri::async_runtime::block_on(async {
            let relay_key = signing_key();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind relay listener");
            let relay_endpoint = endpoint(&listener);
            let (_data_dir, app) = paired_panel_app(&relay_endpoint, &relay_key);
            let secrets = memory_secrets();
            app.handle().manage(secrets.clone());
            let manager = Arc::new(AgentPanelManager::new(app.handle()));
            *manager.lock_state() =
                active_chat_state_for_manager("submit-cancel", "Cancel this question");
            let server = scripted_server(
                listener,
                relay_key,
                vec![
                    ScriptedReply::Signed(serde_json::json!({
                        "models": [{"alias": "fast", "default": true}]
                    })),
                    ScriptedReply::RateLimited { retry_after: None },
                ],
            );
            let cancellation_manager = Arc::clone(&manager);
            let result = manager
                .submit_active_turn_with_delay("submit-cancel", move |_| {
                    let manager = Arc::clone(&cancellation_manager);
                    async move {
                        let mut state = manager.lock_state();
                        let active = state.turn.as_mut().expect("active canceled turn");
                        active.cancel_requested = true;
                        active.state = AgentPanelTurnStateV1::Canceling;
                    }
                })
                .await;
            let requests = server.await.expect("scripted relay task");

            assert_eq!(result, Ok(()));
            let status = manager.current_status();
            assert_eq!(
                status.turn.as_ref().expect("canceled turn").state,
                AgentPanelTurnStateV1::Canceled
            );
            assert_eq!(
                status.conversation[0].outcome,
                Some(SonaAgentChatOutcomeV1::Canceled)
            );
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].line, "GET /v1/models HTTP/1.1");
            assert_eq!(requests[1].line, "POST /v1/jobs/submit HTTP/1.1");
        });
    }

    #[test]
    fn exhausted_submit_rate_limit_is_recorded_without_returning_error() {
        tauri::async_runtime::block_on(async {
            let relay_key = signing_key();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind relay listener");
            let relay_endpoint = endpoint(&listener);
            let (_data_dir, app) = paired_panel_app(&relay_endpoint, &relay_key);
            let secrets = memory_secrets();
            app.handle().manage(secrets.clone());
            let manager = Arc::new(AgentPanelManager::new(app.handle()));
            *manager.lock_state() =
                active_chat_state_for_manager("submit-exhausted", "Try this question");
            let server = scripted_server(
                listener,
                relay_key,
                vec![
                    ScriptedReply::Signed(serde_json::json!({
                        "models": [{"alias": "fast", "default": true}]
                    })),
                    ScriptedReply::RateLimited { retry_after: None },
                    ScriptedReply::RateLimited { retry_after: None },
                    ScriptedReply::RateLimited { retry_after: None },
                    ScriptedReply::RateLimited { retry_after: None },
                ],
            );
            let result = manager
                .submit_active_turn_with_delay("submit-exhausted", |_| std::future::ready(()))
                .await;
            let requests = server.await.expect("scripted relay task");

            assert_eq!(result, Ok(()));
            let status = manager.current_status();
            let turn = status.turn.as_ref().expect("failed turn");
            assert_eq!(turn.state, AgentPanelTurnStateV1::Failed);
            assert_eq!(turn.failure, Some(AgentPanelTurnFailureV1::RateLimited));
            assert_eq!(
                status.conversation[0].outcome,
                Some(SonaAgentChatOutcomeV1::Failure {
                    failure: AgentPanelTurnFailureV1::RateLimited,
                })
            );
            assert_eq!(requests.len(), 5);
            assert_eq!(requests[0].line, "GET /v1/models HTTP/1.1");
            assert!(requests[1..]
                .iter()
                .all(|request| request.line == "POST /v1/jobs/submit HTTP/1.1"));
        });
    }

    #[test]
    fn missing_catalog_keeps_the_fallback_model() {
        tauri::async_runtime::block_on(async {
            let secrets = memory_secrets();
            let relay_key = signing_key();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind relay listener");
            let (client, identity) = relay_client(&secrets, &endpoint(&listener), &relay_key).await;
            let server = scripted_server(
                listener,
                relay_key,
                vec![
                    ScriptedReply::NotFound,
                    successful_job(&identity, "fallback-key", SONA_MODEL_ALIAS),
                ],
            );

            let alias = client.preferred_model_alias().await;
            client
                .submit_turn("fallback-key", &chat_turn("fallback"), &alias)
                .await
                .expect("fallback model job");
            let requests = server.await.expect("scripted relay task");

            assert_eq!(alias, SONA_MODEL_ALIAS);
            let submission: serde_json::Value =
                serde_json::from_slice(&requests[1].body).expect("submission JSON");
            assert_eq!(submission["model"], SONA_MODEL_ALIAS);
        });
    }

    #[test]
    fn catalog_without_a_default_prefers_ultra_then_the_first_model() {
        let catalog = |aliases: &[&str]| SonaModelCatalogV1 {
            models: aliases
                .iter()
                .map(|alias| SonaModelCatalogEntryV1 {
                    alias: (*alias).to_string(),
                    default: false,
                })
                .collect(),
        };

        assert_eq!(
            choose_model_alias(&catalog(&["fast", "ultra"])).as_deref(),
            Some("ultra")
        );
        assert_eq!(
            choose_model_alias(&catalog(&["fast", "slow"])).as_deref(),
            Some("fast")
        );
        assert_eq!(choose_model_alias(&catalog(&[])), None);
    }

    #[test]
    fn a_job_row_must_echo_the_model_that_was_submitted() {
        let row = RelayJobWire {
            id: "job-1".to_string(),
            state: "SUCCEEDED".to_string(),
            kind: "sona-chat".to_string(),
            workspace_id: "sona-chat".to_string(),
            model_alias: "ultra".to_string(),
            capabilities: vec!["sona-chat".to_string()],
            tools: Vec::new(),
            submitter_key_id: "sona-me".to_string(),
            external_ref: "model-key".to_string(),
            result: Some(serde_json::json!({"kind":"text","message":"Done."})),
        };

        assert_eq!(
            row.into_job(
                "sona-me",
                RelayJobExpectation {
                    workspace: AgentPanelWorkspaceV1::SonaChat,
                    model_alias: "fast",
                    job_id: Some("job-1"),
                    idempotency_key: None,
                },
            )
            .err(),
            Some(RelayError::ResponseMalformed)
        );
    }

    #[test]
    fn signed_config_proposal_is_pending_and_settings_replay_is_fenced() {
        tauri::async_runtime::block_on(async {
            let secrets = memory_secrets();
            let relay_key = signing_key();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind relay listener");
            let (client, identity) = relay_client(&secrets, &endpoint(&listener), &relay_key).await;
            let mut settings = crate::settings::get_default_settings();
            let original_theme = settings.theme;
            let target_theme = if original_theme == Theme::Dark {
                Theme::Light
            } else {
                Theme::Dark
            };
            let device_names = DeviceNames::default();
            let snapshot = config::snapshot_from_parts(&settings, &[], &device_names);
            let allowed = snapshot.allowed_values(&device_names);
            let turn = PanelTurnV1::Config(SonaAgentTurnV1 {
                protocol_version: SONA_AGENT_TURN_VERSION.to_string(),
                conversation_id: "conversation-config-e2e".to_string(),
                turn_id: "config-e2e".to_string(),
                user_message: "Use the requested appearance.".to_string(),
                recent_turns: Vec::new(),
                config_snapshot: snapshot,
                proposal_schema: SonaAgentTurnV1::proposal_schema()
                    .expect("static proposal schema"),
                locale: "en".to_string(),
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            });
            turn.validate().expect("valid config turn");
            let idempotency_key = "config-e2e-key";
            let mut state = active_panel_state(turn.clone(), allowed, idempotency_key);
            let server = signed_submission_server(
                listener,
                relay_key,
                identity,
                SonaAgentResponseV1::Proposal {
                    proposal: SonaConfigProposalV1 {
                        version: SONA_CONFIG_PROPOSAL_VERSION.to_string(),
                        summary: "Use the selected appearance.".to_string(),
                        rationale: "It is the requested local setting.".to_string(),
                        actions: vec![SonaSettingChangeV1::Theme(target_theme)],
                        follow_up_question: None,
                        source_settings_revision: settings.settings_revision,
                    },
                    steps: Vec::new(),
                },
            );

            let job = client
                .submit_turn(idempotency_key, &turn, SONA_MODEL_ALIAS)
                .await
                .expect("accept signed relay response");
            let accepted = accept_job_in_state(&mut state, "config-e2e", job, false)
                .expect("accept valid proposal into panel state");
            server.await.expect("relay server task");
            assert_eq!(accepted.turn_state, AgentPanelTurnStateV1::Succeeded);
            assert_eq!(
                accepted.proposal_event,
                Some((
                    "proposal-config-e2e".to_string(),
                    AgentPanelProposalStateV1::Pending,
                ))
            );
            let offered = state.status().proposal.expect("visible config proposal");
            assert_eq!(offered.state, AgentPanelProposalStateV1::Pending);
            assert!(offered.receipt_id.is_none());
            assert_eq!(settings.theme, original_theme, "an offer is not a write");

            let proposal = state.proposal.as_ref().expect("stored proposal");
            let changes = proposal.proposal.actions.clone();
            let allowed = proposal.allowed.clone();
            let original_revision = settings.settings_revision;
            let undo = config::apply_changes_to_settings(
                &mut settings,
                original_revision,
                &changes,
                &allowed,
            )
            .expect("apply offered appearance");
            settings.settings_revision = original_revision + 1;
            assert_eq!(settings.theme, target_theme);
            assert_eq!(
                config::apply_changes_to_settings(
                    &mut settings,
                    original_revision,
                    &changes,
                    &allowed,
                ),
                Err(config::ConfigError::StaleRevision),
                "the persistent wrapper's revision advance fences replay"
            );
            assert_eq!(settings.theme, target_theme);
            let stale_undo_revision = settings.settings_revision + 1;
            assert_eq!(
                config::undo_changes_to_settings(&mut settings, stale_undo_revision, &undo),
                Err(config::ConfigError::StaleRevision)
            );
            let applied_revision = settings.settings_revision;
            config::undo_changes_to_settings(&mut settings, applied_revision, &undo)
                .expect("undo applied appearance");
            settings.settings_revision = applied_revision + 1;
            assert_eq!(settings.theme, original_theme);
            assert_eq!(
                config::undo_changes_to_settings(&mut settings, applied_revision, &undo),
                Err(config::ConfigError::StaleRevision),
                "undo replay is fenced by the next revision"
            );
        });
    }

    #[test]
    fn signed_resolve_loop_card_applies_once_and_reopens_once() {
        tauri::async_runtime::block_on(async {
            let root = tempfile::tempdir().expect("temporary agent panel root");
            let secrets = memory_secrets();
            let meetings = MeetingSessionManager::with_parts(
                None,
                Some(root.path().join("meetings")),
                Arc::clone(&secrets),
                Arc::new(NoCaptureSources),
            );
            let (store, session_id, original) = seed_open_loop(&meetings).await;
            let relay_key = signing_key();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind relay listener");
            let (client, identity) = relay_client(&secrets, &endpoint(&listener), &relay_key).await;
            let context_pack = format!(
                "Evidence: sona://loop/{}
The deck was sent.",
                original.loop_id.as_str()
            );
            let turn = PanelTurnV1::Chat(SonaChatTurnV2 {
                protocol_version: SONA_CHAT_TURN_VERSION.to_string(),
                conversation_id: "conversation-action-e2e".to_string(),
                turn_id: "action-e2e".to_string(),
                user_message: "Close the deck loop.".to_string(),
                recent_turns: Vec::new(),
                context_pack: Some(context_pack),
                tools_allowed: false,
                locale: "en".to_string(),
                app_version: env!("CARGO_PKG_VERSION").to_string(),
                reply_is_json: false,
            });
            turn.validate().expect("valid chat turn");
            let idempotency_key = "action-e2e-key";
            let mut state = active_panel_state(
                turn.clone(),
                SonaAllowedValuesV1::default(),
                idempotency_key,
            );
            let server = signed_submission_server(
                listener,
                relay_key,
                identity,
                SonaAgentResponseV1::Text {
                    message: "I marked the loop done.".to_string(),
                    actions: vec![SonaChatActionV1::ResolveLoop {
                        reason: "The meeting confirmed the deck was sent.".to_string(),
                        loop_id: original.loop_id.clone(),
                    }],
                    steps: Vec::new(),
                },
            );

            let job = client
                .submit_turn(idempotency_key, &turn, SONA_MODEL_ALIAS)
                .await
                .expect("accept signed relay response");
            let accepted = accept_job_in_state(&mut state, "action-e2e", job, false)
                .expect("accept valid action into panel state");
            server.await.expect("relay server task");
            assert_eq!(accepted.turn_state, AgentPanelTurnStateV1::Succeeded);
            let offered = state.status().turn.expect("completed action turn");
            assert_eq!(offered.actions[0].state, AgentPanelActionStateV1::Pending);
            assert!(offered.actions[0].operation_id.is_none());

            let loop_id = match &state.turn.as_ref().expect("stored action turn").actions[0].action
            {
                SonaChatActionV1::ResolveLoop { loop_id, .. } => loop_id.clone(),
                _ => panic!("relay returned the wrong action"),
            };
            let applied = crate::agent_panel::actions::resolve_loop(&meetings, &loop_id)
                .await
                .expect("apply loop action");
            let operation_id = applied
                .operation_id
                .clone()
                .expect("loop resolve receipt id");
            state.turn.as_mut().expect("stored action turn").actions[0].state =
                StoredActionState::Applied(applied);
            let applied_card = &state.turn.as_ref().expect("stored action turn").actions[0];
            assert!(
                applied_card.to_run().is_none(),
                "an applied card cannot reach the mutation again"
            );
            assert!(matches!(applied_card.reversal(), Reversal::Undo(_)));
            let applied_status = state.status().turn.expect("applied action turn");
            assert_eq!(
                applied_status.actions[0].state,
                AgentPanelActionStateV1::Applied
            );
            assert_eq!(
                applied_status.actions[0].operation_id.as_deref(),
                Some(operation_id.as_str())
            );
            let receipt = store
                .operation_receipt(MeetingOperationId::from_uuid(
                    Uuid::parse_str(&operation_id).expect("operation id UUID"),
                ))
                .expect("read loop receipt")
                .expect("stored loop receipt");
            assert_eq!(receipt.command, MeetingCommandKind::LoopResolve);
            assert_eq!(receipt.result, OperationResult::Committed);
            assert!(receipt.new_revision.is_some());
            assert_eq!(
                meetings
                    .loops_list(session_id)
                    .await
                    .expect("list closed loop")
                    .rows[0]
                    .status,
                MeetingLoopStatus::Done
            );
            assert_eq!(
                workflow_core_tests::committed_receipt_count(
                    &store,
                    MeetingCommandKind::LoopResolve,
                ),
                1
            );

            let replay = state.status().turn.expect("replayed action state");
            assert_eq!(replay.actions[0].state, AgentPanelActionStateV1::Applied);
            assert_eq!(
                replay.actions[0].operation_id.as_deref(),
                Some(operation_id.as_str())
            );
            assert_eq!(
                workflow_core_tests::committed_receipt_count(
                    &store,
                    MeetingCommandKind::LoopResolve,
                ),
                1,
                "reading an applied card cannot run its mutation again"
            );

            crate::agent_panel::actions::reopen_loop(&meetings, &loop_id)
                .await
                .expect("undo loop action");
            state.turn.as_mut().expect("stored action turn").actions[0].state =
                StoredActionState::Dismissed;
            let dismissed_card = &state.turn.as_ref().expect("stored action turn").actions[0];
            assert!(dismissed_card.to_run().is_none());
            assert!(matches!(dismissed_card.reversal(), Reversal::Settled));
            let dismissed = state.status().turn.expect("dismissed action turn");
            assert_eq!(
                dismissed.actions[0].state,
                AgentPanelActionStateV1::Dismissed
            );
            assert!(dismissed.actions[0].operation_id.is_none());
            assert_eq!(
                workflow_core_tests::committed_receipt_count(
                    &store,
                    MeetingCommandKind::LoopReopen,
                ),
                1
            );
            let restored = meetings
                .loops_list(session_id)
                .await
                .expect("list reopened loop")
                .rows
                .into_iter()
                .find(|row| row.loop_id == original.loop_id)
                .expect("original loop after reopen");
            assert_eq!(restored.status, MeetingLoopStatus::Open);
            assert_eq!(restored.owner_person_id, original.owner_person_id);
            assert_eq!(restored.resolved_at_utc_ms, original.resolved_at_utc_ms);
            assert_eq!(
                restored.resolving_operation_id,
                original.resolving_operation_id
            );
            assert_eq!(restored.text, original.text);
            assert_eq!(restored.instead, original.instead);
            let replay_dismissal = state.status().turn.expect("replayed dismissal state");
            assert_eq!(
                replay_dismissal.actions[0].state,
                AgentPanelActionStateV1::Dismissed
            );
            assert_eq!(
                workflow_core_tests::committed_receipt_count(
                    &store,
                    MeetingCommandKind::LoopReopen,
                ),
                1,
                "reading a dismissed card cannot run its inverse again"
            );
        });
    }
}
