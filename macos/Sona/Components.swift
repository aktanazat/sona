import SwiftUI

/// One pixel of divider, edge to edge.
struct Hairline: View {
    var body: some View {
        Rectangle().fill(Theme.hairline).frame(height: 1)
    }
}

struct Card<Content: View>: View {
    let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.page)
        .overlay(alignment: .top) { Hairline() }
        .overlay(alignment: .bottom) { Hairline() }
    }
}

/// A row in a card: leading content, trailing meta or controls, a hairline
/// underneath. With an action it is a button to every input: the pointer
/// clicks it, keyboard navigation tabs to it and presses it with space or
/// return, and VoiceOver presses it as one. It stays a plain stack rather
/// than a `Button` so the controls a row may carry in `trailing` keep their
/// own clicks.
struct CardRow<Leading: View, Trailing: View>: View {
    let leading: Leading
    let trailing: Trailing
    var action: (() -> Void)?
    @State private var hovering = false
    @FocusState private var focused: Bool

    init(
        action: (() -> Void)? = nil,
        @ViewBuilder leading: () -> Leading,
        @ViewBuilder trailing: () -> Trailing
    ) {
        self.action = action
        self.leading = leading()
        self.trailing = trailing()
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            leading
            Spacer(minLength: 12)
            trailing
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(action != nil && hovering ? Theme.wash : .clear)
        .overlay(alignment: .bottom) { Hairline() }
        .overlay {
            RoundedRectangle(cornerRadius: Theme.radiusControl)
                .strokeBorder(focused ? Theme.accent : .clear, lineWidth: 2)
        }
        .contentShape(Rectangle())
        .onTapGesture { action?() }
        .onHover { hovering = $0 }
        .focusable(action != nil, interactions: .activate)
        .focused($focused)
        .onKeyPress(.space) { press() }
        .onKeyPress(.return) { press() }
        .accessibilityAddTraits(action != nil ? .isButton : [])
        .accessibilityAction { action?() }
    }

    /// Space or return on the row itself. A control the row carries keeps
    /// its own keys: with focus inside the row, this is not the row's press.
    private func press() -> KeyPress.Result {
        guard focused, let action else { return .ignored }
        action()
        return .handled
    }
}

extension CardRow where Trailing == EmptyView {
    init(action: (() -> Void)? = nil, @ViewBuilder leading: () -> Leading) {
        self.init(action: action, leading: leading) { EmptyView() }
    }
}

/// One quiet sentence in a card, where a row would be too much furniture:
/// "Reading your prompts…", "No activity yet."
struct CardLine: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(text)
            .metaText()
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .overlay(alignment: .bottom) { Hairline() }
    }
}

struct PrimaryButton: ButtonStyle {
    var compact = false
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(TypeScale.label(compact ? 13 : 14))
            .foregroundStyle(Theme.onInvert)
            .padding(.horizontal, compact ? 10 : 12)
            .frame(height: compact ? 28 : 32)
            .background(Theme.invert.opacity(configuration.isPressed ? 0.82 : 1), in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .opacity(isEnabled ? 1 : 0.45)
    }
}

struct SecondaryButton: ButtonStyle {
    var compact = false
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(TypeScale.label(compact ? 13 : 14))
            .foregroundStyle(Theme.ink)
            .padding(.horizontal, compact ? 10 : 12)
            .frame(height: compact ? 28 : 32)
            .background(configuration.isPressed ? Theme.selection : Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
            .opacity(isEnabled ? 1 : 0.45)
    }
}

/// A bare text button in quiet ink, for the third action in a row. Compact
/// matches the label size of the compact buttons it sits beside.
struct QuietButton: ButtonStyle {
    var color: Color = Theme.inkSecondary
    var compact = false
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(TypeScale.label(compact ? 13 : 14))
            .foregroundStyle(configuration.isPressed ? Theme.ink : color)
            .opacity(isEnabled ? 1 : 0.45)
            .contentShape(Rectangle())
    }
}

extension ButtonStyle where Self == PrimaryButton {
    static var primary: PrimaryButton { PrimaryButton() }
}

