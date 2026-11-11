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
                    if let parent = parent(of: row, detail: detail) {
                        ParentLink(parent: parent, place: place)
                    }
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
                .modifier(AttachmentDrop(itemId: row.id, number: row.number))
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
        .environment(\.openURL, OpenURLAction { url in
            guard url.scheme == TaskMention.scheme else { return .systemAction }
            guard let number = Int64(url.host() ?? ""),
                  let target = store.items?.first(where: { $0.number == number }) else { return .discarded }
            model.workFocus.highlighted = target.id
            model.workFocus.open(target.id, in: place)
            return .handled
        })
    }

    private func parent(of row: WorkItemRow, detail: WorkItemDetail?) -> WorkItemRef? {
        guard let id = row.parentId else { return nil }
        if let known = detail?.parent, known.id == id { return known }
        return model.workRows([id]).first.map { WorkItemRef(id: $0.id, number: $0.number, title: $0.title, status: $0.status) }
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
            if let error = model.work.listError {
                HStack(spacing: 10) {
                    Image(systemName: "exclamationmark.triangle").font(.system(size: 12)).foregroundStyle(Theme.live)
                    Text(error).metaText(Theme.live).lineLimit(2)
                    Button("Retry") { Task { await model.work.refresh() } }
                        .buttonStyle(QuietButton(compact: true))
                }
                .padding(.bottom, 16)
            }
            if row.status == .duplicate {
                DuplicateBanner(canonical: row.duplicateOfId, place: place)
                    .padding(.bottom, 16)
            }
            DetailTitle(row: row)
            ProposalStrip(proposals: model.work.proposals(for: row.id))
            DetailNotes(row: row)
                .padding(.top, 10)
            DetailSource(row: row)
            DetailSubtasks(row: row, subTasks: detail?.subTasks, place: place)
                .padding(.top, 32)
            DetailAttachments(row: row, attachments: detail?.attachments)
                .padding(.top, 32)
            if row.status == .triage {
                DetailSuggestions(suggestions: detail?.suggestions ?? model.work.suggestions[row.id] ?? [])
            }
            DetailEvidence(row: row, links: detail?.evidence ?? [])
            DetailRelations(row: row, relations: detail?.relations, place: place)
                .padding(.top, 32)
            DetailActivity(row: row, activity: detail?.activity)
                .padding(.top, 32)
            DetailComments(row: row, comments: detail?.comments)
                .padding(.top, 24)
        }
    }
}

/// On a duplicate: the task it repeats, which opens on a click.
private struct DuplicateBanner: View {
    let canonical: String?
    let place: Place
    @Environment(AppModel.self) private var model
    @State private var hovering = false

