import Charts
import SwiftUI

// MARK: - Head

/// The head of a person's page, the way Contacts heads a card: the portrait,
/// the name, and what they do and where.
struct PersonHero: View {
    let store: PeopleStore
    let person: Person
    let contact: PersonContactCard?
    @Binding var renaming: Bool
    /// Opens the Contacts picker. Nil until the page has read the person.
    var chooseContact: (() -> Void)?

    var body: some View {
        HStack(alignment: .center, spacing: 20) {
            PersonPortrait(personId: person.id, name: person.displayName, size: 84, revision: store.photoRevision)
            VStack(alignment: .leading, spacing: 6) {
                if renaming {
                    PersonNameField(
                        name: person.displayName,
                        commit: { store.rename(to: $0) },
                        close: { renaming = false })
                } else {
                    Text(person.displayName)
                        .heroText()
                        .lineLimit(2)
                        .accessibilityAddTraits(.isHeader)
                        .pageHeadline()
                }
                role
            }
            Spacer(minLength: 16)
            if contact == nil, let chooseContact {
                Button(action: chooseContact) {
                    Label("Add contact", systemImage: "person.crop.circle.badge.plus")
                }
                .buttonStyle(.compact)
                .disabled(store.busy)
            }
        }
        .frame(minHeight: 84)
        .padding(.top, 8)
        .padding(.bottom, 24)
    }

    /// "Design lead · Sample studio". The organization is the one word here
    /// that goes somewhere: the companies list gives it a page.
    @ViewBuilder
    private var role: some View {
        let title = contact?.jobTitle
        let organization = person.organization ?? contact?.organization
        if title != nil || organization != nil {
            HStack(spacing: 6) {
                if let title {
                    Text(title).bodyText(14, Theme.inkSecondary)
                }
                if title != nil, organization != nil {
                    Text("·").bodyText(14, Theme.inkTertiary)
                }
                if let organization, person.organization != nil {
                    Button { store.openOrganization(organization) } label: {
                        HStack(spacing: 3) {
                            Text(organization).font(TypeScale.body(14))
                            Image(systemName: "chevron.right")
                                .font(.system(size: 9, weight: .semibold))
                                .accessibilityHidden(true)
                        }
                    }
                    .buttonStyle(.quiet)
                    .help("Open \(organization)")
                } else if let organization {
                    Text(organization).bodyText(14, Theme.inkSecondary)
                }
            }
            .lineLimit(1)
        }
    }
}

/// The head of a person's or a company's page before anything about it is
/// known: the same height, so nothing moves when the name lands.
struct PersonHeroPlaceholder: View {
    var square = false

    var body: some View {
        HStack(alignment: .center, spacing: 20) {
            RoundedRectangle(cornerRadius: square ? 84 * 0.24 : 42, style: .continuous)
                .fill(Theme.inset)
                .frame(width: 84, height: 84)
            VStack(alignment: .leading, spacing: 10) {
                PlaceholderBar(width: 220, height: 22)
                PlaceholderBar(width: 140, height: 12)
            }
        }
        .frame(minHeight: 84, alignment: .leading)
        .padding(.top, 8)
        .padding(.bottom, 24)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Loading")
    }
}

// MARK: - Numbers

/// What the tiles on a person's page say, worked out once from the reads
/// behind them.
struct PersonFigures: Equatable {
    /// A count on a tile: not read yet, unknown once read, or a number.
    enum Count: Equatable {
        case unread
        case unknown
        case value(Int)
    }

    var meetings: Int?
    var suggested = 0
    var since: Date?
    var months: [PersonMonthCount] = []
    var lastMet: Date?
    var lastMeetingId: String?
    var recentDays: [Bool] = []
    /// Their average share of the talking, in thousandths. Nil once read
    /// means it was never measured.
    var talkShare: UInt32?
    var recordedMs: UInt64?
    var outcomes: LedgerOutcomeCounts?
    var usualGapDays: Int?
    var openMine: Int?
    var openTheirs = 0
    var notes = Count.unread
    var tasks = Count.unread
    var words = Count.unread
    var documents = Count.unread

    /// Nothing read yet.
    init() {}

