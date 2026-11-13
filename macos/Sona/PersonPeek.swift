import AppKit
import SwiftUI

/// Space on the People list: the highlighted person at a glance beside the
/// list. J and K carry the peek along; Space or Esc closes it and Return
/// opens the person's page.
struct PersonPeek: View {
    let entry: PersonListEntry
    var photoRevision: UInt64 = 0
    @Environment(AppModel.self) private var model

    var body: some View {
        let person = entry.person
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Spacer(minLength: 8)
                TaskKeyButton(title: "Open", keys: "return") { PeopleListView.open(person.id, model) }
                WorkBarButton(icon: "xmark", help: "Close  Space") { model.workFocus.peeking = false }
            }
            .padding(.leading, 20)
            .padding(.trailing, 12)
            .frame(height: Theme.headerHeight)
            Hairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    HStack(spacing: 12) {
                        PersonPortrait(personId: person.id, name: person.displayName, size: 48, revision: photoRevision)
                        VStack(alignment: .leading, spacing: 4) {
                            Text(person.displayName)
                                .font(.system(size: 17, weight: .semibold))
                                .foregroundStyle(Theme.ink)
                                .textSelection(.enabled)
                            let place = [person.role, person.organization].compactMap { $0 }.joined(separator: " · ")
                            if !place.isEmpty {
                                Text(place).metaText()
                            }
                            if !person.aliases.isEmpty {
                                Text("Also \(person.aliases.joined(separator: ", "))").metaText()
                            }
                        }
                    }
                    if let summary = person.summary?.text, !summary.isEmpty {
                        Text(summary)
                            .bodyText(13, Theme.inkSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                    }
                    if let last = entry.lastMeeting {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Last met \(PeopleFormat.elapsed(last.at))").metaText()
                            Text(last.title).font(TypeScale.list).foregroundStyle(Theme.ink)
                            if let headline = last.headline?.text, !headline.isEmpty {
                                Text(headline)
                                    .bodyText(13, Theme.inkSecondary)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        }
                    } else if let last = entry.lastMeetingAtUtcMs {
                        Text("Last met \(PeopleFormat.elapsed(PeopleFormat.date(last)))").metaText()
                    }
                    VStack(alignment: .leading, spacing: 8) {
                        LabeledContent("Meetings", value: "\(entry.confirmedCount)")
                        if entry.suggestedCount > 0 {
                            LabeledContent("To confirm", value: "\(entry.suggestedCount)")
                        }
                        ForEach(person.calendarEmails, id: \.self) { email in
                            LabeledContent("Email") { Text(email).textSelection(.enabled) }
                        }
                    }
                    .font(TypeScale.list)
                }
                .padding(20)
            }
        }
        .frame(width: 420)
        .frame(maxHeight: .infinity)
        .background {
            Rectangle()
                .fill(Theme.elevated)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.border).frame(width: 1) }
                .elevation(.high)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Peek at \(person.displayName)")
    }
}

/// Keys on the People list: J, K and the arrows move, Space peeks, Return
/// opens and Esc closes the peek.
@MainActor
enum PeopleListKeys {
    static func handle(_ event: NSEvent, key: String, flags: NSEvent.ModifierFlags, _ model: AppModel) -> Bool {
        guard flags.isEmpty, !model.people.listFailed, let people = model.people.entries, !people.isEmpty
        else { return false }
        let focus = model.workFocus
        let index = focus.personHighlighted.flatMap { id in people.firstIndex { $0.person.id == id } }
        func move(_ delta: Int) -> Bool {
            let next = index.map { min(max($0 + delta, 0), people.count - 1) } ?? (delta > 0 ? 0 : people.count - 1)
            focus.keyPerson(people[next].person.id)
            return true
        }
        if event.keyCode == 53 {
            guard focus.peeking else { return false }
            focus.peeking = false
            return true
        }
        switch event.specialKey {
        case .downArrow?: return move(1)
        case .upArrow?: return move(-1)
        case .carriageReturn?, .enter?:
            guard let index else { return false }
            PeopleListView.open(people[index].person.id, model)
            return true
        default: break
        }
        switch key {
        case "j": return move(1)
        case "k": return move(-1)
        case " ":
            if focus.peeking {
                focus.peeking = false
                return true
            }
            if index == nil { _ = move(1) }
            focus.peeking = true
            return true
        default: return false
        }
    }
}
