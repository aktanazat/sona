import ImageIO
import SwiftUI
import UIKit

enum PhoneMemorySection: String, CaseIterable, Identifiable {
    case people, notes, bookmarks

    var id: String { rawValue }
    var title: String {
        switch self {
        case .people: return String(localized: "memory.people", defaultValue: "People")
        case .notes: return String(localized: "memory.notes", defaultValue: "Saved notes")
        case .bookmarks: return String(localized: "memory.bookmarks", defaultValue: "Bookmarks")
        }
    }
    var symbol: String {
        switch self {
        case .people: return "person.2"
        case .notes: return "note.text"
        case .bookmarks: return "bookmark"
        }
    }
}

struct PhoneMemoryScreen: View {
    @ObservedObject var model: AppModel
    var selection: PhoneMemorySection = .people
    var search = ""

    var body: some View {
        PhoneMemoryContent(model: model, catalogue: model.board.flatMap { board in
            board.memoryIndex.map { PhoneMemoryCatalogue(index: $0, meetings: board.meetings) }
        }, selection: selection, search: search)
    }
}

private struct PhoneMemoryContent: View {
    @ObservedObject var model: AppModel
    let catalogue: PhoneMemoryCatalogue?
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @State private var selection: PhoneMemorySection
    @State private var search: String

    init(model: AppModel, catalogue: PhoneMemoryCatalogue?, selection: PhoneMemorySection, search: String) {
        self.model = model
        self.catalogue = catalogue
        _selection = State(initialValue: selection)
        _search = State(initialValue: search)
    }

    var body: some View {
        List {
            Section {
                Group {
                    if dynamicTypeSize.isAccessibilitySize {
                        contentPicker.pickerStyle(.menu)
                    } else {
                        contentPicker.pickerStyle(.segmented)
                    }
                }
                .frame(minHeight: 44)
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.clear)
                .listRowSeparator(.hidden)
                if model.boardState == .syncing {
                    ProgressView("board.syncing").listRowBackground(Color.clear)
                } else if model.boardState == .offline {
                    Text("board.offline").font(.footnote).foregroundStyle(Theme.textSecondary)
                        .listRowBackground(Color.clear)
                }
            }
            if let catalogue {
                PhoneMemorySnapshotRows(model: model, catalogue: catalogue, selection: selection, search: search)
                Section {
                    Text(String(localized: "memory.updated", defaultValue: "Updated \(catalogue.index.producedAt.formatted(date: .abbreviated, time: .shortened))"))
                        .font(.footnote)
                        .foregroundStyle(Theme.textSecondary)
                        .listRowBackground(Color.clear)
                }
            } else if !model.isPaired {
                Text("library.pairToRead").foregroundStyle(Theme.textSecondary).listRowBackground(Theme.surface)
            } else if model.boardState != .syncing {
                Text(String(localized: "memory.notSynced", defaultValue: "No Mac memory synced."))
                    .foregroundStyle(Theme.textSecondary).listRowBackground(Theme.surface)
            }
        }
        .phoneMemoryList()
        .navigationTitle(selection.title)
        .navigationBarTitleDisplayMode(.inline)
        .searchable(text: $search)
        .toolbar(.hidden, for: .tabBar)
        .task { await model.refreshBoard() }
        .refreshable { await model.refreshBoard() }
    }

    private var contentPicker: some View {
        Picker(String(localized: "memory.content", defaultValue: "Memory"), selection: $selection) {
            ForEach(PhoneMemorySection.allCases) { section in Text(section.title).tag(section) }
        }
    }
}

private struct PhoneMemorySnapshotRows: View {
    @ObservedObject var model: AppModel
    let catalogue: PhoneMemoryCatalogue
    let selection: PhoneMemorySection
    let search: String

