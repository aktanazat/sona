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
    /// Adds context about this person. The import surface owns the file
    /// picker and the command behind it; a person's page owns the verb.
    var ingestDocument: () -> Void = {}
    /// Deletes an imported document, when the import surface is there to do
    /// it. Absent, the catalogue is a catalogue and nothing on it destroys.
    var deleteDocument: ((String) -> Void)?
    /// Opens the word list, where a mined term is taught a spelling. The
    /// vocabulary surface owns that write; absent, the row is not drawn.
    var openVocabulary: (() -> Void)?

    var body: some View {
        Page {
            switch store.route {
            case .list:
                PeopleListView(store: store, openMeeting: openMeeting, openVocabulary: openVocabulary)
            case .person:
                PersonView(
                    store: store,
                    openMeeting: openMeeting,
                    ingestDocument: ingestDocument,
                    deleteDocument: deleteDocument)
            case .organization:
                OrganizationView(store: store, openMeeting: openMeeting)
            }
        }
    }
}

// MARK: - The list

/// Everybody Sona knows, the organizations they are at, what is still open
/// across all of them, and the words it keeps hearing and cannot spell.
struct PeopleListView: View {
    let store: PeopleStore
    var openMeeting: (String) -> Void = { _ in }
    var openVocabulary: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            PageTitle("People", subtitle: subtitle)
            ErrorNote(store.error)
            if store.listFailed {
                Card {
                    CardRow {
                        Text("Couldn't load people.").bodyText(15, Theme.inkSecondary)
                    } trailing: {
                        Button("Retry") { store.reload() }.buttonStyle(.compact)
                    }
                }
            } else if let entries = store.entries {
                if entries.isEmpty {
                    Card {
                        CardRow {
                            Text("People from your meetings appear here.").bodyText(15, Theme.inkSecondary)
                        }
                    }
                } else {
                    PeopleOrganizationStrip(entries: entries, store: store)
                    Card {
                        ForEach(entries) { entry in
                            PersonListRow(entry: entry) { store.openPerson(entry.person.id) }
                        }
                    }
                }
            } else {
                Card {
                    CardRow { Text("Loading…").bodyText(15, Theme.inkSecondary) }
                }
            }
            PeopleInboxSection(store: store, openMeeting: openMeeting)
            PeopleCandidatesSection(store: store, openVocabulary: openVocabulary)
        }
    }

    private var subtitle: String {
        guard let entries = store.entries else { return "Reading the people Sona knows" }
        if entries.isEmpty { return "Nobody yet" }
        return PeopleModel.people(entries.count)
    }
}

/// The organizations the loaded rows already carry, as a strip above them.
///
/// Derived from the rows rather than asked for: every person already says
/// where they are, so a second command for the same fact would be a second
/// answer to it. A corpus with no calendar domains on it draws nothing, which
/// is the honest absence — there is no organization to name.
struct PeopleOrganizationStrip: View {
    let entries: [PersonListEntry]
    let store: PeopleStore

    var body: some View {
        let organizations = PeopleModel.organizations(entries)
        if !organizations.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                Text("Organizations").sectionLabel()
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 150), spacing: 8, alignment: .leading)],
                    alignment: .leading,
                    spacing: 8
                ) {
                    ForEach(organizations, id: \.name) { organization in
                        Button {
                            store.openOrganization(organization.name)
                        } label: {
                            HStack(spacing: 8) {
                                Text(organization.name).lineLimit(1)
                                Text("\(organization.count)").monospacedDigit().foregroundStyle(Theme.inkTertiary)
                            }
                        }
                        .buttonStyle(.compact)
                    }
                }
            }
            .padding(.bottom, 20)
        }
    }
}

/// One person, one row: the name, and one line of relationship facts under it
/// — where they are, how many meetings you have had, how long ago the last one
/// was. The elapsed phrasing is what somebody scans a list of people for; the
/// exact date is on the person's own page, which is what the row opens.
struct PersonListRow: View {
    let entry: PersonListEntry
    let open: () -> Void

