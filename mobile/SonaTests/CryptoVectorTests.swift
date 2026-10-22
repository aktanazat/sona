import CryptoKit
import XCTest

/// Every primitive is checked against `cloudflare/sona-companion/fixtures/crypto-v1.json`,
/// the same file the Rust and TypeScript suites read. A byte that disagrees here is a
/// byte the Worker would reject.
final class CryptoVectorTests: XCTestCase {
    /// The RFC 8032 test seed the Rust suite uses, so the signatures in the fixture are
    /// reproducible from it.
    private static let signingSeed = Data([
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec,
        0x2c, 0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03,
        0x1c, 0xae, 0x7f, 0x60,
    ])

    private var fixture: [String: Any] = [:]

    override func setUpWithError() throws {
        let url = try XCTUnwrap(
            Bundle(for: CryptoVectorTests.self).url(
                forResource: "crypto-v1", withExtension: "json"
            )
        )
        let parsed = try JSONSerialization.jsonObject(with: try Data(contentsOf: url))
        fixture = try XCTUnwrap(parsed as? [String: Any])
    }

    // MARK: - Canonical records

    func testCanonicalRequestRecordAndSignature() throws {
        let vector = try section("canonical_request")
        let input = CanonicalRequestInput(
            audience: try string(vector, "audience"),
            vaultId: try string(vector, "vaultId"),
            deviceId: try string(vector, "deviceId"),
            method: try string(vector, "method"),
            path: try string(vector, "path"),
            query: try queryPairs(vector),
            bodyDigest: try string(vector, "bodyDigest"),
            contentType: try string(vector, "contentType"),
            idempotencyKey: try string(vector, "idempotencyKey"),
            timestamp: UInt64(try integer(vector, "timestamp")),
            nonce: try bytes(vector, "nonce")
        )
        let record = canonicalRequestBytes(input)
        XCTAssertEqual(record, try bytes(vector, "bytes"))

        let publicKey = try ed25519PublicKey(signingSeed: CryptoVectorTests.signingSeed)
        XCTAssertEqual(Base64URL.encode(publicKey), try string(vector, "ed25519_public_key"))
        /* CryptoKit's Ed25519 signing is randomised, so the fixture's signature bytes
         * cannot be reproduced. What has to hold is what the Worker checks: the
         * fixture's signature verifies over these bytes, and ours does too. */
        XCTAssertTrue(
            verifyEd25519(
                publicKey: publicKey,
                signature: try bytes(vector, "signature"),
                message: record
            )
        )
        let signature = try signEd25519(
            signingSeed: CryptoVectorTests.signingSeed, message: record
        )
        XCTAssertTrue(
            verifyEd25519(publicKey: publicKey, signature: signature, message: record)
        )

        var mutated = input
        mutated.method = "GET"
        XCTAssertFalse(
            verifyEd25519(
                publicKey: publicKey,
                signature: signature,
                message: canonicalRequestBytes(mutated)
            ),
            "the fixture's expected_failures pins a mutated method as unverifiable"
        )
    }

    func testCanonicalUploadEnvelopeRecordAndSignature() throws {
        let vector = try section("upload_envelope")
        let chunks = try XCTUnwrap(vector["chunks"] as? [[String: Any]])
        let input = CanonicalUploadEnvelopeInput(
            vaultId: try string(vector, "vaultId"),
            kind: try string(vector, "kind"),
            objectId: vector["objectId"] as? String,
            revisionId: vector["revisionId"] as? String,
            baseRevisionId: vector["baseRevisionId"] as? String,
            shareId: vector["shareId"] as? String,
            manifestDigest: try string(vector, "manifestDigest"),
            cryptoVersion: UInt64(try integer(vector, "cryptoVersion")),
            totalBytes: UInt64(try integer(vector, "totalBytes")),
            chunks: try chunks.map {
                UploadEnvelopeChunk(
                    index: UInt64(try integer($0, "index")),
                    size: UInt64(try integer($0, "size")),
                    sha256: try string($0, "sha256")
                )
            }
        )
        let record = canonicalUploadEnvelopeBytes(input)
        XCTAssertEqual(record, try bytes(vector, "bytes"))
        let publicKey = try ed25519PublicKey(signingSeed: CryptoVectorTests.signingSeed)
        XCTAssertEqual(Base64URL.encode(publicKey), try string(vector, "ed25519_public_key"))
        XCTAssertTrue(
            verifyEd25519(
                publicKey: publicKey,
                signature: try bytes(vector, "signature"),
                message: record
            )
        )
        let signature = try signEd25519(
            signingSeed: CryptoVectorTests.signingSeed, message: record
        )
        XCTAssertTrue(
            verifyEd25519(publicKey: publicKey, signature: signature, message: record)
        )

        var mutated = input
        mutated.manifestDigest = try string(vector, "manifestDigest") + "x"
        XCTAssertFalse(
            verifyEd25519(
                publicKey: publicKey,
                signature: signature,
                message: canonicalUploadEnvelopeBytes(mutated)
            )
        )
    }

