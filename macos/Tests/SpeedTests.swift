import AppKit
import SwiftUI
import XCTest
@testable import Sona

/// Counts the work the app does for one user action, so a regression shows
/// as a number rather than a feeling.
final class SpeedTests: XCTestCase {
    func testALaunchWritesOnlyTheSpotlightRowsThatChanged() throws {
        let file = FileManager.default.temporaryDirectory
            .appending(path: "sona-speed-\(UUID().uuidString)/spotlight-index.json")
        defer { try? FileManager.default.removeItem(at: file.deletingLastPathComponent()) }
        var notes = Dictionary(uniqueKeysWithValues: (0..<500).map {
            ("n\($0)", SystemNote(id: "n\($0)", title: "Note \($0)", body: "Body \($0)"))
        })

        XCTAssertNil(SpotlightStamp.load(from: file))
        var first = SpotlightStamp()
        let firstChange = first.change(to: SpotlightStamp.digests(notes), for: NoteEntity.self)
        XCTAssertNil(firstChange.gone)
        XCTAssertEqual(firstChange.written.count, 500)
        first.kinds[SpotlightStamp.kind(NoteEntity.self)] = SpotlightStamp.digests(notes)
        try first.save(to: file)

        let relaunch = try XCTUnwrap(SpotlightStamp.load(from: file))
        let unchanged = relaunch.change(to: SpotlightStamp.digests(notes), for: NoteEntity.self)
        XCTAssertEqual(unchanged.gone, [])
        XCTAssertEqual(unchanged.written, [])

        notes["n1"] = SystemNote(id: "n1", title: "Note 1", body: "Edited")
        notes["n2"] = nil
        let edited = relaunch.change(to: SpotlightStamp.digests(notes), for: NoteEntity.self)
        XCTAssertEqual(edited.gone, ["n2"])
        XCTAssertEqual(edited.written, ["n1"])
        XCTAssertNil(relaunch.change(to: [:], for: TaskEntity.self).gone)

        var older = relaunch
        older.version = SpotlightStamp.currentVersion - 1
        try older.save(to: file)
        XCTAssertNil(SpotlightStamp.load(from: file))
    }

    /// Thirty rows drawn together used to write the receipts twice each, and
    /// every write redrew every row that reads them.
    @MainActor
    func testRowsDrawnTogetherGetTheirReceiptsInOneChange() async {
        let store = LibraryStore(core: Core())
        let changes = Counter()
        Self.count(changes, of: store)
        for id in Int64(1)...30 { store.loadReceipts(for: id) }
        while store.receipts.count < 30 { await Self.change(of: store) }
        XCTAssertEqual(changes.value, 1)
        store.loadReceipts(for: 7)
        XCTAssertEqual(changes.value, 1)
    }

    @MainActor
    func testPlayableLinesMatchWhatEachRowUsedToScanFor() {
        let tracks = [
            MeetingTrackSnapshot(trackId: "mic", sourceKind: .microphone, format: nil,
                                 firstOffsetNs: 0, lastOffsetNs: 300, durableRecordCount: 4),
            MeetingTrackSnapshot(trackId: "system", sourceKind: .systemAudio, format: nil,
                                 firstOffsetNs: 0, lastOffsetNs: 900, durableRecordCount: 0),
        ]
        let transcript = (0..<400).map { index in
            TranscriptEffectiveSegment(
                base: TranscriptSegment(
                    segmentId: "s\(index % 390)", transcriptRevisionId: "r", trackId: index % 3 == 0 ? "system" : "mic",
                    ordinal: index, startOffsetNs: Int64(index), endOffsetNs: Int64(index + 1),
                    speakerId: "a", text: "", confidenceMilli: nil),
                replacementText: nil, removed: index % 7 == 0, editRevision: nil,
                assignedSpeakerId: "a", speakerAssignment: .unknown)
        }
        let scanned = Set(transcript.map(\.base.segmentId).filter { id in
            guard let segment = transcript.first(where: { $0.base.segmentId == id && !$0.removed }) else { return false }
            return tracks.contains {
                $0.trackId == segment.base.trackId && $0.durableRecordCount > 0
                    && ($0.firstOffsetNs ?? .max) < segment.base.endOffsetNs
                    && ($0.lastOffsetNs ?? 0) > segment.base.startOffsetNs
            }
        })
        XCTAssertFalse(scanned.isEmpty)
        XCTAssertEqual(MeetingsStore.playable(transcript, tracks: tracks), scanned)
    }

    @MainActor
    private final class Counter {
        var value = 0
    }

