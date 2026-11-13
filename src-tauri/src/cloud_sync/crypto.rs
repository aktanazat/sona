//! Cryptographic and canonical-byte primitives for Cloudflare companion protocol v1.
//!
//! Worker-facing records intentionally mirror cloudflare/sona-companion/src/crypto.ts.
//! Local recovery and pairing envelopes are documented beside their encoders because the
//! Worker stores those envelopes opaquely.

use std::{cmp::Ordering, error::Error, fmt};

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

const AES_GCM_NONCE_BYTES: usize = 12;
const AES_GCM_TAG_BYTES: usize = 16;
const AES_GCM_KEY_BYTES: usize = 32;
const ED25519_PUBLIC_KEY_BYTES: usize = 32;
const ED25519_SIGNATURE_BYTES: usize = 64;
const X25519_KEY_BYTES: usize = 32;
const SHA256_BYTES: usize = 32;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// The explicit local recovery-code format version.
pub const RECOVERY_CODE_VERSION: u64 = 1;
/// The explicit local X25519 pairing-envelope format version.
pub const PAIRING_ENVELOPE_VERSION: u64 = 1;

/// Errors from fixed-format protocol and cryptographic operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CryptoError {
    /// A u32 length prefix or output allocation would overflow.
    LengthOverflow,
    /// A fixed-width protocol field had the wrong byte length.
    InvalidLength {
        /// Stable name of the rejected field.
        field: &'static str,
        /// Required byte length.
        expected: usize,
        /// Observed byte length.
        actual: usize,
    },
    /// The value is not canonical unpadded base64url.
    InvalidBase64Url,
    /// A u32be length-prefixed record is malformed or has trailing bytes.
    InvalidRecord,
    /// The object revision index, total, or source format is invalid.
    InvalidObjectRevisionContext,
    /// The recovery-code record is malformed, has an unexpected domain, or has bad field lengths.
    InvalidRecoveryCode,
    /// The recovery-code checksum does not authenticate its record.
    InvalidRecoveryChecksum,
    /// The recovery-code version is unsupported.
    InvalidRecoveryVersion,
    /// The local pairing envelope is malformed, has an unexpected domain, or has bad field lengths.
    InvalidPairingEnvelope,
    /// The local pairing-envelope version is unsupported.
    InvalidPairingEnvelopeVersion,
    /// X25519 produced a non-contributory shared secret.
    InvalidPairingKey,
    /// AES-GCM authentication failed while opening a payload.
    AuthenticationFailed,
    /// AES-GCM could not seal a payload.
    EncryptionFailed,
    /// HKDF-SHA-256 could not derive the requested key material.
    KdfFailed,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LengthOverflow => formatter.write_str("crypto record length overflow"),
            Self::InvalidLength {
                field,
                expected,
                actual,
            } => {
                write!(
                    formatter,
                    "invalid {field} length: expected {expected}, got {actual}"
                )
            }
            Self::InvalidBase64Url => formatter.write_str("invalid unpadded base64url"),
            Self::InvalidRecord => formatter.write_str("invalid length-prefixed record"),
            Self::InvalidObjectRevisionContext => {
                formatter.write_str("invalid object revision crypto context")
            }
            Self::InvalidRecoveryCode => formatter.write_str("invalid recovery code"),
            Self::InvalidRecoveryChecksum => formatter.write_str("invalid recovery-code checksum"),
            Self::InvalidRecoveryVersion => {
                formatter.write_str("unsupported recovery-code version")
            }
            Self::InvalidPairingEnvelope => formatter.write_str("invalid pairing envelope"),
            Self::InvalidPairingEnvelopeVersion => {
                formatter.write_str("unsupported pairing-envelope version")
            }
            Self::InvalidPairingKey => formatter.write_str("invalid pairing key agreement"),
            Self::AuthenticationFailed => {
                formatter.write_str("encrypted payload authentication failed")
            }
            Self::EncryptionFailed => formatter.write_str("encrypted payload sealing failed"),
            Self::KdfFailed => formatter.write_str("HKDF key derivation failed"),
        }
    }
}

impl Error for CryptoError {}

/// One field in a length-prefixed record.
#[derive(Clone, Copy, Debug)]
pub enum RecordField<'a> {
    /// UTF-8 bytes of a protocol string.
    Text(&'a str),
    /// Raw protocol bytes.
    Bytes(&'a [u8]),
    /// Canonical unsigned decimal ASCII without leading zeroes.
    Decimal(u64),
}

/// A query parameter for a Worker-signed request.
pub type QueryParameter<'a> = (&'a str, &'a str);

/// Build a canonical Worker record whose every field has a u32be length prefix.
pub fn length_prefixed_record(fields: &[RecordField<'_>]) -> Result<Vec<u8>, CryptoError> {
    let total = fields.iter().try_fold(0usize, |total, field| {
        checked_prefixed_total(total, record_field_len(*field))
    })?;
    let mut encoded = Vec::with_capacity(total);
    for field in fields {
        append_length(&mut encoded, record_field_len(*field))?;
        match field {
            RecordField::Text(value) => encoded.extend_from_slice(value.as_bytes()),
            RecordField::Bytes(value) => encoded.extend_from_slice(value),
            RecordField::Decimal(value) => append_decimal(&mut encoded, *value)?,
        }
    }
    Ok(encoded)
}

/// Parse a record into borrowed fields, rejecting truncation and trailing partial fields.
pub fn decode_length_prefixed_record(bytes: &[u8]) -> Result<Vec<&[u8]>, CryptoError> {
    let mut remaining = bytes;
    let mut fields = Vec::new();
    while !remaining.is_empty() {
        if remaining.len() < 4 {
            return Err(CryptoError::InvalidRecord);
        }
        let length = usize::try_from(u32::from_be_bytes(
            remaining[..4]
                .try_into()
                .map_err(|_| CryptoError::InvalidRecord)?,
        ))
        .map_err(|_| CryptoError::InvalidRecord)?;
        remaining = &remaining[4..];
        if remaining.len() < length {
            return Err(CryptoError::InvalidRecord);
        }
        let (field, rest) = remaining.split_at(length);
        fields.push(field);
        remaining = rest;
    }
    Ok(fields)
}

/// Encode bytes as URL-safe base64 without padding.
pub fn base64_url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decode only canonical URL-safe base64 without padding.
pub fn base64_url_decode(value: &str) -> Result<Vec<u8>, CryptoError> {
    if value.as_bytes().contains(&b'=') {
        return Err(CryptoError::InvalidBase64Url);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| CryptoError::InvalidBase64Url)?;
    if URL_SAFE_NO_PAD.encode(&decoded) != value {
        return Err(CryptoError::InvalidBase64Url);
    }
    Ok(decoded)
}

/// Return the SHA-256 digest of the supplied bytes.
pub fn sha256_digest(bytes: &[u8]) -> [u8; SHA256_BYTES] {
    let digest = Sha256::digest(bytes);
    let mut output = [0_u8; SHA256_BYTES];
    output.copy_from_slice(&digest);
    output
}

/// Return the SHA-256 digest as unpadded base64url, matching Worker digest fields.
pub fn sha256_base64url(bytes: &[u8]) -> String {
    base64_url_encode(&sha256_digest(bytes))
}

/// SHA-256 over a payload that arrives in pieces, for the one payload too large
/// to hold in memory: a paired device's recording is digested as it streams
/// past on its way to disk.
#[derive(Default)]
pub struct StreamingSha256(Sha256);

impl StreamingSha256 {
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// The digest as unpadded base64url, the same encoding `sha256_base64url`
    /// returns for a payload read in one piece.
    pub fn finish_base64url(self) -> String {
        base64_url_encode(&self.0.finalize())
    }
}
/// Input to the Worker sona-request-v1 signing record.
#[derive(Clone, Copy, Debug)]
pub struct CanonicalRequestInput<'a> {
    pub audience: &'a str,
    pub vault_id: &'a str,
    pub device_id: &'a str,
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a [QueryParameter<'a>],
    pub body_digest: &'a str,
    pub content_type: &'a str,
    pub idempotency_key: &'a str,
    pub timestamp: u64,
    pub nonce: &'a [u8],
}

/// Build the exact Worker sona-request-v1 canonical record.
pub fn canonical_request_bytes(input: &CanonicalRequestInput<'_>) -> Result<Vec<u8>, CryptoError> {
    let mut query = input.query.to_vec();
    query.sort_unstable_by(|(left_key, left_value), (right_key, right_value)| {
        compare_worker_strings(left_key, right_key)
            .then_with(|| compare_worker_strings(left_value, right_value))
    });
    let records = query
        .iter()
        .map(|(key, value)| {
            length_prefixed_record(&[RecordField::Text(key), RecordField::Text(value)])
        })
        .collect::<Result<Vec<_>, _>>()?;
    let query_bytes = prefix_nested_records(&records)?;
    length_prefixed_record(&[
        RecordField::Text("sona-request-v1"),
        RecordField::Text(input.audience),
        RecordField::Text(input.vault_id),
        RecordField::Text(input.device_id),
        RecordField::Text(input.method),
        RecordField::Text(input.path),
        RecordField::Bytes(&query_bytes),
        RecordField::Text(input.body_digest),
        RecordField::Text(input.content_type),
        RecordField::Text(input.idempotency_key),
        RecordField::Decimal(input.timestamp),
        RecordField::Bytes(input.nonce),
    ])
}

/// Input to the Worker sona-bootstrap-v1 self-signing record.
#[derive(Clone, Copy, Debug)]
pub struct CanonicalBootstrapInput<'a> {
    pub audience: &'a str,
    pub vault_id: &'a str,
    pub device_id: &'a str,
    pub signing_public_key: &'a [u8],
    pub pairing_public_key: &'a [u8],
}

/// Build the exact Worker sona-bootstrap-v1 record.
pub fn canonical_bootstrap_bytes(
    input: &CanonicalBootstrapInput<'_>,
) -> Result<Vec<u8>, CryptoError> {
    length_prefixed_record(&[
        RecordField::Text("sona-bootstrap-v1"),
        RecordField::Text(input.audience),
        RecordField::Text(input.vault_id),
        RecordField::Text(input.device_id),
        RecordField::Bytes(input.signing_public_key),
        RecordField::Bytes(input.pairing_public_key),
    ])
}

/// Input to the Worker sona-pair-candidate-v1 proof record.
#[derive(Clone, Copy, Debug)]
pub struct CanonicalPairCandidateInput<'a> {
    pub audience: &'a str,
    pub vault_id: &'a str,
    pub candidate_device_id: &'a str,
    pub candidate_signing_public_key: &'a [u8],
    pub candidate_pairing_public_key: &'a [u8],
    pub pairing_nonce: &'a [u8],
    pub expires_at: u64,
}