    var body: some View {
        CardRow(action: open) {
            VStack(alignment: .leading, spacing: 4) {
                Text(entry.person.displayName).font(TypeScale.label()).foregroundStyle(Theme.ink).lineLimit(1)
                Text(facts).metaText().monospacedDigit().lineLimit(1)
            }
        } trailing: {
            HStack(spacing: 10) {
                if entry.suggestedCount > 0 {
                    Chip("\(entry.suggestedCount) to confirm")
                }
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Theme.inkTertiary)
            }
        }
    }

    /// One line, in the order it is read: the place, the count, then how long
    /// ago. Interpuncts join it rather than separating cells, because it is a
    /// sentence about a person and not a table of them.
    private var facts: String {
        var parts: [String] = []
        if let organization = entry.person.organization {
            parts.append(organization)
        }
        parts.append(PeopleModel.meetings(entry.confirmedCount))
        if let last = entry.lastMeeting {
            parts.append("Last met \(PeopleFormat.elapsed(last.at))")
        } else if let last = entry.lastMeetingAtUtcMs {
            parts.append("Last met \(PeopleFormat.elapsed(PeopleFormat.date(last)))")
        }
        return parts.joined(separator: " · ")
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
            }
        }
    }
}

/// Terms Sona keeps hearing in meetings and does not know how to write.
///
/// Saying no is answered here: the store remembers the answer, so the term
/// goes quiet on every surface that mines it and comes back on none of them.
/// Teaching one is a write to the word list, which the vocabulary surface
/// owns, so this row goes there rather than keeping a second copy of it.
struct PeopleCandidatesSection: View {
    let store: PeopleStore
    var openVocabulary: (() -> Void)?

    var body: some View {
        if !store.candidates.isEmpty {
            PageSection("Words from your meetings") {
                Card {
                    ForEach(store.candidates) { candidate in
                        CardRow {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(candidate.text).bodyText(14)
                                Text(mentions(candidate)).metaText().monospacedDigit()
                            }
                        } trailing: {
                            Button("Dismiss") { store.dismissCandidate(candidate.text) }
                                .buttonStyle(.compact)
                        }
                    }
                    if let openVocabulary {
                        CardRow(action: openVocabulary) {
                            Text("Add one in Custom Words").bodyText(14, Theme.accent)
                        }
                    }
                }
            }
        }
    }

    private func mentions(_ candidate: PeopleVocabularyCandidate) -> String {
        let heard = candidate.occurrences == 1 ? "1 mention" : "\(candidate.occurrences) mentions"
        return "\(heard) · \(PeopleModel.meetings(candidate.meetingsCount))"
    }
}

// MARK: - One person

/// A person's page, in the three lines every document page in the app uses:
/// the way back, the name, and one quiet line — where they are, and when you
/// last met.
///
/// Every verb is in the one menu. Rename opens the title as a field, which
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
        VStack(alignment: .leading, spacing: 0) {
            BackLink(title: "People") { store.toList() }
            if let detail = store.detail {
                PersonHeaderView(
                    store: store,
                    detail: detail,
                    renaming: $renaming,
                    prompt: $prompt,
                    ingestDocument: ingestDocument)
                ErrorNote(store.error)
                PersonSections(
                    store: store,
                    detail: detail,
                    prompt: $prompt,
                    openMeeting: openMeeting,
                    deleteDocument: deleteDocument)
            } else {
                PageTitle("People", subtitle: store.detailFailed ? "This person could not be read" : "Loading…")
                ErrorNote(store.error)
                if store.detailFailed {
                    Card {
                        CardRow {
                            Text("Couldn't load this person.").bodyText(15, Theme.inkSecondary)
                        } trailing: {
                            Button("Retry") { store.reload() }.buttonStyle(.compact)
                        }
                    }
                }
            }
        }
        .sheet(item: $prompt) { prompt in
            PersonPromptSheet(
                store: store,
                prompt: prompt,
                deleteDocument: deleteDocument,
                dismiss: { self.prompt = nil })
        }
    }
}