    /// Counts every write, each as it happens, so writes in one turn are not
    /// folded together.
    @MainActor
    private static func count(_ counter: Counter, of store: LibraryStore) {
        withObservationTracking { _ = store.receipts } onChange: {
            MainActor.assumeIsolated {
                counter.value += 1
                count(counter, of: store)
            }
        }
    }

    @MainActor
    private static func change(of store: LibraryStore) async {
        await withCheckedContinuation { continuation in
            withObservationTracking { _ = store.receipts } onChange: { continuation.resume() }
        }
    }
}

/// The Markdown editor as the note and the task details host it: a window
/// that is never shown, the SwiftUI editor in it, and the text view under it
/// driven the way AppKit drives it.
@MainActor
final class MarkdownEditorTests: XCTestCase {
    private final class Note {
        var text: String
        var selection: TextSelection?
        var focused = false
        var document = UUID()
        var keys: [MarkdownEditorKey] = []

        init(_ text: String) {
            self.text = text
        }
    }

    private var window: NSWindow?
    private var host: NSHostingView<AnyView>?

    override func tearDown() async throws {
        window?.close()
        window = nil
        host = nil
    }

    private func editor(_ note: Note) -> AnyView {
        AnyView(MarkdownTextEditor(
            text: Binding { note.text } set: { note.text = $0 },
            selection: Binding { note.selection } set: { note.selection = $0 },
            focused: Binding { note.focused } set: { note.focused = $0 },
            document: note.document,
            label: "Note",
            keys: { key in
                note.keys.append(key)
                return key == .escape
            }).frame(width: 400))
    }