/// Build the exact Worker sona-pair-candidate-v1 record signed as candidate proof.
pub fn canonical_pair_candidate_bytes(
    input: &CanonicalPairCandidateInput<'_>,
) -> Result<Vec<u8>, CryptoError> {
    length_prefixed_record(&[
        RecordField::Text("sona-pair-candidate-v1"),
        RecordField::Text(input.audience),
        RecordField::Text(input.vault_id),
        RecordField::Text(input.candidate_device_id),
        RecordField::Bytes(input.candidate_signing_public_key),
        RecordField::Bytes(input.candidate_pairing_public_key),
        RecordField::Bytes(input.pairing_nonce),
        RecordField::Decimal(input.expires_at),
    ])
}

/// Input to the Worker sona-pair-approval-v1 approval record.
#[derive(Clone, Copy, Debug)]
pub struct CanonicalPairApprovalInput<'a> {
    pub vault_id: &'a str,
    pub candidate_record: &'a [u8],
    pub candidate_proof: &'a [u8],
    pub envelope: &'a [u8],
}

/// Build the exact Worker sona-pair-approval-v1 record signed by the approving device.
pub fn canonical_pair_approval_bytes(
    input: &CanonicalPairApprovalInput<'_>,
) -> Result<Vec<u8>, CryptoError> {
    length_prefixed_record(&[
        RecordField::Text("sona-pair-approval-v1"),
        RecordField::Text(input.vault_id),
        RecordField::Bytes(input.candidate_record),
        RecordField::Bytes(input.candidate_proof),
        RecordField::Bytes(input.envelope),
    ])
}

/// The Worker upload-envelope kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadKind {
    Object,
    Share,
}

impl UploadKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Object => "object",
            Self::Share => "share",
        }
    }
}

/// A chunk descriptor included in a Worker upload-envelope signature.
#[derive(Clone, Copy, Debug)]
pub struct UploadChunk<'a> {
    pub index: u64,
    pub size: u64,
    pub sha256: &'a str,
}

/// Input to the Worker sona-upload-envelope-v1 signing record.
#[derive(Clone, Copy, Debug)]
pub struct CanonicalUploadEnvelopeInput<'a> {
    pub vault_id: &'a str,
    pub kind: UploadKind,
    pub object_id: Option<&'a str>,
    pub revision_id: Option<&'a str>,
    pub base_revision_id: Option<&'a str>,
    pub share_id: Option<&'a str>,
    pub manifest_digest: &'a str,
    pub crypto_version: u64,
    pub total_bytes: u64,
    pub chunks: &'a [UploadChunk<'a>],
}

/// Build the exact Worker sona-upload-envelope-v1 record.
pub fn canonical_upload_envelope_bytes(
    input: &CanonicalUploadEnvelopeInput<'_>,
) -> Result<Vec<u8>, CryptoError> {
    let records = input
        .chunks
        .iter()
        .map(|chunk| {
            length_prefixed_record(&[
                RecordField::Decimal(chunk.index),
                RecordField::Decimal(chunk.size),
                RecordField::Text(chunk.sha256),
            ])
        })
        .collect::<Result<Vec<_>, _>>()?;
    let chunk_bytes = prefix_nested_records(&records)?;
    let chunk_count = u64::try_from(input.chunks.len()).map_err(|_| CryptoError::LengthOverflow)?;
    length_prefixed_record(&[
        RecordField::Text("sona-upload-envelope-v1"),
        RecordField::Text(input.vault_id),
        RecordField::Text(input.kind.as_str()),
        RecordField::Text(input.object_id.unwrap_or_default()),
        RecordField::Text(input.revision_id.unwrap_or_default()),
        RecordField::Text(input.base_revision_id.unwrap_or_default()),
        RecordField::Text(input.share_id.unwrap_or_default()),
        RecordField::Text(input.manifest_digest),
        RecordField::Decimal(input.crypto_version),
        RecordField::Decimal(input.total_bytes),
        RecordField::Decimal(chunk_count),
        RecordField::Bytes(&chunk_bytes),
    ])
}

/// Input to the Worker sona-tombstone-v1 signing record.
#[derive(Clone, Copy, Debug)]
pub struct CanonicalTombstoneInput<'a> {
    pub vault_id: &'a str,
    pub object_id: &'a str,
    pub tombstone_revision_id: &'a str,
    pub base_revision_id: &'a str,
    pub reason: &'a str,
    pub format_version: u64,
}

/// Build the exact Worker sona-tombstone-v1 record.
pub fn canonical_tombstone_bytes(
    input: &CanonicalTombstoneInput<'_>,
) -> Result<Vec<u8>, CryptoError> {
    length_prefixed_record(&[
        RecordField::Text("sona-tombstone-v1"),
        RecordField::Text(input.vault_id),
        RecordField::Text(input.object_id),
        RecordField::Text(input.tombstone_revision_id),
        RecordField::Text(input.base_revision_id),
        RecordField::Text(input.reason),
        RecordField::Decimal(input.format_version),
    ])
}
/// Derive the Ed25519 public key from a 32-byte signing seed.
pub fn ed25519_public_key(
    signing_seed: &[u8],
) -> Result<[u8; ED25519_PUBLIC_KEY_BYTES], CryptoError> {
    let seed = fixed_array::<ED25519_PUBLIC_KEY_BYTES>(signing_seed, "Ed25519 signing seed")?;
    Ok(SigningKey::from_bytes(&seed).verifying_key().to_bytes())
}

/// Sign a byte record with an Ed25519 32-byte signing seed.
pub fn sign_ed25519(
    signing_seed: &[u8],
    message: &[u8],
) -> Result<[u8; ED25519_SIGNATURE_BYTES], CryptoError> {
    let seed = fixed_array::<ED25519_PUBLIC_KEY_BYTES>(signing_seed, "Ed25519 signing seed")?;
    Ok(SigningKey::from_bytes(&seed).sign(message).to_bytes())
}

/// Verify an Ed25519 signature, returning false for malformed keys or signatures.
pub fn verify_ed25519(public_key: &[u8], signature: &[u8], message: &[u8]) -> bool {
    let Ok(public_key) = fixed_array::<ED25519_PUBLIC_KEY_BYTES>(public_key, "Ed25519 public key")
    else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(signature) else {
        return false;
    };
    let Ok(public_key) = VerifyingKey::from_bytes(&public_key) else {
        return false;
    };
    public_key.verify(message, &signature).is_ok()
}

macro_rules! signed_record_helpers {
    ($sign:ident, $verify:ident, $builder:ident, $input:ident) => {
        pub fn $sign(
            input: &$input<'_>,
            signing_seed: &[u8],
        ) -> Result<[u8; ED25519_SIGNATURE_BYTES], CryptoError> {
            let record = $builder(input)?;
            sign_ed25519(signing_seed, &record)
        }

        #[cfg(test)]
        pub fn $verify(
            input: &$input<'_>,
            public_key: &[u8],
            signature: &[u8],
        ) -> Result<bool, CryptoError> {
            let record = $builder(input)?;
            Ok(verify_ed25519(public_key, signature, &record))
        }
    };
}

