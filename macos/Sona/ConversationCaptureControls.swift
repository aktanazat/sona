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

/// Three bookmarks of a kind, each with how many this recording holds once
/// it holds one. The count rolls up when one lands.
struct ConversationTypedBookmarks: View {
    let isRecording: Bool
    let counts: [ConversationBookmarkKind: Int]?
    let action: (ConversationBookmarkKind) -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        HStack(spacing: 4) {
            ForEach(Array(ConversationBookmarkKind.typed.enumerated()), id: \.element) { index, kind in
                let key = Character("\(index + 1)")
                Button { action(kind) } label: {
                    HStack(spacing: 4) {
                        Image(systemName: kind.symbol).foregroundStyle(kind.color)
                        if let count = counts?[kind], count > 0 {
                            Text("\(count)")
                                .monospacedDigit()
                                .contentTransition(.numericText(value: Double(count)))
                        }
                    }
                    .animation(reduceMotion ? nil : .spring(duration: 0.3, bounce: 0.3),
                               value: counts?[kind, default: 0])
                }
                .buttonStyle(.quiet)
                .disabled(!isRecording)
                .keyboardShortcut(KeyEquivalent(key), modifiers: [.control, .option])
                .accessibilityLabel("Mark a \(kind.label.lowercased())")
                .accessibilityValue(counts.map { "\($0[kind, default: 0])" } ?? "")
                .help("Mark a \(kind.label.lowercased()) (⌃⌥\(key))")
            }
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
    let kind: ConversationKind
    let location: CaptureIntentLocation?
}

private struct CaptureIntentUpdate: Encodable {
    let sessionId: String
    let expectedRevision: UInt64
    let kind: ConversationKind
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
                    ForEach(ConversationKind.allCases.filter { $0 != .imported && $0 != .unspecified }) { kind in
                        Button(kind.label) { Task { await save(kind, context: context) } }
                    }
                } label: {
                    Text(saving ? "Saving…" : context.kind == .unspecified ? "What kind of conversation?" : context.kind.label)
                }
                .disabled(saving)
                .accessibilityLabel("Kind of conversation")
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
            failure = "The kind of conversation could not be read."
        }
    }

    private func save(_ kind: ConversationKind, context: CaptureIntentContext) async {
        guard !saving else { return }
        saving = true
        failure = nil
        defer { saving = false }
        do {
            let request = CaptureIntentUpdate(sessionId: sessionId, expectedRevision: context.revision,
                kind: kind, location: context.location)
            let result: CaptureIntentContext = try await core.request("conversation_context_update", ["request": try JSONValue(request)])
            guard !Task.isCancelled, result.sessionId == sessionId else { return }
            self.context = result
            onSaved()
        } catch {
            failure = "The kind of conversation could not be saved."
        }
    }
}
