import SwiftUI

/// A task's own page: its title and details, the words that made it, how it
/// relates to other tasks and what happened to it, beside an inspector that
/// holds every property. J and K step through the list it was opened from.
struct TaskDetailView: View {
    let id: String
    let place: Place
    @Environment(AppModel.self) private var model
    @State private var width: CGFloat = 1100

    var body: some View {
        let store = model.work
        let row = model.workRows([id]).first
        let detail = store.detail.flatMap { $0.item.id == id ? $0 : nil }
        VStack(spacing: 0) {
            WorkBar {
                BackLink(title: back) { close() }
                if let row {
                    Text("#\(row.number)")
                        .font(TypeScale.mono(12))
                        .foregroundStyle(Theme.inkTertiary)
                        .textSelection(.enabled)
                }
            } trailing: {
                if let row {
                    if row.status == .triage {
                        TriageButtons(ids: [row.id])
                    }
                    DetailStepper(id: row.id)
                    Menu {
                        WorkActionMenu(ids: [row.id], omit: [.peek, .open], keys: false)
                    } label: {
                        Image(systemName: "ellipsis")
                    }
                    .menuStyle(.button)
                    .buttonStyle(.icon)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .help("More")
                    .accessibilityLabel("More")
                }
            }
            if let row {
                HStack(spacing: 0) {
                    ScrollView {
                        DetailMain(row: row, detail: detail, place: place)
                            .frame(maxWidth: 720, alignment: .leading)
                            .padding(.horizontal, 40)
                            .padding(.top, 28)
                            .padding(.bottom, 96)
                            .frame(maxWidth: .infinity)
                    }
                    Rectangle().fill(Theme.hairline).frame(width: 1)
                    ScrollView {
                        TaskPropertyList(row: row)
                            .padding(.horizontal, 14)
                            .padding(.vertical, 18)
                    }
                    .frame(width: width <= 1060 ? 280 : 320)
                }
            } else if store.items == nil {
                TaskSkeleton(rows: 4)
            } else {
                TaskEmpty(line: "This task is gone.") {
                    TaskKeyButton(title: back, keys: "esc") { close() }
                }
            }
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
        .task(id: "\(id):\(row?.revision ?? -1)") { await store.loadDetail(id) }
    }

    private func close() {
        model.workFocus.open(nil, in: place)
    }

    /// Where Esc and the back link return: the list, or the person whose
    /// page the task was opened from.
    private var back: String {
        switch place {
        case .triage: "Triage"
        case .people: model.people.detail?.person.displayName ?? "People"
        default: "Tasks"
        }
    }
}

/// "3 / 12" and the arrows to the tasks either side, as K and J step.
private struct DetailStepper: View {
    let id: String
    @Environment(AppModel.self) private var model

    var body: some View {
        let visible = model.workFocus.visible
        let index = visible.firstIndex(of: id)
        HStack(spacing: 2) {
            if let index {
                Text("\(index + 1) / \(visible.count)")
                    .metaText()
                    .monospacedDigit()
                    .padding(.trailing, 6)
            }
            WorkBarButton(icon: "chevron.up", help: "Previous  K") { _ = model.step(from: id, by: -1) }
                .disabled(index.map { $0 == 0 } ?? true)
            WorkBarButton(icon: "chevron.down", help: "Next  J") { _ = model.step(from: id, by: 1) }
                .disabled(index.map { $0 == visible.count - 1 } ?? true)
        }
    }
}

private struct DetailMain: View {
    let row: WorkItemRow
    let detail: WorkItemDetail?
    let place: Place
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            DetailTitle(row: row)
            ProposalStrip(proposals: model.work.proposals(for: row.id))
            DetailNotes(row: row)
                .padding(.top, 10)
            if row.status == .triage {
                DetailSuggestions(suggestions: detail?.suggestions ?? model.work.suggestions[row.id] ?? [])
            }
            DetailEvidence(row: row, links: detail?.evidence ?? [])
            DetailRelations(row: row, relations: detail?.relations, place: place)
                .padding(.top, 32)
            DetailActivity(row: row, activity: detail?.activity)
                .padding(.top, 32)
        }
    }
}