signed_record_helpers!(
    sign_canonical_request,
    verify_canonical_request,
    canonical_request_bytes,
    CanonicalRequestInput
);
signed_record_helpers!(
    sign_canonical_bootstrap,
    verify_canonical_bootstrap,
    canonical_bootstrap_bytes,
    CanonicalBootstrapInput
);
signed_record_helpers!(
    sign_canonical_pair_candidate,
    verify_canonical_pair_candidate,
    canonical_pair_candidate_bytes,
    CanonicalPairCandidateInput
);
signed_record_helpers!(
    sign_canonical_pair_approval,
    verify_canonical_pair_approval,
    canonical_pair_approval_bytes,
    CanonicalPairApprovalInput
);
signed_record_helpers!(
    sign_canonical_upload_envelope,
    verify_canonical_upload_envelope,
    canonical_upload_envelope_bytes,
    CanonicalUploadEnvelopeInput
);
signed_record_helpers!(
    sign_canonical_tombstone,
    verify_canonical_tombstone,
    canonical_tombstone_bytes,
    CanonicalTombstoneInput
);

/// The object-revision payload domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectContentKind {
    Manifest,
    Chunk,
}

impl ObjectContentKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Chunk => "chunk",
        }
    }
}

/// Metadata which binds a revision payload's HKDF key and AES-GCM AAD.
#[derive(Clone, Copy, Debug)]
pub struct ObjectRevisionCryptoContext<'a> {
    pub vault_id: &'a str,
    pub object_id: &'a str,
    pub revision_id: &'a str,
    pub index: u64,
    pub total: u64,
    pub content_kind: ObjectContentKind,
    pub source_format: &'a str,
}

/// Build the HKDF info record for an object revision root.
pub fn object_revision_root_info(
    vault_id: &str,
    object_id: &str,
    revision_id: &str,
) -> Result<Vec<u8>, CryptoError> {
    length_prefixed_record(&[
        RecordField::Text("sona-revision-root-v1"),
        RecordField::Text(vault_id),
        RecordField::Text(object_id),
        RecordField::Text(revision_id),
    ])
}

/// Derive the 32-byte revision root from the vault root using Worker HKDF parameters.
pub fn derive_object_revision_root(
    vault_root: &[u8],
    vault_id: &str,
    object_id: &str,
    revision_id: &str,
) -> Result<[u8; AES_GCM_KEY_BYTES], CryptoError> {
    let vault_root = fixed_array::<AES_GCM_KEY_BYTES>(vault_root, "vault root")?;
    let info = object_revision_root_info(vault_id, object_id, revision_id)?;
    derive_aes_gcm_key(&vault_root, b"sona-revision-v1", &info)
}

/// Build the HKDF info record for an object revision payload key.
pub fn object_revision_key_info(
    context: &ObjectRevisionCryptoContext<'_>,
) -> Result<Vec<u8>, CryptoError> {
    validate_object_revision_context(context)?;
    length_prefixed_record(&[
        RecordField::Text("sona-object-key-v1"),
        RecordField::Text(context.vault_id),
        RecordField::Text(context.object_id),
        RecordField::Text(context.revision_id),
        RecordField::Decimal(context.index),
        RecordField::Decimal(context.total),
        RecordField::Text(context.content_kind.as_str()),
        RecordField::Text(context.source_format),
    ])
}

/// Build the AES-GCM additional authenticated data for an object revision payload.
pub fn object_revision_aad(
    context: &ObjectRevisionCryptoContext<'_>,
) -> Result<Vec<u8>, CryptoError> {
    validate_object_revision_context(context)?;
    length_prefixed_record(&[
        RecordField::Text("sona-object-aad-v1"),
        RecordField::Text(context.vault_id),
        RecordField::Text(context.object_id),
        RecordField::Text(context.revision_id),
        RecordField::Decimal(context.index),
        RecordField::Decimal(context.total),
        RecordField::Text(context.content_kind.as_str()),
        RecordField::Text(context.source_format),
    ])
}

/// Derive the AES-256-GCM object revision payload key using Worker HKDF parameters.
pub fn derive_object_revision_key(
    vault_root: &[u8],
    context: &ObjectRevisionCryptoContext<'_>,
) -> Result<[u8; AES_GCM_KEY_BYTES], CryptoError> {
    let revision_root = derive_object_revision_root(
        vault_root,
        context.vault_id,
        context.object_id,
        context.revision_id,
    )?;
    let info = object_revision_key_info(context)?;
    derive_aes_gcm_key(&revision_root, b"sona-object-v1", &info)
}

/// Seal an object revision payload as nonce_12 || ciphertext || tag_16.
pub fn seal_object_revision_payload(
    vault_root: &[u8],
    context: &ObjectRevisionCryptoContext<'_>,
    nonce: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let nonce = fixed_array::<AES_GCM_NONCE_BYTES>(nonce, "object payload nonce")?;
    let key = derive_object_revision_key(vault_root, context)?;
    let aad = object_revision_aad(context)?;
    let encrypted = aes_gcm_encrypt(&key, &nonce, &aad, plaintext)?;
    prepend_nonce(&nonce, encrypted)
}

/// Open an object revision payload encoded as nonce_12 || ciphertext || tag_16.
pub fn open_object_revision_payload(
    vault_root: &[u8],
    context: &ObjectRevisionCryptoContext<'_>,
    encrypted_payload: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    ensure_minimum_aes_gcm_payload(encrypted_payload, "object encrypted payload")?;
    let nonce = fixed_array::<AES_GCM_NONCE_BYTES>(
        &encrypted_payload[..AES_GCM_NONCE_BYTES],
        "object payload nonce",
    )?;
    let key = derive_object_revision_key(vault_root, context)?;
    let aad = object_revision_aad(context)?;
    aes_gcm_decrypt(
        &key,
        &nonce,
        &aad,
        &encrypted_payload[AES_GCM_NONCE_BYTES..],
    )
}
/// The shared-link payload domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharePayloadDomain {
    Manifest,
    Chunk,
}

impl SharePayloadDomain {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Chunk => "chunk",
        }
    }
}

/// Metadata which binds a shared-link payload's HKDF key and AES-GCM AAD.
#[derive(Clone, Copy, Debug)]
pub struct SharePayloadContext<'a> {
    pub share_id: &'a str,
    pub index: u64,
    pub total: u64,
    pub domain: SharePayloadDomain,
}

/// Build the HKDF info record for a shared-link payload key.
pub fn share_payload_key_info(context: &SharePayloadContext<'_>) -> Result<Vec<u8>, CryptoError> {
    length_prefixed_record(&[
        RecordField::Text("sona-share-key-v1"),
        RecordField::Text(context.share_id),
        RecordField::Decimal(context.index),
        RecordField::Decimal(context.total),
        RecordField::Text(context.domain.as_str()),
    ])
}

/// Build the AES-GCM additional authenticated data for a shared-link payload.
pub fn share_payload_aad(context: &SharePayloadContext<'_>) -> Result<Vec<u8>, CryptoError> {
    length_prefixed_record(&[
        RecordField::Text("sona-share-aad-v1"),
        RecordField::Text(context.share_id),
        RecordField::Decimal(context.index),
        RecordField::Decimal(context.total),
        RecordField::Text(context.domain.as_str()),
    ])
}

/// Derive the AES-256-GCM key for a shared-link payload using Worker HKDF parameters.
pub fn derive_share_payload_key(
    root: &[u8],
    context: &SharePayloadContext<'_>,
) -> Result<[u8; AES_GCM_KEY_BYTES], CryptoError> {
    let root = fixed_array::<AES_GCM_KEY_BYTES>(root, "share root")?;
    let info = share_payload_key_info(context)?;
    derive_aes_gcm_key(&root, b"sona-share-v1", &info)
}

/// Seal a shared-link payload as nonce_12 || ciphertext || tag_16.
pub fn seal_share_payload(
    root: &[u8],
    context: &SharePayloadContext<'_>,
    nonce: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let nonce = fixed_array::<AES_GCM_NONCE_BYTES>(nonce, "share payload nonce")?;
    let key = derive_share_payload_key(root, context)?;
    let aad = share_payload_aad(context)?;
    let encrypted = aes_gcm_encrypt(&key, &nonce, &aad, plaintext)?;
    prepend_nonce(&nonce, encrypted)
}

