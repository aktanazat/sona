import Charts
import SwiftUI

enum ProjectPageTab: String, CaseIterable, Hashable {
    case overview, tasks, updates

    var title: String {
        switch self {
        case .overview: "Overview"
        case .tasks: "Tasks"
        case .updates: "Updates"
        }
    }

    static var items: [SwitchItem<ProjectPageTab>] { allCases.map { SwitchItem(id: $0, title: $0.title) } }
}

extension PlanningProjectHealth {
    var tint: Color {
        switch self {
        case .onTrack: Theme.statusDone
        case .atRisk: Theme.statusInProgress
        case .offTrack: Theme.live
        }
    }
}

/// A project's health as its latest update gave it, or that no update was
/// ever posted.
struct ProjectHealthPill: View {
    let health: PlanningProjectHealth?
    var compact = false

    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(health?.tint ?? Theme.inkDisabled).frame(width: 7, height: 7)
            if !compact || health != nil {
                Text(health?.label ?? "No update")
                    .font(TypeScale.label(compact ? 11.5 : 12))
                    .foregroundStyle(health == nil ? Theme.inkTertiary : Theme.ink)
                    .lineLimit(1)
            }
        }
        .padding(.horizontal, compact ? 0 : 8)
        .frame(height: compact ? 18 : 22)
        .background(compact ? Color.clear : Theme.control, in: Capsule())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Health: \(health?.label ?? "no update yet")")
    }
}

enum ProjectDates {
    /// A date read at its resolution: "Mar 31, 2027", "March 2027",
    /// "Q1 2027", "H1 2027" or "2027".
    static func text(_ ms: Int64, _ resolution: PlanningDateResolution?) -> String {
        let date = PlanningFormat.moment(ms)
        let calendar = Calendar.current
        let year = calendar.component(.year, from: date)
        let month = calendar.component(.month, from: date)
        switch resolution ?? .day {
        case .day: return date.formatted(.dateTime.month(.abbreviated).day().year())
        case .month: return date.formatted(.dateTime.month(.wide).year())
        case .quarter: return "Q\((month - 1) / 3 + 1) \(year)"
        case .halfYear: return "H\(month <= 6 ? 1 : 2) \(year)"
        case .year: return "\(year)"
        }
    }

    static func relative(_ ms: Int64, now: Int64) -> String {
        if now - ms < 60_000 { return "Just now" }
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        return formatter.localizedString(for: PlanningFormat.moment(ms), relativeTo: PlanningFormat.moment(now))
    }
}

/// The description, read as Markdown; Edit turns it into a text field.
struct ProjectDescriptionEditor: View {
    let text: String?
    let busy: Bool
    let save: (String) -> Void
    @State private var draft: String?

