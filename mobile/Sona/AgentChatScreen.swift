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
                    } else {
                        starters
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

    @ViewBuilder
    private var starters: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("agent.title")
                .font(.title2.weight(.semibold))
                .foregroundStyle(Theme.textPrimary)
            Text(model.isPaired ? "agent.intro" : "agent.needsPairing")
                .font(.subheadline)
                .foregroundStyle(Theme.textSecondary)
        }
        .padding(.top, 24)
        if model.isPaired {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Self.starterKeys, id: \.self) { key in
                    Button { send(NSLocalizedString(key, comment: "")) } label: {
                        Text(LocalizedStringKey(key))
                            .font(.subheadline)
                            .foregroundStyle(Theme.textPrimary)
                            .multilineTextAlignment(.leading)
                            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                            .padding(.horizontal, 14)
                            .background(Theme.surface, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.top, 8)
        }
    }

    private static let starterKeys = ["agent.starter.open", "agent.starter.lastMeeting", "agent.starter.tomorrow"]

    private var composer: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let refusal = chat.refusal {
                Text(Self.refusalKey(refusal))
                    .font(.footnote)
                    .foregroundStyle(Theme.recording)
                    .padding(.horizontal, 6)
            }
            HStack(alignment: .bottom, spacing: 8) {
                TextField("agent.placeholder", text: $draft, axis: .vertical)
                    .lineLimit(1...6)
                    .focused($composing)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 11)
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
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
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
            Label("agent.status.queued", systemImage: "arrow.up.circle")
                .statusStyle()
        case .sent:
            Label("agent.status.sent", systemImage: "desktopcomputer")
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

    private static func markdown(_ text: String) -> AttributedString {
        (try? AttributedString(
            markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        )) ?? AttributedString(text)
    }
}

private extension View {
    func statusStyle() -> some View {
        font(.footnote).foregroundStyle(Theme.textSecondary)
    }
}
