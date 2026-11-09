import AppKit
import SwiftUI

// MARK: - Wire

/// What a reminder was about, so its click knows where to land.
enum WorkReminderKind: String, Decodable {
    case due
    case followUp = "follow_up"
    case brief
}

/// `work-reminder-opened`: a reminder was clicked. `id` is the task or the
/// calendar event, or nil when the reminder named several tasks.
struct WorkReminderOpened: Decodable {
    let kind: WorkReminderKind
    let id: String?
}

/// Whose open work a brief gathers: a calendar event's people, or one person.
enum WorkBriefSubject: Encodable, Hashable {
    case event(String)
    case person(String)

    private enum CodingKeys: String, CodingKey {
        case kind, eventKey, personId
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .event(key):
            try container.encode("event", forKey: .kind)
            try container.encode(key, forKey: .eventKey)
        case let .person(id):
            try container.encode("person", forKey: .kind)
            try container.encode(id, forKey: .personId)
        }
    }
}

struct WorkTalkingPoint: Decodable, Identifiable, Equatable {
    let id: String
    let text: String
    let createdAtMs: Int64
}

struct WorkBriefPerson: Decodable, Identifiable, Equatable {
    let personId: String
    let name: String
    /// My open tasks from meetings they were in, by item id.
    let youOwe: [String]
    /// Their open tasks, by item id.
    let theyOwe: [String]
    var talkingPoints: [WorkTalkingPoint]

    var id: String { personId }
}

struct WorkBriefDecision: Decodable, Equatable {
    let text: String
    /// Where in the meeting it was said, while those words are kept.
    let atMs: Int64?
}

struct WorkBriefMeeting: Decodable, Equatable {
    let meetingId: String
    let title: String
    let atMs: Int64
    let decisions: [WorkBriefDecision]
}

struct WorkBrief: Decodable, Equatable {
    /// The meeting's title, or the person's name.
    let title: String
    let startsAtMs: Int64?
    var people: [WorkBriefPerson]
    let lastTime: WorkBriefMeeting?
}

/// How a nudge reached a compose window. Nothing is ever sent.
enum WorkNudgeMail: Decodable {
    case composed
    case link(url: String, body: MeetingFollowUpMailBody)

    private enum CodingKeys: String, CodingKey {
        case kind, url, body
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(String.self, forKey: .kind) {
        case "composed":
            self = .composed
        case "link":
            self = .link(
                url: try container.decode(String.self, forKey: .url),
                body: try container.decode(MeetingFollowUpMailBody.self, forKey: .body))
        case let kind:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "Unknown nudge mail kind \(kind)")
        }
    }
}

private struct BriefParams: Encodable {
    let subject: WorkBriefSubject
}

private struct NudgeMailParams: Encodable {
    let id: String
    let subject: String
    let body: String
    let overBoundNote: String
}

private struct TalkingPointAddParams: Encodable {
    let operationId: String
    let personId: String
    let text: String
}

private struct TalkingPointDoneParams: Encodable {
    let operationId: String
    let id: String
    let done: Bool
}

// MARK: - Reminders

extension AppModel {
    /// A reminder's click: a due day opens the task, a wait opens its nudge,
    /// a meeting about to start opens its brief.
    func observeReminders() {
        core.observe("work-reminder-opened") { [weak self] line in
            guard let opened: WorkReminderOpened = try? Core.payload(line) else { return }
            self?.openReminder(opened)
        }
    }

    private func openReminder(_ opened: WorkReminderOpened) {
        reveal()
        switch opened.kind {
        case .due:
            showTasks(.owe, at: opened.id)
            if let id = opened.id { workFocus.open(id, in: .tasks) }
        case .followUp:
            showTasks(.waiting, at: opened.id)
            if let id = opened.id { sheet = .nudge(id) }
        case .brief:
            if let key = opened.id { sheet = .workBrief(.event(key)) }
        }
    }

    private func showTasks(_ tab: TasksTab, at id: String?) {
        go(.tasks)
        workFocus.open(nil, in: .tasks)
        workFocus.switchTab(tab)
        workFocus.highlighted = id
    }

