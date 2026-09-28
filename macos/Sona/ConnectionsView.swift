import SwiftUI

struct ConnectionsView: View {
    @Bindable var store: ConnectionsStore
    @State private var editing: SonaConnection?
    @State private var disconnecting: SonaConnection?
    @State private var addingRule = false

    var body: some View {
        Page {
            PageTitle("Connections", subtitle: "Send generated notes to services you choose. Nothing is connected or sent automatically until you turn it on.")
            if let error = store.error { Text(error).bodyText(14, Theme.inkSecondary) }
            if let notice = store.notice { Text(notice).bodyText(14, Theme.inkSecondary) }
            PageSection("Services") {
                Card {
                    if store.loading && store.snapshot == nil { CardLine("Reading connections…") }
                    else if store.snapshot == nil { CardRow { Text("Connections could not be read.").bodyText() } trailing: { Button("Try again") { Task { await store.load() } }.buttonStyle(.quiet) } }
                    else if store.connections.isEmpty { CardLine("No services connected. Your notes stay in Sona.") }
                    ForEach(store.connections) { connection in
                        CardRow {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(connection.name).font(TypeScale.label(15))
                                Text("\(connection.kind.title) · \(connection.enabled ? "On" : "Off")").metaText()
                            }
                        } trailing: {
                            HStack(spacing: 14) {
                                Button("Edit") { editing = connection }.buttonStyle(.quiet)
                                Button("Test") { Task { await store.test(connection) } }.buttonStyle(.quiet).disabled(!connection.enabled)
                                Button("Disconnect") { disconnecting = connection }.buttonStyle(.quiet)
                            }
                        }
                    }
                }
                Button("Connect a service") { editing = SonaConnection(id: UUID().uuidString.lowercased(), name: "", kind: .slackWebhook, target: "", enabled: false) }
                    .buttonStyle(.primary).disabled(store.snapshot == nil || store.busy)
            }
            PageSection("Send automatically") {
                Card {
                    CardLine("Only future meetings or notes newly added to a folder are sent. Existing notes are never backfilled. Only generated notes leave Sona, not transcripts or your private notes.")
                    if let snapshot = store.snapshot {
                        if snapshot.rules.isEmpty { CardLine("No automatic sends. Every send needs your approval.") }
                        ForEach(snapshot.rules) { rule in
                            CardRow {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(snapshot.connections.first(where: { $0.id == rule.connectionId })?.name ?? "Disconnected service").font(TypeScale.label(15))
                                    Text(snapshot.scopes.first(where: { $0.scope == rule.scope })?.name ?? "Unavailable folder or series").metaText()
                                }
                            } trailing: {
                                Button("Stop sending") { Task { await store.removeRule(rule) } }.buttonStyle(.quiet)
                            }
                        }
                    }
                }
                Button("Add an automatic send") { addingRule = true }.buttonStyle(.secondary).disabled(store.enabled.isEmpty || store.busy)
            }
            PageSection("Actions from chat") {
                Card {
                    if let preferences = store.snapshot?.preferences {
                        CardRow {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("Email drafts").font(TypeScale.label(15))
                                Text("Approval opens an editable draft in Mail. Sona never sends it.").metaText()
                            }
                        } trailing: {
                            Toggle("Email drafts", isOn: Binding(get: { preferences.emailDraftsEnabled }, set: { value in
                                Task { await store.preferences(email: value, calendar: preferences.calendarActionsEnabled) }
                            })).labelsHidden().toggleStyle(.switch)
                        }
                        CardRow {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("Calendar events").font(TypeScale.label(15))
                                Text("Approval creates an event in your default calendar. No invitations are sent.").metaText()
                            }
                        } trailing: {
                            Toggle("Calendar events", isOn: Binding(get: { preferences.calendarActionsEnabled }, set: { value in
                                Task { await store.preferences(email: preferences.emailDraftsEnabled, calendar: value) }
                            })).labelsHidden().toggleStyle(.switch)
                        }
                    } else { CardLine("Read connections to change these permissions.") }
                }
            }
            PageSection("Recent sends") {
                Card {
                    if let snapshot = store.snapshot {
                        if snapshot.receipts.isEmpty { CardLine("Nothing has been sent.") }
                        ForEach(snapshot.receipts) { receipt in
                            CardRow {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(receipt.connectionName).font(TypeScale.label(15))
                                    Text(receipt.detail).bodyText(13, Theme.inkSecondary)
                                    Text(receipt.date, style: .date).metaText()
                                }
                            } trailing: {
                                if receipt.canUndo { Button("Undo") { Task { await store.undo(receipt) } }.buttonStyle(.quiet) }
                            }
                        }
                    } else { CardLine("Recent sends are unavailable until the library is read.") }
                }
                Button("Refresh") { Task { await store.load() } }.buttonStyle(.quiet)
            }
        }
        .disabled(store.busy)
        .task { await store.load() }
        .sheet(item: $editing) { connection in ConnectionEditor(store: store, original: connection) }
        .sheet(isPresented: $addingRule) { ConnectionRuleEditor(store: store) }
        .confirmationDialog("Disconnect this service?", isPresented: Binding(get: { disconnecting != nil }, set: { if !$0 { disconnecting = nil } })) {
            Button("Disconnect", role: .destructive) {
                if let connection = disconnecting { Task { await store.disconnect(connection) } }
                disconnecting = nil
            }
        } message: { Text("This stops automatic sends and removes the credential from Keychain. Copies already sent stay at the destination.") }
    }
}