    var body: some View {
        let other = canonical.flatMap { model.workRows([$0]).first }
        HStack(spacing: 10) {
            Image(systemName: "square.on.square")
                .font(.system(size: 12))
                .foregroundStyle(Theme.statusDuplicate)
            Text("Duplicate of").metaText()
            if let other {
                TaskStatusIcon(status: other.status)
                Text("#\(other.number)").font(TypeScale.mono(12)).foregroundStyle(Theme.inkTertiary)
                Text(other.title).bodyText(13).lineLimit(1)
            } else {
                Text("an archived task").bodyText(13, Theme.inkTertiary)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .frame(height: 36)
        .background(hovering && other != nil ? Theme.wash : Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
        .hoverFade(hovering)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .onTapGesture {
            guard let other else { return }
            model.workFocus.highlighted = other.id
            model.workFocus.open(other.id, in: place)
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(other == nil ? [] : .isButton)
        .help(other == nil ? "" : "Open the original")
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

/// The details as Markdown: lists, checklists, links and code. A click
/// edits the text itself, kept when the editor is left; Esc puts it back.
private struct DetailNotes: View {
    let row: WorkItemRow
    @Environment(AppModel.self) private var model
    @State private var text = ""
    @State private var editing = false
    @FocusState private var focused: Bool
    @State private var hovering = false

    var body: some View {
        let details = row.details ?? ""
        Group {
            if editing || details.isEmpty {
                editor
            } else {
                MarkdownBody(source: details) { line in
                    toggle(line, in: details)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.trailing, 32)
                .contentShape(Rectangle())
                .onTapGesture(count: 2) { edit() }
                .overlay(alignment: .topTrailing) {
                    Button { edit() } label: { Image(systemName: "pencil") }
                        .buttonStyle(.icon)
                        .opacity(hovering ? 1 : 0)
                        .help("Edit details")
                        .accessibilityLabel("Edit details")
                }
                .onHover { hovering = $0 }
                .accessibilityElement(children: .contain)
                .accessibilityLabel("Details")
                .accessibilityAction(named: "Edit") { edit() }
                .help("Double-click to edit")
            }
        }
        .onChange(of: row.details, initial: true) { _, details in
            if !focused { text = details ?? "" }
        }
    }

    private var editor: some View {
        ZStack(alignment: .topLeading) {
            Text(text.isEmpty ? " " : text + "\n")
                .font(TypeScale.body(14))
                .lineSpacing(3)
                .frame(maxWidth: .infinity, alignment: .leading)
                .hidden()
            if text.isEmpty {
                Text("Add details, a checklist or links").font(TypeScale.body(14)).foregroundStyle(Theme.inkDisabled)
                    .allowsHitTesting(false)
            }
            TextEditor(text: $text)
                .font(TypeScale.body(14))
                .lineSpacing(3)
                .foregroundStyle(Theme.ink)
                .textEditorStyle(.plain)
                .scrollContentBackground(.hidden)
                .scrollDisabled(true)
                .padding(.horizontal, -5)
                .focused($focused)
                .onExitCommand {
                    text = row.details ?? ""
                    focused = false
                }
                .accessibilityLabel("Details")
        }
        .onChange(of: focused) { _, now in
            if !now {
                commit()
                editing = false
            }
        }
    }

    private func edit() {
        editing = true
        focused = true
    }

    private func commit() {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed != (row.details ?? "") else { return }
        save(trimmed)
    }

    private func toggle(_ line: Int, in details: String) {
        save(MarkdownBody.toggled(details, line: line))
    }

    private func save(_ details: String) {
        let value: WorkPatchValue<String> = details.isEmpty ? .clear : .set(details)
        let id = row.id
        model.changeWork { model.work.update([id], WorkPatch(details: value)) }
    }
}

/// Markdown read as blocks: headings, lists, checklists, quotes and code,
/// with links, emphasis and inline code inside each. `toggle` checks or
/// unchecks the checklist item on that line of the source.
private struct MarkdownBody: View {
    let source: String
    let toggle: (Int) -> Void

    private enum Block {
        case paragraph(String)
        case heading(Int, String)
        case bullet(Int, String)
        case numbered(Int, String, String)
        case check(line: Int, indent: Int, done: Bool, text: String)
        case quote(String)
        case code(String)
        case gap
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(Self.blocks(source).enumerated()), id: \.offset) { _, block in
                view(block)
            }
        }
    }

    @ViewBuilder private func view(_ block: Block) -> some View {
        switch block {
        case let .paragraph(text):
            Self.inline(text).bodyText(14).fixedSize(horizontal: false, vertical: true)
        case let .heading(level, text):
            Self.inline(text)
                .font(.system(size: level == 1 ? 18 : level == 2 ? 16 : 14, weight: .semibold))
                .foregroundStyle(Theme.ink)
                .padding(.top, 4)
                .accessibilityAddTraits(.isHeader)
        case let .bullet(indent, text):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("•").bodyText(14, Theme.inkTertiary).accessibilityHidden(true)
                Self.inline(text).bodyText(14).fixedSize(horizontal: false, vertical: true)
            }
            .padding(.leading, CGFloat(indent) * 18)
        case let .numbered(indent, number, text):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(number).bodyText(14, Theme.inkTertiary).monospacedDigit()
                Self.inline(text).bodyText(14).fixedSize(horizontal: false, vertical: true)
            }
            .padding(.leading, CGFloat(indent) * 18)
        case let .check(line, indent, done, text):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Button { toggle(line) } label: {
                    Image(systemName: done ? "checkmark.square.fill" : "square")
                        .font(.system(size: 13))
                        .foregroundStyle(done ? Theme.accent : Theme.inkTertiary)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(String(Self.attributed(text).characters))
                .accessibilityValue(done ? "Checked" : "Unchecked")
                .accessibilityAddTraits(.isToggle)
                Self.inline(text).bodyText(14, done ? Theme.inkSecondary : Theme.ink)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityHidden(true)
            }
            .padding(.leading, CGFloat(indent) * 18)
        case let .quote(text):
            HStack(spacing: 10) {
                Rectangle().fill(Theme.hairline).frame(width: 2)
                Self.inline(text).bodyText(14, Theme.inkSecondary).fixedSize(horizontal: false, vertical: true)
            }
            .fixedSize(horizontal: false, vertical: true)
        case let .code(text):
            Text(text)
                .font(TypeScale.mono(12))
                .foregroundStyle(Theme.ink)
                .fixedSize(horizontal: false, vertical: true)
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusRow))
        case .gap:
            Color.clear.frame(height: 2)
        }
    }

    /// Emphasis, links and inline code, with bare web addresses and task
    /// mentions like S-12 made links.
    private static func inline(_ text: String) -> Text {
        Text(attributed(text))
    }