/// The title, edited where it stands: Return or leaving keeps it, Esc puts
/// it back. E on the page lands here.
private struct DetailTitle: View {
    let row: WorkItemRow
    @Environment(AppModel.self) private var model
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        TextField("", text: $text, prompt: Text("Title").foregroundStyle(Theme.inkDisabled), axis: .vertical)
            .textFieldStyle(.plain)
            .font(.system(size: 22, weight: .semibold))
            .foregroundStyle(Theme.ink)
            .focused($focused)
            .onSubmit {
                commit()
                focused = false
            }
            .onExitCommand {
                text = row.title
                focused = false
            }
            .onChange(of: focused) { _, now in
                if !now { commit() }
            }
            .onChange(of: row.title, initial: true) { _, title in
                if !focused { text = title }
            }
            .onChange(of: model.workFocus.renaming, initial: true) { _, id in
                guard id == row.id else { return }
                model.workFocus.renaming = nil
                focused = true
            }
            .accessibilityLabel("Title")
    }

    private func commit() {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            text = row.title
            return
        }
        if trimmed != row.title { model.renameTask(row.id, to: trimmed) }
    }
}

/// The details, kept when the field is left.
private struct DetailNotes: View {
    let row: WorkItemRow
    @Environment(AppModel.self) private var model
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        TextField("", text: $text, prompt: Text("Details").foregroundStyle(Theme.inkDisabled), axis: .vertical)
            .textFieldStyle(.plain)
            .font(TypeScale.body(14))
            .lineSpacing(3)
            .foregroundStyle(Theme.ink)
            .focused($focused)
            .onSubmit { focused = false }
            .onExitCommand {
                text = row.details ?? ""
                focused = false
            }
            .onChange(of: focused) { _, now in
                if !now { commit() }
            }
            .onChange(of: row.details, initial: true) { _, details in
                if !focused { text = details ?? "" }
            }
            .accessibilityLabel("Details")
    }

    private func commit() {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed != (row.details ?? "") else { return }
        let value: WorkPatchValue<String> = trimmed.isEmpty ? .clear : .set(trimmed)
        let id = row.id
        model.changeWork { model.work.update([id], WorkPatch(details: value)) }
    }
}

/// What Sona would fill in on accepting, each with why and what it heard.
private struct DetailSuggestions: View {
    let suggestions: [WorkFieldSuggestion]

    var body: some View {
        if !suggestions.isEmpty {
            VStack(alignment: .leading, spacing: 14) {
                DetailHeading(title: "Suggested")
                ForEach(Array(suggestions.enumerated()), id: \.offset) { _, suggestion in
                    SuggestionLine(suggestion: suggestion)
                }
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
            .padding(.top, 24)
        }
    }
}

private struct SuggestionLine: View {
    let suggestion: WorkFieldSuggestion
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: WorkSuggestionText.icon(suggestion.field))
                .font(.system(size: 12))
                .foregroundStyle(Theme.accent)
                .frame(width: 16, height: 18)
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text(WorkSuggestionText.title(suggestion.field)).metaText()
                    Text(WorkSuggestionText.value(suggestion, model: model, now: TaskDates.now()))
                        .font(TypeScale.listTitle)
                        .foregroundStyle(Theme.ink)
                }
                .frame(height: 18)
                Text(suggestion.reason).bodyText(12, Theme.inkSecondary)
                if let quote = suggestion.quote {
                    Text("“\(quote)”").bodyText(12, Theme.inkSecondary).italic()
                }
            }
            Spacer(minLength: 8)
            if let meeting = suggestion.meetingId, let at = suggestion.atMs {
                PlayMoment(sessionId: meeting, atMs: at)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

/// The words the task came from, and every other place it was discussed.
private struct DetailEvidence: View {
    let row: WorkItemRow
    let links: [PlanningEvidenceLink]

    var body: some View {
        let quoted = row.sourceQuote != nil
        let others = links.filter { link in
            !(quoted && link.sourceKind == .meeting && link.sourceId == row.sourceMeetingId && link.atMs == row.sourceAtMs)
        }
        if quoted || !others.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                DetailHeading(title: "Evidence")
                if row.raisedIn > 1 {
                    Text("Raised in \(row.raisedIn) meetings").metaText()
                }
                if let quote = row.sourceQuote {
                    QuoteCard(quote: quote, speaker: row.sourceSpeaker, meetingTitle: row.sourceMeetingTitle,
                              meetingId: row.sourceMeetingId, atMs: row.sourceAtMs)
                }
                ForEach(others) { link in
                    EvidenceLine(link: link)
                }
            }
            .padding(.top, 32)
        }
    }
}

