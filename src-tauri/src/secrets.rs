use crate::settings;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_store::StoreExt;
use zeroize::Zeroizing;

pub(crate) const SECRET_SERVICE_NAME: &str = "com.aktanazat.sona";
pub(crate) const LEGACY_FORK_SECRET_SERVICE_NAME: &str = "com.aktanazat.handy-personal";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    Llm,
    Stt,
    MeetingStorage,
}

impl SecretKind {
    fn namespace(self) -> &'static str {
        match self {
            Self::Llm => "llm",
            Self::Stt => "stt",
            Self::MeetingStorage => "meeting_storage",
        }
    }
}

#[derive(Clone)]
pub(crate) struct SecretAccount {
    account: String,
}

impl SecretAccount {
    pub(crate) fn for_provider(
        kind: SecretKind,
        provider_id: &str,
    ) -> Result<Self, SecretStoreError> {
        if provider_id.is_empty()
            || !provider_id.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err(SecretStoreError::new(SecretErrorKind::Invalid));
        }

        Ok(Self {
            account: format!("{}/{}", kind.namespace(), provider_id),
        })
    }

    pub(crate) fn llm(provider_id: &str) -> Result<Self, SecretStoreError> {
        Self::for_provider(SecretKind::Llm, provider_id)
    }

    pub(crate) fn meeting_storage() -> Self {
        Self {
            account: "meeting_storage/database-key-v1".to_string(),
        }
    }

    pub(crate) fn history_storage() -> Self {
        Self {
            account: "history_storage/database-key-v1".to_string(),
        }
    }

    pub(crate) fn agent_panel_signing_seed() -> Self {
        Self {
            account: "agent_panel/signing-seed-v1".to_string(),
        }
    }
    pub(crate) fn cloud_sync_vault_root() -> Self {
        Self {
            account: "cloud_sync/vault-root-v1".to_string(),
        }
    }

    pub(crate) fn cloud_sync_signing_seed() -> Self {
        Self {
            account: "cloud_sync/signing-seed-v1".to_string(),
        }
    }

    pub(crate) fn cloud_sync_pairing_secret() -> Self {
        Self {
            account: "cloud_sync/pairing-secret-v1".to_string(),
        }
    }

    fn as_str(&self) -> &str {
        &self.account
    }
}

/// A credential value whose owned string is cleared when its last owner drops.
/// It deliberately has no formatting, serialization, cloning, or Specta traits.
pub(crate) struct SecretValue(Zeroizing<String>);

impl SecretValue {
    fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub(crate) fn expose(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) fn into_zeroizing(self) -> Zeroizing<String> {
        self.0
    }
}

/// An opaque SQLCipher key. It cannot be logged, serialized, cloned, or
/// formatted, and its bytes are cleared when the value drops.
pub(crate) struct MeetingStorageKey(Zeroizing<[u8; 32]>);

