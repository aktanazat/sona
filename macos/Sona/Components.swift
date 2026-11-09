import SwiftUI

extension View {
    /// A control floating over the page: the layer Liquid Glass is for, when
    /// the glass material is chosen. Content underneath stays the standard
    /// surface.
    @ViewBuilder
    func floatingControl(_ material: AppearanceMaterial) -> some View {
        if material == .glass, #available(macOS 26.0, *) {
            glassEffect(.regular, in: Capsule())
        } else {
            background(material == .glass ? AnyShapeStyle(.regularMaterial) : AnyShapeStyle(Theme.surface), in: Capsule())
                .overlay(Capsule().strokeBorder(Theme.border, lineWidth: 1))
                .elevation(.high)
        }
    }
}

/// One device pixel of divider, across or, with `.vertical`, down.
struct Hairline: View {
    var axis: Axis = .horizontal
    @Environment(\.displayScale) private var displayScale

    var body: some View {
        let pixel = 1 / displayScale
        Rectangle().fill(Theme.hairline)
            .frame(width: axis == .vertical ? pixel : nil, height: axis == .horizontal ? pixel : nil)
    }
}

/// A card is content, so it never takes Liquid Glass or a material: Apple
/// keeps those for the controls floating over content, and one solid card
/// reads the same on every page.
struct Card<Content: View>: View {
    let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusCard)
        cardContent
            .background(Theme.surface)
            .clipShape(shape)
            .overlay(shape.strokeBorder(Theme.border, lineWidth: 1))
    }

    private var cardContent: some View {
        VStack(alignment: .leading, spacing: 0) {
            content
        }
        // Each row draws its own separator underneath; pulling the stack up
        // one point lets the clip take the last row's, so a card never ends
        // on a line.
        .padding(.bottom, -1)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A row in a card: leading content, trailing meta or controls, a hairline
/// underneath. With an action it is a button to every input: the pointer
/// clicks it, keyboard navigation tabs to it and presses it with space or
/// return, and VoiceOver reads it as one element whose press is the row's,
/// with the controls it carries listed as its other actions. It stays a
/// plain stack rather than a `Button` so those controls keep their own clicks.
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
        if let action {
            row
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(.isButton)
                .accessibilityAction { action() }
        } else {
            row
        }
    }

    private var row: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            leading
            Spacer(minLength: 12)
            trailing
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(action != nil && hovering ? Theme.wash : .clear)
        .hoverFade(hovering)
        .overlay(alignment: .bottom) { Hairline().padding(.horizontal, 16) }
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
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .overlay(alignment: .bottom) { Hairline().padding(.horizontal, 16) }
    }
}

struct PrimaryButton: ButtonStyle {
    var compact = false
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(TypeScale.label(13))
            .foregroundStyle(Theme.onAccent)
            .padding(.horizontal, compact ? 10 : 14)
            .frame(height: compact ? Theme.compactHeight : Theme.controlHeight)
            .background(Theme.accentFill.opacity(configuration.isPressed ? 0.8 : 1), in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .opacity(isEnabled ? 1 : 0.45)
            .animation(Motion.quick, value: configuration.isPressed)
    }
}

struct SecondaryButton: ButtonStyle {
    var compact = false
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(TypeScale.label(13))
            .foregroundStyle(Theme.ink)
            .padding(.horizontal, compact ? 10 : 14)
            .frame(height: compact ? Theme.compactHeight : Theme.controlHeight)
            .background(
                configuration.isPressed ? Theme.controlPressed : hovering && isEnabled ? Theme.controlHover : Theme.control,
                in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .onHover { hovering = $0 }
            .opacity(isEnabled ? 1 : 0.45)
            .hoverFade(hovering)
            .animation(Motion.quick, value: configuration.isPressed)
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
            .font(TypeScale.label(13))
            .foregroundStyle(configuration.isPressed ? Theme.ink : color)
            .opacity(isEnabled ? 1 : 0.45)
            .contentShape(Rectangle())
            .animation(Motion.quick, value: configuration.isPressed)
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

/// A square icon button: back, add, more, close. `on` keeps it filled while
/// what it toggles is showing.
struct IconButton: ButtonStyle {
    var on = false
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusControl)
        configuration.label
            .font(.system(size: 12, weight: .medium))
            .foregroundStyle(on || hovering && isEnabled ? Theme.ink : Theme.inkSecondary)
            .frame(width: Theme.controlHeight, height: Theme.controlHeight)
            .background(
                configuration.isPressed ? Theme.controlPressed
                    : on ? Theme.control
                    : hovering && isEnabled ? Theme.controlHover : .clear,
                in: shape)
            .contentShape(shape)
            .onHover { hovering = $0 }
            .opacity(isEnabled ? 1 : 0.45)
            .hoverFade(hovering)
            .animation(Motion.quick, value: configuration.isPressed)
    }
}

