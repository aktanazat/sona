import AppKit
import SwiftUI

/// The chat with the Sona agent: what was said, what the turn did on the way,
/// the card a settings answer becomes, and the field that asks the next thing.
///
/// The React app docked this as a 340-point column beside the page. Here it is
/// the sheet the shell already presents, so the integrator owns when it is on
/// screen and this owns everything inside it.
struct ChatView: View {
    let store: ChatStore
    /// The way out. The close glyph and Escape both take it, and Escape works
    /// while the field has the caret.
    var onClose: () -> Void = {}
    /// Where the switch, the pairing and this Mac's key live.
    var openSettings: () -> Void = {}
    /// Replies other agents are waiting on. The bridge slice owns the queue
    /// itself; the chat is where the reader notices it.
    var pendingRequests = 0
    var openRequests: () -> Void = {}

    var body: some View {
        VStack(spacing: 0) {
            header
            Hairline()
            scrollback
            footer
        }
        .frame(width: 480, height: 640)
        .background(Theme.page)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusDialog))
        .onExitCommand(perform: onClose)
        .task { await store.start() }
    }

    // MARK: - Head

    private var header: some View {
        HStack(spacing: 10) {
            Button(action: onClose) {
                Image(systemName: "xmark").font(.system(size: 12, weight: .semibold))
            }
            .buttonStyle(.quiet)
            .help("Close chat")

            Text("Sona agent").font(TypeScale.label()).foregroundStyle(Theme.ink)

            Spacer(minLength: 8)

            if store.running {
                ProgressView().controlSize(.small)
            }
            menu
        }
        .padding(.horizontal, 16)
        .frame(height: 48)
        .background(Theme.surface)
    }

    private var menu: some View {
        Menu {
            Button("New chat") { store.newChat() }
                .disabled(store.busy || store.running)

            Menu("Recent chats") { history }

            Divider()

            Picker("Who answers", selection: workspace) {
                ForEach(AgentPanelWorkspace.allCases) { scope in
                    Text(scope.title).tag(scope)
                }
            }
            .pickerStyle(.inline)
            .disabled(store.running)
        } label: {
            Image(systemName: "ellipsis").font(.system(size: 13, weight: .semibold))
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .foregroundStyle(Theme.inkSecondary)
        .help("Chat with the Sona agent")
    }

    /// The conversations the core remembers, newest first. A checkmark marks
    /// the one on screen, which is what a menu of one choice is for.
    @ViewBuilder
    private var history: some View {
        if store.history.isEmpty {
            Text("Chats you start appear here.")
        } else {
            Picker("", selection: conversation) {
                ForEach(store.history) { summary in
                    Text(summary.title).tag(summary.conversationId)
                }
            }
            .pickerStyle(.inline)
            .labelsHidden()
            .disabled(store.busy || store.running)
        }
    }

    // MARK: - Body

    private var scrollback: some View {
        ScrollView {
            content
                .padding(.horizontal, 16)
                .padding(.top, 16)
                .padding(.bottom, 20)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .scrollIndicators(.never)
        .defaultScrollAnchor(.bottom)
        .frame(maxHeight: .infinity)
    }

    @ViewBuilder
    private var content: some View {
        if store.phase == .loading {
            ProgressView()
                .controlSize(.small)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 48)
        } else if store.conversation.isEmpty, store.turn == nil, store.proposal == nil {
            Text("Ask what was said, what you owe, or who someone is.")
                .bodyText(14, Theme.inkSecondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 300)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 48)
        } else {
            ChatScrollback(store: store)
        }
    }

    // MARK: - Foot

    @ViewBuilder
    private var footer: some View {
        if let notice = store.phase.notice {
            Hairline()
            HStack(spacing: 12) {
                Text(notice)
                    .metaText(Theme.inkSecondary)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 8)
                if store.phase.offersRetry {
                    Button("Retry") { store.reread() }.buttonStyle(.quiet)
                }
                if store.phase.offersSettings {
                    Button("Settings", action: openSettings).buttonStyle(.quiet)
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
        }

        if store.consentNeeded, store.phase == .ready {
            Hairline()
            VStack(alignment: .leading, spacing: 8) {
                Text(
                    "Sona answers about your recordings once you turn on meeting intelligence. "
                        + "That sends matching quotes to your server, and writes the notes for "
                        + "every later meeting there too."
                )
                .metaText(Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
                Button("Allow") { store.allowRemoteIntelligence() }
                    .buttonStyle(.compact)
                    .disabled(store.busy)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
        }

        if pendingRequests > 0 {
            Hairline()
            CardRow(action: openRequests) {
                HStack(spacing: 10) {
                    Image(systemName: "tray.full")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(Theme.inkSecondary)
                    Text("Replies waiting for you").bodyText(14)
                }
            } trailing: {
                Chip("\(pendingRequests)")
            }
        }

        if store.error != nil {
            Hairline()
            ErrorNote(store.error)
                .padding(.horizontal, 16)
                .padding(.top, 12)
        }

        Hairline()
        ChatComposer(store: store, onClose: onClose)
    }

    // MARK: - Bindings

    private var workspace: Binding<AgentPanelWorkspace> {
        Binding(get: { store.workspace }, set: { store.workspace = $0 })
    }

    private var conversation: Binding<String> {
        Binding(get: { store.conversationId ?? "" }, set: { store.open($0) })
    }
}

/// The scrollback: what was said, what the turn did on the way, the one card
/// a settings answer becomes, and the cards a corpus change is offered as.
private struct ChatScrollback: View {
    let store: ChatStore

    var body: some View {
        let rows = store.rows
        let workIndex = store.workRowIndex
        let cardIndex = store.proposalRowIndex
        let retry = store.retryMessage

        LazyVStack(alignment: .leading, spacing: 16) {
            ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                if index == workIndex {
                    work(retry)
                }
                if row.turn.role == .user {
                    ChatQuestion(message: row.turn.message)
                } else if index == cardIndex, let proposal = store.proposal {
                    ChatProposalCard(store: store, proposal: proposal)
                } else {
                    ChatAnswer(message: row.turn.message, store: store)
                }
                /* A failure the live turn is already reporting is the same
                 * failure: the row it belongs to is the question above, and
                 * one line about it is enough. */
                if let outcome = row.turn.outcome, !(index == rows.count - 1 && retry != nil) {
                    ChatOutcomeNote(
                        store: store,
                        outcome: outcome,
                        message: row.turn.role == .user ? row.turn.message : nil)
                }
            }

            /* A turn still working has no answer to sit above, so its work
             * goes last — and a proposal with no row of its own is drawn
             * rather than dropped. */
            if workIndex == rows.count {
                work(retry)
            }

            /* The offer sits under the answer that made it, because the
             * answer is what explains it. Each card is its own row so a set
             * of three reads as three choices rather than one block. */
            ForEach(store.turn?.actions ?? []) { action in
                ChatActionCard(store: store, action: action)
            }

            if cardIndex == -1, let proposal = store.proposal {
                ChatProposalCard(store: store, proposal: proposal)
            }
            if let proposal = store.proposal, !proposal.rationale.isEmpty {
                ChatAnswer(message: proposal.rationale, store: store)
            }
            if let question = store.proposal?.followUpQuestion {
                ChatAnswer(message: question, store: store)
            }
        }
    }

    @ViewBuilder
    private func work(_ retry: String?) -> some View {
        if let turn = store.turn {
            ChatWorkRow(store: store, turn: turn, retryMessage: retry)
        }
    }
}

/// A question of your own, as an object on the surface rather than a wash
/// over it.
private struct ChatQuestion: View {
    let message: String

    var body: some View {
        Text(message)
            .bodyText(14)
            .textSelection(.enabled)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.radiusCard)
                    .strokeBorder(Theme.border, lineWidth: 1))
            .frame(maxWidth: 380, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .trailing)
    }
}