/// Open a shared-link payload encoded as nonce_12 || ciphertext || tag_16.
pub fn open_share_payload(
    root: &[u8],
    context: &SharePayloadContext<'_>,
    encrypted_payload: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    ensure_minimum_aes_gcm_payload(encrypted_payload, "share encrypted payload")?;
    let nonce = fixed_array::<AES_GCM_NONCE_BYTES>(
        &encrypted_payload[..AES_GCM_NONCE_BYTES],
        "share payload nonce",
    )?;
    let key = derive_share_payload_key(root, context)?;
    let aad = share_payload_aad(context)?;
    aes_gcm_decrypt(
        &key,
        &nonce,
        &aad,
        &encrypted_payload[AES_GCM_NONCE_BYTES..],
    )
}

/// What a live room's link root opens: the token a browser joins the room
/// with, and the key its frames are sealed with.
pub struct LiveRoomKeys {
    pub join_token: [u8; AES_GCM_KEY_BYTES],
    pub frame_key: [u8; AES_GCM_KEY_BYTES],
}

/// Derive a live room's join token and frame key from its link root, as
/// the browser does from the link's fragment.
pub fn derive_live_room_keys(root: &[u8], room_id: &str) -> Result<LiveRoomKeys, CryptoError> {
    let root = fixed_array::<AES_GCM_KEY_BYTES>(root, "live room root")?;
    let info =
        |label| length_prefixed_record(&[RecordField::Text(label), RecordField::Text(room_id)]);
    Ok(LiveRoomKeys {
        join_token: derive_aes_gcm_key(&root, b"sona-live-v1", &info("sona-live-join-v1")?)?,
        frame_key: derive_aes_gcm_key(&root, b"sona-live-v1", &info("sona-live-key-v1")?)?,
    })
}

/// The AES-GCM additional authenticated data for one live room frame.
pub fn live_frame_aad(room_id: &str, kind: &str, seq: u64) -> Result<Vec<u8>, CryptoError> {
    length_prefixed_record(&[
        RecordField::Text("sona-live-aad-v1"),
        RecordField::Text(room_id),
        RecordField::Text(kind),
        RecordField::Decimal(seq),
    ])
}

/// Seal a live room frame as nonce_12 || ciphertext || tag_16.
pub fn seal_live_frame(
    frame_key: &[u8; AES_GCM_KEY_BYTES],
    room_id: &str,
    kind: &str,
    seq: u64,
    nonce: &[u8; AES_GCM_NONCE_BYTES],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let aad = live_frame_aad(room_id, kind, seq)?;
    prepend_nonce(nonce, aes_gcm_encrypt(frame_key, nonce, &aad, plaintext)?)
}

/// Open a live room frame sealed as nonce_12 || ciphertext || tag_16.
pub fn open_live_frame(
    frame_key: &[u8; AES_GCM_KEY_BYTES],
    room_id: &str,
    kind: &str,
    seq: u64,
    sealed: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    ensure_minimum_aes_gcm_payload(sealed, "live room frame")?;
    let nonce =
        fixed_array::<AES_GCM_NONCE_BYTES>(&sealed[..AES_GCM_NONCE_BYTES], "live room nonce")?;
    let aad = live_frame_aad(room_id, kind, seq)?;
    aes_gcm_decrypt(frame_key, &nonce, &aad, &sealed[AES_GCM_NONCE_BYTES..])
}

/// A decoded local recovery code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryCode {
    pub vault_id: String,
    pub vault_root: [u8; AES_GCM_KEY_BYTES],
}

/// Encode a local recovery code.
///
/// The binary payload is the exact record
/// (sona-recovery-v1, 1, vault_id, vault_root_32) followed by the raw SHA-256 digest of that
/// record. The complete payload is unpadded base64url.
pub fn encode_recovery_code(vault_id: &str, vault_root: &[u8]) -> Result<String, CryptoError> {
    let vault_root = fixed_array::<AES_GCM_KEY_BYTES>(vault_root, "recovery vault root")?;
    let mut record = length_prefixed_record(&[
        RecordField::Text("sona-recovery-v1"),
        RecordField::Decimal(RECOVERY_CODE_VERSION),
        RecordField::Text(vault_id),
        RecordField::Bytes(&vault_root),
    ])?;
    let checksum = sha256_digest(&record);
    record.reserve(SHA256_BYTES);
    record.extend_from_slice(&checksum);
    Ok(base64_url_encode(&record))
}

/// Decode and authenticate a local recovery code.
///
/// Decoding rejects padding, a bad checksum, an unknown version, malformed length prefixes,
/// a non-32-byte root, non-UTF-8 vault id, and any record with extra fields.
pub fn decode_recovery_code(encoded: &str) -> Result<RecoveryCode, CryptoError> {
    let bytes = base64_url_decode(encoded)?;
    if bytes.len() < SHA256_BYTES {
        return Err(CryptoError::InvalidRecoveryCode);
    }
    let split_at = bytes.len() - SHA256_BYTES;
    let (record, checksum) = bytes.split_at(split_at);
    if !constant_time_eq(&sha256_digest(record), checksum) {
        return Err(CryptoError::InvalidRecoveryChecksum);
    }
    let fields =
        decode_length_prefixed_record(record).map_err(|_| CryptoError::InvalidRecoveryCode)?;
    if fields.len() != 4 || fields[0] != b"sona-recovery-v1" {
        return Err(CryptoError::InvalidRecoveryCode);
    }
    if fields[1] != b"1" {
        return Err(CryptoError::InvalidRecoveryVersion);
    }
    let vault_id = std::str::from_utf8(fields[2])
        .map_err(|_| CryptoError::InvalidRecoveryCode)?
        .to_owned();
    let vault_root = fixed_array::<AES_GCM_KEY_BYTES>(fields[3], "recovery vault root")
        .map_err(|_| CryptoError::InvalidRecoveryCode)?;
    Ok(RecoveryCode {
        vault_id,
        vault_root,
    })
}
/// Deterministic inputs for the local X25519 pairing envelope.
#[derive(Clone, Copy, Debug)]
pub struct PairingEnvelopeSealInput<'a> {
    /// The receiving device's X25519 pairing public key.
    pub recipient_public_key: &'a [u8],
    /// Fresh caller-supplied X25519 ephemeral secret key bytes.
    pub ephemeral_secret_key: &'a [u8],
    /// Fresh caller-supplied 12-byte AES-GCM nonce.
    pub nonce: &'a [u8],
    /// The 32-byte vault root sent only in encrypted form.
    pub vault_root: &'a [u8],
}

/// Derive an X25519 public key from a 32-byte secret key.
pub fn x25519_public_key(secret_key: &[u8]) -> Result<[u8; X25519_KEY_BYTES], CryptoError> {
    let secret_key = fixed_array::<X25519_KEY_BYTES>(secret_key, "X25519 secret key")?;
    let secret = StaticSecret::from(secret_key);
    Ok(X25519PublicKey::from(&secret).to_bytes())
}

/// Perform a contributory X25519 key agreement.
pub fn x25519_shared_secret(
    secret_key: &[u8],
    public_key: &[u8],
) -> Result<[u8; X25519_KEY_BYTES], CryptoError> {
    let secret_key = fixed_array::<X25519_KEY_BYTES>(secret_key, "X25519 secret key")?;
    let public_key = fixed_array::<X25519_KEY_BYTES>(public_key, "X25519 public key")?;
    let secret = StaticSecret::from(secret_key);
    let shared = secret.diffie_hellman(&X25519PublicKey::from(public_key));
    if !shared.was_contributory() {
        return Err(CryptoError::InvalidPairingKey);
    }
    Ok(shared.to_bytes())
}

/// Seal the local pairing envelope used as the opaque Worker pairing envelope field.
///
/// The binary envelope is the exact record
/// (sona-pairing-envelope-v1, 1, ephemeral_public_32, nonce_12, ciphertext_48).
/// The plaintext is exactly the 32-byte vault root. Its AES-256-GCM key is HKDF-SHA-256 with
/// IKM equal to the contributory X25519 shared secret, salt sona-pairing-envelope-v1, and
/// info (sona-pairing-envelope-key-v1, recipient_public_32, ephemeral_public_32).
/// AES-GCM AAD is (sona-pairing-envelope-aad-v1, 1, recipient_public_32,
/// ephemeral_public_32, nonce_12). All listed records use u32be length-prefixed fields.
pub fn seal_pairing_envelope(input: &PairingEnvelopeSealInput<'_>) -> Result<Vec<u8>, CryptoError> {
    let recipient_public_key = fixed_array::<X25519_KEY_BYTES>(
        input.recipient_public_key,
        "pairing recipient public key",
    )?;
    let ephemeral_secret_key = fixed_array::<X25519_KEY_BYTES>(
        input.ephemeral_secret_key,
        "pairing ephemeral secret key",
    )?;
    let nonce = fixed_array::<AES_GCM_NONCE_BYTES>(input.nonce, "pairing envelope nonce")?;
    let vault_root = fixed_array::<AES_GCM_KEY_BYTES>(input.vault_root, "pairing vault root")?;
    let ephemeral_public_key = x25519_public_key(&ephemeral_secret_key)?;
    let shared_secret = x25519_shared_secret(&ephemeral_secret_key, &recipient_public_key)?;
    let key = pairing_envelope_key(&shared_secret, &recipient_public_key, &ephemeral_public_key)?;
    let aad = pairing_envelope_aad(&recipient_public_key, &ephemeral_public_key, &nonce)?;
    let ciphertext = aes_gcm_encrypt(&key, &nonce, &aad, &vault_root)?;
    length_prefixed_record(&[
        RecordField::Text("sona-pairing-envelope-v1"),
        RecordField::Decimal(PAIRING_ENVELOPE_VERSION),
        RecordField::Bytes(&ephemeral_public_key),
        RecordField::Bytes(&nonce),
        RecordField::Bytes(&ciphertext),
    ])
}