    private static func attributed(_ text: String) -> AttributedString {
        var string = (try? AttributedString(
            markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        )) ?? AttributedString(text)
        let plain = String(string.characters)
        func link(_ range: Range<String.Index>, to url: URL?) {
            guard let url else { return }
            let start = string.characters.index(string.startIndex, offsetBy: plain.distance(from: plain.startIndex, to: range.lowerBound))
            let end = string.characters.index(start, offsetBy: plain.distance(from: range.lowerBound, to: range.upperBound))
            if string[start..<end].link == nil { string[start..<end].link = url }
        }
        if let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) {
            for match in detector.matches(in: plain, range: NSRange(plain.startIndex..., in: plain)) {
                guard let range = Range(match.range, in: plain) else { continue }
                link(range, to: match.url)
            }
        }
        for match in plain.matches(of: /\bS-(\d+)\b/) {
            link(match.range, to: TaskMention.url(match.output.1))
        }
        return string
    }

    private static func blocks(_ source: String) -> [Block] {
        var blocks: [Block] = []
        var paragraph: [String] = []
        var code: [String]?
        func flush() {
            if !paragraph.isEmpty { blocks.append(.paragraph(paragraph.joined(separator: "\n"))) }
            paragraph = []
        }
        for (index, raw) in source.components(separatedBy: "\n").enumerated() {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if let lines = code {
                if line.hasPrefix("```") {
                    blocks.append(.code(lines.joined(separator: "\n")))
                    code = nil
                } else {
                    code = lines + [raw]
                }
                continue
            }
            let indent = raw.prefix { $0 == " " || $0 == "\t" }.reduce(0) { $0 + ($1 == "\t" ? 2 : 1) } / 2
            if line.hasPrefix("```") {
                flush()
                code = []
            } else if line.isEmpty {
                flush()
                if case .gap? = blocks.last {} else if !blocks.isEmpty { blocks.append(.gap) }
            } else if let match = line.wholeMatch(of: /(#{1,3}) (.+)/) {
                flush()
                blocks.append(.heading(match.1.count, String(match.2)))
            } else if let match = line.wholeMatch(of: /[-*+] \[([ xX])\](?: (.*))?/) {
                flush()
                blocks.append(.check(line: index, indent: indent, done: match.1 != " ", text: String(match.2 ?? "")))
            } else if let match = line.wholeMatch(of: /[-*+] (.+)/) {
                flush()
                blocks.append(.bullet(indent, String(match.1)))
            } else if let match = line.wholeMatch(of: /(\d+[.)]) (.+)/) {
                flush()
                blocks.append(.numbered(indent, String(match.1), String(match.2)))
            } else if let match = line.wholeMatch(of: />\s?(.*)/) {
                flush()
                blocks.append(.quote(String(match.1)))
            } else {
                paragraph.append(raw)
            }
        }
        flush()
        if let code { blocks.append(.code(code.joined(separator: "\n"))) }
        return blocks
    }

    /// The source with the checklist item on `line` checked or unchecked.
    static func toggled(_ source: String, line: Int) -> String {
        var lines = source.components(separatedBy: "\n")
        guard lines.indices.contains(line), let open = lines[line].firstIndex(of: "[") else { return source }
        let mark = lines[line].index(after: open)
        guard mark < lines[line].endIndex else { return source }
        lines[line].replaceSubrange(mark...mark, with: lines[line][mark] == " " ? "x" : " ")
        return lines.joined(separator: "\n")
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
        if !others.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                DetailHeading(title: "Evidence")
                ForEach(others) { link in
                    EvidenceLine(link: link)
                }
            }
            .padding(.top, 32)
        }
    }
}

private struct DetailSource: View {
    let row: WorkItemRow

    var body: some View {
        if let quote = row.sourceQuote {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 8) {
                    DetailHeading(title: "Source")
                    if row.raisedIn > 1 {
                        Text("Raised in \(row.raisedIn) meetings").metaText()
                    }
                }
                QuoteCard(quote: quote, speaker: row.sourceSpeaker, meetingTitle: row.sourceMeetingTitle,
                          meetingId: row.sourceMeetingId, atMs: row.sourceAtMs)
            }
            .padding(.top, 24)
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
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: "waveform")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.accent)
                    .frame(width: 16)
                Text(speaker ?? "Someone").font(TypeScale.label(13)).foregroundStyle(Theme.ink).lineLimit(1)
                if let meetingTitle {
                    Text(meetingTitle).metaText().lineLimit(1)
                }
                Spacer(minLength: 8)
                if let atMs {
                    Text(atMs.meetingMsClock).font(TypeScale.mono(11)).foregroundStyle(Theme.inkTertiary)
                }
            }
            HStack(alignment: .top, spacing: 12) {
                RoundedRectangle(cornerRadius: 1).fill(Theme.accent.opacity(0.45)).frame(width: 2)
                Text(quote)
                    .font(TypeScale.body(15))
                    .lineSpacing(3)
                    .foregroundStyle(Theme.ink)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
            .fixedSize(horizontal: false, vertical: true)
            .padding(.leading, 7)
            HStack(spacing: 14) {
                if let meetingId, let atMs {
                    PlayMoment(sessionId: meetingId, atMs: atMs, prominent: true)
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
                Spacer(minLength: 0)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
    }
}

/// Plays a meeting from a moment, a few seconds early for context; the same
/// button stops it.
struct PlayMoment: View {
    let sessionId: String
    let atMs: Int64
    var prominent = false
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
            let button = Button {
                Self.toggle(sessionId: sessionId, atMs: atMs, model: model)
            } label: {
                Label(running ? "Stop" : "Play", systemImage: running ? "stop.fill" : "play.fill")
            }
            Group {
                if prominent {
                    button.buttonStyle(PrimaryButton(compact: true))
                } else {
                    button.buttonStyle(QuietButton(color: Theme.accent, compact: true))
                }
            }
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
        let entries = RelationEntry.all(for: row.id, in: relations ?? []) { id in
            model.workRows([id]).first?.status.isOpen ?? true
        }
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
                    WorkPickerList(picker: WorkPicker(kind: kind.picker, ids: [row.id]), compact: true) {
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

    var picker: WorkPicker.Kind {
        switch self {
        case .blocks: .blocks
        case .blockedBy: .blockedBy
        case .related: .related
        }
    }
}

/// One relation from this task's side. Once the blocking task is done, the
/// link no longer blocks and reads as related.
private struct RelationEntry: Identifiable {
    let id: String
    let title: String
    let other: String

    static func all(for id: String, in relations: [WorkRelation], isOpen: (String) -> Bool) -> [RelationEntry] {
        let entries = relations.compactMap { relation -> RelationEntry? in
            switch relation.kind {
            case .blocks where relation.fromId == id:
                RelationEntry(id: relation.id, title: isOpen(id) ? "Blocks" : "Related", other: relation.toId)
            case .blocks where relation.toId == id:
                RelationEntry(id: relation.id, title: isOpen(relation.fromId) ? "Blocked by" : "Related", other: relation.fromId)
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

private struct DetailSubtasks: View {
    let row: WorkItemRow
    let subTasks: [WorkItem]?
    let place: Place
    @Environment(AppModel.self) private var model
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        let tasks = subTasks ?? []
        let done = tasks.filter { $0.status == .done }.count
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 10) {
                DetailHeading(title: "Sub-tasks")
                if !tasks.isEmpty {
                    Text("\(done)/\(tasks.count)").metaText().monospacedDigit()
                    Gauge(value: Double(done), in: 0...Double(tasks.count)) {}
                        .gaugeStyle(.accessoryLinearCapacity)
                        .tint(Theme.accent)
                        .frame(width: 72)
                        .accessibilityLabel("\(done) of \(tasks.count) done")
                }
                Spacer(minLength: 0)
            }
            .padding(.bottom, 6)
            ForEach(tasks) { task in
                SubtaskLine(item: task, place: place)
            }
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: "plus")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(focused ? Theme.accent : Theme.inkTertiary)
                    .frame(width: 16)
                TextField("", text: $text, prompt: Text(tasks.isEmpty ? "Add a sub-task, or paste a list" : "Add a sub-task")
                    .foregroundStyle(Theme.inkDisabled), axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(TypeScale.body(13))
                    .focused($focused)
                    .onSubmit(add)
                    .onExitCommand {
                        text = ""
                        focused = false
                    }
                    .onChange(of: text) { _, now in
                        if now.contains("\n") { add() }
                    }
                    .accessibilityLabel("Add a sub-task")
                    .onChange(of: row.id) {
                        text = ""
                        focused = false
                    }
            }
            .padding(.horizontal, 8)
            .frame(minHeight: 32)
            .background(focused ? Theme.inset : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusRow))
        }
    }

    private func add() {
        let titles = text.components(separatedBy: .newlines).map { line in
            line.replacing(/^\s*(?:[-*+•]|\d+[.)])\s+(?:\[[ xX]\]\s+)?/, with: "")
        }
        text = ""
        model.work.createSubtasks(under: row.id, titles: titles)
    }
}