/// The way back, the name, the one quiet line, and the menu every verb lives in.
struct PersonHeaderView: View {
    let store: PeopleStore
    let detail: PersonDetail
    @Binding var renaming: Bool
    @Binding var prompt: PersonView.Prompt?
    var ingestDocument: () -> Void = {}

    var body: some View {
        HStack(alignment: .top, spacing: 16) {
            VStack(alignment: .leading, spacing: 8) {
                if renaming {
                    PersonNameField(
                        name: detail.person.displayName,
                        commit: { store.rename(to: $0) },
                        close: { renaming = false })
                } else {
                    Text(detail.person.displayName).titleText().lineLimit(1)
                }
                facts
            }
            Spacer(minLength: 16)
            menu
        }
        .padding(.bottom, 28)
    }

    /// The organization is the one word on the line that goes somewhere: it
    /// has a page of its own, and this is the only place its name appears.
    @ViewBuilder
    private var facts: some View {
        let lastMet = PeopleModel.lastConfirmed(detail.links)
        if detail.person.organization != nil || lastMet != nil {
            HStack(spacing: 0) {
                if let organization = detail.person.organization {
                    Button { store.openOrganization(organization) } label: {
                        Text(organization).underline()
                    }
                    .buttonStyle(.quiet)
                    if lastMet != nil {
                        Text(" · ").metaText()
                    }
                }
                if let lastMet {
                    Text("Last met \(PeopleFormat.moment(lastMet))").metaText().monospacedDigit()
                }
            }
        }
    }