/// Open a local pairing envelope and return the authenticated 32-byte vault root.
pub fn open_pairing_envelope(
    recipient_secret_key: &[u8],
    encoded_envelope: &[u8],
) -> Result<[u8; AES_GCM_KEY_BYTES], CryptoError> {
    let fields = decode_length_prefixed_record(encoded_envelope)
        .map_err(|_| CryptoError::InvalidPairingEnvelope)?;
    if fields.len() != 5 || fields[0] != b"sona-pairing-envelope-v1" {
        return Err(CryptoError::InvalidPairingEnvelope);
    }
    if fields[1] != b"1" {
        return Err(CryptoError::InvalidPairingEnvelopeVersion);
    }
    let ephemeral_public_key =
        fixed_array::<X25519_KEY_BYTES>(fields[2], "pairing ephemeral public key")
            .map_err(|_| CryptoError::InvalidPairingEnvelope)?;
    let nonce = fixed_array::<AES_GCM_NONCE_BYTES>(fields[3], "pairing envelope nonce")
        .map_err(|_| CryptoError::InvalidPairingEnvelope)?;
    if fields[4].len() != AES_GCM_KEY_BYTES + AES_GCM_TAG_BYTES {
        return Err(CryptoError::InvalidPairingEnvelope);
    }
    let recipient_secret_key =
        fixed_array::<X25519_KEY_BYTES>(recipient_secret_key, "pairing recipient secret key")?;
    let recipient_public_key = x25519_public_key(&recipient_secret_key)?;
    let shared_secret = x25519_shared_secret(&recipient_secret_key, &ephemeral_public_key)?;
    let key = pairing_envelope_key(&shared_secret, &recipient_public_key, &ephemeral_public_key)?;
    let aad = pairing_envelope_aad(&recipient_public_key, &ephemeral_public_key, &nonce)?;
    let vault_root = aes_gcm_decrypt(&key, &nonce, &aad, fields[4])?;
    fixed_array::<AES_GCM_KEY_BYTES>(&vault_root, "pairing vault root")
        .map_err(|_| CryptoError::InvalidPairingEnvelope)
}
fn record_field_len(field: RecordField<'_>) -> usize {
    match field {
        RecordField::Text(value) => value.len(),
        RecordField::Bytes(value) => value.len(),
        RecordField::Decimal(value) => decimal_len(value),
    }
}

fn decimal_len(mut value: u64) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

fn append_decimal(output: &mut Vec<u8>, mut value: u64) -> Result<(), CryptoError> {
    let mut buffer = [0_u8; 20];
    let mut start = buffer.len();
    loop {
        start -= 1;
        let digit = u8::try_from(value % 10).map_err(|_| CryptoError::LengthOverflow)?;
        buffer[start] = b'0' + digit;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    output.extend_from_slice(&buffer[start..]);
    Ok(())
}

fn checked_prefixed_total(total: usize, field_len: usize) -> Result<usize, CryptoError> {
    u32::try_from(field_len).map_err(|_| CryptoError::LengthOverflow)?;
    total
        .checked_add(4)
        .and_then(|total| total.checked_add(field_len))
        .ok_or(CryptoError::LengthOverflow)
}

fn append_length(output: &mut Vec<u8>, field_len: usize) -> Result<(), CryptoError> {
    let field_len = u32::try_from(field_len).map_err(|_| CryptoError::LengthOverflow)?;
    output.extend_from_slice(&field_len.to_be_bytes());
    Ok(())
}

fn prefix_nested_records(records: &[Vec<u8>]) -> Result<Vec<u8>, CryptoError> {
    let total = records.iter().try_fold(0usize, |total, record| {
        checked_prefixed_total(total, record.len())
    })?;
    let mut encoded = Vec::with_capacity(total);
    for record in records {
        append_length(&mut encoded, record.len())?;
        encoded.extend_from_slice(record);
    }
    Ok(encoded)
}

fn compare_worker_strings(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn fixed_array<const N: usize>(bytes: &[u8], field: &'static str) -> Result<[u8; N], CryptoError> {
    bytes.try_into().map_err(|_| CryptoError::InvalidLength {
        field,
        expected: N,
        actual: bytes.len(),
    })
}

fn validate_object_revision_context(
    context: &ObjectRevisionCryptoContext<'_>,
) -> Result<(), CryptoError> {
    if context.index > MAX_SAFE_INTEGER
        || context.total == 0
        || context.total > MAX_SAFE_INTEGER
        || context.index >= context.total
        || context.source_format.is_empty()
    {
        return Err(CryptoError::InvalidObjectRevisionContext);
    }
    Ok(())
}

fn derive_aes_gcm_key(
    material: &[u8],
    salt: &[u8],
    info: &[u8],
) -> Result<[u8; AES_GCM_KEY_BYTES], CryptoError> {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), material);
    let mut key = [0_u8; AES_GCM_KEY_BYTES];
    hkdf.expand(info, &mut key)
        .map_err(|_| CryptoError::KdfFailed)?;
    Ok(key)
}

fn aes_gcm_encrypt(
    key: &[u8; AES_GCM_KEY_BYTES],
    nonce: &[u8; AES_GCM_NONCE_BYTES],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::EncryptionFailed)?;
    cipher
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::EncryptionFailed)
}

fn aes_gcm_decrypt(
    key: &[u8; AES_GCM_KEY_BYTES],
    nonce: &[u8; AES_GCM_NONCE_BYTES],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::AuthenticationFailed)?;
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::AuthenticationFailed)
}

