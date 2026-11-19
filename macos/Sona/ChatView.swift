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
    /// Whether the reader is on the newest line, which is the only time a new
    /// one may move the scrollback.
    @State private var following = true
    /// Which ends of the thread have more beyond them, so only those fade.
    @State private var edges = ChatEdges()
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let opening = isOpening
        VStack(spacing: 0) {
            header
            if store.isPlanning {
                Hairline()
                PlannerAgentView(chat: store, onClose: onClose)
            } else {
                Hairline().opacity(!opening && edges.above ? 1 : 0)
                if opening {
                    openingHead
                } else {
                    scrollback
                }
                footer(opening)
                if opening {
                    openingFoot
                    Spacer(minLength: 0).frame(maxHeight: 64)
                }
            }
        }
        .frame(width: 560, height: 620)
        .background(Theme.page)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusDialog))
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.24), value: opening)
        .onExitCommand(perform: onClose)
        .task(id: store.isPlanning) {
            if store.isPlanning { store.planner.activate() }
            else { await store.start() }
        }
        .onDisappear {
            store.stopVoice()
            store.planner.deactivate()
            store.cancelScreenshotSelection()
        }
    }

    /// Nothing has been said yet, so the field sits in the middle of the
    /// sheet under its question instead of at the foot of an empty page.
    private var isOpening: Bool {
        store.conversation.isEmpty && store.turn == nil && store.proposal == nil
    }

    // MARK: - Head

    /// Sona's mark, who answers, and the ways out of this chat: an earlier
    /// one, a new one, or closing the sheet.
    private var header: some View {
        HStack(spacing: 12) {
            Image("Mark")
                .renderingMode(.template)
                .resizable()
                .scaledToFit()
                .frame(width: 18, height: 18)
                .foregroundStyle(Theme.ink)
                .accessibilityHidden(true)

            Picker("Mode", selection: workspace) {
                ForEach(ChatSheetMode.allCases) { scope in
                    Text(scope.rawValue).tag(scope)
                }
            }
            .labelsHidden()
            .pickerStyle(.segmented)
            .fixedSize()
            .disabled(store.planner.phase == .accepting || (!store.isPlanning && store.running))

            Spacer(minLength: 12)

            HStack(spacing: 4) {
                if !store.isPlanning { history }

                Button {
                    if store.isPlanning { store.planner.reset() }
                    else { store.newChat() }
                } label: {
                    Image(systemName: "square.and.pencil")
                }
                .buttonStyle(ChatGlyphButton())
                .disabled(store.isPlanning ? store.planner.busy : store.busy || store.running)
                .help(store.isPlanning ? "New plan" : "New chat")
                .accessibilityLabel(store.isPlanning ? "New plan" : "New chat")

                Button(action: onClose) {
                    Image(systemName: "xmark")
                }
                .buttonStyle(ChatGlyphButton())
                .help("Close chat")
                .accessibilityLabel("Close chat")
            }
        }
        .padding(.leading, 24)
        .padding(.trailing, 16)
        .frame(height: 52)
    }

    /// The conversations the core remembers, newest first. A checkmark marks
    /// the one on screen, which is what a menu of one choice is for.
    private var history: some View {
        Menu {
            if store.history.isEmpty {
                Text("No chats yet")
            } else {
                Picker("Recent chats", selection: conversation) {
                    ForEach(store.history) { summary in
                        Text(summary.title).tag(summary.conversationId)
                    }
                }
                .pickerStyle(.inline)
                .labelsHidden()
                .disabled(store.busy || store.running)
            }
        } label: {
            Label("Recent chats", systemImage: "clock.arrow.circlepath")
                .labelStyle(.iconOnly)
                .font(.system(size: 13, weight: .regular))
                .frame(width: 28, height: 28)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .modifier(ChatMenuGlyph())
        .help("Recent chats")
        .accessibilityLabel("Recent chats")
    }

    // MARK: - Body

    /// An empty chat's question, standing on the field. Before the first
    /// status the sheet does not know which question it is, so a spinner
    /// holds the place.
    private var openingHead: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 0)
            if store.phase == .loading {
                ProgressView().controlSize(.small)
            } else {
                ChatEmptyState(workspace: store.workspace)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(.horizontal, 24)
        .padding(.bottom, 24)
    }

    /// The questions an empty chat offers, under the field they would fill.
    private var openingFoot: some View {
        VStack(spacing: 0) {
            ChatOpeningSuggestions(store: store)
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(.horizontal, 24)
        .padding(.top, 16)
    }

    private var scrollback: some View {
        ScrollView {
            ChatScrollback(store: store)
                .padding(.horizontal, 24)
                .padding(.top, 20)
                .padding(.bottom, 24)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .scrollIndicators(.never)
        /* A chat opens on its newest line, and a short one reads from the top
         * down, as a page does. A line that arrives later keeps the view on
         * the bottom only when the reader was already there: scrolled up to
         * an earlier answer, the page holds still under them instead of
         * being pushed. */
        .defaultScrollAnchor(.bottom, for: .initialOffset)
        .defaultScrollAnchor(.top, for: .alignment)
        .defaultScrollAnchor(following ? UnitPoint.bottom : .top, for: .sizeChanges)
        .onScrollGeometryChange(for: Bool.self) { geometry in
            geometry.contentOffset.y + geometry.containerSize.height >= geometry.contentSize.height - 24
        } action: { _, atBottom in
            following = atBottom
        }
        .onScrollGeometryChange(for: ChatEdges.self) { geometry in
            ChatEdges(geometry)
        } action: { _, next in
            edges = next
        }
        .mask { ChatFades(edges: edges) }
        /* Another conversation is another page: it opens on its newest line
         * with its rows already in place, not arriving one by one. */
        .id(store.conversationId)
        .onChange(of: store.conversationId) { following = true }
        .frame(maxHeight: .infinity)
    }

    // MARK: - Foot

    /// What the reader should know before asking, then the field. In an
    /// empty chat both sit under the question in the middle of the sheet;
    /// once something has been said they rest at its foot.
    private func footer(_ opening: Bool) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            notices
            ChatComposer(store: store, onClose: onClose, openSettings: openSettings)
        }
        .padding(.horizontal, 24)
        .padding(.top, opening ? 0 : 8)
        .padding(.bottom, opening ? 0 : 24)
    }

    @ViewBuilder
    private var notices: some View {
        if let notice = store.phase.notice {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
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
        }

        if store.consentNeeded, store.phase == .ready {
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
        }

        if pendingRequests > 0 {
            ChatRequestsRow(count: pendingRequests, action: openRequests)
        }

        if store.unsaved {
            Text("This chat couldn't be saved. It stays here until you close it, but won't be in History.")
                .metaText(Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }

        if let error = store.error {
            Text(error)
                .font(TypeScale.body(13))
                .foregroundStyle(Theme.live)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }

        /* The microphone gets a line while it is in use. Where the audio goes
         * is said on the mic button, where it is read before pressing. */
        if store.workspace == .sonaChat,
           let line = store.voice?.phase.label ?? (store.stoppingVoice ? "Stopping microphone…" : nil) {
            Text(line)
                .metaText(Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }

        if store.workspace == .sonaChat, let folder = store.folderScope {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text("Only meetings in “\(folder.name)”")
                    .metaText(Theme.accent)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 8)
                Button("Remove") { store.clearFolderScope() }
                    .buttonStyle(.quiet)
                    .disabled(store.busy || store.running)
                    .accessibilityLabel("Ask across all meetings")
            }
        }
    }

    // MARK: - Bindings

    private var workspace: Binding<ChatSheetMode> {
        Binding(get: {
            store.isPlanning ? .plan : store.workspace == .sonaChat ? .ask : .configure
        }, set: { mode in
            switch mode {
            case .plan: store.openPlanner()
            case .ask: store.leavePlanner(workspace: .sonaChat)
            case .configure: store.leavePlanner(workspace: .sonaConfig)
            }
        })
    }

    private var conversation: Binding<String> {
        Binding(get: { store.conversationId ?? "" }, set: { store.open($0) })
    }
}

