import AppKit
import CoreImage.CIFilterBuiltins
import Foundation
import SwiftUI

/// Cloud sync and phone pairing: the shapes of `src-tauri/src/cloud_sync`.
///
/// Field names mirror the Rust structs with snake_case turned into camelCase
/// by `Core.decoder`. The serde structs a request *carries* are spelled out
/// with the core's own field names at the point of the call, because the
/// request encoder converts nothing.

extension CoreEvent {
    /// The runtime's own invalidation. It carries the meeting and its new
    /// state when one meeting changed, and nothing when the change was the
    /// vault's; either way the panel re-reads what it shows.
    static let cloudSyncChanged = "cloud-sync:changed"
}

/// Why a cloud command refused: `CloudSyncErrorKind`.
enum CloudSyncErrorKind: String, Decodable {
    case portableUnavailable = "portable_unavailable"
    case secretUnavailable = "secret_unavailable"
    case setupRequired = "setup_required"
    case authRequired = "auth_required"
    case quota
    case integrityFailure = "integrity_failure"
    case conflict
    case unsupportedProtocol = "unsupported_protocol"
    case transient

    /// What a reader can do about it, in the words the old panel used.
    var guidance: String {
        switch self {
        case .portableUnavailable: "Cloud sync is unavailable in portable mode."
        case .secretUnavailable: "The system credential store is unavailable."
        case .setupRequired: "Set up cloud sync before using this action."
        case .authRequired: "Cloud sign-in is required."
        case .quota: "Cloud storage quota has been reached."
        case .integrityFailure: "The cloud operation could not be verified."
        case .conflict: "This meeting has a sync conflict."
        case .unsupportedProtocol: "This device does not support the version of cloud sync in use."
        case .transient: "Cloud sync is temporarily unavailable."
        }
    }
}

/// Where one meeting stands with the vault: `CloudObjectState`.
enum CloudSyncObjectState: String, Decodable {
    case local
    case queued
    case uploading
    case committed
    case conflict
    case pendingDeletion = "pending_deletion"
    case deleted
    case paused
    case authRequired = "auth_required"
    case quota
    case integrityFailure = "integrity_failure"

    var word: String {
        switch self {
        case .local: "Local only"
        case .queued: "Queued"
        case .uploading: "Uploading"
        case .committed: "Synced"
        case .conflict: "Conflict"
        case .pendingDeletion: "Removal pending"
        case .deleted: "Removed"
        case .paused: "Paused"
        case .authRequired: "Sign-in required"
        case .quota: "Quota reached"
        case .integrityFailure: "Verification failed"
        }
    }

    /// The three states a retry can clear. A conflict needs a choice, not a
    /// retry, and the rest are either settled or still moving.
    var retryable: Bool {
        self == .authRequired || self == .quota || self == .integrityFailure
    }

    /// Colour follows the reason; the word is always present.
    var tone: Color {
        switch self {
        case .committed: Theme.ink
        case .local, .queued, .uploading, .deleted: Theme.inkSecondary
        case .paused, .pendingDeletion: Theme.accent
        case .conflict, .authRequired, .quota, .integrityFailure: Theme.live
        }
    }
}

/// The one line a reader checks on the way past the section.
enum CloudSyncAccountStatus {
    case loading
    case attention
    case paused
    case ready
    case unavailable
    case local

    var word: String {
        switch self {
        case .loading: "Checking…"
        case .attention: "Needs attention"
        case .paused: "Paused"
        case .ready: "Active"
        case .unavailable: "Unavailable"
        case .local: "Local only"
        }
    }

    var tone: Color {
        switch self {
        case .loading: Theme.inkTertiary
        case .attention, .unavailable: Theme.live
        case .paused: Theme.accent
        case .ready: Theme.ink
        case .local: Theme.inkSecondary
        }
    }
}

/// What the vault is doing as a whole: `CloudSyncOverview`.
struct CloudSyncOverview: Decodable {
    let enabled: Bool
    let portableMode: Bool
    let paused: Bool
    let queuedObjects: UInt32
    let pendingDeletions: UInt32
    /// The failure the runtime is stuck on, if it is stuck.
    let terminalError: CloudSyncErrorKind?
}