    /// "What's open with Dana": the brief for the person whose page is open,
    /// or for the one person who owns every task in front.
    var briefPaletteActions: [PaletteAction] {
        guard let person = briefPerson else { return [] }
        return [PaletteAction(
            id: "work-brief", group: .work, title: "What's open with \(person.name)",
            run: { [weak self] in self?.sheet = .workBrief(.person(person.id)) },
            icon: "person.crop.rectangle.stack")]
    }

    private var briefPerson: WorkPersonChoice? {
        var id: String?
        if place == .people, case let .person(shown) = people.route {
            id = shown
        } else {
            let owners = Set(workRows(workTargets).compactMap(\.ownerPersonId))
            if owners.count == 1 { id = owners.first }
        }
        return id.flatMap { id in work.people.first { $0.id == id } }
    }
}

// MARK: - Nudge

/// The words a nudge starts from, in the owner's voice: "You said Thursday
/// you'd send the rebate sheet; any update?"
enum NudgeDraft {
    static func text(for row: WorkItemRow, now: Int64) -> String {
        let task = action(row.title)
        guard let due = row.dueAtMs else {
            let days = PlanningFormat.days(from: now, to: row.createdAtMs)
            return "You said \(days < -6 ? "on " : "")\(day(row.createdAtMs, days: days)) you'd \(task); any update?"
        }
        let days = PlanningFormat.days(from: now, to: due)
        return "You said you'd \(task) \(days == 0 ? "today" : "by \(day(due, days: days))"); any update?"
    }

    private static func day(_ milliseconds: Int64, days: Int) -> String {
        switch days {
        case 0: "today"
        case -1: "yesterday"
        case 1: "tomorrow"
        case -6 ... -2, 2 ... 6: PlanningFormat.moment(milliseconds).formatted(.dateTime.weekday(.wide))
        default: PlanningFormat.day(milliseconds)
        }
    }

    /// "Send the rebate sheet." as the end of "you'd …": no closing stop, and
    /// the first word in lowercase unless it reads as a name or an acronym.
    private static func action(_ title: String) -> String {
        var text = title.trimmingCharacters(in: .whitespacesAndNewlines)
        while let last = text.last, ".!".contains(last) { text.removeLast() }
        guard let first = text.split(separator: " ").first, first.count > 1,
              first.dropFirst().allSatisfy(\.isLowercase)
        else { return text }
        return text.prefix(1).lowercased() + text.dropFirst()
    }
}

/// R on a wait: a draft that asks about it, in the owner's words. ↩ hands it
/// to Mail and C copies it; nothing leaves Sona any other way.
struct NudgeSheet: View {
    let id: String
    @Environment(AppModel.self) private var model
    @State private var edited: String?
    @State private var editing = false
    @State private var snoozing = false
    @State private var mailing = false
    @State private var copied = false
    @State private var notice: String?
    @State private var failure: String?
    @FocusState private var keysFocused: Bool
    @FocusState private var editorFocused: Bool

    var body: some View {
        let row = model.workRows([id]).first
        let text = edited ?? row.map { NudgeDraft.text(for: $0, now: model.work.now) } ?? ""
        VStack(alignment: .leading, spacing: 0) {
            header(row)
            Hairline()
            if snoozing {
                WorkPickerList(picker: WorkPicker(kind: .snooze, ids: [id]), close: close)
            } else {
                draft(text)
                    .padding(24)
                Hairline()
                footer(text, row: row)
            }
        }
        .frame(width: 540)
        .background(Theme.page)
        .focusable()
        .focusEffectDisabled()
        .focused($keysFocused)
        .onKeyPress(phases: .down) { press($0, text: text, row: row) }
        .onExitCommand(perform: back)
        .onAppear { DispatchQueue.main.async { keysFocused = true } }
    }

