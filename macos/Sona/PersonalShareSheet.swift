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
    @State private var projectId: String?
    @State private var cycleNumber: Int64?

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
                Text("Share").titleText().accessibilityAddTraits(.isHeader)
                Spacer()
                Button { dismiss() } label: { Image(systemName: "xmark") }
                    .buttonStyle(.icon)
                    .keyboardShortcut(.cancelAction)
                    .help("Done")
                    .accessibilityLabel("Done")
            }
            if let note = store.saved {
                VStack(alignment: .leading, spacing: 6) {
                    Text(note.title).headlineText()
                    Text(note.personal
                        ? "Private. Only you and the agents below can reach it."
                        : "Moved. It is an ordinary note now, filed where you put it.")
                        .metaText(Theme.inkSecondary)
                }
                if note.personal {
                    invite
                    collaborators
                    move
                }
            }
            ErrorNote(error)
            if let shareLink {
                HStack {
                    Text("A person gets a read-only copy through a link.").metaText(Theme.inkSecondary)
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

    private var invite: some View {
        Card {
            CardRow {
                HStack(spacing: 8) {
                    TextField("Agent", text: $agent)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 180)
                        .onSubmit { Task { await add() } }
                        .accessibilityLabel("Agent name")
                    Menu {
                        ForEach(Self.knownAgents, id: \.self) { name in
                            Button(name) { agent = name }
                        }
                    } label: {
                        Image(systemName: "chevron.down")
                    }
                    .menuStyle(.borderlessButton)
                    .fixedSize()
                    .help("Agents Sona knows")
                    .accessibilityLabel("Choose an agent")
                }
            } trailing: {
                HStack(spacing: 8) {
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
        }
    }

    @ViewBuilder private var collaborators: some View {
        if store.collaborators.isEmpty {
            Text("Not shared with any agent.").metaText()
        } else {
            Card {
                ForEach(store.collaborators) { collaborator in
                    CardRow {
                        Text(collaborator.agent).bodyText()
                    } trailing: {
                        HStack(spacing: 12) {
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
    }

    private var move: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Move when it is ready").sectionLabel().accessibilityAddTraits(.isHeader)
            Card {
                CardRow {
                    Picker("Project", selection: $projectId) {
                        Text("Choose a project").tag(String?.none)
                        ForEach(projects) { Text($0.title).tag(String?.some($0.id)) }
                    }
                    .frame(maxWidth: 320)
                } trailing: {
                    Button("Move") { Task { await moveToProject() } }
                        .buttonStyle(.compact)
                        .disabled(busy || projectId == nil)
                }
                if !cycles.isEmpty {
                    CardRow {
                        Picker("Cycle", selection: $cycleNumber) {
                            Text("Choose a cycle").tag(Int64?.none)
                            ForEach(cycles) { cycle in
                                Text(cycle.name ?? "Cycle \(cycle.number)").tag(Int64?.some(cycle.number))
                            }
                        }
                        .frame(maxWidth: 320)
                    } trailing: {
                        Button("Move") { Task { await moveToCycle() } }
                            .buttonStyle(.compact)
                            .disabled(busy || cycleNumber == nil)
                    }
                }
            }
            Text("Moving files it as a document there. Its words and history stay, and its sharing ends.")
                .metaText(Theme.inkSecondary)
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

    private func moveToProject() async {
        guard let projectId, let id = store.saved?.id else { return }
        busy = true
        defer { busy = false }
        do {
            try await model.projects.linkSource(
                targetKind: .project, targetId: projectId, sourceKind: .note, sourceId: id,
                atMs: nil, operationId: UUID().uuidString.lowercased())
            await store.refreshOpen()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func moveToCycle() async {
        guard let cycleNumber, let id = store.saved?.id else { return }
        busy = true
        defer { busy = false }
        do {
            try await store.fileUnderCycle(id, number: cycleNumber, filed: true)
            await store.refreshOpen()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }
}