    var body: some View {
        Section {
            switch selection {
            case .people:
                let people = catalogue.index.people.filter { person in
                    search.isEmpty || person.display_name.localizedStandardContains(search) ||
                        (catalogue.personConversations[person.id] ?? []).contains { group in
                            group.conversation.meeting.session.title.localizedStandardContains(search) ||
                                group.turns.contains { $0.originalText.localizedStandardContains(search) || $0.correctedText.localizedStandardContains(search) }
                        }
                }.sorted {
                    let order = $0.display_name.localizedStandardCompare($1.display_name)
                    return order == .orderedSame ? $0.id < $1.id : order == .orderedAscending
                }
                if people.isEmpty { empty }
                ForEach(people) { person in
                    NavigationLink {
                        PhoneMemoryPersonScreen(model: model, personID: person.id)
                    } label: {
                        HStack(spacing: 14) {
                            PhoneMemoryPortrait(person: person)
                            VStack(alignment: .leading, spacing: 4) {
                                Text(person.display_name).font(.headline).foregroundStyle(Theme.textPrimary)
                                if let last = catalogue.personConversations[person.id]?.first {
                                    Text(last.conversation.meeting.date.formatted(date: .abbreviated, time: .shortened))
                                        .font(.footnote).foregroundStyle(Theme.textSecondary)
                                }
                            }
                        }
                        .frame(minHeight: 44)
                        .padding(.vertical, 4)
                    }
                }
            case .notes:
                let notes = catalogue.sortedNotes.filter {
                    search.isEmpty || $0.title.localizedStandardContains(search) || $0.body.localizedStandardContains(search) ||
                        $0.sources.contains { $0.title.localizedStandardContains(search) }
                }
                if notes.isEmpty { empty }
                ForEach(notes) { note in
                    NavigationLink {
                        PhoneMemoryNoteScreen(model: model, noteID: note.id)
                    } label: {
                        PhoneMemoryNoteRow(note: note)
                    }
                }
            case .bookmarks:
                let bookmarks = catalogue.bookmarks.filter {
                    search.isEmpty || ($0.bookmark.label?.localizedStandardContains(search) ?? false) ||
                        ($0.conversation?.meeting.session.title.localizedStandardContains(search) ?? false) ||
                        ($0.turn?.originalText.localizedStandardContains(search) ?? false) ||
                        ($0.turn?.correctedText.localizedStandardContains(search) ?? false)
                }
                if bookmarks.isEmpty { empty }
                ForEach(bookmarks) { bookmark in
                    if let conversation = bookmark.conversation {
                        NavigationLink {
                            PhoneConversationScreen(meeting: conversation.meeting,
                                initialOffsetNanoseconds: bookmark.turn?.segment.start_offset_ns ?? bookmark.bookmark.start_offset_ns)
                        } label: {
                            PhoneMemoryBookmarkRow(entry: bookmark)
                        }
                    } else {
                        PhoneMemoryBookmarkRow(entry: bookmark)
                    }
                }
            }
        } footer: {
            if selection == .bookmarks && !catalogue.bookmarks.isEmpty { PhoneMemoryNoAudio() }
        }
        .listRowBackground(Theme.surface)
    }

    private var empty: some View {
        let text: String
        if !search.isEmpty {
            text = String(localized: "memory.noMatches", defaultValue: "No matches.")
        } else {
            switch selection {
            case .people: text = String(localized: "memory.noPeople", defaultValue: "No saved people.")
            case .notes: text = String(localized: "conversation.noSavedNotes", defaultValue: "No saved notes.")
            case .bookmarks: text = String(localized: "memory.noBookmarks", defaultValue: "No saved bookmarks.")
            }
        }
        return Text(text).foregroundStyle(Theme.textSecondary)
    }
}

private struct PhoneMemoryPersonScreen: View {
    @ObservedObject var model: AppModel
    let personID: String
    @State private var showOriginal = false