private struct SubtaskLine: View {
    let item: WorkItem
    let place: Place
    @Environment(AppModel.self) private var model
    @State private var hovering = false

    var body: some View {
        let finished = item.status == .done || item.status == .canceled
        HStack(spacing: 10) {
            Button {
                let id = item.id
                let status: WorkStatus = finished ? .notStarted : .done
                model.changeWork { model.work.update([id], WorkPatch(status: status)) }
            } label: {
                TaskStatusIcon(status: item.status).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(finished ? "Reopen" : "Mark done")
            .accessibilityLabel(finished ? "Reopen \(item.title)" : "Mark \(item.title) done")
            Text("#\(item.number)").font(TypeScale.mono(12)).foregroundStyle(Theme.inkTertiary)
                .frame(minWidth: 28, alignment: .leading)
            Text(item.title)
                .bodyText(13, finished ? Theme.inkTertiary : Theme.ink)
                .strikethrough(item.status == .canceled)
                .lineLimit(1)
            Spacer(minLength: 8)
            if let due = item.dueAtMs {
                Text(TaskDates.label(due, now: TaskDates.now())).metaText()
            }
            Button {
                let id = item.id
                model.changeWork { model.work.setParent(of: id, to: nil) }
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .bold))
                    .frame(width: 20, height: 20)
                    .contentShape(Rectangle())
            }
            .buttonStyle(QuietButton(compact: true))
            .opacity(hovering ? 1 : 0)
            .help("Remove from this task")
            .accessibilityLabel("Remove \(item.title) from this task")
        }
        .padding(.horizontal, 8)
        .frame(height: 32)
        .background(hovering ? Theme.wash : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusRow))
        .hoverFade(hovering)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .onTapGesture {
            model.workFocus.highlighted = item.id
            model.workFocus.open(item.id, in: place)
        }
        .accessibilityElement(children: .contain)
        .accessibilityAddTraits(.isButton)
    }
}

private struct ParentLink: View {
    let parent: WorkItemRef
    let place: Place
    @Environment(AppModel.self) private var model
    @State private var hovering = false

    var body: some View {
        Button {
            model.workFocus.highlighted = parent.id
            model.workFocus.open(parent.id, in: place)
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.inkTertiary)
                TaskStatusIcon(status: parent.status)
                Text("#\(parent.number)").font(TypeScale.mono(12)).foregroundStyle(Theme.inkTertiary)
                Text(parent.title).bodyText(13, hovering ? Theme.ink : Theme.inkSecondary).lineLimit(1)
            }
            .padding(.horizontal, 6)
            .frame(height: 24)
            .background(hovering ? Theme.controlHover : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help("Open the parent task")
        .accessibilityLabel("Parent, #\(parent.number) \(parent.title)")
    }
}

private struct DetailComments: View {
    let row: WorkItemRow
    let comments: [WorkComment]?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(comments ?? []) { thread in
                CommentThread(thread: thread, itemId: row.id)
            }
            CommentComposer(itemId: row.id, prompt: "Leave a comment")
        }
    }
}

private struct CommentThread: View {
    let thread: WorkComment
    let itemId: String
    @State private var replying = false
    @State private var expanded = false

