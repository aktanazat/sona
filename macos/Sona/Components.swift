import SwiftUI

/// One pixel of divider, edge to edge.
struct Hairline: View {
    var body: some View {
        Rectangle().fill(Theme.hairline).frame(height: 1)
    }
}

/// A content card: white surface, one hairline outline, 14-point corners, flat.
/// Rows inside draw their own divider along the bottom; the last one lands on
/// the outline and disappears into it.
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
        .background(Theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusCard))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard).strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// A row in a card: leading content, trailing meta or controls, a hairline
/// underneath. Tappable when it has an action.
struct CardRow<Leading: View, Trailing: View>: View {
    let leading: Leading
    let trailing: Trailing
    var action: (() -> Void)?
    @State private var hovering = false

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
        HStack(alignment: .firstTextBaseline, spacing: 20) {
            leading
            Spacer(minLength: 16)
            trailing
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(action != nil && hovering ? Theme.wash : .clear)
        .overlay(alignment: .bottom) { Hairline() }
        .contentShape(Rectangle())
        .onTapGesture { action?() }
        .onHover { hovering = $0 }
    }
}

extension CardRow where Trailing == EmptyView {
    init(action: (() -> Void)? = nil, @ViewBuilder leading: () -> Leading) {
        self.init(action: action, leading: leading) { EmptyView() }
    }
}

/// The primary button: ink fill, page text, 10-point corners.
struct PrimaryButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(TypeScale.label())
            .foregroundStyle(Theme.onInvert)
            .padding(.horizontal, 16)
            .frame(height: 36)
            .background(Theme.invert.opacity(configuration.isPressed ? 0.82 : 1), in: RoundedRectangle(cornerRadius: Theme.radiusControl))
    }
}

/// The secondary button: white surface, hairline outline, ink text.
struct SecondaryButton: ButtonStyle {
    var compact = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(TypeScale.label(compact ? 13 : 15))
            .foregroundStyle(Theme.ink)
            .padding(.horizontal, compact ? 12 : 16)
            .frame(height: compact ? 30 : 36)
            .background(configuration.isPressed ? Theme.selection : Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// A bare text button in quiet ink, for the third action in a row.
struct QuietButton: ButtonStyle {
    var color: Color = Theme.inkSecondary

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(TypeScale.label())
            .foregroundStyle(configuration.isPressed ? Theme.ink : color)
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
            .font(TypeScale.body(13))
            .foregroundStyle(Theme.inkSecondary)
            .padding(.horizontal, 7)
            .frame(height: 24)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusKey).strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// A shortcut drawn as its key caps: "⌥ Space" becomes two caps.
struct Shortcut: View {
    let keys: [String]

    init(_ shortcut: String) {
        keys = shortcut.split(separator: " ").map(String.init)
    }

    var body: some View {
        HStack(spacing: 4) {
            ForEach(keys, id: \.self) { KeyCap($0) }
        }
    }
}

/// The search field: white, outlined, a magnifier on the left.
struct SearchField: View {
    let prompt: String
    @Binding var text: String

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(Theme.inkTertiary)
            TextField(prompt, text: $text, prompt: Text(prompt).foregroundStyle(Theme.inkTertiary))
                .textFieldStyle(.plain)
                .font(TypeScale.body())
                .foregroundStyle(Theme.ink)
        }
        .padding(.horizontal, 14)
        .frame(height: 40)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// A plain text input.
struct InputField: View {
    let prompt: String
    @Binding var text: String

    var body: some View {
        TextField(prompt, text: $text, prompt: Text(prompt).foregroundStyle(Theme.inkTertiary))
            .textFieldStyle(.plain)
            .font(TypeScale.body())
            .foregroundStyle(Theme.ink)
            .padding(.horizontal, 12)
            .frame(height: 36)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// A measurement that never animates: a bronze bar on a hairline track.
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

/// A small bronze tag: "2 open", "On".
struct Chip: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(text)
            .font(TypeScale.label(12))
            .foregroundStyle(Theme.accent)
            .padding(.horizontal, 8)
            .frame(height: 22)
            .background(Theme.accentSoft, in: RoundedRectangle(cornerRadius: 6))
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
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 8) {
                Text(title).titleText()
                Text(subtitle).bodyText(15, Theme.inkSecondary)
            }
            Spacer()
            action
        }
        .padding(.bottom, 28)
    }
}

extension PageTitle where Action == EmptyView {
    init(_ title: String, subtitle: String) {
        self.init(title, subtitle: subtitle) { EmptyView() }
    }
}

/// A labelled block: "Activity", then a card.
struct PageSection<Content: View>: View {
    let label: String
    let content: Content

    init(_ label: String, @ViewBuilder content: () -> Content) {
        self.label = label
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(label).sectionLabel()
            content
        }
        .padding(.bottom, 32)
    }
}

/// A page: scrolls, keeps the old margins, caps the measure.
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
            .padding(.top, 40)
            .padding(.bottom, 64)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .scrollIndicators(.never)
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
        .padding(.bottom, 20)
    }
}

/// A row with a bronze switch on the right.
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
            Toggle("", isOn: $isOn)
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
        VStack(alignment: .leading, spacing: 8) {
            Text(label).metaText(Theme.inkSecondary)
            Text(value).font(TypeScale.stat).foregroundStyle(Theme.ink).tracking(-0.4)
        }
        .padding(20)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