/// A quote with who said it, where and when; it plays from that moment and
/// opens the meeting there.
struct QuoteCard: View {
    let quote: String
    let speaker: String?
    let meetingTitle: String?
    let meetingId: String?
    let atMs: Int64?
    @Environment(AppModel.self) private var model

    var body: some View {
        let meta = [speaker, meetingTitle, atMs?.meetingMsClock].compactMap { $0 }.joined(separator: " · ")
        VStack(alignment: .leading, spacing: 10) {
            Text("“\(quote)”")
                .bodyText(14)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
            HStack(spacing: 8) {
                Text(meta).metaText().lineLimit(1)
                Spacer(minLength: 8)
                if let meetingId, let atMs {
                    PlayMoment(sessionId: meetingId, atMs: atMs)
                }
                if let meetingId {
                    Button {
                        model.meetings.open(meetingId, atMilliseconds: atMs.flatMap(UInt64.init(exactly:)))
                        model.go(.meetings)
                    } label: {
                        Label("Open meeting", systemImage: "arrow.up.right")
                    }
                    .buttonStyle(QuietButton(compact: true))
                    .help("Open the meeting at this moment")
                }
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
    }
}

/// Plays a meeting from a moment, a few seconds early for context; the same
/// button stops it.
struct PlayMoment: View {
    let sessionId: String
    let atMs: Int64
    @Environment(AppModel.self) private var model

    var body: some View {
        let audio = model.meetings.audio
        let focus = model.workFocus
        let key = "\(sessionId):\(atMs)"
        let mine = focus.playingMoment == key && audio.sessionId == sessionId
        let running = mine && (audio.playing || audio.loading)
        HStack(spacing: 6) {
            if mine, let error = audio.error {
                Text(error)
                    .metaText(error == ConversationAudioPlayback.heldNotice ? Theme.inkSecondary : Theme.live)
                    .lineLimit(1)
            }
            Button {
                Self.toggle(sessionId: sessionId, atMs: atMs, model: model)
            } label: {
                Label(running ? "Stop" : "Play", systemImage: running ? "stop.fill" : "play.fill")
            }
            .buttonStyle(QuietButton(color: Theme.accent, compact: true))
            .disabled(audio.heldForRecording)
            .help(audio.heldForRecording ? ConversationAudioPlayback.heldNotice : "Play from \(atMs.meetingMsClock)")
        }
    }

    /// Plays from the moment, or stops it when it is the one playing.
    static func toggle(sessionId: String, atMs: Int64, model: AppModel) {
        let audio = model.meetings.audio
        let key = "\(sessionId):\(atMs)"
        guard !audio.heldForRecording else { return }
        if model.workFocus.playingMoment == key, audio.sessionId == sessionId, audio.playing || audio.loading {
            audio.stop()
        } else {
            model.workFocus.playingMoment = key
            audio.playBookmark(sessionId: sessionId, offset: UInt64(max(0, atMs)) * 1_000_000)
        }
    }
}

/// Plays the part of a dictation a task was heard in, and stops where it
/// ends; the same button stops it sooner.
private struct PlaySpan: View {
    let historyId: Int64
    let atMs: Int64
    let endMs: Int64?
    @Environment(AppModel.self) private var model

    var body: some View {
        let library = model.library
        let focus = model.workFocus
        let key = "dictation:\(historyId):\(atMs)"
        let loading = library.loadingAudio == historyId
        let mine = focus.playingMoment == key && (library.playingId == historyId || loading)
        let running = mine && (library.playing || loading)
        let failed = focus.playingMoment == key && library.playingId != historyId && !loading
        HStack(spacing: 6) {
            if failed, let error = library.error {
                Text(error).metaText(Theme.live).lineLimit(1)
            }
            Button {
                if running {
                    library.stopPlayback()
                } else {
                    focus.playingMoment = key
                    library.playSpan(historyId, fromMs: atMs, toMs: endMs)
                }
            } label: {
                Label(running ? "Stop" : "Play", systemImage: running ? "stop.fill" : "play.fill")
            }
            .buttonStyle(QuietButton(color: Theme.accent, compact: true))
            .disabled(library.heldForRecording)
            .help(library.heldForRecording ? ConversationAudioPlayback.heldNotice : "Play what was said")
        }
    }
}

/// Another place the task came up: a meeting moment, a note, a loop, a
/// dictation, a page.
private struct EvidenceLine: View {
    let link: PlanningEvidenceLink
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: icon)
                .font(.system(size: 12))
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: 16)
            Text(text)
                .bodyText(13, link.available ? Theme.ink : Theme.inkTertiary)
                .lineLimit(1)
                .help(help ?? "")
            if let meta {
                Text(meta).metaText().monospacedDigit().lineLimit(1)
            }
            Spacer(minLength: 8)
            if link.available {
                if link.sourceKind == .meeting, let at = link.atMs {
                    PlayMoment(sessionId: link.sourceId, atMs: at)
                }
                if link.sourceKind == .dictation, let id = Int64(link.sourceId), let at = link.atMs {
                    PlaySpan(historyId: id, atMs: at, endMs: link.endMs)
                }
                Button {
                    model.openPlanningEvidence(link)
                } label: {
                    Label("Open", systemImage: "arrow.up.right")
                }
                .buttonStyle(QuietButton(compact: true))
            } else {
                Text("Removed").metaText()
            }
        }
        .frame(minHeight: 30)
    }

    private var text: String {
        switch link.sourceKind {
        case .dictation: link.caption.map { "“\($0)”" } ?? link.title ?? fallback
        case .link: link.caption ?? host ?? fallback
        case .meeting, .note, .loop: link.title ?? fallback
        }
    }

    private var meta: String? {
        switch link.sourceKind {
        case .meeting: link.atMs?.meetingMsClock
        case .dictation:
            link.atMs.map { at in [at.meetingMsClock, link.endMs?.meetingMsClock].compactMap { $0 }.joined(separator: "–") }
        case .link: link.caption == nil ? nil : host
        case .note, .loop: nil
        }
    }

    private var help: String? {
        switch link.sourceKind {
        case .dictation: link.title
        case .link: link.sourceId
        case .meeting, .note, .loop: nil
        }
    }

    /// The site a link points into, or the file's name.
    private var host: String? {
        guard let url = URL(string: link.sourceId) else { return nil }
        if url.isFileURL { return url.lastPathComponent }
        guard let host = url.host() else { return nil }
        return host.hasPrefix("www.") ? String(host.dropFirst(4)) : host
    }

    private var icon: String {
        switch link.sourceKind {
        case .meeting: "waveform"
        case .note: "note.text"
        case .loop: "arrow.triangle.2.circlepath"
        case .dictation: "mic"
        case .link: URL(string: link.sourceId)?.isFileURL == true ? "doc" : "link"
        }
    }

    private var fallback: String {
        switch link.sourceKind {
        case .meeting: "A meeting"
        case .note: "A note"
        case .loop: "A commitment"
        case .dictation: "A dictation"
        case .link: "A link"
        }
    }
}

