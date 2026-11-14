import SwiftUI

/// The changes agents proposed for the open task, under its title, with
/// one answer for all of them.
struct ProposalStrip: View {
    let proposals: [WorkProposal]
    @Environment(AppModel.self) private var model

    var body: some View {
        if !proposals.isEmpty {
            let agent = WorkProposalText.agent(of: proposals)
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: Space.s) {
                    DetailHeading(title: "Proposed changes")
                    if let agent { AgentName(name: agent) }
                    Spacer(minLength: Space.s)
                    ProposalAnswers(proposals: proposals, keyed: true)
                }
                .frame(height: Theme.listRow)
                ForEach(proposals) { proposal in
                    ProposalLine(proposal: proposal, showsAgent: agent == nil)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, Space.m)
            .padding(.vertical, Space.s)
            .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
            .padding(.horizontal, -Space.m)
            .padding(.vertical, Space.m)
        }
    }
}

/// Every task agents proposed changes to, the longest waiting first: the
/// first group in Triage. The keys answer the first; a title opens its task.
struct TriageProposals: View {
    static let group = "triage:proposals"
    let rows: [WorkItemRow]
    @Environment(AppModel.self) private var model

    var body: some View {
        let focus = model.workFocus
        let collapsed = focus.collapsed.contains(Self.group)
        Section {
            if !collapsed {
                ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                    ProposalTask(row: row, proposals: model.work.proposals(for: row.id), keyed: index == 0)
                }
            }
        } header: {
            TaskGroupHeader(
                group: TaskGroup(id: Self.group, title: "Proposed changes", mark: .agent, rows: rows),
                collapsed: collapsed, revision: model.people.photoRevision
            ) {
                if collapsed { focus.collapsed.remove(Self.group) } else { focus.collapsed.insert(Self.group) }
            }
        }
    }
}

/// A task with changes waiting, in a triage row's columns: its status and
/// title, which open it like a row, and under the title each change.
private struct ProposalTask: View {
    /// Where a triage row's title starts, past its checkbox and status.
    private static let titleInset: CGFloat = 22 + 10 + 14 + 10
    let row: WorkItemRow
    let proposals: [WorkProposal]
    let keyed: Bool
    @Environment(AppModel.self) private var model
    @State private var hovering = false

    var body: some View {
        let agent = WorkProposalText.agent(of: proposals)
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                Color.clear.frame(width: 22)
                TaskStatusIcon(status: row.status)
                Text(row.title)
                    .font(TypeScale.listTitle)
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                    .accessibilityAddTraits(.isButton)
                    .accessibilityHint("Opens the task")
                    .accessibilityAction { model.workFocus.open(row.id, in: .triage) }
                if let agent { AgentName(name: agent) }
                Spacer(minLength: Space.s)
                ProposalAnswers(proposals: proposals, keyed: keyed)
            }
            .padding(.leading, 6)
            .padding(.trailing, 16)
            .frame(height: Theme.listRow)
            .taskHighlight(hovering, selected: false)
            .contentShape(Rectangle())
            .onHover { hovering = $0 }
            .onTapGesture { model.workFocus.open(row.id, in: .triage) }
            .allowsWindowActivationEvents()
            .help("Open the task")
            ForEach(proposals) { proposal in
                ProposalLine(proposal: proposal, showsAgent: agent == nil)
                    .padding(.leading, 6 + Self.titleInset)
                    .padding(.trailing, 16)
            }
        }
        .padding(.horizontal, 8)
    }
}

/// Accept and Reject for every change listed. Y and N answer a lone
/// change; ⇧Y accepts several at once.
private struct ProposalAnswers: View {
    let proposals: [WorkProposal]
    let keyed: Bool
    @Environment(AppModel.self) private var model

    var body: some View {
        let ids = proposals.map(\.id)
        let one = proposals.count == 1
        HStack(spacing: Space.s) {
            TaskKeyButton(title: "Accept", keys: one ? "y" : "shift+y", keysShown: keyed) {
                model.changeWork { model.work.acceptProposals(ids) }
            }
            TaskKeyButton(title: "Reject", keys: one ? "n" : nil, keysShown: keyed) {
                model.work.rejectProposals(ids)
            }
        }
        .fixedSize()
    }
}

/// One change an agent proposed: the field, and what it would change from
/// and to. `showsAgent` names who proposed it where several agents did.
struct ProposalLine: View {
    let proposal: WorkProposal
    var showsAgent = false
    @Environment(AppModel.self) private var model

