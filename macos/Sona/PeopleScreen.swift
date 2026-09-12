import SwiftUI

/// Everyone you have met on a recorded call, and what to know before the next one.
struct PeopleScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if let person = model.selectedPerson {
            PersonDetail(person: person)
        } else {
            list
        }
    }

    private var list: some View {
        Page {
            PageTitle("People", subtitle: "\(SampleData.people.count) people, \(organizations.count) organizations · names are linked only when you confirm them")
            ForEach(organizations, id: \.name) { organization in
                PageSection(organization.name) {
                    Card {
                        ForEach(organization.people) { person in
                            CardRow {
                                model.selectedPerson = person
                            } leading: {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(person.name).bodyText(16)
                                    Text(person.role).metaText()
                                }
                            } trailing: {
                                HStack(spacing: 24) {
                                    Text("\(person.meetings) meetings").metaText()
                                    Text("last \(person.lastMet.relativeDay.lowercased())").metaText()
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    private var organizations: [(name: String, people: [Person])] {
        var order: [String] = []
        var groups: [String: [Person]] = [:]
        for person in SampleData.people {
            if groups[person.organization] == nil { order.append(person.organization) }
            groups[person.organization, default: []].append(person)
        }
        return order.map { ($0, groups[$0] ?? []) }
    }
}

struct PersonDetail: View {
    @Environment(AppModel.self) private var model
    let person: Person

    var body: some View {
        Page {
            BackLink(title: "People") { model.selectedPerson = nil }
            PageTitle(person.name, subtitle: "\(person.role) · \(person.organization) · \(person.meetings) meetings") {
                HStack(spacing: 10) {
                    Button {
                        model.chatShown = true
                    } label: {
                        Label("Prepare for the next call", systemImage: "bubble.left")
                    }
                    .buttonStyle(.primary)
                    Button("Rename") {}.buttonStyle(.secondary)
                    Button("Merge") {}.buttonStyle(.quiet)
                }
            }
            PageSection("Summary") {
                Card {
                    Text(person.summary)
                        .bodyText(16)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(20)
                }
            }
            if !person.context.isEmpty {
                PageSection("Before you next speak") {
                    Card {
                        ForEach(person.context, id: \.self) { line in
                            CardRow { Text(line).bodyText() }
                        }
                    }
                }
            }
            PageSection("Meetings together") {
                Card {
                    ForEach(SampleData.meetings.filter { $0.people.contains(person.name) }) { meeting in
                        CardRow {
                            model.place = .meetings
                            model.selectedMeeting = meeting
                        } leading: {
                            Text(meeting.title).bodyText()
                        } trailing: {
                            HStack(spacing: 24) {
                                Text(meeting.app).metaText()
                                Text(meeting.duration.spoken).metaText()
                                Text(meeting.date.short).metaText()
                            }
                        }
                    }
                }
            }
        }
    }
}
