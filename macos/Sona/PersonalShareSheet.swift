import SwiftUI

/// Who a personal document is shared with, and where it goes when it is
/// ready. Agents are its only collaborators: they reach it through Sona's
/// command line and MCP server at the role given here. Moving it files it
/// under a project or a cycle as an ordinary note, words and versions kept.
struct PersonalShareSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let store: ScratchpadStore
    /// Opens the read-only link sheet for the note, the way to give a person
    /// a copy.
    let shareLink: (() -> Void)?

    @State private var agent = ""
    @State private var role: ScratchNoteRole = .edit
    @State private var busy = false
    @State private var error: String?
    @State private var destination: MoveDestination?

    private enum MoveDestination: Hashable {
        case project(String)
        case cycle(Int64)
    }

    private static let knownAgents = ["Claude Code", "Codex", "OMP", "Grok", "Claude"]

    private var projects: [PlanningProject] {
        (model.projects.projects ?? []).map(\.project)
    }

    private var cycles: [WorkCycle] {
        model.work.cycles.filter { $0.state != .past }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .center, spacing: 12) {
                Text(store.saved.map { "Share \u{201C}\($0.title)\u{201D}" } ?? "Share")
                    .titleText()
                    .lineLimit(1)
                    .accessibilityAddTraits(.isHeader)
                Spacer()
                Button { dismiss() } label: { Image(systemName: "xmark") }
                    .buttonStyle(.icon)
                    .keyboardShortcut(.cancelAction)
                    .help("Done")
                    .accessibilityLabel("Done")
            }
            if store.saved?.personal == true {
                invite
                collaborators
                move
            }
            ErrorNote(error)
            if let shareLink {
                HStack {
                    Spacer()
                    Button("Read-only link…") {
                        dismiss()
                        shareLink()
                    }
                    .buttonStyle(.quiet)
                }
            }
        }
        .padding(24)
        .frame(width: 560, alignment: .leading)
        .background(Theme.page)
        .task {
            await store.loadCollaborators()
            if model.projects.projects == nil { await model.projects.refresh() }
            if model.work.cycles.isEmpty { await model.work.refresh() }
        }
    }

    private var suggestedAgents: [String] {
        let typed = agent.trimmingCharacters(in: .whitespaces).lowercased()
        let shared = Set(store.collaborators.map(\.id))
        return Self.knownAgents.filter { name in
            !shared.contains(name.lowercased()) && (typed.isEmpty || name.lowercased().hasPrefix(typed))
        }
    }

    private var invite: some View {
        HStack(spacing: 8) {
            TextField("Agent", text: $agent)
                .textFieldStyle(.roundedBorder)
                .frame(width: 180)
                .textInputSuggestions {
                    ForEach(suggestedAgents, id: \.self) { name in
                        Text(name).textInputCompletion(name)
                    }
                }
                .onSubmit { Task { await add() } }
                .accessibilityLabel("Agent name")
            Spacer()
            Picker("Role", selection: $role) {
                ForEach(ScratchNoteRole.allCases) { Text($0.title).tag($0) }
            }
            .labelsHidden()
            .fixedSize()
            Button("Invite") { Task { await add() } }
                .buttonStyle(.primary)
                .disabled(busy || agent.trimmingCharacters(in: .whitespaces).isEmpty)
        }
    }

    private var collaborators: some View {
        Card {
            if store.collaborators.isEmpty {
                CardRow {
                    Text("No agents").bodyText(14, Theme.inkSecondary)
                }
            }
            ForEach(store.collaborators) { collaborator in
                CardRow {
                    Text(collaborator.agent).bodyText()
                } trailing: {
                    HStack(spacing: 8) {
                        Picker("Role for \(collaborator.agent)", selection: Binding(
                            get: { collaborator.role },
                            set: { next in Task { await change(collaborator.agent, to: next) } }
                        )) {
                            ForEach(ScratchNoteRole.allCases) { Text($0.title).tag($0) }
                        }
                        .labelsHidden()
                        .fixedSize()
                        .disabled(busy)
                        Button("Remove") { Task { await change(collaborator.agent, to: nil) } }
                            .buttonStyle(.compact)
                            .disabled(busy)
                    }
                }
            }
        }
    }

    private var move: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Move to").sectionLabel().accessibilityAddTraits(.isHeader)
            Card {
                CardRow {
                    Picker("Move to", selection: $destination) {
                        Text("Choose a project or cycle").tag(MoveDestination?.none)
                        Section("Projects") {
                            ForEach(projects) { Text($0.title).tag(MoveDestination?.some(.project($0.id))) }
                        }
                        if !cycles.isEmpty {
                            Section("Cycles") {
                                ForEach(cycles) { cycle in
                                    Text(cycle.name ?? "Cycle \(cycle.number)")
                                        .tag(MoveDestination?.some(.cycle(cycle.number)))
                                }
                            }
                        }
                    }
                    .labelsHidden()
                    .fixedSize()
                } trailing: {
                    Button("Move") { Task { await moveToDestination() } }
                        .buttonStyle(.compact)
                        .disabled(busy || destination == nil)
                }
            }
            Text("Moving ends agent sharing").metaText(Theme.inkSecondary)
        }
    }

    private func add() async {
        let name = agent.trimmingCharacters(in: .whitespaces)
        guard !busy, !name.isEmpty else { return }
        await change(name, to: role)
        if error == nil { agent = "" }
    }

    private func change(_ name: String, to next: ScratchNoteRole?) async {
        busy = true
        defer { busy = false }
        do {
            try await store.share(agent: name, role: next)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// Files the document where it was chosen. It is an ordinary note from
    /// then on, so the sheet that shares it closes.
    private func moveToDestination() async {
        guard let destination, let id = store.saved?.id else { return }
        busy = true
        defer { busy = false }
        do {
            switch destination {
            case let .project(projectId):
                try await model.projects.linkSource(
                    targetKind: .project, targetId: projectId, sourceKind: .note, sourceId: id,
                    atMs: nil, operationId: UUID().uuidString.lowercased())
            case let .cycle(number):
                try await store.fileUnderCycle(id, number: number, filed: true)
            }
            await store.refreshOpen()
            error = nil
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