/// Where one meeting stands: `CloudMeetingStatus`.
struct CloudSyncMeetingStatus: Decodable, Identifiable {
    let sessionId: String
    let state: CloudSyncObjectState
    let remoteRevisionId: String?
    let retryAtUtcMs: Int64?
    let shareCount: UInt32

    var id: String { sessionId }
    var retryAt: Date? { retryAtUtcMs.map(CloudSyncClock.date) }
}

/// Setting up mints the recovery code, which is shown once: `CloudSyncBootstrapResult`.
struct CloudSyncBootstrapResult: Decodable {
    let overview: CloudSyncOverview
    let recoveryCode: String
}

/// Provisioning on this device, read from stored settings and the runtime's
/// last access result: `CloudSyncServiceStatus`. Never a switch.
struct CloudSyncServiceStatus: Decodable {
    let configured: Bool
    let endpoint: String?
    let error: CloudSyncErrorKind?
    let reason: String
}

/// The payload of `cloud-sync:changed`: `CloudSyncChangedPayload`, which the
/// event wraps transparently.
struct CloudSyncChanged: Decodable {
    let eventSchemaVersion: UInt32
    let sessionId: String?
    let state: CloudSyncObjectState?
}

/// Which copy of a conflicted meeting wins: `CloudConflictChoice`.
enum CloudSyncConflictChoice: String, Encodable {
    case keepLocal = "keep_local"
    case useRemote = "use_remote"
}

/// The name of a meeting, read from `meeting_list` so a sync row says which
/// meeting it is instead of a session id. Only the three fields the row uses.
struct CloudSyncMeetingLabel: Decodable {
    let sessionId: String
    let title: String
    let createdAtUtcMs: Int64
}

/// `PaginatedMeetings`, of which this panel reads only the entries.
struct CloudSyncMeetingPage: Decodable {
    let entries: [CloudSyncMeetingLabel]
}

/// The core's millisecond timestamps, both ways.
enum CloudSyncClock {
    static func date(_ utcMs: Int64) -> Date {
        Date(timeIntervalSince1970: Double(utcMs) / 1000)
    }

    static func utcMs(_ date: Date) -> Int64 {
        Int64((date.timeIntervalSince1970 * 1000).rounded())
    }

    /// "12 Sep 21:53": a moment a reader can act on, never a weekday alone.
    static func moment(_ date: Date) -> String {
        "\(date.short) \(date.time)"
    }

    static func moment(_ utcMs: Int64) -> String {
        moment(date(utcMs))
    }
}

/// A file another person can be handed: `CloudShareResult`.
struct CloudShareResult: Decodable {
    let shareId: String
    let expiresAtUtcMs: Int64
    let filePath: String
}

/// A link another person can open: `CloudBrowserShareResult`.
struct CloudShareBrowserResult: Decodable {
    let shareId: String
    let expiresAtUtcMs: Int64
    let shareUrl: String
    /// What the viewer's deployment can see, stated by the core.
    let trustDisclosure: String
}

/// What importing a `.sona` file produced: `CloudShareImportResult`.
struct CloudShareImportResult: Decodable {
    let sessionId: String
}

/// A share that exists, and the meeting it belongs to, so a row only shows
/// its own share.
struct CloudShareFile {
    let sessionId: String
    let result: CloudShareResult
}

struct CloudShareLink {
    let sessionId: String
    let result: CloudShareBrowserResult
}

/// The candidate record a phone shows and this Mac approves:
/// `CloudPairingOffer`. `mobile/Shared/Pairing.swift` mints the same shape.
struct PairingOffer: Codable, Equatable {
    let protocolVersion: Int
    let vaultId: String
    let deviceId: String
    let signingPublicKey: String
    let pairingPublicKey: String
    let candidateProof: String
    let pairingNonce: String
    let expiresAtUtcMs: Int64
    /// The fingerprint the offer carries. Never what an approval screen
    /// shows: that one is derived here, from the record.
    let fingerprint: String