extension ButtonStyle where Self == SecondaryButton {
    static var secondary: SecondaryButton { SecondaryButton() }
    static var compact: SecondaryButton { SecondaryButton(compact: true) }
}

extension ButtonStyle where Self == QuietButton {
    static var quiet: QuietButton { QuietButton() }
}

/// A key cap: "⌥", "Space".
struct KeyCap: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(text)
            .font(TypeScale.body(12))
            .foregroundStyle(Theme.inkSecondary)
            .fixedSize()
            .padding(.horizontal, 5)
            .frame(height: 20)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusKey).strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// A backend shortcut drawn as macOS key caps: "option+space" becomes "⌥ Space".
struct Shortcut: View {
    let keys: [String]

    init(_ shortcut: String) {
        let separator: (Character) -> Bool = shortcut.contains("+")
            ? { $0 == "+" }
            : { $0.isWhitespace }
        keys = shortcut.split(whereSeparator: separator).map { Self.label(String($0)) }
    }

    var body: some View {
        HStack(spacing: 4) {
            ForEach(Array(keys.enumerated()), id: \.offset) { _, key in
                KeyCap(key)
            }
        }
    }

    private static func label(_ key: String) -> String {
        switch key.lowercased() {
        case "option", "alt": "⌥"
        case "shift": "⇧"
        case "control", "ctrl": "⌃"
        case "command", "cmd", "meta", "super": "⌘"
        case "space": "Space"
        case "return", "enter": "↩"
        case "tab": "⇥"
        case "escape", "esc": "Esc"
        case "backspace": "⌫"
        case "delete": "⌦"
        case "up", "arrowup": "↑"
        case "down", "arrowdown": "↓"
        case "left", "arrowleft": "←"
        case "right", "arrowright": "→"
        default: key.count == 1 ? key.uppercased() : key.capitalized
        }
    }
}

struct SearchField: View {
    let prompt: String
    @Binding var text: String
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 13, weight: .regular))
                .foregroundStyle(Theme.inkTertiary)
            TextField(prompt, text: $text, prompt: Text(prompt).foregroundStyle(Theme.inkTertiary))
                .textFieldStyle(.plain)
                .font(TypeScale.body())
                .foregroundStyle(Theme.ink)
                .focused($focused)
        }
        .padding(.horizontal, 10)
        .frame(height: 32)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(focused ? Theme.accent : Theme.border, lineWidth: 1))
    }
}

/// A plain text input.
struct InputField: View {
    let prompt: String
    @Binding var text: String
    @FocusState private var focused: Bool

    var body: some View {
        TextField(prompt, text: $text, prompt: Text(prompt).foregroundStyle(Theme.inkTertiary))
            .textFieldStyle(.plain)
            .font(TypeScale.body())
            .foregroundStyle(Theme.ink)
            .focused($focused)
            .padding(.horizontal, 10)
            .frame(height: 32)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(focused ? Theme.accent : Theme.border, lineWidth: 1))
    }
}

struct Meter: View {
    let fraction: Double

    var body: some View {
        GeometryReader { proxy in
            ZStack(alignment: .leading) {
                Capsule().fill(Theme.selection)
                Capsule().fill(Theme.accent).frame(width: proxy.size.width * fraction)
            }
        }
        .frame(height: 4)
    }
}

/// The live dot. A ring while idle, red while recording, ink while the core works.
struct LiveDot: View {
    let state: CaptureState

    var body: some View {
        switch state {
        case .idle:
            Circle().strokeBorder(Theme.inkDisabled, lineWidth: 1.5).frame(width: 10, height: 10)
        case .recording:
            Circle().fill(Theme.live).frame(width: 10, height: 10)
        case .working:
            Circle().fill(Theme.ink).frame(width: 10, height: 10)
        }
    }
}

struct Chip: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(text)
            .font(TypeScale.label(12))
            .foregroundStyle(Theme.inkSecondary)
            .padding(.horizontal, 6)
            .frame(height: 20)
            .background(Theme.selection, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
    }
}