    var body: some View {
        Group {
            if let index = model.board?.memoryIndex,
               let person = index.people.first(where: { $0.id == personID }) {
                let catalogue = PhoneMemoryCatalogue(index: index, meetings: model.board?.meetings ?? [])
                let groups = catalogue.personConversations[personID] ?? []
                let notes = catalogue.notes(for: personID)
                List {
                    Section {
                        HStack(spacing: 16) {
                            PhoneMemoryPortrait(person: person, size: 72)
                            if let latest = groups.first {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(String(localized: "memory.lastConversation", defaultValue: "Last conversation"))
                                        .font(.subheadline).foregroundStyle(Theme.textSecondary)
                                    Text(latest.conversation.meeting.date.formatted(date: .abbreviated, time: .shortened))
                                        .font(.body.weight(.medium)).foregroundStyle(Theme.textPrimary)
                                }
                            } else {
                                Text(String(localized: "memory.noAttributedDialogue", defaultValue: "No attributed dialogue synced."))
                                    .foregroundStyle(Theme.textSecondary)
                            }
                        }
                        .padding(.vertical, 8)
                    }
                    .listRowBackground(Color.clear)
                    if !notes.isEmpty {
                        Section(String(localized: "memory.relatedNotes", defaultValue: "Related notes")) {
                            ForEach(notes) { note in
                                NavigationLink {
                                    PhoneMemoryNoteScreen(model: model, noteID: note.id)
                                } label: { PhoneMemoryNoteRow(note: note) }
                            }
                        }
                        .listRowBackground(Theme.surface)
                    }
                    if !groups.isEmpty {
                        Section {
                            PhoneMemoryWordingPicker(showOriginal: $showOriginal)
                                .listRowInsets(EdgeInsets()).listRowBackground(Color.clear)
                        }
                        ForEach(groups) { group in
                            Section {
                                ForEach(group.turns) { turn in
                                    NavigationLink {
                                        PhoneConversationScreen(meeting: group.conversation.meeting,
                                            initialOffsetNanoseconds: turn.segment.start_offset_ns)
                                    } label: {
                                        PhoneMemoryTurnRow(meeting: group.conversation.meeting, turn: turn,
                                                           showOriginal: showOriginal, confirmedPersonName: person.display_name)
                                    }
                                }
                            } header: {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(group.conversation.meeting.session.title).font(.subheadline.weight(.semibold))
                                    Text(group.conversation.meeting.date.formatted(date: .abbreviated, time: .shortened))
                                        .font(.footnote)
                                }
                                .textCase(nil).foregroundStyle(Theme.textSecondary)
                            }
                            .listRowBackground(Theme.surface)
                        }
                        Section { PhoneMemoryNoAudio() }.listRowBackground(Color.clear)
                    }
                }
                .phoneMemoryList()
                .navigationTitle(person.display_name)
            } else {
                PhoneMemoryUnavailable()
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
    }
}

private struct PhoneMemoryNoteScreen: View {
    @ObservedObject var model: AppModel
    let noteID: String
    @State private var showOriginal = false

