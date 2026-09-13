import SwiftUI

/// The agent bridge, as settings sections: the switches that decide whether
/// any of it runs, the folders it may run in, the code to paste into an
/// agent, then what it has been doing and what is waiting on a person.
struct AgentBridgeView: View {
    let store: AgentBridgeStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            ErrorNote(store.error)
            controls
            projects
            hook
            activity
            queue
            rules
        }
    }

    private var header: some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Agents").headlineText()
                Text("Coding agents on this Mac, what they asked for, and what Sona may answer.")
                    .bodyText(15, Theme.inkSecondary)
            }
            Spacer()
            Button(store.loading ? "Refreshing…" : "Refresh") { store.refreshAll() }
                .buttonStyle(.compact)
                .disabled(store.loading)
        }
        .padding(.bottom, 20)
    }

    private var controls: some View {
        AgentBridgeSection("Agent controls") {
            Card {
                ToggleRow(title: "Enable agent connections", isOn: master)
                ForEach(AgentBridgeAgent.allCases, id: \.self) { agent in
                    ToggleRow(title: agent.label, detail: agent.detail, isOn: enabled(agent))
                        .disabled(!store.bridge.masterEnabled)
                }
                if let status = store.status {
                    CardRow {
                        Text("Status").bodyText()
                    } trailing: {
                        Text(status.diagnostic.label).metaText()
                    }
                }
            }
        }
    }

    private var projects: some View {
        AgentBridgeSection(
            "Authorized projects",
            meta: "Sona recognizes a project from its folder without keeping the folder path here."
        ) {
            Button(store.authorizing ? "Choosing…" : "Authorize folder") { store.authorizeProject() }
                .buttonStyle(.compact)
                .disabled(store.authorizing)
        } content: {
            Card {
                if store.bridge.allowedProjects.isEmpty {
                    AgentBridgeEmpty("No projects are authorized.")
                } else {
                    ForEach(store.bridge.allowedProjects) { project in
                        CardRow {
                            AgentBridgeHash(project.canonicalProjectHash)
                        } trailing: {
                            Button("Remove") { store.removeProject(project.canonicalProjectHash) }
                                .buttonStyle(QuietButton(color: Theme.live))
                        }
                    }
                }
            }
        }
    }

    /// Nothing to copy and nothing to report: the section stays off the page
    /// rather than drawing an empty surface.
    @ViewBuilder
    private var hook: some View {
        if store.hookSnippet != nil || store.hookError != nil {
            AgentBridgeSection(
                "Setup code",
                meta: "Paste this into your agent's own configuration. Sona never changes agent files."
            ) {
                if store.hookSnippet != nil {
                    Button(store.hookCopied ? "Copied" : "Copy setup code") { store.copyHookSnippet() }
                        .buttonStyle(.compact)
                }
            } content: {
                Card {
                    if let hookError = store.hookError {
                        AgentBridgeBlock {
                            Text("Setup code unavailable: \(hookError)")
                                .bodyText(14, Theme.live)
                                .textSelection(.enabled)
                        }
                    }
                    if let snippet = store.hookSnippet {
                        AgentBridgeBlock {
                            Text(snippet)
                                .font(TypeScale.mono(12))
                                .foregroundStyle(Theme.inkSecondary)
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                }
            }
        }
    }

    private var activity: some View {
        VStack(alignment: .leading, spacing: 0) {
            AgentBridgeSection("Sessions") {
                Card {
                    if store.sessions.isEmpty {
                        AgentBridgeEmpty("No agent activity yet.")
                    } else {
                        ForEach(store.sessions) { session in
                            CardRow {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(session.agent.label).bodyText()
                                    AgentBridgeHash(session.id)
                                    AgentBridgeHash(session.canonicalProjectHash)
                                }
                            } trailing: {
                                Text("Seen \(session.lastSeen.time)").metaText()
                            }
                        }
                    }
                }
            }
            AgentBridgeSection("Requests") {
                Card {
                    if store.requests.isEmpty {
                        AgentBridgeEmpty("No requests yet.")
                    } else {
                        ForEach(store.requests) { request in
                            AgentBridgeRequestRow(store: store, request: request)
                        }
                    }
                }
            }
        }
    }

    private var queue: some View {
        VStack(alignment: .leading, spacing: 0) {
            AgentBridgeSection("Reply preview") {
                AgentBridgeComposer(store: store)
            }
            AgentBridgeSection("Pending replies") {
                Card {
                    if store.pending.isEmpty {
                        AgentBridgeEmpty("No pending replies.")
                    } else {
                        ForEach(store.pending) { message in
                            AgentBridgePendingRow(store: store, message: message)
                        }
                    }
                }
            }
        }
    }

    private var rules: some View {
        AgentBridgeSection(
            "Permission rules",
            meta: "One rule per answer you gave: that exact tool call, in that project."
        ) {
            Card {
                if store.bridge.permissionRules.isEmpty {
                    AgentBridgeEmpty("No permission rules.")
                } else {
                    ForEach(store.bridge.permissionRules) { rule in
                        CardRow {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("\(rule.agent.label) · \(rule.toolName) · \(rule.decision.label)")
                                    .bodyText()
                                AgentBridgeHash(rule.canonicalProjectHash)
                            }
                        } trailing: {
                            Button("Remove") { store.deleteRule(rule.id) }
                                .buttonStyle(QuietButton(color: Theme.live))
                        }
                    }
                }
            }
        }
    }

    private var master: Binding<Bool> {
        Binding(get: { store.bridge.masterEnabled }, set: { store.setMaster($0) })
    }

    private func enabled(_ agent: AgentBridgeAgent) -> Binding<Bool> {
        Binding(get: { store.bridge.isEnabled(agent) }, set: { store.setAgent(agent, enabled: $0) })
    }
}