    @MainActor
    init(detail: PersonDetail, memory: PersonMemoryStore?, work: PersonWorkStore?, now: Date = .now) {
        let confirmed = PeopleModel.confirmed(detail.links)
        meetings = confirmed.count
        suggested = detail.links.count - confirmed.count
        since = confirmed.map(\.meeting.at).min()
        months = PeopleModel.cadence(detail.links, now: now)
        let last = confirmed.max { $0.meeting.at < $1.meeting.at }
        lastMet = last?.meeting.at
        lastMeetingId = last?.meeting.id
        recentDays = PeopleModel.recentDays(detail.links, now: now)
        talkShare = detail.talkShareAvgPermille
        recordedMs = detail.recordedMs
        outcomes = detail.ledgerOutcomes
        usualGapDays = PeopleModel.usualGapDays(detail.links)
        let open = detail.openLoops.filter(\.status.outstanding)
        let mine = open.filter { $0.direction == .mine }.count
        openMine = mine
        openTheirs = open.count - mine
        documents = .value(detail.documents.count)
        if let snapshot = memory?.snapshot {
            notes = .value(snapshot.notes.count)
            words = .value(snapshot.contributions.count)
        } else if memory?.error != nil {
            notes = .unknown
            words = .unknown
        }
        if let work, let snapshot = work.snapshot {
            tasks = snapshot.identityStatus == .available ? .value(work.tasks.filter(\.status.isOpen).count) : .unknown
        } else if work?.error != nil {
            tasks = .unknown
        }
    }
}

/// The person in numbers. Each tile opens the part of the page it counts.
struct PersonStats: View, Equatable {
    let figures: PersonFigures
    var show: (PersonPageTab, PersonHistoryFilter?) -> Void = { _, _ in }
    var openMeeting: (String) -> Void = { _ in }
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(spacing: 10) {
            HStack(spacing: 10) {
                meetings
                lastMet
                talkShare
                open
            }
            HStack(spacing: 10) {
                together
                landed
                count(figures.notes, label: "Notes") { show(.history, .notes) }
                count(figures.tasks, label: "Open tasks") { show(.work, nil) }
                count(figures.words, label: "Their words") { show(.history, .contributions) }
                count(figures.documents, label: "Documents") { show(.profile, nil) }
            }
        }
        .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: figures)
    }

    /// The tiles redraw when what they say changes. The closures only route
    /// a press to the page, and a new copy of one never changes a tile.
    nonisolated static func == (lhs: PersonStats, rhs: PersonStats) -> Bool {
        lhs.figures == rhs.figures
    }

    private var meetings: some View {
        WidgetTile(
            "Meetings", icon: "calendar",
            value: figures.meetings.map(String.init) ?? " ",
            caption: figures.suggested > 0
                ? "\(figures.suggested) to confirm"
                : figures.since.map { "Since \($0.formatted(.dateTime.month(.abbreviated).year()))" },
            action: figures.meetings == nil ? nil : { show(.history, .conversations) }
        ) {
            PersonMonthChart(months: figures.months)
        }
    }

    private var lastMet: some View {
        let since = figures.lastMet.map { PeopleFormat.since($0) }
        return WidgetTile(
            "Last met", icon: "clock",
            value: since?.value ?? (figures.meetings == nil ? " " : "—"),
            unit: since?.unit,
            caption: figures.usualGapDays.map { $0 == 1 ? "Usually every day" : "Usually every \($0) days" }
                ?? figures.lastMet.map { "\($0.short) \($0.time)" },
            tone: since == nil ? Theme.inkTertiary : Theme.ink,
            action: figures.lastMeetingId.map { id -> () -> Void in { openMeeting(id) } }
        ) {
            PersonRecentDays(days: figures.recentDays)
        }
    }

    private var talkShare: some View {
        WidgetTile(
            "Talk share", icon: "waveform",
            value: figures.talkShare.map(PeopleFormat.talkShare) ?? (figures.meetings == nil ? " " : "—"),
            caption: figures.talkShare == nil ? nil : "Average",
            tone: figures.talkShare == nil ? Theme.inkTertiary : Theme.ink,
            action: nil
        ) {
            if let share = figures.talkShare {
                ShareBar(fraction: Double(share) / 1000)
            }
        }
    }

    private var open: some View {
        let total = figures.openMine.map { $0 + figures.openTheirs }
        return WidgetTile(
            "Open", icon: "circle.dashed",
            value: total.map(String.init) ?? " ",
            caption: total.flatMap { $0 > 0 ? "\(figures.openMine ?? 0) yours · \(figures.openTheirs) theirs" : nil },
            action: total == nil ? nil : { show(.catchUp, nil) }
        ) {
            if let mine = figures.openMine, mine + figures.openTheirs > 0 {
                PersonSplitBar(mine: mine, theirs: figures.openTheirs)
            }
        }
    }

    private var together: some View {
        let time = figures.recordedMs.map(Self.timeTogether)
        return Button { show(.history, .conversations) } label: {
            NumberTile(
                time?.value ?? (figures.meetings == nil ? " " : "—"), label: "Together", unit: time?.unit,
                tone: time == nil ? Theme.inkTertiary : Theme.ink)
        }
        .buttonStyle(TilePress())
        .disabled(figures.meetings == nil)
        .help("Show conversations")
    }

    /// Threads that landed out of every substantive thread, with no action:
    /// no list of them exists across meetings to open. Meetings whose threads
    /// were all small talk have nothing to count, which is not zero landed.
    private var landed: some View {
        let total = figures.outcomes.map { $0.landed + $0.open + $0.dropped } ?? 0
        let outcomes = total > 0 ? figures.outcomes : nil
        return NumberTile(
            outcomes.map { "\($0.landed)" } ?? (figures.meetings == nil ? " " : "—"),
            label: "Landed", unit: outcomes == nil ? nil : "of \(total)",
            tone: outcomes == nil ? Theme.inkTertiary : Theme.ink)
    }

    /// "45 min", "2.5 h", "12 h".
    private static func timeTogether(_ ms: UInt64) -> (value: String, unit: String) {
        let minutes = Int((Double(ms) / 60_000).rounded())
        if minutes < 60 { return ("\(minutes)", "min") }
        let hours = Double(minutes) / 60
        return hours < 10 ? (hours.formatted(.number.precision(.fractionLength(1))), "h") : ("\(Int(hours.rounded()))", "h")
    }

    private func count(_ value: PersonFigures.Count, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            switch value {
            case .unread: NumberTile(" ", label: label)
            case .unknown: NumberTile("—", label: label, tone: Theme.inkTertiary)
            case let .value(count): NumberTile("\(count)", label: label)
            }
        }
        .buttonStyle(TilePress())
        .disabled(value == .unread)
        .help("Show \(label.lowercased())")
    }
}