    private var menu: some View {
        Menu {
            Button("Rename") { renaming = true }
            Button("Regenerate") { store.regenerateSummary() }
            Button("Import document", action: ingestDocument)
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
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(Theme.inkSecondary)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(store.busy)
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
            .font(TypeScale.title)
            .foregroundStyle(Theme.ink)
            .focused($focused)
            .frame(maxWidth: 360)
            .padding(.horizontal, 12)
            .frame(height: 42)
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

/// The paragraph first, when there is one: who this person is to you. Then
/// what the page is for — what is still open between you, in both directions —
/// and only then the archive it came out of: the meetings, the shape they make,
/// the files, and last of all how Sona connected this person to any of it.
/// Provenance is the line a reader checks once, so it reads last.
///
/// Every one of these draws nothing when it holds nothing: an empty person is
/// a name and a menu, not eight labelled absences.
struct PersonSections: View {
    let store: PeopleStore
    let detail: PersonDetail
    @Binding var prompt: PersonView.Prompt?
    var openMeeting: (String) -> Void = { _ in }
    var deleteDocument: ((String) -> Void)?

    var body: some View {
        PersonSummaryView(summary: detail.person.summary)
        BriefingView(row: store.briefing)
        PersonLedgerView(
            label: "Open loops",
            rows: detail.openLoops.map(PersonLedgerRow.init),
            personName: detail.person.displayName,
            openMeeting: openMeeting)
        PersonLedgerView(
            label: "Commitments",
            rows: detail.commitments.map(PersonLedgerRow.init),
            personName: detail.person.displayName,
            openMeeting: openMeeting)
        PersonMeetingsView(store: store, links: detail.links, prompt: $prompt, openMeeting: openMeeting)
        PersonCadenceView(links: detail.links, talkSharePermille: detail.talkShareAvgPermille)
        PersonDocumentsView(
            documents: detail.documents,
            busy: store.busy,
            prompt: $prompt,
            deletable: deleteDocument != nil)
        PersonEvidenceView(person: detail.person, links: detail.links)
    }
}

/// Three sentences about a relationship, and the two facts that make them
/// readable: a paragraph a model wrote is only readable if you know which
/// model and when. No paragraph, no section — the verb that asks for the first
/// one is in the page's menu.
struct PersonSummaryView: View {
    let summary: PersonSummary?

    var body: some View {
        if let summary {
            PageSection("About") {
                Card {
                    VStack(alignment: .leading, spacing: 12) {
                        Text(summary.text).bodyText(16)
                        Text("Written by \(summary.modelId) on \(PeopleFormat.moment(PeopleFormat.date(summary.generatedAtUtcMs)))")
                            .metaText()
                            .monospacedDigit()
                    }
                    .padding(20)
                }
            }
        }
    }
}

/// What to walk into a room knowing: how often you have met, and the one thing
/// still open in each direction.
///
/// What you owe comes first — it is the thing a reader can act on in the next
/// thirty seconds — and an overdue handoff is marked, because "still open" and
/// "still open after two weeks" are different sentences to walk in with.
struct BriefingView: View {
    let row: BriefingRow?

    var body: some View {
        if let row {
            PageSection("Brief") {
                Card {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(relationship(row)).bodyText(14).monospacedDigit()
                        if let owed = BriefingView.owed(row) {
                            Text("You owe: \(owed.text)").metaText(Theme.inkSecondary).lineLimit(2)
                        }
                        if let awaited = BriefingView.awaited(row) {
                            Text(
                                awaited.stale
                                    ? "Overdue with \(row.displayName): \(awaited.text)"
                                    : "Waiting on \(row.displayName): \(awaited.text)"
                            )
                            .metaText(awaited.stale ? Theme.live : Theme.inkSecondary)
                            .lineLimit(2)
                        }
                    }
                    .padding(20)
                }
            }
        }
    }

    private func relationship(_ row: BriefingRow) -> String {
        let met = row.meetingsCount == 1
            ? "You have met \(row.displayName) once"
            : "You have met \(row.displayName) \(row.meetingsCount) times"
        guard let last = row.last else { return met }
        return "\(met) · last \(PeopleFormat.moment(last.at))"
    }

    private static func rows(_ row: BriefingRow) -> [PersonLedgerRow] {
        (row.openLoops.map(PersonLedgerRow.init) + row.commitments.map(PersonLedgerRow.init))
            .filter { !$0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    }

    static func owed(_ row: BriefingRow) -> PersonLedgerRow? {
        rows(row).mine.first
    }

    /// The overdue one if there is one, because that is the sentence worth the
    /// line.
    static func awaited(_ row: BriefingRow) -> PersonLedgerRow? {
        let waiting = rows(row).waitingOn
        return waiting.first { $0.stale } ?? waiting.first
    }
}

/// One ledger, in two groups: what you owe, and what they do.
///
/// Two groups inside one section, not two sections: "I owe" and "waiting on
/// them" are the same register read from opposite ends. A group with nothing in
/// it says nothing, and with both empty there is no section at all — a heading
/// over the sentence "nothing is still open" is a row spent saying the row has
/// nothing to say.
struct PersonLedgerView: View {
    let label: String
    let rows: [PersonLedgerRow]
    let personName: String
    var openMeeting: (String) -> Void = { _ in }

    var body: some View {
        let mine = rows.mine
        let waitingOn = rows.waitingOn
        if !mine.isEmpty || !waitingOn.isEmpty {
            PageSection(label) {
                Card {
                    if !mine.isEmpty {
                        PersonLedgerGroup(heading: "I owe", rows: mine, openMeeting: openMeeting)
                    }
                    if !waitingOn.isEmpty {
                        PersonLedgerGroup(
                            heading: "Waiting on \(personName)", rows: waitingOn, openMeeting: openMeeting)
                    }
                }
            }
        }
    }
}

struct PersonLedgerGroup: View {
    let heading: String
    let rows: [PersonLedgerRow]
    var openMeeting: (String) -> Void = { _ in }

    var body: some View {
        Text(heading)
            .metaText(Theme.inkSecondary)
            .padding(.horizontal, 20)
            .padding(.top, 14)
            .padding(.bottom, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .overlay(alignment: .bottom) { Hairline() }
        ForEach(rows) { row in
            CardRow(action: row.meetingId.isEmpty ? nil : { openMeeting(row.meetingId) }) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(row.text).bodyText(14)
                    HStack(spacing: 12) {
                        Text(row.title).metaText().lineLimit(1)
                        Text(PeopleFormat.moment(row.at)).metaText().monospacedDigit()
                        // "Open" under a heading that reads "Open loops" is
                        // the heading said twice.
                        if let contradiction = row.status.contradiction {
                            Text(contradiction).metaText(Theme.inkSecondary)
                        }
                        if row.stale {
                            Text("Overdue").metaText(Theme.live)
                        }
                        if let carried = row.carriedNote {
                            Text(carried).metaText().monospacedDigit()
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

/// Six months of confirmed meetings, and the share of the talking they carried.
/// A bare number over a chart labelled "Meeting cadence" reads as a cadence,
/// which it is not: it is how many meetings the bars are drawn from.
struct PersonCadenceView: View {
    let links: [PersonMeetingLink]
    let talkSharePermille: UInt32?

    var body: some View {
        let confirmed = PeopleModel.confirmed(links)
        if !confirmed.isEmpty {
            PageSection("Meeting cadence") {
                Card {
                    VStack(alignment: .leading, spacing: 16) {
                        Text(PeopleModel.meetings(UInt64(confirmed.count)))
                            .font(TypeScale.stat)
                            .foregroundStyle(Theme.ink)
                            .monospacedDigit()
                            .tracking(-0.4)
                        PersonCadenceBars(values: PeopleModel.cadence(links))
                        if let talkSharePermille {
                            HStack(spacing: 6) {
                                Text("Talk share").metaText(Theme.inkSecondary)
                                Text(PeopleFormat.talkShare(talkSharePermille)).metaText(Theme.ink).monospacedDigit()
                            }
                        }
                    }
                    .padding(20)
                }
            }
        }
    }
}

/// Six bars, oldest first. A month with no meeting in it keeps its place and
/// draws the track, because the gap is the fact.
struct PersonCadenceBars: View {
    let values: [Double]

    var body: some View {
        let peak = max(values.max() ?? 0, 1)
        HStack(alignment: .bottom, spacing: 8) {
            ForEach(Array(values.enumerated()), id: \.offset) { _, value in
                VStack(spacing: 0) {
                    Spacer(minLength: 0)
                    RoundedRectangle(cornerRadius: 3)
                        .fill(value > 0 ? Theme.accent : Theme.selection)
                        .frame(height: max(3, 64 * value / peak))
                }
                .frame(maxWidth: .infinity)
            }
        }
        .frame(height: 64)
        .accessibilityLabel("Meetings by month: \(values.map { "\(Int($0))" }.joined(separator: ", "))")
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
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(lines, id: \.self) { line in
                            Text(line).metaText().monospacedDigit()
                        }
                    }
                    .padding(20)
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

    var body: some View {
        PeopleSheet(title: "Link a meeting", dismiss: dismiss) {
            if let candidates = store.linkCandidates {
                if candidates.isEmpty {
                    Text("Every meeting Sona has is already linked to this person.")
                        .bodyText(14, Theme.inkSecondary)
                } else {
                    ScrollView {
                        Card {
                            ForEach(candidates) { candidate in
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
            } else {
                Text("Loading…").bodyText(14, Theme.inkSecondary)
            }
        } footer: {
            Button("Cancel", action: dismiss).buttonStyle(.secondary)
        }
    }
}

// MARK: - One organization

/// One organization, read across its people.
///
/// The same three sections a person's page has, in the same order — who, what
/// you met about, what is still open — because an organization is not a
/// different kind of noun here, it is a set of people. It owns no write:
/// everything that could change is one row away, on the person it belongs to.
struct OrganizationView: View {
    let store: PeopleStore
    var openMeeting: (String) -> Void = { _ in }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            BackLink(title: "People") { store.toList() }
            if let detail = store.organization {
                PageTitle(detail.name, subtitle: PeopleModel.people(detail.people.count))
                ErrorNote(store.error)
                PageSection("People here") {
                    Card {
                        if detail.people.isEmpty {
                            CardRow { Text("Nobody here yet.").bodyText(15, Theme.inkSecondary) }
                        } else {
                            ForEach(detail.people) { entry in
                                CardRow(action: { store.openPerson(entry.person.id) }) {
                                    Text(entry.person.displayName)
                                        .font(TypeScale.label())
                                        .foregroundStyle(Theme.ink)
                                        .lineLimit(1)
                                } trailing: {
                                    HStack(spacing: 10) {
                                        Text(PeopleModel.meetings(entry.confirmedCount))
                                            .metaText()
                                            .monospacedDigit()
                                        Image(systemName: "chevron.right")
                                            .font(.system(size: 11, weight: .semibold))
                                            .foregroundStyle(Theme.inkTertiary)
                                    }
                                }
                            }
                        }
                    }
                }
                PageSection("Recent meetings") {
                    Card {
                        if detail.recentMeetings.isEmpty {
                            CardRow { Text("No linked meetings.").bodyText(15, Theme.inkSecondary) }
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
                                }
                            }
                        }
                    }
                }
                PageSection("Open loops") {
                    Card {
                        if detail.openLoops.isEmpty {
                            CardRow { Text("Nothing is still open.").bodyText(15, Theme.inkSecondary) }
                        } else {
                            ForEach(detail.openLoops) { loop in
                                PersonLoopRow(loop: loop, openMeeting: openMeeting)
                            }
                        }
                    }
                }
            } else {
                PageTitle(
                    "Organizations",
                    subtitle: store.organizationFailed ? "This organization could not be read" : "Loading…")
                ErrorNote(store.error)
                if store.organizationFailed {
                    Card {
                        CardRow {
                            Text("Couldn't load this organization.").bodyText(15, Theme.inkSecondary)
                        } trailing: {
                            Button("Retry") { store.reload() }.buttonStyle(.compact)
                        }
                    }
                }
            }
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

    /// How many times you met before this meeting, which is one fewer than the
    /// count including it.
    private func earlier(_ row: PersonMeetingContextRow) -> String {
        let count = row.meetingsTogether > 0 ? row.meetingsTogether - 1 : 0
        return count == 1 ? "1 earlier meeting" : "\(count) earlier meetings"
    }
}

// MARK: - Voice identity

/// The speakers in this meeting Sona cannot put a name to, and the way into
/// answering for them.
struct VoiceIdentitySection: View {
    let store: VoiceIdentityStore

    var body: some View {
        if !store.speakers.isEmpty {
            PageSection("Speakers") {
                Card {
                    if !store.unresolved.isEmpty {
                        CardRow {
                            Text(
                                store.unresolved.count == 1
                                    ? "1 speaker to label"
                                    : "\(store.unresolved.count) speakers to label"
                            )
                            .bodyText(15)
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
            .sheet(isPresented: asking) {
                VoiceIdentitySheet(store: store)
            }
        }
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

/// Who this speaker is. One command writes a label and a correction, so the
/// words on screen are the only thing that says which one was answered.
///
/// The speaker's own name is in the title, because the sheet reopens on the
/// next unnamed speaker without closing: three speakers to label is the same
/// question three times, and the name is what tells them apart.
struct VoiceIdentitySheet: View {
    let store: VoiceIdentityStore
    /// Empty means a person this answer creates.
    @State private var target: String = ""
    @State private var name: String = ""
    @State private var remember = false

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
    }

    @ViewBuilder
    private func form(_ people: [PersonListEntry]) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Choose a person").metaText(Theme.inkSecondary)
            Picker("", selection: $target) {
                Text("New person").tag("")
                ForEach(people) { entry in
                    Text(entry.person.displayName).tag(entry.person.id)
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
        }
        if target.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                Text("New person's name").metaText(Theme.inkSecondary)
                InputField(prompt: "Name", text: $name)
            }
        }
        Toggle(isOn: $remember) {
            Text("Remember this voice on this Mac").bodyText(14)
        }
        .toggleStyle(.checkbox)
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
        store.save(voiceTarget, remember: remember)
        target = ""
        name = ""
        remember = false
    }
}