private enum ChatSheetMode: String, CaseIterable, Identifiable {
    case ask = "Ask"
    case configure = "Change settings"
    case plan = "Plan a project"
    var id: String { rawValue }
}

/// Replies other agents are waiting on, as one row that opens them.
private struct ChatRequestsRow: View {
    let count: Int
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Image(systemName: "tray.full")
                    .font(.system(size: 13, weight: .regular))
                    .foregroundStyle(Theme.inkSecondary)
                    .accessibilityHidden(true)
                Text("Replies waiting for you").bodyText(13)
                Spacer(minLength: 8)
                Chip("\(count)")
            }
            .padding(.horizontal, 12)
            .frame(height: 36)
            .background(
                hovering ? Theme.inset : Theme.surface,
                in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.radiusControl)
                    .strokeBorder(Theme.border, lineWidth: 1))
            .contentShape(RoundedRectangle(cornerRadius: Theme.radiusControl))
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .hoverFade(hovering)
    }
}

/// Three questions for an empty chat, drawn from what the other pages
/// already read: the next event on the calendar, the newest meeting, and
/// the newest meeting with a promise still open. Nothing is asked of the
/// core for them.
private struct ChatOpeningSuggestions: View {
    let store: ChatStore
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let offered = suggestions
        VStack(alignment: .leading, spacing: 0) {
            if !offered.isEmpty {
                ChatSuggestionPills(suggestions: offered) { store.ask($0) }
                    .transition(.opacity)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .animation(ChatMotion.arrival(reduceMotion), value: offered)
    }

    private var suggestions: [String] {
        guard store.offersSuggestions else { return [] }
        let now = Int64(Date.now.timeIntervalSince1970 * 1000)
        let next = model.overview.upcoming?.rows
            .filter { $0.startUtcMs > now }
            .min { $0.startUtcMs < $1.startUtcMs }
        let latest = model.meetings.entries.max { $0.createdAtUtcMs < $1.createdAtUtcMs }
        var open: FeedOpenLoop?
        if case let .loaded(loops) = model.overview.openLoops {
            open = loops.max { $0.atUtcMs < $1.atUtcMs }
        }
        return ChatSuggestions.pick(next: next?.title, latest: latest?.title, open: open?.title)
    }
}

/// The scrollback: what was said, what the turn did on the way, the one card
/// a settings answer becomes, and the cards a corpus change is offered as.
private struct ChatScrollback: View {
    let store: ChatStore

