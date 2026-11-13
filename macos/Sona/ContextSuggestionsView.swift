import SwiftUI

/// What may be true of this recording and is not written yet: an event near
/// its start, the place its invite pins, the kind of conversation it heard,
/// a plan it may be. Nothing changes until he takes one, and nothing is
/// offered while the recording runs: Sona asks nothing during a call.
struct ContextSuggestionsView: View {
    let sessionId: MeetingSessionId
    let revision: Int
    /// Shows the running capture's words at an offset, on the live screen.
    var showLive: ((Int64) -> Void)?
    @Environment(AppModel.self) private var model
    @State private var suggestions: [FilingSuggestion] = []
    @State private var failure: String?
    @State private var deciding = false
    @State private var changing: FilingSuggestion?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(suggestions) { suggestion in
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(suggestion.target.headline).bodyText(13)
                        FilingReasonsLine(sessionId: sessionId, reasons: suggestion.reasons, showLive: showLive)
                    }
                    Spacer(minLength: 8)
                    Button("Not this") { decide(suggestion, accept: false) }
                        .buttonStyle(.quiet)
                    if case .meeting = suggestion.target {
                        Button("Change") { changing = suggestion }
                            .buttonStyle(.quiet)
                    }
                    Button("Use") { decide(suggestion, accept: true) }
                        .buttonStyle(QuietButton(compact: true))
                }
                .disabled(deciding)
            }
            if let failure { Text(failure).metaText(Theme.live) }
        }
        .padding(.bottom, suggestions.isEmpty && failure == nil ? 0 : 16)
        .task(id: model.live.recording(sessionId) ? "\(sessionId):recording" : "\(sessionId):\(revision)") {
            await load()
        }
        .sheet(item: $changing) { suggestion in
            if case let .meeting(title, people, _, location) = suggestion.target {
                MeetingMaybeSheet(title: title, people: people, location: location) { change in
                    changing = nil
                    decide(suggestion, accept: true, change: change)
                }
            }
        }
    }

    private func decide(_ suggestion: FilingSuggestion, accept: Bool, change: MeetingMaybeChange? = nil) {
        deciding = true
        Task {
            defer { deciding = false }
            do {
                suggestions = try await model.core.request(
                    "context_suggestion_decide",
                    ContextSuggestionDecision(
                        sessionId: sessionId, suggestionId: suggestion.id, accept: accept, change: change))
                failure = nil
                if accept, case .meeting = suggestion.target { model.live.recallNow(sessionId) }
            } catch {
                failure = error.localizedDescription
            }
        }
    }

    private func load() async {
        guard !model.live.recording(sessionId) else {
            suggestions = []
            failure = nil
            return
        }
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

extension MeetingLiveStore {
    /// True while `sessionId` records, on the live screen or the panel.
    func recording(_ sessionId: MeetingSessionId) -> Bool {
        [live?.session, active?.snapshot].contains { $0?.sessionId == sessionId && $0?.phase.isActive == true }
    }
}

private struct ContextSuggestionDecision: Encodable {
    let sessionId: MeetingSessionId
    let suggestionId: String
    let accept: Bool
    let change: MeetingMaybeChange?
}

private struct MeetingMaybeChange: Encodable {
    let title: String
    let people: [String]
    let place: Bool
}

/// A maybe meeting as he would keep it: its title, which of its people were
/// there, and whether its place is saved.
private struct MeetingMaybeSheet: View {
    let people: [FilingPerson]
    let location: ConversationPlace?
    let keep: (MeetingMaybeChange) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var title: String
    @State private var chosen: Set<String>
    @State private var savesPlace: Bool

    init(title: String, people: [FilingPerson], location: ConversationPlace?,
         keep: @escaping (MeetingMaybeChange) -> Void) {
        self.people = people
        self.location = location
        self.keep = keep
        _title = State(initialValue: title)
        _chosen = State(initialValue: Set(people.map(\.personId)))
        _savesPlace = State(initialValue: location != nil)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("What this recording is").headlineText()
            TextField("Title", text: $title).textFieldStyle(.roundedBorder)
            ForEach(people) { person in
                Toggle(person.name, isOn: Binding(
                    get: { chosen.contains(person.personId) },
                    set: { if $0 { chosen.insert(person.personId) } else { chosen.remove(person.personId) } }))
            }
            if let location {
                Toggle("At \(location.name)", isOn: $savesPlace)
            }
            HStack {
                Button("Cancel") { dismiss() }
                    .buttonStyle(.quiet)
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Use") {
                    keep(MeetingMaybeChange(
                        title: title, people: people.map(\.personId).filter(chosen.contains), place: savesPlace))
                }
                .buttonStyle(.primary)
                .keyboardShortcut(.defaultAction)
                .disabled(title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(28).frame(width: 480).background(Theme.page)
    }
}