    var body: some View {
        let change = WorkProposalText.change(proposal, model: model, now: TaskDates.now())
        HStack(spacing: Space.s) {
            HStack(spacing: Space.s) {
                Text(WorkProposalText.title(proposal.field))
                    .metaText()
                    .frame(width: 80, alignment: .leading)
                Text(change)
                    .font(TypeScale.list)
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                    .help(change)
            }
            .accessibilityElement(children: .combine)
            if showsAgent { AgentName(name: proposal.agent) }
            Spacer(minLength: Space.s)
        }
        .frame(height: Theme.listRow)
    }
}

/// How a proposed change reads, wherever it shows.
@MainActor
enum WorkProposalText {
    static func title(_ field: WorkProposalField) -> String {
        switch field {
        case .title: "Title"
        case .details: "Details"
        case .status: "Status"
        case .priority: "Priority"
        case .owner: "Owner"
        case .due: "Due date"
        case .project: "Project"
        case .milestone: "Milestone"
        case .labels: "Labels"
        case .snoozed: "Snoozed until"
        case .recurrence: "Repeats"
        case .cycle: "Cycle"
        case .parent: "Parent"
        case .estimate: "Estimate"
        case .forPerson: "For"
        case .unknown: "Change"
        }
    }

    /// The one agent behind every change listed, or nil when several are.
    static func agent(of proposals: [WorkProposal]) -> String? {
        guard let first = proposals.first?.agent, proposals.allSatisfy({ $0.agent == first }) else { return nil }
        return first
    }

    /// "No due date → Tomorrow"; a title or details read as the new text.
    static func change(_ proposal: WorkProposal, model: AppModel, now: Int64) -> String {
        let to = value(proposal.field, proposal.to, model: model, now: now)
        switch proposal.field {
        case .title, .details, .unknown:
            return to
        case .status, .priority, .owner, .due, .project, .milestone, .labels, .snoozed, .recurrence, .cycle, .parent,
             .estimate, .forPerson:
            return "\(value(proposal.field, proposal.from, model: model, now: now)) → \(to)"
        }
    }

    /// A value in the text history keeps, in words: a status's label, a
    /// person's name, a day, the first line of the details.
    static func value(_ field: WorkProposalField, _ raw: String?, model: AppModel, now: Int64) -> String {
        let work = model.work
        let text = raw.flatMap { $0.isEmpty ? nil : $0 }
        switch field {
        case .title:
            return text ?? "No title"
        case .details:
            return text.flatMap { $0.split(separator: "\n").first }.map(String.init) ?? "No details"
        case .status:
            return text.flatMap(WorkStatus.init(rawValue:))?.label ?? "No status"
        case .priority:
            return TaskPriority(Int(text ?? "") ?? 0).label
        case .owner:
            switch text {
            case "self"?: return "Me"
            case "nobody"?, nil: return "No owner"
            case let id?: return work.people.first { $0.id == id }?.name ?? "Someone"
            }
        case .due:
            return text.flatMap { Int64($0) }.map { TaskDates.label($0, now: now) } ?? "No due date"
        case .project:
            return text.map { id in work.projects.first { $0.id == id }?.title ?? "A project" } ?? "No project"
        case .milestone:
            return text == nil ? "No milestone" : "A milestone"
        case .labels:
            let names = (text ?? "").split(separator: ",").compactMap { id in
                work.labels.first { $0.id == String(id) }?.name
            }
            return names.isEmpty ? "No labels" : names.joined(separator: ", ")
        case .snoozed:
            return text.flatMap { Int64($0) }.map { TaskDates.label($0, now: now) } ?? "Not snoozed"
        case .recurrence:
            return text.map(Recurrence.display) ?? "Doesn't repeat"
        case .cycle:
            return text.map { "Cycle \($0)" } ?? "No cycle"
        case .parent:
            return text.map { id in work.items?.first { $0.id == id }.map { "#\($0.number) \($0.title)" } ?? "A task" }
                ?? "No parent"
        case .estimate:
            return text.map { "\($0) pt" } ?? "No estimate"
        case .forPerson:
            return text.map { id in work.people.first { $0.id == id }?.name ?? "Someone" } ?? "No one"
        case .unknown:
            return text ?? "A change"
        }
    }
}