    var body: some View {
        Group {
            if let index = model.board?.memoryIndex, let note = index.notes.first(where: { $0.id == noteID }) {
                let catalogue = PhoneMemoryCatalogue(index: index, meetings: model.board?.meetings ?? [])
                let dialogue = catalogue.relatedDialogue(to: note)
                List {
                    Section {
                        Text(note.updatedAt.formatted(date: .abbreviated, time: .shortened))
                            .font(.footnote).foregroundStyle(Theme.textSecondary)
                        if !note.body.isEmpty {
                            Text(note.body).font(.body).foregroundStyle(Theme.textPrimary).textSelection(.enabled)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    .listRowBackground(Theme.surface)
                    if !note.sources.isEmpty {
                        Section(String(localized: "memory.sources", defaultValue: "Sources")) {
                            ForEach(Array(note.sources.enumerated()), id: \.offset) { _, source in
                                PhoneMemorySourceRow(model: model, source: source, catalogue: catalogue)
                            }
                        }
                        .listRowBackground(Theme.surface)
                    }
                    if !dialogue.isEmpty {
                        Section(String(localized: "memory.relatedDialogue", defaultValue: "Related dialogue")) {
                            PhoneMemoryWordingPicker(showOriginal: $showOriginal)
                            ForEach(dialogue) { entry in
                                NavigationLink {
                                    PhoneConversationScreen(meeting: entry.meeting,
                                                            initialOffsetNanoseconds: entry.turn.segment.start_offset_ns)
                                } label: {
                                    PhoneMemoryTurnRow(meeting: entry.meeting, turn: entry.turn,
                                                       showOriginal: showOriginal, showsMeeting: true)
                                }
                            }
                        }
                        .listRowBackground(Theme.surface)
                        Section { PhoneMemoryNoAudio() }.listRowBackground(Color.clear)
                    }
                }
                .phoneMemoryList()
                .navigationTitle(note.title.isEmpty ? String(localized: "memory.untitledNote", defaultValue: "Untitled note") : note.title)
                .toolbar {
                    ShareLink(item: [note.title, note.body].filter { !$0.isEmpty }.joined(separator: "\n\n")) {
                        Label("library.share", systemImage: "square.and.arrow.up")
                    }
                }
            } else {
                PhoneMemoryUnavailable()
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
    }
}

private struct PhoneMemorySourceRow: View {
    @ObservedObject var model: AppModel
    let source: PortableMemoryIndex.Source
    let catalogue: PhoneMemoryCatalogue

    var body: some View {
        Group {
            if !source.available {
                label(unavailable: true)
            } else if source.kind == "meeting", let conversation = catalogue.conversations[source.id] {
                NavigationLink { PhoneConversationScreen(meeting: conversation.meeting) } label: { label() }
            } else if source.kind == "person", catalogue.people[source.id] != nil {
                NavigationLink { PhoneMemoryPersonScreen(model: model, personID: source.id) } label: { label() }
            } else if source.kind == "note", catalogue.notesByID[source.id] != nil {
                NavigationLink { PhoneMemoryNoteScreen(model: model, noteID: source.id) } label: { label() }
            } else if ["meeting", "person", "note"].contains(source.kind) {
                label(notSynced: true)
            } else {
                label(unavailable: true)
            }
        }
        .frame(minHeight: 44)
    }

    private func label(unavailable: Bool = false, notSynced: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(source.title.isEmpty ? String(localized: "conversation.source", defaultValue: "Source") : source.title)
                .font(.body).foregroundStyle(Theme.textPrimary)
            if unavailable {
                Text(String(localized: "conversation.sourceUnavailable", defaultValue: "Source unavailable in this copy."))
                    .font(.footnote).foregroundStyle(Theme.textSecondary)
            } else if notSynced {
                Text(String(localized: "memory.sourceNotSynced", defaultValue: "Source not synced to this iPhone."))
                    .font(.footnote).foregroundStyle(Theme.textSecondary)
            }
        }
        .padding(.vertical, 4)
    }
}

private struct PhoneMemoryNoteRow: View {
    let note: PortableMemoryIndex.Note

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(note.title.isEmpty ? String(localized: "memory.untitledNote", defaultValue: "Untitled note") : note.title)
                .font(.headline).foregroundStyle(Theme.textPrimary).lineLimit(2)
            if !note.body.isEmpty {
                Text(note.body).font(.body).foregroundStyle(Theme.textSecondary).lineLimit(3)
            }
            Text(note.updatedAt.formatted(date: .abbreviated, time: .shortened))
                .font(.footnote).foregroundStyle(Theme.textSecondary)
        }
        .padding(.vertical, 6)
        .frame(minHeight: 44, alignment: .leading)
    }
}

private struct PhoneMemoryTurnRow: View {
    let meeting: PhoneMeeting
    let turn: PhoneMeeting.Turn
    let showOriginal: Bool
    var confirmedPersonName: String? = nil
    var showsMeeting = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if showsMeeting {
                Text(meeting.session.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.textPrimary)
                Text(meeting.date.formatted(date: .abbreviated, time: .shortened))
                    .font(.footnote).foregroundStyle(Theme.textSecondary)
            }
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .firstTextBaseline) {
                    speaker
                    Spacer(minLength: 12)
                    timestamp
                }
                VStack(alignment: .leading, spacing: 4) { speaker; timestamp }
            }
            if turn.isRemoved && !showOriginal {
                Text(String(localized: "conversation.removedInReview", defaultValue: "Removed in review"))
                    .font(.body).foregroundStyle(Theme.textSecondary)
            } else {
                Text(showOriginal ? turn.originalText : turn.correctedText)
                    .font(.body).foregroundStyle(Theme.textPrimary).lineLimit(4)
            }
            if turn.isEdited {
                Text(showOriginal
                     ? String(localized: "memory.originalWords", defaultValue: "Original words")
                     : String(localized: "memory.reviewedWords", defaultValue: "Reviewed words"))
                    .font(.caption).foregroundStyle(Theme.textSecondary)
            }
        }
        .padding(.vertical, 8)
        .frame(minHeight: 44, alignment: .leading)
    }

    private var speaker: some View {
        Text(confirmedPersonName ?? turn.speakerName)
            .font(.subheadline.weight(.medium)).foregroundStyle(Theme.textPrimary)
    }
    private var timestamp: some View {
        Text(turn.timestamp).font(.footnote.monospacedDigit()).foregroundStyle(Theme.textSecondary).fixedSize()
    }
}