    var body: some View {
        let resolved = thread.resolvedAtMs != nil
        VStack(alignment: .leading, spacing: 0) {
            if resolved && !expanded {
                Button { expanded = true } label: {
                    HStack(spacing: 8) {
                        Image(systemName: "checkmark.circle.fill")
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.inkTertiary)
                            .frame(width: 16)
                        Text("Resolved thread from \(thread.author.display)").bodyText(13, Theme.inkSecondary)
                        if !thread.replies.isEmpty {
                            Text(thread.replies.count == 1 ? "1 reply" : "\(thread.replies.count) replies").metaText()
                        }
                        Spacer(minLength: 0)
                        Image(systemName: "chevron.down").font(.system(size: 10)).foregroundStyle(Theme.inkTertiary)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Show the thread")
            } else {
                CommentEntry(comment: thread, thread: thread, itemId: itemId) { replying = true }
                ForEach(thread.replies) { reply in
                    CommentEntry(comment: reply, thread: thread, itemId: itemId) { replying = true }
                        .padding(.leading, 24)
                }
                if replying {
                    CommentComposer(itemId: itemId, replyTo: thread.id, prompt: "Reply", autofocus: true) {
                        replying = false
                    }
                    .padding(.leading, 24)
                    .padding(.top, 6)
                } else if !resolved {
                    Button("Reply") { replying = true }
                        .buttonStyle(QuietButton(compact: true))
                        .padding(.leading, 24)
                        .padding(.top, 2)
                }
                if resolved {
                    Button("Hide") { expanded = false }
                        .buttonStyle(QuietButton(compact: true))
                        .padding(.top, 4)
                }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard).strokeBorder(Theme.hairline))
    }
}

private struct CommentEntry: View {
    let comment: WorkComment
    let thread: WorkComment
    let itemId: String
    let reply: () -> Void
    @Environment(AppModel.self) private var model
    @State private var editing = false
    @State private var hovering = false

    var body: some View {
        let root = comment.id == thread.id
        let resolved = thread.resolvedAtMs != nil
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                ActorMark(actor: comment.author.kind)
                Text(comment.author.display).font(TypeScale.label(13)).foregroundStyle(Theme.ink)
                Text(TaskDates.ago(comment.createdAtMs, now: TaskDates.now()))
                    .metaText()
                    .help(PlanningFormat.date(comment.createdAtMs))
                if comment.editedAtMs != nil {
                    Text("edited").metaText()
                }
                Spacer(minLength: 8)
                if !comment.deleted && !editing {
                    HStack(spacing: 2) {
                        if !resolved {
                            action("Reply", "arrowshape.turn.up.left", reply)
                        }
                        if root {
                            action(resolved ? "Reopen" : "Resolve", resolved ? "arrow.uturn.backward" : "checkmark") {
                                run { await model.work.resolveComment(thread.id, on: itemId, resolved: !resolved) }
                            }
                        }
                        if comment.author.kind == .me {
                            action("Edit", "pencil") { editing = true }
                        }
                        action("Delete", "trash") {
                            run { await model.work.deleteComment(comment.id, on: itemId) }
                        }
                    }
                    .opacity(hovering ? 1 : 0)
                }
            }
            .frame(height: 22)
            Group {
                if comment.deleted {
                    Text("Deleted").bodyText(13, Theme.inkTertiary).italic()
                } else if editing, comment.author.kind == .me {
                    CommentComposer(itemId: itemId, editing: comment, prompt: "Edit comment", autofocus: true) {
                        editing = false
                    }
                } else if let body = comment.body {
                    MarkdownBody(source: body) { _ in }
                        .textSelection(.enabled)
                }
            }
            .padding(.leading, 24)
        }
        .padding(.vertical, 6)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .accessibilityElement(children: .contain)
    }

    private func action(_ title: String, _ icon: String, _ run: @escaping () -> Void) -> some View {
        Button(action: run) {
            Image(systemName: icon)
                .font(.system(size: 11))
                .frame(width: 24, height: 22)
                .contentShape(Rectangle())
        }
        .buttonStyle(QuietButton(compact: true))
        .help(title)
        .accessibilityLabel(title)
    }

    private func run(_ work: @escaping @MainActor () async -> Bool) {
        Task { _ = await work() }
    }
}

private struct CommentComposer: View {
    let itemId: String
    var replyTo: String?
    var editing: WorkComment?
    let prompt: String
    var autofocus = false
    var done: () -> Void = {}
    @Environment(AppModel.self) private var model
    @State private var text = ""
    @State private var sending = false
    @FocusState private var focused: Bool