    private func header(_ row: WorkItemRow?) -> some View {
        HStack(alignment: .center, spacing: 12) {
            if let row {
                Avatar(face: row.ownerFace, size: 28, revision: model.people.photoRevision)
            }
            VStack(alignment: .leading, spacing: 3) {
                Text("Nudge \(row?.ownerName ?? "them")")
                    .titleText()
                    .lineLimit(1)
                    .accessibilityAddTraits(.isHeader)
                if let row {
                    Text("#\(row.number) \(row.title)").metaText(Theme.inkSecondary).lineLimit(1)
                }
            }
            Spacer(minLength: 12)
            Button(action: close) {
                Image(systemName: "xmark")
            }
            .buttonStyle(.circle)
            .help("Close")
            .accessibilityLabel("Close")
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 18)
    }

    private func draft(_ text: String) -> some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous)
        return VStack(alignment: .leading, spacing: 10) {
            Group {
                if editing {
                    TextEditor(text: Binding(get: { edited ?? text }, set: { edited = $0 }))
                        .font(TypeScale.body(15))
                        .foregroundStyle(Theme.ink)
                        .scrollContentBackground(.hidden)
                        .focused($editorFocused)
                } else {
                    Text(text)
                        .bodyText(15)
                        .frame(maxWidth: .infinity, alignment: .topLeading)
                }
            }
            .frame(maxWidth: .infinity, minHeight: 88, alignment: .topLeading)
            .padding(14)
            .background(Theme.surface, in: shape)
            .overlay(shape.strokeBorder(editing ? Theme.accent : Theme.border, lineWidth: 1))
            if let notice {
                Text(notice).bodyText(13, Theme.inkSecondary)
            }
            ErrorNote(failure)
        }
    }

    private func footer(_ text: String, row: WorkItemRow?) -> some View {
        HStack(spacing: 8) {
            if editing {
                Spacer(minLength: 8)
                TaskKeyButton(title: "Done editing", keys: "cmd+return", action: finishEditing)
                    .keyboardShortcut(.return, modifiers: .command)
            } else {
                TaskKeyButton(title: "Snooze", keys: "h") { snoozing = true }
                TaskKeyButton(title: "Done", keys: "d", action: done)
                Spacer(minLength: 8)
                TaskKeyButton(title: "Edit", keys: "e") { edit(text) }
                TaskKeyButton(title: copied ? "Copied" : "Copy", keys: "c") { copy(text) }
                TaskKeyButton(title: "Mail", keys: "return") { mail(text, row: row) }
                    .disabled(mailing || row == nil)
            }
        }
        .padding(.horizontal, 24)
        .frame(height: 60)
    }

    private func press(_ key: KeyPress, text: String, row: WorkItemRow?) -> KeyPress.Result {
        guard !editing, !snoozing, key.modifiers.isDisjoint(with: [.command, .option, .control]) else {
            return .ignored
        }
        if key.key == .return {
            mail(text, row: row)
            return .handled
        }
        switch key.characters.lowercased() {
        case "c": copy(text)
        case "e": edit(text)
        case "h": snoozing = true
        case "d": done()
        default: return .ignored
        }
        return .handled
    }

    /// Mail opens a compose window with the draft; the person presses Send
    /// there, or does not.
    private func mail(_ text: String, row: WorkItemRow?) {
        guard !mailing, let row else { return }
        mailing = true
        failure = nil
        Task {
            defer { mailing = false }
            do {
                let mail: WorkNudgeMail = try await model.core.request("work_nudge_mail", NudgeMailParams(
                    id: id, subject: row.title, body: text,
                    overBoundNote: "The nudge is on the clipboard. Paste it here."))
                switch mail {
                case .composed:
                    model.work.nudged(id)
                    close()
                case let .link(url, body):
                    if body == .clipboard { put(text) }
                    guard let link = URL(string: url), NSWorkspace.shared.open(link) else {
                        failure = "Mail could not be opened."
                        return
                    }
                    model.work.nudged(id)
                    if body == .clipboard {
                        notice = "The draft is on the clipboard. Paste it into the new email."
                    } else {
                        close()
                    }
                }
            } catch {
                failure = error.localizedDescription
            }
        }
    }

    private func copy(_ text: String) {
        put(text)
        copied = true
        model.work.nudged(id)
    }

    private func put(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    private func done() {
        close()
        model.changeWork { model.work.update([id], WorkPatch(status: .done)) }
    }

    private func edit(_ text: String) {
        edited = text
        editing = true
        DispatchQueue.main.async { editorFocused = true }
    }

    private func finishEditing() {
        editing = false
        keysFocused = true
    }

    private func back() {
        if editing {
            finishEditing()
        } else if snoozing {
            snoozing = false
            keysFocused = true
        } else {
            close()
        }
    }

    private func close() {
        model.sheet = nil
    }
}

