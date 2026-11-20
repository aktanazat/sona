import SwiftUI

private let toolsAnimation = Animation.snappy(duration: 0.24)

/// The top bar's first button: opens the tools, goes back from a tool's own field, and
/// returns to the keys.
struct ToolsButton: View {
    let tools: KeyboardTools

    var body: some View {
        Button {
            withAnimation(toolsAnimation) {
                if tools.composing != nil { tools.back() } else { tools.toggle() }
            }
        } label: {
            Image(systemName: symbol)
                .font(.system(size: 17, weight: .medium))
                .contentTransition(.symbolEffect(.replace))
                .frame(width: 38, height: 38)
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .glassEffect(tools.isOpen ? .regular.tint(Theme.accent.opacity(0.22)).interactive() : .regular.interactive(), in: Circle())
        .frame(width: 44, height: 44)
        .sensoryFeedback(.selection, trigger: tools.isOpen)
        .accessibilityLabel(Text(tools.composing != nil ? "tools.back" : tools.isOpen ? "tools.close" : "tools.open"))
        .accessibilityIdentifier("keyboard-tools")
    }

    private var symbol: String {
        if tools.composing != nil { return "chevron.backward" }
        return tools.isOpen ? "keyboard.chevron.compact.down" : "sparkles"
    }
}

/// The tools' own text field in the top bar, which the letter keys type into.
struct ComposeBar: View {
    let tools: KeyboardTools
    let purpose: KeyboardTools.Compose

    var body: some View {
        HStack(spacing: 8) {
            HStack(spacing: 1) {
                if !tools.composeText.isEmpty {
                    Text(verbatim: tools.composeText).lineLimit(1).truncationMode(.head)
                }
                Caret()
                if tools.composeText.isEmpty {
                    Text(placeholder).foregroundStyle(Theme.textSecondary).lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .font(.system(size: 16))
            .padding(.horizontal, 14)
            .frame(height: 38)
            .glassEffect(.regular, in: Capsule())
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text(placeholder))
            .accessibilityValue(Text(verbatim: tools.composeText))
            .accessibilityIdentifier("tools-compose")
            Button {
                withAnimation(toolsAnimation) { tools.submit() }
            } label: {
                Image(systemName: "arrow.up")
                    .font(.system(size: 16, weight: .semibold))
                    .frame(width: 38, height: 38)
            }
            .buttonStyle(.glassProminent)
            .foregroundStyle(.white)
            .buttonBorderShape(.circle)
            .disabled(tools.composeText.trimmingCharacters(in: .whitespaces).isEmpty)
            .accessibilityLabel(Text("tools.send"))
            .accessibilityIdentifier("tools-send")
        }
    }

    private var placeholder: LocalizedStringKey {
        switch purpose {
        case .ask: "tools.askPlaceholder"
        case .commandPrompt: "tools.commandPromptPlaceholder"
        case .commandName: "tools.commandNamePlaceholder"
        case .snippetKeyword: "tools.keywordPlaceholder"
        }
    }
}

private struct Caret: View {
    var body: some View {
        PhaseAnimator([true, false]) { on in
            RoundedRectangle(cornerRadius: 1)
                .fill(Theme.accent)
                .frame(width: 2, height: 20)
                .opacity(on ? 1 : 0)
        } animation: { _ in .easeInOut(duration: 0.45).delay(0.1) }
        .accessibilityHidden(true)
    }
}

/// A short message over the keys, with Undo after a change to the field.
struct ToolsNotice: View {
    let tools: KeyboardTools

    var body: some View {
        if let notice = tools.notice {
            HStack(spacing: 10) {
                Text(notice.text).font(.footnote.weight(.medium)).lineLimit(2)
                if notice.undoable, tools.lastChange != nil {
                    Button("tools.undo") { withAnimation(toolsAnimation) { tools.undo() } }
                        .font(.footnote.weight(.semibold))
                        .accessibilityIdentifier("tools-undo-notice")
                }
            }
            .padding(.horizontal, 14)
            .frame(minHeight: 34)
            .glassEffect(.regular, in: Capsule())
            .padding(.top, 4)
            .transition(.move(edge: .top).combined(with: .opacity))
            .id(notice.id)
            .accessibilityIdentifier("tools-notice")
        }
    }
}

/// The tools in place of the letter keys.
struct ToolsPanel: View {
    let tools: KeyboardTools
    let canCopy: Bool

