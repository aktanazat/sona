import Charts
import SwiftUI

enum ProjectPageTab: String, CaseIterable, Hashable {
    case overview, tasks, updates, insights

    var title: String {
        switch self {
        case .overview: "Overview"
        case .tasks: "Tasks"
        case .updates: "Updates"
        case .insights: "Insights"
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
                        MarkdownBody(source: text, toggle: nil)
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
}

/// The project's fields down the side of its overview, each changed in place.
struct ProjectPropertiesPanel: View {
    let detail: PlanningProjectDetail
    let people: [PlanningPersonChoice]
    let labels: [WorkLabel]
    let save: (PlanningProjectSave) -> Void
    @Environment(AppModel.self) private var model
    @State private var addingLink = false
    @State private var linkTitle = ""
    @State private var pickingHue = false
    @State private var linkURL = ""
    @State private var pickingDay: Bool?

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
            row("Initiative") { initiativeMenu(project) }
            row("Blocked by") { dependencies(project, blockedBy: true) }
            row("Blocking") { dependencies(project, blockedBy: false) }
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
        return ProjectStatusPicker(project: project, progress: progress, pick: save) {
            HStack(spacing: 6) {
                ProjectStatusIcon(status: project.status, progress: progress)
                menuLabel(model.projects.statusTitle(of: project))
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
        let mine = project.leadIsSelf == true
        let nobody = project.leadPersonId == nil && !mine
        return Menu {
            Button("No lead") { save(PlanningProjectSave(leadPersonId: .clear)) }
                .disabled(nobody)
            Divider()
            Button("Me") { save(PlanningProjectSave(leadIsSelf: true)) }
                .disabled(mine)
            ForEach(people) { person in
                Button(person.name) { save(PlanningProjectSave(leadPersonId: .set(person.id))) }
                    .disabled(project.leadPersonId == person.id)
            }
        } label: {
            menuLabel(mine ? "Me" : project.leadPersonId.map(name) ?? "No lead", muted: nobody)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("Lead")
    }

    private func membersMenu(_ project: PlanningProject) -> some View {
        let members = project.memberPersonIds ?? []
        let mine = project.membersIncludeSelf == true
        let names = (mine ? ["Me"] : []) + members.map(name)
        return Menu {
            Button {
                save(PlanningProjectSave(membersIncludeSelf: !mine))
            } label: {
                if mine { Label("Me", systemImage: "checkmark") } else { Text("Me") }
            }
            ForEach(people) { person in
                let on = members.contains(person.id)
                Button {
                    save(PlanningProjectSave(memberPersonIds: on ? members.filter { $0 != person.id } : members + [person.id]))
                } label: {
                    if on { Label(person.name, systemImage: "checkmark") } else { Text(person.name) }
                }
            }
        } label: {
            menuLabel(names.isEmpty ? "None" : names.joined(separator: ", "), muted: names.isEmpty)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .accessibilityLabel("Members")
    }

    /// The initiative the project sits in; picking another moves it there.
    private func initiativeMenu(_ project: PlanningProject) -> some View {
        let store = model.projects
        let current = store.initiative(of: project.id)
        return Menu {
            if let current {
                Button("Open \(current.name)") {
                    store.selectedInitiativeId = current.id
                    model.go(.initiatives)
                }
                Button("No initiative") { Task { await store.removeFromInitiative(projectId: project.id) } }
                Divider()
            }
            ForEach(store.initiatives) { initiative in
                Button(initiative.name) { Task { await store.addToInitiative(initiative.id, projectId: project.id) } }
                    .disabled(initiative.id == current?.id)
            }
            if store.initiatives.isEmpty {
                Button("Make one in Initiatives") { model.go(.initiatives) }
            }
        } label: {
            menuLabel(current?.name ?? "None", muted: current == nil)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("Initiative: \(current?.name ?? "none")")
    }

    /// The projects this one waits on, or that wait on it; a broken one, whose
    /// blocker ends after the blocked project starts, in red.
    private func dependencies(_ project: PlanningProject, blockedBy: Bool) -> some View {
        let store = model.projects
        let links = store.dependencies.filter { blockedBy ? $0.blockedId == project.id : $0.blockerId == project.id }
        let other = { (link: PlanningProjectDependency) in blockedBy ? link.blockerId : link.blockedId }
        let title = store.projectTitle
        let choices = store.dependencyChoices(for: project.id)
        let verb = blockedBy ? "Blocked by" : "Blocking"
        return VStack(alignment: .leading, spacing: 2) {
            ForEach(links) { link in
                HStack(spacing: 4) {
                    Button { store.open(other(link)) } label: {
                        HStack(spacing: 5) {
                            Image(systemName: link.broken ? "exclamationmark.triangle.fill" : "arrow.right")
                                .font(.system(size: 9, weight: .semibold))
                            Text(title(other(link))).font(TypeScale.label(12.5)).lineLimit(1)
                        }
                        .foregroundStyle(link.broken ? Theme.live : Theme.ink)
                    }
                    .buttonStyle(.plain)
                    .help(link.broken
                        ? (blockedBy ? "\(title(other(link))) ends after this project starts" : "This project ends after \(title(other(link))) starts")
                        : "Open \(title(other(link)))")
                    .accessibilityLabel("\(verb) \(title(other(link)))\(link.broken ? ", broken" : "")")
                    Button { Task { await store.removeDependency(link) } } label: {
                        Image(systemName: "xmark").font(.system(size: 9, weight: .bold))
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(Theme.inkTertiary)
                    .help("Remove")
                    .accessibilityLabel("Remove \(verb.lowercased()) \(title(other(link)))")
                }
            }
            Menu {
                ForEach(choices) { choice in
                    Button(choice.title) {
                        Task {
                            await store.addDependency(blockerId: blockedBy ? choice.id : project.id,
                                                      blockedId: blockedBy ? project.id : choice.id)
                        }
                    }
                }
            } label: {
                Group {
                    if links.isEmpty { menuLabel("None", muted: true) } else { Image(systemName: "plus").font(.system(size: 10)) }
                }
                .accessibilityLabel(blockedBy ? "Add blocked by" : "Add blocking")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .disabled(choices.isEmpty)
            .help(blockedBy ? "Add a project this one waits on" : "Add a project waiting on this one")
            .accessibilityLabel(blockedBy ? "Add blocked by" : "Add blocking")
        }
        .padding(.vertical, links.isEmpty ? 0 : 4)
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
                Button("Pick a day…") { pickingDay = start }
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
        .popover(isPresented: Binding(get: { pickingDay == start }, set: { if !$0 { pickingDay = nil } }),
                 arrowEdge: .leading) {
            ProjectDayPicker(start: ms.map(PlanningFormat.moment) ?? .now) { day in
                pickingDay = nil
                let value = PlanningFieldChange.set(ProjectsStore.milliseconds(day))
                let reading: PlanningFieldChange<PlanningDateResolution>? = (resolution ?? .day) == .day ? nil : .set(.day)
                save(start ? PlanningProjectSave(startAtMs: value, startResolution: reading)
                     : PlanningProjectSave(targetAtMs: value, targetResolution: reading))
            }
        }
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
                let offered = labels.filter { $0.scope != .tasks || chosen.contains($0.id) }
                if offered.isEmpty { Text(labels.isEmpty ? "No labels yet" : "No project labels") }
                ForEach(offered) { label in
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

/// A month calendar for any day the date presets do not reach.
struct ProjectDayPicker: View {
    let start: Date
    let pick: (Date) -> Void
    @State private var day: Date?

    var body: some View {
        VStack(alignment: .trailing, spacing: 12) {
            DatePicker("Day", selection: Binding(get: { day ?? start }, set: { day = $0 }), displayedComponents: .date)
                .datePickerStyle(.graphical)
                .labelsHidden()
            Button("Set day") { pick(Calendar.current.startOfDay(for: day ?? start)) }
                .buttonStyle(.primary)
                .keyboardShortcut(.defaultAction)
        }
        .padding(16)
    }
}

/// Weekly scope, started and done since the project began, held through
/// today, with the predicted finish and its range when there is pace to go
/// on, against the target day in red.
struct ProjectProgressChart: View {
    let progress: PlanningProjectProgress
    var targetAtMs: Int64?
    var height: CGFloat = 132

    var body: some View {
        let weeks = progress.weeks
        let points = weeks.map { (at: PlanningFormat.moment($0.weekStartMs), week: $0) }
            + (weeks.last.map { [(at: Date.now, week: $0)] } ?? [])
        let peak = max(weeks.map(\.scope).max() ?? 0, 1)
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 14) {
                legend("Scope", Theme.inkTertiary, dashed: true)
                legend("Started", Theme.statusInProgress)
                legend("Done", Theme.accent)
                if targetAtMs != nil { legend("Target", Theme.live) }
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
                if let targetAtMs {
                    RuleMark(x: .value("Target", PlanningFormat.moment(targetAtMs)))
                        .foregroundStyle(Theme.live)
                        .lineStyle(StrokeStyle(lineWidth: 1.5))
                }
                ForEach(points.indices, id: \.self) { index in
                    let (at, week) = points[index]
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
        .accessibilityLabel("Progress \(progress.percent) percent. \(progress.done) done, \(progress.started) started of \(progress.scope). \(prediction)\(target)")
    }

    private var target: String {
        targetAtMs.map { " Target \(PlanningFormat.moment($0).formatted(.dateTime.month(.abbreviated).day()))." } ?? ""
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
    let people: [PlanningPersonChoice]
    @State private var health: PlanningProjectHealth = .onTrack
    @State private var draft = ""
    @State private var drafting = false
    @State private var error: String?
    @State private var editing: String?
    @State private var editHealth: PlanningProjectHealth = .onTrack
    @State private var editBody = ""
    @State private var editError: String?

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
            if let message = store.historyError {
                PlannerFailure(message: message) { Task { await store.loadHistory(projectId) } }
            }
            if let updates = store.updates[projectId] {
                let older = store.updatesCursor[projectId] != nil
                VStack(alignment: .leading, spacing: 10) {
                    if updates.isEmpty { Card { CardLine("No updates yet.") } }
                    ForEach(timeline(updates, older: older)) { entry in
                        switch entry {
                        case let .update(update): row(update, now: now)
                        case let .changes(changes): changeRows(changes, now: now)
                        }
                    }
                    if older {
                        Button("Show older updates") { Task { await store.loadUpdates(projectId, more: true) } }
                            .buttonStyle(.quiet)
                    }
                }
            } else {
                PlannerLoadingRows(label: "Loading updates")
            }
        }
        .task(id: projectId) {
            await store.loadUpdates(projectId)
            if let latest = detail.latestUpdate?.health { health = latest }
            await store.loadHistory(projectId)
        }
    }

    /// Updates and property changes newest first, the changes made between
    /// two updates set between them. While older updates are unread, changes
    /// from before the oldest read one wait for them.
    private func timeline(_ updates: [PlanningProjectUpdate], older: Bool) -> [ProjectTimelineEntry] {
        let floor = older ? updates.last?.createdAtMs : nil
        var rest = (store.history[detail.project.id] ?? []).filter { change in
            line(change) != nil && (floor.map { change.atMs >= $0 } ?? true)
        }[...]
        var entries: [ProjectTimelineEntry] = []
        for update in updates {
            let newer = rest.prefix { $0.atMs > update.createdAtMs }
            if !newer.isEmpty { entries.append(.changes(Array(newer))) }
            rest = rest.dropFirst(newer.count)
            entries.append(.update(update))
        }
        if !rest.isEmpty { entries.append(.changes(Array(rest))) }
        return entries
    }

    private func changeRows(_ changes: [PlanningProjectActivity], now: Int64) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(changes) { change in
                let text = line(change) ?? ""
                HStack(spacing: 8) {
                    Image(systemName: Self.symbol(change.kind))
                        .font(.system(size: 10.5, weight: .medium))
                        .foregroundStyle(Theme.inkTertiary)
                        .frame(width: 16)
                    Text(text).bodyText(12.5, Theme.inkSecondary).lineLimit(1).help(text)
                    Spacer(minLength: 12)
                    Text(ProjectDates.relative(change.atMs, now: now))
                        .metaText()
                        .help(PlanningFormat.date(change.atMs))
                }
                .padding(.horizontal, 14)
                .frame(height: 24)
                .accessibilityElement(children: .combine)
            }
        }
    }

    /// "Target date changed from Mar 3 to Apr 28", or nothing for a change
    /// this app cannot read.
    private func line(_ change: PlanningProjectActivity) -> String? {
        let from = change.fromValue
        let to = change.toValue
        switch change.kind {
        case .title:
            guard let to else { return nil }
            return from.map { "Renamed from “\($0)” to “\(to)”" } ?? "Named “\(to)”"
        case .status:
            return "Status: \(Self.status(from)) → \(Self.status(to))"
        case .statusName:
            switch (from, to) {
            case let (from?, to?): return "Status name: \(from) → \(to)"
            case let (nil, to?): return "Status name set to \(to)"
            case let (from?, nil): return "Status name \(from) removed"
            case (nil, nil): return nil
            }
        case .lead:
            guard let to else { return "Lead removed" }
            return to == "me" ? "You became the lead" : "Lead changed to \(person(to))"
        case .members:
            let before = Self.ids(from)
            let after = Self.ids(to)
            let added = after.filter { !before.contains($0) }.map(person).joined(separator: ", ")
            let removed = before.filter { !after.contains($0) }.map(person).joined(separator: ", ")
            switch (added.isEmpty, removed.isEmpty) {
            case (false, true): return "Members added: \(added)"
            case (true, false): return "Members removed: \(removed)"
            case (false, false): return "Members added: \(added); removed: \(removed)"
            case (true, true): return nil
            }
        case .startDate:
            return Self.dateLine("Start date", from, to)
        case .targetDate:
            return Self.dateLine("Target date", from, to)
        case .priority:
            return "Priority: \(Self.priority(from)) → \(Self.priority(to))"
        case .milestoneAdded:
            return change.subject.map { "Milestone added: \($0)" }
        case .milestoneRemoved:
            return change.subject.map { "Milestone removed: \($0)" }
        case .milestoneTarget:
            guard let subject = change.subject else { return nil }
            switch (Self.day(from), Self.day(to)) {
            case let (from?, to?): return "\(subject) moved from \(from) to \(to)"
            case let (nil, to?): return "\(subject) due \(to)"
            case (_?, nil): return "\(subject) no longer has a due date"
            case (nil, nil): return nil
            }
        case .blockedByAdded:
            return change.subject.map { "Blocked by \($0)" }
        case .blockedByRemoved:
            return change.subject.map { "No longer blocked by \($0)" }
        case .blockingAdded:
            return change.subject.map { "Blocking \($0)" }
        case .blockingRemoved:
            return change.subject.map { "No longer blocking \($0)" }
        case .initiative:
            switch (from, to) {
            case let (from?, to?): return "Moved from initiative \(from) to \(to)"
            case let (nil, to?): return "Added to initiative \(to)"
            case let (from?, nil): return "Removed from initiative \(from)"
            case (nil, nil): return nil
            }
        case .unknown:
            return nil
        }
    }

    private func person(_ id: String) -> String {
        id == "me" ? "Me" : people.first { $0.id == id }?.name ?? "Someone"
    }

    private static func ids(_ value: String?) -> [String] {
        (value ?? "").split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
    }

    private static func status(_ value: String?) -> String {
        value.flatMap(PlanningProjectStatus.init(rawValue:))?.label ?? "None"
    }

    private static func priority(_ value: String?) -> String {
        TaskPriority(value.flatMap { Int($0) } ?? 0).label
    }

    private static func day(_ value: String?) -> String? {
        value.flatMap { Int64($0) }.map(PlanningFormat.day)
    }

    private static func dateLine(_ field: String, _ from: String?, _ to: String?) -> String? {
        switch (day(from), day(to)) {
        case let (from?, to?): "\(field) changed from \(from) to \(to)"
        case let (nil, to?): "\(field) set to \(to)"
        case (_?, nil): "\(field) removed"
        case (nil, nil): nil
        }
    }

    private static func symbol(_ kind: PlanningProjectActivity.Kind) -> String {
        switch kind {
        case .title: "pencil"
        case .status, .statusName: "circle.lefthalf.filled"
        case .lead: "person"
        case .members: "person.2"
        case .startDate, .targetDate: "calendar"
        case .priority: "chart.bar"
        case .milestoneAdded, .milestoneRemoved, .milestoneTarget: "diamond"
        case .blockedByAdded, .blockedByRemoved: "arrow.right.to.line"
        case .blockingAdded, .blockingRemoved: "arrow.left.to.line"
        case .initiative: "scope"
        case .unknown: "circle"
        }
    }

    private func composer(_ projectId: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                healthChoices($health)
                Spacer()
                Button("Draft from tasks and meetings", systemImage: "text.badge.plus") {
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
                .keyboardShortcut(editing == nil ? KeyboardShortcut(.return, modifiers: .command) : nil)
                .disabled(store.mutating || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(14)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
    }

    private func healthChoices(_ selection: Binding<PlanningProjectHealth>) -> some View {
        HStack(spacing: 8) {
            ForEach(PlanningProjectHealth.allCases) { choice in
                Button { selection.wrappedValue = choice } label: {
                    HStack(spacing: 5) {
                        Circle().fill(choice.tint).frame(width: 7, height: 7)
                        Text(choice.label).font(TypeScale.label(12.5))
                    }
                    .padding(.horizontal, 10)
                    .frame(height: Theme.compactHeight)
                    .foregroundStyle(Theme.ink)
                    .background(selection.wrappedValue == choice ? Theme.selection : Theme.control,
                                in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(selection.wrappedValue == choice ? .isSelected : [])
            }
        }
    }

    @ViewBuilder
    private func row(_ update: PlanningProjectUpdate, now: Int64) -> some View {
        if editing == update.id {
            editor(update)
        } else {
            ProjectUpdateCard(update: update, now: now)
                .contextMenu {
                    if update.author.kind == .me {
                        Button("Edit update") {
                            editHealth = update.health
                            editBody = update.body ?? ""
                            editError = nil
                            editing = update.id
                        }
                        Button("Delete update", role: .destructive) { Task { await store.deleteUpdate(update) } }
                    }
                }
        }
    }

    private func editor(_ update: PlanningProjectUpdate) -> some View {
        let body = editBody.trimmingCharacters(in: .whitespacesAndNewlines)
        return VStack(alignment: .leading, spacing: 10) {
            healthChoices($editHealth)
            TextEditor(text: $editBody)
                .font(TypeScale.body(13.5))
                .scrollContentBackground(.hidden)
                .padding(8)
                .frame(minHeight: 90)
                .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                .accessibilityLabel("Update")
            HStack(spacing: 8) {
                if let editError { Text(editError).metaText(Theme.live).lineLimit(2) }
                Spacer()
                Button("Cancel") { editing = nil }
                    .buttonStyle(.secondary)
                    .keyboardShortcut(.cancelAction)
                Button("Save update") {
                    Task {
                        do {
                            try await store.editUpdate(update, health: editHealth, body: body)
                            editing = nil
                        } catch {
                            editError = error.localizedDescription
                        }
                    }
                }
                .buttonStyle(.primary)
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(store.mutating || body.isEmpty)
            }
        }
        .padding(14)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous).strokeBorder(Theme.accent, lineWidth: 1))
    }
}

/// A posted update, or the property changes made between two updates.
private enum ProjectTimelineEntry: Identifiable {
    case update(PlanningProjectUpdate)
    case changes([PlanningProjectActivity])

    var id: String {
        switch self {
        case let .update(update): "update:\(update.id)"
        case let .changes(changes): "changes:\(changes.first?.id ?? "")"
        }
    }
}

/// One posted update: its health, who wrote it and when, and what it says.
struct ProjectUpdateCard: View {
    let update: PlanningProjectUpdate
    let now: Int64

    var body: some View {
        let author = switch update.author.kind {
        case .me: "You"
        case .sona: "Sona"
        case .agent: update.author.name ?? "An agent"
        }
        VStack(alignment: .leading, spacing: 8) {
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
                MarkdownBody(source: body, toggle: nil)
                    .textSelection(.enabled)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// The project's name as the page's headline. A click makes it a field:
/// Return keeps the new name, Esc puts the old one back. The page holds the
/// draft, so its own Esc can end the rename before it closes the project.
struct ProjectTitleField: View {
    let title: String
    @Binding var draft: String?
    let save: (String) -> Void
    @FocusState private var focused: Bool

    var body: some View {
        if let editing = draft {
            TextField("Project name", text: Binding(get: { editing }, set: { draft = $0 }))
                .textFieldStyle(.plain)
                .font(TypeScale.hero)
                .tracking(-0.3)
                .foregroundStyle(Theme.ink)
                .focused($focused)
                .onSubmit(commit)
                .onExitCommand { draft = nil }
                .onChange(of: focused) { _, inside in
                    if !inside { commit() }
                }
                .onAppear { focused = true }
                .accessibilityLabel("Project name")
        } else {
            Button { draft = title } label: {
                Text(title).heroText()
                    .multilineTextAlignment(.leading)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .buttonStyle(.plain)
            .help("Rename project")
            .accessibilityLabel(title)
            .accessibilityHint("Rename the project")
            .accessibilityAddTraits(.isHeader)
        }
    }

    private func commit() {
        guard let editing = draft else { return }
        draft = nil
        let name = editing.trimmingCharacters(in: .whitespacesAndNewlines)
        if !name.isEmpty, name != title { save(name) }
    }
}

/// The project's one-line summary under its name, changed in place: Return
/// or leaving the field keeps it, Esc puts the old one back.
struct ProjectSummaryField: View {
    let summary: String?
    let save: (String) -> Void
    @State private var draft: String?
    @FocusState private var focused: Bool

    var body: some View {
        TextField("Add a short summary…", text: Binding(get: { draft ?? summary ?? "" }, set: { draft = $0 }))
            .textFieldStyle(.plain)
            .font(TypeScale.body(14))
            .foregroundStyle(Theme.inkSecondary)
            .lineLimit(1)
            .focused($focused)
            .onSubmit {
                commit()
                focused = false
            }
            .onExitCommand {
                draft = nil
                focused = false
            }
            .onChange(of: focused) { _, inside in
                if !inside { commit() }
            }
            .accessibilityLabel("Summary")
    }

    private func commit() {
        guard let editing = draft else { return }
        draft = nil
        let text = editing.trimmingCharacters(in: .whitespacesAndNewlines)
        if text != (summary ?? "") { save(text) }
    }
}

/// The project's status: the six categories, each with the owner's own
/// names for it set in under it, and the sheet that edits those names.
struct ProjectStatusPicker<Label: View>: View {
    let project: PlanningProject
    let progress: Double
    let pick: (PlanningProjectSave) -> Void
    @ViewBuilder let label: () -> Label
    @Environment(AppModel.self) private var model
    @State private var open = false
    @State private var editing = false

    var body: some View {
        let store = model.projects
        let named = store.statusName(of: project)
        Button { open.toggle() } label: { label().contentShape(Rectangle()) }
            .buttonStyle(.plain)
            .accessibilityLabel("Status: \(store.statusTitle(of: project))")
            .popover(isPresented: $open, arrowEdge: .bottom) {
                VStack(alignment: .leading, spacing: 1) {
                    ForEach(PlanningProjectStatus.allCases) { category in
                        let current = named == nil && project.status == category
                        ProjectStatusRow(title: category.label, selected: current) {
                            ProjectStatusIcon(status: category, progress: progress)
                        } action: {
                            choose(current ? nil : PlanningProjectSave(
                                status: category, statusNameId: project.statusNameId == nil ? nil : .clear))
                        }
                        ForEach(store.statusNames.filter { $0.category == category }) { name in
                            ProjectStatusRow(title: name.name, selected: named?.id == name.id, nested: true) {
                                ProjectStatusIcon(status: category, progress: progress)
                            } action: {
                                choose(named?.id == name.id ? nil : PlanningProjectSave(statusNameId: .set(name.id)))
                            }
                        }
                    }
                    Divider().padding(.vertical, 4)
                    ProjectStatusRow(title: "Edit statuses…", selected: false) {
                        Image(systemName: "pencil")
                            .font(.system(size: 11, weight: .medium))
                            .foregroundStyle(Theme.inkSecondary)
                    } action: {
                        open = false
                        Task { editing = true }
                    }
                }
                .padding(5)
                .frame(minWidth: 190)
            }
            .sheet(isPresented: $editing) { ProjectStatusNamesSheet(store: store) }
    }

    private func choose(_ change: PlanningProjectSave?) {
        open = false
        if let change { pick(change) }
    }
}

/// A row of the status list: a category, or one of its names set in under it.
private struct ProjectStatusRow<Icon: View>: View {
    let title: String
    let selected: Bool
    var nested = false
    @ViewBuilder let icon: () -> Icon
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                icon().frame(width: 16)
                Text(title).font(TypeScale.label(13)).foregroundStyle(Theme.ink).lineLimit(1)
                Spacer(minLength: 12)
                if selected {
                    Image(systemName: "checkmark")
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(Theme.inkSecondary)
                }
            }
            .padding(.leading, nested ? 24 : 8)
            .padding(.trailing, 8)
            .frame(height: 26)
            .background(hovering ? Theme.wash : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusRow))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .hoverFade(hovering)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// The owner's names for statuses, each under its category: add one, rename
/// it, move it to another category or remove it.
struct ProjectStatusNamesSheet: View {
    let store: ProjectsStore
    @Environment(\.dismiss) private var dismiss
    @State private var newName = ""
    @State private var newCategory = PlanningProjectStatus.active
    @State private var error: String?
    @State private var removing: PlanningProjectStatusName?

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Statuses").titleText().accessibilityAddTraits(.isHeader)
            HStack(spacing: 8) {
                InputField(prompt: "New status name", text: $newName)
                    .onSubmit(add)
                    .accessibilityLabel("New status name")
                Picker("Category", selection: $newCategory) {
                    ForEach(PlanningProjectStatus.allCases) { Text($0.label).tag($0) }
                }
                .labelsHidden()
                .fixedSize()
                Button("Add", action: add)
                    .buttonStyle(.secondary)
                    .disabled(store.mutating || newName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(PlanningProjectStatus.allCases) { category in
                        VStack(alignment: .leading, spacing: 6) {
                            HStack(spacing: 8) {
                                ProjectStatusIcon(status: category, progress: 0.5)
                                Text(category.label)
                                    .font(TypeScale.label(13).weight(.medium))
                                    .foregroundStyle(Theme.ink)
                            }
                            ForEach(store.statusNames.filter { $0.category == category }) { name in
                                ProjectStatusNameRow(name: name, busy: store.mutating) { category, text in
                                    run { try await store.saveStatusName(id: name.id, category: category, name: text) }
                                } remove: {
                                    if uses(name) > 0 {
                                        removing = name
                                    } else {
                                        run { try await store.deleteStatusName(name) }
                                    }
                                }
                            }
                        }
                    }
                }
                .padding(2)
            }
            if let message = error ?? store.statusNamesError { ErrorNote(message) }
            HStack {
                Spacer()
                Button("Done") { dismiss() }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.cancelAction)
            }
        }
        .padding(24)
        .frame(width: 460, height: 540)
        .background(Theme.page)
        .confirmationDialog("Remove “\(removing?.name ?? "")”?",
                            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
                            presenting: removing) { name in
            Button("Remove", role: .destructive) { run { try await store.deleteStatusName(name) } }
        } message: { name in
            let count = uses(name)
            Text(count == 1 ? "1 project keeps \(name.category.label) and loses this name."
                 : "\(count) projects keep \(name.category.label) and lose this name.")
        }
    }

    private func uses(_ name: PlanningProjectStatusName) -> Int {
        (store.projects ?? []).filter { $0.project.statusNameId == name.id }.count
    }

    private func add() {
        let text = newName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !store.mutating else { return }
        run {
            try await store.saveStatusName(id: nil, category: newCategory, name: text)
            newName = ""
        }
    }

    private func run(_ change: @escaping @MainActor () async throws -> Void) {
        Task {
            do {
                try await change()
                error = nil
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

/// One status name in the sheet: renamed in place, moved to another
/// category from its menu, or removed.
private struct ProjectStatusNameRow: View {
    let name: PlanningProjectStatusName
    let busy: Bool
    let save: (PlanningProjectStatus, String) -> Void
    let remove: () -> Void
    @State private var draft: String?
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: 8) {
            TextField("Name", text: Binding(get: { draft ?? name.name }, set: { draft = $0 }))
                .textFieldStyle(.roundedBorder)
                .focused($focused)
                .onSubmit(commit)
                .onChange(of: focused) { _, inside in
                    if !inside { commit() }
                }
                .accessibilityLabel("Name of \(name.name)")
            Menu(name.category.label) {
                Picker("Category", selection: Binding(get: { name.category }, set: { save($0, name.name) })) {
                    ForEach(PlanningProjectStatus.allCases) { Text($0.label).tag($0) }
                }
                .pickerStyle(.inline)
                .labelsHidden()
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .frame(width: 100, alignment: .trailing)
            .disabled(busy)
            .help("Category")
            .accessibilityLabel("Category of \(name.name)")
            Button(action: remove) { Image(systemName: "trash") }
                .buttonStyle(.icon)
                .disabled(busy)
                .help("Remove \(name.name)")
                .accessibilityLabel("Remove \(name.name)")
        }
        .padding(.leading, 24)
    }

    private func commit() {
        guard let editing = draft else { return }
        draft = nil
        let text = editing.trimmingCharacters(in: .whitespacesAndNewlines)
        if !text.isEmpty, text != name.name { save(name.category, text) }
    }
}

/// Space on the projects list: the project at a glance beside it, its latest
/// update and its fields, each changed in place. J and K carry the peek
/// along; Space or Esc closes it and Return opens the project.
struct ProjectPeek: View {
    let store: ProjectsStore
    let detail: PlanningProjectDetail
    let people: [PlanningPersonChoice]
    let labels: [WorkLabel]
    let close: () -> Void

    var body: some View {
        let project = detail.project
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                if let icon = project.icon { Text(icon).font(.system(size: 13)) }
                Spacer(minLength: 8)
                TaskKeyButton(title: "Open", keys: "return") { store.open(project.id) }
                WorkBarButton(icon: "xmark", help: "Close  Space", action: close)
            }
            .padding(.leading, 20)
            .padding(.trailing, 12)
            .frame(height: Theme.headerHeight)
            Hairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    Text(project.title)
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundStyle(Theme.ink)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                    if let description = project.description, !description.isEmpty {
                        Text(description)
                            .bodyText(13, Theme.inkSecondary)
                            .lineLimit(6)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    if let update = detail.latestUpdate {
                        ProjectUpdateCard(update: update, now: ProjectsStore.milliseconds(.now))
                    }
                    ProjectPropertiesPanel(detail: detail, people: people, labels: labels) { change in
                        Task { await store.saveProject(project, change) }
                    }
                }
                .padding(20)
            }
        }
        .frame(width: 400)
        .frame(maxHeight: .infinity)
        .background {
            Rectangle()
                .fill(Theme.elevated)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.border).frame(width: 1) }
                .elevation(.high)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Peek at \(project.title)")
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
