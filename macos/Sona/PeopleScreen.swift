import AppKit
import SwiftUI

/// People, one person, one organization: three screens behind one rail entry.
///
/// Which one is up is the store's route, so a merge that lands on another
/// person and a delete that lands back on the list are the same kind of move
/// as a row press.
struct PeopleScreen: View {
    let store: PeopleStore
    /// Opens the meeting a line was said in. Every ledger row is a way into one.
    var openMeeting: (String) -> Void = { _ in }
    var openContribution: ((String, UInt64) -> Void)?
    /// Adds context about this person. The import surface owns the file
    /// picker and the command behind it; a person's page owns the verb.
    var ingestDocument: () -> Void = {}
    /// Deletes an imported document, when the import surface is there to do
    /// it. Absent, the catalogue is a catalogue and nothing on it destroys.
    var deleteDocument: ((String) -> Void)?
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        Group {
            switch store.route {
            case .list:
                Page {
                    PeopleListView(store: store, openMeeting: openMeeting)
                }
            case .person:
                person
            case .organization:
                OrganizationView(store: store, openMeeting: openMeeting)
            }
        }
        .onChange(of: store.route, initial: true) { model.workFocus.open(nil, in: .people) }
    }

    /// A person's page, and over it a task opened from their list. The page
    /// stays underneath, so Esc lands on the tab and the place it left.
    private var person: some View {
        let focus = model.workFocus
        let opened = focus.opened(in: .people)
        return ZStack(alignment: .bottomLeading) {
            PersonView(
                store: store,
                openMeeting: openMeeting,
                openContribution: openContribution,
                ingestDocument: ingestDocument,
                deleteDocument: deleteDocument)
            .opacity(opened == nil ? 1 : 0)
            .allowsHitTesting(opened == nil)
            .accessibilityHidden(opened != nil)
            if let opened {
                TaskDetailView(id: opened, place: .people)
            }
            UndoToast()
        }
        .overlay(alignment: .trailing) {
            if focus.peeking, opened == nil, let id = focus.highlighted, let row = model.workRows([id]).first {
                TaskPeek(row: row, place: .people)
                    .transition(reduceMotion ? .opacity : .move(edge: .trailing).combined(with: .opacity))
            }
        }
        .overlay(alignment: .bottom) {
            if focus.selecting, opened == nil {
                BulkActionBar(place: .people)
                    .padding(.bottom, 20)
                    .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(Motion.regular, value: focus.peeking)
        .animation(Motion.regular, value: focus.selecting)
        .modifier(WorkSheets())
    }
}

// MARK: - The list

/// Everybody Sona knows: the numbers first, then what still needs you, the
/// people, and the companies they are at.
struct PeopleListView: View {
    let store: PeopleStore
    var openMeeting: (String) -> Void = { _ in }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            PageTitle("People", subtitle: subtitle)
            ErrorNote(store.error)
            if store.listFailed {
                Card {
                    CardRow {
                        Text("Couldn't load people.").bodyText(14, Theme.inkSecondary)
                    } trailing: {
                        Button("Retry") { store.reload() }.buttonStyle(.compact)
                    }
                }
                .padding(.bottom, 28)
            } else if store.entries?.isEmpty == true {
                Card { CardLine("Name a speaker in a meeting, or turn on your calendar, and they appear here.") }
                    .padding(.bottom, 28)
            } else {
                PeopleListStats(entries: store.entries, companies: store.companies)
                    .padding(.bottom, 28)
                PeopleInboxSection(store: store, openMeeting: openMeeting)
                PageSection("Everyone") {
                    if let entries = store.entries {
                        Card {
                            LazyVStack(spacing: 0) {
                                ForEach(entries) { entry in
                                    PersonListRow(entry: entry, photoRevision: store.photoRevision) {
                                        store.openPerson(entry.person.id)
                                    }
                                }
                            }
                        }
                    } else {
                        PeopleRowsPlaceholder(rows: 5)
                    }
                }
                PeopleCompaniesSection(store: store)
            }
        }
    }

    /// The tiles under the title count the people, so the line under it only
    /// speaks while there is nothing to count.
    private var subtitle: String {
        guard let entries = store.entries else { return "Reading the people Sona knows" }
        return entries.isEmpty ? "The people you meet, what you talked about, and what you owe each other" : ""
    }
}

/// The people in four numbers. Until the list answers, each tile holds its
/// place with nothing in it.
struct PeopleListStats: View {
    let entries: [PersonListEntry]?
    let companies: [CompanySummary]?

    var body: some View {
        HStack(spacing: 10) {
            NumberTile(entries.map { "\($0.count)" } ?? " ", label: "People")
            NumberTile(companies.map { "\($0.count)" } ?? " ", label: "Companies")
            NumberTile(entries.map { "\(Self.metThisWeek($0))" } ?? " ", label: "Met this week")
            NumberTile(entries.map { "\($0.reduce(0) { $0 + $1.suggestedCount })" } ?? " ", label: "To confirm")
        }
    }

    private static func metThisWeek(_ entries: [PersonListEntry], now: Date = .now) -> Int {
        let start = now.addingTimeInterval(-7 * 86_400)
        return entries.filter { entry in
            let last = entry.lastMeeting?.at ?? entry.lastMeetingAtUtcMs.map(PeopleFormat.date)
            return last.map { $0 >= start } ?? false
        }.count
    }
}

/// The companies the people are at, newest meeting first, each with what its
/// page holds, counted. A company is the people whose calendar addresses
/// share a work domain, so a row opens the same page a person's header does.
struct PeopleCompaniesSection: View {
    let store: PeopleStore

    var body: some View {
        PageSection("Companies") {
            if store.companiesFailed {
                Card {
                    CardRow {
                        Text("Couldn't load companies.").bodyText(14, Theme.inkSecondary)
                    } trailing: {
                        Button("Retry") { store.reload() }.buttonStyle(.compact)
                    }
                }
            } else if let companies = store.companies {
                Card {
                    if companies.isEmpty {
                        CardLine("Companies appear once people from a work address join your meetings.")
                    } else {
                        LazyVStack(spacing: 0) {
                            ForEach(companies) { company in
                                CompanyRow(company: company) { store.openOrganization(company.slug) }
                            }
                        }
                    }
                }
            } else {
                PeopleRowsPlaceholder(rows: 2)
            }
        }
    }
}

/// One company, one row, read like a person's: its mark, the name and how
/// long ago you last met anybody there, and its people and meetings counted
/// on the right.
struct CompanyRow: View {
    let company: CompanySummary
    let open: () -> Void

    var body: some View {
        CardRow(action: open) {
            HStack(spacing: 12) {
                CompanyMark(name: company.name, size: 36)
                VStack(alignment: .leading, spacing: 3) {
                    Text(company.name).headlineText().lineLimit(1)
                    Text(lastMet).metaText().monospacedDigit().lineLimit(1)
                }
            }
        } trailing: {
            HStack(alignment: .center, spacing: 16) {
                if company.openLoopsCount > 0 {
                    Chip(PeopleModel.openItems(company.openLoopsCount))
                }
                PeopleCount(value: company.peopleCount, unit: company.peopleCount == 1 ? "person" : "people")
                PeopleCount(value: company.meetingsCount, unit: company.meetingsCount == 1 ? "meeting" : "meetings")
                PeopleChevron()
            }
        }
    }