impl MeetingStorageKey {
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// An opaque SQLCipher key for the dictation history database. It is a distinct
/// type from [`MeetingStorageKey`] so one database's key can never open the
/// other by accident.
pub(crate) struct HistoryStorageKey(Zeroizing<[u8; 32]>);

impl HistoryStorageKey {
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// The agent panel's Ed25519 seed. It stays opaque to callers so neither the
/// Tauri command surface nor logging can expose signing material.
pub(crate) struct AgentPanelSigningSeed(Zeroizing<[u8; 32]>);

impl AgentPanelSigningSeed {
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// The cloud-sync cryptographic roots. They are fixed-size and zeroize on drop;
/// they deliberately have no formatting, serialization, or cloning support.
pub(crate) struct CloudSyncKeys {
    pub(crate) vault_root: Zeroizing<[u8; 32]>,
    pub(crate) signing_seed: Zeroizing<[u8; 32]>,
    pub(crate) pairing_secret: Zeroizing<[u8; 32]>,
}

fn decode_fixed_size_secret(
    stored: SecretValue,
) -> Result<Zeroizing<[u8; 32]>, SecretResolveError> {
    let encoded = stored.into_zeroizing();
    if encoded.len() != 64 {
        return Err(SecretResolveError::Store(SecretStoreError::new(
            SecretErrorKind::Corrupt,
        )));
    }

    let mut secret = Zeroizing::new([0_u8; 32]);
    hex::decode_to_slice(encoded.as_bytes(), &mut *secret)
        .map_err(|_| SecretResolveError::Store(SecretStoreError::new(SecretErrorKind::Corrupt)))?;
    Ok(secret)
}

fn decode_agent_panel_signing_seed(
    stored: SecretValue,
) -> Result<AgentPanelSigningSeed, SecretResolveError> {
    Ok(AgentPanelSigningSeed(decode_fixed_size_secret(stored)?))
}

fn decode_meeting_storage_key(
    stored: SecretValue,
) -> Result<MeetingStorageKey, SecretResolveError> {
    Ok(MeetingStorageKey(decode_fixed_size_secret(stored)?))
}

fn decode_history_storage_key(
    stored: SecretValue,
) -> Result<HistoryStorageKey, SecretResolveError> {
    Ok(HistoryStorageKey(decode_fixed_size_secret(stored)?))
}

pub(crate) enum SecretRead {
    Found(SecretValue),
    NotFound,
}

pub(crate) enum DeleteResult {
    Deleted,
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SecretErrorKind {
    Unavailable,
    Locked,
    Corrupt,
    Invalid,
    Busy,
    Backend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SecretStoreError {
    pub(crate) kind: SecretErrorKind,
}

impl SecretStoreError {
    const fn new(kind: SecretErrorKind) -> Self {
        Self { kind }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[serde(rename_all = "camelCase")]
pub struct SecretState {
    pub configured: bool,
    pub last_verified_at: Option<i64>,
    pub last_error_kind: Option<SecretErrorKind>,
}

impl SecretState {
    fn configured(last_verified_at: Option<i64>) -> Self {
        Self {
            configured: true,
            last_verified_at,
            last_error_kind: None,
        }
    }

    fn not_found() -> Self {
        Self::default()
    }

    fn failed(last_verified_at: Option<i64>, error: SecretStoreError) -> Self {
        Self {
            configured: false,
            last_verified_at,
            last_error_kind: Some(error.kind),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SecretCommandError {
    Unavailable,
    Locked,
    Corrupt,
    Invalid,
    Busy,
    Backend,
}

impl From<SecretStoreError> for SecretCommandError {
    fn from(error: SecretStoreError) -> Self {
        match error.kind {
            SecretErrorKind::Unavailable => Self::Unavailable,
            SecretErrorKind::Locked => Self::Locked,
            SecretErrorKind::Corrupt => Self::Corrupt,
            SecretErrorKind::Invalid => Self::Invalid,
            SecretErrorKind::Busy => Self::Busy,
            SecretErrorKind::Backend => Self::Backend,
        }
    }
}
#[derive(Debug)]
pub(crate) enum SecretResolveError {
    NotFound,
    Store(SecretStoreError),
}

pub(crate) trait SecretBackend: Send + Sync + 'static {
    fn read(&self, account: &str) -> Result<SecretRead, SecretStoreError>;
    fn write(&self, account: &str, secret: &str) -> Result<(), SecretStoreError>;
    fn delete(&self, account: &str) -> Result<DeleteResult, SecretStoreError>;
}

fn validate_native_32_byte_secret(secret: &SecretValue) -> Result<(), SecretStoreError> {
    if secret.expose().len() != 64 {
        return Err(SecretStoreError::new(SecretErrorKind::Corrupt));
    }

    let mut decoded = Zeroizing::new([0_u8; 32]);
    hex::decode_to_slice(secret.expose().as_bytes(), &mut *decoded)
        .map_err(|_| SecretStoreError::new(SecretErrorKind::Corrupt))
}

fn read_or_create_native_32_byte_secret(
    backend: &dyn SecretBackend,
    account: &SecretAccount,
) -> Result<SecretValue, SecretStoreError> {
    match backend.read(account.as_str())? {
        SecretRead::Found(existing) => {
            validate_native_32_byte_secret(&existing)?;
            Ok(existing)
        }
        SecretRead::NotFound => {
            let mut generated = Zeroizing::new([0_u8; 32]);
            getrandom::fill(&mut *generated)
                .map_err(|_| SecretStoreError::new(SecretErrorKind::Backend))?;
            let encoded = Zeroizing::new(hex::encode(*generated));
            backend.write(account.as_str(), encoded.as_str())?;
            match backend.read(account.as_str())? {
                SecretRead::Found(stored) if stored.expose() == encoded.as_str() => Ok(stored),
                SecretRead::Found(_) | SecretRead::NotFound => {
                    Err(SecretStoreError::new(SecretErrorKind::Corrupt))
                }
            }
        }
    }
}

fn write_and_verify_native_secret(
    backend: &dyn SecretBackend,
    account: &SecretAccount,
    secret: &Zeroizing<String>,
) -> Result<(), SecretStoreError> {
    backend.write(account.as_str(), secret.as_str())?;
    match backend.read(account.as_str())? {
        SecretRead::Found(stored) if stored.expose() == secret.as_str() => Ok(()),
        SecretRead::Found(_) | SecretRead::NotFound => {
            Err(SecretStoreError::new(SecretErrorKind::Corrupt))
        }
    }
}

struct NativeSecretBackend {
    service: String,
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
impl NativeSecretBackend {
    fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, SecretStoreError> {
        keyring::Entry::new(&self.service, account).map_err(map_keyring_error)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
impl SecretBackend for NativeSecretBackend {
    fn read(&self, account: &str) -> Result<SecretRead, SecretStoreError> {
        let entry = self.entry(account)?;
        match entry.get_password() {
            Ok(secret) => Ok(SecretRead::Found(SecretValue::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(SecretRead::NotFound),
            Err(error) => Err(map_keyring_error(error)),
        }
    }

    fn write(&self, account: &str, secret: &str) -> Result<(), SecretStoreError> {
        self.entry(account)?
            .set_password(secret)
            .map_err(map_keyring_error)
    }

    fn delete(&self, account: &str) -> Result<DeleteResult, SecretStoreError> {
        let entry = self.entry(account)?;
        match entry.delete_credential() {
            Ok(()) => Ok(DeleteResult::Deleted),
            Err(keyring::Error::NoEntry) => Ok(DeleteResult::NotFound),
            Err(error) => Err(map_keyring_error(error)),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn map_keyring_error(error: keyring::Error) -> SecretStoreError {
    match error {
        keyring::Error::NoEntry => SecretStoreError::new(SecretErrorKind::Backend),
        keyring::Error::NoStorageAccess(_) => SecretStoreError::new(SecretErrorKind::Locked),
        keyring::Error::PlatformFailure(_) => SecretStoreError::new(SecretErrorKind::Unavailable),
        keyring::Error::BadEncoding(_) | keyring::Error::Ambiguous(_) => {
            SecretStoreError::new(SecretErrorKind::Corrupt)
        }
        keyring::Error::TooLong(_, _) | keyring::Error::Invalid(_, _) => {
            SecretStoreError::new(SecretErrorKind::Invalid)
        }
        _ => SecretStoreError::new(SecretErrorKind::Backend),
    }
}

enum SecretBackendOwner {
    Native(Arc<dyn SecretBackend>),
    Disabled(SecretUnavailableReason),
}

#[derive(Clone, Copy)]
enum SecretUnavailableReason {
    PortableMode,
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    UnsupportedPlatform,
}

/// The only production credential owner. It has exactly one native backend or
/// one explicit disabled reason; it never falls back to a file, environment,
/// process memory, or a Linux kernel keyring.
pub struct SecretManager {
    backend: SecretBackendOwner,
    operation: Arc<std::sync::Mutex<()>>,
    migration_pending: AtomicBool,
}

impl SecretManager {
    pub(crate) fn native() -> Self {
        Self::native_for_service(SECRET_SERVICE_NAME)
    }

    /// Migration-only constructor for a second native service. It remains
    /// private to the backend so commands cannot target arbitrary services.
    pub(crate) fn native_for_service(service: &str) -> Self {
        if crate::portable::is_portable() {
            return Self::disabled(SecretUnavailableReason::PortableMode);
        }

        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        {
            Self::with_native_backend(Arc::new(NativeSecretBackend::new(service)))
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            let _ = service;
            Self::disabled(SecretUnavailableReason::UnsupportedPlatform)
        }
    }

    fn with_native_backend(backend: Arc<dyn SecretBackend>) -> Self {
        Self {
            backend: SecretBackendOwner::Native(backend),
            operation: Arc::new(std::sync::Mutex::new(())),
            migration_pending: AtomicBool::new(false),
        }
    }

    fn disabled(reason: SecretUnavailableReason) -> Self {
        Self {
            backend: SecretBackendOwner::Disabled(reason),
            operation: Arc::new(std::sync::Mutex::new(())),
            migration_pending: AtomicBool::new(false),
        }
    }

    fn disabled_error(&self) -> Option<SecretStoreError> {
        match &self.backend {
            SecretBackendOwner::Native(_) => None,
            SecretBackendOwner::Disabled(_) => {
                Some(SecretStoreError::new(SecretErrorKind::Unavailable))
            }
        }
    }

    fn is_portable_disabled(&self) -> bool {
        matches!(
            &self.backend,
            SecretBackendOwner::Disabled(SecretUnavailableReason::PortableMode)
        )
    }

    pub(crate) fn set_migration_pending(&self, pending: bool) {
        self.migration_pending.store(pending, Ordering::Release);
    }

    fn migration_is_pending(&self) -> bool {
        self.migration_pending.load(Ordering::Acquire)
    }

    async fn run_blocking<T, F>(&self, operation: F) -> Result<T, SecretStoreError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SecretBackend) -> Result<T, SecretStoreError> + Send + 'static,
    {
        let backend = match &self.backend {
            SecretBackendOwner::Native(backend) => Arc::clone(backend),
            SecretBackendOwner::Disabled(_) => {
                return Err(SecretStoreError::new(SecretErrorKind::Unavailable));
            }
        };
        let operation_lock = Arc::clone(&self.operation);

        tauri::async_runtime::spawn_blocking(move || {
            let _permit = operation_lock
                .lock()
                .map_err(|_| SecretStoreError::new(SecretErrorKind::Backend))?;
            let _span = crate::launch_trace::keyring_span();
            operation(backend.as_ref())
        })
        .await
        .map_err(|_| SecretStoreError::new(SecretErrorKind::Backend))?
    }

    async fn read_native(&self, account: SecretAccount) -> Result<SecretRead, SecretStoreError> {
        self.run_blocking(move |backend| backend.read(account.as_str()))
            .await
    }

    pub(crate) async fn state(
        &self,
        account: SecretAccount,
        last_verified_at: Option<i64>,
    ) -> SecretState {
        if self.migration_is_pending() {
            return SecretState::failed(
                last_verified_at,
                SecretStoreError::new(SecretErrorKind::Unavailable),
            );
        }
        if let Some(error) = self.disabled_error() {
            return SecretState::failed(last_verified_at, error);
        }

        match self.read_native(account).await {
            Ok(SecretRead::Found(value)) if value.expose().trim().is_empty() => {
                SecretState::failed(
                    last_verified_at,
                    SecretStoreError::new(SecretErrorKind::Invalid),
                )
            }
            Ok(SecretRead::Found(_)) => SecretState::configured(last_verified_at),
            Ok(SecretRead::NotFound) => SecretState::not_found(),
            Err(error) => SecretState::failed(last_verified_at, error),
        }
    }

    pub(crate) async fn replace(
        &self,
        account: SecretAccount,
        secret: Zeroizing<String>,
    ) -> Result<SecretState, SecretStoreError> {
        if self.migration_is_pending() {
            return Err(SecretStoreError::new(SecretErrorKind::Unavailable));
        }
        if let Some(error) = self.disabled_error() {
            return Err(error);
        }
        if secret.as_str().trim().is_empty() {
            return Err(SecretStoreError::new(SecretErrorKind::Invalid));
        }

        self.write_and_verify(account, secret).await?;
        Ok(SecretState::configured(None))
    }

    async fn write_and_verify(
        &self,
        account: SecretAccount,
        secret: Zeroizing<String>,
    ) -> Result<(), SecretStoreError> {
        self.run_blocking(move |backend| write_and_verify_native_secret(backend, &account, &secret))
            .await
    }

    pub(crate) async fn remove(
        &self,
        account: SecretAccount,
    ) -> Result<SecretState, SecretStoreError> {
        if self.migration_is_pending() {
            return Err(SecretStoreError::new(SecretErrorKind::Unavailable));
        }
        if let Some(error) = self.disabled_error() {
            return Err(error);
        }

        let _ = self
            .run_blocking(move |backend| backend.delete(account.as_str()))
            .await?;
        Ok(SecretState::not_found())
    }

    pub(crate) async fn resolve_optional(
        &self,
        account: SecretAccount,
    ) -> Result<SecretRead, SecretStoreError> {
        if self.migration_is_pending() {
            return Err(SecretStoreError::new(SecretErrorKind::Unavailable));
        }
        if let Some(error) = self.disabled_error() {
            return Err(error);
        }

        match self.read_native(account).await? {
            SecretRead::Found(value) if value.expose().trim().is_empty() => {
                Err(SecretStoreError::new(SecretErrorKind::Invalid))
            }
            result => Ok(result),
        }
    }

    pub(crate) async fn resolve(
        &self,
        account: SecretAccount,
    ) -> Result<SecretValue, SecretResolveError> {
        match self.resolve_optional(account).await {
            Ok(SecretRead::Found(secret)) => Ok(secret),
            Ok(SecretRead::NotFound) => Err(SecretResolveError::NotFound),
            Err(error) => Err(SecretResolveError::Store(error)),
        }
    }

    /// Resolve all cloud-sync cryptographic roots from the serialized native
    /// credential backend. A disabled native store is reported before any key
    /// material is generated, and no alternate storage is consulted.
    pub(crate) async fn cloud_sync_keys(&self) -> Result<CloudSyncKeys, SecretResolveError> {
        if self.migration_is_pending() {
            return Err(SecretResolveError::Store(SecretStoreError::new(
                SecretErrorKind::Unavailable,
            )));
        }
        if let Some(error) = self.disabled_error() {
            return Err(SecretResolveError::Store(error));
        }

        let vault_root_account = SecretAccount::cloud_sync_vault_root();
        let signing_seed_account = SecretAccount::cloud_sync_signing_seed();
        let pairing_secret_account = SecretAccount::cloud_sync_pairing_secret();
        let (vault_root, signing_seed, pairing_secret) = self
            .run_blocking(move |backend| {
                Ok((
                    read_or_create_native_32_byte_secret(backend, &vault_root_account)?,
                    read_or_create_native_32_byte_secret(backend, &signing_seed_account)?,
                    read_or_create_native_32_byte_secret(backend, &pairing_secret_account)?,
                ))
            })
            .await
            .map_err(SecretResolveError::Store)?;

        Ok(CloudSyncKeys {
            vault_root: decode_fixed_size_secret(vault_root)?,
            signing_seed: decode_fixed_size_secret(signing_seed)?,
            pairing_secret: decode_fixed_size_secret(pairing_secret)?,
        })
    }

    /// Replace only the cloud vault root for controlled recovery. Native-store
    /// availability is checked before the replacement value is encoded.
    pub(crate) async fn replace_cloud_vault_root(
        &self,
        vault_root: [u8; 32],
    ) -> Result<(), SecretResolveError> {
        self.replace_cloud_sync_secret(
            SecretAccount::cloud_sync_vault_root(),
            Zeroizing::new(vault_root),
        )
        .await
    }

    /// Replace all cloud-sync cryptographic roots in one serialized native-store
    /// operation. Each account write is read back; the native backend provides
    /// no file or memory fallback.
    #[cfg(test)]
    pub(crate) async fn replace_cloud_sync_keys(
        &self,
        keys: CloudSyncKeys,
    ) -> Result<(), SecretResolveError> {
        if self.migration_is_pending() {
            return Err(SecretResolveError::Store(SecretStoreError::new(
                SecretErrorKind::Unavailable,
            )));
        }
        if let Some(error) = self.disabled_error() {
            return Err(SecretResolveError::Store(error));
        }

        let CloudSyncKeys {
            vault_root,
            signing_seed,
            pairing_secret,
        } = keys;
        let vault_root = Zeroizing::new(hex::encode(&*vault_root));
        let signing_seed = Zeroizing::new(hex::encode(&*signing_seed));
        let pairing_secret = Zeroizing::new(hex::encode(&*pairing_secret));
        let vault_root_account = SecretAccount::cloud_sync_vault_root();
        let signing_seed_account = SecretAccount::cloud_sync_signing_seed();
        let pairing_secret_account = SecretAccount::cloud_sync_pairing_secret();

        self.run_blocking(move |backend| {
            write_and_verify_native_secret(backend, &vault_root_account, &vault_root)?;
            write_and_verify_native_secret(backend, &signing_seed_account, &signing_seed)?;
            write_and_verify_native_secret(backend, &pairing_secret_account, &pairing_secret)
        })
        .await
        .map_err(SecretResolveError::Store)
    }

    async fn replace_cloud_sync_secret(
        &self,
        account: SecretAccount,
        secret: Zeroizing<[u8; 32]>,
    ) -> Result<(), SecretResolveError> {
        if self.migration_is_pending() {
            return Err(SecretResolveError::Store(SecretStoreError::new(
                SecretErrorKind::Unavailable,
            )));
        }
        if let Some(error) = self.disabled_error() {
            return Err(SecretResolveError::Store(error));
        }

        let encoded = Zeroizing::new(hex::encode(*secret));
        self.write_and_verify(account, encoded)
            .await
            .map_err(SecretResolveError::Store)
    }

    /// Resolve one 32-byte native-store key, creating it on first use. Creation,
    /// storage, and read-back verification happen under the manager's serialized
    /// native operation lock, so concurrent first use cannot replace a key that
    /// already encrypts a database.
    async fn database_key_secret(
        &self,
        account: SecretAccount,
    ) -> Result<SecretValue, SecretResolveError> {
        if self.migration_is_pending() {
            return Err(SecretResolveError::Store(SecretStoreError::new(
                SecretErrorKind::Unavailable,
            )));
        }
        if let Some(error) = self.disabled_error() {
            return Err(SecretResolveError::Store(error));
        }

        self.run_blocking(move |backend| match backend.read(account.as_str())? {
            SecretRead::Found(existing) => Ok(existing),
            SecretRead::NotFound => {
                let mut generated = Zeroizing::new([0_u8; 32]);
                getrandom::fill(&mut *generated)
                    .map_err(|_| SecretStoreError::new(SecretErrorKind::Backend))?;
                let encoded = Zeroizing::new(hex::encode(*generated));
                backend.write(account.as_str(), encoded.as_str())?;
                match backend.read(account.as_str())? {
                    SecretRead::Found(stored) if stored.expose() == encoded.as_str() => Ok(stored),
                    SecretRead::Found(_) | SecretRead::NotFound => {
                        Err(SecretStoreError::new(SecretErrorKind::Corrupt))
                    }
                }
            }
        })
        .await
        .map_err(SecretResolveError::Store)
    }

    /// Resolve the one native-store key used to open the SQLCipher meeting
    /// database.
    pub(crate) async fn meeting_storage_key(
        &self,
    ) -> Result<MeetingStorageKey, SecretResolveError> {
        let stored = self
            .database_key_secret(SecretAccount::meeting_storage())
            .await?;
        decode_meeting_storage_key(stored)
    }

    /// Resolve the one native-store key used to open the SQLCipher dictation
    /// history database.
    pub(crate) async fn history_storage_key(
        &self,
    ) -> Result<HistoryStorageKey, SecretResolveError> {
        let stored = self
            .database_key_secret(SecretAccount::history_storage())
            .await?;
        decode_history_storage_key(stored)
    }

    /// Resolve the dedicated Ed25519 seed used by the attached agent panel.
    /// Creation, storage, and read-back verification happen under one native
    /// credential-store operation, so concurrent first use cannot replace an
    /// already provisioned public identity.
    pub(crate) async fn agent_panel_signing_seed(
        &self,
    ) -> Result<AgentPanelSigningSeed, SecretResolveError> {
        if self.migration_is_pending() {
            return Err(SecretResolveError::Store(SecretStoreError::new(
                SecretErrorKind::Unavailable,
            )));
        }
        if let Some(error) = self.disabled_error() {
            return Err(SecretResolveError::Store(error));
        }

        let account = SecretAccount::agent_panel_signing_seed();
        let stored = self
            .run_blocking(move |backend| match backend.read(account.as_str())? {
                SecretRead::Found(existing) => Ok(existing),
                SecretRead::NotFound => {
                    let mut generated = Zeroizing::new([0_u8; 32]);
                    getrandom::fill(&mut *generated)
                        .map_err(|_| SecretStoreError::new(SecretErrorKind::Backend))?;
                    let encoded = Zeroizing::new(hex::encode(*generated));
                    backend.write(account.as_str(), encoded.as_str())?;
                    match backend.read(account.as_str())? {
                        SecretRead::Found(stored) if stored.expose() == encoded.as_str() => {
                            Ok(stored)
                        }
                        SecretRead::Found(_) | SecretRead::NotFound => {
                            Err(SecretStoreError::new(SecretErrorKind::Corrupt))
                        }
                    }
                }
            })
            .await
            .map_err(SecretResolveError::Store)?;

        decode_agent_panel_signing_seed(stored)
    }

    async fn read_for_migration(
        &self,
        account: SecretAccount,
    ) -> Result<SecretRead, SecretStoreError> {
        self.read_native(account).await
    }

    async fn delete_for_migration(
        &self,
        account: SecretAccount,
    ) -> Result<DeleteResult, SecretStoreError> {
        self.run_blocking(move |backend| backend.delete(account.as_str()))
            .await
    }

    async fn verify_existing_for_migration(
        &self,
        account: SecretAccount,
        legacy_secret: &Zeroizing<String>,
    ) -> Result<(), SecretStoreError> {
        match self.read_for_migration(account).await? {
            SecretRead::Found(stored) if stored.expose() == legacy_secret.as_str() => Ok(()),
            SecretRead::Found(_) | SecretRead::NotFound => {
                Err(SecretStoreError::new(SecretErrorKind::Corrupt))
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn with_backend(backend: Arc<dyn SecretBackend>) -> Self {
        Self::with_native_backend(backend)
    }
}

/// Result of moving one account between two native credential services.
/// A failed read or write never deletes the source value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceAccountMigration {
    Moved,
    AlreadyMoved,
    NotFound,
    NeedsReentry(SecretErrorKind),
}

/// Move one account by write-read-verify-delete. The caller owns the durable
/// receipt; this routine deliberately leaves both services intact on failure.
pub(crate) async fn migrate_service_account(
    source: &SecretManager,
    destination: &SecretManager,
    account: SecretAccount,
) -> ServiceAccountMigration {
    let secret = match source.read_for_migration(account.clone()).await {
        Ok(SecretRead::Found(value)) => value.into_zeroizing(),
        Ok(SecretRead::NotFound) => {
            return match destination.read_for_migration(account).await {
                Ok(SecretRead::Found(_)) => ServiceAccountMigration::AlreadyMoved,
                Ok(SecretRead::NotFound) => ServiceAccountMigration::NotFound,
                Err(error) => ServiceAccountMigration::NeedsReentry(error.kind),
            };
        }
        Err(error) => return ServiceAccountMigration::NeedsReentry(error.kind),
    };

    match destination.read_for_migration(account.clone()).await {
        Ok(SecretRead::NotFound) => {
            if let Err(error) = destination.write_and_verify(account.clone(), secret).await {
                return ServiceAccountMigration::NeedsReentry(error.kind);
            }
        }
        Ok(SecretRead::Found(existing)) if existing.expose() == secret.as_str() => {
            if let Err(error) = destination
                .verify_existing_for_migration(account.clone(), &secret)
                .await
            {
                return ServiceAccountMigration::NeedsReentry(error.kind);
            }
        }
        Ok(SecretRead::Found(_)) => {
            return ServiceAccountMigration::NeedsReentry(SecretErrorKind::Corrupt);
        }
        Err(error) => return ServiceAccountMigration::NeedsReentry(error.kind),
    }

    match source.delete_for_migration(account).await {
        Ok(DeleteResult::Deleted | DeleteResult::NotFound) => ServiceAccountMigration::Moved,
        Err(error) => ServiceAccountMigration::NeedsReentry(error.kind),
    }
}

fn cloud_provider_for_id(
    settings: &settings::AppSettings,
    provider_id: &str,
) -> Option<crate::modes::CloudSttProvider> {
    settings
        .cloud_stt_providers
        .iter()
        .find(|provider| provider.provider.id() == provider_id)
        .map(|provider| provider.provider)
}

fn persisted_provider_state(
    settings: &settings::AppSettings,
    kind: SecretKind,
    provider_id: &str,
) -> SecretState {
    match kind {
        SecretKind::Llm => settings
            .post_process_secret_states
            .get(provider_id)
            .cloned()
            .unwrap_or_default(),
        SecretKind::Stt => cloud_provider_for_id(settings, provider_id)
            .and_then(|provider| settings.cloud_stt_provider(provider))
            .map(|provider| provider.secret_state.clone())
            .unwrap_or_default(),
        SecretKind::MeetingStorage => SecretState::default(),
    }
}

fn persist_provider_state(
    app: &AppHandle,
    kind: SecretKind,
    provider_id: String,
    state: SecretState,
) {
    // Nothing in this function's shape can report a store failure; the settings seam logs it.
    let _ = settings::update_settings(app, |settings| match kind {
        SecretKind::Llm => {
            settings
                .post_process_secret_states
                .insert(provider_id, state);
        }
        SecretKind::Stt => {
            if let Some(provider) = cloud_provider_for_id(settings, &provider_id)
                .and_then(|provider| settings.cloud_stt_provider_mut(provider))
            {
                provider.secret_state = state;
            }
        }
        SecretKind::MeetingStorage => {}
    });
}

fn provider_account_for_command(
    settings: &settings::AppSettings,
    kind: SecretKind,
    provider_id: &str,
) -> Result<SecretAccount, SecretCommandError> {
    match kind {
        SecretKind::Llm => {
            if provider_id == settings::APPLE_INTELLIGENCE_PROVIDER_ID
                || settings.post_process_provider(provider_id).is_none()
            {
                return Err(SecretCommandError::Invalid);
            }
        }
        SecretKind::Stt => {
            if cloud_provider_for_id(settings, provider_id).is_none() {
                return Err(SecretCommandError::Invalid);
            }
        }
        SecretKind::MeetingStorage => return Err(SecretCommandError::Invalid),
    }

    SecretAccount::for_provider(kind, provider_id).map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn get_provider_secret_state(
    app: AppHandle,
    secrets: State<'_, Arc<SecretManager>>,
    kind: SecretKind,
    provider_id: String,
) -> Result<SecretState, SecretCommandError> {
    let settings = settings::get_settings(&app);
    let account = provider_account_for_command(&settings, kind, &provider_id)?;
    let last_verified_at = persisted_provider_state(&settings, kind, &provider_id).last_verified_at;
    let state = secrets.state(account, last_verified_at).await;
    persist_provider_state(&app, kind, provider_id, state.clone());
    Ok(state)
}

#[tauri::command]
#[specta::specta]
pub async fn set_provider_secret(
    app: AppHandle,
    secrets: State<'_, Arc<SecretManager>>,
    kind: SecretKind,
    provider_id: String,
    secret: String,
) -> Result<SecretState, SecretCommandError> {
    let secret = Zeroizing::new(secret);
    let settings = settings::get_settings(&app);
    let account = provider_account_for_command(&settings, kind, &provider_id)?;
    let previous = persisted_provider_state(&settings, kind, &provider_id);

    match secrets.replace(account, secret).await {
        Ok(state) => {
            persist_provider_state(&app, kind, provider_id, state.clone());
            Ok(state)
        }
        Err(error) => {
            persist_provider_state(
                &app,
                kind,
                provider_id,
                SecretState::failed(previous.last_verified_at, error),
            );
            Err(error.into())
        }
    }
}

#[tauri::command]
#[specta::specta]
pub async fn delete_provider_secret(
    app: AppHandle,
    secrets: State<'_, Arc<SecretManager>>,
    kind: SecretKind,
    provider_id: String,
) -> Result<SecretState, SecretCommandError> {
    let settings = settings::get_settings(&app);
    let account = provider_account_for_command(&settings, kind, &provider_id)?;
    let previous = persisted_provider_state(&settings, kind, &provider_id);

    match secrets.remove(account).await {
        Ok(state) => {
            persist_provider_state(&app, kind, provider_id, state.clone());
            Ok(state)
        }
        Err(error) => {
            persist_provider_state(
                &app,
                kind,
                provider_id,
                SecretState::failed(previous.last_verified_at, error),
            );
            Err(error.into())
        }
    }
}
/// Typed result for the explicit, user-triggered STT credential handshake.
/// This deliberately keeps provider failures separate from native-store errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SttSecretVerificationError {
    NotConfigured,
    ConsentRequired,
    Unavailable,
    Locked,
    Corrupt,
    Invalid,
    Busy,
    Backend,
    #[cfg(feature = "cloud-realtime")]
    Authentication,
    #[cfg(feature = "cloud-realtime")]
    Quota,
    #[cfg(feature = "cloud-realtime")]
    Network,
    #[cfg(feature = "cloud-realtime")]
    Protocol,
}

impl From<SecretStoreError> for SttSecretVerificationError {
    fn from(error: SecretStoreError) -> Self {
        match error.kind {
            SecretErrorKind::Unavailable => Self::Unavailable,
            SecretErrorKind::Locked => Self::Locked,
            SecretErrorKind::Corrupt => Self::Corrupt,
            SecretErrorKind::Invalid => Self::Invalid,
            SecretErrorKind::Busy => Self::Busy,
            SecretErrorKind::Backend => Self::Backend,
        }
    }
}

pub(crate) async fn resolve_stt_secret(
    secrets: &SecretManager,
    provider: crate::modes::CloudSttProvider,
) -> Result<Zeroizing<String>, SttSecretVerificationError> {
    let account = SecretAccount::for_provider(SecretKind::Stt, provider.id())
        .map_err(SttSecretVerificationError::from)?;
    match secrets.resolve(account).await {
        Ok(secret) => Ok(secret.into_zeroizing()),
        Err(SecretResolveError::NotFound) => Err(SttSecretVerificationError::NotConfigured),
        Err(SecretResolveError::Store(error)) => Err(error.into()),
    }
}

trait SttSecretVerifier: Send + Sync {
    fn verify<'a>(
        &'a self,
        provider: crate::modes::CloudSttProvider,
        api_key: Zeroizing<String>,
    ) -> Pin<Box<dyn Future<Output = Result<(), SttSecretVerificationError>> + Send + 'a>>;
}

struct DirectSttSecretVerifier;

#[cfg(feature = "cloud-realtime")]
impl SttSecretVerifier for DirectSttSecretVerifier {
    fn verify<'a>(
        &'a self,
        provider: crate::modes::CloudSttProvider,
        api_key: Zeroizing<String>,
    ) -> Pin<Box<dyn Future<Output = Result<(), SttSecretVerificationError>> + Send + 'a>> {
        use crate::cloud_stt::{CloudError, CloudProvider, CloudRunConfig, CloudSession};

        let provider = match provider {
            crate::modes::CloudSttProvider::DeepgramNova3 => CloudProvider::DeepgramNova3,
            crate::modes::CloudSttProvider::ElevenLabsScribeV2 => CloudProvider::ElevenLabsScribeV2,
        };
        let config = CloudRunConfig::new(provider, None, Vec::new(), false);
        Box::pin(async move {
            CloudSession::connect(config, api_key)
                .await
                .map(|_| ())
                .map_err(|error| match error {
                    CloudError::Authentication => SttSecretVerificationError::Authentication,
                    CloudError::Quota => SttSecretVerificationError::Quota,
                    CloudError::Network | CloudError::Disconnected => {
                        SttSecretVerificationError::Network
                    }
                    CloudError::Protocol
                    | CloudError::Backpressure
                    | CloudError::AudioFrameTooLarge
                    | CloudError::Finalized => SttSecretVerificationError::Protocol,
                })
        })
    }
}

#[cfg(not(feature = "cloud-realtime"))]
impl SttSecretVerifier for DirectSttSecretVerifier {
    fn verify<'a>(
        &'a self,
        _provider: crate::modes::CloudSttProvider,
        _api_key: Zeroizing<String>,
    ) -> Pin<Box<dyn Future<Output = Result<(), SttSecretVerificationError>> + Send + 'a>> {
        Box::pin(async { Err(SttSecretVerificationError::Unavailable) })
    }
}

async fn verify_stt_secret_for_settings(
    settings: &settings::AppSettings,
    secrets: &SecretManager,
    provider: crate::modes::CloudSttProvider,
    verifier: &dyn SttSecretVerifier,
) -> Result<(), SttSecretVerificationError> {
    let provider_settings = settings
        .cloud_stt_provider(provider)
        .ok_or(SttSecretVerificationError::NotConfigured)?;
    if !provider_settings.has_current_consent() {
        return Err(SttSecretVerificationError::ConsentRequired);
    }
    let api_key = resolve_stt_secret(secrets, provider).await?;
    // This opens exactly one authenticated provider handshake. No audio frame,
    // keepalive, or background retry is sent, and test callers replace verifier.
    verifier.verify(provider, api_key).await
}
async fn verify_stt_secret_with(
    app: &AppHandle,
    secrets: &SecretManager,
    provider: crate::modes::CloudSttProvider,
    verifier: &dyn SttSecretVerifier,
) -> Result<SecretState, SttSecretVerificationError> {
    let settings = settings::get_settings(app);
    verify_stt_secret_for_settings(&settings, secrets, provider, verifier).await?;
    let state = SecretState::configured(Some(chrono::Utc::now().timestamp_millis()));
    persist_provider_state(
        app,
        SecretKind::Stt,
        provider.id().to_string(),
        state.clone(),
    );
    Ok(state)
}

#[tauri::command]
#[specta::specta]
pub async fn verify_stt_provider_secret(
    app: AppHandle,
    secrets: State<'_, Arc<SecretManager>>,
    provider: crate::modes::CloudSttProvider,
) -> Result<SecretState, SttSecretVerificationError> {
    verify_stt_secret_with(
        &app,
        secrets.inner().as_ref(),
        provider,
        &DirectSttSecretVerifier,
    )
    .await
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LegacySecretMigrationStatus {
    Complete {
        migrated: u32,
    },
    Pending {
        remaining: u32,
        reason: SecretErrorKind,
    },
    Conflict {
        provider_id: String,
    },
    PortableBlocked {
        remaining: u32,
    },
}
/// Result of moving legacy LLM credentials from a separate upstream settings
/// document. The source document is changed only in memory, so upstream bytes
/// remain intact and a later import can resume after an interruption.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UpstreamSecretImportStatus {
    Complete {
        migrated_provider_ids: Vec<String>,
    },
    Pending {
        remaining: u32,
        reason: SecretErrorKind,
    },
    Conflict {
        provider_id: String,
    },
    PortableBlocked {
        remaining: u32,
    },
}

/// Move legacy credentials without placing them in Sona Personal's settings
/// store. Every value is written to the native store and read back exactly
/// before its in-memory source entry is removed. A crash can leave a duplicate
/// in the original source and native store, but cannot lose a credential.
pub(crate) async fn migrate_upstream_legacy_llm_secrets(
    raw_settings: &mut serde_json::Value,
    secrets: &SecretManager,
    known_provider_ids: &HashSet<String>,
) -> UpstreamSecretImportStatus {
    let entries = legacy_provider_entries_value(raw_settings);
    let remaining = u32::try_from(entries.len()).unwrap_or(u32::MAX);

    if entries.is_empty() {
        remove_legacy_provider_field(raw_settings);
        return UpstreamSecretImportStatus::Complete {
            migrated_provider_ids: Vec::new(),
        };
    }
    if secrets.migration_is_pending() {
        return UpstreamSecretImportStatus::Pending {
            remaining,
            reason: SecretErrorKind::Unavailable,
        };
    }
    if secrets.is_portable_disabled() {
        return UpstreamSecretImportStatus::PortableBlocked { remaining };
    }

    let mut migrated_provider_ids = Vec::with_capacity(entries.len());
    for (index, (provider_id, legacy_secret)) in entries.into_iter().enumerate() {
        let remaining_for_entry =
            remaining.saturating_sub(u32::try_from(index).unwrap_or(u32::MAX));
        if !known_provider_ids.contains(&provider_id) {
            return UpstreamSecretImportStatus::Pending {
                remaining: remaining_for_entry,
                reason: SecretErrorKind::Invalid,
            };
        }
        let account = match SecretAccount::llm(&provider_id) {
            Ok(account) => account,
            Err(error) => {
                return UpstreamSecretImportStatus::Pending {
                    remaining: remaining_for_entry,
                    reason: error.kind,
                };
            }
        };

        match secrets.read_for_migration(account.clone()).await {
            Ok(SecretRead::NotFound) => {
                if let Err(error) = secrets.write_and_verify(account, legacy_secret).await {
                    return UpstreamSecretImportStatus::Pending {
                        remaining: remaining_for_entry,
                        reason: error.kind,
                    };
                }
            }
            Ok(SecretRead::Found(existing)) if existing.expose() == legacy_secret.as_str() => {
                if let Err(error) = secrets
                    .verify_existing_for_migration(account, &legacy_secret)
                    .await
                {
                    return UpstreamSecretImportStatus::Pending {
                        remaining: remaining_for_entry,
                        reason: error.kind,
                    };
                }
            }
            Ok(SecretRead::Found(_)) => {
                return UpstreamSecretImportStatus::Conflict { provider_id };
            }
            Err(error) => {
                return UpstreamSecretImportStatus::Pending {
                    remaining: remaining_for_entry,
                    reason: error.kind,
                };
            }
        }

        remove_legacy_provider_entry_in_place(raw_settings, &provider_id);
        migrated_provider_ids.push(provider_id);
    }

    remove_legacy_provider_field(raw_settings);
    UpstreamSecretImportStatus::Complete {
        migrated_provider_ids,
    }
}

#[derive(Clone)]
struct RawSettings(serde_json::Value);

trait LegacySettingsJournal {
    fn settings(&self) -> Option<RawSettings>;
    fn remove_provider_entry(&self, provider_id: &str) -> Result<(), ()>;
    fn finalize(&self) -> Result<(), ()>;
}

struct TauriSettingsJournal {
    app: AppHandle,
}

impl LegacySettingsJournal for TauriSettingsJournal {
    fn settings(&self) -> Option<RawSettings> {
        settings::raw_settings_value(&self.app).map(RawSettings)
    }

    fn remove_provider_entry(&self, provider_id: &str) -> Result<(), ()> {
        settings::mutate_raw_settings_value(&self.app, |raw_settings| {
            remove_legacy_provider_entry_in_place(raw_settings, provider_id);
        })
        .map_err(|_| ())
    }

    fn finalize(&self) -> Result<(), ()> {
        match settings::mutate_raw_settings_value(&self.app, |raw_settings| {
            let Some(object) = raw_settings.as_object_mut() else {
                return false;
            };
            object.remove("post_process_api_keys");
            object.insert(
                "settings_schema_version".to_string(),
                serde_json::Value::from(settings::CURRENT_SETTINGS_SCHEMA_VERSION),
            );
            true
        }) {
            Ok(true) => Ok(()),
            Ok(false) | Err(_) => Err(()),
        }
    }
}

/// Read the old JSON field directly because the public settings type intentionally
/// no longer deserializes credential text. Each durable journal update removes one
/// value only after native write and exact readback have completed.
pub(crate) async fn migrate_legacy_provider_secrets(
    app: &AppHandle,
    secrets: Arc<SecretManager>,
) -> LegacySecretMigrationStatus {
    let known_provider_ids: HashSet<String> = settings::get_settings(app)
        .post_process_providers
        .into_iter()
        .map(|provider| provider.id)
        .collect();
    if app
        .store(crate::portable::store_path(settings::SETTINGS_STORE_PATH))
        .is_err()
    {
        let status = LegacySecretMigrationStatus::Pending {
            remaining: 0,
            reason: SecretErrorKind::Backend,
        };
        secrets.set_migration_pending(true);
        return status;
    }
    let journal = TauriSettingsJournal { app: app.clone() };
    let status = migrate_journal(&journal, &secrets, |provider_id| {
        known_provider_ids.contains(provider_id)
    })
    .await;
    secrets.set_migration_pending(!matches!(
        status,
        LegacySecretMigrationStatus::Complete { .. }
    ));
    status
}

async fn migrate_journal<J, F>(
    journal: &J,
    secrets: &SecretManager,
    provider_exists: F,
) -> LegacySecretMigrationStatus
where
    J: LegacySettingsJournal,
    F: Fn(&str) -> bool,
{
    let Some(raw_settings) = journal.settings() else {
        return LegacySecretMigrationStatus::Complete { migrated: 0 };
    };
    let entries = legacy_provider_entries(&raw_settings);
    let remaining = u32::try_from(entries.len()).unwrap_or(u32::MAX);

    if entries.is_empty() {
        return finalize_legacy_schema(journal, 0);
    }
    if secrets.is_portable_disabled() {
        return LegacySecretMigrationStatus::PortableBlocked { remaining };
    }

    let mut migrated = 0_u32;
    for (index, (provider_id, legacy_secret)) in entries.into_iter().enumerate() {
        let remaining_for_entry =
            remaining.saturating_sub(u32::try_from(index).unwrap_or(u32::MAX));
        if !provider_exists(&provider_id) {
            return LegacySecretMigrationStatus::Pending {
                remaining: remaining_for_entry,
                reason: SecretErrorKind::Invalid,
            };
        }
        let account = match SecretAccount::llm(&provider_id) {
            Ok(account) => account,
            Err(error) => {
                return LegacySecretMigrationStatus::Pending {
                    remaining: remaining_for_entry,
                    reason: error.kind,
                };
            }
        };

        match secrets.read_for_migration(account.clone()).await {
            Ok(SecretRead::NotFound) => {
                if let Err(error) = secrets
                    .write_and_verify(account.clone(), legacy_secret)
                    .await
                {
                    return LegacySecretMigrationStatus::Pending {
                        remaining: remaining_for_entry,
                        reason: error.kind,
                    };
                }
            }
            Ok(SecretRead::Found(existing)) if existing.expose() == legacy_secret.as_str() => {
                if let Err(error) = secrets
                    .verify_existing_for_migration(account.clone(), &legacy_secret)
                    .await
                {
                    return LegacySecretMigrationStatus::Pending {
                        remaining: remaining_for_entry,
                        reason: error.kind,
                    };
                }
            }
            Ok(SecretRead::Found(_)) => {
                return LegacySecretMigrationStatus::Conflict { provider_id };
            }
            Err(error) => {
                return LegacySecretMigrationStatus::Pending {
                    remaining: remaining_for_entry,
                    reason: error.kind,
                };
            }
        }

        if journal.remove_provider_entry(&provider_id).is_err() {
            return LegacySecretMigrationStatus::Pending {
                remaining: remaining_for_entry,
                reason: SecretErrorKind::Backend,
            };
        }
        migrated = migrated.saturating_add(1);
    }

    finalize_legacy_schema(journal, migrated)
}

fn finalize_legacy_schema<J: LegacySettingsJournal>(
    journal: &J,
    migrated: u32,
) -> LegacySecretMigrationStatus {
    if journal.finalize().is_err() {
        return LegacySecretMigrationStatus::Pending {
            remaining: 0,
            reason: SecretErrorKind::Backend,
        };
    }
    LegacySecretMigrationStatus::Complete { migrated }
}

fn legacy_provider_entries(raw_settings: &RawSettings) -> Vec<(String, Zeroizing<String>)> {
    raw_settings
        .0
        .get("post_process_api_keys")
        .and_then(serde_json::Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(provider_id, value)| {
                    value
                        .as_str()
                        .filter(|secret| !secret.is_empty())
                        .map(|secret| (provider_id.clone(), Zeroizing::new(secret.to_string())))
                })
                .collect()
        })
        .unwrap_or_default()
}
fn legacy_provider_entries_value(
    raw_settings: &serde_json::Value,
) -> Vec<(String, Zeroizing<String>)> {
    let mut entries = raw_settings
        .get("post_process_api_keys")
        .and_then(serde_json::Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(provider_id, value)| {
                    value
                        .as_str()
                        .filter(|secret| !secret.is_empty())
                        .map(|secret| (provider_id.clone(), Zeroizing::new(secret.to_string())))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

fn remove_legacy_provider_entry_in_place(raw_settings: &mut serde_json::Value, provider_id: &str) {
    if let Some(entries) = raw_settings
        .get_mut("post_process_api_keys")
        .and_then(serde_json::Value::as_object_mut)
    {
        entries.remove(provider_id);
    }
}

fn remove_legacy_provider_field(raw_settings: &mut serde_json::Value) {
    if let Some(object) = raw_settings.as_object_mut() {
        object.remove("post_process_api_keys");
    }
}

#[cfg(test)]
fn test_lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
pub(crate) struct MemorySecretBackend {
    values: std::sync::Mutex<std::collections::HashMap<String, Zeroizing<String>>>,
    failures: std::sync::Mutex<std::collections::VecDeque<SecretStoreError>>,
    active_operations: std::sync::atomic::AtomicUsize,
    max_active_operations: std::sync::atomic::AtomicUsize,
    operations: std::sync::atomic::AtomicUsize,
    delay: std::sync::Mutex<Option<std::time::Duration>>,
}

#[cfg(test)]
impl MemorySecretBackend {
    pub(crate) fn new() -> Self {
        Self {
            values: std::sync::Mutex::new(std::collections::HashMap::new()),
            failures: std::sync::Mutex::new(std::collections::VecDeque::new()),
            active_operations: std::sync::atomic::AtomicUsize::new(0),
            max_active_operations: std::sync::atomic::AtomicUsize::new(0),
            operations: std::sync::atomic::AtomicUsize::new(0),
            delay: std::sync::Mutex::new(None),
        }
    }

    pub(crate) fn insert(&self, account: &str, value: &str) {
        test_lock(&self.values).insert(account.to_string(), Zeroizing::new(value.to_string()));
    }

    pub(crate) fn has(&self, account: &str) -> bool {
        test_lock(&self.values).contains_key(account)
    }

    pub(crate) fn fail_next(&self, kind: SecretErrorKind) {
        test_lock(&self.failures).push_back(SecretStoreError::new(kind));
    }

    pub(crate) fn set_delay(&self, delay: std::time::Duration) {
        *test_lock(&self.delay) = Some(delay);
    }

    pub(crate) fn max_active_operations(&self) -> usize {
        self.max_active_operations.load(Ordering::Acquire)
    }
    pub(crate) fn operation_count(&self) -> usize {
        self.operations.load(Ordering::Acquire)
    }

    fn enter(&self) -> Result<(), SecretStoreError> {
        self.operations.fetch_add(1, Ordering::AcqRel);
        let active = self.active_operations.fetch_add(1, Ordering::AcqRel) + 1;
        self.max_active_operations
            .fetch_max(active, Ordering::AcqRel);
        if let Some(delay) = *test_lock(&self.delay) {
            std::thread::sleep(delay);
        }
        let failure = test_lock(&self.failures).pop_front();
        self.active_operations.fetch_sub(1, Ordering::AcqRel);
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
impl SecretBackend for MemorySecretBackend {
    fn read(&self, account: &str) -> Result<SecretRead, SecretStoreError> {
        self.enter()?;
        match test_lock(&self.values).get(account) {
            Some(value) => Ok(SecretRead::Found(SecretValue::new(value.to_string()))),
            None => Ok(SecretRead::NotFound),
        }
    }

    fn write(&self, account: &str, secret: &str) -> Result<(), SecretStoreError> {
        self.enter()?;
        test_lock(&self.values).insert(account.to_string(), Zeroizing::new(secret.to_string()));
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<DeleteResult, SecretStoreError> {
        self.enter()?;
        if test_lock(&self.values).remove(account).is_some() {
            Ok(DeleteResult::Deleted)
        } else {
            Ok(DeleteResult::NotFound)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use specta_typescript::{BigIntExportBehavior, Typescript};
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;

    struct MemorySettingsJournal {
        stored: Mutex<RawSettings>,
        fail_next_save: AtomicBool,
        theme_before_remove: Mutex<Option<String>>,
    }

    impl MemorySettingsJournal {
        fn with_legacy_secret(provider_id: &str, secret: &str) -> Self {
            Self {
                stored: Mutex::new(RawSettings(serde_json::json!({
                    "settings_schema_version": 5,
                    "post_process_api_keys": { provider_id: secret },
                }))),
                fail_next_save: AtomicBool::new(false),
                theme_before_remove: Mutex::new(None),
            }
        }

        fn with_two_legacy_secrets() -> Self {
            Self {
                stored: Mutex::new(RawSettings(serde_json::json!({
                    "settings_schema_version": 5,
                    "post_process_api_keys": {
                        "anthropic": "anthropic-secret",
                        "openai": "openai-secret",
                    },
                }))),
                fail_next_save: AtomicBool::new(false),
                theme_before_remove: Mutex::new(None),
            }
        }

        fn with_empty_legacy_field() -> Self {
            Self {
                stored: Mutex::new(RawSettings(serde_json::json!({
                    "settings_schema_version": 5,
                    "post_process_api_keys": {},
                }))),
                fail_next_save: AtomicBool::new(false),
                theme_before_remove: Mutex::new(None),
            }
        }

        fn fail_save_once(&self) {
            self.fail_next_save.store(true, Ordering::Release);
        }

        fn mutate_theme_before_next_remove(&self, theme: &str) {
            *test_lock(&self.theme_before_remove) = Some(theme.to_string());
        }

        fn has_no_legacy_field(&self) -> bool {
            test_lock(&self.stored)
                .0
                .get("post_process_api_keys")
                .is_none()
        }

        fn has_legacy_secret(&self, provider_id: &str, secret: &str) -> bool {
            test_lock(&self.stored)
                .0
                .get("post_process_api_keys")
                .and_then(serde_json::Value::as_object)
                .and_then(|entries| entries.get(provider_id))
                .and_then(serde_json::Value::as_str)
                == Some(secret)
        }

        fn schema_version(&self) -> Option<u64> {
            test_lock(&self.stored)
                .0
                .get("settings_schema_version")
                .and_then(serde_json::Value::as_u64)
        }

        fn theme(&self) -> Option<String> {
            test_lock(&self.stored)
                .0
                .get("theme")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        }
    }

    impl LegacySettingsJournal for MemorySettingsJournal {
        fn settings(&self) -> Option<RawSettings> {
            Some(test_lock(&self.stored).clone())
        }

        fn remove_provider_entry(&self, provider_id: &str) -> Result<(), ()> {
            let mut stored = test_lock(&self.stored);
            if let Some(theme) = test_lock(&self.theme_before_remove).take() {
                stored.0["theme"] = serde_json::Value::String(theme);
            }
            remove_legacy_provider_entry_in_place(&mut stored.0, provider_id);
            if self.fail_next_save.swap(false, Ordering::AcqRel) {
                return Err(());
            }
            Ok(())
        }

        fn finalize(&self) -> Result<(), ()> {
            let mut stored = test_lock(&self.stored);
            let Some(object) = stored.0.as_object_mut() else {
                return Err(());
            };
            object.remove("post_process_api_keys");
            object.insert(
                "settings_schema_version".to_string(),
                serde_json::Value::from(settings::CURRENT_SETTINGS_SCHEMA_VERSION),
            );
            if self.fail_next_save.swap(false, Ordering::AcqRel) {
                return Err(());
            }
            Ok(())
        }
    }

    fn manager() -> (Arc<SecretManager>, Arc<MemorySecretBackend>) {
        let backend = Arc::new(MemorySecretBackend::new());
        let manager = Arc::new(SecretManager::with_backend(backend.clone()));
        (manager, backend)
    }

    #[test]
    fn set_read_state_and_remove_are_keychain_semantics() {
        let (manager, _) = manager();
        let account = SecretAccount::llm("openai").unwrap();
        let state = tauri::async_runtime::block_on(manager.state(account.clone(), None));
        assert_eq!(state, SecretState::default());

        let state = tauri::async_runtime::block_on(
            manager.replace(account.clone(), Zeroizing::new("secret-value".to_string())),
        )
        .unwrap();
        assert!(state.configured);
        assert_eq!(state.last_verified_at, None);

        let secret = tauri::async_runtime::block_on(manager.resolve(account.clone())).unwrap();
        assert_eq!(secret.expose(), "secret-value");
        drop(secret);

        let state = tauri::async_runtime::block_on(manager.remove(account.clone())).unwrap();
        assert_eq!(state, SecretState::default());
        assert!(matches!(
            tauri::async_runtime::block_on(manager.resolve(account)),
            Err(SecretResolveError::NotFound)
        ));
    }

    #[test]
    fn state_maps_locked_and_unavailable_without_backend_details() {
        let (manager, backend) = manager();
        let account = SecretAccount::llm("openai").unwrap();
        backend.fail_next(SecretErrorKind::Locked);
        let locked = tauri::async_runtime::block_on(manager.state(account.clone(), Some(42)));
        assert!(!locked.configured);
        assert_eq!(locked.last_verified_at, Some(42));
        assert_eq!(locked.last_error_kind, Some(SecretErrorKind::Locked));

        backend.fail_next(SecretErrorKind::Unavailable);
        let unavailable = tauri::async_runtime::block_on(manager.state(account, None));
        assert_eq!(
            unavailable.last_error_kind,
            Some(SecretErrorKind::Unavailable)
        );
    }

    #[test]
    fn state_keeps_all_safe_error_kinds_without_backend_details() {
        for kind in [
            SecretErrorKind::Corrupt,
            SecretErrorKind::Invalid,
            SecretErrorKind::Busy,
            SecretErrorKind::Backend,
        ] {
            let (manager, backend) = manager();
            backend.fail_next(kind);
            let state = tauri::async_runtime::block_on(
                manager.state(SecretAccount::llm("openai").unwrap(), None),
            );
            assert_eq!(state.last_error_kind, Some(kind));
        }
    }

    #[test]
    fn externally_deleted_credential_is_not_trusted_from_persisted_state() {
        let (manager, backend) = manager();
        let account = SecretAccount::llm("openai").unwrap();
        tauri::async_runtime::block_on(
            manager.replace(account.clone(), Zeroizing::new("secret-value".to_string())),
        )
        .unwrap();
        test_lock(&backend.values).remove(account.as_str());

        let state = tauri::async_runtime::block_on(manager.state(account, Some(42)));
        assert_eq!(state, SecretState::default());
    }

    #[test]
    fn pending_migration_blocks_network_secret_resolution() {
        let (manager, _) = manager();
        manager.set_migration_pending(true);
        assert!(matches!(
            tauri::async_runtime::block_on(manager.resolve(SecretAccount::llm("openai").unwrap())),
            Err(SecretResolveError::Store(SecretStoreError {
                kind: SecretErrorKind::Unavailable,
            }))
        ));
    }

    #[test]
    fn portable_manager_never_calls_a_backend() {
        let manager = SecretManager::disabled(SecretUnavailableReason::PortableMode);
        let state = tauri::async_runtime::block_on(
            manager.state(SecretAccount::llm("openai").unwrap(), None),
        );
        assert!(!state.configured);
        assert_eq!(state.last_error_kind, Some(SecretErrorKind::Unavailable));
    }

    #[test]
    fn meeting_storage_key_is_keychain_backed_and_stable() {
        let (manager, backend) = manager();

        let first = tauri::async_runtime::block_on(manager.meeting_storage_key()).unwrap();
        let second = tauri::async_runtime::block_on(manager.meeting_storage_key()).unwrap();

        assert_eq!(first.as_bytes(), second.as_bytes());
        assert!(backend.has("meeting_storage/database-key-v1"));
    }

    #[test]
    fn meeting_storage_key_rejects_invalid_native_value() {
        let (manager, backend) = manager();
        backend.insert("meeting_storage/database-key-v1", "not-a-32-byte-key");

        assert!(matches!(
            tauri::async_runtime::block_on(manager.meeting_storage_key()),
            Err(SecretResolveError::Store(SecretStoreError {
                kind: SecretErrorKind::Corrupt,
            }))
        ));
    }

    #[test]
    fn portable_manager_fails_closed_for_meeting_storage_key() {
        let manager = SecretManager::disabled(SecretUnavailableReason::PortableMode);

        assert!(matches!(
            tauri::async_runtime::block_on(manager.meeting_storage_key()),
            Err(SecretResolveError::Store(SecretStoreError {
                kind: SecretErrorKind::Unavailable,
            }))
        ));
    }

    #[test]
    fn cloud_sync_keys_are_keychain_backed_stable_and_namespaced() {
        let (manager, backend) = manager();

        let first = tauri::async_runtime::block_on(manager.cloud_sync_keys()).unwrap();
        let second = tauri::async_runtime::block_on(manager.cloud_sync_keys()).unwrap();

        assert!(first
            .vault_root
            .iter()
            .zip(second.vault_root.iter())
            .all(|(left, right)| left == right));
        assert!(first
            .signing_seed
            .iter()
            .zip(second.signing_seed.iter())
            .all(|(left, right)| left == right));
        assert!(first
            .pairing_secret
            .iter()
            .zip(second.pairing_secret.iter())
            .all(|(left, right)| left == right));
        assert!(backend.has("cloud_sync/vault-root-v1"));
        assert!(backend.has("cloud_sync/signing-seed-v1"));
        assert!(backend.has("cloud_sync/pairing-secret-v1"));
    }

    #[test]
    fn cloud_sync_keys_reject_corrupt_native_values_without_creating_later_keys() {
        let (manager, backend) = manager();
        backend.insert("cloud_sync/vault-root-v1", "not-a-32-byte-key");

        assert!(matches!(
            tauri::async_runtime::block_on(manager.cloud_sync_keys()),
            Err(SecretResolveError::Store(SecretStoreError {
                kind: SecretErrorKind::Corrupt,
            }))
        ));
        assert!(!backend.has("cloud_sync/signing-seed-v1"));
        assert!(!backend.has("cloud_sync/pairing-secret-v1"));
    }

    #[test]
    fn portable_manager_fails_closed_for_cloud_sync_keys() {
        let manager = SecretManager::disabled(SecretUnavailableReason::PortableMode);

        assert!(matches!(
            tauri::async_runtime::block_on(manager.cloud_sync_keys()),
            Err(SecretResolveError::Store(SecretStoreError {
                kind: SecretErrorKind::Unavailable,
            }))
        ));
    }

    #[test]
    fn unavailable_keychain_fails_before_cloud_key_generation() {
        let (manager, backend) = manager();
        backend.fail_next(SecretErrorKind::Unavailable);

        assert!(matches!(
            tauri::async_runtime::block_on(manager.cloud_sync_keys()),
            Err(SecretResolveError::Store(SecretStoreError {
                kind: SecretErrorKind::Unavailable,
            }))
        ));
        assert!(!backend.has("cloud_sync/vault-root-v1"));
        assert!(!backend.has("cloud_sync/signing-seed-v1"));
        assert!(!backend.has("cloud_sync/pairing-secret-v1"));
    }

    #[test]
    fn cloud_sync_keys_support_controlled_replacement() {
        let (manager, backend) = manager();
        tauri::async_runtime::block_on(manager.replace_cloud_vault_root([0x11; 32])).unwrap();
        assert!(backend.has("cloud_sync/vault-root-v1"));

        tauri::async_runtime::block_on(manager.replace_cloud_sync_keys(CloudSyncKeys {
            vault_root: Zeroizing::new([0x22; 32]),
            signing_seed: Zeroizing::new([0x33; 32]),
            pairing_secret: Zeroizing::new([0x44; 32]),
        }))
        .unwrap();

        let keys = tauri::async_runtime::block_on(manager.cloud_sync_keys()).unwrap();
        assert!(keys.vault_root.iter().all(|byte| *byte == 0x22));
        assert!(keys.signing_seed.iter().all(|byte| *byte == 0x33));
        assert!(keys.pairing_secret.iter().all(|byte| *byte == 0x44));
    }

    #[test]
    fn blocking_operations_are_serialized_inside_blocking_workers() {
        let (manager, backend) = manager();
        backend.set_delay(Duration::from_millis(20));
        let first = Arc::clone(&manager);
        let second = Arc::clone(&manager);
        let first_thread = thread::spawn(move || {
            tauri::async_runtime::block_on(
                first.state(SecretAccount::llm("openai").unwrap(), None),
            );
        });
        let second_thread = thread::spawn(move || {
            tauri::async_runtime::block_on(
                second.state(SecretAccount::llm("anthropic").unwrap(), None),
            );
        });
        first_thread.join().unwrap();
        second_thread.join().unwrap();
        assert_eq!(backend.max_active_operations(), 1);
    }

    #[test]
    fn migration_resumes_after_native_write_or_readback_interruptions() {
        let (manager, backend) = manager();
        let journal = MemorySettingsJournal::with_legacy_secret("openai", "legacy-secret");
        backend.insert("llm/openai", "legacy-secret");

        let status = tauri::async_runtime::block_on(migrate_journal(&journal, &manager, |_| true));
        assert_eq!(
            status,
            LegacySecretMigrationStatus::Complete { migrated: 1 }
        );
        assert!(journal.has_no_legacy_field());
        assert!(backend.has("llm/openai"));
    }

    #[test]
    fn migration_stays_pending_after_a_save_failure_then_resumes() {
        let (manager, backend) = manager();
        let journal = MemorySettingsJournal::with_legacy_secret("openai", "legacy-secret");
        journal.fail_save_once();

        let pending = tauri::async_runtime::block_on(migrate_journal(&journal, &manager, |_| true));
        assert_eq!(
            pending,
            LegacySecretMigrationStatus::Pending {
                remaining: 1,
                reason: SecretErrorKind::Backend,
            }
        );
        assert!(!journal.has_legacy_secret("openai", "legacy-secret"));
        assert!(backend.has("llm/openai"));

        let restarted = MemorySettingsJournal::with_legacy_secret("openai", "legacy-secret");
        let complete =
            tauri::async_runtime::block_on(migrate_journal(&restarted, &manager, |_| true));
        assert_eq!(
            complete,
            LegacySecretMigrationStatus::Complete { migrated: 1 }
        );
        assert!(restarted.has_no_legacy_field());
    }

    #[test]
    fn journal_step_preserves_a_concurrent_raw_settings_edit() {
        let (manager, _) = manager();
        let journal = MemorySettingsJournal::with_two_legacy_secrets();
        journal.mutate_theme_before_next_remove("dark");

        let status = tauri::async_runtime::block_on(migrate_journal(&journal, &manager, |_| true));
        assert_eq!(
            status,
            LegacySecretMigrationStatus::Complete { migrated: 2 }
        );
        assert_eq!(journal.theme().as_deref(), Some("dark"));
        assert!(journal.has_no_legacy_field());
        assert_eq!(
            journal.schema_version(),
            Some(u64::from(settings::CURRENT_SETTINGS_SCHEMA_VERSION))
        );
    }

    #[test]
    fn migration_finishes_an_interrupted_successful_per_key_save_and_final_schema_save() {
        let (manager, backend) = manager();
        backend.insert("llm/openai", "legacy-secret");
        let journal = MemorySettingsJournal::with_empty_legacy_field();

        let status = tauri::async_runtime::block_on(migrate_journal(&journal, &manager, |_| true));
        assert_eq!(
            status,
            LegacySecretMigrationStatus::Complete { migrated: 0 }
        );
        assert!(journal.has_no_legacy_field());
        assert_eq!(
            journal.schema_version(),
            Some(u64::from(settings::CURRENT_SETTINGS_SCHEMA_VERSION))
        );
        assert!(backend.has("llm/openai"));
    }

    #[test]
    fn migration_never_overwrites_a_different_native_secret() {
        let (manager, backend) = manager();
        backend.insert("llm/openai", "native-secret");
        let journal = MemorySettingsJournal::with_legacy_secret("openai", "legacy-secret");

        let status = tauri::async_runtime::block_on(migrate_journal(&journal, &manager, |_| true));
        assert_eq!(
            status,
            LegacySecretMigrationStatus::Conflict {
                provider_id: "openai".to_string(),
            }
        );
        assert!(journal.has_legacy_secret("openai", "legacy-secret"));
    }

    #[test]
    fn locked_migration_keeps_the_legacy_source_for_a_later_resume() {
        let (manager, backend) = manager();
        let journal = MemorySettingsJournal::with_legacy_secret("openai", "legacy-secret");
        backend.fail_next(SecretErrorKind::Locked);

        let status = tauri::async_runtime::block_on(migrate_journal(&journal, &manager, |_| true));
        assert_eq!(
            status,
            LegacySecretMigrationStatus::Pending {
                remaining: 1,
                reason: SecretErrorKind::Locked,
            }
        );
        assert!(journal.has_legacy_secret("openai", "legacy-secret"));
    }

    #[test]
    fn portable_migration_leaves_plaintext_source_untouched() {
        let journal = MemorySettingsJournal::with_legacy_secret("openai", "legacy-secret");
        let manager = SecretManager::disabled(SecretUnavailableReason::PortableMode);
        let status = tauri::async_runtime::block_on(migrate_journal(&journal, &manager, |_| true));
        assert_eq!(
            status,
            LegacySecretMigrationStatus::PortableBlocked { remaining: 1 }
        );
        assert!(journal.has_legacy_secret("openai", "legacy-secret"));
    }

    #[test]
    fn public_state_settings_and_specta_export_never_include_secret_text() {
        let legacy_secret = "never-export-this-secret";
        let settings: settings::AppSettings = serde_json::from_value(serde_json::json!({
            "settings_schema_version": 5,
            "post_process_api_keys": { "openai": legacy_secret },
        }))
        .unwrap();
        let serialized = serde_json::to_string(&settings).unwrap();
        let debug = format!("{settings:?}");
        let types = specta_typescript::export::<settings::AppSettings>(
            &Typescript::default().bigint(BigIntExportBehavior::Number),
        )
        .unwrap();

        assert!(!serialized.contains(legacy_secret));
        assert!(!debug.contains(legacy_secret));
        assert!(!types.contains(legacy_secret));
        assert!(!types.contains("post_process_api_keys"));
        assert!(types.contains("SecretState"));
    }

    struct FakeSttVerifier {
        calls: std::sync::atomic::AtomicUsize,
        providers: Mutex<Vec<crate::modes::CloudSttProvider>>,
        key_lengths: Mutex<Vec<usize>>,
        outcome: Result<(), SttSecretVerificationError>,
    }

    impl FakeSttVerifier {
        fn succeeds() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
                providers: Mutex::new(Vec::new()),
                key_lengths: Mutex::new(Vec::new()),
                outcome: Ok(()),
            }
        }

        #[cfg(feature = "cloud-realtime")]
        fn fails(error: SttSecretVerificationError) -> Self {
            Self {
                outcome: Err(error),
                ..Self::succeeds()
            }
        }
    }

    impl SttSecretVerifier for FakeSttVerifier {
        fn verify<'a>(
            &'a self,
            provider: crate::modes::CloudSttProvider,
            api_key: Zeroizing<String>,
        ) -> Pin<Box<dyn Future<Output = Result<(), SttSecretVerificationError>> + Send + 'a>>
        {
            Box::pin(async move {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                self.providers
                    .lock()
                    .expect("providers lock")
                    .push(provider);
                self.key_lengths
                    .lock()
                    .expect("key-lengths lock")
                    .push(api_key.len());
                self.outcome
            })
        }
    }

    fn cloud_stt_settings_with_consent() -> settings::AppSettings {
        let mut settings = settings::get_default_settings();
        let provider = settings
            .cloud_stt_provider_mut(crate::modes::CloudSttProvider::DeepgramNova3)
            .expect("default Deepgram provider");
        provider.consent_version = settings::CLOUD_STT_CONSENT_VERSION;
        provider.audio_transfer_consent = true;
        provider.privacy_consent = true;
        provider.local_fallback_consent = true;
        settings
    }

    fn store_stt_secret(manager: &SecretManager, value: &str) {
        let account = SecretAccount::for_provider(
            SecretKind::Stt,
            crate::modes::CloudSttProvider::DeepgramNova3.id(),
        )
        .expect("STT secret account");
        tauri::async_runtime::block_on(manager.replace(account, Zeroizing::new(value.to_string())))
            .expect("store STT secret");
    }

    #[test]
    fn fake_stt_verifier_is_explicit_and_boot_secret_resolution_opens_no_network() {
        let (manager, _) = manager();
        let verifier = FakeSttVerifier::succeeds();
        store_stt_secret(manager.as_ref(), "test-key");

        let resolved = tauri::async_runtime::block_on(resolve_stt_secret(
            manager.as_ref(),
            crate::modes::CloudSttProvider::DeepgramNova3,
        ))
        .expect("stored STT key");
        assert_eq!(resolved.as_str(), "test-key");
        drop(resolved);
        assert_eq!(
            verifier.calls.load(std::sync::atomic::Ordering::Acquire),
            0,
            "loading stored state must not open a provider handshake"
        );

        tauri::async_runtime::block_on(verify_stt_secret_for_settings(
            &cloud_stt_settings_with_consent(),
            manager.as_ref(),
            crate::modes::CloudSttProvider::DeepgramNova3,
            &verifier,
        ))
        .expect("explicit fake verification");

        assert_eq!(verifier.calls.load(std::sync::atomic::Ordering::Acquire), 1);
        assert_eq!(
            *verifier.providers.lock().expect("providers lock"),
            vec![crate::modes::CloudSttProvider::DeepgramNova3]
        );
        assert_eq!(
            *verifier.key_lengths.lock().expect("key-lengths lock"),
            vec![8]
        );
    }

    #[test]
    fn fake_stt_verifier_enforces_consent_and_key_gates_before_handshake() {
        let (secret_manager, _) = manager();
        let verifier = FakeSttVerifier::succeeds();
        store_stt_secret(secret_manager.as_ref(), "test-key");
        let mut missing_consent = cloud_stt_settings_with_consent();
        missing_consent
            .cloud_stt_provider_mut(crate::modes::CloudSttProvider::DeepgramNova3)
            .expect("default Deepgram provider")
            .audio_transfer_consent = false;

        assert_eq!(
            tauri::async_runtime::block_on(verify_stt_secret_for_settings(
                &missing_consent,
                secret_manager.as_ref(),
                crate::modes::CloudSttProvider::DeepgramNova3,
                &verifier,
            )),
            Err(SttSecretVerificationError::ConsentRequired)
        );
        assert_eq!(verifier.calls.load(std::sync::atomic::Ordering::Acquire), 0);

        let (empty_manager, _) = manager();
        assert_eq!(
            tauri::async_runtime::block_on(verify_stt_secret_for_settings(
                &cloud_stt_settings_with_consent(),
                empty_manager.as_ref(),
                crate::modes::CloudSttProvider::DeepgramNova3,
                &verifier,
            )),
            Err(SttSecretVerificationError::NotConfigured)
        );
        assert_eq!(verifier.calls.load(std::sync::atomic::Ordering::Acquire), 0);

        #[cfg(feature = "cloud-realtime")]
        {
            let failing_verifier = FakeSttVerifier::fails(SttSecretVerificationError::Quota);
            assert_eq!(
                tauri::async_runtime::block_on(verify_stt_secret_for_settings(
                    &cloud_stt_settings_with_consent(),
                    secret_manager.as_ref(),
                    crate::modes::CloudSttProvider::DeepgramNova3,
                    &failing_verifier,
                )),
                Err(SttSecretVerificationError::Quota)
            );
            assert_eq!(
                failing_verifier
                    .calls
                    .load(std::sync::atomic::Ordering::Acquire),
                1
            );
        }
    }
}