private struct ConnectionEditor: View {
    @Bindable var store: ConnectionsStore
    let original: SonaConnection
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var kind: SonaConnectionKind = .slackWebhook
    @State private var target = ""
    @State private var token = ""
    @State private var address = ""
    @State private var enabled = false
    private var existing: Bool { store.connections.contains(where: { $0.id == original.id }) }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text(existing ? "Edit connection" : "Connect a service").headlineText()
            Form {
                TextField("Name", text: $name)
                Picker("Service", selection: $kind) { ForEach(SonaConnectionKind.allCases) { Text($0.title).tag($0) } }.disabled(existing)
                if !kind.webhook { TextField(kind.targetLabel, text: $target) }
                if kind.webhook { SecureField(existing ? "Webhook address (blank keeps it)" : "Webhook address", text: $address) }
                if kind != .slackWebhook { SecureField(kind == .publicWebhook ? "Signing secret" : "API token or key", text: $token) }
                Toggle("Allow Sona to send to this destination", isOn: $enabled)
            }
            .textFieldStyle(.roundedBorder)
            Text(kind.help).bodyText(13, Theme.inkSecondary)
            if existing { Text("Leave credential fields blank to keep them. Credentials are stored in Keychain and never shown here.").metaText() }
            if kind == .publicWebhook {
                Text("Receivers verify X-Sona-Signature = sha256=hex(HMAC-SHA256(secret, timestamp + \".\" + exact body)). Use X-Sona-Timestamp and X-Sona-Delivery to reject old or duplicate deliveries.").metaText().textSelection(.enabled)
            }
            if let error = store.error { Text(error).bodyText(13, Theme.inkSecondary) }
            HStack {
                Button("Cancel") { dismiss() }.buttonStyle(.quiet).keyboardShortcut(.cancelAction)
                Spacer()
                Button(store.busy ? "Saving…" : "Save connection") {
                    let connection = SonaConnection(id: original.id, name: name.trimmingCharacters(in: .whitespacesAndNewlines), kind: kind, target: kind.webhook ? "" : target.trimmingCharacters(in: .whitespacesAndNewlines), enabled: enabled)
                    Task { if await store.save(connection, token: token, address: address) { token = ""; address = ""; dismiss() } }
                }.buttonStyle(.primary).disabled(name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || store.busy)
            }
        }
        .padding(28).frame(width: 570).background(Theme.page)
        .onAppear { name = original.name; kind = original.kind; target = original.target; enabled = original.enabled }
        .onChange(of: kind) { _, _ in if !existing { target = ""; token = ""; address = "" } }
        .interactiveDismissDisabled(store.busy)
    }
}