/// A tile that opens what it counts: a wash under the pointer and while
/// pressed.
private struct TilePress: ButtonStyle {
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusCard)
        configuration.label
            .overlay(shape.fill(Theme.wash).opacity(configuration.isPressed || hovering ? 1 : 0))
            .contentShape(shape)
            .onHover { hovering = $0 }
            .hoverFade(hovering)
    }
}

/// Confirmed meetings in each of the last six months, oldest on the left.
/// The month letters sit in a row of their own under the bars: Charts warns
/// at runtime about the anchor its centered axis labels use.
struct PersonMonthChart: View {
    let months: [PersonMonthCount]

    var body: some View {
        let peak = months.map(\.count).max() ?? 0
        VStack(spacing: 3) {
            Chart(months) { month in
                BarMark(
                    x: .value("Month", month.start.formatted(.iso8601.year().month())),
                    y: .value("Meetings", month.count > 0 ? Double(month.count) : Double(max(peak, 1)) * 0.04),
                    width: .ratio(0.62))
                    .foregroundStyle(month.count > 0 ? Theme.accent : Theme.track)
                    .cornerRadius(Theme.radiusMark)
            }
            .chartXAxis(.hidden)
            .chartYAxis(.hidden)
            .chartYScale(domain: 0...Double(max(peak, 1)))
            HStack(spacing: 0) {
                ForEach(months) { month in
                    Text(month.start.formatted(.dateTime.month(.narrow)))
                        .frame(maxWidth: .infinity)
                }
            }
            .font(.system(size: 9, weight: .medium))
            .foregroundStyle(Theme.inkTertiary)
        }
        .frame(height: 40)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(months.map {
            "\($0.start.formatted(.dateTime.month(.wide))), \(PeopleModel.meetings(UInt64($0.count)))"
        }.joined(separator: "; "))
    }
}

