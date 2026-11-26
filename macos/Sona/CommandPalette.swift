import SwiftUI

/// One row of the palette that does something: a place to go or a verb.
struct PaletteAction: Identifiable {
    enum Group {
        case navigation, actions, work, quicklinks
    }

    let id: String
    let group: Group
    let title: String
    let run: @MainActor () -> Void
    var icon: String? = nil
    /// The key that does the same outside the palette, drawn as key caps.
    var shortcut: String? = nil
    var detail: String? = nil
}

/// A task, project, person or label whose name or number matches the field.
struct PaletteMatch: Identifiable {
    enum Kind {
        case task(WorkStatus)
        case project
        case person
        case label(Double)
    }

    let kind: Kind
    let id: String
    let title: String
    var meta: String?

    var key: String {
        switch kind {
        case .task: "task:\(id)"
        case .project: "project:\(id)"
        case .person: "person:\(id)"
        case .label: "label:\(id)"
        }
    }
}

/// What a keystroke in the field can land on, in the order the list shows them.
private enum PaletteRow: Identifiable {
    case action(PaletteAction)
    case hit(QueryRow)
    case match(PaletteMatch)
    /// A line someone said: return plays it, ⌘return opens its meeting there.
    case heard(SaidMoment)
    /// A meeting picture whose words match: it opens its meeting at the line
    /// said when it went up.
    case screen(ScreenTextMoment)
    /// Every heard line with its audio kept, oldest first, one after another.
    case playAll
    /// The next page of the same question, when the plane said there is one.
    case more
    case ask(String)
    /// A sentence that changes tasks, with the tasks it changes.
    case command(WorkCommand)
    /// A line that answers the question: return plays or opens it, ⇧ with an
    /// arrow marks it, ⌘return makes tasks of the marked lines.
    case answer(WorkAnswerLine)
    case watch(String)
    case unwatch(WorkWatch)
    case watching(WorkWatch)
    /// A section of Sona's own help that matches: return shows its text.
    case help(HelpSection)

    /// Which reel "Play all" is to the shared player.
    static let playAllKey = "palette"

    var id: String {
        switch self {
        case let .action(action): "action:\(action.id)"
        case let .hit(hit): "hit:\(hit.link)"
        case let .match(match): "match:\(match.key)"
        case let .heard(moment): "heard:\(moment.id)"
        case let .screen(moment): "screen:\(moment.id)"
        case .playAll: "play-all"
        case .more: "more"
        case .ask: "ask"
        case .command: "command"
        case let .answer(line): "answer:\(line.id)"
        case .watch: "watch"
        case let .unwatch(watch): "unwatch:\(watch.id)"
        case let .watching(watch): "watching:\(watch.id)"
        case let .help(section): "help:\(section.id)"
        }
    }
}

/// Where the shared player stands for one heard line.
private enum Listening {
    case idle
    case loading
    /// How far through the clip, from 0 to 1.
    case playing(Double)
    case failed(String)
}

/// What the plane has said about the text in the field. The rows shown are
/// always the answer to the question that is there now: an earlier answer
/// stays visible while the next is on its way, dimmed and unreachable, so
/// return can never open a row from a question no longer in the field.
private enum Lookup {
    /// Under two letters: the plane is not asked.
    case none
    /// Asked about the current text, not answered. `earlier` is the last
    /// answer, kept on screen so the list does not blink between keystrokes.
    case pending(earlier: [QueryRow])
    case answered(Answer)
    case failed(String)

    struct Answer {
        var rows: [QueryRow]
        var next: QueryCursor?
        var reason: QueryPageReason?
        /// The next page is on its way; the rows here stay live meanwhile.
        var loadingMore = false
        /// The next page did not arrive.
        var moreFailed = false
    }

    var earlier: [QueryRow] {
        switch self {
        case .none, .failed: []
        case let .pending(earlier): earlier
        case let .answered(answer): answer.rows
        }
    }
}

/// ⌘K. A floating panel: one field, the places and verbs that match, the
/// lines said in meetings that match, the meeting pictures whose words match,
/// then the meetings, tasks (those matched by their comments or across fields
/// the list above missed), people, dictations and loops the query plane found, and
/// the ask row when the agent is allowed to hear the question. The corpus
/// rows are never filtered by the letters typed: the plane matched them,
/// sometimes by meaning, and a title need not share one letter with the
/// question.
struct CommandPalette: View {
    @Environment(AppModel.self) private var model
    @State private var query = ""
    @State private var lookup = Lookup.none
    @State private var selection = 0
    @State private var search: Task<Void, Never>?
    /// The lines said that match the settled question; empty when none did
    /// or the search for them failed.
    @State private var heard: [SaidMoment] = []
    /// The meeting pictures whose words match the settled question.
    @State private var onScreen: [ScreenTextMoment] = []
    /// Sona's help sections that match the settled text, best first.
    @State private var help: [HelpSection] = []
    /// The help section open in its sheet.
    @State private var reading: HelpSection?
    /// The line this palette last asked to hear: its failure shows on that
    /// row, and closing the palette stops only that playback.
    @State private var hearing: TranscriptSegmentId?
    /// The settled text read as a change to tasks.
    @State private var command: WorkCommand?
    /// The settled text read as a question, when it reads as one.
    @State private var answer: WorkAnswer?
    /// Answer lines marked for ⌘return, in the order they were marked.
    @State private var picked: [WorkAnswerLine] = []
    /// Answer lines already made into tasks from this palette.
    @State private var taken: Set<String> = []
    @State private var listHeight: CGFloat = 0
    @FocusState private var focused: Bool