private struct PhoneMemoryBookmarkRow: View {
    let entry: PhoneMemoryCatalogue.BookmarkEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(entry.bookmark.label.flatMap { $0.isEmpty ? nil : $0 } ??
                 String(localized: "memory.bookmark", defaultValue: "Bookmark"))
                .font(.headline).foregroundStyle(Theme.textPrimary)
            if let meeting = entry.conversation?.meeting {
                Text(meeting.session.title).font(.subheadline).foregroundStyle(Theme.textPrimary)
                Text(meeting.date.formatted(date: .abbreviated, time: .shortened))
                    .font(.footnote).foregroundStyle(Theme.textSecondary)
                Text(PhoneMeeting.timestamp(entry.bookmark.start_offset_ns))
                    .font(.footnote.monospacedDigit()).foregroundStyle(Theme.textSecondary)
                if let turn = entry.turn {
                    if turn.segment.start_offset_ns != entry.bookmark.start_offset_ns {
                        Text(String(localized: "memory.nearestTurn", defaultValue: "Nearest turn at \(turn.timestamp)"))
                            .font(.footnote).foregroundStyle(Theme.textSecondary)
                    }
                    Text(turn.isRemoved
                         ? String(localized: "conversation.removedInReview", defaultValue: "Removed in review")
                         : turn.correctedText)
                        .font(.body).foregroundStyle(Theme.textSecondary).lineLimit(3)
                } else if meeting.transcript_purged_at_utc_ms != nil {
                    Text(String(localized: "conversation.transcriptRemoved", defaultValue: "Transcript deleted"))
                        .font(.footnote).foregroundStyle(Theme.textSecondary)
                } else {
                    Text("meeting.noTranscript").font(.footnote).foregroundStyle(Theme.textSecondary)
                }
            } else {
                Text(PhoneMeeting.timestamp(entry.bookmark.start_offset_ns))
                    .font(.footnote.monospacedDigit()).foregroundStyle(Theme.textSecondary)
                Text(String(localized: "memory.conversationNotSynced", defaultValue: "Conversation not synced to this iPhone."))
                    .font(.footnote).foregroundStyle(Theme.textSecondary)
            }
        }
        .padding(.vertical, 6)
        .frame(minHeight: 44, alignment: .leading)
    }
}

private struct PhoneMemoryWordingPicker: View {
    @Binding var showOriginal: Bool
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    var body: some View {
        Group {
            if dynamicTypeSize.isAccessibilitySize {
                picker.pickerStyle(.menu)
            } else {
                picker.pickerStyle(.segmented)
            }
        }
        .frame(minHeight: 44)
    }

    private var picker: some View {
        Picker(String(localized: "conversation.wording", defaultValue: "Wording"), selection: $showOriginal) {
            Text(String(localized: "conversation.corrected", defaultValue: "Corrected")).tag(false)
            Text(String(localized: "conversation.original", defaultValue: "Original")).tag(true)
        }
    }
}

private struct PhoneMemoryPortrait: View {
    let person: PortableMemoryIndex.Person
    var size: CGFloat = 48
    @State private var image: UIImage?

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image).resizable().scaledToFill()
            } else {
                Text(person.display_name.split(whereSeparator: { $0.isWhitespace }).prefix(2)
                    .compactMap { $0.first.map(String.init) }.joined())
                    .font(size > 48 ? .title2.weight(.medium) : .body.weight(.medium))
                    .foregroundStyle(Theme.textSecondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(Theme.inset)
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .overlay(Circle().strokeBorder(Theme.border, lineWidth: 1))
        .accessibilityHidden(true)
        .task(id: person.photoPNGData) {
            guard let bytes = person.photoPNGData,
                  let source = CGImageSourceCreateWithData(bytes as CFData, nil),
                  let thumbnail = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                    kCGImageSourceCreateThumbnailFromImageAlways: true,
                    kCGImageSourceCreateThumbnailWithTransform: true,
                    kCGImageSourceThumbnailMaxPixelSize: 384,
                  ] as CFDictionary) else { image = nil; return }
            image = UIImage(cgImage: thumbnail)
        }
    }
}