/// An answer is prose on the surface, not a card and not a bubble: it is the
/// thing the sheet exists to show, and putting a container around it would
/// make it look like an aside to something else. Only the addresses inside it
/// are chrome, because only they are pressable.
private struct ChatAnswer: View {
    let message: String
    let store: ChatStore

    var body: some View {
        Text(prose)
            .font(TypeScale.body(14))
            .foregroundStyle(Theme.ink)
            .textSelection(.enabled)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .environment(
                \.openURL,
                OpenURLAction { url in
                    store.openLink(url.absoluteString)
                    return .handled
                })
    }

    /// The addresses are links in the run of text rather than buttons beside
    /// it, so the sentence still reads as a sentence.
    private var prose: AttributedString {
        var result = AttributedString()
        for segment in ChatSegment.scan(message) {
            switch segment {
            case let .text(text):
                result += AttributedString(text)
            case let .link(link):
                var run = AttributedString(link)
                run.underlineStyle = .single
                if let url = URL(string: link) {
                    run.link = url
                }
                result += run
            }
        }
        return result
    }
}

/// The activity that belongs below a turn's question.
///
/// A live turn always gets a timing line. A finished turn gets one when the
/// core recorded its finish. Failure and the corpus marker share this row
/// because neither has an assistant answer of its own to introduce them.
private struct ChatWorkRow: View {
    let store: ChatStore
    let turn: AgentPanelTurnStatus
    /// The question a retry would ask again, when the failure belongs to the
    /// row above this one.
    let retryMessage: String?
    @State private var stepsOpen = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if turn.showsTiming {
                timing
            }
            if store.searchedCorpus {
                Text("Looked through your meetings and notes").metaText()
            }
            if turn.isStillWaiting(store.now) {
                HStack(spacing: 8) {
                    Text("Still waiting…").metaText(Theme.inkSecondary)
                    Button("Cancel") { store.stop() }
                        .buttonStyle(.quiet)
                        .disabled(store.busy)
                }
            }
            if let failure = turn.failure {
                HStack(spacing: 8) {
                    Text(failure.message).metaText(Theme.live)
                    if let retryMessage {
                        Button("Retry") { store.retry(retryMessage) }
                            .buttonStyle(QuietButton(color: Theme.live))
                            .disabled(store.busy)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private var timing: some View {
        if turn.steps.isEmpty {
            Text(turn.timing(store.now)).metaText(Theme.inkSecondary)
        } else {
            Button { stepsOpen.toggle() } label: {
                HStack(spacing: 4) {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 9, weight: .semibold))
                        .rotationEffect(.degrees(stepsOpen ? 90 : 0))
                    Text(turn.timing(store.now))
                }
            }
            .buttonStyle(.quiet)
            if stepsOpen {
                steps
            }
        }
    }

    private var steps: some View {
        VStack(alignment: .leading, spacing: 5) {
            ForEach(turn.steps) { step in
                HStack(spacing: 8) {
                    if step.isTool {
                        /* A tool's name is a machine's name, so it reads as
                         * one: a mono word in a hairline pill with no fill.
                         * A step with no tool stays prose, because it is
                         * prose. */
                        Text(step.title)
                            .font(TypeScale.mono(12))
                            .foregroundStyle(step.state == .failed ? Theme.live : Theme.inkSecondary)
                            .lineLimit(1)
                            .padding(.horizontal, 6)
                            .frame(height: 19)
                            .overlay(
                                Capsule().strokeBorder(
                                    step.state == .failed ? Theme.live : Theme.border,
                                    lineWidth: 1))
                    } else {
                        Text(step.title)
                            .metaText(step.state == .failed ? Theme.live : Theme.inkSecondary)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 8)
                    Text("\(AgentPanelTurnStatus.seconds(turn.workedMs(step, store.now)))s")
                        .metaText()
                        .monospacedDigit()
                }
            }
        }
        .padding(.leading, 12)
        .padding(.vertical, 2)
        .overlay(alignment: .leading) {
            Rectangle().fill(Theme.border).frame(width: 1)
        }
    }
}

/// The terminal result remembered with an earlier question.
private struct ChatOutcomeNote: View {
    let store: ChatStore
    let outcome: AgentChatOutcome
    /// The question to ask again, when this row is one.
    let message: String?