    var body: some View {
        let rows = store.rows
        let cardIndex = store.proposalRowIndex
        let retry = store.retryMessage
        let entries = ChatEntry.list(rows, work: store.workRowIndex, turn: store.turn, retry: retry)
        let first = entries.first?.id

        LazyVStack(alignment: .leading, spacing: 0) {
            ForEach(entries) { entry in
                view(for: entry, cardIndex: cardIndex, retry: retry)
                    .padding(.top, entry.id == first ? 0 : entry.gap)
                    .modifier(ChatEntrance())
            }

            /* The offer sits under the answer that made it, because the
             * answer is what explains it. Each card is its own row so a set
             * of three reads as three choices rather than one block. */
            ForEach(store.turn?.actions ?? []) { action in
                ChatActionCard(store: store, action: action)
                    .padding(.top, 12)
                    .modifier(ChatEntrance())
            }

            // A proposal with no row of its own is drawn rather than dropped.
            if cardIndex == -1, let proposal = store.proposal {
                ChatProposalCard(store: store, proposal: proposal)
                    .padding(.top, entries.isEmpty ? 0 : 16)
                    .modifier(ChatEntrance())
            }
            if let proposal = store.proposal, !proposal.rationale.isEmpty {
                ChatAnswer(message: proposal.rationale, landed: nil, store: store)
                    .equatable()
                    .padding(.top, 16)
                    .modifier(ChatEntrance())
            }
            if let question = store.proposal?.followUpQuestion {
                ChatAnswer(message: question, landed: nil, store: store)
                    .equatable()
                    .padding(.top, 16)
                    .modifier(ChatEntrance())
            }
        }
    }