private struct ConnectionRuleEditor: View {
    @Bindable var store: ConnectionsStore
    @Environment(\.dismiss) private var dismiss
    @State private var connectionId = ""
    @State private var scope = ConnectionScope(kind: "all", id: nil)
    @State private var consent = false
    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Send notes automatically").headlineText()
            Picker("Destination", selection: $connectionId) {
                Text("Choose a service").tag("")
                ForEach(store.enabled) { Text($0.name).tag($0.id) }
            }
            Picker("Meetings", selection: $scope) {
                ForEach(store.snapshot?.scopes ?? []) { Text($0.name).tag($0.scope) }
            }
            Toggle("Send future notes without asking me each time", isOn: $consent)
            Text("Generated notes leave this Mac after they are ready, or when you add a meeting to this folder. Existing notes are not sent. Stop the rule here at any time.").bodyText(13, Theme.inkSecondary)
            if let error = store.error { Text(error).metaText() }
            HStack {
                Button("Cancel") { dismiss() }.buttonStyle(.quiet)
                Spacer()
                Button("Start sending") { Task { if await store.addRule(connectionId: connectionId, scope: scope) { dismiss() } } }
                    .buttonStyle(.primary).disabled(!consent || connectionId.isEmpty || store.busy)
            }
        }.padding(28).frame(width: 520).background(Theme.page).interactiveDismissDisabled(store.busy)
    }
}

struct MeetingSendToMenu: View {
    @Environment(AppModel.self) private var model
    let sessionId: String
    @State private var selected: SonaConnection?
    var body: some View {
        Menu("Send to") {
            if model.connections.loading { Text("Reading connections…") }
            ForEach(model.connections.enabled) { connection in Button(connection.name) { selected = connection } }
            if model.connections.enabled.isEmpty { Text("No enabled connections") }
            Divider()
            Button("Connections settings") { model.showSettings(.connections) }
        }
        .disabled(model.connections.busy)
        .task { await model.connections.load() }
        .sheet(item: $selected) { connection in ConnectionSendReview(store: model.connections, connection: connection, sessionId: sessionId) }
    }
}

private struct ConnectionSendReview: View {
    @Bindable var store: ConnectionsStore
    let connection: SonaConnection
    let sessionId: String
    @Environment(\.dismiss) private var dismiss
    @State private var preview: ConnectionNotesPreview?
    @State private var error: String?
    @State private var receipt: ConnectionReceipt?
    @State private var operationId = UUID().uuidString.lowercased()
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Send to \(connection.name)").headlineText()
            Text("Only generated notes are sent. Transcripts and your private notes stay in Sona. People with access to the destination can read the sent copy.").bodyText(13, Theme.inkSecondary)
            if let preview {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        Text(preview.title).font(TypeScale.label(16))
                        Text(preview.notes.isEmpty ? "No generated note text. The meeting title will still be sent." : preview.notes).bodyText()
                    }.frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
                }.frame(minHeight: 160, maxHeight: 350)
            } else if let error {
                Text(error).bodyText()
                Button("Try again") { Task { await load() } }.buttonStyle(.quiet)
            } else { ProgressView("Reading generated notes…") }
            if let error = store.error { Text(error).bodyText(13, Theme.inkSecondary) }
            if let receipt { Text(receipt.detail).bodyText(13, Theme.inkSecondary) }
            HStack {
                Button(receipt == nil ? "Cancel" : "Close") { dismiss() }.buttonStyle(.quiet)
                Spacer()
                if receipt == nil {
                    Button(store.busy ? "Sending…" : "Send notes") { Task { receipt = await store.send(connection: connection, sessionId: sessionId, operationId: operationId) } }
                        .buttonStyle(.primary).disabled(preview == nil || store.busy)
                }
            }
        }.padding(28).frame(width: 590).background(Theme.page)
        .task { await load() }.interactiveDismissDisabled(store.busy)
    }
    private func load() async {
        error = nil
        do { preview = try await store.preview(sessionId: sessionId) }
        catch { self.error = error.localizedDescription }
    }
}
