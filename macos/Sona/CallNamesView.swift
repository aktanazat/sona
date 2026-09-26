import SwiftUI

private struct CallNameTarget: Decodable, Identifiable {
    let id: String
    let label: String
    let bundleId: String
    let provider: String
}

private struct CallNameTargets: Decodable {
    let state: String
    let detail: String
    let targets: [CallNameTarget]
}

private struct CallNameSuggestion: Decodable, Identifiable {
    let speakerId: String
    let displayName: String
    let overlapNs: UInt64
    let speechNs: UInt64
    var id: String { speakerId }
}

private struct CallNameStatus: Decodable {
    let enabled: Bool
    let automaticallyUse: Bool
    let state: String
    let detail: String
    let target: CallNameTarget?
    let roster: [String]
    let suggestions: [CallNameSuggestion]
}

private struct CallNamesSetParams: Encodable {
    let sessionId: String
    let targetId: String?
    let automaticallyUse: Bool
}

/// The choice belongs to this recording, not whichever call happens to be in
/// front. Listing calls is itself an explicit action; appearing never reads AX.
struct CallNamesLiveControl: View {
    let sessionId: String
    @Environment(AppModel.self) private var model
    @State private var status: CallNameStatus?
    @State private var choices: CallNameTargets?
    @State private var selected = ""
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        DisclosureGroup("Names from this call") {
            VStack(alignment: .leading, spacing: 10) {
                Text("Read visible participant names from one Zoom, Teams or Google Meet call. Sona does not open panels or change the call.")
                    .bodyText(13, Theme.inkSecondary)
                ErrorNote(error)
                if let status {
                    Text(status.detail).bodyText(13, Theme.inkSecondary)
                    if status.enabled {
                        if let target = status.target { Text(target.label).bodyText(13) }
                        Toggle("Use clear matches automatically", isOn: Binding(
                            get: { status.automaticallyUse },
                            set: { enabled in Task { await configure(status.target?.id, automatic: enabled) } }))
                            .toggleStyle(.checkbox)
                            .disabled(busy)
                        Text("Names you typed and remembered voices always win. Otherwise, choose names on the review page.")
                            .bodyText(12, Theme.inkSecondary)
                        Button("Stop reading names") { Task { await configure(nil, automatic: false) } }
                            .buttonStyle(.quiet).disabled(busy)
                    } else {
                        Button(choices == nil ? "Choose a call to read names…" : "Refresh calls") {
                            Task { await findCalls() }
                        }
                        .buttonStyle(.compact).disabled(busy)
                        if let choices {
                            Text(choices.detail).bodyText(13, Theme.inkSecondary)
                            if !choices.targets.isEmpty {
                                Picker("Recorded call", selection: $selected) {
                                    Text("Choose the call…").tag("")
                                    ForEach(choices.targets) { target in Text(target.label).tag(target.id) }
                                }
                                Button("Read names for this recording") {
                                    Task { await configure(selected, automatic: false) }
                                }
                                .buttonStyle(SecondaryButton(compact: true))
                                .disabled(busy || selected.isEmpty)
                            }
                        }
                    }
                } else if error == nil {
                    Text("Reading the recording's name setting…").bodyText(13, Theme.inkSecondary)
                } else {
                    Button("Try again") { Task { await refresh() } }.buttonStyle(.compact)
                }
            }
            .padding(.top, 8)
        }
        .font(TypeScale.body(13))
        .foregroundStyle(Theme.inkSecondary)
        .task(id: sessionId) {
            status = nil
            choices = nil
            selected = ""
            await refresh()
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(2)) } catch { return }
                guard !busy else { continue }
                await refresh()
            }
        }
    }

    private func refresh() async {
        do {
            let result: CallNameStatus = try await model.core.request("meeting_call_name_status", ["sessionId": sessionId])
            guard !Task.isCancelled else { return }
            status = result
            error = nil
        } catch {
            guard !Task.isCancelled else { return }
            self.error = "Could not read the call-name setting. \(error.localizedDescription)"
        }
    }

    private func findCalls() async {
        busy = true
        defer { busy = false }
        do {
            let result: CallNameTargets = try await model.core.request("meeting_call_name_targets", ["sessionId": sessionId])
            choices = result
            if !result.targets.contains(where: { $0.id == selected }) { selected = "" }
            error = nil
        } catch { self.error = "Could not read calls. \(error.localizedDescription)" }
    }

    private func configure(_ target: String?, automatic: Bool) async {
        busy = true
        defer { busy = false }
        do {
            let result: CallNameStatus = try await model.core.request("meeting_call_names_set",
                CallNamesSetParams(sessionId: sessionId, targetId: target, automaticallyUse: automatic))
            status = result
            choices = nil
            selected = ""
            error = nil
        } catch { self.error = "Could not change call-name reading. \(error.localizedDescription)" }
    }
}