/// One observed hook invocation. Allow and Deny appear only where an answer
/// reaches the agent; where the hook already returned, the row says so
/// instead of offering an answer nobody is waiting to claim.
private struct AgentBridgeRequestRow: View {
    let store: AgentBridgeStore
    let request: AgentBridgeObservedRequest

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(request.headline).bodyText()
                Text("Expires \(AgentBridgeFormat.expiry.string(from: request.expiresAt))").metaText()
                if request.observeOnly {
                    Text("Sona can only watch this request. Answer it in \(request.agent.label).")
                        .metaText(Theme.inkSecondary)
                }
            }
        } trailing: {
            HStack(spacing: 12) {
                if request.canRespond(interactiveReady: store.interactiveReady) {
                    Button("Allow exactly this") { store.decidePermission(request, .allow) }
                        .buttonStyle(.compact)
                    Button("Deny exactly this") { store.decidePermission(request, .deny) }
                        .buttonStyle(QuietButton(color: Theme.live))
                }
                if request.isOpen {
                    Button("Dismiss") { store.dismissRequest(request.id) }
                        .buttonStyle(.quiet)
                }
            }
        }
    }
}

/// One reply the core is holding. Confirming sends back the same id, session
/// and words the preview came with; anything else the core refuses.
private struct AgentBridgePendingRow: View {
    let store: AgentBridgeStore
    let message: AgentBridgePendingMessage

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(message.agent.label).bodyText()
                    AgentBridgeHash(message.sessionId)
                }
                Text(message.text).bodyText(14).textSelection(.enabled)
                Text(message.stateLabel).metaText()
            }
        } trailing: {
            HStack(spacing: 12) {
                if message.canConfirm {
                    Button("Confirm exact reply") { store.confirmPending(message) }
                        .buttonStyle(.primary)
                }
                if message.canCancel {
                    Button("Cancel") { store.cancelPending(message.id) }
                        .buttonStyle(.quiet)
                }
            }
        }
    }
}

/// The two-step composer: choose the session, write the words, and the core
/// holds them until they are confirmed below.
private struct AgentBridgeComposer: View {
    @Bindable var store: AgentBridgeStore

    var body: some View {
        Card {
            if !store.interactiveReady {
                AgentBridgeEmpty("Turn on agent connections and at least one agent before replying.")
            }
            CardRow {
                Text("Reply destination").bodyText()
            } trailing: {
                if store.replySessions.isEmpty {
                    Text("No session Sona can reply to").metaText()
                } else {
                    Picker("", selection: $store.replySessionId) {
                        ForEach(store.replySessions) { session in
                            Text("\(session.agent.label) · \(AgentBridgeFormat.short(session.id))")
                                .tag(session.id)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .frame(maxWidth: 280)
                    .disabled(!store.interactiveReady)
                }
            }
            AgentBridgeBlock {
                Text("Reply text").bodyText()
                TextEditor(text: $store.replyText)
                    .font(TypeScale.body())
                    .foregroundStyle(Theme.ink)
                    .scrollContentBackground(.hidden)
                    .padding(8)
                    .frame(height: 96)
                    .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                    .overlay(
                        RoundedRectangle(cornerRadius: Theme.radiusControl)
                            .strokeBorder(Theme.border, lineWidth: 1)
                    )
                    .disabled(!store.interactiveReady || store.replySessionId.isEmpty)
                HStack {
                    Spacer()
                    Button("Create preview") { store.createReplyPreview() }
                        .buttonStyle(.primary)
                        .disabled(!store.canCreatePreview)
                }
            }
        }
    }
}

/// A labelled block with an action beside the label: PageSection, plus the
/// one button a section owns.
private struct AgentBridgeSection<Action: View, Content: View>: View {
    let label: String
    let meta: String?
    let action: Action
    let content: Content

    init(
        _ label: String,
        meta: String? = nil,
        @ViewBuilder action: () -> Action,
        @ViewBuilder content: () -> Content
    ) {
        self.label = label
        self.meta = meta
        self.action = action()
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(label).sectionLabel()
                    if let meta {
                        Text(meta).metaText()
                    }
                }
                Spacer(minLength: 16)
                action
            }
            content
        }
        .padding(.bottom, 32)
    }
}

extension AgentBridgeSection where Action == EmptyView {
    init(_ label: String, meta: String? = nil, @ViewBuilder content: () -> Content) {
        self.init(label, meta: meta, action: { EmptyView() }, content: content)
    }
}

/// A card row that is one stack of full-width content rather than a leading
/// and a trailing side.
private struct AgentBridgeBlock<Content: View>: View {
    let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            content
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { Hairline() }
    }
}

/// What a list says when it has nothing in it.
private struct AgentBridgeEmpty: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        CardRow {
            Text(text).metaText()
        }
    }
}

/// An opaque identifier the bridge hands out: a hash, readable but quiet.
private struct AgentBridgeHash: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(text)
            .font(TypeScale.mono(12))
            .foregroundStyle(Theme.inkTertiary)
            .textSelection(.enabled)
    }
}
