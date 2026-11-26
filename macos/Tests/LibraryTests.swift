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
    /// reason. Privacy must say it cannot be read: neither that it is
    /// encrypted at rest nor that it is stored unencrypted.
    func testLockedHistoryReadsAsUnreadable() throws {
        for reason in ["key_rejected", "key_unavailable", "encryption_unavailable"] {
            let status = try storage(encrypted: true, reason: reason)
            XCTAssertEqual(status.word, "Locked")
            XCTAssertTrue(status.sentence?.hasSuffix("so history cannot be read.") == true, status.sentence ?? "")
        }
        XCTAssertEqual(try storage(encrypted: true, reason: nil).word, "Encrypted at rest")
        let plain = try storage(encrypted: false, reason: "key_unavailable")
        XCTAssertEqual(plain.word, "Not encrypted")
        XCTAssertTrue(plain.sentence?.hasSuffix("so history is stored unencrypted.") == true, plain.sentence ?? "")
    }

    /// Imports bring dictations from earlier years. Their day heading names
    /// the year, so one is never read as a day of this year.
    func testADayFromAnEarlierYearShowsItsYear() throws {
        let lastYear = try XCTUnwrap(Calendar.current.date(byAdding: .year, value: -1, to: .now))
        let year = Calendar.current.component(.year, from: lastYear)
        XCTAssertTrue(lastYear.relativeDay.hasSuffix(" \(year)"), lastYear.relativeDay)
    }

    /// One read of a history event gives the page both the change and the
    /// row it carries; a change without an entry still arrives.
    func testAHistoryEventIsReadOnceAsChangeAndRow() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let added = #"{"action":"added","entry":{"id":5,"timestamp":1,"saved":false,"title":"t","#
            + #""transcription_text":" heard ","post_processed_text":null,"parent_id":2}}"#
        let event = try decoder.decode(HistoryRowUpdate.self, from: Data(added.utf8))
        guard case .added(let entry) = event.update else { return XCTFail("\(event.update)") }
        XCTAssertEqual(entry.id, 5)
        XCTAssertEqual(event.entry?.spokenText, "heard")
        XCTAssertEqual(event.entry?.parentId, 2)

        let deleted = try decoder.decode(HistoryRowUpdate.self, from: Data(#"{"action":"deleted","id":5}"#.utf8))
        guard case .deleted(5) = deleted.update else { return XCTFail("\(deleted.update)") }
        XCTAssertNil(deleted.entry)
    }

    private func storage(encrypted: Bool, reason: String?) throws -> StorageStatus {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let reason = reason.map { "\"\($0)\"" } ?? "null"
        let json = #"{"encrypted":\#(encrypted),"migrated_at":null,"reason":\#(reason)}"#
        return try decoder.decode(StorageStatus.self, from: Data(json.utf8))
    }
}
