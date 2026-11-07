import SwiftUI

struct PersonFirstMet: Decodable, Equatable {
    let meetingId: MeetingSessionId
    let atUtcMs: Int64
    let place: String?
    let introduction: PersonIntroduction?
}

struct PersonIntroduction: Decodable, Equatable {
    let suggestionId: String
    let introducerName: String
    let quote: String
    let segmentId: TranscriptSegmentId?
    let confirmed: Bool
}

/// When he first met this person on record and where. Who introduced them
/// is named only once he says yes; before that it is a question.
struct PersonFirstMetSection: View {
    let personId: String
    /// The page's revision: a new one reads the first meeting again.
    let revision: UInt64
    var openMeeting: (String) -> Void = { _ in }
    @Environment(AppModel.self) private var model
    @State private var firstMet: PersonFirstMet?
    @State private var failure: String?
    @State private var deciding = false

    private struct ReadKey: Equatable {
        let personId: String
        let revision: UInt64
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if firstMet != nil || failure != nil {
                PageSection("First met") {
                    Card {
                        VStack(alignment: .leading, spacing: 10) {
                            if let firstMet {
                                summary(firstMet)
                                if let introduction = firstMet.introduction, !introduction.confirmed {
                                    question(introduction, meetingId: firstMet.meetingId)
                                }
                            }
                            if let failure { Text(failure).metaText(Theme.live) }
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 14)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
        }
        .task(id: ReadKey(personId: personId, revision: revision)) { await load() }
    }

    private func summary(_ firstMet: PersonFirstMet) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Button { openMeeting(firstMet.meetingId) } label: {
                Text(line(firstMet)).bodyText(14).multilineTextAlignment(.leading)
            }
            .buttonStyle(.plain)
            .accessibilityHint("Opens the meeting")
            Spacer(minLength: 8)
            if let introduction = firstMet.introduction, introduction.confirmed {
                play(introduction, meetingId: firstMet.meetingId)
            }
        }
    }

    private func question(_ introduction: PersonIntroduction, meetingId: MeetingSessionId) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("Introduced by \(introduction.introducerName)?").bodyText(13)
                Spacer(minLength: 8)
                play(introduction, meetingId: meetingId)
                Button("No") { decide(introduction, accept: false) }
                    .buttonStyle(.quiet)
                Button("Yes") { decide(introduction, accept: true) }
                    .buttonStyle(QuietButton(compact: true))
            }
            .disabled(deciding)
            Text("“\(introduction.quote)”").bodyText(13, Theme.inkSecondary).lineLimit(3)
        }
    }

    @ViewBuilder
    private func play(_ introduction: PersonIntroduction, meetingId: MeetingSessionId) -> some View {
        if let segmentId = introduction.segmentId {
            Button {
                model.meetings.playContribution(sessionId: meetingId, segmentId: segmentId)
            } label: {
                Image(systemName: "play.fill")
            }
            .buttonStyle(QuietButton(color: Theme.accent, compact: true))
            .accessibilityLabel("Play the introduction")
        }
    }

    private func line(_ firstMet: PersonFirstMet) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(firstMet.atUtcMs) / 1000)
        let sameYear = Calendar.current.isDate(date, equalTo: Date(), toGranularity: .year)
        let day = Date.FormatStyle.dateTime.month(.abbreviated).day()
        var parts = [date.formatted(sameYear ? day : day.year())]
        if let place = firstMet.place { parts.append(place) }
        if let introduction = firstMet.introduction, introduction.confirmed {
            parts.append("introduced by \(introduction.introducerName)")
        }
        return parts.joined(separator: " · ")
    }

    private func decide(_ introduction: PersonIntroduction, accept: Bool) {
        deciding = true
        Task {
            defer { deciding = false }
            do {
                firstMet = try await model.core.request(
                    "person_first_met_decide",
                    FirstMetDecision(personId: personId, suggestionId: introduction.suggestionId, accept: accept))
                failure = nil
            } catch {
                failure = error.localizedDescription
            }
        }
    }

    private func load() async {
        do {
            let result: PersonFirstMet? = try await model.core.request("person_first_met", ["personId": personId])
            guard !Task.isCancelled else { return }
            firstMet = result
            failure = nil
        } catch {
            guard !Task.isCancelled else { return }
            failure = "When you first met could not be read."
        }
    }
}

private struct FirstMetDecision: Encodable {
    let personId: String
    let suggestionId: String
    let accept: Bool
}