    @ViewBuilder
    private func view(for entry: ChatEntry, cardIndex: Int, retry: String?) -> some View {
        switch entry {
        case let .work(turn):
            ChatWorkRow(store: store, turn: turn, retryMessage: retry)
        case let .row(index, row):
            if row.turn.role == .user {
                ChatQuestion(message: row.turn.message)
            } else if index == cardIndex, let proposal = store.proposal {
                ChatProposalCard(store: store, proposal: proposal)
            } else {
                ChatAnswer(message: row.turn.message, landed: landed(row), store: store).equatable()
            }
        case let .outcome(row, outcome):
            ChatOutcomeNote(
                store: store,
                outcome: outcome,
                message: row.turn.role == .user ? row.turn.message : nil)
        }
    }

    /// When `row` landed, for the one answer that arrived while the chat was
    /// on screen.
    private func landed(_ row: ChatRow) -> Date? {
        store.landing.flatMap { $0.rowId == row.id ? $0.at : nil }
    }
}

/// One entry of the scrollback, in reading order. The live turn's work is an
/// entry of its own, keyed by the turn, so the answer landing under it leaves
/// it where it was, with its fold as the reader left it.
private enum ChatEntry: Identifiable {
    case row(Int, ChatRow)
    /// The terminal result remembered with an earlier question.
    case outcome(ChatRow, AgentChatOutcome)
    case work(AgentPanelTurnStatus)

    var id: String {
        switch self {
        case let .row(_, row): row.id
        case let .outcome(row, _): row.id + "\u{1F}outcome"
        case let .work(turn): "work\u{1F}" + turn.turnId
        }
    }

    /// The space above this entry. A question opens a new exchange, so it
    /// stands apart; the work and the answer under it stay close to it.
    var gap: CGFloat {
        switch self {
        case let .row(_, row): row.turn.role == .user ? 28 : 16
        case .work: 12
        case .outcome: 6
        }
    }

