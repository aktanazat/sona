import XCTest
@testable import Sona

final class MeetingNotesTests: XCTestCase {
    @MainActor
    func testComparisonsPreserveMoneyAndIndependentDates() async {
        let cases: [(String, [String])] = [
            ("The fixtures cost $40,000 against a $30,000 cap.", ["$40,000", "$30,000"]),
            (
                "The fixtures cost $40,000 against a $30,000 cap, and the launch moves to March 21 instead of March 14.",
                ["$40,000", "$30,000"]),
            ("The launch is on March 21 and the demo is on April 2.", ["March 21", "April 2"]),
            (
                "The launch moves to March 21 instead of March 14, and the demo is on April 2.",
                ["April 2"]),
            (
                "The launch is on March 21 and the demo is on April 2, with $40,000 instead of $30,000.",
                ["March 21", "April 2", "$40,000", "$30,000"]),
        ]
        for (text, values) in cases {
            let digest = MeetingNotesDigest(notes(summary: text), transcript: [], speakerNames: [:])
            XCTAssertEqual(digest.figures.map(\.value), values, text)
        }
    }

    @MainActor
    func testDateHistoryKeepsItsTextWhileTheCurrentCardUsesItsOwnSource() async {
        let earlier = ArtifactCitation(segmentId: "earlier", startOffsetNs: 25_000_000_000, endOffsetNs: 30_000_000_000)
        let current = ArtifactCitation(segmentId: "current", startOffsetNs: 75_000_000_000, endOffsetNs: 82_000_000_000)
        let history = "The public launch of the harbour map is set for March 14 instead of March 1, and then again to March 21 due to council timing."
        let content = notes(
            summary: "The harbour map launch date was moved to March 21 due to the council meeting.",
            trace: [.init(line: 0, anchor: current)],
            decisions: [.init(text: history, citations: [earlier, current])])
        let digest = MeetingNotesDigest(content, transcript: [], speakerNames: [:])
        XCTAssertEqual(digest.figures.map(\.value), ["March 21"])
        XCTAssertEqual(digest.figures.first?.line.moment?.citation, current)
        XCTAssertEqual(digest.decisions.map(\.text), [history])
    }

    @MainActor
    private func notes(
        summary: String,
        trace: [ArtifactSummaryLineTrace] = [],
        decisions: [ArtifactCitedText] = []
    ) -> MeetingGeneratedArtifacts {
        MeetingGeneratedArtifacts(
            summary: .init(text: summary, citations: trace.map(\.anchor)),
            summaryTrace: trace, outline: [], decisions: decisions, actionItems: [],
            keyQuestions: [], risks: [], followUpDraft: .init(text: "", citations: []),
            ledger: nil, ledgerFailure: nil, plain: nil, plainBecause: nil, leftOut: nil)
    }
}