fn prepend_nonce(
    nonce: &[u8; AES_GCM_NONCE_BYTES],
    ciphertext: Vec<u8>,
) -> Result<Vec<u8>, CryptoError> {
    let capacity = nonce
        .len()
        .checked_add(ciphertext.len())
        .ok_or(CryptoError::LengthOverflow)?;
    let mut output = Vec::with_capacity(capacity);
    output.extend_from_slice(nonce);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

fn ensure_minimum_aes_gcm_payload(payload: &[u8], field: &'static str) -> Result<(), CryptoError> {
    let minimum = AES_GCM_NONCE_BYTES + AES_GCM_TAG_BYTES;
    if payload.len() < minimum {
        return Err(CryptoError::InvalidLength {
            field,
            expected: minimum,
            actual: payload.len(),
        });
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn pairing_envelope_key(
    shared_secret: &[u8; X25519_KEY_BYTES],
    recipient_public_key: &[u8; X25519_KEY_BYTES],
    ephemeral_public_key: &[u8; X25519_KEY_BYTES],
) -> Result<[u8; AES_GCM_KEY_BYTES], CryptoError> {
    let info = length_prefixed_record(&[
        RecordField::Text("sona-pairing-envelope-key-v1"),
        RecordField::Bytes(recipient_public_key),
        RecordField::Bytes(ephemeral_public_key),
    ])?;
    derive_aes_gcm_key(shared_secret, b"sona-pairing-envelope-v1", &info)
}

fn pairing_envelope_aad(
    recipient_public_key: &[u8; X25519_KEY_BYTES],
    ephemeral_public_key: &[u8; X25519_KEY_BYTES],
    nonce: &[u8; AES_GCM_NONCE_BYTES],
) -> Result<Vec<u8>, CryptoError> {
    length_prefixed_record(&[
        RecordField::Text("sona-pairing-envelope-aad-v1"),
        RecordField::Decimal(PAIRING_ENVELOPE_VERSION),
        RecordField::Bytes(recipient_public_key),
        RecordField::Bytes(ephemeral_public_key),
        RecordField::Bytes(nonce),
    ])
}
#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../cloudflare/sona-companion/fixtures/crypto-v1.json"
    ));
    const FIXTURE_SIGNING_SEED: [u8; 32] = [
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c,
        0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae,
        0x7f, 0x60,
    ];

    struct Fixture(serde_json::Value);

    #[derive(Clone, Copy)]
    struct FixtureNode<'a>(&'a serde_json::Value);

    impl Fixture {
        fn parse() -> Self {
            Self(serde_json::from_str(FIXTURE).expect("frozen crypto fixture is valid JSON"))
        }

        fn field(&self, name: &str) -> FixtureNode<'_> {
            FixtureNode(
                self.0
                    .get(name)
                    .unwrap_or_else(|| panic!("fixture is missing {name}")),
            )
        }
    }

    impl<'a> FixtureNode<'a> {
        fn field(self, name: &str) -> Self {
            Self(
                self.0
                    .get(name)
                    .unwrap_or_else(|| panic!("fixture is missing {name}")),
            )
        }

        fn text(self) -> &'a str {
            self.0.as_str().expect("fixture value is a string")
        }

        fn optional_text(self) -> Option<&'a str> {
            self.0.as_str()
        }

        fn number(self) -> u64 {
            self.0
                .as_u64()
                .expect("fixture value is an unsigned integer")
        }

        fn boolean(self) -> bool {
            self.0.as_bool().expect("fixture value is a boolean")
        }

        fn base64url(self) -> Vec<u8> {
            base64_url_decode(self.text()).expect("fixture base64url is valid")
        }

        fn entries(self) -> impl Iterator<Item = Self> + 'a {
            self.0
                .as_array()
                .expect("fixture value is an array")
                .iter()
                .map(Self)
        }

        fn pair(self) -> (&'a str, &'a str) {
            let mut entries = self.entries();
            let first = entries.next().expect("fixture pair has a key").text();
            let second = entries.next().expect("fixture pair has a value").text();
            assert!(entries.next().is_none(), "fixture pair has two values");
            (first, second)
        }
    }

    fn fixture() -> Fixture {
        Fixture::parse()
    }

    fn object_kind(value: &str) -> ObjectContentKind {
        match value {
            "manifest" => ObjectContentKind::Manifest,
            "chunk" => ObjectContentKind::Chunk,
            _ => panic!("unknown object content kind"),
        }
    }

    fn share_domain(value: &str) -> SharePayloadDomain {
        match value {
            "manifest" => SharePayloadDomain::Manifest,
            "chunk" => SharePayloadDomain::Chunk,
            _ => panic!("unknown share payload domain"),
        }
    }

    #[test]
    fn frozen_canonical_records_match_worker_bytes_signatures_and_failures() {
        let fixture = fixture();
        let request = fixture.field("canonical_request");
        let nonce = request.field("nonce").base64url();
        let query = request
            .field("query")
            .entries()
            .map(|pair| pair.pair())
            .collect::<Vec<_>>();
        let request_input = CanonicalRequestInput {
            audience: request.field("audience").text(),
            vault_id: request.field("vaultId").text(),
            device_id: request.field("deviceId").text(),
            method: request.field("method").text(),
            path: request.field("path").text(),
            query: &query,
            body_digest: request.field("bodyDigest").text(),
            content_type: request.field("contentType").text(),
            idempotency_key: request.field("idempotencyKey").text(),
            timestamp: request.field("timestamp").number(),
            nonce: &nonce,
        };
        let request_public_key = request.field("ed25519_public_key").base64url();
        let request_signature = request.field("signature").base64url();
        assert_eq!(
            canonical_request_bytes(&request_input).expect("request record"),
            request.field("bytes").base64url()
        );
        assert_eq!(
            ed25519_public_key(&FIXTURE_SIGNING_SEED)
                .expect("fixture public key")
                .to_vec(),
            request_public_key
        );
        assert_eq!(
            sign_canonical_request(&request_input, &FIXTURE_SIGNING_SEED)
                .expect("request signature")
                .to_vec(),
            request_signature
        );
        assert!(
            verify_canonical_request(&request_input, &request_public_key, &request_signature)
                .expect("request signature verifies")
        );
        let request_failures = fixture
            .field("expected_failures")
            .field("canonical_request");
        assert_eq!(request_failures.field("mutated_field").text(), "method");
        let mutated_request = CanonicalRequestInput {
            method: "PUT",
            ..request_input
        };
        assert_eq!(
            verify_canonical_request(&mutated_request, &request_public_key, &request_signature)
                .expect("mutated request signature result"),
            request_failures.field("signature_valid").boolean()
        );

        let upload = fixture.field("upload_envelope");
        let chunks = upload
            .field("chunks")
            .entries()
            .map(|chunk| UploadChunk {
                index: chunk.field("index").number(),
                size: chunk.field("size").number(),
                sha256: chunk.field("sha256").text(),
            })
            .collect::<Vec<_>>();
        let upload_input = CanonicalUploadEnvelopeInput {
            vault_id: upload.field("vaultId").text(),
            kind: UploadKind::Object,
            object_id: upload.field("objectId").optional_text(),
            revision_id: upload.field("revisionId").optional_text(),
            base_revision_id: upload.field("baseRevisionId").optional_text(),
            share_id: upload.field("shareId").optional_text(),
            manifest_digest: upload.field("manifestDigest").text(),
            crypto_version: upload.field("cryptoVersion").number(),
            total_bytes: upload.field("totalBytes").number(),
            chunks: &chunks,
        };
        let upload_public_key = upload.field("ed25519_public_key").base64url();
        let upload_signature = upload.field("signature").base64url();
        assert_eq!(
            canonical_upload_envelope_bytes(&upload_input).expect("upload record"),
            upload.field("bytes").base64url()
        );
        assert_eq!(
            sign_canonical_upload_envelope(&upload_input, &FIXTURE_SIGNING_SEED)
                .expect("upload signature")
                .to_vec(),
            upload_signature
        );
        assert!(verify_canonical_upload_envelope(
            &upload_input,
            &upload_public_key,
            &upload_signature
        )
        .expect("upload signature verifies"));
        let upload_failures = fixture.field("expected_failures").field("upload_envelope");
        assert_eq!(
            upload_failures.field("mutated_field").text(),
            "manifestDigest"
        );
        let mutated_upload = CanonicalUploadEnvelopeInput {
            manifest_digest: "mutated",
            ..upload_input
        };
        assert_eq!(
            verify_canonical_upload_envelope(
                &mutated_upload,
                &upload_public_key,
                &upload_signature
            )
            .expect("mutated upload signature result"),
            upload_failures.field("signature_valid").boolean()
        );

        let tombstone = fixture.field("tombstone");
        let tombstone_input = CanonicalTombstoneInput {
            vault_id: tombstone.field("vaultId").text(),
            object_id: tombstone.field("objectId").text(),
            tombstone_revision_id: tombstone.field("tombstoneRevisionId").text(),
            base_revision_id: tombstone.field("baseRevisionId").text(),
            reason: tombstone.field("reason").text(),
            format_version: tombstone.field("formatVersion").number(),
        };
        let tombstone_public_key = tombstone.field("ed25519_public_key").base64url();
        let tombstone_signature = tombstone.field("signature").base64url();
        assert_eq!(
            canonical_tombstone_bytes(&tombstone_input).expect("tombstone record"),
            tombstone.field("bytes").base64url()
        );
        assert_eq!(
            sign_canonical_tombstone(&tombstone_input, &FIXTURE_SIGNING_SEED)
                .expect("tombstone signature")
                .to_vec(),
            tombstone_signature
        );
        assert!(verify_canonical_tombstone(
            &tombstone_input,
            &tombstone_public_key,
            &tombstone_signature
        )
        .expect("tombstone signature verifies"));
        let tombstone_failures = fixture.field("expected_failures").field("tombstone");
        assert_eq!(tombstone_failures.field("mutated_field").text(), "reason");
        let mutated_tombstone = CanonicalTombstoneInput {
            reason: "mutated",
            ..tombstone_input
        };
        assert_eq!(
            verify_canonical_tombstone(
                &mutated_tombstone,
                &tombstone_public_key,
                &tombstone_signature
            )
            .expect("mutated tombstone signature result"),
            tombstone_failures.field("signature_valid").boolean()
        );
    }
    #[test]
    fn frozen_object_revision_vectors_match_worker_and_failure_vectors() {
        let fixture = fixture();
        let object = fixture.field("object_revision_aes_gcm_hkdf");
        let vault_root = object.field("vault_root").base64url();
        assert_eq!(
            object_revision_root_info(
                object.field("vault_id").text(),
                object.field("object_id").text(),
                object.field("revision_id").text()
            )
            .expect("revision root info"),
            object.field("root_hkdf_info").base64url()
        );
        assert_eq!(
            derive_object_revision_root(
                &vault_root,
                object.field("vault_id").text(),
                object.field("object_id").text(),
                object.field("revision_id").text()
            )
            .expect("revision root")
            .to_vec(),
            object.field("revision_root").base64url()
        );
        let failures = fixture.field("expected_failures").field("object_revision");
        assert_eq!(failures.field("mutated_field").text(), "sourceFormat");
        let truncated_bytes = usize::try_from(failures.field("truncated_payload_bytes").number())
            .expect("fixture truncated payload length fits usize");
        let expected_decrypts = failures.field("decrypts").boolean();

        for name in ["manifest", "chunk"] {
            let vector = object.field(name);
            let context = ObjectRevisionCryptoContext {
                vault_id: object.field("vault_id").text(),
                object_id: object.field("object_id").text(),
                revision_id: object.field("revision_id").text(),
                index: vector.field("index").number(),
                total: vector.field("total").number(),
                content_kind: object_kind(vector.field("content_kind").text()),
                source_format: vector.field("source_format").text(),
            };
            let nonce = vector.field("nonce").base64url();
            let plaintext = vector.field("plaintext").base64url();
            let ciphertext = vector.field("ciphertext").base64url();
            assert_eq!(
                object_revision_key_info(&context).expect("object key info"),
                vector.field("key_info").base64url()
            );
            assert_eq!(
                object_revision_aad(&context).expect("object AAD"),
                vector.field("aad").base64url()
            );
            assert_eq!(
                seal_object_revision_payload(&vault_root, &context, &nonce, &plaintext)
                    .expect("object sealing"),
                ciphertext
            );
            assert_eq!(
                open_object_revision_payload(&vault_root, &context, &ciphertext)
                    .expect("object opening"),
                plaintext
            );
            let mutated_context = ObjectRevisionCryptoContext {
                source_format: "mutated-format",
                ..context
            };
            assert_eq!(
                open_object_revision_payload(&vault_root, &mutated_context, &ciphertext).is_ok(),
                expected_decrypts
            );
            assert_eq!(
                open_object_revision_payload(&vault_root, &context, &ciphertext[..truncated_bytes])
                    .is_ok(),
                expected_decrypts
            );
        }
    }

    #[test]
    fn frozen_share_vectors_match_worker_and_failure_vectors() {
        let fixture = fixture();
        let share = fixture.field("share_aes_gcm_hkdf");
        let root = share.field("root").base64url();
        let failures = fixture.field("expected_failures").field("share");
        assert_eq!(failures.field("mutated_field").text(), "domain");
        let truncated_bytes = usize::try_from(failures.field("truncated_payload_bytes").number())
            .expect("fixture truncated payload length fits usize");
        let expected_decrypts = failures.field("decrypts").boolean();

        for name in ["manifest", "chunk"] {
            let vector = share.field(name);
            let context = SharePayloadContext {
                share_id: share.field("share_id").text(),
                index: vector.field("index").number(),
                total: vector.field("total").number(),
                domain: share_domain(vector.field("domain").text()),
            };
            let nonce = vector.field("nonce").base64url();
            let plaintext = vector.field("plaintext").base64url();
            let ciphertext = vector.field("ciphertext").base64url();
            assert_eq!(
                share_payload_key_info(&context).expect("share key info"),
                vector.field("key_info").base64url()
            );
            assert_eq!(
                share_payload_aad(&context).expect("share AAD"),
                vector.field("aad").base64url()
            );
            assert_eq!(
                seal_share_payload(&root, &context, &nonce, &plaintext).expect("share sealing"),
                ciphertext
            );
            assert_eq!(
                open_share_payload(&root, &context, &ciphertext).expect("share opening"),
                plaintext
            );
            let mutated_context = SharePayloadContext {
                domain: match context.domain {
                    SharePayloadDomain::Manifest => SharePayloadDomain::Chunk,
                    SharePayloadDomain::Chunk => SharePayloadDomain::Manifest,
                },
                ..context
            };
            assert_eq!(
                open_share_payload(&root, &mutated_context, &ciphertext).is_ok(),
                expected_decrypts
            );
            assert_eq!(
                open_share_payload(&root, &context, &ciphertext[..truncated_bytes]).is_ok(),
                expected_decrypts
            );
        }
    }

    #[test]
    fn frozen_live_room_vectors_match_the_page_and_worker() {
        let fixture = fixture();
        let live = fixture.field("live_room_aes_gcm_hkdf");
        let room_id = live.field("room_id").text();
        let keys = derive_live_room_keys(&live.field("root").base64url(), room_id)
            .expect("live room keys");
        assert_eq!(
            keys.join_token.to_vec(),
            live.field("join_token").base64url()
        );
        assert_eq!(keys.frame_key.to_vec(), live.field("frame_key").base64url());
        assert_eq!(
            sha256_base64url(&keys.join_token),
            live.field("join_verifier").text()
        );
        assert_eq!(
            sha256_base64url(&live.field("host_token").base64url()),
            live.field("host_verifier").text()
        );

        for kind in ["state", "edit"] {
            let vector = live.field(kind);
            let seq = vector.field("seq").number();
            let nonce =
                fixed_array::<AES_GCM_NONCE_BYTES>(&vector.field("nonce").base64url(), "nonce")
                    .expect("fixture nonce is 12 bytes");
            let plaintext = vector.field("plaintext").base64url();
            let sealed = vector.field("box").base64url();
            assert_eq!(
                live_frame_aad(room_id, kind, seq).expect("live AAD"),
                vector.field("aad").base64url()
            );
            assert_eq!(
                seal_live_frame(&keys.frame_key, room_id, kind, seq, &nonce, &plaintext)
                    .expect("live sealing"),
                sealed
            );
            assert_eq!(
                open_live_frame(&keys.frame_key, room_id, kind, seq, &sealed)
                    .expect("live opening"),
                plaintext
            );
            assert!(
                open_live_frame(&keys.frame_key, room_id, kind, seq + 1, &sealed).is_err(),
                "a {kind} frame opens only at its own seq"
            );
        }
    }

    #[test]
    fn pairing_records_recovery_codes_and_local_envelopes_are_authenticated() {
        let candidate_seed = [7_u8; ED25519_PUBLIC_KEY_BYTES];
        let candidate_public = ed25519_public_key(&candidate_seed).expect("candidate public key");
        let candidate_pairing_public =
            x25519_public_key(&[8_u8; X25519_KEY_BYTES]).expect("candidate pairing public key");
        let bootstrap = CanonicalBootstrapInput {
            audience: "sona-companion",
            vault_id: "vault",
            device_id: "device",
            signing_public_key: &candidate_public,
            pairing_public_key: &candidate_pairing_public,
        };
        let bootstrap_signature =
            sign_canonical_bootstrap(&bootstrap, &candidate_seed).expect("bootstrap signature");
        assert!(
            verify_canonical_bootstrap(&bootstrap, &candidate_public, &bootstrap_signature)
                .expect("bootstrap verification")
        );

        let pairing_nonce = [9_u8; 16];
        let candidate = CanonicalPairCandidateInput {
            audience: "sona-companion",
            vault_id: "vault",
            candidate_device_id: "candidate",
            candidate_signing_public_key: &candidate_public,
            candidate_pairing_public_key: &candidate_pairing_public,
            pairing_nonce: &pairing_nonce,
            expires_at: 1_700_000_000_000,
        };
        let candidate_record =
            canonical_pair_candidate_bytes(&candidate).expect("candidate record");
        let proof =
            sign_canonical_pair_candidate(&candidate, &candidate_seed).expect("candidate proof");
        assert!(
            verify_canonical_pair_candidate(&candidate, &candidate_public, &proof)
                .expect("candidate proof verification")
        );
        let approver_seed = [10_u8; ED25519_PUBLIC_KEY_BYTES];
        let approver_public = ed25519_public_key(&approver_seed).expect("approver public key");
        let approval = CanonicalPairApprovalInput {
            vault_id: "vault",
            candidate_record: &candidate_record,
            candidate_proof: &proof,
            envelope: b"opaque-envelope",
        };
        let approval_signature =
            sign_canonical_pair_approval(&approval, &approver_seed).expect("approval signature");
        assert!(
            verify_canonical_pair_approval(&approval, &approver_public, &approval_signature)
                .expect("approval verification")
        );

        let root = [0x42_u8; AES_GCM_KEY_BYTES];
        let recovery = encode_recovery_code("vault", &root).expect("recovery encoding");
        assert!(!recovery.contains('='));
        assert_eq!(
            decode_recovery_code(&recovery).expect("recovery decoding"),
            RecoveryCode {
                vault_id: "vault".to_owned(),
                vault_root: root
            }
        );
        let mut bad_recovery = base64_url_decode(&recovery).expect("recovery bytes");
        *bad_recovery.last_mut().expect("checksum byte") ^= 1;
        assert_eq!(
            decode_recovery_code(&base64_url_encode(&bad_recovery)),
            Err(CryptoError::InvalidRecoveryChecksum)
        );
        let mut wrong_version = length_prefixed_record(&[
            RecordField::Text("sona-recovery-v1"),
            RecordField::Text("2"),
            RecordField::Text("vault"),
            RecordField::Bytes(&root),
        ])
        .expect("recovery version record");
        let wrong_version_checksum = sha256_digest(&wrong_version);
        wrong_version.extend_from_slice(&wrong_version_checksum);
        assert_eq!(
            decode_recovery_code(&base64_url_encode(&wrong_version)),
            Err(CryptoError::InvalidRecoveryVersion)
        );
        let short_root = [0_u8; AES_GCM_KEY_BYTES - 1];
        let mut wrong_length = length_prefixed_record(&[
            RecordField::Text("sona-recovery-v1"),
            RecordField::Text("1"),
            RecordField::Text("vault"),
            RecordField::Bytes(&short_root),
        ])
        .expect("recovery length record");
        let wrong_length_checksum = sha256_digest(&wrong_length);
        wrong_length.extend_from_slice(&wrong_length_checksum);
        assert_eq!(
            decode_recovery_code(&base64_url_encode(&wrong_length)),
            Err(CryptoError::InvalidRecoveryCode)
        );
        assert_eq!(
            base64_url_decode("AA=="),
            Err(CryptoError::InvalidBase64Url)
        );

        let recipient_secret = [0x11_u8; X25519_KEY_BYTES];
        let recipient_public = x25519_public_key(&recipient_secret).expect("recipient public key");
        let ephemeral_secret = [0x22_u8; X25519_KEY_BYTES];
        let nonce = [0x33_u8; AES_GCM_NONCE_BYTES];
        let input = PairingEnvelopeSealInput {
            recipient_public_key: &recipient_public,
            ephemeral_secret_key: &ephemeral_secret,
            nonce: &nonce,
            vault_root: &root,
        };
        let envelope = seal_pairing_envelope(&input).expect("pairing envelope");
        assert_eq!(
            seal_pairing_envelope(&input).expect("deterministic envelope"),
            envelope
        );
        let fields = decode_length_prefixed_record(&envelope).expect("envelope fields");
        assert_eq!(fields[0], b"sona-pairing-envelope-v1");
        assert_eq!(fields[1], b"1");
        assert_eq!(fields[2].len(), X25519_KEY_BYTES);
        assert_eq!(fields[3], nonce.as_slice());
        assert_eq!(fields[4].len(), AES_GCM_KEY_BYTES + AES_GCM_TAG_BYTES);
        assert_eq!(
            open_pairing_envelope(&recipient_secret, &envelope).expect("envelope open"),
            root
        );
        let mut tampered = envelope.clone();
        *tampered.last_mut().expect("ciphertext byte") ^= 1;
        assert_eq!(
            open_pairing_envelope(&recipient_secret, &tampered),
            Err(CryptoError::AuthenticationFailed)
        );
    }

    /// A deterministic generator. Seeded per case so a failure prints the seed that
    /// reproduces it, and no case depends on the order the others ran in.
    struct Xorshift(u64);

    impl Xorshift {
        fn next_u64(&mut self) -> u64 {
            let mut state = self.0;
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            self.0 = state;
            state
        }

        fn bytes(&mut self, length: usize) -> Vec<u8> {
            (0..length)
                .map(|_| u8::try_from(self.next_u64() & 0xff).expect("masked to one byte"))
                .collect()
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next_u64() % bound
        }
    }

    /// The two AES-GCM envelopes that carry user content off this machine. They are
    /// separate code paths over the same contract, so the property is stated once and
    /// run against both.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Envelope {
        ObjectRevision,
        Share,
    }

    /// Everything a sealed payload is bound to. Neighbouring cases differ from a case
    /// in exactly one of these fields.
    #[derive(Clone, Debug)]
    struct Binding {
        root: Vec<u8>,
        id: String,
        index: u64,
        total: u64,
        manifest: bool,
    }

    impl Binding {
        fn generate(random: &mut Xorshift) -> Self {
            let total = random.below(8) + 1;
            Self {
                root: random.bytes(AES_GCM_KEY_BYTES),
                id: format!("id{:016x}", random.next_u64()),
                index: random.below(total),
                total,
                manifest: random.next_u64() & 1 == 0,
            }
        }

        /// Every one-field neighbour of this binding, named for the failure message.
        /// A neighbour that cannot exist (there is no other index when total is one)
        /// is left out rather than fudged.
        fn neighbours(&self) -> Vec<(&'static str, Self)> {
            let mut neighbours = vec![
                (
                    "root",
                    Self {
                        root: self.root.iter().map(|byte| byte ^ 0x5a).collect(),
                        ..self.clone()
                    },
                ),
                (
                    "id",
                    Self {
                        id: format!("{}x", self.id),
                        ..self.clone()
                    },
                ),
                (
                    "total",
                    Self {
                        total: self.total + 1,
                        ..self.clone()
                    },
                ),
                (
                    "content kind",
                    Self {
                        manifest: !self.manifest,
                        ..self.clone()
                    },
                ),
            ];
            if self.index + 1 < self.total {
                neighbours.push((
                    "index",
                    Self {
                        index: self.index + 1,
                        ..self.clone()
                    },
                ));
            }
            neighbours
        }
    }

    impl Envelope {
        fn seal(
            self,
            binding: &Binding,
            nonce: &[u8],
            plaintext: &[u8],
        ) -> Result<Vec<u8>, CryptoError> {
            match self {
                Self::ObjectRevision => seal_object_revision_payload(
                    &binding.root,
                    &object_context(binding),
                    nonce,
                    plaintext,
                ),
                Self::Share => {
                    seal_share_payload(&binding.root, &share_context(binding), nonce, plaintext)
                }
            }
        }

        fn open(self, binding: &Binding, payload: &[u8]) -> Result<Vec<u8>, CryptoError> {
            match self {
                Self::ObjectRevision => {
                    open_object_revision_payload(&binding.root, &object_context(binding), payload)
                }
                Self::Share => open_share_payload(&binding.root, &share_context(binding), payload),
            }
        }
    }

    fn object_context(binding: &Binding) -> ObjectRevisionCryptoContext<'_> {
        ObjectRevisionCryptoContext {
            vault_id: "vault-id",
            object_id: &binding.id,
            revision_id: "revision-id",
            index: binding.index,
            total: binding.total,
            content_kind: if binding.manifest {
                ObjectContentKind::Manifest
            } else {
                ObjectContentKind::Chunk
            },
            source_format: "sona-meeting-bundle-v1",
        }
    }

    fn share_context(binding: &Binding) -> SharePayloadContext<'_> {
        SharePayloadContext {
            share_id: &binding.id,
            index: binding.index,
            total: binding.total,
            domain: if binding.manifest {
                SharePayloadDomain::Manifest
            } else {
                SharePayloadDomain::Chunk
            },
        }
    }

    /// The invariant both egress envelopes owe: a sealed payload opens to exactly the
    /// plaintext under the binding it was sealed with, and under nothing else. Any
    /// mutation of the sealed bytes, any truncation of them, and any single-field
    /// change to the binding all fail to open rather than returning some other
    /// plaintext.
    #[test]
    fn a_sealed_payload_opens_only_under_the_binding_it_was_sealed_with() {
        for envelope in [Envelope::ObjectRevision, Envelope::Share] {
            for case in 0..64_u64 {
                let seed = 0x2545_f491_4f6c_dd1d
                    ^ (case << 8)
                    ^ match envelope {
                        Envelope::ObjectRevision => 0,
                        Envelope::Share => 1,
                    };
                let mut random = Xorshift(seed);
                let context = format!("{envelope:?} seed {seed:#x}");
                let binding = Binding::generate(&mut random);
                let nonce = random.bytes(AES_GCM_NONCE_BYTES);
                let plaintext_len = usize::try_from(random.below(65)).expect("small");
                let plaintext = random.bytes(plaintext_len);

                let sealed = envelope
                    .seal(&binding, &nonce, &plaintext)
                    .unwrap_or_else(|error| panic!("{context}: seal failed: {error}"));
                assert_eq!(
                    sealed.len(),
                    AES_GCM_NONCE_BYTES + plaintext.len() + AES_GCM_TAG_BYTES,
                    "{context}: sealed payload is nonce || ciphertext || tag"
                );
                assert_eq!(
                    envelope.open(&binding, &sealed).as_deref(),
                    Ok(plaintext.as_slice()),
                    "{context}: round trip"
                );

                for position in 0..sealed.len() {
                    let mut mutated = sealed.clone();
                    mutated[position] ^= 1 << (position % 8);
                    assert_eq!(
                        envelope.open(&binding, &mutated),
                        Err(CryptoError::AuthenticationFailed),
                        "{context}: mutating byte {position} must not open"
                    );
                }

                for length in 0..sealed.len() {
                    assert!(
                        envelope.open(&binding, &sealed[..length]).is_err(),
                        "{context}: a {length}-byte prefix must not open"
                    );
                }

                for (field, neighbour) in binding.neighbours() {
                    assert_eq!(
                        envelope.open(&neighbour, &sealed),
                        Err(CryptoError::AuthenticationFailed),
                        "{context}: a payload must not open under a different {field}"
                    );
                }
            }
        }
    }
}