/// Blocks, blocked by, related: each opens its task, and the plus adds one.
private struct DetailRelations: View {
    let row: WorkItemRow
    let relations: [WorkRelation]?
    let place: Place
    @Environment(AppModel.self) private var model
    @State private var adding: RelationAdd?

    var body: some View {
        let entries = RelationEntry.all(for: row.id, in: relations ?? [])
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 8) {
                DetailHeading(title: "Relations")
                Spacer(minLength: 0)
                Menu {
                    ForEach(RelationAdd.allCases) { kind in
                        Button(kind.title) { adding = kind }
                    }
                } label: {
                    Image(systemName: "plus")
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .help("Add a relation")
                .accessibilityLabel("Add a relation")
                .popover(item: $adding, arrowEdge: .bottom) { kind in
                    ChoiceList(prompt: kind.prompt, compact: true, empty: "No task matches.") { text in
                        choices(kind, text: text, taken: Set(entries.map(\.other)))
                    } close: {
                        adding = nil
                    }
                    .padding(.vertical, 4)
                    .environment(model)
                }
            }
            .padding(.bottom, 6)
            ForEach(entries) { entry in
                RelationLine(entry: entry, place: place)
            }
        }
    }

    private func choices(_ kind: RelationAdd, text: String, taken: Set<String>) -> [WorkChoice] {
        let id = row.id
        let number = text.hasPrefix("#") ? String(text.dropFirst()) : text
        let pool = (model.work.items ?? []).filter { other in
            other.id != id && !taken.contains(other.id)
                && (text.isEmpty || other.title.localizedCaseInsensitiveContains(text) || "\(other.number)" == number)
        }
        return pool.sorted { $0.updatedAtMs > $1.updatedAtMs }.prefix(30).map { other in
            WorkChoice(id: other.id, title: other.title, mark: .status(other.status), detail: "#\(other.number)") {
                let (from, to) = kind == .blockedBy ? (other.id, id) : (id, other.id)
                model.changeWork { model.work.addRelation(from: from, to: to, kind: kind.kind) }
            }
        }
    }
}