/// The head of every page: one title, one line of plain fact, one action.
struct PageTitle<Action: View>: View {
    let title: String
    let subtitle: String
    let action: Action

    init(_ title: String, subtitle: String, @ViewBuilder action: () -> Action) {
        self.title = title
        self.subtitle = subtitle
        self.action = action()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    heading.fixedSize(horizontal: true, vertical: false)
                    Spacer(minLength: 12)
                    action.fixedSize(horizontal: true, vertical: false)
                }
                VStack(alignment: .leading, spacing: 12) {
                    heading
                    action
                }
            }
            if !subtitle.isEmpty {
                Text(subtitle).metaText(Theme.inkSecondary)
            }
        }
        .padding(.bottom, 20)
    }

    private var heading: some View {
        Text(title)
            .titleText()
            .lineLimit(2)
            .accessibilityAddTraits(.isHeader)
    }
}

extension PageTitle where Action == EmptyView {
    init(_ title: String, subtitle: String) {
        self.init(title, subtitle: subtitle) { EmptyView() }
    }
}

struct PageSection<Content: View>: View {
    let label: String
    let content: Content

    init(_ label: String, @ViewBuilder content: () -> Content) {
        self.label = label
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(label).sectionLabel()
            content
        }
        .padding(.bottom, 24)
    }
}

struct Page<Content: View>: View {
    let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                content
            }
            .frame(maxWidth: Theme.contentMax, alignment: .leading)
            .padding(.horizontal, Theme.margin)
            .padding(.top, 24)
            .padding(.bottom, 40)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .scrollIndicators(.automatic)
    }
}

/// "‹ Library": the way back from a detail page.
struct BackLink: View {
    let title: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 6) {
                Image(systemName: "chevron.left").font(.system(size: 12, weight: .semibold))
                Text(title)
            }
        }
        .buttonStyle(.quiet)
        .padding(.bottom, 12)
    }
}

struct ToggleRow: View {
    let title: String
    var detail: String? = nil
    @Binding var isOn: Bool

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).bodyText()
                if let detail {
                    Text(detail).metaText()
                }
            }
        } trailing: {
            // The title is the switch's name for VoiceOver; only its drawing
            // is hidden, since the row already shows it.
            Toggle(title, isOn: $isOn)
                .labelsHidden()
                .toggleStyle(.switch)
                .tint(Theme.accent)
        }
    }
}

/// A number with its label, for the activity row.
struct Stat: View {
    let label: String
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(label).metaText(Theme.inkSecondary)
            Text(value).font(TypeScale.stat).foregroundStyle(Theme.ink).monospacedDigit()
        }
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// The last thing a store could not do, in live red under the title. Shows
/// nothing while there is nothing to say.
struct ErrorNote: View {
    let message: String?

    init(_ message: String?) {
        self.message = message
    }

    var body: some View {
        if let message {
            Text(message)
                .font(TypeScale.body(14))
                .foregroundStyle(Theme.live)
                .textSelection(.enabled)
                .padding(.bottom, 20)
        }
    }
}

/// A row with a menu of choices on the right: "Language — English".
struct ChoiceRow<Choice: Hashable>: View {
    let title: String
    var detail: String? = nil
    let choices: [Choice]
    let label: (Choice) -> String
    @Binding var selection: Choice

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).bodyText()
                if let detail {
                    Text(detail).metaText()
                }
            }
        } trailing: {
            Picker(title, selection: $selection) {
                ForEach(choices, id: \.self) { choice in
                    Text(label(choice)).tag(choice)
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .fixedSize()
        }
    }
}

/// A row whose right side is one button: "Check now", "Reveal".
struct ActionRow: View {
    let title: String
    var detail: String? = nil
    let button: String
    var busy = false
    let action: () -> Void

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).bodyText()
                if let detail {
                    Text(detail).metaText()
                }
            }
        } trailing: {
            Button(button, action: action)
                .buttonStyle(.secondary)
                .disabled(busy)
        }
    }
}