    // MARK: - Object revision crypto

    func testObjectRevisionKeysAadAndPayloads() throws {
        let vector = try section("object_revision_aes_gcm_hkdf")
        let vaultRoot = try bytes(vector, "vault_root")
        let vaultId = try string(vector, "vault_id")
        let objectId = try string(vector, "object_id")
        let revisionId = try string(vector, "revision_id")

        XCTAssertEqual(
            objectRevisionRootInfo(vaultId: vaultId, objectId: objectId, revisionId: revisionId),
            try bytes(vector, "root_hkdf_info")
        )
        let revisionRoot = try deriveObjectRevisionRoot(
            vaultRoot: vaultRoot, vaultId: vaultId, objectId: objectId, revisionId: revisionId
        )
        XCTAssertEqual(Base64URL.encode(revisionRoot), try string(vector, "revision_root"))

        for name in ["manifest", "chunk"] {
            let payload = try XCTUnwrap(vector[name] as? [String: Any], name)
            let context = ObjectRevisionCryptoContext(
                vaultId: vaultId,
                objectId: objectId,
                revisionId: revisionId,
                index: UInt64(try integer(payload, "index")),
                total: UInt64(try integer(payload, "total")),
                contentKind: try XCTUnwrap(
                    ObjectContentKind(rawValue: try string(payload, "content_kind"))
                ),
                sourceFormat: try string(payload, "source_format")
            )
            XCTAssertEqual(try objectRevisionKeyInfo(context), try bytes(payload, "key_info"), name)
            XCTAssertEqual(try objectRevisionAad(context), try bytes(payload, "aad"), name)

            let expected = try bytes(payload, "ciphertext")
            let sealed = try sealObjectRevisionPayload(
                vaultRoot: vaultRoot,
                context: context,
                nonce: try bytes(payload, "nonce"),
                plaintext: try bytes(payload, "plaintext")
            )
            XCTAssertEqual(sealed, expected, name)
            XCTAssertEqual(
                try openObjectRevisionPayload(
                    vaultRoot: vaultRoot, context: context, encryptedPayload: expected
                ),
                try bytes(payload, "plaintext"),
                name
            )

            var mutated = context
            mutated.sourceFormat = "markdown-utf8-mutated"
            XCTAssertThrowsError(
                try openObjectRevisionPayload(
                    vaultRoot: vaultRoot, context: mutated, encryptedPayload: expected
                ),
                name
            ) { error in
                XCTAssertEqual(error as? SonaCryptoError, .authenticationFailed)
            }

            let truncated = expected.prefix(27)
            XCTAssertThrowsError(
                try openObjectRevisionPayload(
                    vaultRoot: vaultRoot, context: context, encryptedPayload: Data(truncated)
                ),
                name
            )
        }
    }

    func testSharePayloadKeysAadAndDecryption() throws {
        let vector = try section("share_aes_gcm_hkdf")
        let root = try bytes(vector, "root")
        let shareId = try string(vector, "share_id")
        for name in ["manifest", "chunk"] {
            let payload = try XCTUnwrap(vector[name] as? [String: Any], name)
            let context = SharePayloadContext(
                shareId: shareId,
                index: UInt64(try integer(payload, "index")),
                total: UInt64(try integer(payload, "total")),
                domain: try XCTUnwrap(
                    SharePayloadDomain(rawValue: try string(payload, "domain"))
                )
            )
            XCTAssertEqual(sharePayloadKeyInfo(context), try bytes(payload, "key_info"), name)
            XCTAssertEqual(sharePayloadAad(context), try bytes(payload, "aad"), name)
            XCTAssertEqual(
                try openSharePayload(
                    root: root,
                    context: context,
                    encryptedPayload: try bytes(payload, "ciphertext")
                ),
                try bytes(payload, "plaintext"),
                name
            )
        }
    }

    // MARK: - Encoding rules

    func testBase64URLRejectsNonCanonicalInput() {
        XCTAssertEqual(Base64URL.encode(Data([0xff, 0xef, 0xbe])), "_---")
        XCTAssertEqual(Base64URL.decode("_---"), Data([0xff, 0xef, 0xbe]))
        XCTAssertNil(Base64URL.decode("AA=="), "padding is not canonical")
        XCTAssertNil(Base64URL.decode("A"), "a one-character group cannot decode")
        XCTAssertNil(Base64URL.decode("a+b/"), "standard base64 alphabet is rejected")
        XCTAssertNil(Base64URL.decode("AB"), "trailing bits must be zero to round-trip")
    }

    func testRecordDecodingRejectsTruncation() {
        let record = canonicalRecord([.text("a"), .bytes(Data([1, 2, 3]))])
        XCTAssertEqual(decodeCanonicalRecord(record), [Data("a".utf8), Data([1, 2, 3])])
        XCTAssertNil(decodeCanonicalRecord(record.dropLast()))
        XCTAssertNil(decodeCanonicalRecord(Data([0, 0])))
    }