private enum RelationAdd: String, CaseIterable, Identifiable {
    case blocks, blockedBy, related

    var id: String { rawValue }

    var title: String {
        switch self {
        case .blocks: "Blocks…"
        case .blockedBy: "Blocked by…"
        case .related: "Related to…"
        }
    }

    var prompt: String {
        switch self {
        case .blocks: "Blocks which task…"
        case .blockedBy: "Blocked by which task…"
        case .related: "Related to which task…"
        }
    }

    var kind: WorkRelationKind { self == .related ? .related : .blocks }
}

/// One relation from this task's side.
private struct RelationEntry: Identifiable {
    let id: String
    let title: String
    let other: String

    static func all(for id: String, in relations: [WorkRelation]) -> [RelationEntry] {
        let entries = relations.compactMap { relation -> RelationEntry? in
            switch relation.kind {
            case .blocks where relation.fromId == id:
                RelationEntry(id: relation.id, title: "Blocks", other: relation.toId)
            case .blocks where relation.toId == id:
                RelationEntry(id: relation.id, title: "Blocked by", other: relation.fromId)
            case .related where relation.fromId == id || relation.toId == id:
                RelationEntry(id: relation.id, title: "Related", other: relation.fromId == id ? relation.toId : relation.fromId)
            default:
                nil
            }
        }
        let order = ["Blocked by", "Blocks", "Related"]
        return entries.sorted { (order.firstIndex(of: $0.title) ?? 3) < (order.firstIndex(of: $1.title) ?? 3) }
    }
}

private struct RelationLine: View {
    let entry: RelationEntry
    let place: Place
    @Environment(AppModel.self) private var model
    @State private var hovering = false

