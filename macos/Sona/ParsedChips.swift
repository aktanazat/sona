import SwiftUI

/// What the core reads in a typed title, kept current as the words change.
/// An answer that arrives after newer words is dropped.
@MainActor @Observable
final class TitleReading {
    private(set) var parse: WorkParse?
    /// Words turned back into text; the core leaves them as they are.
    private(set) var plain: [String] = []
    /// The chip the arrow keys are on.
    var selected: Int?
    @ObservationIgnored private var text = ""
    @ObservationIgnored private var asked = 0
    @ObservationIgnored private var answered = 0

    var tokens: [WorkToken] { parse?.tokens ?? [] }

    func read(_ text: String, in work: WorkStore) async {
        asked += 1
        let mine = asked
        var answer: WorkParse?
        if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            answer = await work.parse(text, plain: plain)
        }
        guard mine == asked else { return }
        answered = mine
        self.text = text
        parse = answer
        if let selected, selected >= tokens.count { self.selected = tokens.indices.last }
    }

    /// The reading of exactly these words, asked again when the last answer
    /// was for others or a newer question is still out.
    func settled(_ text: String, in work: WorkStore) async -> WorkParse? {
        if text != self.text || answered != asked { await read(text, in: work) }
        return text == self.text ? parse : nil
    }

    func keepAsText(_ token: WorkToken, in text: String, work: WorkStore) async {
        plain.append(token.text)
        await read(text, in: work)
    }

    func labelToken(_ id: String) -> WorkToken? {
        tokens.first { token in
            if case let .label(labelId?, _) = token.field { return labelId == id }
            return false
        }
    }

    /// ↓ steps from the title onto the chips, ← → move along them, ⌫ or
    /// Space keeps the chip's words as text, ↑ or Esc goes back to the
    /// title. Any other key goes back too, and on to the title.
    func press(_ key: KeyPress, text: String, work: WorkStore) -> KeyPress.Result {
        let modified = !key.modifiers.isDisjoint(with: [.command, .option, .control])
        guard let index = selected else {
            guard key.key == .downArrow, !modified, !key.modifiers.contains(.shift), !tokens.isEmpty else { return .ignored }
            selected = 0
            return .handled
        }
        if modified {
            selected = nil
            return .ignored
        }
        switch key.key {
        case .leftArrow: selected = max(0, index - 1)
        case .rightArrow: selected = min(tokens.count - 1, index + 1)
        case .upArrow, .escape: selected = nil
        case .delete, .space:
            if tokens.indices.contains(index) {
                let token = tokens[index]
                Task { await keepAsText(token, in: text, work: work) }
            }
        case .downArrow: break
        default:
            selected = nil
            return .ignored
        }
        return .handled
    }
}

/// The fields a title's words set. The last words for a field win; every
/// label counts.
struct TitleFields {
    /// What is left of the title, nil when the words were all fields.
    var title: String?
    var due: Int64?
    var priority: Int?
    var owner: WorkOwner?
    var project: String?
    var labelIds: [String] = []
    /// Labels named in the title that do not exist yet.
    var newLabels: [String] = []
    var recurrence: String?

    init(_ parse: WorkParse?) {
        guard let parse else { return }
        let rest = parse.title.trimmingCharacters(in: .whitespacesAndNewlines)
        title = rest.isEmpty ? nil : rest
        for token in parse.tokens {
            switch token.field {
            case let .due(at, _): due = at
            case let .priority(level): priority = level
            case let .label(id?, _):
                if !labelIds.contains(id) { labelIds.append(id) }
            case .label(nil, let name):
                if !newLabels.contains(where: { $0.caseInsensitiveCompare(name) == .orderedSame }) { newLabels.append(name) }
            case let .owner(owner, _): self.owner = owner
            case let .project(id, _): project = id
            case let .recurrence(every, nextAt):
                recurrence = every
                due = due ?? nextAt
            }
        }
    }

    /// Every label the words name, a new one made for a name that has none.
    @MainActor func labels(making work: WorkStore) -> [String] {
        var ids = labelIds
        for name in newLabels {
            let id = work.labels.first { $0.name.caseInsensitiveCompare(name) == .orderedSame }?.id
                ?? work.makeLabel(named: name)
            if !ids.contains(id) { ids.append(id) }
        }
        return ids
    }
}