    private func open(_ note: Note) throws -> MarkdownTextView {
        let host = NSHostingView(rootView: editor(note))
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 400, height: 600), styleMask: [.titled],
            backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        host.layoutSubtreeIfNeeded()
        self.window = window
        self.host = host
        let view = try XCTUnwrap(Self.textView(in: host))
        XCTAssertTrue(window.makeFirstResponder(view))
        return view
    }

    private static func textView(in view: NSView) -> MarkdownTextView? {
        if let found = view as? MarkdownTextView { return found }
        for child in view.subviews {
            if let found = textView(in: child) { return found }
        }
        return nil
    }

    private func command(_ characters: String, code: UInt16) -> NSEvent? {
        NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: .command, timestamp: 0,
            windowNumber: window?.windowNumber ?? 0, context: nil, characters: characters,
            charactersIgnoringModifiers: characters, isARepeat: false, keyCode: code)
    }

    func testBoldIsOneStepOfUndoAndTheNoteFollowsBothWays() throws {
        let note = Note("make this bold")
        let view = try open(note)
        // Each key press is its own undo group in the app; the test has no
        // event loop, so it opens and closes them as the events would.
        let undo = try XCTUnwrap(view.undoManager)
        undo.groupsByEvent = false
        undo.beginUndoGrouping()
        view.setSelectedRange(NSRange(location: 0, length: 0))
        view.insertText("x", replacementRange: NSRange(location: 0, length: 0))
        undo.endUndoGrouping()
        view.setSelectedRange(NSRange(location: 11, length: 4))
        undo.beginUndoGrouping()
        XCTAssertTrue(view.performKeyEquivalent(with: try XCTUnwrap(command("b", code: 11))))
        undo.endUndoGrouping()
        XCTAssertEqual(note.text, "xmake this **bold**")
        XCTAssertEqual(note.selection.flatMap { $0.range(in: note.text) }.map { String(note.text[$0]) }, "bold")

        undo.undo()
        XCTAssertEqual(view.string, "xmake this bold")
        XCTAssertEqual(note.text, "xmake this bold")
        undo.redo()
        XCTAssertEqual(note.text, "xmake this **bold**")
        undo.undo()
        undo.undo()
        XCTAssertEqual(note.text, "make this bold")
    }

    func testShortcutLettersFollowTheLayoutWithUsPositionsForOtherScripts() throws {
        // Dvorak types B on the key US ANSI calls N (45), and X where US has B (11).
        XCTAssertEqual(MarkdownTextView.command(typed: "b", code: 45, modifiers: .command, link: nil), .mark("**"))
        XCTAssertNil(MarkdownTextView.command(typed: "x", code: 11, modifiers: .command, link: nil))
        // A Cyrillic layout types a letter on the US B key; the menus read it as Command-B.
        XCTAssertEqual(
            MarkdownTextView.command(typed: "\u{438}", code: 11, modifiers: .command, link: nil), .mark("**"))
        XCTAssertEqual(
            MarkdownTextView.command(typed: "S", code: 1, modifiers: [.command, .shift], link: nil), .mark("~~"))
        // AZERTY types an accented e on the US 2 key; headings stay on the key position.
        XCTAssertEqual(
            MarkdownTextView.command(typed: "\u{E9}", code: 19, modifiers: [.command, .option], link: nil),
            .heading(2))
        let note = Note("dvorak")
        let view = try open(note)
        view.setSelectedRange(NSRange(location: 0, length: 6))
        XCTAssertTrue(view.performKeyEquivalent(with: try XCTUnwrap(command("b", code: 45))))
        XCTAssertEqual(note.text, "**dvorak**")
    }

    func testComposedWordsLandWhileReturnAndEscapeReachTheirOwners() throws {
        let note = Note("- one")
        let view = try open(note)
        view.setSelectedRange(NSRange(location: 5, length: 0))
        view.setMarkedText("ka", selectedRange: NSRange(location: 2, length: 0),
                           replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertTrue(view.hasMarkedText())
        view.insertText("\u{304B}", replacementRange: view.markedRange())
        XCTAssertFalse(view.hasMarkedText())
        XCTAssertEqual(note.text, "- one\u{304B}")
        view.doCommand(by: #selector(NSResponder.insertNewline(_:)))
        XCTAssertEqual(note.text, "- one\u{304B}\n- ")
        view.doCommand(by: #selector(NSResponder.cancelOperation(_:)))
        XCTAssertEqual(note.keys, [.newline, .escape])
    }

    /// The note editor stays focused while another note is opened in it, by
    /// the add button, a note link or Spotlight.
    func testUndoAfterOpeningAnotherNoteKeepsThatNotesWords() throws {
        let note = Note("first note")
        let view = try open(note)
        let undo = try XCTUnwrap(view.undoManager)
        undo.groupsByEvent = false
        undo.beginUndoGrouping()
        view.insertText("!", replacementRange: NSRange(location: 10, length: 0))
        undo.endUndoGrouping()
        // The app has heard the focus report by the time another note opens.
        note.focused = true
        note.text = "second note"
        note.document = UUID()
        host?.rootView = editor(note)
        host?.layoutSubtreeIfNeeded()
        XCTAssertEqual(view.string, "second note")
        XCTAssertTrue(window?.firstResponder === view)
        undo.undo()
        XCTAssertEqual(view.string, "second note")
        XCTAssertEqual(note.text, "second note")
    }

    /// The note and the task details give the editor no width of their own:
    /// it sits in a page's column and must take that column's width.
    func testEditorOnAPageFillsTheColumnAndWrapsAtIt() throws {
        let column = 700 - 2 * Theme.margin
        try checkFills(minHeight: 240, width: column + 10) { editor in
            AnyView(Page { Text("New note"); editor.padding(.horizontal, -5) })
        }
        try checkFills(lineSpacing: 3, width: column + 10) { editor in
            AnyView(Page {
                Text("Details")
                ZStack(alignment: .topLeading) {
                    Text("Add details").allowsHitTesting(false)
                    editor.padding(.horizontal, -5)
                }
            })
        }
    }

    private func checkFills(
        minHeight: CGFloat = 0, lineSpacing: CGFloat = 0, width: CGFloat,
        line: UInt = #line, _ screen: (MarkdownTextEditor) -> AnyView
    ) throws {
        let note = Note("Weekly plan\n- ship the editor\n" + String(repeating: "make this bold ", count: 40))
        let editor = MarkdownTextEditor(
            text: Binding { note.text } set: { note.text = $0 },
            selection: Binding { note.selection } set: { note.selection = $0 },
            focused: Binding { note.focused } set: { note.focused = $0 },
            document: note.document, label: "Note", lineSpacing: lineSpacing, minHeight: minHeight)
        let host = NSHostingView(rootView: screen(editor))
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 700, height: 600), styleMask: [.titled],
            backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        self.window = window
        defer { window.close() }
        host.layoutSubtreeIfNeeded()
        let view = try XCTUnwrap(Self.textView(in: host), line: line)
        let container = try XCTUnwrap(view.textContainer, line: line)
        let layout = try XCTUnwrap(view.layoutManager, line: line)
        XCTAssertEqual(view.frame.width, width, accuracy: 1, line: line)
        XCTAssertEqual(container.size.width, view.frame.width, accuracy: 1, line: line)
        layout.ensureLayout(for: container)
        XCTAssertGreaterThan(layout.usedRect(for: container).width, width - 60, line: line)
        // Three source lines, the long one wrapped onto a handful at that width.
        var lines = 0
        layout.enumerateLineFragments(forGlyphRange: layout.glyphRange(for: container)) { _, _, _, _, _ in lines += 1 }
        XCTAssertGreaterThan(lines, 3, line: line)
        XCTAssertLessThan(lines, 20, line: line)
        XCTAssertEqual(
            view.frame.height, max(layout.usedRect(for: container).height, minHeight), accuracy: 2, line: line)
    }
}