    /// The work sits above the answer it produced, or last while there is
    /// none; `work` is the row it sits above, `-1` when the turn has nothing
    /// to say.
    static func list(
        _ rows: [ChatRow], work: Int, turn: AgentPanelTurnStatus?, retry: String?
    ) -> [ChatEntry] {
        var entries: [ChatEntry] = []
        for (index, row) in rows.enumerated() {
            if index == work, let turn {
                entries.append(.work(turn))
            }
            entries.append(.row(index, row))
            /* A failure the live turn is already reporting is the same
             * failure: the row it belongs to is the question above, and
             * one line about it is enough. */
            if let outcome = row.turn.outcome, !(index == rows.count - 1 && retry != nil) {
                entries.append(.outcome(row, outcome))
            }
        }
        if work == rows.count, let turn {
            entries.append(.work(turn))
        }
        return entries
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
/// A proposal that moves nothing is a question back, and gets no Apply: the
/// core refuses an empty change, so the button would only ever fail.
private struct ChatProposalCard: View {
    let store: ChatStore
    let proposal: AgentPanelProposal

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "slider.horizontal.3")
                .font(.system(size: 13, weight: .regular))
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: 16, height: 20)
                .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 4) {
                /* Wraps rather than truncates: this sentence is the whole of
                 * what the assistant said, and a press on Apply under half a
                 * sentence is a press on something nobody was shown. The
                 * same goes for the changes: one line each, with the target,
                 * because the card is the disclosure, not the prose. */
                Text(proposal.summary)
                    .bodyText(14)
                    .fixedSize(horizontal: false, vertical: true)
                ForEach(proposal.actions) { change in
                    Text(change.line)
                        .metaText()
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            Spacer(minLength: 8)

            switch proposal.state {
            case .pending where proposal.actions.isEmpty:
                EmptyView()
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
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.radiusCard)
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
    @State private var reviewing = false

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(action.action.line(naming: store.people))
                    .bodyText(14)
                    .fixedSize(horizontal: false, vertical: true)
                Text(action.action.reason)
                    .metaText()
                    .fixedSize(horizontal: false, vertical: true)
                if let detail = action.detail {
                    Text(detail).bodyText(13, Theme.inkSecondary).textSelection(.enabled)
                }
            }

            Spacer(minLength: 8)

            switch action.state {
            case .pending:
                HStack(spacing: 8) {
                    Button(action.action.needsReview ? "Review" : "Apply") {
                        if action.action.needsReview { reviewing = true }
                        else { store.applyAction(action.actionIndex) }
                    }
                        .buttonStyle(.compact)
                        .disabled(store.busy)
                    Button("Dismiss") { store.dismissAction(action.actionIndex) }
                        .buttonStyle(.quiet)
                        .disabled(store.busy)
                }
            case .applied:
                HStack(spacing: 8) {
                    Text(action.action.appliedLabel).metaText(Theme.inkSecondary)
                    if action.canUndo {
                        Button("Undo") { store.dismissAction(action.actionIndex) }
                            .buttonStyle(.quiet)
                            .disabled(store.busy)
                    }
                }
            case .dismissed:
                Text("Dismissed").metaText(Theme.inkSecondary)
            case .failed:
                Text("Not confirmed").metaText(Theme.inkSecondary)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.radiusCard)
                .strokeBorder(Theme.border, lineWidth: 1))
        .sheet(isPresented: $reviewing) { ChatActionReview(store: store, card: action) }
    }
}