    var body: some View {
        let other = model.workRows([entry.other]).first
        HStack(spacing: 10) {
            Text(entry.title).metaText().frame(width: 76, alignment: .leading)
            if let other {
                TaskStatusIcon(status: other.status)
                Text("#\(other.number)").font(TypeScale.mono(12)).foregroundStyle(Theme.inkTertiary)
                Text(other.title).bodyText(13).lineLimit(1)
            } else {
                Text("An archived task").bodyText(13, Theme.inkTertiary)
            }
            Spacer(minLength: 8)
            Button {
                let id = entry.id
                model.changeWork { model.work.removeRelation(id) }
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .bold))
                    .frame(width: 20, height: 20)
                    .contentShape(Rectangle())
            }
            .buttonStyle(QuietButton(compact: true))
            .opacity(hovering ? 1 : 0)
            .help("Remove")
            .accessibilityLabel("Remove \(entry.title.lowercased()) relation")
        }
        .padding(.horizontal, 8)
        .frame(height: 32)
        .background(hovering ? Theme.wash : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusRow))
        .hoverFade(hovering)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .onTapGesture {
            guard let other else { return }
            model.workFocus.highlighted = other.id
            model.workFocus.open(other.id, in: place)
        }
        .accessibilityElement(children: .contain)
        .accessibilityAddTraits(.isButton)
    }
}

/// Everything that happened to the task, oldest first.
private struct DetailActivity: View {
    let row: WorkItemRow
    let activity: [WorkActivity]?
    @Environment(AppModel.self) private var model

    var body: some View {
        let now = TaskDates.now()
        VStack(alignment: .leading, spacing: 0) {
            DetailHeading(title: "Activity")
                .padding(.bottom, 8)
            if let activity {
                ForEach(activity.sorted { $0.atMs < $1.atMs }) { entry in
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        ActorMark(actor: entry.actor)
                        Text(WorkActivityText.line(entry, model: model))
                            .bodyText(13, Theme.inkSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                        Spacer(minLength: 8)
                        Text(TaskDates.ago(entry.atMs, now: now))
                            .metaText()
                            .help(PlanningFormat.date(entry.atMs))
                    }
                    .padding(.vertical, 5)
                }
            } else if let error = model.work.detailError {
                HStack(spacing: 10) {
                    Text(error).metaText(Theme.live).lineLimit(2)
                    Button("Retry") {
                        let id = row.id
                        Task { await model.work.loadDetail(id) }
                    }
                    .buttonStyle(QuietButton(compact: true))
                }
            } else {
                VStack(alignment: .leading, spacing: 14) {
                    ForEach([220, 170, 250], id: \.self) { width in
                        Capsule().fill(Theme.track).frame(width: CGFloat(width), height: 8)
                    }
                }
                .padding(.vertical, 6)
                .accessibilityElement()
                .accessibilityLabel("Loading activity")
            }
        }
    }
}

private struct ActorMark: View {
    let actor: WorkActor
    @Environment(AppModel.self) private var model

    var body: some View {
        switch actor {
        case .me:
            Avatar(face: AvatarFace(kind: .me, name: "Me"), size: 16, revision: model.people.photoRevision)
        case .sona:
            Image(systemName: "waveform")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(Theme.accent)
                .frame(width: 16, height: 16)
        case .agent:
            Image(systemName: "sparkle")
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: 16, height: 16)
        }
    }
}

struct DetailHeading: View {
    let title: String

    var body: some View {
        Text(title)
            .font(.system(size: 12, weight: .semibold))
            .foregroundStyle(Theme.inkTertiary)
            .accessibilityAddTraits(.isHeader)
    }
}

/// The properties, each a value that opens its picker: the detail page's
/// inspector and the peek.
struct TaskPropertyList: View {
    let row: WorkItemRow
    @Environment(AppModel.self) private var model