    var body: some View {
        switch outcome {
        case .canceled:
            Text("Stopped").metaText(Theme.inkSecondary)
        case let .failure(failure):
            HStack(spacing: 8) {
                Text(failure.message).metaText(Theme.live)
                if let message {
                    Button("Retry") { store.retry(message) }
                        .buttonStyle(QuietButton(color: Theme.live))
                        .disabled(store.busy)
                }
            }
        }
    }
}

/// A settings answer, as the one thing you can do about it.
///
/// The card is the assistant's turn — the core puts the proposal's summary
/// into the conversation and stores the proposal beside it, so drawing both
/// would print one sentence twice. Applying moves this same card to Applied
/// with an Undo, rather than adding a second card that reports on the first.
private struct ChatProposalCard: View {
    let store: ChatStore
    let proposal: AgentPanelProposal

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "slider.horizontal.3")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(Theme.ink)
                .frame(width: 30, height: 30)
                .background(Theme.inset, in: RoundedRectangle(cornerRadius: 8))

            VStack(alignment: .leading, spacing: 3) {
                /* Wraps rather than truncates: this sentence is the whole of
                 * what the assistant said, and a press on Apply under half a
                 * sentence is a press on something nobody was shown. */
                Text(proposal.summary)
                    .bodyText(14)
                    .fixedSize(horizontal: false, vertical: true)
                if !proposal.changeKeys.isEmpty {
                    Text(proposal.changeKeys).metaText().lineLimit(1)
                }
            }