    var body: some View {
        VStack(spacing: 6) {
            header.frame(height: 34)
            content.frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .padding(.horizontal, 6)
        .padding(.top, 2)
        .animation(toolsAnimation, value: tools.page)
        .sensoryFeedback(.selection, trigger: tools.page)
    }

    @ViewBuilder private var header: some View {
        switch tools.page {
        case let .browse(tab, nil):
            TabBar(tools: tools, selected: tab)
        case let .browse(_, menu?):
            BackHeader(tools: tools, title: Text(Self.title(of: menu)))
        case let .working(action):
            BackHeader(tools: tools, title: action.title)
        case let .answer(answer):
            BackHeader(tools: tools, title: answer.action?.title ?? Text("tools.ai.ask"))
        case .review:
            BackHeader(tools: tools, title: Text("tools.review"))
        case .closed, .compose:
            EmptyView()
        }
    }

    @ViewBuilder private var content: some View {
        switch tools.page {
        case .browse(.ai, nil): AIGrid(tools: tools)
        case .browse(.ai, .translate?): TranslateMenu(tools: tools)
        case .browse(.ai, .rewrite?): RewriteMenu(tools: tools)
        case .browse(.ai, .commands?): CommandsMenu(tools: tools)
        case .browse(.ai, .changeCase?):
            ChipGrid(items: TextCase.allCases) { item in
                Chip { Text(item.label) } action: { tools.transform { item.apply(to: $0) } }
            }
        case .browse(.ai, .styles?):
            ChipGrid(items: TextStyle.allCases) { item in
                Chip { Text(verbatim: item.sample).accessibilityLabel(Text(item.label)) } action: {
                    tools.transform { item.apply(to: $0) }
                }
            }
        case .browse(.snippets, _): SnippetsList(tools: tools)
        case .browse(.dictation, _): RecordingOptions(tools: tools)
        case let .working(action): Working(tools: tools, action: action)
        case let .answer(answer): AnswerView(tools: tools, answer: answer, canCopy: canCopy)
        case let .review(text): ReviewView(tools: tools, text: text, canCopy: canCopy)
        case .closed, .compose: EmptyView()
        }
    }

    private static func title(of menu: KeyboardTools.Menu) -> LocalizedStringKey {
        switch menu {
        case .translate: "tools.ai.translate"
        case .rewrite: "tools.ai.rewrite"
        case .commands: "tools.ai.commands"
        case .changeCase: "tools.ai.case"
        case .styles: "tools.ai.styles"
        }
    }
}

private struct TabBar: View {
    let tools: KeyboardTools
    let selected: KeyboardTools.Tab
    @Namespace private var highlight

    var body: some View {
        HStack(spacing: 4) {
            tab(.ai, "tools.tab.ai", "sparkles")
            tab(.snippets, "tools.tab.snippets", "text.append")
            tab(.dictation, "tools.tab.dictation", "waveform")
        }
        .padding(3)
        .glassEffect(.regular, in: Capsule())
    }

    private func tab(_ tab: KeyboardTools.Tab, _ title: LocalizedStringKey, _ symbol: String) -> some View {
        Button {
            withAnimation(toolsAnimation) { tools.show(tab) }
        } label: {
            Label(title, systemImage: symbol)
                .font(.subheadline.weight(.semibold))
                .labelStyle(.titleAndIcon)
                .lineLimit(1)
                .minimumScaleFactor(0.8)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .foregroundStyle(selected == tab ? Theme.textPrimary : Theme.textSecondary)
                .background {
                    if selected == tab {
                        Capsule().fill(Theme.surface).matchedGeometryEffect(id: "tab", in: highlight)
                            .shadow(color: .black.opacity(0.08), radius: 2, y: 1)
                    }
                }
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected == tab ? .isSelected : [])
        .accessibilityIdentifier("tools-tab-\(String(describing: tab))")
    }
}

private struct BackHeader: View {
    let tools: KeyboardTools
    let title: Text

    var body: some View {
        HStack(spacing: 6) {
            Button {
                withAnimation(toolsAnimation) { tools.back() }
            } label: {
                Image(systemName: "chevron.backward")
                    .font(.system(size: 15, weight: .semibold))
                    .frame(width: 34, height: 34)
            }
            .buttonStyle(.plain)
            .glassEffect(.regular.interactive(), in: Circle())
            .accessibilityLabel(Text("tools.back"))
            .accessibilityIdentifier("tools-back")
            title.font(.headline).lineLimit(1)
            Spacer(minLength: 0)
        }
    }
}

/// Eight actions in two rows, sized to the panel.
private struct AIGrid: View {
    let tools: KeyboardTools