    /// A single letter matches half the corpus, so it is not a query yet.
    private static let minimumQuery = 2
    /// One page: the newest dozen that matched. Recency orders the plane.
    private static let limit: Int64 = 12
    /// The few lines whose words matched best: enough to find the moment,
    /// few enough that the meetings stay in view.
    private static let heardLimit = 4
    /// A few help sections, under the rows the corpus found.
    private static let helpLimit = 3
    private static let kinds: [(kind: QueryRowKind, label: String)] = [
        (.meeting, "Meetings"), (.task, "Tasks"), (.person, "People"), (.note, "Notes"),
        (.dictation, "Dictations"), (.loop, "Loops"),
    ]

    var body: some View {
        let panelShape = RoundedRectangle(cornerRadius: Theme.radiusDialog)
        ZStack(alignment: .top) {
            Theme.scrim
                .ignoresSafeArea()
                .onTapGesture { model.paletteShown = false }
            Group {
                if let picker = model.workFocus.picker {
                    WorkPickerList(picker: picker) { model.paletteShown = false }
                } else {
                    list
                }
            }
            .frame(width: 600)
            .background {
                if model.about.material == .glass {
                    panelShape.fill(Theme.surface)
                }
            }
            .clipShape(panelShape)
            .padding(model.about.material == .glass ? 6 : 0)
            .background {
                if model.about.material == .solid {
                    panelShape
                        .fill(Theme.elevated)
                        .overlay(panelShape.strokeBorder(Theme.border, lineWidth: 1))
                        .elevation(.high)
                } else if #available(macOS 26.0, *) {
                    Color.clear
                        .glassEffect(.regular, in: panelShape)
                } else {
                    panelShape
                        .fill(.regularMaterial)
                        .overlay(panelShape.strokeBorder(Theme.border, lineWidth: 1))
                        .elevation(.high)
                }
            }
            .padding(.top, 96)
            .sheet(item: $reading) { section in
                HelpSectionSheet(section: section) { reading = nil }
            }
            .onAppear {
                query = model.takePaletteSeed()
                DispatchQueue.main.async { focused = true }
            }
            .onExitCommand { model.paletteShown = false }
            .onChange(of: query) { _, text in
                selection = 0
                schedule(text)
            }
            .onDisappear {
                search?.cancel()
                let audio = model.meetings.audio
                if let hearing, audio.segmentId == hearing {
                    audio.stop()
                } else if audio.reelKey == PaletteRow.playAllKey {
                    audio.stop()
                }
            }
        }
    }

    private var list: some View {
        VStack(spacing: 0) {
            if let context = model.paletteContext {
                WorkContextChip(text: context)
                    .padding(.horizontal, 20)
                    .padding(.top, 14)
            }
            HStack(spacing: 10) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 15, weight: .regular))
                    .foregroundStyle(Theme.inkTertiary)
                TextField("", text: $query, prompt: Text("Search or ask").foregroundStyle(Theme.inkTertiary))
                    .textFieldStyle(.plain)
                    .font(TypeScale.body(17))
                    .foregroundStyle(Theme.ink)
                    .focused($focused)
                    .onSubmit { runSelected() }
                    .onKeyPress(.upArrow, phases: .down) { press in step(-1, marking: press.modifiers.contains(.shift)) }
                    .onKeyPress(.downArrow, phases: .down) { press in step(1, marking: press.modifiers.contains(.shift)) }
                    .onKeyPress(.return, phases: .down) { press in
                        press.modifiers.contains(.command) ? openSelected() : .ignored
                    }
                if case .pending = lookup {
                    ProgressView().controlSize(.small)
                }
            }
            .padding(.horizontal, 20)
            .frame(height: 56)
            Hairline()
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 2) {
                        ForEach(Array(numberedSections.enumerated()), id: \.offset) { _, section in
                            Text(section.label)
                                .font(.system(size: 12, weight: .semibold))
                                .foregroundStyle(Theme.inkTertiary)
                                .accessibilityAddTraits(.isHeader)
                                .padding(.horizontal, 20)
                                .padding(.top, 12)
                                .padding(.bottom, 4)
                            ForEach(section.rows, id: \.row.id) { index, row in
                                PaletteRowView(
                                    row: row, selected: index == selection, marks: marks, asked: hearing,
                                    picked: picked.contains { "answer:\($0.id)" == row.id },
                                    taken: taken.contains { "answer:\($0)" == row.id }
                                ) {
                                    selection = index
                                    run(row)
                                } open: {
                                    open(row)
                                }
                                .id(row.id)
                                .opacity(section.live ? 1 : 0.4)
                                .allowsHitTesting(section.live)
                            }
                        }
                        footnote
                    }
                    .padding(.bottom, 8)
                    .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { listHeight = $0 }
                }
                .scrollIndicators(.never)
                .frame(height: min(listHeight, 420))
                .onChange(of: selection) { _, index in
                    if rows.indices.contains(index) {
                        proxy.scrollTo(rows[index].id)
                    }
                }
            }
            Hairline()
            keys
        }
    }

    private var trimmed: String { query.trimmingCharacters(in: .whitespaces) }

    /// The words of the query as the spoken-line search splits them, for
    /// marking in the lines it found; for an answer, only the words it read.
    private var marks: [String] {
        (answer?.readAs.joined(separator: " ") ?? trimmed).split { !$0.isLetter && !$0.isNumber }.map(String.init)
    }

    private var actions: [PaletteAction] {
        let all = model.paletteActions
        guard !trimmed.isEmpty else { return all }
        return all.filter { $0.title.localizedCaseInsensitiveContains(trimmed) }
    }

    /// The sections in the order the list shows them: places, verbs, the lines
    /// said that match, the pictures whose words match, then the plane's rows
    /// by kind, then the one ask row. A section is live when its rows answer
    /// the question in the field now.
    private var sections: [(label: String, rows: [PaletteRow], live: Bool)] {
        var sections: [(label: String, rows: [PaletteRow], live: Bool)] = []
        let task = actions.filter { $0.group == .work }.map(PaletteRow.action)
        let navigation = actions.filter { $0.group == .navigation }.map(PaletteRow.action)
        let verbs = actions.filter { $0.group == .actions }.map(PaletteRow.action)
        let links = trimmed.isEmpty ? [] : actions.filter { $0.group == .quicklinks }.map(PaletteRow.action)
        if !task.isEmpty { sections.append((model.workTargets.count > 1 ? "Tasks" : "Task", task, true)) }
        let matches = model.paletteMatches(trimmed)
        for (label, picked) in [
            ("Tasks", matches.filter { if case .task = $0.kind { true } else { false } }),
            ("Projects", matches.filter { if case .project = $0.kind { true } else { false } }),
            ("People", matches.filter { if case .person = $0.kind { true } else { false } }),
            ("Labels", matches.filter { if case .label = $0.kind { true } else { false } }),
        ] where !picked.isEmpty {
            sections.append((label, picked.map(PaletteRow.match), true))
        }
        let named = Set(matches.compactMap { if case .person = $0.kind { $0.title } else { nil } })
        let listed = Set(matches.compactMap { if case .task = $0.kind { $0.id } else { nil } })
        let settled = if case .pending = lookup { false } else { true }
        if let command, !command.items.isEmpty {
            sections.append(("Change", [.command(command)], settled))
        }
        let watching = watchRows
        if !watching.isEmpty { sections.append(("Watch", watching, true)) }
        if !navigation.isEmpty { sections.append(("Navigation", navigation, true)) }
        if !verbs.isEmpty { sections.append(("Actions", verbs, true)) }
        if !links.isEmpty { sections.append(("Quicklinks", links, true)) }
        let hits = lookup.earlier.filter {
            switch $0.kind {
            case .person: !named.contains($0.title)
            case .task: !listed.contains($0.id)
            default: true
            }
        }
        let live = settled
        if let answer, !answer.lines.isEmpty {
            sections.append(("Answer", answer.lines.map(PaletteRow.answer), live))
        } else if !heard.isEmpty {
            let playable = heard.filter(\.audioRetained).count
            let lines = heard.map(PaletteRow.heard)
            sections.append(("Heard", playable > 1 ? lines + [.playAll] : lines, live))
        }
        if !onScreen.isEmpty {
            sections.append(("On screen", onScreen.map(PaletteRow.screen), live))
        }
        for (kind, label) in Self.kinds {
            let rows = hits.filter { $0.kind == kind }.map(PaletteRow.hit)
            if !rows.isEmpty { sections.append((kind == .task && !listed.isEmpty ? "More tasks" : label, rows, live)) }
        }
        if !help.isEmpty {
            sections.append(("Help", help.map(PaletteRow.help), live))
        }
        if case let .answered(answer) = lookup, answer.next != nil, !answer.loadingMore, !answer.moreFailed {
            sections.append(("More", [.more], true))
        }
        if model.canAsk, !trimmed.isEmpty {
            sections.append(("Ask", [.ask(trimmed)], true))
        }
        return sections
    }

    /// The sections, each row with its place among the rows a keystroke can
    /// land on (-1 for a row that is not live), read once per draw.
    private var numberedSections: [(label: String, rows: [(index: Int, row: PaletteRow)], live: Bool)] {
        var next = 0
        return sections.map { section in
            let rows = section.rows.map { row -> (index: Int, row: PaletteRow) in
                guard section.live else { return (-1, row) }
                defer { next += 1 }
                return (next, row)
            }
            return (section.label, rows, section.live)
        }
    }

    /// The rows a keystroke can land on.
    private var rows: [PaletteRow] { sections.filter(\.live).flatMap(\.rows) }

    /// Under the rows: what the plane could not do, or the one sentence a
    /// settled search with nothing to show is allowed. Never a verdict while
    /// the plane is still being asked.
    @ViewBuilder
    private var footnote: some View {
        switch lookup {
        case .none, .pending:
            EmptyView()
        case let .failed(reason):
            note(reason, retry: "Try again") { schedule(query, now: true) }
        case let .answered(answer):
            if answer.moreFailed {
                note("The next page did not arrive.", retry: "Try again") { loadMore() }
            } else if answer.loadingMore {
                note("Loading more…")
            }
            if answer.reason == .semanticUnavailable {
                note(
                    "Only exact words matched: the meaning model is not on this Mac yet.",
                    retry: "Search again"
                ) { schedule(query, now: true) }
            } else if answer.rows.isEmpty, rows.isEmpty {
                note("Nothing matched “\(trimmed)”.")
            }
        }
    }

    private func note(_ text: String, retry: String? = nil, action: @escaping () -> Void = {}) -> some View {
        HStack(spacing: 12) {
            Text(text).metaText()
            if let retry {
                Spacer(minLength: 0)
                Button(retry, action: action).buttonStyle(.compact)
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    /// The keys the palette answers to, as the footer of the panel.
    private var keys: some View {
        HStack(spacing: 6) {
            Spacer(minLength: 0)
            KeyCap("↑")
            KeyCap("↓")
            Text("Select").metaText(Theme.inkSecondary).padding(.trailing, 10)
            if selectedLine != nil {
                KeyCap("⇧")
                KeyCap("↓")
                Text("Mark").metaText(Theme.inkSecondary).padding(.trailing, 10)
            }
            KeyCap("↩")
            Text(returnVerb).metaText(Theme.inkSecondary).padding(.trailing, 10)
            if selectedMoment?.audioRetained == true {
                KeyCap("⌘")
                KeyCap("↩")
                Text("Open").metaText(Theme.inkSecondary).padding(.trailing, 10)
            } else if selectedLine != nil {
                KeyCap("⌘")
                KeyCap("↩")
                Text(picked.count > 1 ? "Make \(picked.count) tasks" : "Make task")
                    .metaText(Theme.inkSecondary).padding(.trailing, 10)
            }
            KeyCap("esc")
            Text("Close").metaText(Theme.inkSecondary)
        }
        .padding(.horizontal, 16)
        .frame(height: 40)
        .background(Theme.inset)
        .accessibilityElement(children: .combine)
    }

    private func move(_ delta: Int) {
        guard !rows.isEmpty else { return }
        selection = (selection + delta + rows.count) % rows.count
    }

    /// An arrow; with shift, the answer line under the selection is marked
    /// or unmarked first.
    private func step(_ delta: Int, marking: Bool) -> KeyPress.Result {
        if marking, let line = selectedLine, !taken.contains(line.id) {
            if let index = picked.firstIndex(of: line) {
                picked.remove(at: index)
            } else {
                picked.append(line)
            }
        }
        move(delta)
        return .handled
    }

    /// The answer line under the selection, when that is where it is.
    private var selectedLine: WorkAnswerLine? {
        guard rows.indices.contains(selection), case let .answer(line) = rows[selection] else { return nil }
        return line
    }

    /// "watch pricing" offers the watch; "unwatch pricing" or "stop watching
    /// pricing" the ones it names; anything starting "watch" lists them.
    private var watchRows: [PaletteRow] {
        let lowered = trimmed.lowercased()
        let watches = model.work.watches
        for lead in ["unwatch ", "stop watching "] where lowered.hasPrefix(lead) {
            let phrase = Self.phrase(after: lead, in: trimmed)
            return watches.filter { phrase.isEmpty || $0.phrase.localizedCaseInsensitiveContains(phrase) }
                .map(PaletteRow.unwatch)
        }
        guard lowered.hasPrefix("watch") else { return [] }
        let phrase = lowered.hasPrefix("watch ") ? Self.phrase(after: "watch ", in: trimmed) : ""
        let known = watches.first { $0.personId == nil && $0.phrase.caseInsensitiveCompare(phrase) == .orderedSame }
        let offer: [PaletteRow] = phrase.isEmpty || known != nil ? [] : [.watch(phrase)]
        let listed = watches.filter { phrase.isEmpty || $0.phrase.localizedCaseInsensitiveContains(phrase) }
        return offer + listed.map(PaletteRow.watching)
    }

    private static func phrase(after lead: String, in text: String) -> String {
        String(text.dropFirst(lead.count))
            .trimmingCharacters(in: CharacterSet(charactersIn: " \"'“”‘’"))
    }

    /// Asked as a question: a question word first, or a question mark last.
    private static func asks(_ text: String) -> Bool {
        let first = text.split(separator: " ").first.map { $0.lowercased() } ?? ""
        return text.hasSuffix("?") || questionWords.contains(first)
    }

    private static let questionWords: Set<String> = [
        "what", "what's", "whats", "who", "who's", "when", "where", "why", "how", "which", "did", "does", "do",
        "is", "are", "was", "were", "has", "have", "had", "can", "could", "should", "will", "would",
    ]

    /// ⌘return on answer lines: the marked ones, or the one under the
    /// selection, become Triage tasks. They read as made at once; one the
    /// core turns down comes back.
    private func takeLines() -> KeyPress.Result {
        guard let current = selectedLine else { return .ignored }
        let lines = (picked.isEmpty ? [current] : picked).filter { !taken.contains($0.id) }
        picked = []
        guard !lines.isEmpty else { return .handled }
        taken.formUnion(lines.map(\.id))
        let work = model.work
        Task {
            let made = await work.take(lines)
            if made.isEmpty { taken.subtract(lines.map(\.id)) }
        }
        return .handled
    }

    private func play(_ line: WorkAnswerLine) {
        guard let play = line.play else {
            open(.answer(line))
            return
        }
        let audio = model.meetings.audio
        if audio.segmentId == play.segmentId, audio.loading || audio.playing {
            audio.stop()
            return
        }
        hearing = play.segmentId
        audio.play(sessionId: play.sessionId, segmentId: play.segmentId, startOffsetNs: play.startOffsetNs)
    }

    private func runSelected() {
        guard rows.indices.contains(selection) else { return }
        run(rows[selection])
    }

    private func run(_ row: PaletteRow) {
        if case .more = row {
            loadMore()
            return
        }
        if case let .help(section) = row {
            reading = section
            return
        }
        if case .playAll = row {
            playAll()
            return
        }
        if case let .heard(moment) = row, moment.audioRetained {
            hear(moment)
            return
        }
        if case let .answer(line) = row {
            play(line)
            return
        }
        open(row)
    }

    /// Closes the palette and goes where the row points: a heard line opens
    /// its meeting at the moment it was said.
    private func open(_ row: PaletteRow) {
        if case let .help(section) = row {
            reading = section
            return
        }
        model.paletteShown = false
        switch row {
        case let .action(action): action.run()
        case let .hit(hit): model.open(link: hit.link)
        case let .match(match): model.open(match)
        case let .heard(moment): model.open(link: moment.link)
        case let .screen(moment): model.open(link: moment.link)
        case let .ask(question): model.ask(question)
        case let .command(command): model.work.apply(command)
        case let .answer(line): model.open(link: line.link)
        case let .watch(phrase): model.work.watch(phrase)
        case let .unwatch(watch): model.work.unwatch(watch.id)
        case let .watching(watch):
            if let latest = watch.hits.first { model.open(link: latest.line.link) }
        case .more, .playAll, .help: break
        }
    }

    /// ⌘return on a heard line: its meeting, at the moment it was said.
    private func openSelected() -> KeyPress.Result {
        if selectedLine != nil { return takeLines() }
        guard let selectedMoment else { return .ignored }
        open(.heard(selectedMoment))
        return .handled
    }

    /// Return on a heard line whose audio is kept: plays it from the moment
    /// it was said, or stops it when it is the line already playing. The
    /// palette stays open, so the next line is one arrow away.
    private func hear(_ moment: SaidMoment) {
        if sounding(moment) {
            model.meetings.audio.stop()
            return
        }
        hearing = moment.segmentId
        model.meetings.audio.play(
            sessionId: moment.sessionId, segmentId: moment.segmentId, startOffsetNs: moment.startOffsetNs)
    }

    /// "Play all": every heard line with its audio kept, oldest first, each
    /// the way its own row plays it. Pressed again, it stops.
    private func playAll() {
        let audio = model.meetings.audio
        if audio.reelKey == PaletteRow.playAllKey {
            audio.stop()
            return
        }
        hearing = nil
        let clips = heard
            .sorted { ($0.meetingStartedAtUtcMs, $0.startOffsetNs) < ($1.meetingStartedAtUtcMs, $1.startOffsetNs) }
            .map { ReelClip(said: $0, startOffsetNs: $0.startOffsetNs) }
        audio.playReel(clips, key: PaletteRow.playAllKey)
    }

    /// The line is the one the shared player is loading or playing now.
    private func sounding(_ moment: SaidMoment) -> Bool {
        let audio = model.meetings.audio
        return audio.segmentId == moment.segmentId && (audio.loading || audio.playing)
    }

    /// The heard line under the selection, when that is where it is.
    private var selectedMoment: SaidMoment? {
        guard rows.indices.contains(selection), case let .heard(moment) = rows[selection] else { return nil }
        return moment
    }

    /// What return does to the selected row: a heard line with its audio kept
    /// plays or stops, as does "Play all"; every other row opens.
    private var returnVerb: String {
        if rows.indices.contains(selection) {
            switch rows[selection] {
            case .playAll:
                return model.meetings.audio.reelKey == PaletteRow.playAllKey ? "Stop" : "Play"
            case .command: return "Apply"
            case .watch: return "Watch"
            case .unwatch: return "Stop watching"
            case let .answer(line):
                guard let play = line.play else { return "Open" }
                let audio = model.meetings.audio
                return audio.segmentId == play.segmentId && (audio.loading || audio.playing) ? "Stop" : "Play"
            default: break
            }
        }
        guard let selectedMoment, selectedMoment.audioRetained else { return "Open" }
        return sounding(selectedMoment) ? "Stop" : "Play"
    }

    /// Long enough to swallow a typed word, short enough to feel like the list
    /// is keeping up: 150 ms after the last keystroke, one page from the plane,
    /// the few lines said that match and the few pictures whose words match,
    /// asked together. `now` skips the pause for a retry the reader asked for
    /// by hand.
    private func schedule(_ text: String, now: Bool = false) {
        search?.cancel()
        let query = text.trimmingCharacters(in: .whitespaces)
        guard query.count >= Self.minimumQuery else {
            lookup = .none
            heard = []
            onScreen = []
            help = []
            command = nil
            answer = nil
            picked = []
            return
        }
        lookup = .pending(earlier: lookup.earlier)
        let core = model.core
        let work = model.work
        let request: [String: JSONValue] = ["query": .string(query), "limit": .number(Double(Self.heardLimit))]
        search = Task {
            if !now {
                try? await Task.sleep(for: .milliseconds(150))
            }
            guard !Task.isCancelled else { return }
            async let said: [SaidMoment] = core.request("meeting_said_search", request)
            async let shown: [ScreenTextMoment] = core.request("meeting_snapshot_text_search", request)
            async let change = work.command(query)
            async let answered: WorkAnswer? = Self.asks(query) ? try? work.ask(query) : nil
            async let helped: [HelpSection] = core.request(
                "help_search",
                ["words": .string(query), "limit": .number(Double(Self.helpLimit))] as [String: JSONValue])
            do {
                let page = try await ask(query, after: nil)
                let moments = (try? await said) ?? []
                let pictures = (try? await shown) ?? []
                let sections = (try? await helped) ?? []
                let read = await change
                let lines = await answered
                guard !Task.isCancelled else { return }
                heard = moments
                onScreen = pictures
                help = sections
                settle(read, lines)
                lookup = .answered(.init(rows: page.entries, next: page.nextCursor, reason: page.reason))
            } catch {
                let moments = (try? await said) ?? []
                let pictures = (try? await shown) ?? []
                let sections = (try? await helped) ?? []
                let read = await change
                let lines = await answered
                guard !Task.isCancelled else { return }
                heard = moments
                onScreen = pictures
                help = sections
                settle(read, lines)
                lookup = .failed(Self.sentence(for: error))
            }
        }
    }

    /// The change and the answer for the settled text. Marks on lines the
    /// new answer still has stay.
    private func settle(_ read: WorkCommand?, _ lines: WorkAnswer?) {
        command = read
        answer = lines
        picked = picked.filter { line in lines?.lines.contains(line) ?? false }
    }

    /// The next page of the same question, appended under the rows already
    /// here. A cursor the corpus no longer knows starts the question over.
    private func loadMore() {
        guard case let .answered(answer) = lookup, let next = answer.next, !answer.loadingMore else { return }
        var waiting = answer
        waiting.loadingMore = true
        waiting.moreFailed = false
        lookup = .answered(waiting)
        let query = trimmed
        search = Task {
            do {
                let page = try await ask(query, after: next)
                guard !Task.isCancelled else { return }
                var grown = answer
                grown.rows += page.entries
                grown.next = page.nextCursor
                grown.reason = page.reason ?? answer.reason
                lookup = .answered(grown)
            } catch {
                guard !Task.isCancelled else { return }
                if let failure = error as? CoreError, failure.remote(as: QueryFailure.self) == .unknownCursor {
                    schedule(query, now: true)
                    return
                }
                var failed = answer
                failed.moreFailed = true
                lookup = .answered(failed)
            }
        }
    }

    private func ask(_ query: String, after cursor: QueryCursor?) async throws -> QuerySearchPage {
        try await model.core.request(
            "sona_query_search",
            [
                "scope": "all",
                "query": .string(query),
                "limit": .number(Double(Self.limit)),
                "cursor": try cursor.map { try JSONValue($0) } ?? .null,
            ] as [String: JSONValue])
    }

    private static func sentence(for error: Error) -> String {
        if let failure = error as? CoreError, let code = failure.remote(as: QueryFailure.self) {
            return code.sentence
        }
        return "Search is unavailable right now."
    }
}

private struct PaletteRowView: View {
    let row: PaletteRow
    let selected: Bool
    /// The query's words, marked where a heard line has a word they start.
    var marks: [String] = []
    /// The line the palette last asked to hear: a failure shows on that row.
    var asked: TranscriptSegmentId?
    /// An answer line marked for ⌘return.
    var picked = false
    /// An answer line already made into a task.
    var taken = false
    let action: () -> Void
    /// A heard line's meeting at that moment, for VoiceOver.
    var open: () -> Void = {}
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var hovering = false

    var body: some View {
        let button = Button(action: action) {
            HStack(spacing: 12) {
                leading
                switch row {
                case let .action(action):
                    HStack(spacing: 8) {
                        Text(action.title).bodyText().lineLimit(1)
                        if let detail = action.detail {
                            Text(detail).metaText().lineLimit(1).layoutPriority(-1)
                        }
                    }
                case let .hit(hit):
                    VStack(alignment: .leading, spacing: 2) {
                        Text(hit.title).bodyText().lineLimit(1)
                        HStack(spacing: 6) {
                            Text(hit.when.relativeDay)
                            if !hit.snippet.isEmpty {
                                Text("·")
                                Text(hit.snippet).lineLimit(1)
                            }
                        }
                        .metaText()
                    }
                case let .match(match):
                    HStack(spacing: 8) {
                        Text(match.title).bodyText().lineLimit(1).truncationMode(.tail)
                        if let meta = match.meta {
                            Text(meta).metaText().lineLimit(1).layoutPriority(-1)
                        }
                    }
                    .help(match.title)
                case let .heard(moment):
                    VStack(alignment: .leading, spacing: 3) {
                        Text(quote(moment.text)).bodyText().lineLimit(2)
                        progress
                        Text(meta(moment)).metaText().lineLimit(1)
                    }
                case let .screen(moment):
                    VStack(alignment: .leading, spacing: 3) {
                        Text(marked(moment.excerpt.replacingOccurrences(of: "\n", with: " · "))).bodyText().lineLimit(2)
                        Text(meta(moment)).metaText().lineLimit(1)
                    }
                case .playAll:
                    Text("Play all, oldest first").bodyText()
                case .more:
                    Text("Show more results").bodyText(14, Theme.inkSecondary)
                case let .ask(question):
                    Text("Ask Sona: \(question)").bodyText()
                case let .command(command):
                    PaletteCommandPreview(command: command)
                case let .answer(line):
                    VStack(alignment: .leading, spacing: 3) {
                        Text(quote(line.quote)).bodyText().lineLimit(2)
                        Text(PaletteCommandPreview.meta(line)).metaText().lineLimit(1)
                    }
                case let .watch(phrase):
                    Text("Watch “\(phrase)”").bodyText()
                case let .unwatch(watch):
                    Text("Stop watching “\(watch.phrase)”").bodyText()
                case let .watching(watch):
                    PaletteWatchSummary(watch: watch)
                case let .help(section):
                    VStack(alignment: .leading, spacing: 2) {
                        Text(section.title).bodyText().lineLimit(1)
                        Text(section.text.replacingOccurrences(of: "\n", with: " · ")).metaText().lineLimit(1)
                    }
                }
                Spacer(minLength: 0)
                if case let .heard(moment) = row {
                    state(moment)
                } else if case .playAll = row {
                    if model.meetings.audio.reelKey == PaletteRow.playAllKey {
                        glyph("stop.fill")
                    } else if model.meetings.audio.failedReelKey == PaletteRow.playAllKey,
                              let error = model.meetings.audio.error {
                        Text(error)
                            .metaText(error == ConversationAudioPlayback.heldNotice ? Theme.inkSecondary : Theme.live)
                            .multilineTextAlignment(.trailing)
                            .lineLimit(2)
                            .frame(maxWidth: 180, alignment: .trailing)
                    }
                } else if case let .action(action) = row, let shortcut = action.shortcut {
                    Shortcut(shortcut)
                } else if case let .answer(line) = row {
                    if taken {
                        Text("In Triage").metaText()
                    } else if line.play != nil {
                        glyph("play.fill")
                    }
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(minHeight: 38)
            .background(fill, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .padding(.horizontal, 8)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        if case let .heard(moment) = row {
            button
                .accessibilityLabel(spoken(moment))
                .accessibilityValue(status(moment))
                .accessibilityAction(named: "Open meeting", open)
        } else {
            button
        }
    }

    /// Where the shared player stands for this row's line. Read here rather
    /// than in the palette, so a playing clip's ticks redraw one row.
    private var listening: Listening {
        guard case let .heard(moment) = row else { return .idle }
        let audio = model.meetings.audio
        if audio.segmentId == moment.segmentId {
            if audio.loading { return .loading }
            if audio.playing { return .playing(audio.duration > 0 ? audio.position / audio.duration : 0) }
            return .idle
        }
        if asked == moment.segmentId, audio.sessionId == nil, let error = audio.error {
            return .failed(error)
        }
        return .idle
    }

    /// The row's picture: a heard line shows the face of the person it was
    /// confirmed as, and only that.
    @ViewBuilder
    private var leading: some View {
        if case .answer = row {
            Image(systemName: picked || taken ? "checkmark.circle.fill" : "circle")
                .font(.system(size: 14, weight: .regular))
                .foregroundStyle(picked || taken ? Theme.accent : Theme.inkTertiary)
                .frame(width: 20)
                .accessibilityHidden(true)
        } else if case let .heard(moment) = row {
            if let person = moment.person {
                PersonPortrait(
                    personId: person.personId, name: person.displayName, size: 28, revision: model.people.photoRevision)
            } else {
                Image(systemName: icon)
                    .font(.system(size: 12, weight: .regular))
                    .foregroundStyle(Theme.inkSecondary)
                    .frame(width: 28, height: 28)
                    .background(Theme.inset, in: Circle())
                    .accessibilityHidden(true)
            }
        } else if case let .match(match) = row {
            switch match.kind {
            case let .task(status):
                TaskStatusIcon(status: status).frame(width: 20)
            case .person:
                Avatar(face: AvatarFace(kind: .person(match.id), name: match.title), size: 18,
                       revision: model.people.photoRevision)
                    .frame(width: 20)
            case let .label(hue):
                Circle().fill(Theme.label(hue: hue)).frame(width: 8, height: 8).frame(width: 20)
            case .project:
                Image(systemName: "folder")
                    .font(.system(size: 14, weight: .regular))
                    .foregroundStyle(Theme.inkSecondary)
                    .frame(width: 20)
            }
        } else {
            Image(systemName: icon)
                .font(.system(size: 14, weight: .regular))
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: 20)
                .accessibilityHidden(true)
        }
    }

    /// How far the playing line has got, under its words. The track stays in
    /// place, empty, while the line is not playing, so the row keeps its height.
    private var progress: some View {
        let played: Double? = if case let .playing(fraction) = listening { min(max(fraction, 0), 1) } else { nil }
        return Capsule()
            .fill(Theme.hairline)
            .overlay(alignment: .leading) {
                GeometryReader { proxy in
                    Capsule().fill(Theme.accent).frame(width: proxy.size.width * (played ?? 0))
                }
            }
            .frame(height: 2)
            .opacity(played == nil ? 0 : 1)
            .animation(reduceMotion ? nil : .linear(duration: 0.1), value: played)
            .accessibilityHidden(true)
    }

    /// The end of a heard line: what return will do, or what came of the
    /// last try.
    @ViewBuilder
    private func state(_ moment: SaidMoment) -> some View {
        if !moment.audioRetained {
            Text("Audio not kept").metaText()
        } else {
            switch listening {
            case .idle:
                glyph("play.fill")
            case .loading:
                ProgressView().controlSize(.small).frame(width: 20)
            case .playing:
                glyph("stop.fill")
            case let .failed(reason):
                Text(reason)
                    .metaText(Theme.live)
                    .multilineTextAlignment(.trailing)
                    .lineLimit(2)
                    .frame(maxWidth: 180, alignment: .trailing)
            }
        }
    }

    private func glyph(_ name: String) -> some View {
        Image(systemName: name)
            .font(.system(size: 11, weight: .regular))
            .foregroundStyle(Theme.inkSecondary)
            .frame(width: 20)
            .accessibilityHidden(true)
    }

    /// The line in quotes, each word the query starts marked the way the
    /// transcript marks a search: the line matched by word prefix, ignoring
    /// case and accents.
    private func quote(_ text: String) -> AttributedString {
        marked("“\(text)”")
    }

    private func marked(_ text: String) -> AttributedString {
        var line = AttributedString(text)
        for mark in marks {
            var cursor = line.startIndex
            while cursor < line.endIndex,
                  let found = line[cursor...].range(of: mark, options: [.caseInsensitive, .diacriticInsensitive]) {
                if line.characters[..<found.lowerBound].last.map({ !$0.isLetter && !$0.isNumber }) ?? true {
                    line[found].backgroundColor = Theme.accentSoft
                    line[found].foregroundColor = Theme.accent
                }
                cursor = found.upperBound
            }
        }
        return line
    }

    /// Who said it, in which meeting, on which day.
    private func meta(_ moment: SaidMoment) -> String {
        [moment.speaker, moment.meetingTitle, moment.meetingStarted.relativeDay]
            .compactMap { $0 }
            .filter { !$0.isEmpty }
            .joined(separator: " · ")
    }

    /// The picture's title, unless the words shown already start with it,
    /// then its meeting and day.
    private func meta(_ moment: ScreenTextMoment) -> String {
        let title = moment.excerpt.hasPrefix(moment.title) ? "" : moment.title
        return [title, moment.meetingTitle, moment.meetingStarted.relativeDay]
            .filter { !$0.isEmpty }
            .joined(separator: " · ")
    }

    private func spoken(_ moment: SaidMoment) -> String {
        let said = moment.speaker.map { "\($0) said: \(moment.text)" } ?? moment.text
        return [said, moment.meetingTitle, moment.meetingStarted.relativeDay]
            .filter { !$0.isEmpty }
            .joined(separator: ", ")
    }

    private func status(_ moment: SaidMoment) -> String {
        guard moment.audioRetained else { return "Audio not kept" }
        switch listening {
        case .idle: return ""
        case .loading: return "Loading"
        case .playing: return "Playing"
        case let .failed(reason): return reason
        }
    }

    private var fill: Color {
        if selected { return Theme.controlHover }
        return hovering ? Theme.wash : .clear
    }

    private var icon: String {
        switch row {
        case let .action(action):
            switch action.id {
            case "go-settings": "gearshape"
            case "go-settings-modes": "slider.horizontal.3"
            case "go-settings-vocabulary": "character.book.closed"
            case "go-settings-models": "cpu"
            case "toggle-recording": "mic"
            case "record-meeting": "video"
            case "record-screen": "rectangle.dashed.badge.record"
            case "open-chat": "bubble.left"
            case "import-audio": "square.and.arrow.down"
            default: action.icon ?? Place.allCases.first { "go-\($0.rawValue)" == action.id }?.icon ?? "arrow.right"
            }
        case let .hit(hit):
            switch hit.kind {
            case .meeting: "video"
            case .dictation: "waveform"
            case .note: "square.and.pencil"
            case .task: "checkmark.circle"
            case .person: "person"
            case .series: "calendar"
            case .loop: "arrow.triangle.2.circlepath"
            case .receipt: "doc.text"
            }
        case .match: "arrow.right"
        case .heard: "waveform"
        case .screen: "photo"
        case .playAll: "play.circle"
        case .more: "ellipsis"
        case .ask: "bubble.left"
        case .command: "arrow.right.circle"
        case .answer: "text.quote"
        case .watch, .watching: "eye"
        case .unwatch: "eye.slash"
        case .help: "questionmark.circle"
        }
    }
}

/// One section of Sona's help, as `help_search` answers it.
private struct HelpSection: Decodable, Identifiable {
    /// "guide" for the README, "shortcuts" for the shortcut sheet.
    let source: String
    let title: String
    let text: String

    var id: String { "\(source):\(title)" }
}

/// The text of one help section the palette found.
private struct HelpSectionSheet: View {
    let section: HelpSection
    let done: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(section.title)
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(Theme.ink)
                    Text(section.source == "shortcuts" ? "Keyboard shortcuts" : "Sona guide").metaText()
                }
                Spacer(minLength: 0)
                Button("Done", action: done)
                    .buttonStyle(.quiet)
                    .keyboardShortcut(.cancelAction)
            }
            .padding(.horizontal, 20)
            .frame(height: 60)
            Hairline()
            ScrollView {
                Text(section.text)
                    .font(TypeScale.body(13))
                    .foregroundStyle(Theme.inkSecondary)
                    .lineSpacing(3)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(20)
            }
            .frame(minHeight: 160, maxHeight: 480)
        }
        .frame(width: 520)
        .background(Theme.elevated)
    }
}