// MARK: - Brief

/// A line the brief's highlight can rest on.
private enum BriefLine: Hashable {
    case task(person: String, id: String)
    case point(String)
    case decision(Int)
}

/// Before a meeting, or about one person: what I owe them, what they owe me,
/// what I meant to raise, and what was decided last time, with the moment it
/// was said.
struct WorkBriefSheet: View {
    let subject: WorkBriefSubject
    @Environment(AppModel.self) private var model
    @State private var brief: WorkBrief?
    @State private var loadFailure: String?
    @State private var failure: String?
    @State private var highlighted: BriefLine?
    @State private var adding: String?
    @State private var point = ""
    @State private var finished: Set<String> = []
    @State private var reopen: [String: WorkStatus] = [:]
    @FocusState private var keysFocused: Bool
    @FocusState private var fieldFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Hairline()
            ScrollViewReader { proxy in
                ScrollView {
                    content
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(24)
                }
                .onChange(of: highlighted) { _, line in
                    if let line { withAnimation(Motion.regular) { proxy.scrollTo(line) } }
                }
            }
            Hairline()
            footer
        }
        .frame(width: 660, height: 560)
        .background(Theme.page)
        .focusable()
        .focusEffectDisabled()
        .focused($keysFocused)
        .onKeyPress(phases: .down, action: press)
        .onExitCommand(perform: escape)
        .task { await load() }
        .onAppear { DispatchQueue.main.async { keysFocused = true } }
    }

    /// A brief asked for about one person: their name heads the sheet.
    private var personal: Bool {
        if case .person = subject { true } else { false }
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 16) {
            if personal, let person = brief?.people.first {
                Avatar(face: AvatarFace(kind: .person(person.personId), name: person.name), size: 32,
                       revision: model.people.photoRevision)
            }
            VStack(alignment: .leading, spacing: 4) {
                Text(brief?.title ?? "Brief")
                    .titleText()
                    .lineLimit(1)
                    .accessibilityAddTraits(.isHeader)
                if let starts = brief?.startsAtMs {
                    Text("\(TaskDates.label(starts, now: model.work.now)), \(PlanningFormat.moment(starts).formatted(date: .omitted, time: .shortened))")
                        .metaText(Theme.inkSecondary)
                }
            }
            Spacer(minLength: 12)
            Button(action: close) {
                Image(systemName: "xmark")
            }
            .buttonStyle(.circle)
            .help("Close")
            .accessibilityLabel("Close")
        }
        .padding(24)
    }

    @ViewBuilder
    private var content: some View {
        if let brief {
            let open = brief.people.filter { person in
                !visible(person.youOwe).isEmpty || !visible(person.theyOwe).isEmpty
                    || !person.talkingPoints.isEmpty || adding == person.personId
            }
            VStack(alignment: .leading, spacing: 28) {
                ErrorNote(failure)
                if open.isEmpty {
                    Text(nothingOpen(brief)).bodyText(14, Theme.inkSecondary)
                }
                ForEach(open) { person in
                    section(person)
                }
                if let last = brief.lastTime {
                    lastTime(last)
                }
            }
        } else if let loadFailure {
            ErrorNote(loadFailure)
        }
    }

    private func section(_ person: WorkBriefPerson) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            if !personal {
                HStack(spacing: 10) {
                    Avatar(face: AvatarFace(kind: .person(person.personId), name: person.name), size: 24,
                           revision: model.people.photoRevision)
                    Text(person.name).font(TypeScale.headline).foregroundStyle(Theme.ink)
                }
            }
            tasks("You owe", person: person.personId, ids: person.youOwe)
            tasks("They owe you", person: person.personId, ids: person.theyOwe)
            if !person.talkingPoints.isEmpty || adding == person.personId {
                group("Talking points") {
                    ForEach(person.talkingPoints) { point in
                        let line = BriefLine.point(point.id)
                        BriefPointRow(point: point, done: finished.contains(point.id),
                                      highlighted: highlighted == line, now: model.work.now)
                            .id(line)
                            .onTapGesture { pick(line) }
                    }
                    if adding == person.personId {
                        field(for: person.personId)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func tasks(_ title: String, person: String, ids: [String]) -> some View {
        let rows = model.workRows(ids)
        if !rows.isEmpty {
            group(title) {
                ForEach(rows) { row in
                    let line = BriefLine.task(person: person, id: row.id)
                    BriefTaskRow(row: row, highlighted: highlighted == line, now: model.work.now)
                        .id(line)
                        .onTapGesture { pick(line) }
                }
            }
        }
    }

    private func field(for personId: String) -> some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusRow, style: .continuous)
        return TextField("", text: $point, prompt: Text("Something to raise").foregroundStyle(Theme.inkTertiary))
            .textFieldStyle(.plain)
            .font(TypeScale.list)
            .foregroundStyle(Theme.ink)
            .focused($fieldFocused)
            .onSubmit { addPoint(to: personId) }
            .padding(.horizontal, 10)
            .frame(height: 32)
            .background(Theme.surface, in: shape)
            .overlay(shape.strokeBorder(Theme.accent, lineWidth: 1))
    }

    private func lastTime(_ last: WorkBriefMeeting) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Text("Last time").sectionLabel()
                Button(last.title.isEmpty ? "Meeting" : last.title) { openMeeting(last.meetingId) }
                    .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                Text(PlanningFormat.day(last.atMs)).metaText()
            }
            if last.decisions.isEmpty {
                Text("No decisions were written down.").bodyText(13, Theme.inkSecondary)
            } else {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(Array(last.decisions.enumerated()), id: \.offset) { index, decision in
                        let line = BriefLine.decision(index)
                        BriefDecisionRow(decision: decision, meetingId: last.meetingId, highlighted: highlighted == line)
                            .id(line)
                            .onTapGesture { pick(line) }
                    }
                }
            }
        }
    }

    private func group<Content: View>(_ title: String, @ViewBuilder _ content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
                .font(TypeScale.label(12))
                .foregroundStyle(Theme.inkTertiary)
                .padding(.leading, 10)
                .padding(.bottom, 4)
            content()
        }
    }

    private var footer: some View {
        HStack(spacing: 6) {
            hint("j k", "Move")
            hint("d", "Done")
            hint("n", "Talking point")
            hint("space", "Play")
            hint("return", "Open")
            Spacer(minLength: 0)
            hint("esc", "Close")
        }
        .padding(.horizontal, 16)
        .frame(height: 40)
        .background(Theme.inset)
        .accessibilityElement(children: .combine)
    }

    private func hint(_ keys: String, _ title: String) -> some View {
        HStack(spacing: 6) {
            Shortcut(keys)
            Text(title).metaText(Theme.inkSecondary)
        }
        .padding(.trailing, 8)
    }

    // MARK: Keys

    private func press(_ key: KeyPress) -> KeyPress.Result {
        guard adding == nil, let brief, key.modifiers.isDisjoint(with: [.command, .option, .control]) else {
            return .ignored
        }
        let all = lines(brief)
        switch key.key {
        case .downArrow: move(1, in: all)
        case .upArrow: move(-1, in: all)
        case .return: openHighlighted()
        case .space: playHighlighted()
        default:
            switch key.characters.lowercased() {
            case "j": move(1, in: all)
            case "k": move(-1, in: all)
            case "d": toggleDone()
            case "n": startPoint(in: brief)
            default: return .ignored
            }
        }
        return .handled
    }

    private func escape() {
        if adding != nil {
            adding = nil
            point = ""
            keysFocused = true
        } else {
            close()
        }
    }

    /// Every line in the order the sheet shows them.
    private func lines(_ brief: WorkBrief) -> [BriefLine] {
        let people = brief.people.flatMap { person in
            visible(person.youOwe + person.theyOwe).map { BriefLine.task(person: person.personId, id: $0) }
                + person.talkingPoints.map { BriefLine.point($0.id) }
        }
        return people + (brief.lastTime?.decisions.indices.map(BriefLine.decision) ?? [])
    }

    private func visible(_ ids: [String]) -> [String] {
        model.workRows(ids).map(\.id)
    }

    private func move(_ delta: Int, in all: [BriefLine]) {
        guard !all.isEmpty else { return }
        guard let highlighted, let at = all.firstIndex(of: highlighted) else {
            self.highlighted = delta > 0 ? all.first : all.last
            return
        }
        self.highlighted = all[min(max(at + delta, 0), all.count - 1)]
    }

    private func pick(_ line: BriefLine) {
        highlighted = line
        keysFocused = true
    }

    private func toggleDone() {
        switch highlighted {
        case let .task(_, id):
            guard let row = model.workRows([id]).first else { return }
            let status: WorkStatus
            if row.status == .done {
                status = reopen.removeValue(forKey: id) ?? .notStarted
            } else {
                reopen[id] = row.status
                status = .done
            }
            model.changeWork { model.work.update([id], WorkPatch(status: status)) }
        case let .point(id):
            finishPoint(id, done: !finished.contains(id))
        case .decision, nil:
            break
        }
    }

    private func openHighlighted() {
        switch highlighted {
        case let .task(_, id):
            close()
            model.go(.tasks)
            model.workFocus.highlighted = id
            model.workFocus.open(id, in: .tasks)
        case let .decision(index):
            play(index)
        case .point, nil:
            break
        }
    }

    private func playHighlighted() {
        switch highlighted {
        case let .task(_, id):
            guard let row = model.workRows([id]).first, let meeting = row.sourceMeetingId,
                  let atMs = row.sourceAtMs
            else { return }
            PlayMoment.toggle(sessionId: meeting, atMs: atMs, model: model)
        case let .decision(index):
            play(index)
        case .point, nil:
            break
        }
    }

    private func play(_ index: Int) {
        guard let last = brief?.lastTime, last.decisions.indices.contains(index),
              let atMs = last.decisions[index].atMs
        else { return }
        PlayMoment.toggle(sessionId: last.meetingId, atMs: atMs, model: model)
    }

    private func openMeeting(_ id: String) {
        close()
        model.openMeeting(id)
    }

    // MARK: Talking points

    private func startPoint(in brief: WorkBrief) {
        guard let person = owner(of: highlighted, in: brief) ?? brief.people.first?.personId else { return }
        adding = person
        point = ""
        DispatchQueue.main.async { fieldFocused = true }
    }

    private func owner(of line: BriefLine?, in brief: WorkBrief) -> String? {
        switch line {
        case let .task(person, _): person
        case let .point(id): brief.people.first { $0.talkingPoints.contains { $0.id == id } }?.personId
        case .decision, nil: nil
        }
    }

    private func addPoint(to personId: String) {
        let text = point.trimmingCharacters(in: .whitespacesAndNewlines)
        adding = nil
        point = ""
        keysFocused = true
        guard !text.isEmpty else { return }
        let draft = WorkTalkingPoint(id: "draft-\(UUID().uuidString)", text: text, createdAtMs: TaskDates.now())
        change(personId) { $0.talkingPoints.append(draft) }
        highlighted = .point(draft.id)
        failure = nil
        Task {
            do {
                let saved: WorkTalkingPoint = try await model.core.request(
                    "work_talking_point_add",
                    TalkingPointAddParams(operationId: UUID().uuidString.lowercased(), personId: personId, text: text))
                change(personId) { person in
                    if let at = person.talkingPoints.firstIndex(where: { $0.id == draft.id }) {
                        person.talkingPoints[at] = saved
                    }
                }
                if highlighted == .point(draft.id) { highlighted = .point(saved.id) }
            } catch {
                change(personId) { $0.talkingPoints.removeAll { $0.id == draft.id } }
                failure = error.localizedDescription
            }
        }
    }

    private func finishPoint(_ id: String, done: Bool) {
        guard !id.hasPrefix("draft-") else { return }
        if done { finished.insert(id) } else { finished.remove(id) }
        failure = nil
        Task {
            do {
                try await model.core.request(
                    "work_talking_point_done",
                    TalkingPointDoneParams(operationId: UUID().uuidString.lowercased(), id: id, done: done))
            } catch {
                if done { finished.remove(id) } else { finished.insert(id) }
                failure = error.localizedDescription
            }
        }
    }

    private func change(_ personId: String, _ edit: (inout WorkBriefPerson) -> Void) {
        guard var shown = brief, let at = shown.people.firstIndex(where: { $0.personId == personId }) else { return }
        edit(&shown.people[at])
        brief = shown
    }

    // MARK: Loading

    private func load() async {
        do {
            let loaded: WorkBrief = try await model.core.request("work_brief", BriefParams(subject: subject))
            brief = loaded
            highlighted = lines(loaded).first
        } catch {
            loadFailure = error.localizedDescription
        }
    }

    private func nothingOpen(_ brief: WorkBrief) -> String {
        let names = brief.people.map(\.name)
        guard !names.isEmpty else { return "Sona doesn't know anyone in this meeting yet." }
        return "Nothing open with \(ListFormatter.localizedString(byJoining: names))."
    }

    private func close() {
        model.sheet = nil
    }
}

