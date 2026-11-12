import SwiftUI

/// The weekly project digest beside the inbox: each project's status, its
/// latest update, what got done in the week and what puts it at risk.
/// Space or Esc closes it; a project's title opens its page.
struct ProjectDigestPanel: View {
    let digest: WorkProjectDigest
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Text("Weekly project digest").bodyText(13, Theme.inkSecondary)
                Spacer(minLength: 8)
                WorkBarButton(icon: "xmark", help: "Close  Space") { model.workFocus.peeking = false }
            }
            .padding(.leading, 20)
            .padding(.trailing, 12)
            .frame(height: Theme.headerHeight)
            Hairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    Text("\(PlanningFormat.day(digest.fromMs)) – \(PlanningFormat.day(digest.toMs))")
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundStyle(Theme.ink)
                    ForEach(digest.projects) { project in
                        DigestProjectBlock(project: project)
                    }
                }
                .padding(20)
            }
        }
        .frame(width: 480)
        .frame(maxHeight: .infinity)
        .background {
            Rectangle()
                .fill(Theme.elevated)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.border).frame(width: 1) }
                .elevation(.high)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Weekly project digest")
    }
}

private struct DigestProjectBlock: View {
    let project: WorkDigestProject
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Button(project.title) { model.openProject(project.id) }
                    .buttonStyle(.plain)
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Theme.ink)
                    .help("Open project")
                Spacer(minLength: 8)
                Text("\(project.statusName ?? project.status.label) · \(project.percent)%").metaText()
            }
            if let update = project.latestUpdate {
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        Circle().fill(update.health.tint).frame(width: 7, height: 7)
                        Text("\(update.health.label) · \(PlanningFormat.day(update.createdAtMs))").metaText()
                    }
                    if let excerpt = update.excerpt {
                        Text(excerpt)
                            .bodyText(13, Theme.inkSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                    }
                }
            } else {
                Text("No update yet").metaText()
            }
            if project.done.isEmpty {
                Text("Nothing done this week").metaText()
            } else {
                Text(project.done.count == 1 ? "1 task done" : "\(project.done.count) tasks done")
                    .metaText(Theme.inkSecondary)
                ForEach(project.done) { task in
                    DigestTaskLine(task: task)
                }
            }
            ForEach(Array(project.risks.enumerated()), id: \.offset) { _, risk in
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Image(systemName: "exclamationmark.triangle").font(.system(size: 10))
                    Text(Self.line(risk)).fixedSize(horizontal: false, vertical: true)
                }
                .metaText(Theme.statusInProgress)
            }
        }
    }

    private static func line(_ risk: WorkDigestRisk) -> String {
        let day = { (ms: Int64?) in ms.map(PlanningFormat.day) ?? "" }
        switch risk.kind {
        case "health": return "Latest update says \(risk.health?.label.lowercased() ?? "at risk")"
        case "past_target": return "Target \(day(risk.targetAtMs)) has passed"
        case "late_finish": return "At this pace it finishes \(day(risk.expectedAtMs)), after its target"
        case "overdue":
            let count = risk.tasks?.count ?? 0
            let names = (risk.tasks ?? []).prefix(3).map { "#\($0.number) \($0.title)" }.joined(separator: ", ")
            let more = count > 3 ? " and \(count - 3) more" : ""
            return "\(count) overdue: \(names)\(more)"
        case "late_milestone": return "Milestone \(risk.title ?? "") was due \(day(risk.dueAtMs))"
        case "no_update":
            return risk.lastAtMs.map { "No update since \(PlanningFormat.day($0))" } ?? "No update yet"
        default: return "At risk"
        }
    }
}

private struct DigestTaskLine: View {
    let task: WorkDigestTask
    @Environment(AppModel.self) private var model

    var body: some View {
        Button {
            model.workFocus.peeking = false
            model.workFocus.highlighted = task.id
            model.workFocus.open(task.id, in: .inbox)
        } label: {
            HStack(spacing: 6) {
                Text("#\(task.number)").font(TypeScale.mono(12)).foregroundStyle(Theme.inkTertiary)
                Text(task.title).bodyText(13, Theme.ink).lineLimit(1)
            }
        }
        .buttonStyle(.plain)
        .help("Open task")
    }
}

enum DigestDay: String, Codable, CaseIterable, Sendable {
    case monday, tuesday, wednesday, thursday, friday, saturday, sunday

    var label: String { rawValue.capitalized }
}

/// Settings > Tasks: whether the weekly project digest arrives, and on
/// which day.
struct ProjectDigestSettings: View {
    @Environment(AppModel.self) private var model
    @State private var error: String?

    var body: some View {
        PageSection("Weekly digest") {
            ErrorNote(error ?? (model.agents.settingsUnread ? "Couldn't load the digest setting." : nil))
            Card {
                ToggleRow(
                    title: "Weekly project digest",
                    detail: "At 9 in the morning, the inbox gets each project's status, latest update, what got done that week and what's at risk.",
                    isOn: Binding(get: { model.agents.digestEnabled },
                                  set: { on in Task { await save(on, model.agents.digestDay) } }))
                ChoiceRow(
                    title: "Day",
                    choices: DigestDay.allCases,
                    label: \.label,
                    selection: Binding(get: { model.agents.digestDay },
                                       set: { picked in Task { await save(model.agents.digestEnabled, picked) } }))
                    .disabled(!model.agents.digestEnabled)
            }
            .disabled(model.agents.settingsUnread)
        }
    }

    private func save(_ on: Bool, _ picked: DigestDay) async {
        do {
            try await model.agents.setDigest(on, picked)
            error = nil
        } catch {
            self.error = "Couldn't save the digest setting."
        }
    }
}