/// Suggestions are distinct from roster choices: a roster entry never claims
/// to know who spoke. Both use the existing, revision-checked rename command.
struct CallNamesReviewSection: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot
    @Environment(AppModel.self) private var model
    @State private var status: CallNameStatus?
    @State private var error: String?
    @State private var busy = false

    var body: some View {
        PageSection("Names from the call") {
            Card {
                if let error {
                    CardRow { Text(error).bodyText(14, Theme.inkSecondary) } trailing: {
                        Button("Try again") { Task { await load() } }.buttonStyle(.compact)
                    }
                } else if let status {
                    CardLine(status.detail)
                    if status.roster.isEmpty {
                        CardLine("No participant names were read during this recording.")
                    } else {
                        ForEach(status.suggestions) { suggestion in
                            CardRow {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text("\(speakerName(suggestion.speakerId)) may be \(suggestion.displayName)").bodyText(14)
                                    Text("The call's speaking indicator matched this voice.").metaText()
                                }
                            } trailing: {
                                HStack(spacing: 12) {
                                    Button("Dismiss") { Task { await dismiss(suggestion.speakerId) } }
                                        .buttonStyle(.quiet)
                                    Button("Use \(suggestion.displayName)") {
                                        store.renameSpeaker(suggestion.speakerId, to: suggestion.displayName)
                                    }
                                    .buttonStyle(.compact)
                                }
                                .disabled(busy || store.busy || !store.editable)
                            }
                        }
                        CardLine("Choose a participant for any speaker below. These names alone do not identify a voice.")
                        ForEach(snapshot.speakers) { speaker in
                            CardRow { Text(speaker.displayName).bodyText(14) } trailing: {
                                Menu("Choose name") {
                                    ForEach(status.roster, id: \.self) { name in
                                        Button(name) { store.renameSpeaker(speaker.speakerId, to: name) }
                                    }
                                }
                                .disabled(busy || store.busy || !store.editable)
                                .accessibilityLabel("Choose a call participant for \(speaker.displayName)")
                            }
                        }
                        if snapshot.speakers.isEmpty { CardLine("Speakers appear when the transcript is ready.") }
                    }
                } else {
                    CardLine("Reading names from the call…")
                }
            }
        }
        .task(id: "\(snapshot.session.sessionId):\(snapshot.session.revision)") {
            status = nil
            await load()
        }
    }

    private func speakerName(_ id: String) -> String {
        snapshot.speakers.first { $0.speakerId == id }?.displayName ?? "Unknown speaker"
    }

    private func load() async {
        do {
            let result: CallNameStatus = try await model.core.request("meeting_call_name_status", ["sessionId": snapshot.session.sessionId])
            guard !Task.isCancelled else { return }
            status = result
            error = nil
        } catch {
            guard !Task.isCancelled else { return }
            self.error = "Could not read call names. \(error.localizedDescription)"
        }
    }

    private func dismiss(_ speakerId: String) async {
        busy = true
        defer { busy = false }
        do {
            status = try await model.core.request("meeting_call_name_dismiss", ["sessionId": snapshot.session.sessionId, "speakerId": speakerId])
            error = nil
        } catch { self.error = "Could not dismiss that suggestion. \(error.localizedDescription)" }
    }
}
