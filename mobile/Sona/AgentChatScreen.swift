import SwiftUI

/// The button that opens the chat with Sona's agent, on the screens the app opens to.
struct AskSonaButton: View {
    @ObservedObject var model: AppModel

    var body: some View {
        Button { model.openChat(nil) } label: {
            Label("agent.open", systemImage: "bubble.left.and.text.bubble.right")
        }
        .accessibilityIdentifier("ask-sona")
    }
}

/// A chat with Sona's agent. The paired Mac answers each question through the vault.
struct AgentChatScreen: View {
    @ObservedObject var model: AppModel
    @ObservedObject var chat: PhoneAgentChat
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var draft = ""
    @FocusState private var composing: Bool

    private var conversation: AgentConversation? {
        model.chatConversation.flatMap { id in chat.conversations.first { $0.id == id } }
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 16) {
                    if let conversation {
                        ForEach(conversation.turns) { turn in
                            AgentTurnView(turn: turn) {
                                Task { await chat.retry(turn, in: conversation.id) }
                            }
                            .id(turn.id)
                        }
                    } else if !model.isPaired {
                        Text("agent.needsPairing")
                            .font(.subheadline)
                            .foregroundStyle(Theme.textSecondary)
                    }
                }
                .padding(.horizontal, Theme.pageInset)
                .padding(.vertical, 12)
                .frame(maxWidth: 680)
                .frame(maxWidth: .infinity)
            }
            .defaultScrollAnchor(.bottom)
            .scrollDismissesKeyboard(.interactively)
            .background(Theme.background)
            .navigationTitle(conversation.map { Text($0.title) } ?? Text("agent.title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("library.done") { dismiss() }
                }
                ToolbarItemGroup(placement: .primaryAction) {
                    if !chat.conversations.isEmpty { history }
                    Button { model.chatConversation = nil; composing = true } label: {
                        Label("agent.new", systemImage: "square.and.pencil")
                    }
                    .disabled(conversation == nil)
                }
            }
            .safeAreaBar(edge: .bottom) { composer }
            .onChange(of: scenePhase, initial: true) { _, phase in if phase == .active { chat.watch() } }
            .onAppear { composing = conversation == nil && model.isPaired }
        }
        .tint(Theme.accent)
    }

    private var history: some View {
        Menu {
            ForEach(chat.conversations) { item in
                Button { model.chatConversation = item.id } label: {
                    Label(item.title, systemImage: item.id == model.chatConversation ? "checkmark" : "bubble.left")
                }
            }
        } label: {
            Label("agent.history", systemImage: "clock.arrow.circlepath")
        }
    }

    private var composer: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let refusal = chat.refusal {
                Text(Self.refusalKey(refusal))
                    .font(.footnote)
                    .foregroundStyle(Theme.recording)
                    .padding(.horizontal, 16)
            }
            HStack(alignment: .bottom, spacing: 8) {
                TextField("agent.placeholder", text: $draft, axis: .vertical)
                    .lineLimit(1...6)
                    .focused($composing)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .accessibilityIdentifier("agent-composer")
                Button { send(draft) } label: {
                    Image(systemName: "arrow.up")
                        .font(.body.weight(.semibold))
                        .frame(width: 36, height: 36)
                        .background(canSend ? Theme.accent : Theme.inset, in: Circle())
                        .foregroundStyle(canSend ? Color.white : Theme.textTertiary)
                }
                .buttonStyle(.plain)
                .disabled(!canSend)
                .padding(4)
                .accessibilityLabel(Text("agent.send"))
                .accessibilityIdentifier("agent-send")
            }
            .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
        }
        .padding(.horizontal, Theme.pageInset)
        .padding(.bottom, 8)
        .frame(maxWidth: 680)
        .frame(maxWidth: .infinity)
    }

    private var canSend: Bool {
        model.isPaired && !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func send(_ text: String) {
        let into = model.chatConversation
        Task {
            guard let asked = await chat.ask(text, in: into) else { return }
            draft = ""
            model.chatConversation = asked.conversation
        }
    }

    private static func refusalKey(_ refusal: AgentAskObject.Refusal) -> LocalizedStringKey {
        switch refusal {
        case .empty: return "agent.refusal.empty"
        case .tooLong: return "agent.refusal.tooLong"
        case .address: return "agent.refusal.address"
        }
    }
}

private struct AgentTurnView: View {
    let turn: AgentTurn
    let retry: () -> Void

    var body: some View {
        switch turn.role {
        case .user:
            VStack(alignment: .trailing, spacing: 6) {
                Text(turn.text)
                    .font(.body)
                    .foregroundStyle(Color.white)
                    .textSelection(.enabled)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(Theme.accent, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
                status
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
            .padding(.leading, 48)
        case .assistant:
            Text(Self.markdown(turn.text))
                .font(.body)
                .foregroundStyle(Theme.textPrimary)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contextMenu {
                    Button { UIPasteboard.general.string = turn.text } label: {
                        Label("library.copy", systemImage: "doc.on.doc")
                    }
                    ShareLink(item: turn.text) { Label("library.share", systemImage: "square.and.arrow.up") }
                }
        }
    }

    @ViewBuilder
    private var status: some View {
        switch turn.status {
        case .queued:
            Text("agent.status.queued")
                .statusStyle()
        case .sent:
            Text("agent.status.sent")
                .statusStyle()
        case .working:
            HStack(spacing: 6) {
                ProgressView().controlSize(.mini)
                Text("agent.status.working")
            }
            .statusStyle()
        case .unanswered:
            Text(LocalizedStringKey(AgentFailure.message(turn.status)))
                .statusStyle()
                .multilineTextAlignment(.trailing)
        case .failed:
            VStack(alignment: .trailing, spacing: 4) {
                Text(LocalizedStringKey(AgentFailure.message(turn.status)))
                    .multilineTextAlignment(.trailing)
                Button("agent.retry", action: retry)
                    .font(.footnote.weight(.semibold))
            }
            .statusStyle()
        case .answered, nil:
            EmptyView()
        }
    }

    /// Inline styling only. A link in an answer can come from any transcript the Mac
    /// looked at, so it shows as text and opens nothing.
    private static func markdown(_ text: String) -> AttributedString {
        var styled = (try? AttributedString(
            markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        )) ?? AttributedString(text)
        styled.link = nil
        return styled
    }
}

private extension View {
    func statusStyle() -> some View {
        font(.footnote).foregroundStyle(Theme.textSecondary)
    }
}

extension Binding where Value == Bool {
    /// False while the chat is up, so a screen's own sheet steps aside when a
    /// notification opens the chat over it.
    func yielding(to model: AppModel) -> Binding<Bool> {
        Binding(get: { wrappedValue && !model.showsChat }, set: { wrappedValue = $0 })
    }
}

extension Binding {
    func yielding<Item>(to model: AppModel) -> Binding<Item?> where Value == Item? {
        Binding<Item?>(get: { model.showsChat ? nil : wrappedValue }, set: { wrappedValue = $0 })
    }
}