extension ButtonStyle where Self == IconButton {
    static var icon: IconButton { IconButton() }
}

/// A key cap: "⌥", "Space".
struct KeyCap: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(text)
            .font(.system(size: 12, weight: .semibold))
            .foregroundStyle(Theme.inkSecondary)
            .fixedSize()
            .padding(.horizontal, 6)
            .frame(minWidth: 22, minHeight: 22)
            .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
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
        .frame(height: Theme.controlHeight)
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
            .padding(.horizontal, 12)
            .frame(height: Theme.controlHeight)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(focused ? Theme.accent : Theme.border, lineWidth: 1))
    }
}

struct Meter: View {
    let fraction: Double

    var body: some View {
        GeometryReader { proxy in
            ZStack(alignment: .leading) {
                Capsule().fill(Theme.track)
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
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(Theme.inkSecondary)
            .padding(.horizontal, 6)
            .frame(height: 18)
            .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
    }
}

/// The head of every page: its title and its one action in the window's
/// toolbar, on the line of the close, minimize and zoom buttons, and one line
/// of plain fact at the top of the column. A page with tabs leads with them
/// instead of its title.
struct PageTitle<Lead: View, Action: View>: View {
    let subtitle: String
    let lead: Lead
    let action: Action

    init(subtitle: String, @ViewBuilder lead: () -> Lead, @ViewBuilder action: () -> Action) {
        self.subtitle = subtitle
        self.lead = lead()
        self.action = action()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if !subtitle.isEmpty {
                Text(subtitle).metaText(Theme.inkSecondary)
                    .padding(.bottom, 20)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .pageBar {
            lead
        } trailing: {
            action
        }
    }
}

extension PageTitle where Lead == PageHeading {
    init(_ title: String, subtitle: String, @ViewBuilder action: () -> Action) {
        self.init(subtitle: subtitle, lead: { PageHeading(title: title) }, action: action)
    }
}

extension PageTitle where Lead == PageHeading, Action == EmptyView {
    init(_ title: String, subtitle: String) {
        self.init(title, subtitle: subtitle) { EmptyView() }
    }
}

/// A page's name where its bar starts.
struct PageHeading: View {
    let title: String

    var body: some View {
        Text(title)
            .titleText()
            .lineLimit(1)
            .accessibilityAddTraits(.isHeader)
    }
}

/// How far a page's bar runs: over the page's column, or across the whole
/// pane the way a list's bar does.
enum PageBarSpan {
    case column
    case full
}

extension EnvironmentValues {
    /// The width of the pane beside the sidebar. A page's bar sits in the
    /// window's toolbar, outside the page, so it takes the pane's width from
    /// here to line up with the column under it.
    @Entry var paneWidth: CGFloat = 0
}

extension View {
    /// A page's bar: what the page is on the left, its actions on the right,
    /// in the window's toolbar on the line of the close, minimize and zoom
    /// buttons. Below that line the window takes a click as the start of a
    /// drag, and only the toolbar's own items answer one there.
    func pageBar<Leading: View, Trailing: View>(
        _ span: PageBarSpan = .column,
        @ViewBuilder leading: () -> Leading,
        @ViewBuilder trailing: () -> Trailing
    ) -> some View {
        modifier(PageBar(span: span, leading: leading(), trailing: trailing()))
    }

    /// Keeps the window's toolbar while a page shows no bar: without an item
    /// the toolbar goes, and the close, minimize and zoom buttons jump to the
    /// window's corner.
    func keepsWindowToolbar() -> some View {
        toolbar {
            if #available(macOS 26.0, *) {
                toolbarAnchor.sharedBackgroundVisibility(.hidden)
            } else {
                toolbarAnchor
            }
        }
    }

    private var toolbarAnchor: some ToolbarContent {
        ToolbarItem(placement: .automatic) {
            Color.clear.frame(width: 1, height: 1).accessibilityHidden(true)
        }
    }
}

/// The toolbar centers its middle item in the pane beside the sidebar, so an
/// item as wide as the page's column lines up with the column. Across the
/// whole pane it keeps a list's 16 points on the left and 24 on the right.
/// The toolbar sizes an item once, when it adds it, so the item's identity
/// carries its width: a resized pane brings a new item. A kept item stayed
/// at its first width and went to the toolbar's overflow when the window
/// narrowed, out of reach of a click.
/// Its own glass is left off: the controls in it bring their own shapes.
private struct PageBar<Leading: View, Trailing: View>: ViewModifier {
    let span: PageBarSpan
    let leading: Leading
    let trailing: Trailing
    @Environment(\.paneWidth) private var paneWidth

    func body(content: Content) -> some View {
        content.toolbar {
            if #available(macOS 26.0, *) {
                item.sharedBackgroundVisibility(.hidden)
            } else {
                item
            }
        }
    }

    private var item: some ToolbarContent {
        ToolbarItem(id: "page-bar-\(Int(width))", placement: .principal) {
            HStack(spacing: 10) {
                leading
                Spacer(minLength: 12)
                trailing
            }
            .labelStyle(.titleAndIcon)
            .padding(.trailing, span == .full ? 8 : 0)
            .frame(width: width)
        }
    }

    private var width: CGFloat {
        switch span {
        case .column: max(0, min(Theme.contentMax, paneWidth - 2 * Theme.margin))
        case .full: max(0, paneWidth - 32)
        }
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
        VStack(alignment: .leading, spacing: 10) {
            Text(label).sectionLabel()
                .accessibilityAddTraits(.isHeader)
            content
        }
        .padding(.bottom, 28)
    }
}

/// Where a detail page goes back to: the word on its back button and the
/// step that takes it there.
struct PageBack {
    let title: String
    let action: () -> Void