/// The composer: one rounded well with room for two lines before the field
/// grows, and a toolbar under the field. In Ask the toolbar starts with the
/// screenshot menu; then comes the model, and at the far end the microphone
/// in Ask and the one round button, which is Send until a question is on
/// its way and Stop from then until the turn is over. A screenshot the next
/// question carries sits at the top of the well.
private struct ChatComposer: View {
    let store: ChatStore
    let onClose: () -> Void
    let openSettings: () -> Void
    @State private var fieldFocused = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let screenshot = store.screenshot {
                attachment(screenshot)
                    .padding(.horizontal, 12)
                    .padding(.top, 12)
            }
            field
                .padding(.horizontal, 14)
                .padding(.top, 10)
                .padding(.bottom, 4)
            toolbar
                .padding(.horizontal, 8)
                .padding(.bottom, 8)
        }
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.radiusCard)
                .strokeBorder(fieldFocused ? Theme.inkDisabled : Theme.border, lineWidth: 1)
                .animation(Motion.quick, value: fieldFocused)
                .allowsHitTesting(false))
    }

    private var field: some View {
        ZStack(alignment: .topLeading) {
            if store.draft.isEmpty {
                Text(store.workspace.prompt)
                    .bodyText(14, Theme.inkTertiary)
                    .padding(.top, 4)
                    .allowsHitTesting(false)
                    .accessibilityHidden(true)
            }
            ChatComposerField(
                text: Binding(get: { store.draft }, set: { store.editDraft($0) }),
                label: store.workspace.prompt,
                isEnabled: store.phase != .loading && store.phase != .disabled && !store.savingModel,
                send: { store.send() },
                escape: onClose,
                focusChanged: { fieldFocused = $0 })
        }
    }

    private var toolbar: some View {
        HStack(spacing: 4) {
            if store.workspace == .sonaChat {
                attach
            }
            ChatModelPicker(store: store, openSettings: openSettings)
            Spacer(minLength: 8)
            if store.workspace == .sonaChat {
                ChatWellButton(
                    symbol: store.voiceActive ? "waveform" : "mic",
                    help: store.voiceActive ? "End voice conversation" : "Start voice conversation",
                    note: "Audio stays on this Mac. Questions go to your server.",
                    isOn: store.voiceActive,
                    isEnabled: store.voiceActive || store.canStartVoice,
                    action: { if store.voiceActive { store.stopVoice() } else { store.startVoice() } })
            }
            if store.running || store.sending {
                ChatRoundButton(
                    symbol: "stop.fill", help: "Stop", isEnabled: !store.stopping,
                    action: { store.stop() })
            } else {
                ChatRoundButton(
                    symbol: "arrow.up", help: "Send", isEnabled: store.canSend,
                    action: { store.send() })
            }
        }
        .frame(height: 28)
    }

    private var attach: some View {
        Menu {
            Button("Use a window…") { store.chooseScreenshot(window: true) }
            Button("Choose screenshot…") { store.chooseScreenshot(window: false) }
        } label: {
            Label("Attach a screenshot", systemImage: "plus")
                .labelStyle(.iconOnly)
                .font(.system(size: 13, weight: .regular))
                .frame(width: 28, height: 28)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .modifier(ChatMenuGlyph())
        .disabled(!store.canAttachScreenshot)
        .help("Attach a screenshot")
        .accessibilityLabel("Attach a screenshot")
    }

    /// The screenshot the next question carries: the image in its own shape,
    /// small enough to leave the field its room, where it goes, and Remove.
    private func attachment(_ screenshot: ChatScreenshotDraft) -> some View {
        let size = Self.thumbnail(screenshot.image.size)
        return HStack(alignment: .top, spacing: 12) {
            Image(nsImage: screenshot.image)
                .resizable()
                .scaledToFit()
                .frame(width: size.width, height: size.height)
                .clipShape(RoundedRectangle(cornerRadius: Theme.radiusControl))
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.radiusControl)
                        .strokeBorder(Theme.border, lineWidth: 1))
                .accessibilityLabel("Screenshot to share with your next question")
            VStack(alignment: .leading, spacing: 4) {
                Text(screenshot.name)
                    .font(TypeScale.label(13))
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text("Sent to your selected model when you press Send. Add it again for follow-up questions.")
                    .metaText(Theme.inkSecondary)
                    .fixedSize(horizontal: false, vertical: true)
                Button("Remove") { store.removeScreenshot() }
                    .buttonStyle(.quiet)
                    .disabled(store.busy)
                    .accessibilityLabel("Remove screenshot")
                    .padding(.top, 4)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// The image's own proportions, no wider than 160 points and no taller
    /// than 96, and never drawn larger than it is.
    private static func thumbnail(_ size: CGSize) -> CGSize {
        guard size.width > 0, size.height > 0 else { return CGSize(width: 160, height: 96) }
        let scale = min(160 / size.width, 96 / size.height, 1)
        return CGSize(width: size.width * scale, height: size.height * scale)
    }
}

/// Send and Stop: one round button at the end of the toolbar, in full ink
/// while it can be pressed.
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
                .frame(width: 28, height: 28)
                .background(isEnabled ? Theme.invert : Theme.control, in: Circle())
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .disabled(!isEnabled)
        .help(help)
        .accessibilityLabel(help)
    }
}

/// A control in the toolbar that starts something, full ink while what it
/// started is running.
private struct ChatWellButton: View {
    let symbol: String
    let help: String
    /// What to know before pressing, added to the tooltip and read as the
    /// hint by assistive technology.
    let note: String
    let isOn: Bool
    let isEnabled: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
        }
        .buttonStyle(ChatGlyphButton(isOn: isOn))
        .disabled(!isEnabled)
        .help(help + ". " + note)
        .accessibilityLabel(help)
        .accessibilityHint(note)
    }
}

