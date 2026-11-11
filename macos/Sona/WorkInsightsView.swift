import Charts
import SwiftUI

struct WorkTiming: Codable, Equatable {
    var count: Int
    var medianMs: Int64?
    var p90Ms: Int64?
}

struct WorkInsightsWeek: Codable, Equatable, Identifiable {
    var startMs: Int64
    var endMs: Int64
    var lead: WorkTiming
    var triage: WorkTiming

    var id: Int64 { endMs }
}

struct WorkInsights: Codable, Equatable {
    var days: Int
    var fromMs: Int64
    var toMs: Int64
    var lead: WorkTiming
    var triage: WorkTiming
    var accepted: Int
    var declined: Int
    var weeks: [WorkInsightsWeek]
}

enum InsightsFormat {
    /// A span as a person reads it: "3d 4h", "5h 12m", "8m".
    static func took(_ ms: Int64) -> String {
        let minutes = ms / 60_000
        let (days, hours, rest) = (minutes / 1440, minutes / 60 % 24, minutes % 60)
        if days > 0 { return "\(days)d \(hours)h" }
        if hours > 0 { return "\(hours)h \(rest)m" }
        return rest > 0 ? "\(rest)m" : "under 1m"
    }
}

/// A project's lead time and triage time over a chosen window, narrowed
/// by label and owner, with the weekly medians drawn as bars.
struct WorkInsightsView: View {
    let projectId: String
    let labels: [WorkLabel]
    let people: [PlanningPersonChoice]
    @Environment(AppModel.self) private var model
    @State private var days = 30
    @State private var labelId: String?
    @State private var owner: Owner?
    @State private var insights: WorkInsights?
    @State private var failure: String?

    enum Owner: Hashable {
        case me
        case person(String)
    }

    private static let windows: [SwitchItem<Int>] = [
        SwitchItem(id: 7, title: "1 week"),
        SwitchItem(id: 30, title: "1 month"),
        SwitchItem(id: 90, title: "3 months"),
        SwitchItem(id: 365, title: "1 year"),
    ]

    private var query: WorkQuery {
        var query = WorkQuery(projectIds: [projectId], includeArchived: true, includeSnoozed: true)
        if let labelId { query.labelIds = [labelId] }
        switch owner {
        case .me: query.direction = .mine
        case .person(let id): query.ownerPersonIds = [id]
        case nil: break
        }
        return query
    }

    private struct Key: Hashable {
        let query: WorkQuery
        let days: Int
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            HStack(spacing: 14) {
                SegmentedSwitch(items: Self.windows, selection: days) { days = $0 }
                    .fixedSize()
                labelMenu
                ownerMenu
                Spacer(minLength: 0)
            }
            if let failure {
                PlannerFailure(message: failure) { Task { await load() } }
            } else if let insights {
                HStack(alignment: .top, spacing: 12) {
                    tile("Lead time", detail: "created to done", timing: insights.lead, counted: "done")
                    tile("Triage time", detail: "until accepted or declined", timing: insights.triage,
                         counted: insights.triage.count > 0
                            ? "decided, \(insights.accepted) accepted, \(insights.declined) declined" : "decided")
                }
                if insights.weeks.count > 1 {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Median by week").sectionLabel().accessibilityAddTraits(.isHeader)
                        Card { WeeklyMedians(weeks: insights.weeks).padding(16) }
                    }
                }
            } else {
                ProgressView().controlSize(.small)
            }
        }
        .task(id: Key(query: query, days: days)) { await load() }
    }

    private func load() async {
        do {
            insights = try await model.work.insights(query, days: days)
            failure = nil
        } catch {
            failure = error.localizedDescription
        }
    }

    private func tile(_ title: String, detail: String, timing: WorkTiming, counted: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(title).sectionLabel()
                Text(detail).metaText()
            }
            HStack(spacing: 10) {
                NumberTile(timing.medianMs.map(InsightsFormat.took) ?? "–", label: "Median")
                NumberTile(timing.p90Ms.map(InsightsFormat.took) ?? "–", label: "90th percentile")
            }
            Text("\(timing.count) \(counted)").metaText(Theme.inkSecondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var labelMenu: some View {
        let name = labels.first { $0.id == labelId }?.name
        return Menu {
            Button { labelId = nil } label: { choice("Any label", on: labelId == nil) }
            Divider()
            ForEach(labels) { label in
                Button { labelId = label.id } label: { choice(label.name, on: label.id == labelId) }
            }
        } label: {
            filterLabel("Label", name ?? "Any")
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("Label: \(name ?? "any")")
    }

    private var ownerMenu: some View {
        let name: String? = switch owner {
        case .me: "Me"
        case .person(let id): people.first { $0.id == id }?.name
        case nil: nil
        }
        return Menu {
            Button { owner = nil } label: { choice("Anyone", on: owner == nil) }
            Button { owner = .me } label: { choice("Me", on: owner == .me) }
            Divider()
            ForEach(people) { person in
                Button { owner = .person(person.id) } label: {
                    choice(person.name, on: owner == .person(person.id))
                }
            }
        } label: {
            filterLabel("Owner", name ?? "Anyone")
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("Owner: \(name ?? "anyone")")
    }

    @ViewBuilder
    private func choice(_ title: String, on: Bool) -> some View {
        if on { Label(title, systemImage: "checkmark") } else { Text(title) }
    }

    private func filterLabel(_ field: String, _ value: String) -> some View {
        Text("\(field): \(value)").font(TypeScale.body(13)).foregroundStyle(Theme.ink)
    }
}

/// Lead and triage medians week by week, in minutes, hours or days.
private struct WeeklyMedians: View {
    let weeks: [WorkInsightsWeek]

    private struct Bar: Identifiable {
        let week: Date
        let measure: String
        let value: Double
        var id: String { "\(week.timeIntervalSince1970)\(measure)" }
    }

    var body: some View {
        let top = weeks.flatMap { [$0.lead.medianMs, $0.triage.medianMs] }.compactMap { $0 }.max() ?? 0
        let (unit, unitName): (Double, String) = switch top {
        case Int64(48 * 3_600_000)...: (86_400_000, "days")
        case Int64(2 * 3_600_000)...: (3_600_000, "hours")
        default: (60_000, "minutes")
        }
        let bars = weeks.flatMap { week -> [Bar] in
            [("Lead time", week.lead), ("Triage time", week.triage)].map { measure, timing in
                Bar(week: PlanningFormat.moment(week.endMs), measure: measure, value: Double(timing.medianMs ?? 0) / unit)
            }
        }
        Chart(bars) { bar in
            BarMark(x: .value("Week ending", bar.week, unit: .weekOfYear), y: .value(unitName, bar.value))
                .position(by: .value("Measure", bar.measure))
                .foregroundStyle(by: .value("Measure", bar.measure))
        }
        .chartForegroundStyleScale(["Lead time": Theme.accent, "Triage time": Theme.inkTertiary])
        .chartYAxisLabel(unitName)
        .frame(height: 160)
        .accessibilityLabel("Median lead time and triage time by week, in \(unitName)")
    }
}
