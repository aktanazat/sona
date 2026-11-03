import SwiftUI

struct MemoryHubView: View {
    @Environment(AppModel.self) private var model
    @State private var people: [PersonListEntry] = []
    @State private var entries: [MemoryHomeEntry] = []
    @State private var peopleError: String?
    @State private var notesError: String?
    @State private var conversationsError: String?
    @State private var loading = true

    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            if !people.isEmpty {
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        Text("People").sectionLabel()
                        Spacer()
                        Button("All people") { model.go(.people) }.buttonStyle(.quiet)
                    }
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(people) { entry in
                                Button { model.openPerson(entry.id) } label: {
                                    HStack(spacing: 8) {
                                        PersonPortrait(
                                            personId: entry.person.id,
                                            name: entry.person.displayName,
                                            size: 28,
                                            revision: model.people.photoRevision)
                                        Text(entry.person.displayName).font(TypeScale.body(13))
                                    }
                                    .foregroundStyle(Theme.ink)
                                    .padding(.leading, 6).padding(.trailing, 12).padding(.vertical, 5)
                                    .background(Theme.surface, in: Capsule())
                                    .overlay(Capsule().strokeBorder(Theme.border, lineWidth: 1))
                                }
                                .buttonStyle(.plain)
                                .accessibilityLabel("Open \(entry.person.displayName)")
                            }
                        }
                    }
                }
            }
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text("Conversations and notes").sectionLabel()
                    Spacer()
                    Button("New note") {
                        model.go(.scratchpad)
                        Task { await model.scratchpad.newNote() }
                    }
                    .buttonStyle(.compact)
                    Button("Record conversation") { model.recordMeeting() }.buttonStyle(.compact)
                }
                if loading {
                    Text("Reading your recent work…").metaText()
                } else if entries.isEmpty && notesError == nil && conversationsError == nil {
                    HStack(alignment: .firstTextBaseline) {
                        Text("Your conversations and notes will appear here.").bodyText(14, Theme.inkSecondary)
                        Spacer()
                    }
                    .padding(.vertical, 12)
                } else {
                    VStack(spacing: 0) {
                        ForEach(entries) { entry in
                            Button { open(entry) } label: {
                                HStack(alignment: .top, spacing: 12) {
                                    Image(systemName: entry.isNote ? "square.and.pencil" : "bubble.left.and.bubble.right")
                                        .font(.system(size: 14)).foregroundStyle(Theme.inkTertiary)
                                        .frame(width: 20).padding(.top, 2)
                                    VStack(alignment: .leading, spacing: 4) {
                                        Text(entry.title).bodyText(14).lineLimit(1)
                                        if !entry.detail.isEmpty {
                                            Text(entry.detail).metaText().lineLimit(2)
                                        }
                                    }
                                    Spacer(minLength: 16)
                                    Text(entry.date, style: .relative).metaText()
                                }
                                .padding(.vertical, 12)
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            Hairline()
                        }
                    }
                }
                if let peopleError { retryLine(peopleError) }
                if let conversationsError { retryLine(conversationsError) }
                if let notesError { retryLine(notesError) }
                HStack(spacing: 16) {
                    Button("All conversations") { model.go(.meetings) }.buttonStyle(.quiet)
                    Button("All notes") { model.go(.scratchpad) }.buttonStyle(.quiet)
                    if people.isEmpty {
                        Button("People") { model.go(.people) }.buttonStyle(.quiet)
                    }
                }
            }
        }
        .padding(.vertical, 12).padding(.bottom, 16)
        .task { await load() }
    }

    private func recentEntries(notes: [ScratchNote], conversations: [MeetingHistorySummary]) -> [MemoryHomeEntry] {
        let notes = notes.map {
            MemoryHomeEntry(id: "note:\($0.id)", targetId: $0.id, isNote: true,
                title: $0.title, detail: $0.preview,
                date: Date(timeIntervalSince1970: Double($0.updatedAtMs) / 1_000))
        }
        let meetings = conversations.map {
            MemoryHomeEntry(id: "meeting:\($0.id)", targetId: $0.id, isNote: false,
                title: $0.title, detail: $0.headline?.text ?? ($0.speakerLabels ?? []).joined(separator: ", "),
                date: $0.date)
        }
        return Array((notes + meetings).sorted {
            $0.date == $1.date ? $0.id < $1.id : $0.date > $1.date
        }.prefix(8))
    }

    private func open(_ entry: MemoryHomeEntry) {
        if entry.isNote {
            model.open(link: "sona://note/\(entry.targetId)")
        } else {
            model.openMeeting(entry.targetId)
        }
    }

    private func retryLine(_ message: String) -> some View {
        HStack {
            Text(message).metaText(Theme.live)
            Button("Try again") { Task { await load() } }.buttonStyle(.quiet)
        }
    }

    private func load() async {
        loading = true
        async let people: Void = loadPeople()
        async let notes = loadNotes()
        async let conversations = loadConversations()
        let (_, loadedNotes, loadedConversations) = await (people, notes, conversations)
        guard !Task.isCancelled else { return }
        entries = recentEntries(notes: loadedNotes, conversations: loadedConversations)
        loading = false
    }

    private func loadPeople() async {
        do {
            let result: PeopleListResult = try await model.core.request("people_list")
            guard !Task.isCancelled else { return }
            people = Array(result.entries.sorted {
                let aHasConversation = $0.lastMeetingAtUtcMs != nil
                let bHasConversation = $1.lastMeetingAtUtcMs != nil
                if aHasConversation != bHasConversation { return aHasConversation }
                let a = $0.lastMeetingAtUtcMs ?? $0.person.updatedAtUtcMs
                let b = $1.lastMeetingAtUtcMs ?? $1.person.updatedAtUtcMs
                return a == b ? $0.id < $1.id : a > b
            }.prefix(6))
            peopleError = nil
        } catch {
            guard !Task.isCancelled else { return }
            peopleError = PeopleStore.message(error)
        }
    }

    private func loadNotes() async -> [ScratchNote] {
        do {
            let result: [ScratchNote] = try await model.core.request("scratchpad_recent", ["limit": 8])
            guard !Task.isCancelled else { return [] }
            notesError = nil
            return result
        } catch {
            guard !Task.isCancelled else { return [] }
            notesError = "Notes could not be read. \(error.localizedDescription)"
            return []
        }
    }

    private func loadConversations() async -> [MeetingHistorySummary] {
        do {
            let result: MeetingsPage = try await model.core.request("meeting_list", ["limit": 8])
            guard !Task.isCancelled else { return [] }
            conversationsError = nil
            return result.entries
        } catch {
            guard !Task.isCancelled else { return [] }
            conversationsError =
                (error as? CoreError)?.remote(as: MeetingCommandError.self)?.label
                ?? error.localizedDescription
            return []
        }
    }

}

private struct MemoryHomeEntry: Identifiable {
    let id: String
    let targetId: String
    let isNote: Bool
    let title: String
    let detail: String
    let date: Date
}
