use std::{
    fmt,
    sync::{
        atomic::{AtomicI64, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use chrono::DateTime;
use futures_util::StreamExt;
use reqwest::{
    header::{CONTENT_LENGTH, CONTENT_TYPE, DATE, ETAG, RETRY_AFTER},
    Method, StatusCode, Url,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::cloud_sync::crypto::{
    base64_url_encode, sha256_base64url, sign_canonical_request, CanonicalRequestInput,
};

const PROTOCOL_AUDIENCE: &str = "sona-companion";
const PROTOCOL_VERSION: u32 = 1;
const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 4 * 1024 * 1024;
#[cfg(test)]
const MAX_SHARE_BYTES: usize = 256 * 1024 * 1024;
const MAX_CHUNKS_PER_UPLOAD: u32 = 4096;
#[cfg(test)]
const MAX_CHUNKS_PER_UPLOAD_USIZE: usize = 4096;
const MIN_ENCRYPTED_PAYLOAD_BYTES: usize = 28;
#[cfg(test)]
const SHARE_FILE_HEADER_CAP: usize = 2 * 1024 * 1024;
#[cfg(test)]
const MAX_SHARE_FILE_BYTES: usize =
    MAX_SHARE_BYTES + SHARE_FILE_HEADER_CAP + (MAX_CHUNKS_PER_UPLOAD_USIZE * 4);
const CAPABILITIES_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const JSON_CONTENT_TYPE: &str = "application/json";
const OCTET_STREAM_CONTENT_TYPE: &str = "application/octet-stream";
#[cfg(test)]
const SHARE_FILE_CONTENT_TYPE: &str = "application/vnd.sona.encrypted-share";

#[derive(Clone)]
pub(crate) struct CloudClient {
    endpoint: Url,
    http: reqwest::Client,
    capabilities: Arc<Mutex<Option<CapabilitiesCache>>>,
    last_timestamp_ms: Arc<AtomicU64>,
    clock_offset_ms: Arc<AtomicI64>,
    latest_server_date: Arc<Mutex<Option<ServerDateObservation>>>,
}

struct AuthenticatedPayload<'a> {
    body: Vec<u8>,
    content_type: Option<&'static str>,
    idempotency_key: Option<&'a IdempotencyKey>,
}

impl<'a> AuthenticatedPayload<'a> {
    fn read() -> Self {
        Self {
            body: Vec::new(),
            content_type: None,
            idempotency_key: None,
        }
    }

    fn mutation(
        body: Vec<u8>,
        content_type: &'static str,
        idempotency_key: &'a IdempotencyKey,
    ) -> Self {
        Self {
            body,
            content_type: Some(content_type),
            idempotency_key: Some(idempotency_key),
        }
    }
}

impl CloudClient {
    pub(crate) fn new(endpoint: &str) -> Result<Self, CloudClientError> {
        let endpoint = Url::parse(endpoint).map_err(|_| CloudClientError::InvalidEndpoint)?;
        Self::from_url(endpoint)
    }

    pub(crate) fn from_url(mut endpoint: Url) -> Result<Self, CloudClientError> {
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !matches!(endpoint.path(), "" | "/")
        {
            return Err(CloudClientError::InvalidEndpoint);
        }

        endpoint.set_path("/");
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CloudClientError::Transport)?;

        Ok(Self {
            endpoint,
            http,
            capabilities: Arc::new(Mutex::new(None)),
            last_timestamp_ms: Arc::new(AtomicU64::new(0)),
            clock_offset_ms: Arc::new(AtomicI64::new(0)),
            latest_server_date: Arc::new(Mutex::new(None)),
        })
    }

    pub(crate) fn cached_capabilities(&self) -> Option<CapabilitiesCache> {
        let cache = self
            .capabilities
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = cache.as_ref()?;
        (entry.expires_at > Instant::now()).then(|| entry.clone())
    }

    pub(crate) fn set_clock_offset_ms(&self, offset_ms: i64) {
        self.clock_offset_ms.store(offset_ms, Ordering::Relaxed);
    }

    pub(crate) fn clock_offset_ms(&self) -> i64 {
        self.clock_offset_ms.load(Ordering::Relaxed)
    }

    pub(crate) fn latest_server_date(&self) -> Option<ServerDateObservation> {
        self.latest_server_date
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .copied()
    }

    pub(crate) async fn capabilities(
        &self,
        credentials: &CloudCredentials<'_>,
    ) -> Result<CloudCapabilities, CloudClientError> {
        if let Some(cache) = self.cached_capabilities() {
            return Ok(cache.capabilities);
        }

        let response: CloudCapabilities = self
            .authenticated_json(
                credentials,
                Method::GET,
                &["v1", "capabilities"],
                Vec::new(),
                AuthenticatedPayload::read(),
            )
            .await?;
        let fetched_at = Instant::now();
        let cache = CapabilitiesCache {
            capabilities: response.clone(),
            expires_at: fetched_at + CAPABILITIES_CACHE_TTL,
        };
        let mut slot = self
            .capabilities
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = Some(cache);
        Ok(response)
    }

    pub(crate) async fn devices(
        &self,
        credentials: &CloudCredentials<'_>,
    ) -> Result<DevicesResponse, CloudClientError> {
        self.authenticated_json(
            credentials,
            Method::GET,
            &["v1", "devices"],
            Vec::new(),
            AuthenticatedPayload::read(),
        )
        .await
    }

    pub(crate) async fn self_device(
        &self,
        credentials: &CloudCredentials<'_>,
    ) -> Result<SelfDeviceResponse, CloudClientError> {
        self.authenticated_json(
            credentials,
            Method::GET,
            &["v1", "devices", "self"],
            Vec::new(),
            AuthenticatedPayload::read(),
        )
        .await
    }

    pub(crate) async fn bootstrap_device(
        &self,
        bootstrap_secret: &str,
        idempotency_key: &IdempotencyKey,
        payload: &BootstrapDeviceRequest,
    ) -> Result<BootstrapDeviceResponse, CloudClientError> {
        let body = json_body(payload)?;
        let (url, _) = self.route(&["v1", "bootstrap", "device"], Vec::new())?;
        let request = self
            .http
            .request(Method::POST, url)
            .header("X-Sona-Bootstrap-Secret", bootstrap_secret)
            .header("X-Sona-Idempotency-Key", idempotency_key.as_str())
            .header(CONTENT_TYPE, JSON_CONTENT_TYPE)
            .header(CONTENT_LENGTH, body.len().to_string())
            .body(body);
        self.send_json(request).await
    }

    pub(crate) async fn pair_device(
        &self,
        credentials: &CloudCredentials<'_>,
        idempotency_key: &IdempotencyKey,
        payload: &PairDeviceRequest,
    ) -> Result<PairDeviceResponse, CloudClientError> {
        self.authenticated_json(
            credentials,
            Method::POST,
            &["v1", "devices", "pair"],
            Vec::new(),
            AuthenticatedPayload::mutation(json_body(payload)?, JSON_CONTENT_TYPE, idempotency_key),
        )
        .await
    }

    pub(crate) async fn create_object_upload(
        &self,
        credentials: &CloudCredentials<'_>,
        idempotency_key: &IdempotencyKey,
        payload: &ObjectUploadPlan,
    ) -> Result<UploadCreatedResponse, CloudClientError> {
        self.authenticated_json(
            credentials,
            Method::POST,
            &["v1", "uploads"],
            Vec::new(),
            AuthenticatedPayload::mutation(json_body(payload)?, JSON_CONTENT_TYPE, idempotency_key),
        )
        .await
    }

    pub(crate) async fn upload_status(
        &self,
        credentials: &CloudCredentials<'_>,
        upload_id: &str,
    ) -> Result<UploadStatusResponse, CloudClientError> {
        let upload_id = checked_opaque_id(upload_id)?;
        self.authenticated_json(
            credentials,
            Method::GET,
            &["v1", "uploads", upload_id],
            Vec::new(),
            AuthenticatedPayload::read(),
        )
        .await
    }

    pub(crate) async fn upload_chunk(
        &self,
        credentials: &CloudCredentials<'_>,
        idempotency_key: &IdempotencyKey,
        upload_id: &str,
        index: u32,
        ciphertext: Vec<u8>,
    ) -> Result<UploadChunkResponse, CloudClientError> {
        let upload_id = checked_opaque_id(upload_id)?;
        let index = checked_chunk_index(index)?;
        if ciphertext.len() < MIN_ENCRYPTED_PAYLOAD_BYTES {
            return Err(CloudClientError::InvalidInput);
        }
        if ciphertext.len() > MAX_CHUNK_BYTES {
            return Err(CloudClientError::RequestTooLarge {
                limit: MAX_CHUNK_BYTES,
            });
        }

        let chunk_digest = sha256_base64url(&ciphertext);
        let request = self.authenticated_request(
            credentials,
            Method::PUT,
            &["v1", "uploads", upload_id, "chunks", &index],
            Vec::new(),
            AuthenticatedPayload::mutation(ciphertext, OCTET_STREAM_CONTENT_TYPE, idempotency_key),
        )?;
        self.send_json(request.header("X-Sona-Chunk-Sha256", chunk_digest))
            .await
    }

    pub(crate) async fn commit_upload(
        &self,
        credentials: &CloudCredentials<'_>,
        idempotency_key: &IdempotencyKey,
        upload_id: &str,
    ) -> Result<UploadCommittedResponse, CloudClientError> {
        let upload_id = checked_opaque_id(upload_id)?;
        self.authenticated_json(
            credentials,
            Method::POST,
            &["v1", "uploads", upload_id, "commit"],
            Vec::new(),
            AuthenticatedPayload::mutation(version_body()?, JSON_CONTENT_TYPE, idempotency_key),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn cancel_upload(
        &self,
        credentials: &CloudCredentials<'_>,
        idempotency_key: &IdempotencyKey,
        upload_id: &str,
    ) -> Result<UploadCancelledResponse, CloudClientError> {
        let upload_id = checked_opaque_id(upload_id)?;
        self.authenticated_json(
            credentials,
            Method::DELETE,
            &["v1", "uploads", upload_id],
            Vec::new(),
            AuthenticatedPayload::mutation(version_body()?, JSON_CONTENT_TYPE, idempotency_key),
        )
        .await
    }

    pub(crate) async fn tombstone_object(
        &self,
        credentials: &CloudCredentials<'_>,
        idempotency_key: &IdempotencyKey,
        object_id: &str,
        payload: &TombstoneRequest,
    ) -> Result<TombstoneResponse, CloudClientError> {
        let object_id = checked_opaque_id(object_id)?;
        self.authenticated_json(
            credentials,
            Method::DELETE,
            &["v1", "objects", object_id],
            Vec::new(),
            AuthenticatedPayload::mutation(json_body(payload)?, JSON_CONTENT_TYPE, idempotency_key),
        )
        .await
    }

    pub(crate) async fn changes(
        &self,
        credentials: &CloudCredentials<'_>,
        cursor: Option<&str>,
        limit: Option<u16>,
    ) -> Result<ChangesPage, CloudClientError> {
        let mut query = Vec::new();
        if let Some(cursor) = cursor {
            query.push(("cursor".to_owned(), checked_query_value(cursor)?.to_owned()));
        }
        if let Some(limit) = limit {
            query.push(("limit".to_owned(), checked_page_limit(limit)?.to_string()));
        }
        self.authenticated_json(
            credentials,
            Method::GET,
            &["v1", "changes"],
            query,
            AuthenticatedPayload::read(),
        )
        .await
    }

    pub(crate) async fn snapshot(
        &self,
        credentials: &CloudCredentials<'_>,
        high_water: Option<&str>,
        after: Option<&str>,
        limit: Option<u16>,
    ) -> Result<SnapshotPage, CloudClientError> {
        if high_water.is_none() && after.is_some() {
            return Err(CloudClientError::InvalidInput);
        }

        let mut query = Vec::new();
        if let Some(high_water) = high_water {
            query.push((
                "highWater".to_owned(),
                checked_query_value(high_water)?.to_owned(),
            ));
        }
        if let Some(after) = after {
            query.push(("after".to_owned(), checked_query_value(after)?.to_owned()));
        }
        if let Some(limit) = limit {
            query.push(("limit".to_owned(), checked_page_limit(limit)?.to_string()));
        }
        self.authenticated_json(
            credentials,
            Method::GET,
            &["v1", "snapshot"],
            query,
            AuthenticatedPayload::read(),
        )
        .await
    }

    pub(crate) async fn object_manifest(
        &self,
        credentials: &CloudCredentials<'_>,
        object_id: &str,
        revision_id: &str,
    ) -> Result<ObjectManifestResponse, CloudClientError> {
        let object_id = checked_opaque_id(object_id)?;
        let revision_id = checked_opaque_id(revision_id)?;
        self.authenticated_json(
            credentials,
            Method::GET,
            &[
                "v1",
                "objects",
                object_id,
                "revisions",
                revision_id,
                "manifest",
            ],
            Vec::new(),
            AuthenticatedPayload::read(),
        )
        .await
    }

    pub(crate) async fn object_chunk(
        &self,
        credentials: &CloudCredentials<'_>,
        object_id: &str,
        revision_id: &str,
        index: u32,
    ) -> Result<EncryptedChunk, CloudClientError> {
        let object_id = checked_opaque_id(object_id)?;
        let revision_id = checked_opaque_id(revision_id)?;
        let index = checked_chunk_index(index)?;
        let request = self.authenticated_request(
            credentials,
            Method::GET,
            &[
                "v1",
                "objects",
                object_id,
                "revisions",
                revision_id,
                "chunks",
                &index,
            ],
            Vec::new(),
            AuthenticatedPayload::read(),
        )?;
        self.send_chunk(request).await
    }

    pub(crate) async fn create_share_upload(
        &self,
        credentials: &CloudCredentials<'_>,
        idempotency_key: &IdempotencyKey,
        payload: &ShareUploadPlan,
    ) -> Result<UploadCreatedResponse, CloudClientError> {
        self.authenticated_json(
            credentials,
            Method::POST,
            &["v1", "shares"],
            Vec::new(),
            AuthenticatedPayload::mutation(json_body(payload)?, JSON_CONTENT_TYPE, idempotency_key),
        )
        .await
    }

    pub(crate) async fn revoke_share(
        &self,
        credentials: &CloudCredentials<'_>,
        idempotency_key: &IdempotencyKey,
        share_id: &str,
    ) -> Result<ShareRevokedResponse, CloudClientError> {
        let share_id = checked_opaque_id(share_id)?;
        self.authenticated_json(
            credentials,
            Method::DELETE,
            &["v1", "shares", share_id],
            Vec::new(),
            AuthenticatedPayload::mutation(version_body()?, JSON_CONTENT_TYPE, idempotency_key),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn public_share_manifest(
        &self,
        share_id: &str,
    ) -> Result<PublicShareManifest, CloudClientError> {
        let share_id = checked_opaque_id(share_id)?;
        let (url, _) = self.route(&["v1", "shares", share_id, "manifest"], Vec::new())?;
        self.send_json(self.http.get(url)).await
    }

    #[cfg(test)]
    pub(crate) async fn download_share_file(&self, share_id: &str) -> Result<(), CloudClientError> {
        let share_id = checked_opaque_id(share_id)?;
        let (url, _) = self.route(&["v1", "shares", share_id, "file"], Vec::new())?;
        let response = self
            .send_bytes(self.http.get(url), MAX_SHARE_FILE_BYTES)
            .await?;
        if response.content_type.as_deref() != Some(SHARE_FILE_CONTENT_TYPE) {
            return Err(CloudClientError::InvalidResponse {
                status: response.status.as_u16(),
            });
        }
        Ok(())
    }

    async fn authenticated_json<T>(
        &self,
        credentials: &CloudCredentials<'_>,
        method: Method,
        segments: &[&str],
        query: Vec<(String, String)>,
        payload: AuthenticatedPayload<'_>,
    ) -> Result<T, CloudClientError>
    where
        T: DeserializeOwned,
    {
        let request = self.authenticated_request(credentials, method, segments, query, payload)?;
        self.send_json(request).await
    }
    fn authenticated_request(
        &self,
        credentials: &CloudCredentials<'_>,
        method: Method,
        segments: &[&str],
        query: Vec<(String, String)>,
        payload: AuthenticatedPayload<'_>,
    ) -> Result<reqwest::RequestBuilder, CloudClientError> {
        let AuthenticatedPayload {
            body,
            content_type,
            idempotency_key,
        } = payload;
        let (url, canonical_query) = self.route(segments, query)?;
        let body_digest = sha256_base64url(&body);
        let timestamp_ms = self.next_timestamp_ms()?;
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| CloudClientError::Randomness)?;
        let canonical_query = canonical_query
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        let input = CanonicalRequestInput {
            audience: PROTOCOL_AUDIENCE,
            vault_id: credentials.vault_id,
            device_id: credentials.device_id,
            method: method.as_str(),
            path: url.path(),
            query: &canonical_query,
            body_digest: &body_digest,
            content_type: content_type.unwrap_or_default(),
            idempotency_key: idempotency_key.map_or("", IdempotencyKey::as_str),
            timestamp: timestamp_ms,
            nonce: &nonce,
        };
        let signature = sign_canonical_request(&input, credentials.signing_seed)
            .map_err(|_| CloudClientError::Signing)?;
        let mut request = self
            .http
            .request(method, url)
            .header("X-Sona-Vault-Id", credentials.vault_id)
            .header("X-Sona-Device-Id", credentials.device_id)
            .header("X-Sona-Timestamp", timestamp_ms.to_string())
            .header("X-Sona-Nonce", base64_url_encode(&nonce))
            .header("X-Sona-Signature", base64_url_encode(&signature));
        if let Some(idempotency_key) = idempotency_key {
            request = request.header("X-Sona-Idempotency-Key", idempotency_key.as_str());
        }
        if let Some(content_type) = content_type {
            request = request
                .header(CONTENT_TYPE, content_type)
                .header(CONTENT_LENGTH, body.len().to_string())
                .body(body);
        }
        Ok(request)
    }
    fn route(
        &self,
        segments: &[&str],
        mut query: Vec<(String, String)>,
    ) -> Result<(Url, Vec<(String, String)>), CloudClientError> {
        query.sort_unstable();
        let mut url = self.endpoint.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| CloudClientError::InvalidEndpoint)?;
            path.clear();
            for segment in segments {
                path.push(segment);
            }
        }
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            pairs.clear();
            for (key, value) in &query {
                pairs.append_pair(key, value);
            }
        }
        Ok((url, query))
    }

    fn next_timestamp_ms(&self) -> Result<u64, CloudClientError> {
        let wall_clock = unix_millis(SystemTime::now()).ok_or(CloudClientError::Clock)?;
        let offset = self.clock_offset_ms.load(Ordering::Relaxed);
        let observed = i64::try_from(wall_clock)
            .ok()
            .and_then(|millis| millis.checked_add(offset))
            .and_then(|millis| u64::try_from(millis).ok())
            .filter(|millis| (1_000_000_000_000..10_000_000_000_000).contains(millis))
            .ok_or(CloudClientError::Clock)?;
        let mut previous = self.last_timestamp_ms.load(Ordering::Relaxed);
        loop {
            let next = observed.max(previous.checked_add(1).ok_or(CloudClientError::Clock)?);
            match self.last_timestamp_ms.compare_exchange_weak(
                previous,
                next,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(next),
                Err(current) => previous = current,
            }
        }
    }

    async fn send_json<T>(&self, request: reqwest::RequestBuilder) -> Result<T, CloudClientError>
    where
        T: DeserializeOwned,
    {
        let response = self.send_bytes(request, MAX_JSON_BYTES).await?;
        if !is_json_content_type(response.content_type.as_deref()) {
            return Err(CloudClientError::InvalidResponse {
                status: response.status.as_u16(),
            });
        }
        serde_json::from_slice(&response.bytes).map_err(|_| CloudClientError::InvalidResponse {
            status: response.status.as_u16(),
        })
    }

    async fn send_chunk(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<EncryptedChunk, CloudClientError> {
        let response = self.send_bytes(request, MAX_CHUNK_BYTES).await?;
        if response.content_type.as_deref() != Some(OCTET_STREAM_CONTENT_TYPE) {
            return Err(CloudClientError::InvalidResponse {
                status: response.status.as_u16(),
            });
        }
        Ok(EncryptedChunk {
            bytes: response.bytes,
            etag: response.etag,
        })
    }

    async fn send_bytes(
        &self,
        request: reqwest::RequestBuilder,
        limit: usize,
    ) -> Result<ResponseBytes, CloudClientError> {
        let response = request
            .send()
            .await
            .map_err(|_| CloudClientError::Transport)?;
        let response = collect_response(response, limit).await?;
        self.observe_server_date(response.server_date);
        if response.status.is_success() {
            return Ok(response);
        }
        Err(parse_api_error(response))
    }

    fn observe_server_date(&self, server_date: Option<SystemTime>) {
        let Some(server_date) = server_date else {
            return;
        };
        let Some(server_time_ms) = unix_millis(server_date) else {
            return;
        };
        let Some(local_time_ms) = unix_millis(SystemTime::now()) else {
            return;
        };
        let (Ok(server_time), Ok(local_time)) =
            (i64::try_from(server_time_ms), i64::try_from(local_time_ms))
        else {
            return;
        };
        let Some(clock_offset_ms) = server_time.checked_sub(local_time) else {
            return;
        };
        self.clock_offset_ms
            .store(clock_offset_ms, Ordering::Relaxed);
        let mut latest = self
            .latest_server_date
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *latest = Some(ServerDateObservation {
            server_time_ms,
            clock_offset_ms,
        });
    }
}

#[derive(Clone)]
pub(crate) struct CloudCredentials<'a> {
    vault_id: &'a str,
    device_id: &'a str,
    signing_seed: &'a [u8; 32],
}

impl<'a> CloudCredentials<'a> {
    pub(crate) fn new(
        vault_id: &'a str,
        device_id: &'a str,
        signing_seed: &'a [u8; 32],
    ) -> Result<Self, CloudClientError> {
        checked_opaque_id(vault_id)?;
        checked_opaque_id(device_id)?;
        Ok(Self {
            vault_id,
            device_id,
            signing_seed,
        })
    }
}

impl fmt::Debug for CloudCredentials<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CloudCredentials(<redacted>)")
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct IdempotencyKey(String);

impl IdempotencyKey {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, CloudClientError> {
        let value = value.into();
        checked_opaque_id(&value)?;
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for IdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IdempotencyKey(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ServerDateObservation {
    pub(crate) server_time_ms: u64,
    pub(crate) clock_offset_ms: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct CapabilitiesCache {
    pub(crate) capabilities: CloudCapabilities,
    pub(crate) expires_at: Instant,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudCapabilities {
    pub(crate) protocol_version: u32,
    pub(crate) crypto_version: u32,
    pub(crate) request_auth: RequestAuthCapabilities,
    pub(crate) limits: CloudLimits,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestAuthCapabilities {
    pub(crate) algorithm: String,
    pub(crate) clock_skew_seconds: u64,
    pub(crate) nonce_retention_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudLimits {
    pub(crate) remote_bytes: u64,
    pub(crate) chunk_bytes: u64,
    pub(crate) chunks_per_upload: u32,
    pub(crate) active_uploads: u32,
    pub(crate) active_shares: u32,
    pub(crate) share_bytes: u64,
    pub(crate) share_expiry_seconds: u64,
    pub(crate) change_page: u16,
    pub(crate) snapshot_page: u16,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BootstrapDeviceRequest {
    pub(crate) version: u32,
    pub(crate) vault_id: String,
    pub(crate) device_id: String,
    pub(crate) signing_public_key: String,
    pub(crate) pairing_public_key: String,
    pub(crate) self_signature: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PairDeviceRequest {
    pub(crate) version: u32,
    pub(crate) candidate_device_id: String,
    pub(crate) candidate_signing_public_key: String,
    pub(crate) candidate_pairing_public_key: String,
    pub(crate) candidate_proof: String,
    pub(crate) pairing_nonce: String,
    pub(crate) expires_at: u64,
    pub(crate) envelope: String,
    pub(crate) approval_signature: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UploadChunkPlan {
    pub(crate) index: u32,
    pub(crate) size: u64,
    pub(crate) sha256: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ObjectUploadPlan {
    pub(crate) version: u32,
    pub(crate) crypto_version: u32,
    pub(crate) upload_id: String,
    pub(crate) object_id: String,
    pub(crate) revision_id: String,
    pub(crate) base_revision_id: Option<String>,
    pub(crate) manifest: String,
    pub(crate) manifest_sha256: String,
    pub(crate) chunks: Vec<UploadChunkPlan>,
    pub(crate) chunk_count: u32,
    pub(crate) total_bytes: u64,
    pub(crate) writer_signature: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ShareUploadPlan {
    pub(crate) version: u32,
    pub(crate) crypto_version: u32,
    pub(crate) upload_id: String,
    pub(crate) share_id: String,
    pub(crate) expires_at: u64,
    pub(crate) manifest: String,
    pub(crate) manifest_sha256: String,
    pub(crate) chunks: Vec<UploadChunkPlan>,
    pub(crate) chunk_count: u32,
    pub(crate) total_bytes: u64,
    pub(crate) writer_signature: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TombstoneRequest {
    pub(crate) tombstone_revision_id: String,
    pub(crate) base_revision_id: String,
    pub(crate) format_version: u32,
    pub(crate) reason: TombstoneReason,
    pub(crate) writer_signature: String,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TombstoneReason {
    UserRequest,
}

#[derive(Serialize)]
struct VersionRequest {
    version: u32,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct BootstrapDeviceResponse {
    pub(crate) vault_id: String,
    pub(crate) device_id: String,
    pub(crate) status: String,
    pub(crate) capabilities: CloudCapabilities,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PairDeviceResponse {
    pub(crate) device_id: String,
    pub(crate) status: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeviceRecord {
    pub(crate) device_id: String,
    pub(crate) signing_public_key: String,
    pub(crate) pairing_public_key: String,
    pub(crate) status: String,
    pub(crate) created_at: u64,
    pub(crate) revoked_at: Option<u64>,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DevicesResponse {
    pub(crate) devices: Vec<DeviceRecord>,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SelfDeviceResponse {
    pub(crate) device_id: String,
    pub(crate) signing_public_key: String,
    pub(crate) pairing_public_key: String,
    pub(crate) status: String,
    pub(crate) envelope: Option<String>,
    pub(crate) protocol_version: Option<u32>,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct UploadCreatedResponse {
    pub(crate) upload_id: String,
    #[serde(default)]
    pub(crate) share_id: Option<String>,
    pub(crate) state: String,
    pub(crate) accepted_indexes: Vec<u32>,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct UploadStatusResponse {
    pub(crate) upload_id: String,
    pub(crate) state: String,
    pub(crate) accepted_indexes: Vec<u32>,
    pub(crate) expires_at: u64,
    pub(crate) committed_sequence: Option<u64>,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct UploadChunkResponse {
    pub(crate) upload_id: String,
    pub(crate) index: u32,
    pub(crate) accepted: bool,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct UploadCommittedResponse {
    pub(crate) upload_id: String,
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) revision_id: Option<String>,
    #[serde(default)]
    pub(crate) change_sequence: Option<u64>,
    #[serde(default)]
    pub(crate) share_id: Option<String>,
}

#[cfg(test)]
#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct UploadCancelledResponse {
    pub(crate) upload_id: String,
    pub(crate) state: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShareRevokedResponse {
    pub(crate) share_id: String,
    pub(crate) state: String,
}
#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TombstoneResponse {
    pub(crate) object_id: String,
    pub(crate) revision_id: String,
    pub(crate) tombstone: bool,
    pub(crate) change_sequence: u64,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangeRecord {
    pub(crate) sequence: u64,
    pub(crate) object_id: String,
    pub(crate) revision_id: String,
    pub(crate) tombstone: bool,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangesPage {
    pub(crate) changes: Vec<ChangeRecord>,
    pub(crate) next_cursor: String,
    pub(crate) has_more: bool,
    pub(crate) high_water: u64,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotHead {
    pub(crate) object_id: String,
    pub(crate) revision_id: String,
    pub(crate) tombstone: bool,
    pub(crate) sequence: u64,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotPage {
    pub(crate) heads: Vec<SnapshotHead>,
    pub(crate) high_water: String,
    pub(crate) after: Option<String>,
    pub(crate) has_more: bool,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObjectRevisionEnvelope {
    pub(crate) object_id: String,
    pub(crate) revision_id: String,
    pub(crate) parent_revision_id: Option<String>,
    pub(crate) manifest_sha256: String,
    pub(crate) chunk_count: u32,
    pub(crate) total_bytes: u64,
    pub(crate) crypto_version: u32,
    pub(crate) writer_device_id: String,
    pub(crate) writer_signature: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObjectManifestResponse {
    pub(crate) envelope: ObjectRevisionEnvelope,
    pub(crate) manifest: String,
}

#[cfg(test)]
#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShareUploadChunkPlan {
    pub(crate) index: u32,
    pub(crate) size: u64,
    pub(crate) sha256: String,
}

#[cfg(test)]
#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicShareMetadata {
    pub(crate) share_id: String,
    pub(crate) crypto_version: u32,
    pub(crate) manifest_sha256: String,
    pub(crate) chunk_count: u32,
    pub(crate) total_bytes: u64,
    pub(crate) writer_signature: String,
}

#[cfg(test)]
#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicShareManifest {
    pub(crate) version: u32,
    pub(crate) share: PublicShareMetadata,
    pub(crate) manifest: String,
    pub(crate) chunks: Vec<ShareUploadChunkPlan>,
}

pub(crate) struct EncryptedChunk {
    pub(crate) bytes: Vec<u8>,
    pub(crate) etag: Option<String>,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct WireApiError {
    code: String,
    request_id: String,
    retryable: bool,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct RequestId(String);

impl RequestId {
    #[cfg(test)]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RequestId(<redacted>)")
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloudErrorCode {
    InvalidRequest,
    Unauthorized,
    RevokedDevice,
    ClockSkew,
    Replay,
    IdempotencyConflict,
    NotFound,
    StaleRevision,
    UploadIncomplete,
    ChunkConflict,
    IntegrityFailed,
    UnsupportedVersion,
    QuotaExceeded,
    RateLimited,
    DependencyUnavailable,
    CursorExpired,
    Unknown,
}

impl CloudErrorCode {
    fn parse(value: &str) -> Self {
        match value {
            "invalid_request" => Self::InvalidRequest,
            "unauthorized" => Self::Unauthorized,
            "revoked_device" => Self::RevokedDevice,
            "clock_skew" => Self::ClockSkew,
            "replay" => Self::Replay,
            "idempotency_conflict" => Self::IdempotencyConflict,
            "not_found" => Self::NotFound,
            "stale_revision" => Self::StaleRevision,
            "upload_incomplete" => Self::UploadIncomplete,
            "chunk_conflict" => Self::ChunkConflict,
            "integrity_failed" => Self::IntegrityFailed,
            "unsupported_version" => Self::UnsupportedVersion,
            "quota_exceeded" => Self::QuotaExceeded,
            "rate_limited" => Self::RateLimited,
            "dependency_unavailable" => Self::DependencyUnavailable,
            "cursor_expired" => Self::CursorExpired,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClockSkewCategory {
    ServerAhead,
    ServerBehind,
    Indeterminate,
    DateUnavailable,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct CloudApiError {
    pub(crate) status: u16,
    pub(crate) code: CloudErrorCode,
    pub(crate) request_id: RequestId,
    pub(crate) retryable: bool,
    pub(crate) retry_after: Option<Duration>,
    pub(crate) server_date: Option<SystemTime>,
    pub(crate) clock_skew: Option<ClockSkewCategory>,
}

impl fmt::Debug for CloudApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CloudApiError")
            .field("status", &self.status)
            .field("code", &self.code)
            .field("request_id", &"<redacted>")
            .field("retryable", &self.retryable)
            .field("retry_after", &self.retry_after)
            .field("server_date", &self.server_date)
            .field("clock_skew", &self.clock_skew)
            .finish()
    }
}

pub(crate) enum CloudClientError {
    InvalidEndpoint,
    InvalidInput,
    RequestTooLarge { limit: usize },
    ResponseTooLarge { limit: usize },
    Serialization,
    Signing,
    Randomness,
    Clock,
    Transport,
    InvalidResponse { status: u16 },
    Api(CloudApiError),
}

impl CloudClientError {
    pub(crate) fn api_error(&self) -> Option<&CloudApiError> {
        match self {
            Self::Api(error) => Some(error),
            _ => None,
        }
    }
}

impl fmt::Debug for CloudClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => formatter.write_str("InvalidEndpoint"),
            Self::InvalidInput => formatter.write_str("InvalidInput"),
            Self::RequestTooLarge { limit } => formatter
                .debug_struct("RequestTooLarge")
                .field("limit", limit)
                .finish(),
            Self::ResponseTooLarge { limit } => formatter
                .debug_struct("ResponseTooLarge")
                .field("limit", limit)
                .finish(),
            Self::Serialization => formatter.write_str("Serialization"),
            Self::Signing => formatter.write_str("Signing"),
            Self::Randomness => formatter.write_str("Randomness"),
            Self::Clock => formatter.write_str("Clock"),
            Self::Transport => formatter.write_str("Transport"),
            Self::InvalidResponse { status } => formatter
                .debug_struct("InvalidResponse")
                .field("status", status)
                .finish(),
            Self::Api(error) => formatter.debug_tuple("Api").field(error).finish(),
        }
    }
}

impl fmt::Display for CloudClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => formatter.write_str("invalid cloud endpoint"),
            Self::InvalidInput => formatter.write_str("invalid cloud request input"),
            Self::RequestTooLarge { .. } => formatter.write_str("cloud request exceeds its limit"),
            Self::ResponseTooLarge { .. } => {
                formatter.write_str("cloud response exceeds its limit")
            }
            Self::Serialization => formatter.write_str("cloud request serialization failed"),
            Self::Signing => formatter.write_str("cloud request signing failed"),
            Self::Randomness => formatter.write_str("cloud request nonce generation failed"),
            Self::Clock => formatter.write_str("cloud request clock failed"),
            Self::Transport => formatter.write_str("cloud transport failed"),
            Self::InvalidResponse { .. } => formatter.write_str("invalid cloud response"),
            Self::Api(_) => formatter.write_str("cloud API rejected the request"),
        }
    }
}

impl std::error::Error for CloudClientError {}

struct ResponseBytes {
    status: StatusCode,
    bytes: Vec<u8>,
    content_type: Option<String>,
    etag: Option<String>,
    retry_after: Option<Duration>,
    server_date: Option<SystemTime>,
}

async fn collect_response(
    response: reqwest::Response,
    limit: usize,
) -> Result<ResponseBytes, CloudClientError> {
    let limit_u64 =
        u64::try_from(limit).map_err(|_| CloudClientError::ResponseTooLarge { limit })?;
    if response
        .content_length()
        .is_some_and(|length| length > limit_u64)
    {
        return Err(CloudClientError::ResponseTooLarge { limit });
    }

    let status = response.status();
    let headers = response.headers();
    let now = SystemTime::now();
    let server_date = headers
        .get(DATE)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_http_date);
    let retry_after = headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_retry_after(value, now));
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let etag = headers
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(next) = stream.next().await {
        let next = next.map_err(|_| CloudClientError::Transport)?;
        let next_length = bytes
            .len()
            .checked_add(next.len())
            .ok_or(CloudClientError::ResponseTooLarge { limit })?;
        if next_length > limit {
            return Err(CloudClientError::ResponseTooLarge { limit });
        }
        bytes.extend_from_slice(&next);
    }

    Ok(ResponseBytes {
        status,
        bytes,
        content_type,
        etag,
        retry_after,
        server_date,
    })
}

fn parse_api_error(response: ResponseBytes) -> CloudClientError {
    let wire = match serde_json::from_slice::<WireApiError>(&response.bytes) {
        Ok(wire) => wire,
        Err(_) => {
            return CloudClientError::InvalidResponse {
                status: response.status.as_u16(),
            }
        }
    };
    let code = CloudErrorCode::parse(&wire.code);
    let clock_skew = (code == CloudErrorCode::ClockSkew).then(|| {
        let Some(server_date) = response.server_date else {
            return ClockSkewCategory::DateUnavailable;
        };
        match server_date.duration_since(SystemTime::now()) {
            Ok(delta) if delta > Duration::ZERO => ClockSkewCategory::ServerAhead,
            Err(error) if error.duration() > Duration::ZERO => ClockSkewCategory::ServerBehind,
            _ => ClockSkewCategory::Indeterminate,
        }
    });

    CloudClientError::Api(CloudApiError {
        status: response.status.as_u16(),
        code,
        request_id: RequestId(wire.request_id),
        retryable: wire.retryable,
        retry_after: response.retry_after,
        server_date: response.server_date,
        clock_skew,
    })
}

fn unix_millis(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()
}

fn parse_http_date(value: &str) -> Option<SystemTime> {
    let parsed = DateTime::parse_from_rfc2822(value).ok()?;
    let seconds = u64::try_from(parsed.timestamp()).ok()?;
    UNIX_EPOCH.checked_add(Duration::new(seconds, parsed.timestamp_subsec_nanos()))
}

fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            parse_http_date(value).map(|at| at.duration_since(now).unwrap_or(Duration::ZERO))
        })
}

fn checked_opaque_id(value: &str) -> Result<&str, CloudClientError> {
    if !(16..=128).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(CloudClientError::InvalidInput);
    }
    Ok(value)
}

fn checked_chunk_index(index: u32) -> Result<String, CloudClientError> {
    if index >= MAX_CHUNKS_PER_UPLOAD {
        return Err(CloudClientError::InvalidInput);
    }
    Ok(index.to_string())
}

fn checked_query_value(value: &str) -> Result<&str, CloudClientError> {
    if value.len() > 512
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'-'))
    {
        return Err(CloudClientError::InvalidInput);
    }
    Ok(value)
}

fn checked_page_limit(limit: u16) -> Result<u16, CloudClientError> {
    if !(1..=100).contains(&limit) {
        return Err(CloudClientError::InvalidInput);
    }
    Ok(limit)
}

fn json_body<T: Serialize>(payload: &T) -> Result<Vec<u8>, CloudClientError> {
    let body = serde_json::to_vec(payload).map_err(|_| CloudClientError::Serialization)?;
    if body.len() > MAX_JSON_BYTES {
        return Err(CloudClientError::RequestTooLarge {
            limit: MAX_JSON_BYTES,
        });
    }
    Ok(body)
}

fn version_body() -> Result<Vec<u8>, CloudClientError> {
    json_body(&VersionRequest {
        version: PROTOCOL_VERSION,
    })
}

fn is_json_content_type(value: Option<&str>) -> bool {
    matches!(value, Some(value) if value == "application/json; charset=utf-8")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        net::SocketAddr,
        time::{Duration, UNIX_EPOCH},
    };

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        time::timeout,
    };

    use super::{
        sha256_base64url, ClockSkewCategory, CloudClient, CloudCredentials, CloudErrorCode,
        IdempotencyKey,
    };

    const VAULT_ID: &str = "vaultid123456789";
    const DEVICE_ID: &str = "deviceid12345678";
    const UPLOAD_ID: &str = "uploadid12345678";
    const SHARE_ID: &str = "shareid123456789";

    #[derive(Debug)]
    struct CapturedRequest {
        line: String,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    }

    async fn listener() -> TcpListener {
        TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener")
    }

    fn endpoint(listener: &TcpListener) -> String {
        let address: SocketAddr = listener.local_addr().expect("listener address");
        format!("http://{address}")
    }

    fn credentials<'a>(seed: &'a [u8; 32]) -> CloudCredentials<'a> {
        CloudCredentials::new(VAULT_ID, DEVICE_ID, seed).expect("test credentials")
    }

    async fn request(stream: &mut TcpStream) -> CapturedRequest {
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
        let line = lines.next().expect("request line").to_owned();
        let mut headers = BTreeMap::new();
        for header in lines.take_while(|line| !line.is_empty()) {
            let (name, value) = header.split_once(':').expect("header separator");
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
        let body_len = headers
            .get("content-length")
            .map(|value| value.parse::<usize>().expect("content length"))
            .unwrap_or(0);
        while received.len() < headers_end + body_len {
            let count = stream.read(&mut buffer).await.expect("read body");
            assert_ne!(count, 0, "request closed before body");
            received.extend_from_slice(&buffer[..count]);
        }
        CapturedRequest {
            line,
            headers,
            body: received[headers_end..headers_end + body_len].to_vec(),
        }
    }

    async fn respond(stream: &mut TcpStream, status: &str, headers: &[(&str, &str)], body: &[u8]) {
        let mut head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        stream
            .write_all(head.as_bytes())
            .await
            .expect("write headers");
        stream.write_all(body).await.expect("write body");
    }

    fn error_body(code: &str) -> Vec<u8> {
        format!("{{\"code\":\"{code}\",\"request_id\":\"requestid12345678\",\"retryable\":false}}")
            .into_bytes()
    }

    #[tokio::test]
    async fn disables_redirects_for_public_share_files() {
        let source = listener().await;
        let source_endpoint = endpoint(&source);
        let redirect_target = listener().await;
        let location = format!("{}/followed", endpoint(&redirect_target));
        let client = CloudClient::new(&source_endpoint).expect("client");
        let source_task = tokio::spawn(async move {
            let (mut stream, _) = source.accept().await.expect("accept source");
            let captured = request(&mut stream).await;
            let body = error_body("not_found");
            let headers = [
                ("Location", location.as_str()),
                ("Content-Type", "application/json; charset=utf-8"),
            ];
            respond(&mut stream, "302 Found", &headers, &body).await;
            captured
        });
        let redirected = tokio::spawn(async move {
            timeout(Duration::from_millis(250), redirect_target.accept())
                .await
                .is_ok()
        });

        assert!(client.download_share_file(SHARE_ID).await.is_err());
        let captured = source_task.await.expect("source task");
        assert_eq!(
            captured.line,
            format!("GET /v1/shares/{SHARE_ID}/file HTTP/1.1")
        );
        assert!(!redirected.await.expect("target task"));
    }

    #[tokio::test]
    async fn parses_retry_date_clock_skew_and_redacts_request_id() {
        let server = listener().await;
        let endpoint = endpoint(&server);
        let task = tokio::spawn(async move {
            let (mut stream, _) = server.accept().await.expect("accept error");
            let captured = request(&mut stream).await;
            let body = error_body("clock_skew");
            let headers = [
                ("Content-Type", "application/json; charset=utf-8"),
                ("Retry-After", "5"),
                ("Date", "Thu, 01 Jan 1970 00:00:00 GMT"),
            ];
            respond(&mut stream, "401 Unauthorized", &headers, &body).await;
            captured
        });

        let client = CloudClient::new(&endpoint).expect("client");
        let error = match client.public_share_manifest(SHARE_ID).await {
            Ok(_) => panic!("must reject error response"),
            Err(error) => error,
        };
        let captured = task.await.expect("error task");
        assert_eq!(
            captured.line,
            format!("GET /v1/shares/{SHARE_ID}/manifest HTTP/1.1")
        );
        let api = error.api_error().expect("typed error");
        assert_eq!(api.status, 401);
        assert_eq!(api.code, CloudErrorCode::ClockSkew);
        assert_eq!(api.request_id.as_str(), "requestid12345678");
        assert!(!api.retryable);
        assert_eq!(api.retry_after, Some(Duration::from_secs(5)));
        assert_eq!(api.server_date, Some(UNIX_EPOCH));
        assert_eq!(api.clock_skew, Some(ClockSkewCategory::ServerBehind));
        assert!(!format!("{error:?}").contains("requestid12345678"));
    }

    #[tokio::test]
    async fn builds_exact_chunk_route_body_and_headers() {
        let server = listener().await;
        let endpoint = endpoint(&server);
        let task = tokio::spawn(async move {
            let (mut stream, _) = server.accept().await.expect("accept chunk");
            let captured = request(&mut stream).await;
            let body = format!("{{\"upload_id\":\"{UPLOAD_ID}\",\"index\":7,\"accepted\":true}}")
                .into_bytes();
            respond(
                &mut stream,
                "200 OK",
                &[("Content-Type", "application/json; charset=utf-8")],
                &body,
            )
            .await;
            captured
        });

        let seed = [7_u8; 32];
        let key = IdempotencyKey::new("idempotencykey123").expect("key");
        let ciphertext = vec![42_u8; 28];
        let digest = sha256_base64url(&ciphertext);
        let client = CloudClient::new(&endpoint).expect("client");
        client
            .upload_chunk(&credentials(&seed), &key, UPLOAD_ID, 7, ciphertext.clone())
            .await
            .expect("chunk response");
        let captured = task.await.expect("chunk task");

        assert_eq!(
            captured.line,
            format!("PUT /v1/uploads/{UPLOAD_ID}/chunks/7 HTTP/1.1")
        );
        assert_eq!(
            captured.headers.get("content-type").map(String::as_str),
            Some("application/octet-stream")
        );
        assert_eq!(
            captured.headers.get("content-length").map(String::as_str),
            Some("28")
        );
        assert_eq!(
            captured.headers.get("x-sona-vault-id").map(String::as_str),
            Some(VAULT_ID)
        );
        assert_eq!(
            captured.headers.get("x-sona-device-id").map(String::as_str),
            Some(DEVICE_ID)
        );
        assert_eq!(
            captured
                .headers
                .get("x-sona-idempotency-key")
                .map(String::as_str),
            Some(key.as_str())
        );
        assert_eq!(captured.headers.get("x-sona-chunk-sha256"), Some(&digest));
        assert_eq!(captured.body, ciphertext);
        assert_eq!(captured.headers["x-sona-timestamp"].len(), 13);
        assert_eq!(captured.headers["x-sona-nonce"].len(), 22);
        assert_eq!(captured.headers["x-sona-signature"].len(), 86);
    }

    #[tokio::test]
    async fn caller_can_reuse_an_idempotency_key_after_lost_response() {
        let server = listener().await;
        let endpoint = endpoint(&server);
        let task = tokio::spawn(async move {
            let mut captured = Vec::new();
            for attempt in 0..2 {
                let (mut stream, _) = server.accept().await.expect("accept mutation");
                let request = request(&mut stream).await;
                if attempt == 1 {
                    let body = format!("{{\"upload_id\":\"{UPLOAD_ID}\",\"state\":\"cancelled\"}}")
                        .into_bytes();
                    respond(
                        &mut stream,
                        "200 OK",
                        &[("Content-Type", "application/json; charset=utf-8")],
                        &body,
                    )
                    .await;
                }
                captured.push(request);
            }
            captured
        });

        let seed = [9_u8; 32];
        let key = IdempotencyKey::new("idempotencykey123").expect("key");
        let client = CloudClient::new(&endpoint).expect("client");
        assert!(client
            .cancel_upload(&credentials(&seed), &key, UPLOAD_ID)
            .await
            .is_err());
        client
            .cancel_upload(&credentials(&seed), &key, UPLOAD_ID)
            .await
            .expect("idempotent replay");
        let captured = task.await.expect("mutation task");

        assert_eq!(captured[0].body, br#"{"version":1}"#);
        assert_eq!(captured[0].body, captured[1].body);
        assert_eq!(captured[0].headers["x-sona-idempotency-key"], key.as_str());
        assert_eq!(
            captured[0].headers["x-sona-idempotency-key"],
            captured[1].headers["x-sona-idempotency-key"]
        );
        assert_ne!(
            captured[0].headers["x-sona-nonce"],
            captured[1].headers["x-sona-nonce"]
        );
        assert_ne!(
            captured[0].headers["x-sona-signature"],
            captured[1].headers["x-sona-signature"]
        );
        let first = captured[0].headers["x-sona-timestamp"]
            .parse::<u64>()
            .expect("timestamp");
        let second = captured[1].headers["x-sona-timestamp"]
            .parse::<u64>()
            .expect("timestamp");
        assert!(second > first);
    }

    #[tokio::test]
    async fn caches_typed_capabilities_and_observes_success_date() {
        let server = listener().await;
        let endpoint = endpoint(&server);
        let task = tokio::spawn(async move {
            let (mut stream, _) = server.accept().await.expect("accept capabilities");
            let captured = request(&mut stream).await;
            let body = br#"{"protocol_version":1,"crypto_version":1,"request_auth":{"algorithm":"Ed25519","clock_skew_seconds":300,"nonce_retention_seconds":600},"limits":{"remote_bytes":8589934592,"chunk_bytes":4194304,"chunks_per_upload":4096,"active_uploads":16,"active_shares":32,"share_bytes":268435456,"share_expiry_seconds":2592000,"change_page":100,"snapshot_page":100}}"#;
            let headers = [
                ("Content-Type", "application/json; charset=utf-8"),
                ("Date", "Thu, 01 Jan 1970 00:00:00 GMT"),
            ];
            respond(&mut stream, "200 OK", &headers, body).await;
            captured
        });

        let seed = [11_u8; 32];
        let client = CloudClient::new(&endpoint).expect("client");
        let capabilities = client
            .capabilities(&credentials(&seed))
            .await
            .expect("capabilities");
        let captured = task.await.expect("capabilities task");
        let cached = client
            .cached_capabilities()
            .expect("cache entry")
            .capabilities;

        assert_eq!(captured.line, "GET /v1/capabilities HTTP/1.1");
        assert_eq!(capabilities.protocol_version, 1);
        assert_eq!(cached.limits.chunk_bytes, 4 * 1024 * 1024);
        assert_eq!(client.latest_server_date().expect("date").server_time_ms, 0);
    }
}
