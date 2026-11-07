import SwiftUI

/// What may be true of this recording and is not written yet: an event near
/// its start, the place its invite pins, the kind of conversation it heard.
/// Nothing changes until he takes one.
struct ContextSuggestionsView: View {
    let sessionId: MeetingSessionId
    let revision: Int
    @Environment(AppModel.self) private var model
    @State private var suggestions: [FilingSuggestion] = []
    @State private var failure: String?
    @State private var deciding = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(suggestions) { suggestion in
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(suggestion.target.headline).bodyText(13)
                        FilingReasonsLine(sessionId: sessionId, reasons: suggestion.reasons)
                    }
                    Spacer(minLength: 8)
                    Button("Not this") { decide(suggestion, accept: false) }
                        .buttonStyle(.quiet)
                    Button("Use") { decide(suggestion, accept: true) }
                        .buttonStyle(QuietButton(compact: true))
                }
                .disabled(deciding)
            }
            if let failure { Text(failure).metaText(Theme.live) }
        }
        .padding(.bottom, suggestions.isEmpty && failure == nil ? 0 : 16)
        .task(id: "\(sessionId):\(revision)") { await load() }
    }

    private func decide(_ suggestion: FilingSuggestion, accept: Bool) {
        deciding = true
        Task {
            defer { deciding = false }
            do {
                suggestions = try await model.core.request(
                    "context_suggestion_decide",
                    ContextSuggestionDecision(sessionId: sessionId, suggestionId: suggestion.id, accept: accept))
                failure = nil
            } catch {
                failure = error.localizedDescription
            }
        }
    }

    private func load() async {
        do {
            let result: [FilingSuggestion] = try await model.core.request(
                "context_suggestions", ["sessionId": sessionId])
            guard !Task.isCancelled else { return }
            suggestions = result
            failure = nil
        } catch {
            guard !Task.isCancelled else { return }
            failure = "Suggestions could not be read."
        }
    }
}

private struct ContextSuggestionDecision: Encodable {
    let sessionId: MeetingSessionId
    let suggestionId: String
    let accept: Bool
}