/// A glyph in the header or the toolbar: quiet ink on nothing, a soft wash
/// under the pointer, and full ink while what it started is running.
private struct ChatGlyphButton: ButtonStyle {
    var isOn = false
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 13, weight: isOn ? .semibold : .regular))
            .foregroundStyle(isEnabled ? (isOn || hovering ? Theme.ink : Theme.inkSecondary) : Theme.inkDisabled)
            .frame(width: 28, height: 28)
            .background(
                configuration.isPressed ? Theme.controlPressed : (isEnabled && hovering ? Theme.wash : Color.clear),
                in: RoundedRectangle(cornerRadius: Theme.radiusRow))
            .contentShape(RoundedRectangle(cornerRadius: Theme.radiusRow))
            .onHover { hovering = $0 }
            .hoverFade(hovering)
    }
}

/// The same glyph for a control that opens a menu. A menu draws its own
/// button, so the look is given from outside it.
private struct ChatMenuGlyph: ViewModifier {
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    func body(content: Content) -> some View {
        let lit = isEnabled && hovering
        content
            .tint(lit ? Theme.ink : Theme.inkSecondary)
            .foregroundStyle(lit ? Theme.ink : Theme.inkSecondary)
            .background(lit ? Theme.wash : Color.clear, in: RoundedRectangle(cornerRadius: Theme.radiusRow))
            .contentShape(RoundedRectangle(cornerRadius: Theme.radiusRow))
            .onHover { hovering = $0 }
            .hoverFade(lit)
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
    /// What the field is for, said to assistive technology. The visible
    /// placeholder is a separate view that leaves once there is text, so
    /// the field carries its own name.
    let label: String
    let isEnabled: Bool
    let send: () -> Void
    let escape: () -> Void
    /// Whether the caret is in the field, for the well's outline.
    let focusChanged: (Bool) -> Void

    /// Where React's composer stopped growing, which is about six lines.
    private static let maxHeight: CGFloat = 120
    private static let inset: CGFloat = 4
    private static let font = NSFont.systemFont(ofSize: 14)
    /// Two lines, so an empty field already reads as somewhere to write and
    /// a click anywhere in them lands in the text.
    private static let minHeight = height("\n", 1000)

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        let view = ChatComposerTextView()
        view.focusChanged = focusChanged
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
        view.minSize = NSSize(width: 0, height: Self.minHeight)
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
        guard let view = scroll.documentView as? ChatComposerTextView else { return }
        context.coordinator.text = $text
        context.coordinator.send = send
        context.coordinator.escape = escape
        view.focusChanged = focusChanged
        if view.string != text {
            view.string = text
        }
        view.isEditable = isEnabled
        view.setAccessibilityLabel(label)
        Self.focus(view, context.coordinator, isEnabled: isEnabled)
    }

    func sizeThatFits(
        _ proposal: ProposedViewSize, nsView: NSScrollView, context: Context
    ) -> CGSize? {
        let width = proposal.width ?? 240
        let wanted = Self.height(text, width)
        return CGSize(width: width, height: min(max(wanted, Self.minHeight), Self.maxHeight))
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

/// The composer's text view, reporting when it takes and gives up the caret.
private final class ChatComposerTextView: NSTextView {
    var focusChanged: (Bool) -> Void = { _ in }

    override func becomeFirstResponder() -> Bool {
        let became = super.becomeFirstResponder()
        if became { report(true) }
        return became
    }

    override func resignFirstResponder() -> Bool {
        let resigned = super.resignFirstResponder()
        if resigned { report(false) }
        return resigned
    }

    /// The caret is first given from inside `updateNSView`, where SwiftUI
    /// state may not change, so the report waits for the next turn.
    private func report(_ focused: Bool) {
        let focusChanged = focusChanged
        DispatchQueue.main.async { focusChanged(focused) }
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
        // AppKit also uses insertNewline: for Shift-Return.
        switch command {
        case Self.newline where NSApp.currentEvent?.modifierFlags.contains(.shift) != true:
            send()
            return true
        case Self.newline, Self.lineBreak:
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
