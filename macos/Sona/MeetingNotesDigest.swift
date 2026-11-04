import Foundation

/// The notes in the order a reader scans them: the outcome, what was decided,
/// who does what by when, the numbers, what is still open, then what was
/// covered. Every word is the model's; this orders and splits them and names
/// who was talking at the moment each line came from.
struct MeetingNotesDigest {
    /// When a line was said, and that moment as the page writes it.
    struct Moment {
        let citation: ArtifactCitation
        let clock: String
    }

    struct Line: Identifiable {
        let id: Int
        let text: String
        let moment: Moment?
        /// Who was talking in the cited words, when the transcript names them.
        let speaker: String?
    }

    struct Action: Identifiable {
        /// The item's place in the generation, which its done state is kept by.
        let id: Int
        let line: Line
        let owner: String?
        let due: String?
    }

    /// A figure the notes state, with the sentence that states it.
    struct Figure: Identifiable {
        let id: Int
        let value: String
        let line: Line
    }

    struct Topic: Identifiable {
        let id: Int
        let line: Line
        let points: [String]
    }

    let outcome: Line?
    let decisions: [Line]
    let actions: [Action]
    let figures: [Figure]
    let questions: [Line]
    let risks: [Line]
    let topics: [Topic]
    /// With no outline, each summary line's sentences that the sections above
    /// do not already say.
    let covered: [Line]

    init(
        _ content: MeetingGeneratedArtifacts,
        transcript: [TranscriptEffectiveSegment],
        speakerNames: [SpeakerId: String]
    ) {
        let voices = Dictionary(
            transcript.compactMap { segment in
                speakerNames[segment.assignedSpeakerId].map { (segment.base.segmentId, $0) }
            },
            uniquingKeysWith: { first, _ in first })
        let outlineTexts = content.outline.flatMap { [$0.title] + ($0.detail.map { [$0] } ?? []) }
        // "1:00" beside "1:04:00" reads as an hour mark, so once any time in
        // the notes passes an hour, every time shows its hours: "0:01:00".
        let hours = ([content.summary] + content.decisions + content.actionItems.map(\.text)
            + content.keyQuestions + content.risks + outlineTexts)
            .flatMap(\.citations)
            .contains { $0.startOffsetNs >= Self.hourNs }
        func line(_ id: Int, _ text: String, _ citations: [ArtifactCitation]) -> Line {
            var names: [String] = []
            for citation in citations {
                if let name = voices[citation.segmentId], !names.contains(name) { names.append(name) }
            }
            return Line(
                id: id,
                text: text.trimmingCharacters(in: .whitespacesAndNewlines),
                moment: citations.first.map {
                    Moment(citation: $0, clock: Self.clock($0.startOffsetNs, hours: hours))
                },
                speaker: names.isEmpty ? nil : names.prefix(2).joined(separator: ", "))
        }
        func lines(_ items: [ArtifactCitedText]) -> [Line] {
            items.enumerated()
                .map { line($0.offset, $0.element.text, $0.element.citations) }
                .filter { !$0.text.isEmpty }
        }

        let anchors = Dictionary(
            (content.summaryTrace ?? []).map { ($0.line, $0.anchor) },
            uniquingKeysWith: { first, _ in first })
        let summary = content.summary.text
            .split(separator: "\n", omittingEmptySubsequences: false)
            .enumerated()
            .map { line($0.offset, String($0.element), anchors[$0.offset].map { [$0] } ?? []) }
            .filter { !$0.text.isEmpty }

        let headline = content.ledger?.headline.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        outcome = headline.isEmpty
            ? summary.first.map { first in
                Line(
                    id: first.id, text: Self.sentences(first.text).first ?? first.text,
                    moment: first.moment, speaker: first.speaker)
            }
            : Line(id: 0, text: headline, moment: nil, speaker: nil)
        decisions = lines(content.decisions)
        actions = content.actionItems.enumerated()
            .map { index, item in
                Action(
                    id: index,
                    line: line(index, item.text.text, item.text.citations),
                    owner: Self.words(item.ownerText),
                    due: Self.words(item.dueText))
            }
            .filter { !$0.line.text.isEmpty }
        questions = lines(content.keyQuestions)
        risks = lines(content.risks)
        topics = content.outline.enumerated()
            .map { index, topic in
                Topic(
                    id: index,
                    line: line(index, topic.title.text, topic.title.citations),
                    points: Self.sentences(topic.detail?.text ?? ""))
            }
            .filter { !$0.line.text.isEmpty }

        var said = Set(
            ([outcome].compactMap { $0 } + decisions + actions.map(\.line) + questions + risks)
                .flatMap { Self.sentences($0.text) }
                .map(Self.key))
        var covered: [Line] = []
        if content.outline.isEmpty {
            for source in summary {
                let fresh = Self.sentences(source.text).filter { said.insert(Self.key($0)).inserted }
                guard !fresh.isEmpty else { continue }
                covered.append(Line(
                    id: covered.count, text: fresh.joined(separator: " "),
                    moment: source.moment, speaker: source.speaker))
            }
        }
        self.covered = covered

        let outlineLines = outlineTexts.map { line(0, $0.text, $0.citations) }
        let sources = decisions + actions.map(\.line) + summary + outlineLines + questions + risks
            + [outcome].compactMap { $0 }
        var seen = Set<String>()
        var figures: [Figure] = []
        search: for source in sources {
            for sentence in Self.sentences(source.text) {
                for match in sentence.matches(of: Self.figure) {
                    let value = String(match.output).trimmingCharacters(in: .whitespaces)
                    guard seen.insert(Self.key(value)).inserted else { continue }
                    figures.append(Figure(
                        id: figures.count,
                        value: value,
                        line: Line(
                            id: figures.count, text: sentence,
                            moment: source.moment, speaker: source.speaker)))
                    if figures.count == Self.figureLimit { break search }
                }
            }
        }
        self.figures = figures
    }