private struct BriefTaskRow: View {
    let row: WorkItemRow
    let highlighted: Bool
    let now: Int64

    var body: some View {
        let done = row.status == .done
        HStack(spacing: 10) {
            TaskStatusIcon(status: row.status)
            Text("#\(row.number)")
                .font(TypeScale.meta)
                .foregroundStyle(Theme.inkTertiary)
            Text(row.title)
                .font(TypeScale.list)
                .foregroundStyle(done ? Theme.inkTertiary : Theme.ink)
                .strikethrough(done, color: Theme.inkTertiary)
                .lineLimit(1)
            Spacer(minLength: 8)
            if let due = row.dueAtMs {
                TaskDue(due: due, open: row.status.isOpen, now: now)
            }
            if let meeting = row.sourceMeetingId, let atMs = row.sourceAtMs {
                PlayMoment(sessionId: meeting, atMs: atMs)
            }
        }
        .padding(.horizontal, 10)
        .frame(height: 32)
        .taskHighlight(highlighted, selected: false)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }
}

private struct BriefPointRow: View {
    let point: WorkTalkingPoint
    let done: Bool
    let highlighted: Bool
    let now: Int64

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: done ? "checkmark.circle.fill" : "circle")
                .font(.system(size: 13))
                .foregroundStyle(done ? Theme.accent : Theme.inkTertiary)
                .frame(width: 14)
            Text(point.text)
                .font(TypeScale.list)
                .foregroundStyle(done ? Theme.inkTertiary : Theme.ink)
                .strikethrough(done, color: Theme.inkTertiary)
                .lineLimit(2)
            Spacer(minLength: 8)
            Text(TaskDates.ago(point.createdAtMs, now: now)).metaText()
        }
        .padding(.horizontal, 10)
        .frame(minHeight: 32)
        .taskHighlight(highlighted, selected: false)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }
}

private struct BriefDecisionRow: View {
    let decision: WorkBriefDecision
    let meetingId: String
    let highlighted: Bool

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text("•").bodyText(13, Theme.inkTertiary).accessibilityHidden(true)
            Text(decision.text)
                .bodyText(13)
                .lineLimit(3)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let atMs = decision.atMs {
                PlayMoment(sessionId: meetingId, atMs: atMs)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .taskHighlight(highlighted, selected: false)
        .contentShape(Rectangle())
    }
}