extension View {
    /// The title field's keys for the chips under it.
    func chipKeys(_ reading: TitleReading, text: String, work: WorkStore) -> some View {
        onKeyPress(phases: .down) { reading.press($0, text: text, work: work) }
    }
}

/// The fields read from the title, as chips under it. A click keeps a
/// chip's words as text, and so do ⌫ and Space on the chip the arrows are on.
struct ParsedChips: View {
    let reading: TitleReading
    let text: String
    @Environment(AppModel.self) private var model

    var body: some View {
        ChatFlowRow(spacing: Space.xs) {
            ForEach(Array(reading.tokens.enumerated()), id: \.element.id) { index, token in
                ParsedChip(token: token, selected: reading.selected == index) {
                    reading.selected = nil
                    Task { await reading.keepAsText(token, in: text, work: model.work) }
                }
            }
        }
    }
}

private struct ParsedChip: View {
    let token: WorkToken
    let selected: Bool
    let action: () -> Void
    @Environment(AppModel.self) private var model
    @State private var hovering = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusKey)
        Button(action: action) {
            HStack(spacing: Space.xs) {
                mark.frame(width: 16, height: 16)
                Text(title).lineLimit(1)
            }
            .font(TypeScale.label(12))
            .foregroundStyle(Theme.ink)
            .padding(.horizontal, Space.s)
            .frame(height: 22)
            .background(selected ? Theme.selection : hovering ? Theme.controlHover : Theme.control, in: shape)
            .overlay(shape.strokeBorder(selected ? Theme.accent : .clear, lineWidth: 1))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .hoverFade(hovering)
        .help("Click to keep “\(token.text)” as text")
        .accessibilityLabel(spoken)
        .accessibilityHint("Keeps “\(token.text)” as text")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    @ViewBuilder private var mark: some View {
        switch token.field {
        case .due: symbol("calendar")
        case let .priority(level): PriorityIcon(priority: TaskPriority(level))
        case let .label(id, _):
            if let label = model.work.labels.first(where: { $0.id == id }) {
                Circle().fill(Theme.label(hue: label.hue)).frame(width: 8, height: 8)
            } else {
                symbol("tag")
            }
        case let .owner(owner, name):
            Avatar(face: face(owner, name), size: 16, revision: model.people.photoRevision)
        case .project: symbol("folder")
        case .recurrence: symbol("repeat")
        }
    }

    private func symbol(_ name: String) -> some View {
        Image(systemName: name).font(.system(size: 11)).foregroundStyle(Theme.inkSecondary)
    }

    private func face(_ owner: WorkOwner, _ name: String) -> AvatarFace {
        switch owner {
        case .myself: AvatarFace(kind: .me, name: name)
        case let .person(id): AvatarFace(kind: .person(id), name: name)
        case .nobody: AvatarFace(kind: .unassigned, name: name)
        }
    }

    private var title: String {
        switch token.field {
        case let .due(at, timed):
            let day = TaskDates.label(at, now: TaskDates.now())
            if timed {
                let moment = Date(timeIntervalSince1970: TimeInterval(at) / 1000)
                return "\(day) \(moment.formatted(date: .omitted, time: .shortened))"
            }
            return day
        case let .priority(level): return TaskPriority(level).label
        case let .label(id, name): return model.work.labels.first { $0.id == id }?.name ?? name
        case let .owner(_, name): return name
        case let .project(_, title): return title
        case let .recurrence(every, _): return Recurrence.display(every)
        }
    }

    private var spoken: String {
        switch token.field {
        case .due: "Due \(title)"
        case .priority: "Priority \(title)"
        case .label: "Label \(title)"
        case .owner: "Owner \(title)"
        case .project: "Project \(title)"
        case .recurrence: title
        }
    }
}

/// How a repeat reads on screen: "every monday" shows as "Every Monday".
enum Recurrence {
    private static let names: Set<String> = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.locale = Locale(identifier: "en_US_POSIX")
        let symbols = calendar.weekdaySymbols + calendar.shortWeekdaySymbols
            + calendar.monthSymbols + calendar.shortMonthSymbols
        return Set(symbols.map { $0.lowercased() })
    }()

    static func display(_ every: String) -> String {
        let words = every.split(separator: " ", omittingEmptySubsequences: false).map { word in
            names.contains(word.lowercased()) ? word.capitalized : String(word)
        }
        let line = words.joined(separator: " ")
        return line.prefix(1).uppercased() + line.dropFirst()
    }
}
