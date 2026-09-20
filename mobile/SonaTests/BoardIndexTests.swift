import XCTest

final class BoardIndexTests: XCTestCase {
    /// The board leads with what is not sorted yet, then clusters by name; inside a
    /// cluster pinned cards lead and the rest run newest first. An archived card hides
    /// its thought, and the newest card about a thought is the one that counts.
    func testSectionsOrderUnsortedFirstThenClustersWithPinnedLeading() {
        var index = BoardIndex(vaultId: "vault")
        index.heads["t-old"] = head(.thought(thought(capturedAt: 10)))
        index.heads["t-new"] = head(.thought(thought(capturedAt: 30)))
        index.heads["t-pinned"] = head(.thought(thought(capturedAt: 20)))
        index.heads["t-waiting"] = head(.thought(thought(capturedAt: 40)))
        index.heads["t-hidden"] = head(.thought(thought(capturedAt: 50)))
        index.heads["t-resorted"] = head(.thought(thought(capturedAt: 60)))
        let ideas = CardManifest.Cluster(key: "ideas", name: "Ideas", hue: 200)
        let admin = CardManifest.Cluster(key: "admin", name: "Admin", hue: 20)
        index.heads["c-old"] = head(.card(card(thought: "t-old", cluster: ideas, at: 100)))
        index.heads["c-new"] = head(.card(card(thought: "t-new", cluster: ideas, at: 100)))
        index.heads["c-pinned"] = head(
            .card(card(thought: "t-pinned", cluster: ideas, at: 100, pinned: true))
        )
        index.heads["c-hidden"] = head(
            .card(card(thought: "t-hidden", cluster: ideas, at: 100, archived: true))
        )
        index.heads["c-first"] = head(.card(card(thought: "t-resorted", cluster: ideas, at: 100)))
        index.heads["c-later"] = head(.card(card(thought: "t-resorted", cluster: admin, at: 200)))
        index.heads["m-bundle"] = head(.other)

        let sections = index.sections()

        XCTAssertEqual(sections.map(\.id), ["", "admin", "ideas"])
        XCTAssertEqual(sections[0].tiles.map(\.id), ["t-waiting"])
        XCTAssertNil(sections[0].tiles[0].card)
        XCTAssertEqual(sections[1].tiles.map(\.id), ["t-resorted"])
        XCTAssertEqual(sections[1].tiles[0].card?.cluster, admin)
        XCTAssertEqual(sections[2].tiles.map(\.id), ["t-pinned", "t-new", "t-old"])
    }

    func testTileTitleIsTheCardTitleElseTheFirstLineOfText() {
        let voice = BoardTile(
            id: "t", head: head(.other), thought: thought(capturedAt: 1, text: ""), card: nil
        )
        XCTAssertNil(voice.title)
        let typed = BoardTile(
            id: "t", head: head(.other),
            thought: thought(capturedAt: 1, text: "  first line \nsecond"), card: nil
        )
        XCTAssertEqual(typed.title, "first line")
        let filed = BoardTile(
            id: "t", head: head(.other), thought: thought(capturedAt: 1, text: "first line"),
            card: card(thought: "t", cluster: .init(key: "k", name: "K", hue: 1), at: 1)
        )
        XCTAssertEqual(filed.title, "Card title")
    }

    /// After a snapshot rebuild the reader resumes the change feed with a cursor in the
    /// Worker's own encoding, built from the sequence the snapshot's high water names.
    func testChangeCursorAfterSnapshotUsesTheWorkerEncoding() {
        let highWater = "h." + Base64URL.encode(Data(#"{"v":1,"w":42}"#.utf8))
        XCTAssertEqual(VaultReader.snapshotSequence(highWater), 42)
        XCTAssertEqual(
            VaultReader.changeCursor(after: 42),
            "c." + Base64URL.encode(Data(#"{"v":1,"a":42}"#.utf8))
        )
        XCTAssertNil(VaultReader.snapshotSequence("c." + Base64URL.encode(Data(#"{"v":1,"a":42}"#.utf8))))
    }

    // MARK: - Fixtures

    private func head(_ object: BoardObject) -> BoardHead {
        BoardHead(revisionId: "rev", chunkCount: 1, object: object)
    }

    private func thought(capturedAt: Int64, text: String = "words") -> ThoughtManifest {
        ThoughtObject.manifest(
            deviceId: "phone", capturedAtUtcMs: capturedAt, origin: .typed, text: text,
            link: nil, attachments: []
        )
    }

    private func card(
        thought: String, cluster: CardManifest.Cluster, at writtenAt: Int64,
        pinned: Bool = false, archived: Bool = false
    ) -> CardManifest {
        CardManifest(
            format_version: 1,
            kind: CardObject.kind,
            thought_id: thought,
            thought_revision_id: "rev",
            title: "Card title",
            summary: "",
            tags: [],
            cluster: cluster,
            written_at_utc_ms: writtenAt,
            writer: CardManifest.Writer(device_id: "sorter", role: "sorter"),
            sorter: nil,
            pinned: pinned,
            archived: archived
        )
    }
}