    var body: some View {
        let work = model.work
        let ids = [row.id]
        VStack(alignment: .leading, spacing: 2) {
            PropertyLine(title: "Status") {
                WorkPickerButton(kind: .status, ids: ids) {
                    PropertyValue(text: row.status.label) { TaskStatusIcon(status: row.status) }
                }
            }
            PropertyLine(title: "Priority") {
                WorkPickerButton(kind: .priority, ids: ids) {
                    PropertyValue(text: TaskPriority(row.priority).label, faint: row.priority == 0) {
                        PriorityIcon(priority: TaskPriority(row.priority))
                    }
                }
            }
            PropertyLine(title: "Owner") {
                WorkPickerButton(kind: .owner, ids: ids) {
                    PropertyValue(text: row.ownerFace.name, faint: !row.ownerIsSelf && row.ownerPersonId == nil) {
                        Avatar(face: row.ownerFace, size: 16, revision: model.people.photoRevision)
                    }
                }
            }
            PropertyLine(title: "Agent") {
                WorkPickerButton(kind: .delegate, ids: ids) {
                    let waiting = row.doer != nil && model.agents.waits(on: row)
                    PropertyValue(text: row.doer.map { waiting ? "\($0.agent), waiting for you" : $0.agent } ?? "No agent",
                                  faint: row.doer == nil) {
                        Image(systemName: "sparkle").font(.system(size: 12))
                            .foregroundStyle(waiting ? Theme.live : Theme.inkSecondary)
                    }
                }
            }
            PropertyLine(title: "Due date") {
                WorkPickerButton(kind: .due, ids: ids) {
                    PropertyValue(text: row.dueAtMs.map(PlanningFormat.day) ?? "No due date", faint: row.dueAtMs == nil) {
                        Image(systemName: "calendar").font(.system(size: 12)).foregroundStyle(Theme.inkSecondary)
                    }
                }
            }
            if let detail = work.detail, detail.item.id == row.id, let rule = detail.recurrence {
                PropertyLine(title: "Repeats") {
                    HStack(spacing: 8) {
                        Image(systemName: "repeat")
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.inkSecondary)
                            .frame(width: 16, height: 16)
                        Text(Recurrence.display(rule)).bodyText(13, Theme.ink).lineLimit(1)
                    }
                    .padding(.horizontal, 8)
                    .frame(height: 28)
                }
            }
            PropertyLine(title: "Project") {
                WorkPickerButton(kind: .project, ids: ids) {
                    PropertyValue(text: row.projectTitle ?? work.projects.first { $0.id == row.projectId }?.title ?? "No project",
                                  faint: row.projectId == nil) {
                        Image(systemName: "folder").font(.system(size: 12)).foregroundStyle(Theme.inkSecondary)
                    }
                }
            }
            PropertyLine(title: "Labels") {
                WorkPickerButton(kind: .labels, ids: ids) {
                    let labels = row.labelIds.compactMap { id in work.labels.first { $0.id == id } }
                    if labels.isEmpty {
                        PropertyValue(text: "No labels", faint: true) {
                            Image(systemName: "tag").font(.system(size: 12)).foregroundStyle(Theme.inkSecondary)
                        }
                    } else {
                        FlowLabels(labels: labels)
                    }
                }
            }
            Hairline().padding(.vertical, 12)
            if let meeting = row.sourceMeetingId {
                PropertyLine(title: "Meeting") {
                    Button {
                        model.meetings.open(meeting, atMilliseconds: row.sourceAtMs.flatMap(UInt64.init(exactly:)))
                        model.go(.meetings)
                    } label: {
                        PropertyValue(text: row.sourceMeetingTitle ?? "A meeting") {
                            Image(systemName: "waveform").font(.system(size: 12)).foregroundStyle(Theme.inkSecondary)
                        }
                    }
                    .buttonStyle(.plain)
                }
            }
            PropertyLine(title: "Created") {
                Text(PlanningFormat.date(row.createdAtMs)).metaText(Theme.inkSecondary).padding(.horizontal, 8)
            }
            PropertyLine(title: "Updated") {
                Text(PlanningFormat.date(row.updatedAtMs)).metaText(Theme.inkSecondary).padding(.horizontal, 8)
            }
        }
    }
}

private struct PropertyLine<Value: View>: View {
    let title: String
    @ViewBuilder let value: () -> Value

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Text(title).metaText().frame(width: 72, alignment: .leading)
            value()
            Spacer(minLength: 0)
        }
        .frame(minHeight: 32)
    }
}