    var body: some View {
        let empty = text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let nested = editing != nil || replyTo != nil
        VStack(alignment: .trailing, spacing: 8) {
            ZStack(alignment: .topLeading) {
                Text(text.isEmpty ? " " : text + "\n")
                    .font(TypeScale.body(13))
                    .lineSpacing(2)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .hidden()
                if text.isEmpty {
                    Text(prompt).font(TypeScale.body(13)).foregroundStyle(Theme.inkDisabled)
                        .allowsHitTesting(false)
                }
                TextEditor(text: $text)
                    .font(TypeScale.body(13))
                    .lineSpacing(2)
                    .foregroundStyle(Theme.ink)
                    .textEditorStyle(.plain)
                    .scrollContentBackground(.hidden)
                    .scrollDisabled(true)
                    .padding(.horizontal, -5)
                    .focused($focused)
                    .onKeyPress(.return, phases: .down) { press in
                        guard press.modifiers.contains(.command) else { return .ignored }
                        post()
                        return .handled
                    }
                    .onExitCommand {
                        focused = false
                        if nested { done() }
                    }
                    .accessibilityLabel(prompt)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.radiusControl)
                    .strokeBorder(focused ? Theme.accent.opacity(0.6) : Theme.hairline, lineWidth: 1)
            )
            .contentShape(Rectangle())
            .onTapGesture { focused = true }
            if focused || !empty || nested {
                HStack(spacing: 12) {
                    Text("⌘↩").metaText()
                    if nested {
                        Button("Cancel") { done() }.buttonStyle(QuietButton(compact: true))
                    }
                    Button(editing != nil ? "Save" : replyTo != nil ? "Reply" : "Comment") { post() }
                        .buttonStyle(PrimaryButton(compact: true))
                        .disabled(empty || sending)
                }
            }
        }
        .onAppear {
            if let editing { text = editing.body ?? "" }
            if autofocus { focused = true }
        }
    }

    private func post() {
        let body = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !body.isEmpty, !sending else { return }
        sending = true
        let work = model.work
        Task {
            let saved = if let editing {
                await work.editComment(editing.id, on: itemId, body: body)
            } else {
                await work.addComment(on: itemId, body: body, replyTo: replyTo)
            }
            sending = false
            guard saved else { return }
            text = ""
            done()
        }
    }
}

enum TaskMention {
    static let scheme = "sona-task"