    var body: some View {
        GlassEffectContainer(spacing: 8) {
            VStack(spacing: 8) {
                HStack(spacing: 8) {
                    Tile("tools.ai.fix", "text.badge.checkmark", id: "fix") { tools.run(.fix) }
                    Tile("tools.ai.ask", "bubble.left.and.text.bubble.right", id: "ask") { tools.begin(.ask) }
                    Tile("tools.ai.translate", "translate", id: "translate") { tools.show(.ai, .translate) }
                    Tile("tools.ai.rewrite", "pencil.and.scribble", id: "rewrite") { tools.show(.ai, .rewrite) }
                }
                HStack(spacing: 8) {
                    Tile("tools.ai.commands", "command", id: "commands") { tools.show(.ai, .commands) }
                    Tile("tools.ai.case", "textformat", id: "case") { tools.show(.ai, .changeCase) }
                    Tile("tools.ai.styles", "bold.italic.underline", id: "styles") { tools.show(.ai, .styles) }
                    Tile("tools.undo", "arrow.uturn.backward", id: "undo") { tools.undo() }
                        .disabled(tools.lastChange == nil)
                }
            }
        }
        .padding(.bottom, 4)
    }
}

private struct Tile: View {
    let title: LocalizedStringKey
    let symbol: String
    let id: String
    let action: () -> Void
    @Environment(\.isEnabled) private var isEnabled

    init(_ title: LocalizedStringKey, _ symbol: String, id: String, action: @escaping () -> Void) {
        self.title = title
        self.symbol = symbol
        self.id = id
        self.action = action
    }