    init(_ title: String, action: @escaping () -> Void) {
        self.title = title
        self.action = action
    }
}

/// The scrolling column every screen sits in. A detail page names where it
/// goes back to; its way back, its name once the headline has scrolled away
/// and its actions make its bar in the window's toolbar.
///
/// `pinned` sits at the top of the pane and never scrolls, so a page whose
/// column changes under its own tabs keeps them where they are.
struct Page<Content: View, Actions: View, Pinned: View>: View {
    let back: PageBack?
    let title: String?
    let content: Content
    let actions: Actions
    let pinned: Pinned

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var scroll = PageScroll()
    @State private var headlineBottom: CGFloat?

    init(
        back: PageBack? = nil,
        title: String? = nil,
        @ViewBuilder content: () -> Content,
        @ViewBuilder actions: () -> Actions,
        @ViewBuilder pinned: () -> Pinned
    ) {
        self.back = back
        self.title = title
        self.content = content()
        self.actions = actions()
        self.pinned = pinned()
    }

    var body: some View {
        if let back {
            pane.pageBar {
                bar(back)
            } trailing: {
                actions
            }
        } else {
            pane
        }
    }

    @ViewBuilder
    private var pane: some View {
        if #available(macOS 26.0, *) {
            column
                .safeAreaBar(edge: .top, spacing: 0) { chrome }
                .scrollEdgeEffectStyle(.hard, for: .top)
        } else {
            column.safeAreaInset(edge: .top, spacing: 0) {
                chrome.background { Theme.page.ignoresSafeArea(edges: .top) }
            }
        }
    }

