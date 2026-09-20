import CryptoKit
import Foundation

enum SonaCryptoError: Error, Equatable {
    case invalidLength(field: String, expected: Int, actual: Int)
    case invalidRecord
    case invalidObjectRevisionContext
    case invalidPairingEnvelope
    case invalidPairingEnvelopeVersion
    case invalidPairingKey
    case authenticationFailed
    case encryptionFailed
}

private let aesGcmNonceBytes = 12
private let aesGcmTagBytes = 16
private let keyBytes = 32
private let maxSafeInteger: UInt64 = 9_007_199_254_740_991

// MARK: - Digests and keys

func sha256Digest(_ bytes: Data) -> Data {
    Data(SHA256.hash(data: bytes))
}

/// SHA-256 as unpadded base64url, the shape of every digest field on the wire.
func sha256Base64URL(_ bytes: Data) -> String {
    Base64URL.encode(sha256Digest(bytes))
}

func ed25519PublicKey(signingSeed: Data) throws -> Data {
    try Curve25519.Signing.PrivateKey(rawRepresentation: signingSeed)
        .publicKey.rawRepresentation
}

func signEd25519(signingSeed: Data, message: Data) throws -> Data {
    try Curve25519.Signing.PrivateKey(rawRepresentation: signingSeed).signature(for: message)
}

func verifyEd25519(publicKey: Data, signature: Data, message: Data) -> Bool {
    guard publicKey.count == keyBytes, signature.count == 64,
          let key = try? Curve25519.Signing.PublicKey(rawRepresentation: publicKey)
    else { return false }
    return key.isValidSignature(signature, for: message)
}

func x25519PublicKey(secretKey: Data) throws -> Data {
    try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: secretKey)
        .publicKey.rawRepresentation
}

/// A contributory X25519 agreement, rejecting the all-zero shared secret the Rust
/// authority rejects through `was_contributory`.
func x25519SharedSecret(secretKey: Data, publicKey: Data) throws -> Data {
    let secret = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: secretKey)
    let peer = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: publicKey)
    let shared = try secret.sharedSecretFromKeyAgreement(with: peer)
    let bytes = shared.withUnsafeBytes { Data($0) }
    guard bytes.contains(where: { $0 != 0 }) else { throw SonaCryptoError.invalidPairingKey }
    return bytes
}

func randomBytes(_ count: Int) -> Data {
    var bytes = Data(count: count)
    bytes.withUnsafeMutableBytes { buffer in
        guard let base = buffer.baseAddress else { return }
        _ = SecRandomCopyBytes(kSecRandomDefault, count, base)
    }
    return bytes
}

/// A random opaque id in the Worker's `[A-Za-z0-9_-]{16,128}` alphabet, matching the
/// desktop's `random_opaque_id` (24 random bytes as base64url).
func randomOpaqueId() -> String {
    Base64URL.encode(randomBytes(24))
}

// MARK: - Signed records

struct CanonicalRequestInput {
    var audience = SonaProtocol.audience
    var vaultId: String
    var deviceId: String
    var method: String
    var path: String
    var query: [(String, String)]
    var bodyDigest: String
    var contentType: String
    var idempotencyKey: String
    var timestamp: UInt64
    var nonce: Data
}

func canonicalRequestBytes(_ input: CanonicalRequestInput) -> Data {
    let sorted = input.query.sorted { left, right in
        left.0 == right.0
            ? workerStringIsOrderedBefore(left.1, right.1)
            : workerStringIsOrderedBefore(left.0, right.0)
    }
    let queryBytes = canonicalNestedRecords(
        sorted.map { canonicalRecord([.text($0.0), .text($0.1)]) }
    )
    return canonicalRecord([
        .text("sona-request-v1"),
        .text(input.audience),
        .text(input.vaultId),
        .text(input.deviceId),
        .text(input.method),
        .text(input.path),
        .bytes(queryBytes),
        .text(input.bodyDigest),
        .text(input.contentType),
        .text(input.idempotencyKey),
        .decimal(input.timestamp),
        .bytes(input.nonce),
    ])
}