    var body: some View {
        Button {
            withAnimation(toolsAnimation) { action() }
        } label: {
            VStack(spacing: 5) {
                Image(systemName: symbol).font(.system(size: 19, weight: .medium)).frame(height: 22)
                Text(title).font(.caption.weight(.medium)).lineLimit(1).minimumScaleFactor(0.75)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .contentShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
        .buttonStyle(.plain)
        .foregroundStyle(Theme.textPrimary)
        .opacity(isEnabled ? 1 : 0.35)
        .glassEffect(.regular.interactive(), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .accessibilityIdentifier("tools-\(id)")
    }
}

/// Two columns of choices that scroll when there are more than fit.
private struct ChipGrid<Item: Identifiable, Content: View>: View {
    let items: [Item]
    var leading: AnyView?
    @ViewBuilder let chip: (Item) -> Content

    init(items: [Item], @ViewBuilder chip: @escaping (Item) -> Content) {
        self.items = items
        self.chip = chip
    }

    var body: some View {
        ScrollView {
            GlassEffectContainer(spacing: 6) {
                LazyVGrid(columns: [GridItem(.flexible(), spacing: 6), GridItem(.flexible(), spacing: 6)], spacing: 6) {
                    ForEach(items) { chip($0) }
                }
            }
            .padding(.bottom, 6)
        }
        .scrollIndicators(.hidden)
        .scrollBounceBehavior(.basedOnSize)
    }
}

private struct Chip<Label: View>: View {
    var selected = false
    @ViewBuilder let label: () -> Label
    let action: () -> Void

    var body: some View {
        Button {
            withAnimation(toolsAnimation) { action() }
        } label: {
            label()
                .font(.system(size: 15, weight: .medium))
                .lineLimit(1)
                .minimumScaleFactor(0.7)
                .padding(.horizontal, 12)
                .frame(maxWidth: .infinity, minHeight: 40)
                .contentShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
        .buttonStyle(.plain)
        .foregroundStyle(Theme.textPrimary)
        .glassEffect(
            selected ? .regular.tint(Theme.accent.opacity(0.3)).interactive() : .regular.interactive(),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

private struct LanguageItem: Identifiable {
    let language: Locale.Language
    var id: String { language.minimalIdentifier }
}

private struct TranslateMenu: View {
    let tools: KeyboardTools

    var body: some View {
        if let reason = KeyboardAI.unavailable {
            Hint(text: reason)
        } else {
            ChipGrid(items: KeyboardAI.languages(first: tools.languages).map(LanguageItem.init)) { item in
                Chip { Text(verbatim: KeyboardAI.name(of: item.language)) } action: { tools.run(.translate(item.language)) }
            }
        }
    }
}

private struct RewriteMenu: View {
    let tools: KeyboardTools

    var body: some View {
        if let reason = KeyboardAI.unavailable {
            Hint(text: reason)
        } else if tools.profile.styles.isEmpty {
            Hint(text: "tools.noStyles")
        } else {
            ChipGrid(items: tools.profile.styles) { style in
                Chip { Text(verbatim: style.name) } action: { tools.run(.style(style)) }
            }
        }
    }
}

private struct CommandItem: Identifiable {
    let command: SavedCommand?
    var id: UUID? { command?.id }
}

private struct CommandsMenu: View {
    let tools: KeyboardTools

    var body: some View {
        ChipGrid(items: [CommandItem(command: nil)] + tools.commands.map(CommandItem.init)) { item in
            if let command = item.command {
                Chip { Text(verbatim: command.name) } action: { tools.run(.command(command)) }
                    .contextMenu {
                        Button("tools.delete", systemImage: "trash", role: .destructive) { tools.delete(command) }
                    }
                    .accessibilityIdentifier("tools-command-\(command.name)")
            } else {
                Chip { Label("tools.newCommand", systemImage: "plus") } action: { tools.begin(.commandPrompt) }
                    .accessibilityIdentifier("tools-new-command")
            }
        }
    }
}

private struct SnippetsList: View {
    let tools: KeyboardTools

    var body: some View {
        ScrollView {
            GlassEffectContainer(spacing: 6) {
                LazyVStack(spacing: 6) {
                    Row(action: tools.saveSelection) {
                        Label("tools.saveSelection", systemImage: "plus.square.on.square")
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(Theme.accent)
                    }
                    .accessibilityIdentifier("tools-save-snippet")
                    ForEach(tools.allSnippets, id: \.self) { snippet in
                        Row { tools.insert(snippet.expansion) } label: {
                            HStack(spacing: 10) {
                                Text(verbatim: snippet.trigger)
                                    .font(.footnote.monospaced().weight(.medium))
                                    .foregroundStyle(Theme.textSecondary)
                                    .lineLimit(1)
                                    .frame(maxWidth: 110, alignment: .leading)
                                Text(verbatim: snippet.expansion).font(.subheadline).lineLimit(1)
                            }
                        }
                        .contextMenu {
                            if tools.snippets.contains(snippet) {
                                Button("tools.delete", systemImage: "trash", role: .destructive) { tools.delete(snippet) }
                            }
                        }
                    }
                    if tools.allSnippets.isEmpty {
                        Text("tools.noSnippets").font(.footnote).foregroundStyle(Theme.textSecondary)
                            .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 4)
                    }
                }
            }
            .padding(.bottom, 6)
        }
        .scrollIndicators(.hidden)
        .scrollBounceBehavior(.basedOnSize)
    }
}

private struct Row<Content: View>: View {
    let action: () -> Void
    @ViewBuilder let label: () -> Content

    var body: some View {
        Button {
            withAnimation(toolsAnimation) { action() }
        } label: {
            label()
                .frame(maxWidth: .infinity, minHeight: 40, alignment: .leading)
                .padding(.horizontal, 12)
                .contentShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
        .buttonStyle(.plain)
        .foregroundStyle(Theme.textPrimary)
        .glassEffect(.regular.interactive(), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}

private struct StyleChoice: Identifiable {
    let id: String
    let name: Text
}

/// Choices for the next recording only, and review before inserting, which stays.
private struct RecordingOptions: View {
    @Bindable var tools: KeyboardTools

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                section("tools.recordingStyle") {
                    ForEach(styles) { choice in
                        Chip(selected: (tools.recordingStyle ?? "default") == choice.id) { choice.name } action: {
                            tools.recordingStyle = choice.id == "default" ? nil : choice.id
                        }
                        .fixedSize()
                    }
                }
                if let style = tools.effectiveStyle, DictationTone.styleIDs.contains(style.id) {
                    ToneSlider(style: style).id(style.id).padding(.horizontal, 6)
                }
                section("tools.recordingLanguage") {
                    Chip(selected: tools.recordingLanguage == nil) { Text("tools.typingLanguage") } action: {
                        tools.recordingLanguage = nil
                    }
                    .fixedSize()
                    ForEach(TypingLanguage.allCases) { language in
                        Chip(selected: tools.recordingLanguage == language) { Text(verbatim: language.name) } action: {
                            tools.recordingLanguage = language
                        }
                        .fixedSize()
                    }
                }
                Toggle("tools.reviewBeforeInsert", isOn: $tools.reviewsDictation)
                    .font(.subheadline)
                    .padding(.horizontal, 6)
                    .accessibilityIdentifier("tools-review")
                Text("tools.recordingFooter").font(.caption).foregroundStyle(Theme.textSecondary).padding(.horizontal, 6)
            }
            .padding(.bottom, 6)
        }
        .scrollIndicators(.hidden)
        .scrollBounceBehavior(.basedOnSize)
    }

    private var styles: [StyleChoice] {
        [StyleChoice(id: "default", name: Text("tools.chosenStyle")), StyleChoice(id: "", name: Text("dictation.noStyle"))]
            + tools.profile.styles.map { StyleChoice(id: $0.id, name: Text(verbatim: $0.name)) }
    }

    private func section(_ title: LocalizedStringKey, @ViewBuilder chips: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption.weight(.semibold)).foregroundStyle(Theme.textSecondary).padding(.horizontal, 6)
            ScrollView(.horizontal) {
                GlassEffectContainer(spacing: 6) {
                    HStack(spacing: 6) { chips() }
                }
            }
            .scrollIndicators(.hidden)
        }
    }
}

private struct Working: View {
    let tools: KeyboardTools
    let action: KeyboardAI

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: "sparkles")
                .font(.system(size: 28, weight: .medium))
                .foregroundStyle(Theme.accent)
                .symbolEffect(.variableColor.iterative.dimInactiveLayers.nonReversing, options: .repeating)
            Text("tools.working").font(.subheadline).foregroundStyle(Theme.textSecondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("tools-working")
    }
}

/// The model's answer and what to do with it.
private struct AnswerView: View {
    let tools: KeyboardTools
    let answer: KeyboardTools.Answer
    let canCopy: Bool

    var body: some View {
        VStack(spacing: 8) {
            TextCard(text: answer.text)
            HStack(spacing: 8) {
                if answer.source != nil {
                    Button("tools.replace") { withAnimation(toolsAnimation) { tools.replace(with: answer) } }
                        .buttonStyle(.glassProminent)
                        .foregroundStyle(.white)
                        .accessibilityIdentifier("tools-replace")
                }
                if answer.source == nil {
                    insertButton.buttonStyle(.glassProminent).foregroundStyle(.white)
                } else if answer.source?.selected == false {
                    insertButton.buttonStyle(.glass)
                }
                if canCopy {
                    Button("tools.copy") { tools.copy(answer.text) }.buttonStyle(.glass)
                }
                if case let .ask(question) = answer.action {
                    Button("tools.saveCommand") { withAnimation(toolsAnimation) { tools.begin(.commandName(prompt: question)) } }
                        .buttonStyle(.glass)
                        .accessibilityIdentifier("tools-save-command")
                }
            }
            .font(.subheadline.weight(.semibold))
            .lineLimit(1)
            .minimumScaleFactor(0.8)
            .padding(.bottom, 4)
        }
    }

    private var insertButton: some View {
        Button("tools.insert") { withAnimation(toolsAnimation) { tools.insert(answer.text) } }
            .accessibilityIdentifier("tools-insert")
    }
}

private struct ReviewView: View {
    let tools: KeyboardTools
    let text: String
    let canCopy: Bool

    var body: some View {
        VStack(spacing: 8) {
            TextCard(text: text)
            HStack(spacing: 8) {
                Button("keyboard.insert") { withAnimation(toolsAnimation) { tools.insert(text) } }
                    .buttonStyle(.glassProminent)
                    .foregroundStyle(.white)
                    .accessibilityIdentifier("tools-review-insert")
                Button("keyboard.discard") { withAnimation(toolsAnimation) { tools.back() } }
                    .buttonStyle(.glass)
                if canCopy {
                    Button("tools.copy") { tools.copy(text) }.buttonStyle(.glass)
                }
            }
            .font(.subheadline.weight(.semibold))
            .padding(.bottom, 4)
        }
    }
}

private struct TextCard: View {
    let text: String

    var body: some View {
        ScrollView {
            Text(verbatim: text)
                .font(.system(size: 16))
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(12)
                .accessibilityIdentifier("tools-answer")
        }
        .scrollBounceBehavior(.basedOnSize)
        .background(Theme.surface.opacity(0.7), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
    }
}

private struct Hint: View {
    let text: LocalizedStringKey

    var body: some View {
        Text(text).font(.footnote).foregroundStyle(Theme.textSecondary)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(.horizontal, 16)
    }
}
