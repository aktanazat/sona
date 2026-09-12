import SwiftUI

/// Meetings noticed and recorded, newest first; one meeting at a time in detail.
struct MeetingsScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if let meeting = model.selectedMeeting {
            MeetingDetail(meeting: meeting)
        } else {
            list
        }
    }

    private var list: some View {
        Page {
            PageTitle("Meetings", subtitle: "\(SampleData.meetings.count) this week · \(totalDuration.spoken) · every one announced first") {
                Button {
                } label: {
                    Label("Record the current call", systemImage: "video")
                }
                .buttonStyle(.primary)
            }
            ForEach(days, id: \.day) { group in
                PageSection(group.day) {
                    Card {
                        ForEach(group.items) { meeting in
                            CardRow {
                                model.selectedMeeting = meeting
                            } leading: {
                                VStack(alignment: .leading, spacing: 4) {
                                    HStack(spacing: 10) {
                                        Text(meeting.title).bodyText(16)
                                        if !meeting.loops.isEmpty {
                                            Chip("\(meeting.loops.count) open")
                                        }
                                    }
                                    Text(meeting.people.joined(separator: ", ")).metaText()
                                }
                            } trailing: {
                                HStack(spacing: 24) {
                                    Text(meeting.app).metaText()
                                    Text(meeting.duration.spoken).metaText()
                                    Text(meeting.date.time).metaText()
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    private var totalDuration: TimeInterval {
        SampleData.meetings.reduce(0) { $0 + $1.duration }
    }

    private var days: [(day: String, items: [Meeting])] {
        var order: [String] = []
        var groups: [String: [Meeting]] = [:]
        for meeting in SampleData.meetings {
            let day = meeting.date.relativeDay
            if groups[day] == nil { order.append(day) }
            groups[day, default: []].append(meeting)
        }
        return order.map { ($0, groups[$0] ?? []) }
    }
}

struct MeetingDetail: View {
    @Environment(AppModel.self) private var model
    let meeting: Meeting

    var body: some View {
        Page {
            BackLink(title: "Meetings") { model.selectedMeeting = nil }
            PageTitle(meeting.title, subtitle: "\(meeting.app) · \(meeting.date.relativeDay), \(meeting.date.time) · \(meeting.duration.spoken)") {
                HStack(spacing: 10) {
                    Button {
                        model.chatShown = true
                    } label: {
                        Label("Ask about this meeting", systemImage: "bubble.left")
                    }
                    .buttonStyle(.primary)
                    Button("Copy summary") {}.buttonStyle(.secondary)
                }
            }
            HStack(spacing: 8) {
                ForEach(meeting.people, id: \.self) { name in
                    Button(name) {
                        if let person = SampleData.people.first(where: { $0.name == name }) {
                            model.place = .people
                            model.selectedPerson = person
                        }
                    }
                    .buttonStyle(.compact)
                }
            }
            .padding(.bottom, 24)
            PageSection("Summary") {
                Card {
                    Text(meeting.summary)
                        .bodyText(16)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(20)
                }
            }
            if !meeting.decisions.isEmpty {
                PageSection("Decisions") {
                    Card {
                        ForEach(meeting.decisions, id: \.self) { decision in
                            CardRow { Text(decision).bodyText() }
                        }
                    }
                }
            }
            if !meeting.actions.isEmpty {
                PageSection("Action items") {
                    Card {
                        ForEach(meeting.actions) { action in
                            CardRow {
                                Text(action.text).bodyText()
                            } trailing: {
                                Text("\(action.owner) · \(action.due)").metaText()
                            }
                        }
                    }
                }
            }
            if !meeting.loops.isEmpty {
                PageSection("Open loops") {
                    Card {
                        ForEach(meeting.loops, id: \.self) { loop in
                            CardRow {
                                Text(loop).bodyText()
                            } trailing: {
                                HStack(spacing: 8) {
                                    Button("Resolve") {}.buttonStyle(.compact)
                                    Button("Assign") {}.buttonStyle(.compact)
                                }
                            }
                        }
                    }
                }
            }
            PageSection("Transcript") {
                Card {
                    CardRow {
                        Text("\(meeting.duration.spoken), \(meeting.people.count) speakers, names confirmed by you.").bodyText()
                    } trailing: {
                        Button("Read") {}.buttonStyle(.compact)
                    }
                    CardRow {
                        Text("Export as Markdown, text, or the original audio.").bodyText()
                    } trailing: {
                        Button("Export") {}.buttonStyle(.compact)
                    }
                }
            }
        }
    }
}