    static func url(_ number: Substring) -> URL? {
        URL(string: "\(scheme)://\(number)")
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
/// inspector and the peek. Tab or a click focuses one; return or space opens
/// it, the arrows move between them.
struct TaskPropertyList: View {
    let row: WorkItemRow
    @Environment(AppModel.self) private var model
    @FocusState private var focused: PropertyField?
    @State private var opened: PropertyField?

    var body: some View {
        let work = model.work
        let ids = [row.id]
        VStack(alignment: .leading, spacing: 2) {
            PropertyLine(title: "Status") {
                WorkPickerButton(kind: .status, ids: ids, presented: shown(.status)) {
                    PropertyValue(text: row.status.label) { TaskStatusIcon(status: row.status) }
                }
                .modifier(focus(.status))
            }
            PropertyLine(title: "Priority") {
                WorkPickerButton(kind: .priority, ids: ids, presented: shown(.priority)) {
                    PropertyValue(text: row.priority == 0 ? "Set priority" : TaskPriority(row.priority).label,
                                  faint: row.priority == 0) {
                        PriorityIcon(priority: TaskPriority(row.priority))
                    }
                }
                .modifier(focus(.priority))
            }
            PropertyLine(title: "Owner") {
                WorkPickerButton(kind: .owner, ids: ids, presented: shown(.owner)) {
                    let unowned = !row.ownerIsSelf && row.ownerPersonId == nil
                    PropertyValue(text: unowned ? "Add owner" : row.ownerFace.name, faint: unowned) {
                        Avatar(face: row.ownerFace, size: 16, revision: model.people.photoRevision)
                    }
                }
                .modifier(focus(.owner))
            }
            PropertyLine(title: "Agent") {
                WorkPickerButton(kind: .delegate, ids: ids, presented: shown(.agent)) {
                    let waiting = row.doer != nil && model.agents.waits(on: row)
                    PropertyValue(text: row.doer.map { waiting ? "\($0.agent), waiting for you" : $0.agent } ?? "Add agent",
                                  faint: row.doer == nil) {
                        Image(systemName: "sparkle").font(.system(size: 12))
                            .foregroundStyle(waiting ? Theme.live : Theme.inkSecondary)
                    }
                }
                .modifier(focus(.agent))
            }
            PropertyLine(title: "Due date") {
                WorkPickerButton(kind: .due, ids: ids, presented: shown(.due)) {
                    PropertyValue(text: row.dueAtMs.map(PlanningFormat.day) ?? "Add due date", faint: row.dueAtMs == nil) {
                        Image(systemName: "calendar").font(.system(size: 12)).foregroundStyle(Theme.inkSecondary)
                    }
                }
                .modifier(focus(.due))
            }
            PropertyLine(title: "Repeats") {
                RepeatMenu(row: row, rule: row.repeat?.every
                    ?? work.detail.flatMap({ $0.item.id == row.id ? $0.recurrence : nil }), shown: shown(.repeats))
                    .modifier(focus(.repeats))
            }
            if row.status.isOpen || row.cycle != nil {
                PropertyLine(title: "Cycle") {
                    WorkPickerButton(kind: .cycle, ids: ids, presented: shown(.cycle)) {
                        PropertyValue(text: row.cycle.map { "Cycle \($0)" } ?? "Add to cycle", faint: row.cycle == nil) {
                            Image(systemName: "arrow.triangle.2.circlepath").font(.system(size: 12))
                                .foregroundStyle(Theme.inkSecondary)
                        }
                    }
                    .modifier(focus(.cycle))
                }
            }
            if row.status.isOpen {
                PropertyLine(title: "Time") {
                    if let block = row.timeBlock {
                        PropertyValue(text: TaskBlockText.when(block)) {
                            Image(systemName: "calendar.badge.checkmark").font(.system(size: 12))
                                .foregroundStyle(Theme.inkSecondary)
                        }
                    } else {
                        Button { work.book(row.id) } label: {
                            PropertyValue(text: row.atRisk ? "At risk, book time" : "Book time",
                                          faint: !row.atRisk) {
                                Image(systemName: row.atRisk ? "exclamationmark.circle.fill" : "calendar.badge.clock")
                                    .font(.system(size: 12))
                                    .foregroundStyle(row.atRisk ? Theme.live : Theme.inkSecondary)
                            }
                        }
                        .buttonStyle(.plain)
                        .help("Book the next free hour before it is due  B")
                        .modifier(focus(.time) { work.book(row.id) })
                    }
                }
            }
            PropertyLine(title: "Project") {
                WorkPickerButton(kind: .project, ids: ids, presented: shown(.project)) {
                    PropertyValue(text: row.projectTitle ?? work.projects.first { $0.id == row.projectId }?.title ?? "Add to project",
                                  faint: row.projectId == nil) {
                        Image(systemName: "folder").font(.system(size: 12)).foregroundStyle(Theme.inkSecondary)
                    }
                }
                .modifier(focus(.project))
            }
            if let projectId = row.projectId {
                PropertyLine(title: "Milestone") {
                    MilestoneMenu(row: row, projectId: projectId, shown: shown(.milestone))
                        .modifier(focus(.milestone))
                }
            }
            PropertyLine(title: "Labels") {
                WorkPickerButton(kind: .labels, ids: ids, presented: shown(.labels)) {
                    let labels = row.labelIds.compactMap { id in work.labels.first { $0.id == id } }
                    if labels.isEmpty {
                        PropertyValue(text: "Add labels", faint: true) {
                            Image(systemName: "tag").font(.system(size: 12)).foregroundStyle(Theme.inkSecondary)
                        }
                    } else {
                        FlowLabels(labels: labels)
                    }
                }
                .modifier(focus(.labels))
            }
            PropertyLine(title: "Parent") {
                WorkPickerButton(kind: .parent, ids: ids, presented: shown(.parent)) {
                    let parent = row.parentId.flatMap { model.workRows([$0]).first }
                    PropertyValue(text: parent.map { "#\($0.number) \($0.title)" } ?? (row.parentId == nil ? "Set parent" : "A task"),
                                  faint: row.parentId == nil) {
                        Image(systemName: "arrow.turn.left.up").font(.system(size: 12)).foregroundStyle(Theme.inkSecondary)
                    }
                }
                .help(row.parentId.flatMap { model.workRows([$0]).first }.map { "#\($0.number) \($0.title)" } ?? "")
                .modifier(focus(.parent))
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
        .onChange(of: focused) { _, field in model.workFocus.propertyFocused = field != nil }
        .onDisappear { model.workFocus.propertyFocused = false }
    }

    private var fields: [PropertyField] {
        var list: [PropertyField] = [.status, .priority, .owner, .agent, .due, .repeats]
        if row.status.isOpen || row.cycle != nil { list.append(.cycle) }
        if row.status.isOpen, row.timeBlock == nil { list.append(.time) }
        list.append(.project)
        if row.projectId != nil { list.append(.milestone) }
        return list + [.labels, .parent]
    }

    private func shown(_ field: PropertyField) -> Binding<Bool> {
        Binding(get: { opened == field }, set: { open in
            opened = open ? field : nil
            guard !open else { return }
            DispatchQueue.main.async {
                NSApp.keyWindow?.makeFirstResponder(nil)
                focused = field
            }
        })
    }

    private func focus(_ field: PropertyField, activate: (() -> Void)? = nil) -> PropertyFocus {
        PropertyFocus(field: field, fields: fields, focused: $focused) {
            if let activate { activate() } else { opened = field }
        }
    }
}

enum PropertyField: Hashable {
    case status, priority, owner, agent, due, repeats, cycle, time, project, milestone, labels, parent
}

private struct PropertyFocus: ViewModifier {
    let field: PropertyField
    let fields: [PropertyField]
    var focused: FocusState<PropertyField?>.Binding
    let activate: () -> Void

    func body(content: Content) -> some View {
        content
            .focusable(interactions: .edit)
            .focused(focused, equals: field)
            .focusEffectDisabled()
            .overlay {
                RoundedRectangle(cornerRadius: Theme.radiusControl)
                    .strokeBorder(Theme.accent, lineWidth: 2)
                    .opacity(focused.wrappedValue == field ? 1 : 0)
                    .allowsHitTesting(false)
            }
            .onKeyPress(keys: [.return, .space]) { _ in
                activate()
                return .handled
            }
            .onKeyPress(.upArrow) { step(-1) }
            .onKeyPress(.downArrow) { step(1) }
    }

    private func step(_ delta: Int) -> KeyPress.Result {
        guard let index = fields.firstIndex(of: field) else { return .ignored }
        focused.wrappedValue = fields[min(max(index + delta, 0), fields.count - 1)]
        return .handled
    }
}

private struct MilestoneMenu: View {
    let row: WorkItemRow
    let projectId: String
    let shown: Binding<Bool>
    @Environment(AppModel.self) private var model
    @State private var milestones: [PlanningMilestone] = []

    var body: some View {
        let current = milestones.first { $0.id == row.milestoneId }
        Button { shown.wrappedValue = true } label: {
            PropertyValue(text: current?.title ?? (row.milestoneId == nil ? "Add milestone" : "A milestone"),
                          faint: row.milestoneId == nil) {
                Image(systemName: "flag").font(.system(size: 12)).foregroundStyle(Theme.inkSecondary)
            }
        }
        .buttonStyle(.plain)
        .popover(isPresented: shown, arrowEdge: .bottom) {
            PropertyChoices(choices: [PropertyChoice(id: "none", title: "No milestone", on: row.milestoneId == nil) { set(nil) }]
                + milestones.map { milestone in
                    PropertyChoice(id: milestone.id, title: milestone.title, on: milestone.id == row.milestoneId) { set(milestone.id) }
                }) { shown.wrappedValue = false }
        }
        .task(id: projectId) { milestones = await model.projects.milestones(of: projectId) }
        .accessibilityLabel("Milestone")
    }

    private func set(_ id: String?) {
        let task = row.id
        model.changeWork { model.work.update([task], WorkPatch(milestoneId: id.map { .set($0) } ?? .clear)) }
    }
}

private struct RepeatMenu: View {
    let row: WorkItemRow
    let rule: String?
    let shown: Binding<Bool>
    @Environment(AppModel.self) private var model
    private static let rules = ["every day", "every weekday", "every monday", "every month"]

    var body: some View {
        let kept = row.repeat.flatMap { $0.of > 0 ? ", kept \($0.kept) of \($0.of)" : nil } ?? ""
        Button { shown.wrappedValue = true } label: {
            PropertyValue(text: rule.map { Recurrence.display($0) + kept } ?? "Set repeat", faint: rule == nil) {
                Image(systemName: "repeat").font(.system(size: 12)).foregroundStyle(Theme.inkSecondary)
            }
        }
        .buttonStyle(.plain)
        .popover(isPresented: shown, arrowEdge: .bottom) {
            PropertyChoices(choices: Self.rules.map { every in
                PropertyChoice(id: every, title: Recurrence.display(every), on: every == rule) { set(every) }
            } + (rule == nil ? [] : [PropertyChoice(id: "none", title: "Don't repeat", on: false) { set(nil) }])) {
                shown.wrappedValue = false
            }
        }
        .accessibilityLabel("Repeats")
    }

    private func set(_ every: String?) {
        let task = row.id
        model.changeWork { model.work.update([task], WorkPatch(recurrence: every.map { .set($0) } ?? .clear)) }
    }
}

private struct PropertyChoice: Identifiable {
    let id: String
    let title: String
    let on: Bool
    let pick: () -> Void
}

/// A short list in a popover, for properties with a few fixed answers:
/// the arrows move, return picks.
private struct PropertyChoices: View {
    let choices: [PropertyChoice]
    let done: () -> Void
    @State private var index = 0
    @State private var hovered: String?
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(choices.enumerated()), id: \.element.id) { offset, choice in
                Button { pick(choice) } label: {
                    HStack(spacing: 12) {
                        Text(choice.title).bodyText(13)
                        Spacer(minLength: 0)
                        Image(systemName: "checkmark")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(Theme.accent)
                            .opacity(choice.on ? 1 : 0)
                    }
                    .padding(.horizontal, 10)
                    .frame(height: 28)
                    .background(offset == index || hovered == choice.id ? Theme.controlHover : .clear,
                                in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .onHover { hovered = $0 ? choice.id : nil }
            }
        }
        .padding(4)
        .frame(minWidth: 200)
        .focusable()
        .focused($focused)
        .focusEffectDisabled()
        .onKeyPress(.upArrow) { move(-1) }
        .onKeyPress(.downArrow) { move(1) }
        .onKeyPress(.return) {
            guard choices.indices.contains(index) else { return .ignored }
            pick(choices[index])
            return .handled
        }
        .onAppear {
            index = choices.firstIndex(where: \.on) ?? 0
            focused = true
        }
    }

    private func move(_ delta: Int) -> KeyPress.Result {
        guard !choices.isEmpty else { return .ignored }
        index = min(max(index + delta, 0), choices.count - 1)
        return .handled
    }

    private func pick(_ choice: PropertyChoice) {
        choice.pick()
        done()
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
        case .recurrence:
            guard let every = entry.to, !every.isEmpty else { return "stopped it repeating" }
            let shown = Recurrence.display(every)
            return "set it to repeat \(shown.prefix(1).lowercased() + shown.dropFirst())"
        case .cycle:
            if let to = entry.to { return entry.from == nil ? "planned it in cycle \(to)" : "moved it to cycle \(to)" }
            return "took it out of its cycle"
        case .block:
            guard let start = entry.to.flatMap({ Int64($0) }) else { return "cleared its booked time" }
            return "booked time \(PlanningFormat.day(start))"
        case .parent:
            guard let to = entry.to, !to.isEmpty else { return "took it out of its parent" }
            return "made it a sub-task of \(task(to, model: model))"
        case .comment:
            return "commented"
        case .attachment:
            if let added = entry.to { return "attached “\(added)”" }
            if let removed = entry.from { return "removed “\(removed)”" }
            return "changed an attachment"
        case .unknown:
            return "changed the task"
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

enum TaskRepeatText {
    /// "Every week, kept 3 of 4".
    static func line(_ repeating: WorkRepeat) -> String {
        let rule = Recurrence.display(repeating.every)
        return repeating.of > 0 ? "\(rule), kept \(repeating.kept) of \(repeating.of)" : rule
    }
}

enum TaskBlockText {
    static func when(_ block: WorkTimeBlock) -> String {
        let start = PlanningFormat.moment(block.startsAtMs)
        let end = PlanningFormat.moment(block.endsAtMs)
        return (start..<end).formatted(.interval.weekday(.abbreviated).hour().minute())
    }
}