    private var lastMet: String {
        company.lastMeetingAtUtcMs.map { "Last met \(PeopleFormat.elapsed(PeopleFormat.date($0)))" }
            ?? "No meetings yet"
    }
}

/// One person, one row: the face, the name, where they are and how long ago
/// you last met, and the meetings you have had, counted on the right. The
/// exact date is on the person's own page, which is what the row opens.
struct PersonListRow: View {
    let entry: PersonListEntry
    var photoRevision: UInt64 = 0
    let open: () -> Void

    var body: some View {
        CardRow(action: open) {
            HStack(spacing: 12) {
                PersonPortrait(personId: entry.person.id, name: entry.person.displayName, size: 36, revision: photoRevision)
                VStack(alignment: .leading, spacing: 3) {
                    Text(entry.person.displayName).headlineText().lineLimit(1)
                    if !facts.isEmpty {
                        Text(facts).metaText().monospacedDigit().lineLimit(1)
                    }
                }
            }
        } trailing: {
            HStack(alignment: .center, spacing: 16) {
                if entry.suggestedCount > 0 {
                    Chip("\(entry.suggestedCount) to confirm")
                }
                PeopleCount(value: entry.confirmedCount, unit: entry.confirmedCount == 1 ? "meeting" : "meetings")
                PeopleChevron()
            }
        }
    }

    /// The place, then how long ago, joined as a sentence about a person
    /// rather than cells of a table.
    private var facts: String {
        var parts: [String] = []
        if let organization = entry.person.organization {
            parts.append(organization)
        }
        if let last = entry.lastMeeting {
            parts.append("Last met \(PeopleFormat.elapsed(last.at))")
        } else if let last = entry.lastMeetingAtUtcMs {
            parts.append("Last met \(PeopleFormat.elapsed(PeopleFormat.date(last)))")
        }
        return parts.joined(separator: " · ")
    }
}

/// A count at the end of a row, the number over its noun.
struct PeopleCount: View {
    let value: UInt64
    let unit: String

    var body: some View {
        VStack(alignment: .trailing, spacing: 0) {
            Text("\(value)").font(TypeScale.stat).foregroundStyle(Theme.ink).monospacedDigit()
            Text(unit).metaText()
        }
        .frame(minWidth: 52, alignment: .trailing)
        .accessibilityElement(children: .combine)
    }
}

private struct PeopleChevron: View {
    var body: some View {
        Image(systemName: "chevron.right")
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(Theme.inkTertiary)
            .accessibilityHidden(true)
    }
}

/// A company's mark: its first letter on a rounded tile, the square twin of
/// a person's initials.
struct CompanyMark: View {
    let name: String
    let size: CGFloat

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: size * 0.24, style: .continuous)
        Text(name.first.map { String($0).uppercased() } ?? "")
            .font(.system(size: size * 0.4, weight: .semibold))
            .foregroundStyle(Theme.inkSecondary)
            .frame(width: size, height: size)
            .background(Theme.inset, in: shape)
            .overlay(shape.strokeBorder(Theme.border, lineWidth: 1))
            .accessibilityHidden(true)
    }
}

/// Where the rows will be, while they are read: the same height, with a
/// circle where each face goes.
struct PeopleRowsPlaceholder: View {
    var rows = 5