            Spacer(minLength: 8)

            switch proposal.state {
            case .pending:
                Button("Apply") { store.applyProposal() }
                    .buttonStyle(.compact)
                    .disabled(store.busy)
            case .applied:
                HStack(spacing: 8) {
                    Text("Applied").metaText(Theme.inkSecondary)
                    Button("Undo") { store.undoProposal() }
                        .buttonStyle(.quiet)
                        .disabled(store.busy || proposal.receiptId == nil)
                }
            case .undone, .rejected:
                Text(proposal.state.label).metaText(Theme.inkSecondary)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusPanel))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.radiusPanel)
                .strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// One corpus change the answer offered, as the one thing you can do about it.
///
/// Built like the settings card and read the same way: what changes on the
/// first line, why underneath, one button that makes it happen. Dismiss and
/// Undo are one gesture with two labels — this change is not in effect — so
/// both go to the same command.
private struct ChatActionCard: View {
    let store: ChatStore
    let action: AgentPanelAction

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                Text(action.action.line)
                    .bodyText(14)
                    .fixedSize(horizontal: false, vertical: true)
                Text(action.action.reason)
                    .metaText()
                    .fixedSize(horizontal: false, vertical: true)
            }

            Spacer(minLength: 8)

            switch action.state {
            case .pending:
                HStack(spacing: 8) {
                    Button("Apply") { store.applyAction(action.actionIndex) }
                        .buttonStyle(.compact)
                        .disabled(store.busy)
                    Button("Dismiss") { store.dismissAction(action.actionIndex) }
                        .buttonStyle(.quiet)
                        .disabled(store.busy)
                }
            case .applied:
                HStack(spacing: 8) {
                    Text("Applied").metaText(Theme.inkSecondary)
                    Button("Undo") { store.dismissAction(action.actionIndex) }
                        .buttonStyle(.quiet)
                        .disabled(store.busy)
                }
            case .dismissed:
                Text("Dismissed").metaText(Theme.inkSecondary)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusPanel))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.radiusPanel)
                .strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// The field and the one button beside it, which is Send until a turn is
/// running and Stop while it is.
private struct ChatComposer: View {
    let store: ChatStore
    let onClose: () -> Void

    var body: some View {
        HStack(alignment: .bottom, spacing: 10) {
            ZStack(alignment: .topLeading) {
                if store.draft.isEmpty {
                    Text(store.workspace.prompt)
                        .font(TypeScale.body(14))
                        .foregroundStyle(Theme.inkTertiary)
                        .padding(.top, 4)
                        .allowsHitTesting(false)
                }
                ChatComposerField(
                    text: Binding(get: { store.draft }, set: { store.draft = $0 }),
                    isEnabled: !store.composerDisabled && !store.running,
                    send: { store.send() },
                    escape: onClose)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.radiusControl)
                    .strokeBorder(Theme.border, lineWidth: 1))

            if store.running {
                ChatRoundButton(
                    symbol: "stop.fill", help: "Stop", isEnabled: !store.busy,
                    action: { store.stop() })
            } else {
                ChatRoundButton(
                    symbol: "arrow.up", help: "Send", isEnabled: store.canSend,
                    action: { store.send() })
            }
        }
        .padding(12)
        .background(Theme.surface)
    }
}

/// Send and Stop: one 30-point circle, filled while it can be pressed.
private struct ChatRoundButton: View {
    let symbol: String
    let help: String
    let isEnabled: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(isEnabled ? Theme.onInvert : Theme.inkDisabled)
                .frame(width: 30, height: 30)
                .background(isEnabled ? Theme.invert : Theme.selection, in: Circle())
        }
        .buttonStyle(.plain)
        .disabled(!isEnabled)
        .help(help)
    }
}

/// The composer's field.
///
/// Return sends and Shift-Return breaks the line, which `TextField` cannot
/// do: `onSubmit` fires for both and there is no way to let one of them
/// through as a newline. So the field is an `NSTextView`, the two keys are
/// read where they arrive, and the same place gives Escape its chance to
/// close the sheet while the caret is in the field.
private struct ChatComposerField: NSViewRepresentable {
    @Binding var text: String
    let isEnabled: Bool
    let send: () -> Void
    let escape: () -> Void

