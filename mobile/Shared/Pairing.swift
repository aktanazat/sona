import Foundation

/// The candidate record the desktop's `cloud_sync_pairing_approve` consumes.
///
/// Field names and types are the desktop's `CloudPairingOffer`; its zod schema in
/// `src/components/cloud-sync/cloudSync.ts` accepts exactly this JSON, so the phone
/// shows it verbatim as text and as a QR code.
struct PairingOffer: Codable, Equatable {
    var protocol_version: Int
    var vault_id: String
    var device_id: String
    var signing_public_key: String
    var pairing_public_key: String
    var candidate_proof: String
    var pairing_nonce: String
    var expires_at_utc_ms: Int64
    var fingerprint: String

    var json: String {
        let encoder = JSONEncoder()
        guard let bytes = try? encoder.encode(self),
              let text = String(data: bytes, encoding: .utf8)
        else { return "" }
        return text
    }
}

enum PairingError: Error, Equatable {
    /// The offer expired before the desktop approved it; only a new code can pair.
    case offerExpired
    /// The desktop has not approved this device yet. Its approval is what introduces the
    /// device to the Worker, so until then the Worker answers `unauthorized`.
    case notApprovedYet
    /// The Worker's record of this device does not match the offer it was shown.
    case identityMismatch
    case crypto
}

enum Pairing {
    /// Offers live 15 minutes, the window `pairing_approve` enforces on the desktop.
    static let offerLifetimeMs: Int64 = 15 * 60 * 1000

    /// Mint the candidate record and its proof.
    ///
    /// `nowUtcMs` is the phone's clock corrected by the Worker's `Date` header: the
    /// approving desktop rejects an expiry outside `(now, now + 15 min]` against its own
    /// corrected clock, so an uncorrected phone clock would fail pairing for no visible
    /// reason.
    static func candidateOffer(
        identity: DeviceIdentity,
        vaultId: String,
        nowUtcMs: Int64
    ) throws -> PairingOffer {
        let pairingNonce = randomBytes(16)
        let expiresAtUtcMs = nowUtcMs + offerLifetimeMs
        let record = canonicalPairCandidateBytes(
            vaultId: vaultId,
            candidateDeviceId: identity.deviceId,
            candidateSigningPublicKey: identity.signingPublicKey,
            candidatePairingPublicKey: identity.pairingPublicKey,
            pairingNonce: pairingNonce,
            expiresAt: UInt64(expiresAtUtcMs)
        )
        let proof = try signEd25519(signingSeed: identity.signingSeed, message: record)
        let fingerprint = String(Base64URL.encode(sha256Digest(record)).prefix(12))
        return PairingOffer(
            protocol_version: SonaProtocol.protocolVersion,
            vault_id: vaultId,
            device_id: identity.deviceId,
            signing_public_key: Base64URL.encode(identity.signingPublicKey),
            pairing_public_key: Base64URL.encode(identity.pairingPublicKey),
            candidate_proof: Base64URL.encode(proof),
            pairing_nonce: Base64URL.encode(pairingNonce),
            expires_at_utc_ms: expiresAtUtcMs,
            fingerprint: fingerprint
        )
    }

    /// Fetch the Worker's record of this device and open the approval it carries.
    static func finish(
        identity: DeviceIdentity,
        pending: PendingPairing
    ) async throws -> VaultCredentials {
        let client = try CompanionClient(endpoint: pending.endpoint)
        let device: SelfDeviceResponse
        do {
            device = try await client.selfDevice(identity: identity, vaultId: pending.vaultId)
        } catch CompanionError.api(code: "unauthorized", status: _) {
            let now = await client.nowUtcMs()
            throw now < pending.offer.expires_at_utc_ms
                ? PairingError.notApprovedYet
                : PairingError.offerExpired
        }
        return try acceptApproval(identity: identity, pending: pending, selfDevice: device)
    }

    /// Turn the desktop's approval into vault credentials.
    ///
    /// Mirrors `runtime.rs::pairing_accept`: the Worker's own record of this device must
    /// match the keys the offer published before its envelope is opened.
    static func acceptApproval(
        identity: DeviceIdentity,
        pending: PendingPairing,
        selfDevice: SelfDeviceResponse
    ) throws -> VaultCredentials {
        guard selfDevice.deviceId == identity.deviceId,
              selfDevice.status == "active",
              selfDevice.signingPublicKey == pending.offer.signing_public_key,
              selfDevice.pairingPublicKey == pending.offer.pairing_public_key
        else { throw PairingError.identityMismatch }
        guard selfDevice.protocolVersion == SonaProtocol.protocolVersion else {
            throw PairingError.identityMismatch
        }
        guard let envelopeText = selfDevice.envelope else { throw PairingError.identityMismatch }
        guard let envelope = Base64URL.decode(envelopeText) else { throw PairingError.crypto }
        let vaultRoot = try openPairingEnvelope(
            recipientSecretKey: identity.pairingSecret, envelope: envelope
        )
        return VaultCredentials(
            endpoint: pending.endpoint,
            vaultId: pending.vaultId,
            vaultRootBase64URL: Base64URL.encode(vaultRoot)
        )
    }
}