    /// The core's own field names. `Core.decoder` converts snake_case on the
    /// way in, so only the way out needs spelling, and the request encoder
    /// converts nothing.
    private enum WireKey: String, CodingKey {
        case protocolVersion = "protocol_version"
        case vaultId = "vault_id"
        case deviceId = "device_id"
        case signingPublicKey = "signing_public_key"
        case pairingPublicKey = "pairing_public_key"
        case candidateProof = "candidate_proof"
        case pairingNonce = "pairing_nonce"
        case expiresAtUtcMs = "expires_at_utc_ms"
        case fingerprint
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: WireKey.self)
        try container.encode(protocolVersion, forKey: .protocolVersion)
        try container.encode(vaultId, forKey: .vaultId)
        try container.encode(deviceId, forKey: .deviceId)
        try container.encode(signingPublicKey, forKey: .signingPublicKey)
        try container.encode(pairingPublicKey, forKey: .pairingPublicKey)
        try container.encode(candidateProof, forKey: .candidateProof)
        try container.encode(pairingNonce, forKey: .pairingNonce)
        try container.encode(expiresAtUtcMs, forKey: .expiresAtUtcMs)
        try container.encode(fingerprint, forKey: .fingerprint)
    }

    /// The record as another device reads it: the text to type, and the bytes
    /// behind the code to scan. Sorted, because a keyed container is not
    /// insertion-ordered and the same offer must render the same code twice;
    /// unescaped, because the keys are base64 and `\/` only makes the code
    /// denser and the text worse to read.
    var json: String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        guard let bytes = try? encoder.encode(self),
              let text = String(data: bytes, encoding: .utf8)
        else { return "" }
        return text
    }

    var expiresAt: Date { CloudSyncClock.date(expiresAtUtcMs) }

    /// A pasted or scanned record, or nothing. The old panel's schema: every
    /// string present and not empty, both numbers whole and positive.
    static func parse(_ text: String) -> PairingOffer? {
        guard let bytes = text.data(using: .utf8),
              let offer = try? Core.decoder.decode(PairingOffer.self, from: bytes),
              offer.isComplete
        else { return nil }
        return offer
    }

    private var isComplete: Bool {
        let strings = [
            vaultId, deviceId, signingPublicKey, pairingPublicKey,
            candidateProof, pairingNonce, fingerprint,
        ]
        return protocolVersion > 0 && expiresAtUtcMs > 0 && !strings.contains(where: \.isEmpty)
    }
}

/// Approving a device that is showing its own code on a screen. The
/// fingerprint is derived by this Mac from the pasted record, not read out of
/// it, so the reader compares two independent answers; carrying the record
/// alongside it is what stops an approval from acting on anything else.
enum PairingCandidate {
    case empty
    case reading
    case invalid
    case ready(fingerprint: String, offer: PairingOffer)
    case approved

    /// The line under the field, and whether it is a complaint.
    var line: String? {
        switch self {
        case .empty: nil
        case .reading: "Checking…"
        case .invalid: "That code is not valid."
        case .ready: "Check this matches the code on the device before you approve."
        case .approved: "Device approved."
        }
    }

    var tone: Color {
        if case .invalid = self { return Theme.live }
        return Theme.inkTertiary
    }

    /// What this Mac read out of the paste, never the value the paste carried.
    var derivedFingerprint: String? {
        if case let .ready(fingerprint, _) = self { return fingerprint }
        return nil
    }

    var offer: PairingOffer? {
        if case let .ready(_, offer) = self { return offer }
        return nil
    }
}

/// A pairing record as a code a phone can read: the same bytes the text
/// beside it shows, so a scan and a retype produce the same record.
/// `mobile/Sona/PairingScreen.swift` draws its side with the same generator.
enum PairingCode {
    static func image(_ payload: String, side: CGFloat) -> NSImage? {
        let generator = CIFilter.qrCodeGenerator()
        generator.message = Data(payload.utf8)
        generator.correctionLevel = "M"
        guard let coded = generator.outputImage, coded.extent.width > 0 else { return nil }
        let scale = side / coded.extent.width
        let scaled = coded.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        guard let bitmap = CIContext().createCGImage(scaled, from: scaled.extent) else { return nil }
        return NSImage(cgImage: bitmap, size: CGSize(width: side, height: side))
    }
}