    private static let figureLimit = 8
    private static let hourNs: Int64 = 3_600_000_000_000

    private static func clock(_ offsetNs: Int64, hours: Bool) -> String {
        guard hours else { return offsetNs.meetingOffsetClock }
        let seconds = Int(offsetNs / 1_000_000_000)
        return String(format: "%d:%02d:%02d", seconds / 3600, seconds % 3600 / 60, seconds % 60)
    }

    /// Money, shares, calendar dates, clock times, and counts of time or
    /// people: the figures a reader looks for first.
    private static let figure = #/
        [$€£¥] \d[\d,]* (?:\.\d+)? (?:k|m|bn|\s(?:thousand|million|billion))? \b
      | \b \d[\d,]* (?:\.\d+)? (?:k|m|bn|\s(?:thousand|million|billion))? \s? (?:dollars?|usd|euros?|eur|pounds?|gbp|yen) \b
      | \b \d+ (?:\.\d+)? \s? (?:%|percent\b)
      | \b (?:jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?) \.? \s \d{1,2} (?:st|nd|rd|th)? \b (?:,?\s\d{4}\b)?
      | \b \d{1,2} (?:st|nd|rd|th)? \s (?:of\s)? (?:jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?) \b (?:,?\s\d{4}\b)?
      | \b \d{4}-\d{2}-\d{2} \b
      | \b \d{1,2} (?::\d{2})? \s? (?:am|pm) \b
      | \b \d[\d,]* (?:\.\d+)? [\s-] (?:seconds?|minutes?|mins?|hours?|hrs?|days?|weeks?|months?|quarters?|years?|users?|customers?|people|seats?|accounts?|engineers?|hires?|licenses?|tickets?|deals?) \b
    /#.ignoresCase()

    static func sentences(_ text: String) -> [String] {
        var result: [String] = []
        text.enumerateSubstrings(in: text.startIndex..<text.endIndex, options: .bySentences) { sentence, _, _, _ in
            guard let sentence = sentence?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !sentence.isEmpty
            else { return }
            result.append(sentence)
        }
        return result
    }

    private static func words(_ text: String?) -> String? {
        guard let text = text?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty else {
            return nil
        }
        return text
    }

    /// Two lines that differ only in case and punctuation say the same thing.
    private static func key(_ text: String) -> String {
        text.lowercased()
            .split(whereSeparator: { !$0.isLetter && !$0.isNumber })
            .joined(separator: " ")
    }
}