/// The last fourteen days, one mark each, filled where you met.
struct PersonRecentDays: View {
    let days: [Bool]

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 3) {
                ForEach(days.indices, id: \.self) { index in
                    RoundedRectangle(cornerRadius: Theme.radiusMark, style: .continuous)
                        .fill(days[index] ? Theme.accent : Theme.track)
                        .frame(height: 12)
                }
            }
            if !days.isEmpty {
                HStack {
                    Text("2 weeks ago")
                    Spacer(minLength: 4)
                    Text("Today")
                }
                .font(.system(size: 9, weight: .medium))
                .foregroundStyle(Theme.inkTertiary)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Met on \(days.filter { $0 }.count) of the last \(days.count) days")
    }
}

/// What is still open, split by who owes it: yours first, in accent.
struct PersonSplitBar: View {
    let mine: Int
    let theirs: Int

    var body: some View {
        GeometryReader { proxy in
            let total = CGFloat(max(mine + theirs, 1))
            HStack(spacing: 2) {
                if mine > 0 {
                    Capsule().fill(Theme.accent).frame(width: max(4, (proxy.size.width - 2) * CGFloat(mine) / total))
                }
                if theirs > 0 {
                    Capsule().fill(Theme.inkDisabled.opacity(0.5))
                }
            }
        }
        .frame(height: 6)
        .accessibilityHidden(true)
    }
}

// MARK: - Contact

/// The contact attached to this person, laid out the way Contacts lays out a
/// card: each label quiet on the left, its value on the right.
struct PersonContactSection: View {
    let card: PersonContactCard
    let person: Person

    var body: some View {
        PageSection("Contact") {
            Card {
                Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 16, verticalSpacing: 12) {
                    ForEach(rows) { row in
                        GridRow {
                            Text(row.label)
                                .metaText()
                                .lineLimit(1)
                                .gridColumnAlignment(.trailing)
                            value(row)
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 14)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    @ViewBuilder
    private func value(_ row: Row) -> some View {
        if let link = row.link {
            Link(destination: link) {
                Text(row.value).font(TypeScale.body(14)).foregroundStyle(Theme.accent).lineLimit(1)
            }
            .help(row.value)
        } else {
            Text(row.value)
                .bodyText(14)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private struct Row: Identifiable {
        let id: String
        let label: String
        let value: String
        var link: URL?
    }

    private var rows: [Row] {
        var rows: [Row] = []
        if card.name != person.displayName {
            rows.append(Row(id: "name", label: "name", value: card.name))
        }
        for (index, email) in card.emails.enumerated() {
            rows.append(Row(id: "email\(index)", label: email.label ?? "email", value: email.value,
                            link: URL(string: "mailto:\(email.value)")))
        }
        for (index, phone) in card.phones.enumerated() {
            let dial = phone.value.filter { $0.isNumber || $0 == "+" }
            rows.append(Row(id: "phone\(index)", label: phone.label ?? "phone", value: phone.value,
                            link: dial.isEmpty ? nil : URL(string: "tel:\(dial)")))
        }
        if let organization = card.organization, person.organization != nil, organization != person.organization {
            rows.append(Row(id: "company", label: "company", value: organization))
        }
        for (index, address) in card.addresses.enumerated() {
            rows.append(Row(id: "address\(index)", label: address.label ?? "address", value: address.value))
        }
        for (index, url) in card.urls.enumerated() {
            rows.append(Row(id: "url\(index)", label: url.label ?? "homepage", value: url.value,
                            link: URL(string: url.value)))
        }
        if let birthday = card.birthday {
            rows.append(Row(id: "birthday", label: "birthday", value: birthday.text))
        }
        return rows
    }
}

// MARK: - Placeholders

/// A grey bar where text will be, while it is read.
struct PlaceholderBar: View {
    var width: CGFloat?
    var height: CGFloat = 10

    var body: some View {
        RoundedRectangle(cornerRadius: height / 2, style: .continuous)
            .fill(Theme.inset)
            .frame(width: width, height: height)
            .frame(maxWidth: width == nil ? .infinity : nil, alignment: .leading)
    }
}

/// Where rows will be, while they are read: card rows of the same height,
/// and no spinner.
struct PersonRowsPlaceholder: View {
    var rows = 3

    var body: some View {
        Card {
            ForEach(0..<rows, id: \.self) { index in
                VStack(alignment: .leading, spacing: 8) {
                    PlaceholderBar(width: index.isMultiple(of: 2) ? 360 : 280, height: 10)
                    PlaceholderBar(width: 160, height: 8)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 14)
                .frame(maxWidth: .infinity, alignment: .leading)
                .overlay(alignment: .bottom) { Hairline().padding(.horizontal, 16) }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Loading")
    }
}
