import XCTest

final class LibraryTests: XCTestCase {
    /// The limit can land while the reader has the field focused. Leaving the
    /// field untouched must not save the number it showed before, which would
    /// delete every unsaved dictation past it.
    func testOnlyATypedLimitIsSaved() {
        var draft = LimitDraft()
        draft.follow(0, editing: false)
        draft.follow(100, editing: true)
        XCTAssertEqual(draft.text, "100", "an untouched field shows the limit that landed")
        XCTAssertNil(draft.commit(ceiling: 1000), "leaving it untouched saves nothing")

        draft.text = "25"
        draft.follow(60, editing: true)
        XCTAssertEqual(draft.text, "25", "a limit landing mid-edit leaves the typing alone")
        XCTAssertEqual(draft.commit(ceiling: 1000), 25)
    }

    /// The core reports a history file it cannot open as encrypted with a
    /// reason. The Library must say it cannot be read: neither that it is
    /// encrypted at rest nor that it is stored unencrypted.
    func testLockedHistoryReadsAsUnreadable() throws {
        for reason in ["key_rejected", "key_unavailable", "encryption_unavailable"] {
            let line = try storage(encrypted: true, reason: reason).line
            XCTAssertTrue(line.hasSuffix("so history cannot be read."), line)
        }
        XCTAssertEqual(try storage(encrypted: true, reason: nil).line, "Encrypted at rest")
        let plain = try storage(encrypted: false, reason: "key_unavailable").line
        XCTAssertTrue(plain.hasSuffix("so history is stored unencrypted."), plain)
    }

    private func storage(encrypted: Bool, reason: String?) throws -> StorageStatus {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let reason = reason.map { "\"\($0)\"" } ?? "null"
        let json = #"{"encrypted":\#(encrypted),"migrated_at":null,"reason":\#(reason)}"#
        return try decoder.decode(StorageStatus.self, from: Data(json.utf8))
    }
}
