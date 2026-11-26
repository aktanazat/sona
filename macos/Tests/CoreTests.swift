import XCTest

final class CoreTests: XCTestCase {
    /// A reply of many megabytes is decoded once, by the request that asked
    /// for it. Reading which request it answers must not parse the result:
    /// a result that is not JSON past its first bytes still names its id.
    func testAReplyIsMatchedFromItsFirstBytes() throws {
        let unread = Data("{\"id\":42,\"result\":".utf8) + Data(repeating: UInt8(ascii: "x"), count: 4 << 20)
        let head = try XCTUnwrap(Core.head(of: unread))
        XCTAssertEqual(head.id, 42)
        XCTAssertNil(head.event)
        XCTAssertNil(head.error)
    }

    func testAnEventIsNamedFromItsFirstBytes() throws {
        let unread = Data("{\"event\":\"history-update-payload\",\"payload\":".utf8)
            + Data(repeating: UInt8(ascii: "x"), count: 1 << 20)
        let head = try XCTUnwrap(Core.head(of: unread))
        XCTAssertEqual(head.event, "history-update-payload")
        XCTAssertNil(head.id)
    }

    /// A failure carries its error in the head, and a command whose error
    /// type is `()` fails with `null`: both must still read as failures.
    func testAFailureCarriesItsError() throws {
        let typed = try XCTUnwrap(Core.head(of: Data("{\"id\":7,\"error\":{\"kind\":\"unpaired\"}}".utf8)))
        XCTAssertEqual(typed.id, 7)
        XCTAssertEqual(typed.error, .object(["kind": .string("unpaired")]))
        let unit = try XCTUnwrap(Core.head(of: Data("{\"id\":8,\"error\":null}".utf8)))
        XCTAssertEqual(unit.id, 8)
        XCTAssertEqual(unit.error, .null)
    }

    /// The first bytes are a shortcut, not the contract: a frame whose keys
    /// come in another order is read whole and still delivered.
    func testAFrameInAnotherOrderIsStillRead() throws {
        let reply = try XCTUnwrap(Core.head(of: Data("{\"result\":[1],\"id\":3}".utf8)))
        XCTAssertEqual(reply.id, 3)
        XCTAssertNil(reply.error)
        let failure = try XCTUnwrap(Core.head(of: Data("{\"error\":\"no\",\"id\":4}".utf8)))
        XCTAssertEqual(failure.error, .string("no"))
        let event = try XCTUnwrap(Core.head(of: Data("{\"payload\":{},\"event\":\"a\\\"b\"}".utf8)))
        XCTAssertEqual(event.event, "a\"b")
        XCTAssertNil(Core.head(of: Data("not json".utf8)))
    }
}