    private var column: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                content
            }
            .frame(maxWidth: Theme.contentMax, alignment: .leading)
            .padding(.horizontal, Theme.margin)
            .padding(.top, Space.m)
            .padding(.bottom, 40)
            .frame(maxWidth: .infinity)
            .coordinateSpace(.named(PageHeadlineKey.space))
        }
        .scrollIndicators(.automatic)
        .onScrollGeometryChange(for: PageScroll.self) { geometry in
            guard back != nil else { return PageScroll() }
            return PageScroll(
                offset: geometry.contentOffset.y + geometry.contentInsets.top,
                headlineBottom: headlineBottom)
        } action: { _, next in
            withAnimation(reduceMotion ? nil : .easeOut(duration: 0.16)) { scroll = next }
        }
        .onPreferenceChange(PageHeadlineKey.self) { headlineBottom = $0 }
    }

    private var chrome: some View {
        VStack(alignment: .leading, spacing: 0) {
            pinned
        }
        .frame(maxWidth: Theme.contentMax, alignment: .leading)
        .padding(.horizontal, Theme.margin)
        .frame(maxWidth: .infinity)
    }

    /// "‹ People › Noah Sample": the way back, then the page's name once its
    /// own headline has scrolled away.
    private func bar(_ back: PageBack) -> some View {
        HStack(spacing: 10) {
            BackLink(title: back.title, action: back.action)
                .keyboardShortcut("[", modifiers: .command)
            if let title, scroll.pastHeadline {
                HStack(spacing: 10) {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Theme.inkTertiary)
                        .accessibilityHidden(true)
                    Text(title).headlineText().lineLimit(1)
                }
                .transition(.opacity)
            }
        }
    }
}

extension Page where Pinned == EmptyView {
    init(
        back: PageBack? = nil,
        title: String? = nil,
        @ViewBuilder content: () -> Content,
        @ViewBuilder actions: () -> Actions
    ) {
        self.init(back: back, title: title, content: content, actions: actions) { EmptyView() }
    }
}

extension Page where Actions == EmptyView {
    init(
        back: PageBack? = nil,
        title: String? = nil,
        @ViewBuilder content: () -> Content,
        @ViewBuilder pinned: () -> Pinned
    ) {
        self.init(back: back, title: title, content: content, actions: { EmptyView() }, pinned: pinned)
    }
}

extension Page where Actions == EmptyView, Pinned == EmptyView {
    init(back: PageBack? = nil, title: String? = nil, @ViewBuilder content: () -> Content) {
        self.init(back: back, title: title, content: content, actions: { EmptyView() }, pinned: { EmptyView() })
    }
}

/// Whether a detail page has scrolled past its own headline, the step its
/// bar shows the page's name at.
private struct PageScroll: Equatable {
    var pastHeadline = false

    init() {}

    init(offset: CGFloat, headlineBottom: CGFloat?) {
        pastHeadline = headlineBottom.map { offset > $0 } ?? false
    }
}

/// Where a detail page's headline ends, measured down its column.
private struct PageHeadlineKey: PreferenceKey {
    static let space = "page.column"
    static let defaultValue: CGFloat? = nil

    static func reduce(value: inout CGFloat?, nextValue: () -> CGFloat?) {
        value = nextValue() ?? value
    }
}

extension View {
    /// Marks the line a detail page's bar repeats once it has scrolled away.
    func pageHeadline() -> some View {
        background {
            GeometryReader { proxy in
                Color.clear.preference(
                    key: PageHeadlineKey.self,
                    value: proxy.frame(in: .named(PageHeadlineKey.space)).maxY)
            }
        }
    }
}

/// "‹ Library": the way back from a detail page. The chevron starts on the
/// column's edge; the hover fill reaches past it into the margin.
struct BackLink: View {
    let title: String
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 6) {
                Image(systemName: "chevron.left")
                    .font(.system(size: 12, weight: .medium))
                Text(title).font(TypeScale.label(13))
            }
            .foregroundStyle(hovering ? Theme.ink : Theme.inkSecondary)
            .frame(height: Theme.controlHeight)
            .background {
                RoundedRectangle(cornerRadius: Theme.radiusControl)
                    .fill(hovering ? Theme.controlHover : .clear)
                    .padding(.horizontal, -8)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .hoverFade(hovering)
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
                    Text(detail).metaText(Theme.inkSecondary)
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
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(value).font(TypeScale.stat).foregroundStyle(Theme.ink).monospacedDigit()
                .contentTransition(reduceMotion ? .opacity : .numericText())
                .animation(Motion.settle(reduced: reduceMotion), value: value)
            Text(label).metaText(Theme.inkSecondary)
        }
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
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
            .controlSize(.large)
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