func canonicalBootstrapBytes(
    vaultId: String,
    deviceId: String,
    signingPublicKey: Data,
    pairingPublicKey: Data
) -> Data {
    canonicalRecord([
        .text("sona-bootstrap-v1"),
        .text(SonaProtocol.audience),
        .text(vaultId),
        .text(deviceId),
        .bytes(signingPublicKey),
        .bytes(pairingPublicKey),
    ])
}

func canonicalPairCandidateBytes(
    vaultId: String,
    candidateDeviceId: String,
    candidateSigningPublicKey: Data,
    candidatePairingPublicKey: Data,
    pairingNonce: Data,
    expiresAt: UInt64
) -> Data {
    canonicalRecord([
        .text("sona-pair-candidate-v1"),
        .text(SonaProtocol.audience),
        .text(vaultId),
        .text(candidateDeviceId),
        .bytes(candidateSigningPublicKey),
        .bytes(candidatePairingPublicKey),
        .bytes(pairingNonce),
        .decimal(expiresAt),
    ])
}

func canonicalPairApprovalBytes(
    vaultId: String,
    candidateRecord: Data,
    candidateProof: Data,
    envelope: Data
) -> Data {
    canonicalRecord([
        .text("sona-pair-approval-v1"),
        .text(vaultId),
        .bytes(candidateRecord),
        .bytes(candidateProof),
        .bytes(envelope),
    ])
}

struct UploadEnvelopeChunk {
    var index: UInt64
    var size: UInt64
    var sha256: String
}

struct CanonicalUploadEnvelopeInput {
    var vaultId: String
    var kind: String
    var objectId: String?
    var revisionId: String?
    var baseRevisionId: String?
    var shareId: String?
    var manifestDigest: String
    var cryptoVersion: UInt64
    var totalBytes: UInt64
    var chunks: [UploadEnvelopeChunk]
}

func canonicalUploadEnvelopeBytes(_ input: CanonicalUploadEnvelopeInput) -> Data {
    let chunkBytes = canonicalNestedRecords(
        input.chunks.map {
            canonicalRecord([.decimal($0.index), .decimal($0.size), .text($0.sha256)])
        }
    )
    return canonicalRecord([
        .text("sona-upload-envelope-v1"),
        .text(input.vaultId),
        .text(input.kind),
        .text(input.objectId ?? ""),
        .text(input.revisionId ?? ""),
        .text(input.baseRevisionId ?? ""),
        .text(input.shareId ?? ""),
        .text(input.manifestDigest),
        .decimal(input.cryptoVersion),
        .decimal(input.totalBytes),
        .decimal(UInt64(input.chunks.count)),
        .bytes(chunkBytes),
    ])
}

struct CanonicalTombstoneInput {
    var vaultId: String
    var objectId: String
    var tombstoneRevisionId: String
    var baseRevisionId: String
    var reason: String
    var formatVersion: UInt64
}

/// The record a writer signs to tombstone an object, as `crypto.ts` lays it out.
func canonicalTombstoneBytes(_ input: CanonicalTombstoneInput) -> Data {
    canonicalRecord([
        .text("sona-tombstone-v1"),
        .text(input.vaultId),
        .text(input.objectId),
        .text(input.tombstoneRevisionId),
        .text(input.baseRevisionId),
        .text(input.reason),
        .decimal(input.formatVersion),
    ])
}

// MARK: - Object revision payloads

enum ObjectContentKind: String {
    case manifest
    case chunk
}

/// Metadata which binds a revision payload's HKDF key and AES-GCM AAD.
struct ObjectRevisionCryptoContext {
    var vaultId: String
    var objectId: String
    var revisionId: String
    var index: UInt64
    var total: UInt64
    var contentKind: ObjectContentKind
    var sourceFormat: String
}

func objectRevisionRootInfo(vaultId: String, objectId: String, revisionId: String) -> Data {
    canonicalRecord([
        .text("sona-revision-root-v1"),
        .text(vaultId),
        .text(objectId),
        .text(revisionId),
    ])
}