    /// Where React's composer stopped growing, which is about six lines.
    private static let maxHeight: CGFloat = 120
    private static let inset: CGFloat = 4
    private static let font = NSFont.systemFont(ofSize: 14)

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        let view = NSTextView()
        view.delegate = context.coordinator
        view.isRichText = false
        view.allowsUndo = true
        view.font = Self.font
        view.textColor = NSColor(Theme.ink)
        view.insertionPointColor = NSColor(Theme.ink)
        view.drawsBackground = false
        view.textContainerInset = NSSize(width: 0, height: Self.inset)
        view.isVerticallyResizable = true
        view.isHorizontallyResizable = false
        view.autoresizingMask = [.width]
        view.textContainer?.widthTracksTextView = true
        view.textContainer?.lineFragmentPadding = 0
        scroll.documentView = view
        scroll.drawsBackground = false
        scroll.borderType = .noBorder
        scroll.hasVerticalScroller = false
        scroll.autohidesScrollers = true
        /* The sheet opens on a question, so the field takes the caret. It
         * has no window to take it in until the sheet is on screen, which is
         * the next pass of the run loop. */
        DispatchQueue.main.async { [coordinator = context.coordinator] in
            Self.focus(view, coordinator, isEnabled: true)
        }
        return scroll
    }

    /// Given once: taking the caret back on every redraw would steal it from
    /// wherever the reader had moved it.
    private static func focus(
        _ view: NSTextView, _ coordinator: ChatComposerCoordinator, isEnabled: Bool
    ) {
        guard isEnabled, !coordinator.focused, let window = view.window else { return }
        coordinator.focused = true
        window.makeFirstResponder(view)
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let view = scroll.documentView as? NSTextView else { return }
        context.coordinator.text = $text
        context.coordinator.send = send
        context.coordinator.escape = escape
        if view.string != text {
            view.string = text
        }
        view.isEditable = isEnabled
        Self.focus(view, context.coordinator, isEnabled: isEnabled)
    }

    func sizeThatFits(
        _ proposal: ProposedViewSize, nsView: NSScrollView, context: Context
    ) -> CGSize? {
        let width = proposal.width ?? 240
        let wanted = Self.height(text, width)
        return CGSize(width: width, height: min(max(wanted, 21), Self.maxHeight))
    }

    func makeCoordinator() -> ChatComposerCoordinator {
        ChatComposerCoordinator(text: $text, send: send, escape: escape)
    }

    /// The height the text wants at this width, measured the way it is drawn.
    private static func height(_ text: String, _ width: CGFloat) -> CGFloat {
        /* A trailing newline has no line of its own to measure, but the caret
         * is already sitting on it. */
        let measured = text.hasSuffix("\n") ? text + " " : text
        let bounds = (measured as NSString).boundingRect(
            with: NSSize(width: max(width, 1), height: .greatestFiniteMagnitude),
            options: [.usesLineFragmentOrigin, .usesFontLeading],
            attributes: [.font: font])
        return ceil(bounds.height) + inset * 2
    }
}

/// The composer's delegate: the two keys, Escape, and the text going back.
private final class ChatComposerCoordinator: NSObject, NSTextViewDelegate {
    var text: Binding<String>
    var send: () -> Void
    var escape: () -> Void
    /// The caret is given once. Taking it again on every redraw would steal
    /// it back from whatever the reader moved it to.
    var focused = false

    init(text: Binding<String>, send: @escaping () -> Void, escape: @escaping () -> Void) {
        self.text = text
        self.send = send
        self.escape = escape
    }

    func textDidChange(_ notification: Notification) {
        guard let view = notification.object as? NSTextView else { return }
        text.wrappedValue = view.string
    }

    func textView(_ view: NSTextView, doCommandBy command: Selector) -> Bool {
        switch command {
        case Self.newline:
            send()
            return true
        case Self.lineBreak:
            /* Shift-Return's own command inserts a line separator, which is
             * not a newline anywhere this text is read again. */
            view.insertText("\n", replacementRange: view.selectedRange())
            return true
        /* Escape reaches a text view as `complete:` — AppKit's word
         * completion — before it reaches anything as a cancel. In a sheet
         * with one field, Escape means close, and a completion list nobody
         * asked for would eat the key on the way. */
        case Self.cancel, Self.complete:
            escape()
            return true
        default:
            return false
        }
    }

    private static let newline = NSSelectorFromString("insertNewline:")
    private static let lineBreak = NSSelectorFromString("insertLineBreak:")
    private static let cancel = NSSelectorFromString("cancelOperation:")
    private static let complete = NSSelectorFromString("complete:")
}