    var body: some View {
        Card {
            ForEach(0..<rows, id: \.self) { index in
                HStack(spacing: 12) {
                    Circle().fill(Theme.inset).frame(width: 36, height: 36)
                    VStack(alignment: .leading, spacing: 7) {
                        PlaceholderBar(width: index.isMultiple(of: 2) ? 150 : 120, height: 10)
                        PlaceholderBar(width: 200, height: 8)
                    }
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .overlay(alignment: .bottom) { Hairline().padding(.horizontal, 16) }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Loading")
    }
}

/// What is still open across everybody, newest first. A promise whose meeting
/// is gone still has to be read; it just has nothing to open.
struct PeopleInboxSection: View {
    let store: PeopleStore
    var openMeeting: (String) -> Void = { _ in }

    var body: some View {
        if !store.inbox.isEmpty {
            PageSection("Needs you") {
                Card {
                    ForEach(store.inbox) { loop in
                        PersonLoopRow(loop: loop, openMeeting: openMeeting)
                    }
                }
            }
        }
    }
}

struct PersonLoopRow: View {
    let loop: PersonOpenLoop
    var openMeeting: (String) -> Void = { _ in }
    @Environment(AppModel.self) private var model

    var body: some View {
        CardRow(action: loop.meetingId.isEmpty ? nil : { openMeeting(loop.meetingId) }) {
            VStack(alignment: .leading, spacing: 4) {
                Text(loop.text).bodyText(14)
                HStack(spacing: 12) {
                    Text(loop.title).metaText().lineLimit(1)
                    Text(PeopleFormat.elapsed(loop.at)).metaText().monospacedDigit()
                    if loop.waitingOnStale {
                        Text("Overdue").metaText(Theme.live)
                    }
                }
                if let carried {
                    Text(carried).metaText(Theme.inkSecondary)
                }
            }
        } trailing: {
            if carried != nil {
                receipts
            }
        }
    }

    private var carried: String? {
        switch loop.raisedInMeetings {
        case ..<2: nil
        case 2: "Carried over, asked twice"
        case let count: "Carried over, asked \(count) times"
        }
    }

    private var receiptsKey: String { "loop:\(loop.loopId)" }

    @ViewBuilder
    private var receipts: some View {
        let audio = model.meetings.audio
        if audio.reelKey == receiptsKey {
            Button("Stop") { audio.stop() }
                .buttonStyle(QuietButton(color: Theme.accent))
        } else {
            Button {
                audio.playReel(reading: "conversation_reel_for_loop", ["loopId": .string(loop.loopId)], key: receiptsKey)
            } label: {
                Image(systemName: "play.fill")
            }
            .buttonStyle(QuietButton(color: Theme.accent, compact: true))
            .disabled(audio.heldForRecording)
            .help(audio.heldForRecording ? ConversationAudioPlayback.heldNotice : "")
            .accessibilityLabel("Play each time it came up")
        }
    }
}

// MARK: - One person

/// A person's page: a pinned bar with the way back and the verbs, the face
/// and the name the way Contacts heads a card, the person in numbers, the
/// contact, and the tabs.
///
/// Every verb is in the one menu, and attaching a contact also has a button
/// in the head while none is attached. Rename opens the title as a field, which
/// commits on Enter or on leaving it and reverts on Escape; there is no Save,
/// because a rename is one value with a receipt behind it. The two verbs that
/// write a section — the relationship paragraph, and an imported document —
/// are in the menu rather than in those sections, because a section with
/// nothing in it is not drawn and a verb that disappears with its own empty
/// state can never be pressed. Splitting, merging, forgetting a voice and
/// deleting change who this person is, so they sit under a separator and every
/// irreversible one keeps its question.
struct PersonView: View {
    let store: PeopleStore
    var openMeeting: (String) -> Void = { _ in }
    var openContribution: ((String, UInt64) -> Void)?
    var ingestDocument: () -> Void = {}
    var deleteDocument: ((String) -> Void)?

    /// Which question is open in front of the page.
    enum Prompt: Identifiable {
        case merge
        case delete
        case voiceProfile
        case split
        case link
        case unlink(PersonMeetingLink)
        case document(PersonDocumentSummary)

        var id: String {
            switch self {
            case .merge: "merge"
            case .delete: "delete"
            case .voiceProfile: "voice"
            case .split: "split"
            case .link: "link"
            case let .unlink(link): "unlink:\(link.id)"
            case let .document(document): "document:\(document.id)"
            }
        }
    }

    @State private var prompt: Prompt?
    @State private var renaming = false

    var body: some View {
        Page(back: PageBack("People") { store.toList() }, title: header?.displayName) {
            VStack(alignment: .leading, spacing: 0) {
                if let person = header {
                    PersonHero(
                        store: store,
                        person: person,
                        contact: store.detail?.contact,
                        renaming: $renaming,
                        chooseContact: store.detail == nil ? nil : { store.chooseContact() })
                } else if !store.detailFailed {
                    PersonHeroPlaceholder()
                }
                ErrorNote(store.error)
                if store.detail == nil, store.detailFailed {
                    Card {
                        CardRow {
                            Text("Couldn't load this person.").bodyText(14, Theme.inkSecondary)
                        } trailing: {
                            Button("Retry") { store.reload() }.buttonStyle(.compact)
                        }
                    }
                } else if case let .person(personId) = store.route {
                    PersonSections(
                        store: store,
                        personId: personId,
                        detail: store.detail,
                        prompt: $prompt,
                        openMeeting: openMeeting,
                        openContribution: openContribution,
                        deleteDocument: deleteDocument)
                    .id(personId)
                }
            }
        } actions: {
            if store.detail != nil {
                PersonMenu(store: store, renaming: $renaming, prompt: $prompt, ingestDocument: ingestDocument)
                    .background(PersonContactPickerAnchor(picker: store.contactPicker))
            }
        }
        .sheet(item: $prompt) { prompt in
            PersonPromptSheet(
                store: store,
                prompt: prompt,
                deleteDocument: deleteDocument,
                dismiss: { self.prompt = nil })
        }
        .alert("Which voice should Sona keep?", isPresented: voiceConflictPresented, presenting: store.voiceConflict) { conflict in
            Button("Keep \(store.detail?.person.displayName ?? "this person")'s voice") {
                store.resolveVoiceConflict(keepSource: true)
            }
            Button("Keep \(conflict.targetName)'s voice") {
                store.resolveVoiceConflict(keepSource: false)
            }
            Button("Cancel", role: .cancel) { store.dismissVoiceConflict() }
        } message: { conflict in
            Text("Both people have a saved voice, and the two were learned by different models, so they cannot be combined. The merged person keeps one; the other is forgotten, and \(conflict.targetName) can be enrolled again later.")
        }
    }

    /// The person as far as the page knows them: the detail once it is read,
    /// and the list's row until then, so the face and the name are up from
    /// the first frame.
    private var header: Person? {
        guard case let .person(personId) = store.route else { return nil }
        if let person = store.detail?.person, person.id == personId { return person }
        return store.entries?.first { $0.person.id == personId }?.person
    }

    private var voiceConflictPresented: Binding<Bool> {
        Binding(get: { store.voiceConflict != nil }, set: { if !$0 { store.dismissVoiceConflict() } })
    }
}

/// The round ⋯ button every verb on a person's page lives in.
struct PersonMenu: View {
    let store: PeopleStore
    @Binding var renaming: Bool
    @Binding var prompt: PersonView.Prompt?
    var ingestDocument: () -> Void = {}

    var body: some View {
        Menu {
            Button("Rename") { renaming = true }
            Button("Regenerate") { store.regenerateSummary() }
            Button("Import document", action: ingestDocument)
            Button("Choose photo") { store.choosePhotoSource() }
            Button("Remove photo") { store.removeAvatar() }
            Button(store.detail?.contact == nil ? "Attach contact…" : "Replace contact…") { store.chooseContact() }
            if store.detail?.contact != nil {
                Button("Remove contact") { store.removeContact() }
            }
            Divider()
            Button("Merge") { prompt = .merge }
                .disabled((store.entries?.count ?? 0) < 2)
            Button("Split person") { prompt = .split }
            Button("Link a meeting") {
                store.loadLinkCandidates()
                prompt = .link
            }
            Divider()
            Button("Remove voice profile") { prompt = .voiceProfile }
            Button("Delete person") { prompt = .delete }
        } label: {
            Image(systemName: "ellipsis")
        }
        .menuStyle(.button)
        .buttonStyle(.icon)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(store.busy)
        .help("Person options")
        .accessibilityLabel("Person options")
    }
}

/// The title as a field. One commit path: Enter and Escape both give up the
/// focus, Escape puts the saved name back first, so leaving the field is the
/// only thing that ever writes — and it writes only when the name changed.
struct PersonNameField: View {
    let name: String
    let commit: (String) -> Void
    let close: () -> Void
    @State private var draft: String = ""
    @FocusState private var focused: Bool

    var body: some View {
        TextField("Person name", text: $draft)
            .textFieldStyle(.plain)
            .font(TypeScale.hero)
            .foregroundStyle(Theme.ink)
            .focused($focused)
            .frame(maxWidth: 360)
            .padding(.horizontal, 10)
            .frame(height: 40)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
            .onAppear {
                draft = name
                focused = true
            }
            .onSubmit { focused = false }
            .onExitCommand {
                draft = name
                focused = false
            }
            .onChange(of: focused) { _, focused in
                guard !focused else { return }
                let trimmed = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                if !trimmed.isEmpty, trimmed != name {
                    commit(trimmed)
                }
                close()
            }
    }
}

/// Everything under the head of a person's page: the numbers, the contact,
/// and the tabs. It owns the two reads the numbers and the tabs share, so a
/// tab switch never reads again and the numbers never wait on a tab. A tile
/// opens its tab and brings the tabs into view. The tiles stand from the
/// first frame and fill in place as the reads land.
struct PersonSections: View {
    let store: PeopleStore
    let personId: String
    let detail: PersonDetail?
    @Binding var prompt: PersonView.Prompt?
    var openMeeting: (String) -> Void = { _ in }
    var openContribution: ((String, UInt64) -> Void)?
    var deleteDocument: ((String) -> Void)?
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var memory: PersonMemoryStore?
    @State private var work: PersonWorkStore?
    @State private var tab: PersonPageTab = .history

    private struct LoadKey: Equatable {
        let personId: String
        let revision: UInt64
    }

    private static let tabsAnchor = "person.tabs"

    var body: some View {
        ScrollViewReader { scroll in
            VStack(alignment: .leading, spacing: 0) {
                PersonStats(
                    figures: detail.map { PersonFigures(detail: $0, memory: memory, work: work) } ?? PersonFigures(),
                    show: { show($0, filter: $1, scroll: scroll) },
                    openMeeting: openMeeting)
                .equatable()
                .padding(.bottom, 28)
                if let detail {
                    if let contact = detail.contact {
                        PersonContactSection(card: contact, person: detail.person)
                    }
                    PersonFirstMetSection(personId: personId, revision: store.detailRevision, openMeeting: openMeeting)
                    PersonReelSection(personId: personId, revision: store.detailRevision)
                    PersonWatchSection(personId: personId, name: detail.person.displayName)
                    PersonContinuityView(
                        personId: personId,
                        memory: memory,
                        work: work,
                        openLoops: detail.openLoops,
                        plansWithoutRecord: detail.plansWithoutRecord,
                        tab: $tab,
                        openMeeting: openMeeting,
                        openContribution: openContribution
                    ) {
                        PersonSummaryView(summary: detail.person.summary, outcome: store.summaryOutcome)
                        TranscriptCorrectionsView(core: store.core, personId: personId) { sessionId, segmentId in
                            model.meetings.playContribution(sessionId: sessionId, segmentId: segmentId)
                        }
                        ConversationAudioBar(audio: model.meetings.audio)
                        PersonMeetingsView(store: store, links: detail.links, prompt: $prompt, openMeeting: openMeeting)
                        PersonDocumentsView(
                            documents: detail.documents,
                            busy: store.busy,
                            prompt: $prompt,
                            deletable: deleteDocument != nil)
                        PersonEvidenceView(person: detail.person, links: detail.links)
                    }
                    .id(Self.tabsAnchor)
                }
            }
        }
        .task(id: loadKey) {
            guard loadKey != nil else { return }
            let memory = self.memory ?? PersonMemoryStore(core: store.core)
            self.memory = memory
            await memory.load(personId)
        }
        .task(id: loadKey) {
            guard loadKey != nil else { return }
            let work = self.work ?? PersonWorkStore(core: store.core)
            self.work = work
            await work.load(personId)
        }
        .onDisappear {
            memory?.stop()
            work?.stop()
        }
    }

    /// Nil until the person is read: the tabs' reads wait for the page's.
    private var loadKey: LoadKey? {
        detail.map { _ in LoadKey(personId: personId, revision: store.detailRevision) }
    }

    private func show(_ next: PersonPageTab, filter: PersonHistoryFilter?, scroll: ScrollViewProxy) {
        if let filter { memory?.filter = filter }
        withAnimation(reduceMotion ? nil : .easeOut(duration: 0.2)) {
            tab = next
            scroll.scrollTo(Self.tabsAnchor, anchor: .top)
        }
    }
}

/// Three sentences about a relationship, and the two facts that make them
/// readable: a paragraph a model wrote is only readable if you know which
/// model and when. No paragraph, no section — the verb that asks for the first
/// one is in the page's menu — unless that verb was just pressed and wrote
/// nothing, in which case the section says why.
struct PersonSummaryView: View {
    let summary: PersonSummary?
    let outcome: PersonSummaryOutcome?

    var body: some View {
        if summary != nil || outcome?.note != nil {
            PageSection("About") {
                Card {
                    CardRow {
                        VStack(alignment: .leading, spacing: 8) {
                            if let summary {
                                Text(summary.text).bodyText(14)
                                Text("AI interpretation · \(summary.modelId) · \(PeopleFormat.moment(PeopleFormat.date(summary.generatedAtUtcMs)))")
                                    .metaText()
                                    .monospacedDigit()
                            }
                            if let note = outcome?.note {
                                Text(note).metaText(Theme.live)
                            }
                        }
                    }
                }
            }
        }
    }
}


/// Every meeting linked to this person, and the one word that asks the reader
/// for a decision. Where a link came from is the page's evidence section; what
/// stays on the row is what to do about it.
struct PersonMeetingsView: View {
    let store: PeopleStore
    let links: [PersonMeetingLink]
    @Binding var prompt: PersonView.Prompt?
    var openMeeting: (String) -> Void = { _ in }

    var body: some View {
        if !links.isEmpty {
            PageSection("Meetings together") {
                Card {
                    ForEach(links) { link in
                        CardRow(action: { openMeeting(link.meeting.id) }) {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(link.meeting.title).font(TypeScale.label()).foregroundStyle(Theme.ink).lineLimit(1)
                                if let headline = link.meeting.headline {
                                    Text(headline).metaText().lineLimit(1)
                                }
                                HStack(spacing: 12) {
                                    if link.isSuggested {
                                        Text("Suggested").metaText(Theme.accent)
                                    }
                                    if link.meeting.seriesNumber >= 2 {
                                        Text("Meeting \(link.meeting.seriesNumber) in series")
                                            .metaText()
                                            .monospacedDigit()
                                    }
                                    Text(PeopleFormat.moment(link.meeting.at)).metaText().monospacedDigit()
                                }
                            }
                        } trailing: {
                            HStack(spacing: 10) {
                                if link.isSuggested {
                                    Button("Confirm") { store.confirmLink(link.meeting.id) }
                                        .buttonStyle(.compact)
                                        .disabled(store.busy)
                                    Button("Dismiss") { store.removeLink(link.meeting.id) }
                                        .buttonStyle(.quiet)
                                        .disabled(store.busy)
                                } else {
                                    Button("Unlink") { prompt = .unlink(link) }
                                        .buttonStyle(.quiet)
                                        .disabled(store.busy)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}


/// The documents imported about this person. Nothing imported, nothing here:
/// the verb that imports the first one is in the page's menu, so this section
/// is only ever the catalogue of what came back.
struct PersonDocumentsView: View {
    let documents: [PersonDocumentSummary]
    let busy: Bool
    @Binding var prompt: PersonView.Prompt?
    let deletable: Bool

    var body: some View {
        if !documents.isEmpty {
            PageSection("Documents") {
                Card {
                    ForEach(documents) { document in
                        CardRow {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(document.title).font(TypeScale.label()).foregroundStyle(Theme.ink).lineLimit(1)
                                HStack(spacing: 12) {
                                    Text(document.sourceName).metaText().lineLimit(1)
                                    Text(PeopleFormat.moment(document.createdAt)).metaText().monospacedDigit()
                                }
                            }
                        } trailing: {
                            if deletable {
                                Button("Delete") { prompt = .document(document) }
                                    .buttonStyle(QuietButton(color: Theme.live))
                                    .disabled(busy)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// How Sona knows this is this person: what the links were made from, what
/// else they are called, and the addresses an invite reaches them at.
///
/// Three quiet lines rather than a table of one row per kind of evidence.
/// Nothing here is pressable, so nothing here is a chip. With none of the
/// three the section is not on the page: there is no honest empty state for
/// "why", only silence.
struct PersonEvidenceView: View {
    let person: Person
    let links: [PersonMeetingLink]

    var body: some View {
        let lines = self.lines
        if !lines.isEmpty {
            PageSection("Why Sona links this person") {
                Card {
                    CardRow {
                        VStack(alignment: .leading, spacing: 6) {
                            ForEach(lines, id: \.self) { line in
                                Text(line).metaText().monospacedDigit()
                            }
                        }
                    }
                }
            }
        }
    }

    private var lines: [String] {
        var lines: [String] = []
        let sources = PersonLinkSource.allCases.compactMap { source -> String? in
            let count = links.filter { $0.source == source }.count
            guard count > 0 else { return nil }
            return "\(source.label) \(PeopleModel.meetings(UInt64(count)))"
        }
        if !sources.isEmpty {
            lines.append(sources.joined(separator: " · "))
        }
        if !person.aliases.isEmpty {
            lines.append("Also: \(person.aliases.joined(separator: " · "))")
        }
        if !person.calendarEmails.isEmpty {
            lines.append("Invited as \(person.calendarEmails.joined(separator: " · "))")
        }
        return lines
    }
}

// MARK: - The questions a person's page asks

/// Every question the page asks, in one sheet. A reversible edit of a name
/// gets none of this; everything that changes who a person is gets one.
struct PersonPromptSheet: View {
    let store: PeopleStore
    let prompt: PersonView.Prompt
    var deleteDocument: ((String) -> Void)?
    let dismiss: () -> Void

    var body: some View {
        switch prompt {
        case .merge:
            PersonMergeSheet(store: store, dismiss: dismiss)
        case .split:
            PersonSplitSheet(store: store, dismiss: dismiss)
        case .link:
            LinkPickerSheet(store: store, dismiss: dismiss)
        case .delete:
            PeopleConfirmSheet(
                title: "Delete this person?",
                message: "Delete \(store.detail?.person.displayName ?? "this person") and remove all of their meeting links?",
                confirm: "Delete person",
                busy: store.busy,
                dismiss: dismiss
            ) {
                store.delete()
            }
        case .voiceProfile:
            PeopleConfirmSheet(
                title: "Remove voice profile?",
                message: "Sona will stop remembering this voice on this Mac. The person's name and meeting links stay.",
                confirm: "Remove voice profile",
                busy: store.busy,
                dismiss: dismiss
            ) {
                store.removeVoiceProfile()
            }
        case let .unlink(link):
            PeopleConfirmSheet(
                title: "Unlink this meeting?",
                message: "Remove \(link.meeting.title) from this person?",
                confirm: "Unlink meeting",
                busy: store.busy,
                dismiss: dismiss
            ) {
                store.removeLink(link.meeting.id)
            }
        case let .document(document):
            PeopleConfirmSheet(
                title: "Delete this document?",
                message: "Delete \(document.title) from Sona's documents?",
                confirm: "Delete document",
                busy: store.busy,
                dismiss: dismiss
            ) {
                deleteDocument?(document.id)
            }
        }
    }
}

/// One question, two answers. The destructive one is the accent-free button on
/// the right, and it closes the sheet as it commits.
struct PeopleConfirmSheet: View {
    let title: String
    let message: String
    let confirm: String
    var busy = false
    let dismiss: () -> Void
    let action: () -> Void

    var body: some View {
        PeopleSheet(title: title, dismiss: dismiss) {
            Text(message).bodyText(14, Theme.inkSecondary)
        } footer: {
            Button("Cancel", action: dismiss).buttonStyle(.secondary)
            Button(confirm) {
                dismiss()
                action()
            }
            .buttonStyle(.primary)
            .disabled(busy)
        }
    }
}

/// The sheet shell every question here is drawn in: a title, the question, and
/// the answers along the bottom.
struct PeopleSheet<Content: View, Footer: View>: View {
    let title: String
    let dismiss: () -> Void
    @ViewBuilder let content: () -> Content
    @ViewBuilder let footer: () -> Footer

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text(title).headlineText()
            content()
            HStack(spacing: 10) {
                Spacer()
                footer()
            }
        }
        .padding(24)
        .frame(width: 440)
        .background(Theme.page)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusDialog))
    }
}

/// Merging keeps both records' samples, which is what merging two records of
/// one person means. Until a person is chosen there is nothing to confirm.
struct PersonMergeSheet: View {
    let store: PeopleStore
    let dismiss: () -> Void
    @State private var target: String = ""

    var body: some View {
        let options = (store.entries ?? []).filter { $0.person.id != store.detail?.person.id }
        PeopleSheet(title: "Merge this person?", dismiss: dismiss) {
            VStack(alignment: .leading, spacing: 12) {
                Text(message(options)).bodyText(14, Theme.inkSecondary)
                Picker("", selection: $target) {
                    Text("Choose a person").tag("")
                    ForEach(options) { entry in
                        Text(entry.person.displayName).tag(entry.person.id)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .controlSize(.large)
            }
        } footer: {
            Button("Cancel", action: dismiss).buttonStyle(.secondary)
            Button("Merge") {
                let target = target
                dismiss()
                store.merge(into: target)
            }
            .buttonStyle(.primary)
            .disabled(store.busy || target.isEmpty)
        }
    }

    private func message(_ options: [PersonListEntry]) -> String {
        let source = store.detail?.person.displayName ?? "this person"
        let name = options.first { $0.person.id == target }?.person.displayName ?? "another person"
        return "Merge \(source) into \(name)? Links, aliases, and relationship history will be combined."
    }
}

/// Moves the chosen evidence onto another person, new or existing.
///
/// A new person needs a name and nothing else, because the split is what
/// creates them. Moving onto somebody who already exists needs at least one
/// item, because moving nothing is not a move.
struct PersonSplitSheet: View {
    let store: PeopleStore
    let dismiss: () -> Void
    /// Empty means the target is a person this split creates.
    @State private var target: String = ""
    @State private var name: String = ""
    @State private var meetingIds: Set<String> = []
    @State private var aliases: Set<String> = []
    @State private var calendarEmails: Set<String> = []
    @State private var documentIds: Set<String> = []

    var body: some View {
        PeopleSheet(title: "Split this person?", dismiss: dismiss) {
            if let detail = store.detail {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        Text("Move selected details and meetings from \(detail.person.displayName) to another person.")
                            .bodyText(14, Theme.inkSecondary)
                        targetPicker
                        if target.isEmpty {
                            VStack(alignment: .leading, spacing: 6) {
                                Text("New person's name").metaText(Theme.inkSecondary)
                                InputField(prompt: "Name", text: $name)
                            }
                        }
                        evidence(detail)
                    }
                    .padding(.vertical, 2)
                }
                .frame(maxHeight: 360)
            }
        } footer: {
            Button("Cancel", action: dismiss).buttonStyle(.secondary)
            Button("Split person") {
                let request = self.request
                dismiss()
                store.split(
                    to: request.target,
                    meetingIds: request.meetingIds,
                    aliases: request.aliases,
                    calendarEmails: request.calendarEmails,
                    documentIds: request.documentIds)
            }
            .buttonStyle(.primary)
            .disabled(store.busy || !submittable)
        }
    }

    private var targetPicker: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Move to").metaText(Theme.inkSecondary)
            Picker("", selection: $target) {
                Text("New person").tag("")
                ForEach((store.entries ?? []).filter { $0.person.id != store.detail?.person.id }) { entry in
                    Text(entry.person.displayName).tag(entry.person.id)
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .controlSize(.large)
        }
    }

    @ViewBuilder
    private func evidence(_ detail: PersonDetail) -> some View {
        let empty = detail.links.isEmpty
            && detail.person.aliases.isEmpty
            && detail.person.calendarEmails.isEmpty
            && detail.documents.isEmpty
        if empty {
            Text("This person has no details or meetings to split.").metaText()
        } else {
            VStack(alignment: .leading, spacing: 14) {
                Text("Choose what to move").metaText(Theme.inkSecondary)
                PersonSplitGroup(
                    label: "Meetings",
                    options: detail.links.map {
                        (value: $0.meeting.id, label: "\($0.meeting.title) · \(PeopleFormat.moment($0.meeting.at))")
                    },
                    selected: $meetingIds)
                PersonSplitGroup(
                    label: "Aliases",
                    options: detail.person.aliases.map { (value: $0, label: $0) },
                    selected: $aliases)
                PersonSplitGroup(
                    label: "Calendar emails",
                    options: detail.person.calendarEmails.map { (value: $0, label: $0) },
                    selected: $calendarEmails)
                PersonSplitGroup(
                    label: "Documents",
                    options: detail.documents.map { (value: $0.id, label: $0.title) },
                    selected: $documentIds)
                if !target.isEmpty, selectedCount == 0 {
                    Text("Choose at least one item to move.").metaText()
                }
            }
        }
    }

    private var selectedCount: Int {
        meetingIds.count + aliases.count + calendarEmails.count + documentIds.count
    }

    private var submittable: Bool {
        target.isEmpty
            ? !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            : selectedCount > 0
    }

    private var request: (
        target: PersonSplitTarget, meetingIds: [String], aliases: [String],
        calendarEmails: [String], documentIds: [String]
    ) {
        let target: PersonSplitTarget = self.target.isEmpty
            ? .create(displayName: name.trimmingCharacters(in: .whitespacesAndNewlines))
            : .existing(personId: self.target)
        return (
            target: target,
            meetingIds: Array(meetingIds),
            aliases: Array(aliases),
            calendarEmails: Array(calendarEmails),
            documentIds: Array(documentIds))
    }
}

/// One group of things a split can move. A group with nothing in it is not
/// drawn.
struct PersonSplitGroup: View {
    let label: String
    let options: [(value: String, label: String)]
    @Binding var selected: Set<String>

    var body: some View {
        if !options.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                Text(label).metaText(Theme.inkSecondary)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(options, id: \.value) { option in
                        Toggle(isOn: binding(option.value)) {
                            Text(option.label).bodyText(14).lineLimit(1)
                        }
                        .toggleStyle(.checkbox)
                    }
                }
            }
        }
    }

    private func binding(_ value: String) -> Binding<Bool> {
        Binding(
            get: { selected.contains(value) },
            set: { on in
                if on {
                    selected.insert(value)
                } else {
                    selected.remove(value)
                }
            })
    }
}

/// You say this meeting was with this person: the strongest evidence there is,
/// and the only kind Sona never guesses at. The meetings already linked are
/// not offered.
struct LinkPickerSheet: View {
    let store: PeopleStore
    let dismiss: () -> Void

    @State private var query = ""

    var body: some View {
        let picker = store.linkPicker
        let rows = picker.state.rows
        PeopleSheet(title: "Link a meeting", dismiss: dismiss) {
            VStack(alignment: .leading, spacing: 12) {
                InputField(prompt: "Search by title", text: $query)
                    .onChange(of: query) { _, next in
                        store.loadLinkCandidates(query: next.trimmingCharacters(in: .whitespaces))
                    }
                if !rows.isEmpty {
                    ScrollView {
                        Card {
                            ForEach(rows) { candidate in
                                CardRow(action: {
                                    dismiss()
                                    store.addManualLink(candidate.sessionId)
                                }) {
                                    VStack(alignment: .leading, spacing: 4) {
                                        Text(candidate.title)
                                            .font(TypeScale.label())
                                            .foregroundStyle(Theme.ink)
                                            .lineLimit(1)
                                        Text(PeopleFormat.moment(candidate.createdAt))
                                            .metaText()
                                            .monospacedDigit()
                                    }
                                }
                            }
                        }
                    }
                    .frame(maxHeight: 320)
                }
                footnote(picker)
            }
        } footer: {
            Button("Cancel", action: dismiss).buttonStyle(.secondary)
        }
    }

    /// Under the rows: what the read is doing, what it could not do, or the
    /// way to the next page. The "everything is linked" sentence is reserved
    /// for an unfiltered list that has been read to its end.
    @ViewBuilder
    private func footnote(_ picker: LinkPicker) -> some View {
        switch picker.state {
        case .idle, .loading:
            Text(picker.state.rows.isEmpty ? "Loading…" : "Loading more…").bodyText(14, Theme.inkSecondary)
        case let .failed(reason, _):
            HStack(spacing: 12) {
                Text("Sona could not read the meetings: \(reason)").bodyText(14, Theme.live)
                Spacer(minLength: 0)
                Button("Try again") {
                    if picker.state.rows.isEmpty {
                        store.loadLinkCandidates(query: picker.query)
                    } else {
                        store.loadMoreLinkCandidates()
                    }
                }
                .buttonStyle(.compact)
            }
        case let .loaded(rows, next):
            if next != nil {
                Button("Show more") { store.loadMoreLinkCandidates() }.buttonStyle(.compact)
            } else if rows.isEmpty {
                Text(picker.query.isEmpty
                    ? "Every meeting Sona has is already linked to this person."
                    : "No meeting title matches \u{201C}\(picker.query)\u{201D}.")
                    .bodyText(14, Theme.inkSecondary)
            }
        }
    }
}

// MARK: - One organization

/// One organization, read across its people: who is there, what you met
/// about, and what is still open. It owns no write: everything that could
/// change is one row away, on the person it belongs to.
struct OrganizationView: View {
    let store: PeopleStore
    var openMeeting: (String) -> Void = { _ in }

    var body: some View {
        Page(back: PageBack("People") { store.toList() }, title: store.organization?.name) {
            VStack(alignment: .leading, spacing: 0) {
                if let detail = store.organization {
                    HStack(alignment: .center, spacing: 20) {
                        CompanyMark(name: detail.name, size: 84)
                        VStack(alignment: .leading, spacing: 6) {
                            Text(detail.name)
                                .heroText()
                                .lineLimit(2)
                                .accessibilityAddTraits(.isHeader)
                                .pageHeadline()
                            Text(PeopleModel.people(detail.people.count)).bodyText(14, Theme.inkSecondary)
                        }
                    }
                    .frame(minHeight: 84)
                    .padding(.top, 8)
                    .padding(.bottom, 24)
                    ErrorNote(store.error)
                    stats(detail)
                        .padding(.bottom, 28)
                    PageSection("People here") {
                        Card {
                            if detail.people.isEmpty {
                                CardLine("Nobody here yet.")
                            } else {
                                ForEach(detail.people) { entry in
                                    PersonListRow(entry: entry, photoRevision: store.photoRevision) {
                                        store.openPerson(entry.person.id)
                                    }
                                }
                            }
                        }
                    }
                    PageSection("Recent meetings") {
                        Card {
                            if detail.recentMeetings.isEmpty {
                                CardLine("No linked meetings.")
                            } else {
                                ForEach(detail.recentMeetings) { meeting in
                                    CardRow(action: { openMeeting(meeting.id) }) {
                                        VStack(alignment: .leading, spacing: 4) {
                                            Text(meeting.title)
                                                .font(TypeScale.label())
                                                .foregroundStyle(Theme.ink)
                                                .lineLimit(1)
                                            Text(
                                                meeting.headline.map { "\($0) · \(PeopleFormat.moment(meeting.at))" }
                                                    ?? PeopleFormat.moment(meeting.at)
                                            )
                                            .metaText()
                                            .monospacedDigit()
                                            .lineLimit(1)
                                        }
                                    } trailing: {
                                        PeopleChevron()
                                    }
                                }
                            }
                        }
                    }
                    PageSection("Still open") {
                        Card {
                            if detail.openLoops.isEmpty {
                                CardLine("Nothing is still open.")
                            } else {
                                ForEach(detail.openLoops) { loop in
                                    PersonLoopRow(loop: loop, openMeeting: openMeeting)
                                }
                            }
                        }
                    }
                } else if store.organizationFailed {
                    ErrorNote(store.error)
                    Card {
                        CardRow {
                            Text("Couldn't load this organization.").bodyText(14, Theme.inkSecondary)
                        } trailing: {
                            Button("Retry") { store.reload() }.buttonStyle(.compact)
                        }
                    }
                } else {
                    PersonHeroPlaceholder(square: true)
                }
            }
        }
    }

    /// The organization in four numbers. Its meetings are counted once each
    /// on the companies list, so that is where the count comes from.
    private func stats(_ detail: OrganizationDetail) -> some View {
        let meetings = store.companies?.first { $0.name == detail.name }?.meetingsCount
        let since = detail.recentMeetings.map(\.at).max().map { PeopleFormat.since($0) }
        return HStack(spacing: 10) {
            NumberTile("\(detail.people.count)", label: "People")
            NumberTile(meetings.map { "\($0)" } ?? "—", label: "Meetings",
                       tone: meetings == nil ? Theme.inkTertiary : Theme.ink)
            NumberTile("\(detail.openLoops.count)", label: "Still open")
            NumberTile(since?.value ?? "—", label: "Last met", unit: since?.unit,
                       tone: since == nil ? Theme.inkTertiary : Theme.ink)
        }
    }
}

// MARK: - People beside a meeting

/// Everybody in this meeting you have met before, what you met about, and the
/// one thing still open with them. Drawn above a meeting's notes, where the
/// question is who is in the room.
struct PersonMeetingContextView: View {
    let store: PeopleStore
    let sessionId: String
    /// Opens a person's page. The band is read inside a meeting, so the route
    /// out of it belongs to whoever put it there.
    var openPerson: (String) -> Void = { _ in }
    @State private var rows: [PersonMeetingContextRow] = []

    var body: some View {
        // Only people with a meeting before this one: "previously together"
        // with nothing previous is the band saying nothing. The read hangs off
        // the container rather than the band, so it happens once whether or
        // not there turns out to be anything to draw.
        let previous = rows.filter { $0.lastPriorMeeting != nil }
        VStack(alignment: .leading, spacing: 0) {
            if !previous.isEmpty {
                PageSection("Previously together") {
                    Card {
                        ForEach(previous) { row in
                            CardRow(action: { openPerson(row.personId) }) {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(row.displayName)
                                        .font(TypeScale.label())
                                        .foregroundStyle(Theme.ink)
                                        .lineLimit(1)
                                    if let loop = row.topOpenLoop {
                                        Text(loop.text).metaText(Theme.inkSecondary).lineLimit(1)
                                    }
                                }
                            } trailing: {
                                VStack(alignment: .trailing, spacing: 4) {
                                    Text(earlier(row)).metaText().monospacedDigit()
                                    if let last = row.lastPriorMeeting {
                                        Text("Last \(PeopleFormat.moment(last.at))").metaText().monospacedDigit()
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        .task(id: sessionId) { rows = await store.meetingContext(sessionId) }
    }

    /// How many times you met before this meeting.
    private func earlier(_ row: PersonMeetingContextRow) -> String {
        row.priorMeetings == 1 ? "1 earlier meeting" : "\(row.priorMeetings) earlier meetings"
    }
}

// MARK: - Voice identity

/// The speakers in this meeting Sona cannot put a name to, and the way into
/// answering for them.
struct VoiceIdentitySection: View {
    let store: VoiceIdentityStore

    var body: some View {
        PageSection("Speakers") {
            Card {
                if !store.loaded {
                    CardLine("Reading speakers…")
                } else if store.speakers.isEmpty, let error = store.error {
                    CardRow {
                        Text(error).bodyText(14, Theme.inkSecondary)
                    } trailing: {
                        Button("Try again") { reload() }
                            .buttonStyle(.compact)
                    }
                } else if store.speakers.isEmpty {
                    CardLine("Speakers show up here once the transcript is ready.")
                } else {
                    ForEach(store.suggestions) { suggestion in
                        VoiceSuggestionRow(
                            suggestion: suggestion,
                            speakerName: store.speaker(suggestion.speakerId)?.displayName ?? "This speaker",
                            busy: store.busy,
                            confirm: { store.confirm(suggestion) },
                            reject: { store.ask(.label(suggestion.speakerId)) })
                    }
                    if !store.unresolved.isEmpty {
                        CardRow {
                            Text(
                                store.unresolved.count == 1
                                    ? "1 speaker to label"
                                    : "\(store.unresolved.count) speakers to label"
                            )
                            .bodyText(14)
                            .monospacedDigit()
                        } trailing: {
                            Button("Label speaker") { store.askNext() }
                                .buttonStyle(.compact)
                                .disabled(store.busy)
                        }
                    }
                    ForEach(store.speakers) { speaker in
                        CardRow {
                            Text(speaker.displayName).bodyText(14)
                        } trailing: {
                            Button("Correct") { store.askCorrection(speaker.speakerId) }
                                .buttonStyle(.quiet)
                                .disabled(store.busy)
                        }
                    }
                }
            }
        }
        .sheet(isPresented: asking) {
            VoiceIdentitySheet(store: store)
        }
    }

    private func reload() {
        guard let sessionId = store.sessionId else { return }
        Task { await store.load(sessionId) }
    }

    /// The sheet is up exactly while the store holds a question, and closing
    /// it withdraws that question.
    private var asking: Binding<Bool> {
        Binding(
            get: { store.question != nil },
            set: { shown in
                if !shown { store.ask(nil) }
            })
    }
}

/// A waiting speaker whose voice matches the one you named as somebody, or
/// as yourself, in other meetings. Confirm names the speaker; "Not them" asks
/// who it is instead, where another person or "unknown" answers it. Nothing
/// is named until one of those is pressed.
struct VoiceSuggestionRow: View {
    let suggestion: VoiceSpeakerSuggestion
    let speakerName: String
    let busy: Bool
    let confirm: () -> Void
    let reject: () -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        CardRow {
            HStack(spacing: 12) {
                if let personId = suggestion.personId {
                    PersonPortrait(
                        personId: personId,
                        name: suggestion.displayName,
                        size: 28,
                        revision: model.people.photoRevision)
                } else {
                    Image(systemName: "person.crop.circle.fill")
                        .font(.system(size: 26))
                        .foregroundStyle(Theme.inkSecondary)
                        .frame(width: 28, height: 28)
                }
                VStack(alignment: .leading, spacing: 3) {
                    if suggestion.personId == nil {
                        Text("\(speakerName) sounds like you").bodyText(14)
                        Text("Matches the voice Sona knows as yours").metaText()
                    } else {
                        Text("\(speakerName) sounds like \(suggestion.displayName)").bodyText(14)
                        Text("Matches the voice you named \(suggestion.displayName) in other meetings").metaText()
                    }
                }
            }
        } trailing: {
            HStack(spacing: 8) {
                Button(suggestion.personId == nil ? "Not me" : "Not them", action: reject)
                    .buttonStyle(.quiet)
                    .disabled(busy)
                Button("Confirm", action: confirm)
                    .buttonStyle(.compact)
                    .disabled(busy)
            }
        }
    }
}

/// One meeting's speakers on its review page: the store is built once per
/// page and re-read whenever the page moves to another meeting.
struct MeetingVoiceIdentitySection: View {
    let sessionId: String
    @Environment(AppModel.self) private var model
    @State private var store: VoiceIdentityStore?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let store {
                VoiceIdentitySection(store: store)
            }
        }
        .task(id: sessionId) {
            let store = self.store ?? VoiceIdentityStore(core: model.core)
            self.store = store
            await store.load(sessionId)
        }
    }
}

/// Who this speaker is. One command writes a label and a correction, so the
/// words on screen are the only thing that says which one was answered.
///
/// The speaker's own name is in the title, because the sheet reopens on the
/// next unnamed speaker without closing: three speakers to label is the same
/// question three times, and the name is what tells them apart.
struct VoiceIdentitySheet: View {
    let store: VoiceIdentityStore
    @Environment(AppModel.self) private var model
    /// Empty means a person this answer creates.
    @State private var target: String = ""
    @State private var name: String = ""
    @State private var remember = false
    /// Other meetings with the same voice the user says are someone else.
    @State private var notSame: Set<String> = []
    /// The meeting whose clip this sheet started.
    @State private var playing: String?

    var body: some View {
        PeopleSheet(title: title, dismiss: { store.ask(nil) }) {
            VStack(alignment: .leading, spacing: 16) {
                ErrorNote(store.error)
                if store.confirmingUnknown {
                    Text(
                        "This transcript stops naming \(speakerName), and Sona deletes the voice samples saved from this speaker. The transcript text and your notes stay."
                    )
                    .bodyText(14, Theme.inkSecondary)
                } else if store.peopleFailed {
                    HStack(spacing: 12) {
                        Text("Couldn't load people.").bodyText(14, Theme.inkSecondary)
                        Button("Retry") { Task { await store.loadPeople() } }.buttonStyle(.compact)
                    }
                } else if let people = store.people {
                    form(people)
                } else {
                    Text("Loading…").bodyText(14, Theme.inkSecondary)
                }
            }
        } footer: {
            footer
        }
        .onDisappear {
            if let playing, audio.sessionId == playing { audio.stop() }
        }
    }

    @ViewBuilder
    private func form(_ people: [PersonListEntry]) -> some View {
        if let sameVoice {
            heardBefore(sameVoice)
        }
        VStack(alignment: .leading, spacing: 6) {
            Text("Choose a person").metaText(Theme.inkSecondary)
            Picker("", selection: $target) {
                Text("New person").tag("")
                let here = people.filter { store.inMeeting.contains($0.person.id) }
                if here.isEmpty {
                    ForEach(people) { entry in
                        Text(entry.person.displayName).tag(entry.person.id)
                    }
                } else {
                    Section("In this meeting") {
                        ForEach(here) { entry in
                            Text(entry.person.displayName).tag(entry.person.id)
                        }
                    }
                    Section("Everyone else") {
                        ForEach(people.filter { !store.inMeeting.contains($0.person.id) }) { entry in
                            Text(entry.person.displayName).tag(entry.person.id)
                        }
                    }
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .controlSize(.large)
        }
        if target.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                Text("New person's name").metaText(Theme.inkSecondary)
                InputField(prompt: "Name", text: $name)
            }
        }
        VStack(alignment: .leading, spacing: 4) {
            Toggle(isOn: $remember) {
                Text("Name this voice automatically in later meetings").bodyText(14)
            }
            .toggleStyle(.checkbox)
            Text("Sona keeps a fingerprint of each voice on this Mac to suggest names. It is deleted with the meeting, or after 90 days if nobody names it.")
                .metaText(Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func heardBefore(_ sameVoice: SameVoice) -> some View {
        var seen = Set<String>()
        let days = sameVoice.meetings.map { PlanningFormat.day($0.startedAtUtcMs) }
            .filter { seen.insert($0).inserted }
        return VStack(alignment: .leading, spacing: 6) {
            Text("This voice was also in \(days.formatted(.list(type: .and))). Same person?")
                .bodyText(14)
                .fixedSize(horizontal: false, vertical: true)
            ForEach(sameVoice.meetings) { meeting in
                HStack(spacing: 8) {
                    Toggle(isOn: same(meeting)) {
                        HStack(spacing: 6) {
                            Text(PlanningFormat.day(meeting.startedAtUtcMs)).bodyText(14)
                            Text(meeting.title).metaText(Theme.inkSecondary).lineLimit(1)
                        }
                    }
                    .toggleStyle(.checkbox)
                    Spacer(minLength: 8)
                    listen(meeting)
                }
            }
            if playing != nil, let message = audio.error {
                Text(message).metaText(Theme.live)
            }
        }
    }

    private func same(_ meeting: SameVoiceMeeting) -> Binding<Bool> {
        Binding(
            get: { !notSame.contains(meeting.sessionId) },
            set: { same in
                if same { notSame.remove(meeting.sessionId) } else { notSame.insert(meeting.sessionId) }
            })
    }

    private func listen(_ meeting: SameVoiceMeeting) -> some View {
        let active = playing == meeting.sessionId && audio.sessionId == meeting.sessionId
            && (audio.playing || audio.loading)
        let recording = audio.heldForRecording || model.capture != .idle
        return Button {
            if active {
                audio.stop()
                playing = nil
            } else if let clip = meeting.clip {
                playing = meeting.sessionId
                audio.play(sessionId: meeting.sessionId, startOffsetNs: clip.startOffsetNs,
                           endOffsetNs: clip.endOffsetNs)
            }
        } label: {
            Image(systemName: active ? "stop.fill" : "play.fill")
                .font(.system(size: 11, weight: .semibold))
                .frame(width: 22, height: 22)
        }
        .buttonStyle(.quiet)
        .disabled(meeting.clip == nil || recording)
        .help(meeting.clip == nil ? "No recording of this voice."
            : (recording ? ConversationAudioPlayback.heldNotice : "Hear this voice"))
        .accessibilityLabel(active ? "Stop" : "Hear this voice in \(meeting.title)")
    }

    private var audio: ConversationAudioPlayback { model.meetings.audio }

    /// The other meetings the same voice waits in, for a speaker being labelled.
    private var sameVoice: SameVoice? {
        guard let question = store.question, !question.isCorrection else { return nil }
        return store.sameVoice.first { $0.speakerId == question.speakerId }
    }

    @ViewBuilder
    private var footer: some View {
        if store.confirmingUnknown {
            Button("Cancel") { store.cancelUnknown() }.buttonStyle(.secondary)
            Button("Mark as unknown") { store.markUnknown() }
                .buttonStyle(.primary)
                .disabled(store.busy)
        } else {
            Button("Not now") { store.ask(nil) }.buttonStyle(.quiet)
            Button("Mark as unknown") { store.requestUnknown() }
                .buttonStyle(.secondary)
                .disabled(store.busy || store.people == nil)
            Button(store.busy ? "Saving…" : "Save") { save() }
                .buttonStyle(.primary)
                .disabled(!savable)
        }
    }

    private var speakerName: String {
        guard let question = store.question, let speaker = store.speaker(question.speakerId) else {
            return "Unknown speaker"
        }
        return speaker.displayName
    }

    private var title: String {
        guard let question = store.question else { return "Label speaker" }
        return question.isCorrection ? "Correct \(speakerName)" : "Label \(speakerName)"
    }

    private var savable: Bool {
        guard !store.busy, store.people != nil else { return false }
        return !target.isEmpty || !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func save() {
        guard savable else { return }
        let voiceTarget: VoiceIdentityTarget = target.isEmpty
            ? .create(displayName: name.trimmingCharacters(in: .whitespacesAndNewlines))
            : .existing(personId: target)
        let kept = sameVoice?.meetings.filter { !notSame.contains($0.sessionId) } ?? []
        store.save(voiceTarget, remember: remember, sameVoice: kept)
        if let playing, audio.sessionId == playing { audio.stop() }
        target = ""
        name = ""
        remember = false
        notSame = []
        playing = nil
    }
}
