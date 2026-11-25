import XCTest

/// The keyboard rules for Markdown notes and task details, run on plain text
/// with the caret as character offsets.
final class MarkdownEditTests: XCTestCase {
    private func edit(_ text: String, _ command: MarkdownCommand, from start: Int, to end: Int? = nil) -> MarkdownSpan? {
        MarkdownEdit.run(command, on: MarkdownSpan(text: text, start: start, end: end ?? start))
    }

    func testBoldWrapsTheSelectionAndUnwrapsItAgain() throws {
        let wrapped = try XCTUnwrap(edit("x", .mark("**"), from: 0, to: 1))
        XCTAssertEqual(wrapped, MarkdownSpan(text: "**x**", start: 2, end: 3))
        XCTAssertEqual(edit(wrapped.text, .mark("**"), from: wrapped.start, to: wrapped.end),
                       MarkdownSpan(text: "x", start: 0, end: 1))
    }

    func testItalicNextToBoldIsNotRemovedAsBold() throws {
        let bold = MarkdownSpan(text: "**x**", start: 2, end: 3)
        let result = try XCTUnwrap(edit(bold.text, .mark("*"), from: bold.start, to: bold.end))
        XCTAssertEqual(result.text, "***x***")
    }

    func testLinkSelectsThePlaceholderWhenNoAddressIsGiven() throws {
        let result = try XCTUnwrap(edit("see home", .link(nil), from: 4, to: 8))
        XCTAssertEqual(result, MarkdownSpan(text: "see [home](https://)", start: 11, end: 19))
    }

    func testLinkWithAnAddressPutsTheCaretAfterIt() throws {
        let result = try XCTUnwrap(edit("see home", .link("https://a.b"), from: 4, to: 8))
        XCTAssertEqual(result, MarkdownSpan(text: "see [home](https://a.b)", start: 23, end: 23))
    }

    func testReturnOnAnItemStartsTheNextItem() throws {
        let result = try XCTUnwrap(edit("- milk", .newline, from: 6))
        XCTAssertEqual(result, MarkdownSpan(text: "- milk\n- ", start: 9, end: 9))
    }

    func testReturnOnAnEmptyItemEndsTheList() throws {
        let result = try XCTUnwrap(edit("- milk\n- ", .newline, from: 9))
        XCTAssertEqual(result, MarkdownSpan(text: "- milk\n", start: 7, end: 7))
    }

    func testReturnRenumbersTheNextNumberedItem() throws {
        let result = try XCTUnwrap(edit("1. a", .newline, from: 4))
        XCTAssertEqual(result, MarkdownSpan(text: "1. a\n2. ", start: 8, end: 8))
    }

    func testNestingIndentsTheListLineAndLeavesPlainLinesAlone() throws {
        let result = try XCTUnwrap(edit("1. a\n2. b", .nest(outdent: false), from: 5))
        XCTAssertEqual(result.text, "1. a\n  1. b")
    }

    func testCheckingATaskFlipsItsBox() throws {
        let result = try XCTUnwrap(edit("- [ ] task", .check(nested: false), from: 0))
        XCTAssertEqual(result.text, "- [x] task")
    }

    func testCheckingAPlainLineMakesItATask() throws {
        let result = try XCTUnwrap(edit("text", .check(nested: false), from: 0))
        XCTAssertEqual(result.text, "- [ ] text")
    }

    func testNestedChecksFollowTheirParent() throws {
        let text = "- [ ] parent\n  - [ ] child\n- [ ] sibling"
        let result = try XCTUnwrap(edit(text, .check(nested: true), from: 0))
        XCTAssertEqual(result.text, "- [x] parent\n  - [x] child\n- [ ] sibling")
    }

    func testHeadingTogglesOffWhenRepeated() throws {
        let heading = try XCTUnwrap(edit("hello", .heading(2), from: 0))
        XCTAssertEqual(heading.text, "## hello")
        XCTAssertEqual(edit(heading.text, .heading(2), from: heading.start)?.text, "hello")
    }

    func testBulletTogglesOffWhenRepeated() throws {
        XCTAssertEqual(edit("- a", .block(.bullet), from: 2)?.text, "a")
    }

    func testCodeBlockWrapsTheLines() throws {
        let result = try XCTUnwrap(edit("x", .codeBlock, from: 0))
        XCTAssertEqual(result, MarkdownSpan(text: "```\nx\n```", start: 4, end: 4))
    }

    func testOnlyAPlainWebAddressPastesAsALink() {
        XCTAssertTrue(MarkdownEdit.isWebLink("https://openbenchmarks.com/x"))
        XCTAssertFalse(MarkdownEdit.isWebLink("not a link"))
        XCTAssertFalse(MarkdownEdit.isWebLink("javascript:alert(1)"))
    }
}
