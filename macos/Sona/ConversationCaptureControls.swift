import Foundation
import SwiftUI

struct ConversationBookmarkButton: View {
    let isRecording: Bool
    let saved: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label(saved ? "Bookmarked" : "Bookmark", systemImage: saved ? "bookmark.fill" : "bookmark")
        }
        .buttonStyle(.quiet)
        .keyboardShortcut("b", modifiers: [.command, .shift])
        .disabled(!isRecording)
        .accessibilityLabel("Bookmark this moment")
        .help("Bookmark this moment (⇧⌘B)")
    }
}

private enum ConversationCaptureIntent: String, Codable, CaseIterable, Identifiable {
    case unspecified, huddle
    case onlineCall = "online_call"
    case inPerson = "in_person"
    case noteSession = "note_session"
    case imported
    var id: Self { self }
    var label: String {
        switch self {
        case .unspecified: "Choose intent"
        case .noteSession: "Thought"
        case .inPerson: "In person"
        case .huddle: "Huddle"
        case .onlineCall: "Call"
        case .imported: "Imported recording"
        }
    }
}

private struct CaptureIntentLocation: Codable {
    let name: String
    let latitude: Double
    let longitude: Double
}

private struct CaptureIntentContext: Decodable {
    let sessionId: String
    let revision: UInt64
    let kind: ConversationCaptureIntent
    let location: CaptureIntentLocation?
}

private struct CaptureIntentUpdate: Encodable {
    let sessionId: String
    let expectedRevision: UInt64
    let kind: ConversationCaptureIntent
    let location: CaptureIntentLocation?
    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case expectedRevision = "expected_revision"
        case kind, location
    }
}

struct ConversationCaptureIntentPicker: View {
    let core: Core
    let sessionId: String
    var onSaved: () -> Void = {}
    @State private var context: CaptureIntentContext?
    @State private var saving = false
    @State private var failure: String?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            if let context {
                Menu {
                    ForEach(ConversationCaptureIntent.allCases.filter { $0 != .imported && $0 != .unspecified }) { intent in
                        Button(intent.label) { Task { await save(intent, context: context) } }
                    }
                } label: {
                    Text(saving ? "Saving…" : context.kind.label)
                }
                .disabled(saving)
                .accessibilityLabel("Capture intent")
                .fixedSize()
            }
            if let failure {
                Text(failure).metaText(Theme.live)
                if context == nil {
                    Button("Retry") { Task { await load() } }.buttonStyle(.quiet)
                }
            }
        }
        .task(id: sessionId) {
            context = nil
            await load()
        }
    }

    private func load() async {
        do {
            let result: CaptureIntentContext = try await core.request("conversation_context", ["sessionId": sessionId])
            guard !Task.isCancelled, result.sessionId == sessionId else { return }
            context = result
            failure = nil
        } catch {
            guard !Task.isCancelled else { return }
            failure = "Capture intent could not be read."
        }
    }

    private func save(_ intent: ConversationCaptureIntent, context: CaptureIntentContext) async {
        guard !saving else { return }
        saving = true
        failure = nil
        defer { saving = false }
        do {
            let request = CaptureIntentUpdate(sessionId: sessionId, expectedRevision: context.revision,
                kind: intent, location: context.location)
            let result: CaptureIntentContext = try await core.request("conversation_context_update", ["request": try JSONValue(request)])
            guard !Task.isCancelled, result.sessionId == sessionId else { return }
            self.context = result
            onSaved()
        } catch {
            failure = "Capture intent could not be saved."
        }
    }
}