private struct PhoneMemoryNoAudio: View {
    var body: some View {
        Text(String(localized: "conversation.noAudio", defaultValue: "Audio is not included in this copy."))
            .font(.footnote).foregroundStyle(Theme.textSecondary)
    }
}

private struct PhoneMemoryUnavailable: View {
    var body: some View {
        List {
            Text(String(localized: "conversation.sourceUnavailable", defaultValue: "Source unavailable in this copy."))
                .foregroundStyle(Theme.textSecondary).listRowBackground(Theme.surface)
        }
        .phoneMemoryList()
    }
}

private extension View {
    func phoneMemoryList() -> some View {
        listStyle(.insetGrouped)
            .listSectionSpacing(.custom(12))
            .scrollContentBackground(.hidden)
            .background(Theme.background)
            .tint(Theme.accent)
    }
}

private struct PhoneMemoryCatalogue {
    struct Conversation {
        let meeting: PhoneMeeting
        let turns: [PhoneMeeting.Turn]
    }
    struct PersonConversation: Identifiable {
        let conversation: Conversation
        let turns: [PhoneMeeting.Turn]
        var id: String { conversation.meeting.id }
    }
    struct DialogueEntry: Identifiable {
        struct ID: Hashable { let meetingID: String; let turnID: String }
        let meeting: PhoneMeeting
        let turn: PhoneMeeting.Turn
        var id: ID { ID(meetingID: meeting.id, turnID: turn.id) }
    }
    struct BookmarkEntry: Identifiable {
        let id: Int
        let bookmark: PortableMemoryIndex.Bookmark
        let conversation: Conversation?
        let turn: PhoneMeeting.Turn?
    }

    let index: PortableMemoryIndex
    let people: [String: PortableMemoryIndex.Person]
    let notesByID: [String: PortableMemoryIndex.Note]
    let sortedNotes: [PortableMemoryIndex.Note]
    let conversations: [String: Conversation]
    let personConversations: [String: [PersonConversation]]
    let bookmarks: [BookmarkEntry]