/// A property's value as a button face: hover washes it, like a row.
private struct PropertyValue<Mark: View>: View {
    let text: String
    var faint = false
    @ViewBuilder let mark: () -> Mark
    @State private var hovering = false

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            mark().frame(width: 16, height: 16)
            Text(text)
                .bodyText(13, faint ? Theme.inkTertiary : Theme.ink)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .frame(minHeight: 28)
        .background(hovering ? Theme.controlHover : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .hoverFade(hovering)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }
}

private struct FlowLabels: View {
    let labels: [WorkLabel]
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 4) {
            ForEach(labels) { label in
                LabelPill(label: label)
            }
        }
        .padding(.horizontal, 6)
        .frame(minHeight: 28)
        .background(hovering ? Theme.controlHover : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .hoverFade(hovering)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }
}

/// What the activity list says happened, in words.
@MainActor
enum WorkActivityText {
    static func line(_ entry: WorkActivity, model: AppModel) -> String {
        let actor = switch entry.actor {
        case .me: "You"
        case .sona: "Sona"
        case .agent: entry.agent ?? "An agent"
        }
        return "\(actor) \(verb(entry, model: model))"
    }

    private static func verb(_ entry: WorkActivity, model: AppModel) -> String {
        let work = model.work
        switch entry.kind {
        case .created:
            return entry.to == WorkStatus.triage.rawValue ? "heard this in a meeting" : "created the task"
        case .status:
            return "changed the status from \(status(entry.from)) to \(status(entry.to))"
        case .priority:
            let priority = TaskPriority(Int(entry.to ?? "") ?? 0)
            return priority == .none ? "removed the priority" : "set the priority to \(priority.label)"
        case .owner:
            switch entry.to {
            case "self"?: return "took it on"
            case "nobody"?, nil: return "removed the owner"
            case let id?: return "assigned it to \(work.people.first { $0.id == id }?.name ?? "someone")"
            }
        case .due:
            guard let due = entry.to.flatMap({ Int64($0) }) else { return "removed the due date" }
            return "set the due date to \(PlanningFormat.day(due))"
        case .project:
            guard let id = entry.to, !id.isEmpty else { return "took it out of its project" }
            return "moved it to \(work.projects.first { $0.id == id }?.title ?? "a project")"
        case .title:
            return "renamed it to “\(entry.to ?? "")”"
        case .details:
            return "edited the details"
        case .labels:
            let names = (entry.to ?? "").split(separator: ",").compactMap { id in
                work.labels.first { $0.id == String(id) }?.name
            }
            return names.isEmpty ? "removed the labels" : "set the labels to \(names.joined(separator: ", "))"
        case .archived:
            return "archived it"
        case .restored:
            return "restored it"
        case .duplicate:
            return "marked it a duplicate of \(task(entry.to, model: model))"
        case .accepted:
            return "accepted it"
        case .declined:
            return "declined it"
        case .snoozed:
            guard let wake = entry.to.flatMap({ Int64($0) }) else { return "woke it" }
            return "snoozed it until \(PlanningFormat.day(wake))"
        case .relation:
            if let added = entry.to { return "marked it \(relation(added, model: model))" }
            if let removed = entry.from { return "unlinked it from \(task(relationTarget(removed), model: model))" }
            return "changed its relations"
        case .kept:
            return "confirmed it still stands"
        }
    }

    private static func status(_ raw: String?) -> String {
        raw.flatMap(WorkStatus.init(rawValue:))?.label ?? "none"
    }

    private static func task(_ id: String?, model: AppModel) -> String {
        guard let id, let row = model.workRows([id]).first else { return "another task" }
        return "#\(row.number)"
    }

    /// The task on the other side of "blocks:<id>" and its kin.
    private static func relationTarget(_ value: String) -> String? {
        let parts = value.split(separator: ":", maxSplits: 1)
        return parts.count == 2 ? String(parts[1]) : nil
    }

    private static func relation(_ value: String, model: AppModel) -> String {
        let other = task(relationTarget(value), model: model)
        return if value.hasPrefix("blocks:") {
            "as blocking \(other)"
        } else if value.hasPrefix("blocked_by:") {
            "as blocked by \(other)"
        } else {
            "as related to \(other)"
        }
    }
}