func deriveObjectRevisionRoot(
    vaultRoot: Data,
    vaultId: String,
    objectId: String,
    revisionId: String
) throws -> Data {
    try fixedLength(vaultRoot, keyBytes, "vault root")
    return deriveAesGcmKey(
        material: vaultRoot,
        salt: Data("sona-revision-v1".utf8),
        info: objectRevisionRootInfo(vaultId: vaultId, objectId: objectId, revisionId: revisionId)
    )
}

func objectRevisionKeyInfo(_ context: ObjectRevisionCryptoContext) throws -> Data {
    try validate(context)
    return canonicalRecord([
        .text("sona-object-key-v1"),
        .text(context.vaultId),
        .text(context.objectId),
        .text(context.revisionId),
        .decimal(context.index),
        .decimal(context.total),
        .text(context.contentKind.rawValue),
        .text(context.sourceFormat),
    ])
}

func objectRevisionAad(_ context: ObjectRevisionCryptoContext) throws -> Data {
    try validate(context)
    return canonicalRecord([
        .text("sona-object-aad-v1"),
        .text(context.vaultId),
        .text(context.objectId),
        .text(context.revisionId),
        .decimal(context.index),
        .decimal(context.total),
        .text(context.contentKind.rawValue),
        .text(context.sourceFormat),
    ])
}

func deriveObjectRevisionKey(
    vaultRoot: Data,
    context: ObjectRevisionCryptoContext
) throws -> Data {
    let revisionRoot = try deriveObjectRevisionRoot(
        vaultRoot: vaultRoot,
        vaultId: context.vaultId,
        objectId: context.objectId,
        revisionId: context.revisionId
    )
    return deriveAesGcmKey(
        material: revisionRoot,
        salt: Data("sona-object-v1".utf8),
        info: try objectRevisionKeyInfo(context)
    )
}

/// Seal an object revision payload as nonce_12 || ciphertext || tag_16.
func sealObjectRevisionPayload(
    vaultRoot: Data,
    context: ObjectRevisionCryptoContext,
    nonce: Data,
    plaintext: Data
) throws -> Data {
    try fixedLength(nonce, aesGcmNonceBytes, "object payload nonce")
    let key = try deriveObjectRevisionKey(vaultRoot: vaultRoot, context: context)
    let aad = try objectRevisionAad(context)
    guard let sealed = try? AES.GCM.seal(
        plaintext,
        using: SymmetricKey(data: key),
        nonce: AES.GCM.Nonce(data: nonce),
        authenticating: aad
    ) else { throw SonaCryptoError.encryptionFailed }
    return nonce + sealed.ciphertext + sealed.tag
}

/// Open an object revision payload encoded as nonce_12 || ciphertext || tag_16.
func openObjectRevisionPayload(
    vaultRoot: Data,
    context: ObjectRevisionCryptoContext,
    encryptedPayload: Data
) throws -> Data {
    let minimum = aesGcmNonceBytes + aesGcmTagBytes
    guard encryptedPayload.count >= minimum else {
        throw SonaCryptoError.invalidLength(
            field: "object encrypted payload", expected: minimum, actual: encryptedPayload.count
        )
    }
    let key = try deriveObjectRevisionKey(vaultRoot: vaultRoot, context: context)
    let aad = try objectRevisionAad(context)
    guard let opened = try? AES.GCM.open(
        try AES.GCM.SealedBox(combined: encryptedPayload),
        using: SymmetricKey(data: key),
        authenticating: aad
    ) else { throw SonaCryptoError.authenticationFailed }
    return opened
}

// MARK: - Shared-link payloads

enum SharePayloadDomain: String {
    case manifest
    case chunk
}

struct SharePayloadContext {
    var shareId: String
    var index: UInt64
    var total: UInt64
    var domain: SharePayloadDomain
}

func sharePayloadKeyInfo(_ context: SharePayloadContext) -> Data {
    canonicalRecord([
        .text("sona-share-key-v1"),
        .text(context.shareId),
        .decimal(context.index),
        .decimal(context.total),
        .text(context.domain.rawValue),
    ])
}

func sharePayloadAad(_ context: SharePayloadContext) -> Data {
    canonicalRecord([
        .text("sona-share-aad-v1"),
        .text(context.shareId),
        .decimal(context.index),
        .decimal(context.total),
        .text(context.domain.rawValue),
    ])
}

