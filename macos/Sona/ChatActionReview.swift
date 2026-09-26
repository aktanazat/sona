import SwiftUI

struct ChatEmailDraft: Decodable {
    let reason: String
    let recipients: [String]
    let subject: String
    let body: String
}
struct ChatSlackDraft: Decodable {
    let reason: String
    let connectionId: String
    let text: String
}
struct ChatCalendarDraft: Decodable {
    let reason: String
    let title: String
    let startUtcMs: Int64
    let endUtcMs: Int64
    let notes: String
    let location: String
}
struct ChatNotesSend: Decodable {
    let reason: String
    let connectionId: String
    let sessionId: String
}

struct ChatActionReview: View {
    let store: ChatStore
    let card: AgentPanelAction
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var recipients = ""
    @State private var title = ""
    @State private var bodyText = ""
    @State private var location = ""
    @State private var start = Date()
    @State private var end = Date()
    @State private var notes: ConnectionNotesPreview?
    @State private var loadError: String?
    @State private var loaded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text(card.action.line(naming: store.people)).headlineText()
            Text(card.action.reason).bodyText(14, Theme.inkSecondary)
            content
            if let error = store.error { Text(error).bodyText(13, Theme.inkSecondary) }
            if let loadError {
                Text(loadError).bodyText(13, Theme.inkSecondary)
                Button("Try again") { Task { await load() } }.buttonStyle(.quiet)
            }
            HStack {
                Button("Cancel") { dismiss() }.buttonStyle(.quiet).keyboardShortcut(.cancelAction)
                Spacer()
                Button(store.busy ? "Applying…" : approvalLabel) {
                    if let action = reviewedAction {
                        Task { if await store.approveReviewedAction(card.actionIndex, action: action) { dismiss() } }
                    }
                }.buttonStyle(.primary).disabled(store.busy || !loaded || loadError != nil)
            }
        }
        .padding(28).frame(width: 590).background(Theme.page)
        .interactiveDismissDisabled(store.busy)
        .task { populate(); await load() }
    }

    @ViewBuilder private var content: some View {
        switch card.action {
        case .draftEmail:
            TextField("To (separate addresses with commas)", text: $recipients).textFieldStyle(.roundedBorder)
            TextField("Subject", text: $title).textFieldStyle(.roundedBorder)
            editor
            Text("This opens a draft in Mail. Edit or close it there. Sona cannot send it or close the draft for you.").metaText()
        case let .postSlack(draft):
            destination(draft.connectionId)
            editor
            Text("Approval posts this message to the destination above. People there can read it immediately.").metaText()
        case .createCalendarEvent:
            TextField("Title", text: $title).textFieldStyle(.roundedBorder)
            DatePicker("Starts", selection: $start, displayedComponents: [.date, .hourAndMinute])
            DatePicker("Ends", selection: $end, displayedComponents: [.date, .hourAndMinute])
            TextField("Location (optional)", text: $location).textFieldStyle(.roundedBorder)
            editor
            Text("Uses this Mac's time zone and your default calendar. No invitees are added and no invitations are sent.").metaText()
        case let .sendNotes(draft):
            destination(draft.connectionId)
            if let notes {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        Text(notes.title).font(TypeScale.label(16))
                        Text(notes.notes.isEmpty ? "No generated note text. The meeting title will still be sent." : notes.notes).bodyText()
                    }.frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
                }.frame(height: 250)
            } else if loadError == nil { ProgressView("Reading generated notes…") }
            Text("Only generated notes are sent. Transcripts and your private notes stay in Sona.").metaText()
        default:
            Text("This action does not need an external review.").metaText()
        }
    }

    private var editor: some View {
        TextEditor(text: $bodyText).font(TypeScale.body(14)).frame(height: 220)
            .padding(8).background(Theme.surface)
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).stroke(Theme.border, lineWidth: 1))
            .accessibilityLabel("Message or event notes")
    }

    @ViewBuilder private func destination(_ id: String) -> some View {
        if let connection = model.connections.connections.first(where: { $0.id == id }) {
            Text("To: \(connection.name) · \(connection.kind.title)").font(TypeScale.label(14))
            if connection.kind.webhook { Text("This destination cannot undo a send.").metaText() }
        } else if let error = model.connections.error {
            Text(error).metaText()
        } else { Text(model.connections.loading ? "Reading destination…" : "Destination unavailable").metaText() }
    }

    private var approvalLabel: String {
        switch card.action {
        case .draftEmail: "Open draft in Mail"
        case .postSlack: "Post to Slack"
        case .createCalendarEvent: "Create event"
        case .sendNotes: "Send notes"
        default: "Apply"
        }
    }

    private var reviewedAction: JSONValue? {
        var fields: [String: JSONValue] = ["reason": .string(card.action.reason)]
        switch card.action {
        case .draftEmail:
            fields["kind"] = "draft_email"
            fields["recipients"] = .array(recipients.split(separator: ",").map { .string($0.trimmingCharacters(in: .whitespacesAndNewlines)) })
            fields["subject"] = .string(title); fields["body"] = .string(bodyText)
        case let .postSlack(draft):
            fields["kind"] = "post_slack"; fields["connection_id"] = .string(draft.connectionId); fields["text"] = .string(bodyText)
        case .createCalendarEvent:
            fields["kind"] = "create_calendar_event"; fields["title"] = .string(title)
            fields["start_utc_ms"] = .number((start.timeIntervalSince1970 * 1000).rounded())
            fields["end_utc_ms"] = .number((end.timeIntervalSince1970 * 1000).rounded())
            fields["notes"] = .string(bodyText); fields["location"] = .string(location)
        case let .sendNotes(draft):
            fields["kind"] = "send_notes"; fields["connection_id"] = .string(draft.connectionId); fields["session_id"] = .string(draft.sessionId)
        default: return nil
        }
        return .object(fields)
    }

    private func populate() {
        switch card.action {
        case let .draftEmail(draft): recipients = draft.recipients.joined(separator: ", "); title = draft.subject; bodyText = draft.body
        case let .postSlack(draft): bodyText = draft.text
        case let .createCalendarEvent(draft):
            title = draft.title; bodyText = draft.notes; location = draft.location
            start = Date(timeIntervalSince1970: Double(draft.startUtcMs) / 1000)
            end = Date(timeIntervalSince1970: Double(draft.endUtcMs) / 1000)
        default: break
        }
    }

    private func load() async {
        loaded = false; loadError = nil
        switch card.action {
        case let .postSlack(draft):
            await model.connections.load()
            if !model.connections.enabled.contains(where: { $0.id == draft.connectionId }) { loadError = model.connections.error ?? "This destination is disabled or disconnected." }
        case let .sendNotes(draft):
            await model.connections.load()
            if !model.connections.enabled.contains(where: { $0.id == draft.connectionId }) { loadError = model.connections.error ?? "This destination is disabled or disconnected." }
            do { notes = try await model.connections.preview(sessionId: draft.sessionId) }
            catch { loadError = error.localizedDescription }
        default: break
        }
        loaded = true
    }
}
