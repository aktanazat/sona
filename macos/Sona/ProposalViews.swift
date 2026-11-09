import SwiftUI

/// The changes agents proposed for the open task, under its title. Y and N
/// answer the oldest; ⇧Y accepts them all.
struct ProposalStrip: View {
    let proposals: [WorkProposal]
    @Environment(AppModel.self) private var model

    var body: some View {
        if !proposals.isEmpty {
            VStack(alignment: .leading, spacing: 14) {
                HStack(spacing: 8) {
                    DetailHeading(title: "Proposed changes")
                    Spacer(minLength: 8)
                    if proposals.count > 1 {
                        TaskKeyButton(title: "Accept all", keys: "shift+y") {
                            model.changeWork { model.work.acceptProposals(proposals.map(\.id)) }
                        }
                    }
                }
                ForEach(Array(proposals.enumerated()), id: \.element.id) { index, proposal in
                    ProposalLine(proposal: proposal, keyed: index == 0)
                }
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
            .padding(.vertical, 12)
        }
    }
}

/// Every task agents proposed changes to, the longest waiting first, above
/// the triage list. The keys answer the first; a task's number and title
/// open it.
struct TriageProposals: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let open = model.work.proposals
        let rows = model.triageProposed
        if !rows.isEmpty {
            ScrollView { list(rows, open) }
                .scrollBounceBehavior(.basedOnSize)
                .frame(maxHeight: 320)
                .fixedSize(horizontal: false, vertical: true)
                .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
        }
    }

    private func list(_ rows: [WorkItemRow], _ open: [String: [WorkProposal]]) -> some View {
        VStack(alignment: .leading, spacing: 18) {
            DetailHeading(title: "Proposed changes")
            ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                ProposalTask(row: row, proposals: open[row.id] ?? [], keyed: index == 0)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ProposalTask: View {
    let row: WorkItemRow
    let proposals: [WorkProposal]
    let keyed: Bool
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Button {
                    model.workFocus.open(row.id, in: .triage)
                } label: {
                    HStack(spacing: 8) {
                        Text(verbatim: "#\(row.number)")
                            .font(TypeScale.mono(12))
                            .foregroundStyle(Theme.inkTertiary)
                        Text(row.title)
                            .font(TypeScale.listTitle)
                            .foregroundStyle(Theme.ink)
                            .lineLimit(1)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Open the task")
                Spacer(minLength: 8)
                if proposals.count > 1 {
                    TaskKeyButton(title: "Accept all", keys: keyed ? "shift+y" : nil) {
                        model.changeWork { model.work.acceptProposals(proposals.map(\.id)) }
                    }
                }
            }
            ForEach(Array(proposals.enumerated()), id: \.element.id) { index, proposal in
                ProposalLine(proposal: proposal, keyed: keyed && index == 0)
            }
        }
    }
}

/// One change an agent proposed: the field, what it would change from and
/// to, and who proposed it, with its answers. `keyed` names Y and N where
/// those keys answer this line.
struct ProposalLine: View {
    let proposal: WorkProposal
    var keyed = false
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: WorkProposalText.icon(proposal.field))
                .font(.system(size: 12))
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: 16, height: 18)
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text(WorkProposalText.title(proposal.field)).metaText()
                    Text(WorkProposalText.change(proposal, model: model, now: TaskDates.now()))
                        .font(TypeScale.listTitle)
                        .foregroundStyle(Theme.ink)
                        .lineLimit(2)
                }
                HStack(spacing: 4) {
                    Image(systemName: "sparkle").font(.system(size: 9, weight: .semibold))
                    Text(proposal.agent)
                }
                .font(TypeScale.label(12))
                .foregroundStyle(Theme.inkSecondary)
            }
            .accessibilityElement(children: .combine)
            Spacer(minLength: 8)
            TaskKeyButton(title: "Accept", keys: keyed ? "y" : nil) {
                model.changeWork { model.work.acceptProposals([proposal.id]) }
            }
            TaskKeyButton(title: "Reject", keys: keyed ? "n" : nil) {
                model.work.rejectProposals([proposal.id])
            }
        }
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
        }
    }

    static func icon(_ field: WorkProposalField) -> String {
        switch field {
        case .title: "textformat"
        case .details: "text.alignleft"
        case .status: "circle.dashed"
        case .priority: "chart.bar"
        case .owner: "person.crop.circle"
        case .due: "calendar"
        case .project: "folder"
        case .milestone: "flag"
        case .labels: "tag"
        case .snoozed: "moon.zzz"
        }
    }

    /// "No due date → Tomorrow"; a title or details read as the new text.
    static func change(_ proposal: WorkProposal, model: AppModel, now: Int64) -> String {
        let to = value(proposal.field, proposal.to, model: model, now: now)
        switch proposal.field {
        case .title, .details:
            return to
        case .status, .priority, .owner, .due, .project, .milestone, .labels, .snoozed:
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
        }
    }
}