func deriveSharePayloadKey(root: Data, context: SharePayloadContext) throws -> Data {
    try fixedLength(root, keyBytes, "share root")
    return deriveAesGcmKey(
        material: root,
        salt: Data("sona-share-v1".utf8),
        info: sharePayloadKeyInfo(context)
    )
}

func openSharePayload(
    root: Data,
    context: SharePayloadContext,
    encryptedPayload: Data
) throws -> Data {
    let minimum = aesGcmNonceBytes + aesGcmTagBytes
    guard encryptedPayload.count >= minimum else {
        throw SonaCryptoError.invalidLength(
            field: "share encrypted payload", expected: minimum, actual: encryptedPayload.count
        )
    }
    let key = try deriveSharePayloadKey(root: root, context: context)
    guard let opened = try? AES.GCM.open(
        try AES.GCM.SealedBox(combined: encryptedPayload),
        using: SymmetricKey(data: key),
        authenticating: sharePayloadAad(context)
    ) else { throw SonaCryptoError.authenticationFailed }
    return opened
}

// MARK: - Pairing envelope

let pairingEnvelopeVersion: UInt64 = 1

/// Open the desktop's pairing envelope and return the authenticated 32-byte vault root.
///
/// The phone is always the candidate, so it only ever opens one of these; sealing stays
/// on the approving desktop.
func openPairingEnvelope(recipientSecretKey: Data, envelope: Data) throws -> Data {
    guard let fields = decodeCanonicalRecord(envelope),
          fields.count == 5,
          fields[0] == Data("sona-pairing-envelope-v1".utf8)
    else { throw SonaCryptoError.invalidPairingEnvelope }
    guard fields[1] == Data("1".utf8) else {
        throw SonaCryptoError.invalidPairingEnvelopeVersion
    }
    let ephemeralPublicKey = fields[2]
    let nonce = fields[3]
    let ciphertext = fields[4]
    guard ephemeralPublicKey.count == keyBytes,
          nonce.count == aesGcmNonceBytes,
          ciphertext.count == keyBytes + aesGcmTagBytes
    else { throw SonaCryptoError.invalidPairingEnvelope }
    let recipientPublicKey = try x25519PublicKey(secretKey: recipientSecretKey)
    let shared = try x25519SharedSecret(
        secretKey: recipientSecretKey, publicKey: ephemeralPublicKey
    )
    let key = deriveAesGcmKey(
        material: shared,
        salt: Data("sona-pairing-envelope-v1".utf8),
        info: canonicalRecord([
            .text("sona-pairing-envelope-key-v1"),
            .bytes(recipientPublicKey),
            .bytes(ephemeralPublicKey),
        ])
    )
    let aad = canonicalRecord([
        .text("sona-pairing-envelope-aad-v1"),
        .decimal(pairingEnvelopeVersion),
        .bytes(recipientPublicKey),
        .bytes(ephemeralPublicKey),
        .bytes(nonce),
    ])
    guard let vaultRoot = try? AES.GCM.open(
        try AES.GCM.SealedBox(combined: nonce + ciphertext),
        using: SymmetricKey(data: key),
        authenticating: aad
    ), vaultRoot.count == keyBytes else {
        throw SonaCryptoError.authenticationFailed
    }
    return vaultRoot
}

// MARK: - Internals

private func deriveAesGcmKey(material: Data, salt: Data, info: Data) -> Data {
    HKDF<SHA256>.deriveKey(
        inputKeyMaterial: SymmetricKey(data: material),
        salt: salt,
        info: info,
        outputByteCount: keyBytes
    ).withUnsafeBytes { Data($0) }
}

private func fixedLength(_ bytes: Data, _ expected: Int, _ field: String) throws {
    guard bytes.count == expected else {
        throw SonaCryptoError.invalidLength(
            field: field, expected: expected, actual: bytes.count
        )
    }
}

private func validate(_ context: ObjectRevisionCryptoContext) throws {
    guard context.index <= maxSafeInteger,
          context.total >= 1,
          context.total <= maxSafeInteger,
          context.index < context.total,
          !context.sourceFormat.isEmpty
    else { throw SonaCryptoError.invalidObjectRevisionContext }
}