    var body: some View {
        if let editing = draft {
            VStack(alignment: .leading, spacing: 8) {
                TextEditor(text: Binding(get: { editing }, set: { draft = $0 }))
                    .font(TypeScale.body(13.5))
                    .scrollContentBackground(.hidden)
                    .padding(8)
                    .frame(minHeight: 110)
                    .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                    .accessibilityLabel("Description")
                HStack(spacing: 8) {
                    Text("Markdown").metaText()
                    Spacer()
                    Button("Cancel") { draft = nil }
                        .buttonStyle(.secondary)
                        .keyboardShortcut(.cancelAction)
                    Button("Save") {
                        save(editing.trimmingCharacters(in: .whitespacesAndNewlines))
                        draft = nil
                    }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.return, modifiers: .command)
                    .disabled(busy)
                }
            }
        } else {
            Button { draft = text ?? "" } label: {
                Group {
                    if let text, !text.isEmpty {
                        Text(Self.markdown(text)).bodyText(14, Theme.inkSecondary)
                    } else {
                        Text("Add a description").bodyText(14, Theme.inkTertiary)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Edit description")
            .accessibilityLabel(text?.isEmpty == false ? "Description: \(text ?? ""). Edit" : "Add a description")
        }
    }

    static func markdown(_ text: String) -> AttributedString {
        (try? AttributedString(markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(text)
    }
}

/// The project's fields down the side of its overview, each changed in place.
struct ProjectPropertiesPanel: View {
    let detail: PlanningProjectDetail
    let people: [PlanningPersonChoice]
    let labels: [WorkLabel]
    let save: (PlanningProjectSave) -> Void
    @State private var addingLink = false
    @State private var linkTitle = ""
    @State private var pickingHue = false
    @State private var linkURL = ""

    private static let hues = [210, 260, 300, 340, 20, 40, 130, 170]

    var body: some View {
        let project = detail.project
        VStack(alignment: .leading, spacing: 0) {
            row("Health") { ProjectHealthPill(health: detail.latestUpdate?.health) }
            row("Status") { statusMenu(project) }
            row("Priority") { priorityMenu(project) }
            row("Lead") { leadMenu(project) }
            row("Members") { membersMenu(project) }
            row("Start") { dateField(project.startAtMs, project.startResolution, start: true) }
            row("Target") { dateField(project.targetAtMs, project.targetResolution, start: false) }
            row("Labels") { labelsMenu(project) }
            row("Look") { lookMenu(project) }
            links(project)
        }
        .padding(.vertical, 6)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
    }

    private func row<Value: View>(_ title: String, @ViewBuilder value: () -> Value) -> some View {
        HStack(spacing: 8) {
            Text(title).metaText().frame(width: 64, alignment: .leading)
            value()
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .frame(minHeight: 30)
    }

    private func menuLabel(_ text: String, muted: Bool = false) -> some View {
        Text(text)
            .font(TypeScale.label(12.5))
            .foregroundStyle(muted ? Theme.inkTertiary : Theme.ink)
            .lineLimit(1)
    }

    private func statusMenu(_ project: PlanningProject) -> some View {
        let progress = Double(detail.progress?.percent ?? 0) / 100
        return ChoicePicker(PlanningProjectStatus.allCases, current: project.status, title: \.label,
                            accessibility: "Status: \(project.status.label)",
                            pick: { save(PlanningProjectSave(status: $0)) },
                            icon: { ProjectStatusIcon(status: $0, progress: progress) }) {
            HStack(spacing: 6) {
                ProjectStatusIcon(status: project.status, progress: progress)
                menuLabel(project.status.label)
            }
        }
    }

    private func priorityMenu(_ project: PlanningProject) -> some View {
        let current = TaskPriority(project.priority ?? 0)
        return ChoicePicker(TaskPriority.menuOrder, current: current, title: \.label,
                            accessibility: "Priority: \(current.label)",
                            pick: { save(PlanningProjectSave(priority: $0.rawValue)) },
                            icon: { PriorityIcon(priority: $0) }) {
            HStack(spacing: 6) {
                PriorityIcon(priority: current)
                menuLabel(current.label, muted: current == .none)
            }
        }
    }

    private func name(_ id: String) -> String { people.first { $0.id == id }?.name ?? "Someone" }

    private func leadMenu(_ project: PlanningProject) -> some View {
        Menu {
            Button("No lead") { save(PlanningProjectSave(leadPersonId: .clear)) }
                .disabled(project.leadPersonId == nil)
            Divider()
            ForEach(people) { person in
                Button(person.name) { save(PlanningProjectSave(leadPersonId: .set(person.id))) }
                    .disabled(project.leadPersonId == person.id)
            }
        } label: {
            menuLabel(project.leadPersonId.map(name) ?? "No lead", muted: project.leadPersonId == nil)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("Lead")
    }

    private func membersMenu(_ project: PlanningProject) -> some View {
        let members = project.memberPersonIds ?? []
        return Menu {
            ForEach(people) { person in
                let on = members.contains(person.id)
                Button {
                    save(PlanningProjectSave(memberPersonIds: on ? members.filter { $0 != person.id } : members + [person.id]))
                } label: {
                    if on { Label(person.name, systemImage: "checkmark") } else { Text(person.name) }
                }
            }
        } label: {
            menuLabel(members.isEmpty ? "None" : members.map(name).joined(separator: ", "), muted: members.isEmpty)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .accessibilityLabel("Members")
    }

    private func dateField(_ ms: Int64?, _ resolution: PlanningDateResolution?, start: Bool) -> some View {
        let label = start ? "start" : "target"
        return Menu {
            Section("Date") {
                ForEach(Self.dateChoices(), id: \.0) { title, date in
                    Button(title) {
                        let value = ProjectsStore.milliseconds(date)
                        save(start ? PlanningProjectSave(startAtMs: .set(value)) : PlanningProjectSave(targetAtMs: .set(value)))
                    }
                }
            }
            if ms != nil {
                Section("Read as") {
                    ForEach(PlanningDateResolution.allCases) { choice in
                        Button {
                            save(start ? PlanningProjectSave(startResolution: .set(choice))
                                 : PlanningProjectSave(targetResolution: .set(choice)))
                        } label: {
                            if choice == (resolution ?? .day) { Label(choice.label, systemImage: "checkmark") } else { Text(choice.label) }
                        }
                    }
                }
                Button("Clear \(label) date") {
                    save(start ? PlanningProjectSave(startAtMs: .clear, startResolution: .clear)
                         : PlanningProjectSave(targetAtMs: .clear, targetResolution: .clear))
                }
            }
        } label: {
            menuLabel(ms.map { ProjectDates.text($0, resolution) } ?? "None", muted: ms == nil)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel(start ? "Start date" : "Target date")
    }

    /// Today, the ends of this month, quarter, half-year and year, and next
    /// year's end: the dates a plan is usually pinned to.
    private static func dateChoices() -> [(String, Date)] {
        let calendar = Calendar.current
        let today = calendar.startOfDay(for: .now)
        let month = calendar.component(.month, from: today)
        let year = calendar.component(.year, from: today)
        func lastDay(month endMonth: Int, year endYear: Int) -> Date {
            let first = calendar.date(from: DateComponents(year: endYear, month: endMonth + 1, day: 1)) ?? today
            return calendar.date(byAdding: .day, value: -1, to: first) ?? today
        }
        return [
            ("Today", today),
            ("In two weeks", calendar.date(byAdding: .day, value: 14, to: today) ?? today),
            ("End of this month", lastDay(month: month, year: year)),
            ("End of this quarter", lastDay(month: ((month - 1) / 3 + 1) * 3, year: year)),
            ("End of next quarter", lastDay(month: ((month - 1) / 3 + 2) * 3, year: year)),
            ("End of this half", lastDay(month: month <= 6 ? 6 : 12, year: year)),
            ("End of this year", lastDay(month: 12, year: year)),
            ("End of next year", lastDay(month: 12, year: year + 1)),
        ]
    }

    private func labelsMenu(_ project: PlanningProject) -> some View {
        let chosen = project.labelIds ?? []
        let names = labels.filter { chosen.contains($0.id) }
        return HStack(spacing: 5) {
            ForEach(names) { label in
                Circle().fill(Theme.label(hue: label.hue)).frame(width: 7, height: 7)
            }
            Menu {
                if labels.isEmpty { Text("No labels yet") }
                ForEach(labels) { label in
                    let on = chosen.contains(label.id)
                    Button {
                        save(PlanningProjectSave(labelIds: on ? chosen.filter { $0 != label.id } : chosen + [label.id]))
                    } label: {
                        if on { Label(label.name, systemImage: "checkmark") } else { Text(label.name) }
                    }
                }
            } label: {
                menuLabel(names.isEmpty ? "None" : names.map(\.name).joined(separator: ", "), muted: names.isEmpty)
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .accessibilityLabel("Labels")
        }
    }

    private func lookMenu(_ project: PlanningProject) -> some View {
        HStack(spacing: 8) {
            Button { pickingHue = true } label: {
                Circle()
                    .fill(Theme.label(hue: Double(project.hue ?? 210)))
                    .frame(width: 14, height: 14)
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .help("Color")
            .accessibilityLabel("Color")
            .popover(isPresented: $pickingHue, arrowEdge: .bottom) {
                HStack(spacing: 8) {
                    ForEach(Self.hues, id: \.self) { hue in
                        Button {
                            pickingHue = false
                            if project.hue != hue { save(PlanningProjectSave(hue: hue)) }
                        } label: {
                            Circle()
                                .fill(Theme.label(hue: Double(hue)))
                                .frame(width: 18, height: 18)
                                .overlay(Circle().strokeBorder(Theme.ink, lineWidth: project.hue == hue ? 2 : 0))
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Hue \(hue) degrees")
                    }
                }
                .padding(10)
            }
            TextField("Icon", text: Binding(
                get: { project.icon ?? "" },
                set: { value in
                    let icon = String(value.trimmingCharacters(in: .whitespaces).prefix(2))
                    guard icon != (project.icon ?? "") else { return }
                    save(PlanningProjectSave(icon: icon.isEmpty ? .clear : .set(icon)))
                }))
                .textFieldStyle(.plain)
                .font(TypeScale.label(12.5))
                .frame(width: 44)
                .help("An emoji for the project")
                .accessibilityLabel("Icon")
        }
    }

    @ViewBuilder
    private func links(_ project: PlanningProject) -> some View {
        let links = project.links ?? []
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text("Links").metaText()
                Spacer()
                Button { addingLink = true } label: { Image(systemName: "plus") }
                    .buttonStyle(.icon)
                    .help("Add link")
                    .accessibilityLabel("Add link")
                    .popover(isPresented: $addingLink, arrowEdge: .bottom) { linkForm(links) }
            }
            ForEach(links) { link in
                HStack(spacing: 6) {
                    Image(systemName: "link").font(.system(size: 10)).foregroundStyle(Theme.inkTertiary)
                    if let url = URL(string: link.url) {
                        Link(link.title ?? link.url, destination: url)
                            .font(TypeScale.label(12.5))
                            .lineLimit(1)
                    } else {
                        Text(link.title ?? link.url).font(TypeScale.label(12.5)).lineLimit(1)
                    }
                    Spacer(minLength: 0)
                    Button {
                        save(PlanningProjectSave(links: links.filter { $0.id != link.id }
                            .map { PlanningProjectLinkInput(title: $0.title, url: $0.url) }))
                    } label: { Image(systemName: "xmark").font(.system(size: 9, weight: .bold)) }
                        .buttonStyle(.plain)
                        .foregroundStyle(Theme.inkTertiary)
                        .help("Remove link")
                        .accessibilityLabel("Remove link \(link.title ?? link.url)")
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 6)
    }

    private func linkForm(_ links: [PlanningProjectLink]) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("Title", text: $linkTitle).textFieldStyle(.roundedBorder)
            TextField("https://", text: $linkURL).textFieldStyle(.roundedBorder)
            HStack {
                Spacer()
                Button("Add") {
                    let url = linkURL.trimmingCharacters(in: .whitespaces)
                    let title = linkTitle.trimmingCharacters(in: .whitespaces)
                    save(PlanningProjectSave(links: links.map { PlanningProjectLinkInput(title: $0.title, url: $0.url) }
                        + [PlanningProjectLinkInput(title: title.isEmpty ? nil : title, url: url)]))
                    linkTitle = ""
                    linkURL = ""
                    addingLink = false
                }
                .buttonStyle(.primary)
                .keyboardShortcut(.defaultAction)
                .disabled(URL(string: linkURL.trimmingCharacters(in: .whitespaces))?.scheme == nil)
            }
        }
        .padding(12)
        .frame(width: 260)
    }
}

/// Weekly scope, started and done since the project began, with the
/// predicted finish and its range when there is pace to go on.
struct ProjectProgressChart: View {
    let progress: PlanningProjectProgress
    var height: CGFloat = 132

    var body: some View {
        let weeks = progress.weeks
        let peak = max(weeks.map(\.scope).max() ?? 0, 1)
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 14) {
                legend("Scope", Theme.inkTertiary, dashed: true)
                legend("Started", Theme.statusInProgress)
                legend("Done", Theme.accent)
                Spacer()
                Text("\(progress.percent)%")
                    .font(TypeScale.label(13).weight(.semibold))
                    .monospacedDigit()
            }
            Chart {
                if let prediction = progress.prediction {
                    RectangleMark(xStart: .value("Early", PlanningFormat.moment(prediction.optimisticAtMs)),
                                  xEnd: .value("Late", PlanningFormat.moment(prediction.pessimisticAtMs)))
                        .foregroundStyle(Theme.accentSoft.opacity(0.6))
                    RuleMark(x: .value("Predicted", PlanningFormat.moment(prediction.expectedAtMs)))
                        .foregroundStyle(Theme.accent)
                        .lineStyle(StrokeStyle(lineWidth: 1.5, dash: [4, 3]))
                }
                ForEach(weeks, id: \.weekStartMs) { week in
                    let at = PlanningFormat.moment(week.weekStartMs)
                    AreaMark(x: .value("Week", at), y: .value("Done", week.done))
                        .foregroundStyle(Theme.accentSoft)
                    LineMark(x: .value("Week", at), y: .value("Count", week.done), series: .value("Line", "Done"))
                        .foregroundStyle(Theme.accent)
                        .lineStyle(StrokeStyle(lineWidth: 2))
                    LineMark(x: .value("Week", at), y: .value("Count", week.done + week.started),
                             series: .value("Line", "Started"))
                        .foregroundStyle(Theme.statusInProgress)
                        .lineStyle(StrokeStyle(lineWidth: 1.5))
                    LineMark(x: .value("Week", at), y: .value("Count", week.scope), series: .value("Line", "Scope"))
                        .foregroundStyle(Theme.inkTertiary)
                        .lineStyle(StrokeStyle(lineWidth: 1.5, dash: [3, 3]))
                }
            }
            .chartYScale(domain: 0...Double(peak))
            .chartLegend(.hidden)
            .chartXAxis {
                AxisMarks(values: .automatic(desiredCount: 4)) { _ in
                    AxisValueLabel(format: .dateTime.month(.abbreviated).day())
                }
            }
            .chartYAxis {
                AxisMarks(position: .leading, values: .automatic(desiredCount: 3)) { _ in
                    AxisGridLine().foregroundStyle(Theme.hairline)
                    AxisValueLabel()
                }
            }
            .frame(height: height)
            Text(prediction).metaText()
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Progress \(progress.percent) percent. \(progress.done) done, \(progress.started) started of \(progress.scope). \(prediction)")
    }

    private var prediction: String {
        guard let prediction = progress.prediction else {
            return progress.scope == 0 ? "No tasks yet." : "Not enough finished work to predict a finish."
        }
        let day = { (ms: Int64) in PlanningFormat.moment(ms).formatted(.dateTime.month(.abbreviated).day()) }
        return "Predicted finish \(day(prediction.expectedAtMs)), between \(day(prediction.optimisticAtMs)) and \(day(prediction.pessimisticAtMs))."
    }

    private func legend(_ title: String, _ color: Color, dashed: Bool = false) -> some View {
        HStack(spacing: 5) {
            Capsule()
                .stroke(color, style: StrokeStyle(lineWidth: 2, dash: dashed ? [3, 2] : []))
                .frame(width: 12, height: 2)
            Text(title).metaText()
        }
    }
}

/// The project's updates, newest first, under a composer that can start
/// from what moved since the last one.
struct ProjectUpdatesView: View {
    let store: ProjectsStore
    let detail: PlanningProjectDetail
    @State private var health: PlanningProjectHealth = .onTrack
    @State private var draft = ""
    @State private var drafting = false
    @State private var error: String?

    var body: some View {
        let projectId = detail.project.id
        let now = ProjectsStore.milliseconds(.now)
        VStack(alignment: .leading, spacing: 16) {
            if detail.updateMissing {
                HStack(spacing: 10) {
                    Image(systemName: "clock.badge.exclamationmark").foregroundStyle(Theme.statusInProgress)
                    Text(detail.latestUpdate == nil ? "This project has no update yet." : "An update is due. The last one is over a cycle old.")
                        .bodyText(13)
                    Spacer()
                }
                .padding(12)
                .background(Theme.accentSoft.opacity(0.5), in: RoundedRectangle(cornerRadius: Theme.radiusCard))
            }
            composer(projectId)
            if let message = store.updatesError {
                PlannerFailure(message: message) { Task { await store.loadUpdates(projectId) } }
            }
            if let updates = store.updates[projectId] {
                if updates.isEmpty {
                    Card { CardLine("No updates yet.") }
                } else {
                    VStack(alignment: .leading, spacing: 10) {
                        ForEach(updates) { update in row(update, now: now) }
                        if store.updatesCursor[projectId] != nil {
                            Button("Show older updates") { Task { await store.loadUpdates(projectId, more: true) } }
                                .buttonStyle(.quiet)
                        }
                    }
                }
            } else {
                PlannerLoadingRows(label: "Loading updates")
            }
        }
        .task(id: projectId) {
            await store.loadUpdates(projectId)
            if let latest = detail.latestUpdate?.health { health = latest }
        }
    }

    private func composer(_ projectId: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                ForEach(PlanningProjectHealth.allCases) { choice in
                    Button { health = choice } label: {
                        HStack(spacing: 5) {
                            Circle().fill(choice.tint).frame(width: 7, height: 7)
                            Text(choice.label).font(TypeScale.label(12.5))
                        }
                        .padding(.horizontal, 10)
                        .frame(height: Theme.compactHeight)
                        .foregroundStyle(Theme.ink)
                        .background(health == choice ? Theme.selection : Theme.control,
                                    in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(health == choice ? .isSelected : [])
                }
                Spacer()
                Button("Draft from progress", systemImage: "text.badge.plus") {
                    Task {
                        drafting = true
                        defer { drafting = false }
                        do {
                            draft = try await store.draftUpdate(projectId: projectId)
                            error = nil
                        } catch {
                            self.error = error.localizedDescription
                        }
                    }
                }
                .buttonStyle(.secondary)
                .disabled(drafting)
            }
            TextEditor(text: $draft)
                .font(TypeScale.body(13.5))
                .scrollContentBackground(.hidden)
                .padding(8)
                .frame(minHeight: 90)
                .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                .overlay(alignment: .topLeading) {
                    if draft.isEmpty {
                        Text("What moved, what is in the way, what is next")
                            .bodyText(13.5, Theme.inkTertiary)
                            .padding(.horizontal, 13)
                            .padding(.vertical, 8)
                            .allowsHitTesting(false)
                    }
                }
                .accessibilityLabel("Update")
            HStack {
                if let error { Text(error).metaText(Theme.live).lineLimit(2) }
                Spacer()
                Button("Post update") {
                    let body = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                    Task {
                        do {
                            try await store.postUpdate(projectId: projectId, health: health, body: body)
                            draft = ""
                            error = nil
                        } catch {
                            self.error = error.localizedDescription
                        }
                    }
                }
                .buttonStyle(.primary)
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(store.mutating || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(14)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
    }

    private func row(_ update: PlanningProjectUpdate, now: Int64) -> some View {
        let author = switch update.author.kind {
        case .me: "You"
        case .sona: "Sona"
        case .agent: update.author.name ?? "An agent"
        }
        return VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                ProjectHealthPill(health: update.health)
                Text(author).font(TypeScale.label(12.5).weight(.medium)).foregroundStyle(Theme.ink)
                Text(ProjectDates.relative(update.createdAtMs, now: now))
                    .metaText()
                    .help(PlanningFormat.date(update.createdAtMs))
                if update.editedAtMs != nil { Text("Edited").metaText() }
                Spacer()
                Text("\(update.progress)%").metaText().monospacedDigit()
            }
            if let body = update.body, !body.isEmpty {
                Text(ProjectDescriptionEditor.markdown(body))
                    .bodyText(13.5, Theme.inkSecondary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
        .contextMenu {
            if update.author.kind == .me {
                Button("Delete update", role: .destructive) { Task { await store.deleteUpdate(update) } }
            }
        }
    }
}

/// Projects in the bin, each restorable until it is deleted for good 30
/// days after it went in.
struct ProjectBinList: View {
    let store: ProjectsStore

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Recently deleted").sectionLabel().accessibilityAddTraits(.isHeader)
            Card {
                if let error = store.trashedError {
                    CardLine(error)
                } else if let trashed = store.trashed {
                    if trashed.isEmpty {
                        CardLine("Nothing deleted in the last 30 days.")
                    } else {
                        ForEach(trashed) { summary in row(summary) }
                    }
                } else {
                    PlannerLoadingRows(label: "Loading deleted projects")
                }
            }
        }
        .task { await store.loadTrashed() }
    }

    private func row(_ summary: PlanningProjectSummary) -> some View {
        let left = summary.project.trashedAtMs.map { trashed in
            max(0, 30 - PlanningFormat.days(from: trashed, to: ProjectsStore.milliseconds(.now)))
        }
        return HStack(spacing: 10) {
            Text(summary.project.title).bodyText(13.5).lineLimit(1)
            Spacer()
            if let left {
                Text(left == 1 ? "Deleted for good tomorrow" : "Deleted for good in \(left) days").metaText()
            }
            Button("Restore") { Task { await store.restoreProject(summary.id) } }
                .buttonStyle(.secondary)
                .disabled(store.mutating)
        }
        .padding(.horizontal, 16)
        .frame(height: Theme.listRow)
    }
}

/// "Moved Harbor Redesign to the bin  Undo", for eight seconds.
struct ProjectBinToast: View {
    let store: ProjectsStore

    var body: some View {
        ZStack(alignment: .bottomLeading) {
            if let project = store.binned {
                HStack(spacing: 12) {
                    Text("Moved \(project.title) to Recently deleted").bodyText(13).lineLimit(1)
                    Button("Undo") { Task { await store.restoreProject(project.id) } }
                        .buttonStyle(.plain)
                        .font(TypeScale.label(13))
                        .foregroundStyle(Theme.accent)
                }
                .padding(.leading, 14)
                .padding(.trailing, 10)
                .frame(height: Theme.listRow)
                .background {
                    RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous)
                        .fill(Theme.elevated)
                        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
                        .elevation(.medium)
                }
                .task(id: project.id) {
                    try? await Task.sleep(for: .seconds(8))
                    if !Task.isCancelled, store.binned?.id == project.id { store.dismissBinned() }
                }
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomLeading)
        .animation(Motion.regular, value: store.binned)
    }
}
