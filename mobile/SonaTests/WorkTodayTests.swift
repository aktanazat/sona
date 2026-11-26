import XCTest

/// The phone's half of the Mac's Today: reading the Mac's snapshot and sending decisions back.
final class WorkTodayTests: XCTestCase {
    private func fixture(_ name: String) throws -> Data {
        let url = try XCTUnwrap(Bundle(for: WorkTodayTests.self).url(forResource: name, withExtension: "json"))
        return try Data(contentsOf: url)
    }

    private func scratchFile() -> URL {
        FileManager.default.temporaryDirectory.appending(path: "today-\(UUID().uuidString).json")
    }

    /// `work_today.rs` checks the same two fixtures: the Mac parses the decision with
    /// `deny_unknown_fields`, and writes the snapshot the phone reads here.
    func testDecisionsAndSnapshotsMatchTheMacsBytes() throws {
        let actionBytes = try fixture("work-action-v1")
        let action = try JSONDecoder().decode(WorkActionManifest.self, from: actionBytes)
        XCTAssertEqual(try WorkActionObject.encodeManifest(action), actionBytes)

        let today = try JSONDecoder().decode(WorkToday.self, from: fixture("work-today-v1"))
        XCTAssertEqual(today.due.map(\.title), ["Send the deck"])
        XCTAssertNil(today.triage[0].due_at_ms)
        XCTAssertEqual(today.questions[0].options, [.init(label: "Acme", value: "acme")])
        XCTAssertEqual(today.handled[0].outcome, .applied)
        XCTAssertEqual(WorkTodayGlance(today).decisions, 2)
    }

    @MainActor
    func testTheMacsAnswerSettlesEachDecisionAndDeletesIt() async throws {
        let transport = FakeTodayTransport()
        let today = PhoneToday(transport: transport, file: scratchFile())
        var snapshot = try JSONDecoder().decode(WorkToday.self, from: fixture("work-today-v1"))
        snapshot.handled = []

        try await today.decide(.accept, row: "t2", title: "Call the venue", in: snapshot, deviceId: "phone", taskId: "t2")
        try await today.decide(.done, row: "t1", title: "Send the deck", in: snapshot, deviceId: "phone",
                               taskId: "t1", expectedRevision: 3)
        try await today.decide(.decline, row: "t2", title: "Call the venue", in: snapshot, deviceId: "phone", taskId: "t2")
        XCTAssertEqual(transport.sent.map(\.action), [.accept, .done])
        XCTAssertEqual(transport.sent.map(\.target_device_id), ["dev_mac_fixture", "dev_mac_fixture"])
        XCTAssertEqual(transport.sent[1].expected_revision, 3)

        await today.reconcile(snapshot)
        XCTAssertEqual(today.pending.count, 2)
        XCTAssertTrue(transport.deleted.isEmpty)

        snapshot.handled = [
            .init(action_id: "object-0", outcome: .applied, at_utc_ms: 1),
            .init(action_id: "object-1", outcome: .refused, at_utc_ms: 2),
        ]
        transport.refuseDeletes = true
        await today.reconcile(snapshot)
        XCTAssertTrue(today.pending.isEmpty)
        XCTAssertEqual(today.refused, ["Send the deck"])

        transport.refuseDeletes = false
        await today.reconcile(nil)
        XCTAssertEqual(transport.deleted, ["object-0", "object-1"])
    }
}

@MainActor
private final class FakeTodayTransport: TodayTransport {
    var sent: [WorkActionManifest] = []
    var deleted: [String] = []
    var refuseDeletes = false

    func send(_ action: WorkActionManifest) async throws -> String {
        sent.append(action)
        return "object-\(sent.count - 1)"
    }

    func deleteObject(_ objectId: String) async throws {
        if refuseDeletes { throw VaultReaderError.notInIndex }
        deleted.append(objectId)
    }
}