    init(index: PortableMemoryIndex, meetings: [PhoneMeeting]) {
        self.index = index
        people = Dictionary(index.people.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        notesByID = Dictionary(index.notes.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        sortedNotes = index.notes.sorted {
            $0.updated_at_ms == $1.updated_at_ms ? $0.id < $1.id : $0.updated_at_ms > $1.updated_at_ms
        }
        let meetingsByID = Dictionary(meetings.map { ($0.id, $0) }, uniquingKeysWith: { first, second in
            first.date >= second.date ? first : second
        })
        let conversations = meetingsByID.mapValues { Conversation(meeting: $0, turns: $0.turns) }
        self.conversations = conversations
        let attributions = Dictionary(grouping: index.attributions, by: \.meeting_id)
        var grouped: [String: [PersonConversation]] = [:]
        for conversation in conversations.values {
            let meeting = conversation.meeting
            let speakers = Dictionary(meeting.speakers.map { ($0.speaker_id, $0) }, uniquingKeysWith: { _, last in last })
            func resolvedSpeaker(_ id: String) -> String? {
                var current = id
                var seen: Set<String> = []
                while let next = speakers[current]?.merged_into_speaker_id {
                    guard seen.insert(current).inserted else { return nil }
                    current = next
                }
                return current
            }
            var confirmed: [String: Set<String>] = [:]
            for attribution in attributions[meeting.id] ?? [] {
                if let speaker = resolvedSpeaker(attribution.speaker_id) {
                    confirmed[speaker, default: []].insert(attribution.person_id)
                }
            }
            let assignments = Dictionary(meeting.diarization_assignments.filter {
                $0.generation_id == meeting.session.current_diarization_generation_id
            }.map { ($0.segment_id, $0) }, uniquingKeysWith: { _, last in last })
            var turnsByPerson: [String: [PhoneMeeting.Turn]] = [:]
            for turn in conversation.turns {
                let assignment = assignments[turn.id]
                guard assignment?.assignment_kind != "unknown",
                      assignment?.assignment_kind != "overlap" else { continue }
                let speakerID: String
                if meeting.session.current_diarization_generation_id != nil {
                    guard let assignment else { continue }
                    speakerID = assignment.speaker_id
                } else {
                    speakerID = assignment?.speaker_id ?? turn.segment.speaker_id
                }
                guard let speaker = resolvedSpeaker(speakerID),
                      let identities = confirmed[speaker], identities.count == 1,
                      let person = identities.first else { continue }
                turnsByPerson[person, default: []].append(turn)
            }
            for (person, turns) in turnsByPerson {
                grouped[person, default: []].append(PersonConversation(conversation: conversation, turns: turns))
            }
        }
        personConversations = grouped.mapValues { groups in
            groups.sorted {
                let first = $0.conversation.meeting
                let second = $1.conversation.meeting
                return first.date == second.date ? first.id < second.id : first.date > second.date
            }
        }
        bookmarks = index.bookmarks.enumerated().map { position, bookmark in
            let conversation = conversations[bookmark.meeting_id]
            let nearest = conversation?.turns.min {
                let first = Self.distance($0.segment.start_offset_ns, bookmark.start_offset_ns)
                let second = Self.distance($1.segment.start_offset_ns, bookmark.start_offset_ns)
                return first == second ? $0.segment.start_offset_ns < $1.segment.start_offset_ns : first < second
            }
            return BookmarkEntry(id: position, bookmark: bookmark, conversation: conversation, turn: nearest)
        }.sorted {
            let firstDate = $0.conversation?.meeting.date ?? .distantPast
            let secondDate = $1.conversation?.meeting.date ?? .distantPast
            if firstDate != secondDate { return firstDate > secondDate }
            if $0.bookmark.meeting_id != $1.bookmark.meeting_id { return $0.bookmark.meeting_id < $1.bookmark.meeting_id }
            if $0.bookmark.start_offset_ns != $1.bookmark.start_offset_ns { return $0.bookmark.start_offset_ns < $1.bookmark.start_offset_ns }
            return $0.id < $1.id
        }
    }

    func notes(for personID: String) -> [PortableMemoryIndex.Note] {
        let meetingIDs = Set(index.attributions.filter { $0.person_id == personID }.map(\.meeting_id))
        return sortedNotes.filter { note in
            note.sources.contains { source in
                source.available && ((source.kind == "person" && source.id == personID) ||
                                     (source.kind == "meeting" && meetingIDs.contains(source.id)))
            }
        }
    }

    func relatedDialogue(to note: PortableMemoryIndex.Note) -> [DialogueEntry] {
        var entries: [DialogueEntry.ID: DialogueEntry] = [:]
        for source in note.sources where source.available {
            let groups: [PersonConversation]
            if source.kind == "meeting", let conversation = conversations[source.id] {
                groups = [PersonConversation(conversation: conversation, turns: conversation.turns)]
            } else if source.kind == "person" {
                groups = personConversations[source.id] ?? []
            } else {
                continue
            }
            for group in groups {
                for turn in group.turns.suffix(6) {
                    let entry = DialogueEntry(meeting: group.conversation.meeting, turn: turn)
                    entries[entry.id] = entry
                }
            }
        }
        return Array(entries.values.sorted {
            if $0.meeting.date != $1.meeting.date { return $0.meeting.date > $1.meeting.date }
            if $0.meeting.id != $1.meeting.id { return $0.meeting.id < $1.meeting.id }
            if $0.turn.segment.start_offset_ns != $1.turn.segment.start_offset_ns {
                return $0.turn.segment.start_offset_ns > $1.turn.segment.start_offset_ns
            }
            return $0.turn.id < $1.turn.id
        }.prefix(6))
    }

    private static func distance(_ first: UInt64, _ second: UInt64) -> UInt64 {
        first >= second ? first - second : second - first
    }
}