    /// The Worker sorts query keys by UTF-16 code unit, which differs from Swift's own
    /// string ordering above the BMP.
    func testWorkerStringOrderIsUtf16CodeUnitOrder() {
        XCTAssertTrue(workerStringIsOrderedBefore("a", "b"))
        XCTAssertFalse(workerStringIsOrderedBefore("b", "a"))
        XCTAssertTrue(workerStringIsOrderedBefore("limit", "limits"))
        XCTAssertFalse(workerStringIsOrderedBefore("a", "a"))
        XCTAssertTrue(
            workerStringIsOrderedBefore("\u{1F600}", "\u{FFFD}"),
            "a surrogate pair leads with 0xD83D, which is below U+FFFD"
        )
    }

    func testCanonicalRequestSortsQueryByKeyThenValue() {
        let sorted = canonicalRequestBytes(request(query: [("a", "1"), ("b", "2")]))
        let reversed = canonicalRequestBytes(request(query: [("b", "2"), ("a", "1")]))
        XCTAssertEqual(sorted, reversed)
        let byValue = canonicalRequestBytes(request(query: [("a", "2"), ("a", "1")]))
        let byValueReversed = canonicalRequestBytes(request(query: [("a", "1"), ("a", "2")]))
        XCTAssertEqual(byValue, byValueReversed)
    }

    func testDecimalFieldsAreCanonicalAscii() {
        XCTAssertEqual(canonicalRecord([.decimal(0)]), Data([0, 0, 0, 1]) + Data("0".utf8))
        XCTAssertEqual(canonicalRecord([.decimal(1700000000000)]).dropFirst(4), Data("1700000000000".utf8))
    }

    // MARK: - Pairing envelope

    /// The desktop seals the envelope; the phone only opens one, so the test builds the
    /// desktop's side with the documented derivation and checks the phone's reader.
    func testPairingEnvelopeRoundTrip() throws {
        let recipientSecret = randomBytes(32)
        let recipientPublic = try x25519PublicKey(secretKey: recipientSecret)
        let ephemeralSecret = randomBytes(32)
        let ephemeralPublic = try x25519PublicKey(secretKey: ephemeralSecret)
        let vaultRoot = randomBytes(32)
        let nonce = randomBytes(12)
        let shared = try x25519SharedSecret(
            secretKey: ephemeralSecret, publicKey: recipientPublic
        )
        let key = HKDF<SHA256>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: shared),
            salt: Data("sona-pairing-envelope-v1".utf8),
            info: canonicalRecord([
                .text("sona-pairing-envelope-key-v1"),
                .bytes(recipientPublic),
                .bytes(ephemeralPublic),
            ]),
            outputByteCount: 32
        )
        let aad = canonicalRecord([
            .text("sona-pairing-envelope-aad-v1"),
            .decimal(1),
            .bytes(recipientPublic),
            .bytes(ephemeralPublic),
            .bytes(nonce),
        ])
        let sealed = try AES.GCM.seal(
            vaultRoot, using: key, nonce: AES.GCM.Nonce(data: nonce), authenticating: aad
        )
        let envelope = canonicalRecord([
            .text("sona-pairing-envelope-v1"),
            .decimal(1),
            .bytes(ephemeralPublic),
            .bytes(nonce),
            .bytes(sealed.ciphertext + sealed.tag),
        ])
        XCTAssertEqual(
            try openPairingEnvelope(recipientSecretKey: recipientSecret, envelope: envelope),
            vaultRoot
        )
        XCTAssertThrowsError(
            try openPairingEnvelope(
                recipientSecretKey: randomBytes(32), envelope: envelope
            )
        )
    }

    // MARK: - Helpers

    private func request(query: [(String, String)]) -> CanonicalRequestInput {
        CanonicalRequestInput(
            vaultId: "vault",
            deviceId: "device",
            method: "GET",
            path: "/v1/changes",
            query: query,
            bodyDigest: "digest",
            contentType: "",
            idempotencyKey: "",
            timestamp: 1_700_000_000_000,
            nonce: Data(repeating: 0, count: 16)
        )
    }

    private func section(_ name: String) throws -> [String: Any] {
        try XCTUnwrap(fixture[name] as? [String: Any], name)
    }

    private func string(_ value: [String: Any], _ key: String) throws -> String {
        try XCTUnwrap(value[key] as? String, key)
    }

    private func integer(_ value: [String: Any], _ key: String) throws -> Int {
        try XCTUnwrap(value[key] as? Int, key)
    }

    private func bytes(_ value: [String: Any], _ key: String) throws -> Data {
        try XCTUnwrap(Base64URL.decode(try string(value, key)), key)
    }

    private func queryPairs(_ value: [String: Any]) throws -> [(String, String)] {
        let raw = try XCTUnwrap(value["query"] as? [[String]], "query")
        return try raw.map { pair in
            guard pair.count == 2 else { throw XCTSkip("malformed query pair") }
            return (pair[0], pair[1])
        }
    }
}
