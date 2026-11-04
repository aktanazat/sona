import SwiftUI

enum ProjectsLayout: String {
    case list, board
}

/// The ways to look at planned work. Calendar is its own place, so choosing
/// it leaves the projects list.
enum ProjectsViewChoice: Hashable {
    case list, board, calendar

    static var items: [SwitchItem<ProjectsViewChoice>] {
        [
            SwitchItem(id: .list, title: "List", icon: "list.bullet"),
            SwitchItem(id: .board, title: "Board", icon: "rectangle.split.3x1"),
            SwitchItem(id: .calendar, title: "Calendar", icon: "calendar"),
        ]
    }
}

struct ProjectsView: View {
    let store: ProjectsStore
    let people: [PlanningPersonChoice]
    var peopleUnavailable = false
    var material: AppearanceMaterial = .solid
    var photoRevision: UInt64 = 0
    let openEvidence: (PlanningEvidenceLink) -> Void
    let openPerson: (String) -> Void
    let discussProject: (PlanningProject) -> Void
    let planWithSona: () -> Void
    let showCalendar: () -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @AppStorage("projects.layout") private var layout = ProjectsLayout.list
    @State private var query = ""
    @State private var creation: PlanningCreation?
    @State private var sourceLink: PlanningLinkRequest?
    @State private var reviewProject: PlanningProject?
    @State private var cursor: String?
    @State private var collapsed: Set<PlanningProjectStatus> = []
    @FocusState private var keysFocused: Bool

    var body: some View {
        Group {
            if let id = store.selectedProjectId {
                projectPage(id)
                    .transition(.opacity)
            } else {
                Page { projectList }
                    .transition(.opacity)
            }
        }
        .animation(Motion.settle(reduced: reduceMotion), value: store.selectedProjectId)
        .task { await store.start() }
        .task(id: store.selectedProjectId) {
            if let id = store.selectedProjectId { await store.loadDetail(id) }
        }
        .sheet(item: $creation) { target in
            PlanningCreateSheet(target: target, store: store, people: people, peopleUnavailable: peopleUnavailable)
        }
        .sheet(item: $sourceLink) { request in
            PlanningLinkSourceSheet(request: request, store: store, material: material)
        }
        .sheet(item: $reviewProject) { project in
            PlannerReviewView(project: project, store: store, material: material) { link in
                reviewProject = nil
                openEvidence(link)
            }
        }
        .transaction {
            if reduceMotion { $0.animation = nil; $0.disablesAnimations = true }
        }
    }

    private var now: Int64 { ProjectsStore.milliseconds(.now) }

    private var visibleProjects: [PlanningProjectSummary] {
        let search = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let filter = store.filter
        let now = now
        return (store.projects ?? []).filter { summary in
            if let status = filter.status, summary.project.status != status { return false }
            if filter.overdueOnly, (store.details[summary.id]?.overdueTaskCount(at: now) ?? 0) == 0 { return false }
            if filter.mineOnly, store.details[summary.id]?.hasMyOpenWork != true { return false }
            guard !search.isEmpty else { return true }
            return summary.project.title.localizedCaseInsensitiveContains(search)
                || (summary.project.description?.localizedCaseInsensitiveContains(search) ?? false)
        }
    }

    // MARK: List

    private var projectList: some View {
        VStack(alignment: .leading, spacing: 0) {
            listHeader
                .padding(.bottom, store.filter.isActive ? 10 : 18)
            if store.filter.isActive {
                filterChips.padding(.bottom, 14)
            }
            if let error = store.listError {
                PlannerFailure(message: error) { Task { await store.loadProjects() } }
                    .padding(.bottom, 16)
            }
            if let error = store.mutationError {
                PlannerFailure(message: error, retryTitle: "Refresh") {
                    store.clearMutationError()
                    Task { await store.refresh() }
                }
                .padding(.bottom, 16)
            }
            if store.projects == nil {
                if store.listLoading { PlannerLoadingRows(label: "Loading projects") }
            } else if visibleProjects.isEmpty {
                Card(material: material) {
                    CardLine(store.projects?.isEmpty == true ? "No projects yet." : "No matching projects.")
                }
            } else if layout == .board {
                board
            } else {
                ScrollViewReader { proxy in list(proxy) }
            }
        }
    }

    private var listHeader: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 10) {
                headerTitle
                Spacer(minLength: 16)
                headerControls
            }
            VStack(alignment: .leading, spacing: 10) {
                headerTitle
                HStack(spacing: 10) { headerControls }
            }
        }
    }

    private var headerTitle: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text("Projects").titleText().accessibilityAddTraits(.isHeader)
            if let count = store.projects?.count, count > 0 {
                Text("\(count)").metaText().monospacedDigit()
            }
        }
    }

    @ViewBuilder
    private var headerControls: some View {
        SearchField(prompt: "Search", text: $query)
            .frame(width: 160)
            .accessibilityLabel("Search projects")
        filterMenu
        SegmentedSwitch(items: ProjectsViewChoice.items, selection: layout == .board ? .board : .list,
                        iconOnly: true) { choice in
            switch choice {
            case .list: layout = .list
            case .board: layout = .board
            case .calendar: showCalendar()
            }
        }
        Button("Plan with Sona", systemImage: "bubble.left") { planWithSona() }
            .buttonStyle(.compact)
        Button("New project", systemImage: "plus") { creation = .project }
            .buttonStyle(PrimaryButton(compact: true))
            .help("New project (C)")
    }

    private var filterMenu: some View {
        Menu {
            Section("Status") {
                Picker("Status", selection: Binding(get: { store.filter.status }, set: { store.filter.status = $0 })) {
                    Text("Any status").tag(PlanningProjectStatus?.none)
                    ForEach(PlanningProjectStatus.allCases) { status in
                        Text(status.label).tag(PlanningProjectStatus?.some(status))
                    }
                }
                .pickerStyle(.inline)
                .labelsHidden()
            }
            Section {
                Toggle("Overdue tasks", isOn: Binding(get: { store.filter.overdueOnly },
                                                      set: { store.filter.overdueOnly = $0 }))
                Toggle("Assigned to me", isOn: Binding(get: { store.filter.mineOnly },
                                                       set: { store.filter.mineOnly = $0 }))
            }
            if store.filter.isActive {
                Button("Clear filters") { store.filter = PlanningListFilter() }
            }
        } label: {
            Image(systemName: "line.3.horizontal.decrease")
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .frame(width: 30, height: 28)
        .background(store.filter.isActive ? Theme.accentSoft : Theme.control,
                    in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .help("Filter")
        .accessibilityLabel("Filter projects")
    }

    private var filterChips: some View {
        HStack(spacing: 6) {
            if let status = store.filter.status {
                filterChip(status.label) { store.filter.status = nil }
            }
            if store.filter.overdueOnly {
                filterChip("Overdue tasks") { store.filter.overdueOnly = false }
            }
            if store.filter.mineOnly {
                filterChip("Assigned to me") { store.filter.mineOnly = false }
            }
        }
    }

    private func filterChip(_ title: String, remove: @escaping () -> Void) -> some View {
        Button(action: remove) {
            HStack(spacing: 6) {
                Text(title)
                Image(systemName: "xmark")
                    .font(.system(size: 8, weight: .bold))
                    .foregroundStyle(Theme.inkTertiary)
            }
            .font(TypeScale.label(12))
            .foregroundStyle(Theme.ink)
            .padding(.horizontal, 10)
            .frame(height: 24)
            .background(Theme.control, in: Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Remove filter: \(title)")
    }

    private var listRows: [ProjectListRow] {
        let visible = visibleProjects
        var rows: [ProjectListRow] = []
        for status in PlanningProjectStatus.allCases {
            let members = visible.filter { $0.project.status == status }
            guard !members.isEmpty else { continue }
            rows.append(.group(status, members.count))
            if !collapsed.contains(status) { rows += members.map(ProjectListRow.project) }
        }
        return rows
    }

    private func list(_ proxy: ScrollViewProxy) -> some View {
        let rows = listRows
        let now = now
        return Card(material: material) {
            ForEach(rows) { row in
                switch row {
                case let .group(status, count):
                    GroupHeader(title: status.label, count: count, collapsed: collapsed.contains(status)) {
                        ProjectStatusIcon(status: status, progress: 0.5)
                    } toggle: {
                        if collapsed.contains(status) { collapsed.remove(status) } else { collapsed.insert(status) }
                    }
                case let .project(summary):
                    projectRow(summary, now: now)
                        .id(row.id)
                }
            }
        }
        .focusable()
        .focusEffectDisabled()
        .focused($keysFocused)
        .onKeyPress(.downArrow) { moveCursor(1, in: rows, proxy: proxy) }
        .onKeyPress(.upArrow) { moveCursor(-1, in: rows, proxy: proxy) }
        .onKeyPress(.return) {
            guard let cursor, rows.contains(where: { $0.id == ProjectListRow.projectId(cursor) }) else { return .ignored }
            store.open(cursor)
            return .handled
        }
        .onKeyPress(characters: CharacterSet(charactersIn: "c"), phases: .down, action: createProject)
        .onAppear { keysFocused = true }
        .animation(Motion.settle(reduced: reduceMotion), value: rows.map(\.id))
        .animation(Motion.settle(reduced: reduceMotion), value: store.projects)
    }

    private func moveCursor(_ step: Int, in rows: [ProjectListRow], proxy: ScrollViewProxy) -> KeyPress.Result {
        let ids = rows.compactMap(\.projectId)
        guard !ids.isEmpty else { return .ignored }
        let next = if let cursor, let index = ids.firstIndex(of: cursor) {
            ids[min(max(index + step, 0), ids.count - 1)]
        } else {
            step > 0 ? ids[0] : ids[ids.count - 1]
        }
        cursor = next
        proxy.scrollTo(ProjectListRow.projectId(next))
        return .handled
    }

    private func createProject(_ press: KeyPress) -> KeyPress.Result {
        guard press.modifiers.isEmpty else { return .ignored }
        creation = .project
        return .handled
    }

    private func projectRow(_ summary: PlanningProjectSummary, now: Int64) -> some View {
        let total = summary.openTaskCount + summary.completedTaskCount
        let fraction = total > 0 ? Double(summary.completedTaskCount) / Double(total) : 0
        return DenseRow(highlighted: cursor == summary.id, action: { store.open(summary.id) },
                        hover: { if $0 { cursor = summary.id } }) {
            ProjectStatusIcon(status: summary.project.status, progress: fraction)
            Text(summary.project.title)
                .font(TypeScale.body(13.5).weight(.medium))
                .foregroundStyle(Theme.ink)
                .lineLimit(1)
            if total == 0 { Text("No tasks").metaText() }
            Spacer(minLength: 12)
            AvatarStack(faces: store.details[summary.id].map(owners) ?? [], size: 18, revision: photoRevision)
                .frame(width: 62, alignment: .trailing)
            dueLabel(summary.nextDueAtMs, overdue: summary.nextDueAtMs.map { $0 < now } ?? false)
                .frame(width: 58, alignment: .trailing)
            ProjectProgressRing(done: summary.completedTaskCount, total: total)
                .frame(width: 58, alignment: .trailing)
            Text("\(summary.completedTaskCount)/\(total)")
                .metaText()
                .monospacedDigit()
                .frame(width: 40, alignment: .trailing)
                .accessibilityHidden(true)
        }
        .contextMenu { moveMenu(summary.project) }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityAction { store.open(summary.id) }
    }

    @ViewBuilder
    private func moveMenu(_ project: PlanningProject) -> some View {
        ForEach(PlanningProjectStatus.allCases) { status in
            Button("Move to \(status.label)") { Task { await store.setProjectStatus(project, status) } }
                .disabled(status == project.status)
        }
    }

    private func owners(_ detail: PlanningProjectDetail) -> [AvatarFace] {
        var faces: [AvatarFace] = []
        var seen = Set<String>()
        for task in detail.tasks where task.status.isOpen {
            let face: AvatarFace? = if task.ownerIsSelf {
                AvatarFace(kind: .me, name: "Me")
            } else if let id = task.ownerPersonId, task.ownerAvailable {
                AvatarFace(kind: .person(id), name: people.first { $0.id == id }?.name ?? "Someone")
            } else {
                nil
            }
            if let face, seen.insert(face.id).inserted { faces.append(face) }
        }
        return faces
    }

    // MARK: Board

    private var board: some View {
        let visible = visibleProjects
        let now = now
        return HStack(alignment: .top, spacing: 10) {
            ForEach(PlanningProjectStatus.allCases) { status in
                let members = visible.filter { $0.project.status == status }
                BoardColumn(status: status, count: members.count) {
                    ForEach(members) { summary in
                        BoardCard(summary: summary, faces: store.details[summary.id].map(owners) ?? [],
                                  revision: photoRevision, now: now) { store.open(summary.id) }
                            .contextMenu { moveMenu(summary.project) }
                    }
                } drop: { id in move(id, to: status) }
            }
        }
        .focusable()
        .focusEffectDisabled()
        .focused($keysFocused)
        .onKeyPress(characters: CharacterSet(charactersIn: "c"), phases: .down, action: createProject)
        .onAppear { keysFocused = true }
        .animation(Motion.settle(reduced: reduceMotion), value: visible.map { "\($0.id):\($0.project.status.rawValue)" })
    }

    private func move(_ id: String, to status: PlanningProjectStatus) -> Bool {
        guard !store.mutating, let summary = store.projects?.first(where: { $0.id == id }),
              summary.project.status != status else { return false }
        Task { await store.setProjectStatus(summary.project, status) }
        return true
    }

    // MARK: Detail

    private func projectPage(_ id: String) -> some View {
        let detail = store.detail
        return Page(back: PageBack("Projects") { store.closeProject() }, title: detail?.project.title) {
            if let detail {
                projectContent(detail)
            } else if let error = store.detailError {
                PlannerFailure(message: error) { Task { await store.loadDetail(id) } }
            } else {
                PlannerLoadingRows(label: "Loading project")
            }
        } actions: {
            if let detail {
                Button("Review links", systemImage: "link") { reviewProject = detail.project }
                    .buttonStyle(.compact)
                    .disabled(store.mutating)
                Button("Talk to Sona", systemImage: "bubble.left") { discussProject(detail.project) }
                    .buttonStyle(.compact)
                Button {
                    Task { await store.loadDetail(id) }
                } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(.quiet)
                    .accessibilityLabel("Refresh project")
                    .help("Refresh project")
                    .disabled(store.detailLoading)
            }
        }
    }

    private func projectContent(_ detail: PlanningProjectDetail) -> some View {
        let now = now
        return VStack(alignment: .leading, spacing: 0) {
            header(detail, now: now)
                .padding(.top, 6)
                .padding(.bottom, 20)
            if let error = store.mutationError {
                PlannerFailure(message: error, retryTitle: "Refresh") {
                    store.clearMutationError()
                    Task { await store.loadDetail(detail.project.id) }
                }
                .padding(.bottom, 20)
            }
            numbers(detail, now: now)
                .padding(.bottom, 28)
            if let series = detail.burnUp(until: .now) {
                VStack(alignment: .leading, spacing: 10) {
                    Text("Progress").sectionLabel().accessibilityAddTraits(.isHeader)
                    Card(material: material) {
                        BurnUpChart(series: series).padding(16)
                    }
                }
                .padding(.bottom, 28)
            }
            milestones(detail, now: now)
            tasks(detail, now: now)
            evidence(detail)
        }
        .animation(Motion.settle(reduced: reduceMotion), value: detail)
        .focusable()
        .focusEffectDisabled()
        .focused($keysFocused)
        .onKeyPress(characters: CharacterSet(charactersIn: "c"), phases: .down) { press in
            guard press.modifiers.isEmpty, !store.mutating else { return .ignored }
            creation = .task(detail.project.id, detail.milestones)
            return .handled
        }
        .onKeyPress(.escape) {
            store.closeProject()
            return .handled
        }
        .onAppear { keysFocused = true }
    }

    private func header(_ detail: PlanningProjectDetail, now: Int64) -> some View {
        let fraction = detail.countedTaskCount > 0
            ? Double(detail.completedTaskCount) / Double(detail.countedTaskCount) : 0
        let project = detail.project
        return VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .center, spacing: 16) {
                ZStack {
                    ProgressRing(fraction: fraction, size: 52, lineWidth: 4.5)
                    Text(ProgressRing.percent(fraction))
                        .font(TypeScale.label(12).weight(.semibold))
                        .monospacedDigit()
                        .foregroundStyle(Theme.ink)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("\(ProgressRing.percent(fraction)) of tasks done")
                VStack(alignment: .leading, spacing: 8) {
                    Text(project.title).heroText()
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityAddTraits(.isHeader)
                        .pageHeadline()
                    HStack(spacing: 12) {
                        ChoicePicker(PlanningProjectStatus.allCases, current: project.status, title: \.label,
                                     accessibility: "Project status: \(project.status.label)",
                                     pick: { status in Task { await store.setProjectStatus(project, status) } },
                                     icon: { ProjectStatusIcon(status: $0, progress: fraction) }) {
                            HStack(spacing: 6) {
                                ProjectStatusIcon(status: project.status, progress: fraction)
                                Text(project.status.label)
                                    .font(TypeScale.label(12.5))
                                    .foregroundStyle(Theme.ink)
                                Image(systemName: "chevron.down")
                                    .font(.system(size: 8, weight: .bold))
                                    .foregroundStyle(Theme.inkTertiary)
                            }
                            .padding(.horizontal, 10)
                            .frame(height: 24)
                            .background(Theme.control, in: Capsule())
                        }
                        Text("Started \(PlanningFormat.day(project.createdAtMs))").metaText()
                        if let due = nextDue(detail) {
                            let late = due < now
                            Text("Next due \(PlanningFormat.day(due))")
                                .metaText(late ? Theme.live : Theme.inkTertiary)
                                .help(PlanningFormat.date(due))
                        }
                    }
                }
            }
            if let description = project.description, !description.isEmpty {
                Text(description).bodyText(14, Theme.inkSecondary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func nextDue(_ detail: PlanningProjectDetail) -> Int64? {
        (detail.tasks.filter { $0.status.isOpen }.compactMap(\.dueAtMs)
            + detail.milestones.filter { $0.status.isOpen }.compactMap(\.dueAtMs)).min()
    }

    private func numbers(_ detail: PlanningProjectDetail, now: Int64) -> some View {
        let overdue = detail.overdueTaskCount(at: now)
        let milestonesDone = detail.milestones.filter { $0.status == .done }.count
        return HStack(spacing: 10) {
            NumberTile("\(detail.completedTaskCount)", label: "Tasks done")
            NumberTile("\(detail.openTaskCount)", label: "Open")
            NumberTile("\(overdue)", label: "Overdue", tone: overdue > 0 ? Theme.live : Theme.ink)
            NumberTile("\(milestonesDone)", label: "Milestones done", unit: "of \(detail.milestones.count)")
            NumberTile("\(sourceGroups(detail).count)", label: "Sources")
        }
    }

    private func sectionHeader<Action: View>(_ title: String, count: Int,
                                             @ViewBuilder action: () -> Action) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(title).sectionLabel().accessibilityAddTraits(.isHeader)
            if count > 0 { Text("\(count)").metaText().monospacedDigit() }
            Spacer()
            action()
        }
    }

    private func milestones(_ detail: PlanningProjectDetail, now: Int64) -> some View {
        let dated = detail.milestones.compactMap { milestone -> TimelineMilestone? in
            guard let due = milestone.dueAtMs else { return nil }
            return TimelineMilestone(id: milestone.id, title: milestone.title, due: PlanningFormat.moment(due),
                                     status: milestone.status, overdue: milestone.isOverdue(at: now))
        }
        let ordered = detail.milestones.sorted { ($0.dueAtMs ?? .max, $0.createdAtMs) < ($1.dueAtMs ?? .max, $1.createdAtMs) }
        return VStack(alignment: .leading, spacing: 10) {
            sectionHeader("Milestones", count: detail.milestones.count) {
                Button("Add milestone", systemImage: "plus") { creation = .milestone(detail.project.id) }
                    .buttonStyle(.quiet)
                    .disabled(store.mutating)
            }
            Card(material: material) {
                if detail.milestones.isEmpty {
                    CardLine("No milestones yet.")
                } else {
                    if !dated.isEmpty {
                        MilestoneTimeline(milestones: dated, start: PlanningFormat.moment(detail.project.createdAtMs),
                                          now: PlanningFormat.moment(now))
                            .padding(.horizontal, 16)
                            .padding(.top, 14)
                            .padding(.bottom, 12)
                            .overlay(alignment: .bottom) { Hairline().padding(.horizontal, 16) }
                    }
                    ForEach(ordered) { milestone in
                        milestoneRow(milestone, detail: detail, now: now)
                    }
                }
            }
        }
        .padding(.bottom, 28)
    }

    private func milestoneRow(_ milestone: PlanningMilestone, detail: PlanningProjectDetail, now: Int64) -> some View {
        let overdue = milestone.isOverdue(at: now)
        let scoped = detail.tasks.filter { $0.milestoneId == milestone.id && $0.status != .canceled }
        let done = scoped.filter { $0.status == .done }.count
        return DenseRow {
            workStatusPicker(milestone.status, title: milestone.title) { status in
                Task { await store.setMilestoneStatus(milestone, status) }
            } icon: { status in
                MilestoneMark(status: status, overdue: overdue && status == milestone.status)
            }
            Text(milestone.title).bodyText(13.5).lineLimit(1).help(milestone.title)
            Spacer(minLength: 12)
            if !scoped.isEmpty {
                Text("\(done)/\(scoped.count) tasks").metaText().monospacedDigit()
            }
            dueLabel(milestone.dueAtMs, overdue: overdue)
            linkButton(milestone.title) {
                linkSource(to: .milestone, id: milestone.id, title: milestone.title, detail: detail)
            }
        }
    }

    private func tasks(_ detail: PlanningProjectDetail, now: Int64) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            sectionHeader("Tasks", count: detail.tasks.count) {
                Button("Add task", systemImage: "plus") {
                    creation = .task(detail.project.id, detail.milestones)
                }
                .buttonStyle(.quiet)
                .disabled(store.mutating)
            }
            Card(material: material) {
                if detail.tasks.isEmpty { CardLine("No tasks yet.") }
                ForEach(TaskListRow.rows(detail.tasks)) { row in
                    switch row {
                    case let .group(status, count):
                        GroupHeader(title: status.label, count: count) {
                            WorkStatusIcon(status: status)
                        }
                    case let .task(task):
                        taskRow(task, detail: detail, now: now)
                    }
                }
            }
        }
        .padding(.bottom, 28)
    }

    private func taskRow(_ task: PlanningTask, detail: PlanningProjectDetail, now: Int64) -> some View {
        let overdue = task.isOverdue(at: now)
        let milestone = task.milestoneId.flatMap { id in detail.milestones.first { $0.id == id } }
        return DenseRow {
            workStatusPicker(task.status, title: task.title) { status in
                Task { await store.setTaskStatus(task, status) }
            } icon: { status in
                WorkStatusIcon(status: status, overdue: overdue && status == task.status)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(task.title)
                    .bodyText(13.5, task.status == .canceled ? Theme.inkTertiary : Theme.ink)
                    .strikethrough(task.status == .canceled, color: Theme.inkTertiary)
                    .lineLimit(1)
                if let details = task.details, !details.isEmpty {
                    Text(details).metaText().lineLimit(1)
                }
            }
            .padding(.vertical, 8)
            .help(task.details.map { "\(task.title)\n\($0)" } ?? task.title)
            Spacer(minLength: 12)
            if let milestone {
                HStack(spacing: 5) {
                    MilestoneMark(status: milestone.status, size: 9)
                    Text(milestone.title).lineLimit(1)
                }
                .font(TypeScale.label(11.5))
                .foregroundStyle(Theme.inkSecondary)
                .padding(.horizontal, 8)
                .frame(height: 20)
                .frame(maxWidth: 150)
                .background(Theme.inset, in: Capsule())
                .fixedSize()
                .help(milestone.title)
            }
            owner(task)
            dueLabel(task.dueAtMs, overdue: overdue)
            linkButton(task.title) {
                linkSource(to: .task, id: task.id, title: task.title, detail: detail)
            }
        }
    }

    @ViewBuilder
    private func owner(_ task: PlanningTask) -> some View {
        if task.ownerIsSelf {
            Avatar(face: AvatarFace(kind: .me, name: "Me"), size: 20, revision: photoRevision)
                .help("Me")
        } else if let id = task.ownerPersonId, task.ownerAvailable,
                  let person = people.first(where: { $0.id == id }) {
            Button { openPerson(id) } label: {
                Avatar(face: AvatarFace(kind: .person(id), name: person.name), size: 20, revision: photoRevision)
            }
            .buttonStyle(.plain)
            .help(person.name)
            .accessibilityLabel("Open \(person.name)")
        } else if task.ownerPersonId != nil || !task.ownerAvailable {
            Image(systemName: "person.crop.circle.badge.questionmark")
                .font(.system(size: 15))
                .foregroundStyle(Theme.inkTertiary)
                .frame(width: 20, height: 20)
                .help("Person unavailable")
                .accessibilityLabel("Person unavailable")
        } else {
            Avatar(face: AvatarFace(kind: .unassigned, name: "No owner"), size: 20, revision: photoRevision)
                .help("No owner")
        }
    }

    @ViewBuilder
    private func dueLabel(_ due: Int64?, overdue: Bool) -> some View {
        if let due {
            Text(PlanningFormat.day(due))
                .font(TypeScale.label(12))
                .monospacedDigit()
                .foregroundStyle(overdue ? Theme.live : Theme.inkSecondary)
                .lineLimit(1)
                .help(PlanningFormat.date(due))
                .accessibilityLabel(overdue ? "Overdue, was due \(PlanningFormat.date(due))" : "Due \(PlanningFormat.date(due))")
        }
    }

    private func linkButton(_ title: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: "link").font(.system(size: 11, weight: .semibold))
        }
        .buttonStyle(.quiet)
        .help("Link source")
        .accessibilityLabel("Link source to \(title)")
        .disabled(store.mutating || store.detailLoading)
    }

    private func workStatusPicker<Icon: View>(_ current: PlanningWorkStatus, title: String,
                                              change: @escaping (PlanningWorkStatus) -> Void,
                                              @ViewBuilder icon: @escaping (PlanningWorkStatus) -> Icon) -> some View {
        ChoicePicker(PlanningWorkStatus.allCases, current: current, title: \.label,
                     accessibility: "Status of \(title): \(current.label)", pick: change, icon: icon) {
            icon(current).frame(width: 22, height: 22)
        }
        .help(current.label)
    }

    private func evidence(_ detail: PlanningProjectDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            sectionHeader("Sources", count: sourceGroups(detail).count) {
                Button("Link source", systemImage: "link") {
                    linkSource(to: .project, id: detail.project.id, title: detail.project.title, detail: detail)
                }
                .buttonStyle(.quiet)
                .disabled(store.mutating || store.detailLoading)
            }
            Card(material: material) {
                if detail.evidence.isEmpty { CardLine("No linked sources.") }
                ForEach(sourceGroups(detail)) { group in
                    let link = group.link
                    CardRow(action: link.available ? { openEvidence(link) } : nil) {
                        VStack(alignment: .leading, spacing: 5) {
                            Text(link.available ? (link.title ?? sourceLabel(link.sourceKind)) : "Source unavailable")
                                .bodyText(14, link.available ? Theme.ink : Theme.inkTertiary)
                                .fixedSize(horizontal: false, vertical: true)
                            if !group.targets.isEmpty { Text(group.targets.joined(separator: " · ")).metaText() }
                            if let at = link.atMs, link.available {
                                Text(PlanningFormat.passage(at)).metaText()
                            }
                        }
                    } trailing: {
                        if link.available {
                            Image(systemName: "arrow.up.right")
                                .font(.system(size: 11))
                                .foregroundStyle(Theme.inkTertiary)
                                .accessibilityHidden(true)
                        }
                    }
                }
            }
        }
    }

    private func linkSource(to kind: PlanningTargetKind, id: String, title: String, detail: PlanningProjectDetail) {
        sourceLink = PlanningLinkRequest(detail: detail, target: PlanningLinkTarget(kind: kind, targetId: id, title: title))
    }

    private func sourceLabel(_ kind: PlanningSourceKind) -> String {
        switch kind {
        case .meeting: "Meeting"
        case .note: "Note"
        case .loop: "Commitment"
        }
    }

    private func targetTitle(_ link: PlanningEvidenceLink, detail: PlanningProjectDetail) -> String? {
        switch link.targetKind {
        case .project: nil
        case .milestone: detail.milestones.first { $0.id == link.targetId }?.title
        case .task: detail.tasks.first { $0.id == link.targetId }?.title
        }
    }

    private func sourceGroups(_ detail: PlanningProjectDetail) -> [PlanningSourceGroup] {
        var groups: [PlanningSourceGroup] = []
        var positions: [String: Int] = [:]
        for link in detail.evidence {
            let key = "\(link.sourceKind):\(link.sourceId):\(link.atMs.map(String.init) ?? ""):\(link.meetingNoteId ?? ""):\(link.available)"
            let target = targetTitle(link, detail: detail)
            if let position = positions[key] {
                if let target, !groups[position].targets.contains(target) { groups[position].targets.append(target) }
            } else {
                positions[key] = groups.count
                groups.append(PlanningSourceGroup(link: link, targets: target.map { [$0] } ?? []))
            }
        }
        return groups
    }
}

private enum ProjectListRow: Identifiable {
    case group(PlanningProjectStatus, Int)
    case project(PlanningProjectSummary)

    static func projectId(_ id: String) -> String { "project:\(id)" }

    var id: String {
        switch self {
        case let .group(status, _): "group:\(status.rawValue)"
        case let .project(summary): Self.projectId(summary.id)
        }
    }

    var projectId: String? {
        if case let .project(summary) = self { summary.id } else { nil }
    }
}

private enum TaskListRow: Identifiable {
    case group(PlanningWorkStatus, Int)
    case task(PlanningTask)

    static func rows(_ tasks: [PlanningTask]) -> [TaskListRow] {
        let order: [PlanningWorkStatus] = [.inProgress, .notStarted, .done, .canceled]
        return order.flatMap { status in
            let members = tasks.filter { $0.status == status }
            return members.isEmpty ? [] : [.group(status, members.count)] + members.map(TaskListRow.task)
        }
    }

    var id: String {
        switch self {
        case let .group(status, _): "group:\(status.rawValue)"
        case let .task(task): "task:\(task.id)"
        }
    }
}

private extension PlanningProjectDetail {
    /// Scope rises when a task is added and falls when one is canceled; done
    /// rises when a task is finished. A task's last change stands in for the
    /// moment it was finished or canceled.
    func burnUp(until now: Date) -> BurnUpSeries? {
        guard !tasks.isEmpty else { return nil }
        var scope: [(Int64, Int)] = []
        var done: [(Int64, Int)] = []
        for task in tasks {
            scope.append((task.createdAtMs, 1))
            switch task.status {
            case .canceled: scope.append((max(task.updatedAtMs, task.createdAtMs), -1))
            case .done: done.append((max(task.updatedAtMs, task.createdAtMs), 1))
            case .notStarted, .inProgress: break
            }
        }
        let first = min(project.createdAtMs, scope.map(\.0).min() ?? project.createdAtMs)
        let start = PlanningFormat.moment(first)
        return BurnUpSeries(start: start, end: max(now, start.addingTimeInterval(86_400)),
                            scope: Self.running(scope), done: Self.running(done))
    }

    private static func running(_ changes: [(Int64, Int)]) -> [BurnUpSeries.Point] {
        var total = 0
        return changes.sorted { $0.0 < $1.0 }.map { change in
            total += change.1
            return BurnUpSeries.Point(at: PlanningFormat.moment(change.0), value: total)
        }
    }
}

/// One line of a dense list. The highlight follows the pointer unless the
/// list keeps its own cursor.
private struct DenseRow<Content: View>: View {
    var highlighted: Bool?
    var action: (() -> Void)?
    var hover: ((Bool) -> Void)?
    @ViewBuilder let content: () -> Content
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 10) { content() }
            .padding(.horizontal, 16)
            .frame(minHeight: 36)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background((highlighted ?? (hovering && action != nil)) ? Theme.wash : .clear)
            .overlay(alignment: .bottom) { Hairline().padding(.leading, 16) }
            .contentShape(Rectangle())
            .onHover { inside in
                hovering = inside
                hover?(inside)
            }
            .onTapGesture { action?() }
    }
}

private struct GroupHeader<Icon: View>: View {
    let title: String
    let count: Int
    var collapsed: Bool?
    @ViewBuilder let icon: () -> Icon
    var toggle: (() -> Void)?

    var body: some View {
        HStack(spacing: 8) {
            if let collapsed {
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(Theme.inkTertiary)
                    .rotationEffect(.degrees(collapsed ? 0 : 90))
                    .frame(width: 10)
            }
            icon()
            Text(title)
                .font(TypeScale.label(12.5).weight(.medium))
                .foregroundStyle(Theme.ink)
            Text("\(count)").metaText().monospacedDigit()
            Spacer()
        }
        .padding(.horizontal, 16)
        .frame(height: 30)
        .background(Theme.inset.opacity(0.55))
        .overlay(alignment: .bottom) { Hairline() }
        .contentShape(Rectangle())
        .onTapGesture { toggle?() }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(toggle == nil ? .isHeader : [.isHeader, .isButton])
        .accessibilityAction { toggle?() }
    }
}

private struct BoardColumn<Cards: View>: View {
    let status: PlanningProjectStatus
    let count: Int
    @ViewBuilder let cards: () -> Cards
    let drop: (String) -> Bool
    @State private var targeted = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                ProjectStatusIcon(status: status, progress: 0.5)
                Text(status.label)
                    .font(TypeScale.label(12.5).weight(.medium))
                    .foregroundStyle(Theme.ink)
                Text("\(count)").metaText().monospacedDigit()
                Spacer()
            }
            .padding(.horizontal, 6)
            .frame(height: 26)
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
            cards()
            if count == 0 {
                Text("Drop a project here")
                    .metaText()
                    .frame(maxWidth: .infinity, minHeight: 56)
                    .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Theme.border, style: StrokeStyle(lineWidth: 1, dash: [4, 4])))
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, minHeight: 180, alignment: .top)
        .background(targeted ? Theme.accentSoft : Theme.inset.opacity(0.55),
                    in: RoundedRectangle(cornerRadius: Theme.radiusCard))
        .dropDestination(for: String.self) { ids, _ in
            ids.first.map(drop) ?? false
        } isTargeted: { targeted = $0 }
    }
}

private struct BoardCard: View {
    let summary: PlanningProjectSummary
    let faces: [AvatarFace]
    let revision: UInt64
    let now: Int64
    let open: () -> Void
    @State private var hovering = false

    var body: some View {
        let total = summary.openTaskCount + summary.completedTaskCount
        let fraction = total > 0 ? Double(summary.completedTaskCount) / Double(total) : 0
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 8) {
                ProjectStatusIcon(status: summary.project.status, progress: fraction)
                    .padding(.top, 2)
                Text(summary.project.title)
                    .font(TypeScale.body(13.5).weight(.medium))
                    .foregroundStyle(Theme.ink)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
            }
            HStack(spacing: 8) {
                ProjectProgressRing(done: summary.completedTaskCount, total: total)
                Text(total == 0 ? "No tasks" : "\(summary.completedTaskCount)/\(total)")
                    .metaText()
                    .monospacedDigit()
                Spacer(minLength: 6)
                if let due = summary.nextDueAtMs {
                    Text(PlanningFormat.day(due))
                        .font(TypeScale.label(12))
                        .monospacedDigit()
                        .foregroundStyle(due < now ? Theme.live : Theme.inkSecondary)
                        .help(PlanningFormat.date(due))
                }
                if !faces.isEmpty { AvatarStack(faces: faces, size: 18, revision: revision) }
            }
        }
        .padding(12)
        .background(hovering ? Theme.wash : Theme.surface, in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Theme.border, lineWidth: 1))
        .contentShape(RoundedRectangle(cornerRadius: 10))
        .onHover { hovering = $0 }
        .onTapGesture(perform: open)
        .draggable(summary.id) {
            Text(summary.project.title)
                .font(TypeScale.body(13).weight(.medium))
                .foregroundStyle(Theme.ink)
                .padding(.horizontal, 12)
                .frame(height: 30)
                .background(Theme.surface, in: RoundedRectangle(cornerRadius: 8))
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityAction { open() }
    }
}

private struct PlanningSourceGroup: Identifiable {
    let link: PlanningEvidenceLink
    var targets: [String]
    var id: String { link.id }
}

private struct ProjectProgressRing: View {
    let done: Int64
    let total: Int64

    var body: some View {
        let fraction = total > 0 ? Double(done) / Double(total) : 0
        HStack(spacing: 6) {
            ProgressRing(fraction: fraction, size: 14, lineWidth: 2.25)
            Text(ProgressRing.percent(fraction))
                .font(TypeScale.label(12))
                .monospacedDigit()
                .foregroundStyle(Theme.inkSecondary)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(done) of \(total) tasks done")
    }
}

private struct PlanningLinkTarget: Identifiable {
    let kind: PlanningTargetKind
    let targetId: String
    let title: String
    var id: String { "\(kind.rawValue):\(targetId)" }
    var label: String {
        switch kind {
        case .project: "Project · \(title)"
        case .milestone: "Milestone · \(title)"
        case .task: "Task · \(title)"
        }
    }
}

private struct PlanningLinkRequest: Identifiable {
    let detail: PlanningProjectDetail
    let target: PlanningLinkTarget
    var id: String { target.id }
    var targets: [PlanningLinkTarget] {
        [PlanningLinkTarget(kind: .project, targetId: detail.project.id, title: detail.project.title)]
            + detail.milestones.map { PlanningLinkTarget(kind: .milestone, targetId: $0.id, title: $0.title) }
            + detail.tasks.map { PlanningLinkTarget(kind: .task, targetId: $0.id, title: $0.title) }
    }
}

private struct PlanningSourceSearch: Equatable {
    let kind: PlanningSourceList
    let query: String
    let revision: Int
    let meetingId: String?
}

private struct PlanningLinkSourceSheet: View {
    let request: PlanningLinkRequest
    let store: ProjectsStore
    let material: AppearanceMaterial
    @Environment(\.dismiss) private var dismiss
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var targetId: String
    @State private var kind: PlanningSourceList = .meeting
    @State private var query = ""
    @State private var loopMeeting: PlanningSourceChoice?
    @State private var sources: [PlanningSourceChoice] = []
    @State private var selected: PlanningSourceChoice?
    @State private var nextCursor: Int64?
    @State private var lookup = UUID()
    @State private var loading = true
    @State private var loadError: String?
    @State private var saveError: String?
    @State private var saving = false
    @State private var operationId = UUID().uuidString
    @FocusState private var searchFocused: Bool

    init(request: PlanningLinkRequest, store: ProjectsStore, material: AppearanceMaterial) {
        self.request = request
        self.store = store
        self.material = material
        _targetId = State(initialValue: request.target.id)
    }

    private var search: PlanningSourceSearch {
        PlanningSourceSearch(kind: kind, query: query.trimmingCharacters(in: .whitespacesAndNewlines),
                             revision: store.sourcesRevision, meetingId: loopMeeting?.sourceId)
    }

    private var target: PlanningLinkTarget? { request.targets.first { $0.id == targetId } }

    private var targetSelection: Binding<String> {
        Binding(get: { targetId }, set: { value in
            if targetId != value { selectionChanged() }
            targetId = value
        })
    }

    private var sourceSelection: Binding<PlanningSourceChoice?> {
        Binding(get: { selected }, set: { chooseSource($0) })
    }

    private var choosingLoopMeeting: Bool { kind == .loop && loopMeeting == nil }

    private var selectedLabel: String {
        switch selected?.kind {
        case .meeting: "Selected meeting"
        case .note: "Selected note"
        case .loop: "Selected commitment"
        case nil: "Selected source"
        }
    }

    private var emptyMessage: String {
        if choosingLoopMeeting { return search.query.isEmpty ? "No saved meetings." : "No matching meetings." }
        if kind == .loop { return search.query.isEmpty ? "No commitments." : "No matching commitments." }
        return search.query.isEmpty ? "No saved \(kind.label.lowercased())." : "No matching sources."
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Link source").titleText().accessibilityAddTraits(.isHeader)
            VStack(alignment: .leading, spacing: 16) {
                Picker("Link to", selection: targetSelection) {
                    ForEach(request.targets) { Text($0.label).tag($0.id) }
                }
                PlannerNavigationSurface(material: material) {
                    HStack(spacing: 12) {
                        Picker("Source type", selection: $kind) {
                            ForEach(PlanningSourceList.allCases) { Text($0.label).tag($0) }
                        }
                        .frame(width: 130)
                        SearchField(prompt: choosingLoopMeeting ? "Search meetings" : "Search sources", text: $query)
                            .accessibilityLabel(choosingLoopMeeting ? "Search saved meetings" : "Search saved \(kind.label.lowercased())")
                            .focused($searchFocused)
                    }
                }
                if let loopMeeting, kind == .loop {
                    HStack(alignment: .top, spacing: 12) {
                        Button("Meetings", systemImage: "chevron.left") {
                            self.loopMeeting = nil
                            chooseSource(nil)
                            query = ""
                        }
                        .buttonStyle(.quiet)
                        Text(loopMeeting.title).bodyText(13, Theme.inkSecondary)
                            .lineLimit(2)
                    }
                }
                sourceList
                if let loadError {
                    PlannerFailure(message: loadError) { Task { await loadSources(reset: nextCursor == nil) } }
                }
                if let selected {
                    VStack(alignment: .leading, spacing: 5) {
                        Text(selectedLabel).sectionLabel()
                        Text(selected.title).bodyText()
                            .lineLimit(3)
                    }
                    .accessibilityElement(children: .combine)
                }
                if let saveError { ErrorNote(saveError) }
            }
            .disabled(saving)
            HStack {
                Button("Cancel") { dismiss() }
                    .buttonStyle(.quiet)
                    .keyboardShortcut(.cancelAction)
                    .disabled(saving)
                Spacer()
                if saving { ProgressView().controlSize(.small).accessibilityLabel("Linking source") }
                Button(saveError == nil ? "Link source" : "Retry") { Task { await save() } }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
                    .disabled(saving || store.mutating || selected == nil || target == nil)
            }
        }
        .padding(24)
        .frame(width: 580)
        .background(Theme.page)
        .interactiveDismissDisabled(saving)
        .onAppear { searchFocused = true }
        .task(id: search) { await loadSources(reset: true) }
        .onChange(of: kind) { _, _ in
            loopMeeting = nil
            chooseSource(nil)
            query = ""
        }
        .onChange(of: store.sourcesRevision) { _, _ in
            if selected != nil {
                chooseSource(nil)
            }
        }
        .transaction {
            if reduceMotion { $0.animation = nil; $0.disablesAnimations = true }
        }
    }

    private var sourceList: some View {
        VStack(spacing: 10) {
            Group {
                if choosingLoopMeeting {
                    List(sources) { source in
                        Button {
                            loopMeeting = source
                            chooseSource(nil)
                            query = ""
                        } label: {
                            HStack(spacing: 12) {
                                sourceRow(source)
                                Spacer()
                                Image(systemName: "chevron.right")
                                    .foregroundStyle(Theme.inkTertiary)
                                    .accessibilityHidden(true)
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Choose meeting \(source.title)")
                    }
                } else {
                    List(selection: sourceSelection) {
                        ForEach(sources) { source in sourceRow(source).tag(source) }
                    }
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .accessibilityLabel("Saved sources")
            .overlay {
                if sources.isEmpty {
                    if loading {
                        ProgressView("Loading sources…").controlSize(.small)
                    } else if loadError == nil {
                        Text(emptyMessage).metaText()
                    }
                }
            }
            if nextCursor != nil {
                HStack {
                    if loading { ProgressView().controlSize(.small).accessibilityLabel("Loading more sources") }
                    Button("Load more") { Task { await loadSources(reset: false) } }
                        .buttonStyle(.compact)
                        .disabled(loading)
                }
            }
        }
        .frame(maxWidth: .infinity, minHeight: 180, idealHeight: 240, maxHeight: 320)
    }

    private func sourceRow(_ source: PlanningSourceChoice) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(source.title).bodyText()
                .lineLimit(3)
            if let date = source.updatedAtMs {
                Text(PlanningFormat.date(date)).metaText(Theme.inkSecondary)
            }
        }
        .padding(.vertical, 5)
        .accessibilityElement(children: .combine)
    }

    private func chooseSource(_ value: PlanningSourceChoice?) {
        if selected?.id != value?.id { selectionChanged() }
        selected = value
    }

    private func selectionChanged() {
        operationId = UUID().uuidString
        saveError = nil
    }

    private func loadSources(reset: Bool) async {
        let searched = search
        if reset {
            lookup = UUID()
            sources = []
            nextCursor = nil
        } else if loading {
            return
        }
        let current = lookup
        let cursor = reset ? nil : nextCursor
        loading = true
        loadError = nil
        defer { if lookup == current, search == searched { loading = false } }
        do {
            if reset, !searched.query.isEmpty { try await Task.sleep(for: .milliseconds(200)) }
            let page = try await store.sourceCandidates(searched.kind, query: searched.query, after: cursor,
                                                        meetingId: searched.meetingId)
            guard !Task.isCancelled, lookup == current, search == searched else { return }
            if reset {
                sources = page.entries
            } else {
                let existing = Set(sources.map(\.id))
                sources += page.entries.filter { !existing.contains($0.id) }
            }
            nextCursor = page.nextCursor
        } catch is CancellationError {
        } catch {
            guard !Task.isCancelled, lookup == current, search == searched else { return }
            loadError = error.localizedDescription
        }
    }

    private func save() async {
        guard !saving, let selected, let target else { return }
        saving = true
        saveError = nil
        defer { saving = false }
        do {
            try await store.linkSource(targetKind: target.kind, targetId: target.targetId,
                                       sourceKind: selected.kind, sourceId: selected.sourceId,
                                       atMs: nil, operationId: operationId)
            dismiss()
        } catch {
            saveError = error.localizedDescription
        }
    }
}

private enum PlanningCreation: Identifiable {
    case project
    case milestone(String)
    case task(String, [PlanningMilestone])
    var id: String {
        switch self {
        case .project: "project"
        case let .milestone(id): "milestone:\(id)"
        case let .task(id, _): "task:\(id)"
        }
    }
    var title: String {
        switch self {
        case .project: "New project"
        case .milestone: "New milestone"
        case .task: "New task"
        }
    }
}

private struct PlanningDraft: Equatable {
    var title = ""
    var body = ""
    var dueAt: Date?
    var milestoneId: String?
    var owner: PlanningOwner = .unassigned
}

private struct PlanningCreateSheet: View {
    let target: PlanningCreation
    let store: ProjectsStore
    let people: [PlanningPersonChoice]
    let peopleUnavailable: Bool
    @Environment(\.dismiss) private var dismiss
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var draft = PlanningDraft()
    @State private var operationId = UUID().uuidString
    @State private var saving = false
    @State private var error: String?
    @FocusState private var titleFocused: Bool

    private var ownerAvailable: Bool {
        if case let .person(id) = draft.owner { return people.contains { $0.id == id } && !peopleUnavailable }
        return true
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text(target.title).titleText().accessibilityAddTraits(.isHeader)
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    VStack(alignment: .leading, spacing: 7) {
                        Text("Title").sectionLabel()
                        InputField(prompt: "Title", text: $draft.title)
                            .accessibilityLabel("Title")
                            .focused($titleFocused)
                    }
                    switch target {
                    case .project:
                        bodyField("Description")
                    case .milestone:
                        PlanningDateField(date: $draft.dueAt)
                    case let .task(_, milestones):
                        bodyField("Details")
                        if !milestones.isEmpty {
                            Picker("Milestone", selection: $draft.milestoneId) {
                                Text("None").tag(String?.none)
                                ForEach(milestones) { Text($0.title).tag(Optional($0.id)) }
                            }
                        }
                        Picker("Owner", selection: $draft.owner) {
                            Text("Unassigned").tag(PlanningOwner.unassigned)
                            Text("Me").tag(PlanningOwner.myself)
                            if !peopleUnavailable {
                                ForEach(people) { Text($0.name).tag(PlanningOwner.person($0.id)) }
                            }
                            if !ownerAvailable { Text("Person unavailable").tag(draft.owner) }
                        }
                        if peopleUnavailable { Text("People unavailable.").metaText() }
                        PlanningDateField(date: $draft.dueAt)
                    }
                    if !ownerAvailable { Text("Choose an available owner.").bodyText(13, Theme.live) }
                    if let error { ErrorNote(error) }
                }
                .padding(2)
            }
            .disabled(saving)
            HStack {
                Button("Cancel") { dismiss() }
                    .buttonStyle(.quiet)
                    .keyboardShortcut(.cancelAction)
                    .disabled(saving)
                Spacer()
                if saving { ProgressView().controlSize(.small).accessibilityLabel("Saving") }
                Button("Create") { Task { await save() } }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
                    .disabled(saving || store.mutating || draft.title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !ownerAvailable)
            }
        }
        .padding(24)
        .frame(width: 460, height: sheetHeight)
        .background(Theme.page)
        .interactiveDismissDisabled(saving)
        .onChange(of: draft) { _, _ in operationId = UUID().uuidString; error = nil }
        .onAppear { titleFocused = true }
        .transaction {
            if reduceMotion { $0.animation = nil; $0.disablesAnimations = true }
        }
    }

    private var sheetHeight: CGFloat {
        switch target {
        case .project: 350
        case .milestone: 300
        case .task: 500
        }
    }

    private func bodyField(_ label: String) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(label).sectionLabel()
            TextField(label, text: $draft.body, axis: .vertical)
                .lineLimit(3...6)
                .textFieldStyle(.roundedBorder)
                .font(TypeScale.body())
                .accessibilityLabel(label)
        }
    }

    private func save() async {
        guard !saving, ownerAvailable else { return }
        saving = true
        error = nil
        defer { saving = false }
        let title = draft.title.trimmingCharacters(in: .whitespacesAndNewlines)
        let body = draft.body.trimmingCharacters(in: .whitespacesAndNewlines)
        do {
            switch target {
            case .project:
                _ = try await store.createProject(title: title, description: body.isEmpty ? nil : body, operationId: operationId)
            case let .milestone(projectId):
                try await store.createMilestone(projectId: projectId, title: title, dueAt: draft.dueAt, operationId: operationId)
            case let .task(projectId, _):
                try await store.createTask(projectId: projectId, milestoneId: draft.milestoneId, title: title,
                                           details: body.isEmpty ? nil : body, dueAt: draft.dueAt,
                                           owner: draft.owner, operationId: operationId)
            }
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }
    }
}

private struct PlanningDateField: View {
    @Binding var date: Date?
    @State private var choosing = false
    @State private var candidate = Date()

    var body: some View {
        HStack {
            Text("Due date").sectionLabel()
            Spacer()
            Button {
                candidate = date ?? Date()
                choosing = true
            } label: {
                if let date { Text(date.formatted(date: .abbreviated, time: .shortened)) }
                else { Text("Set date") }
            }
            .buttonStyle(.compact)
            .popover(isPresented: $choosing) {
                VStack(alignment: .leading, spacing: 16) {
                    DatePicker("Date", selection: $candidate, displayedComponents: .date)
                        .datePickerStyle(.graphical)
                    DatePicker("Time", selection: $candidate, displayedComponents: .hourAndMinute)
                    HStack {
                        Button("Cancel") { choosing = false }.buttonStyle(.quiet)
                        Spacer()
                        Button("Use date") { date = candidate; choosing = false }.buttonStyle(.primary)
                    }
                }
                .padding(20)
                .frame(width: 320)
            }
            if date != nil {
                Button { date = nil } label: { Image(systemName: "xmark") }
                    .buttonStyle(.quiet)
                    .accessibilityLabel("Remove due date")
                    .help("Remove due date")
            }
        }
    }
}

struct PlannerFailure: View {
    let message: String
    var retryTitle = "Retry"
    let retry: () -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(message).bodyText(13, Theme.live).textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            Button(retryTitle, action: retry).buttonStyle(.compact)
        }
        .accessibilityElement(children: .contain)
    }
}

struct PlannerLoadingRows: View {
    let label: String
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(label + "…").metaText()
            Card {
                ForEach(0..<3) { _ in
                    VStack(alignment: .leading, spacing: 8) {
                        RoundedRectangle(cornerRadius: 3).fill(Theme.control).frame(width: 180, height: 14)
                        RoundedRectangle(cornerRadius: 3).fill(Theme.control).frame(width: 120, height: 12)
                    }
                    .padding(16)
                    .accessibilityHidden(true)
                }
            }
        }
    }
}

struct PlannerNavigationSurface<Content: View>: View {
    let material: AppearanceMaterial
    @ViewBuilder let content: () -> Content
    var body: some View {
        if material == .glass {
            if #available(macOS 26.0, *) {
                content().padding(8)
                    .glassEffect(.regular, in: RoundedRectangle(cornerRadius: Theme.radiusControl + 8, style: .continuous))
            } else {
                content().padding(8)
                    .background(.regularMaterial, in: RoundedRectangle(cornerRadius: Theme.radiusControl + 8, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: Theme.radiusControl + 8, style: .continuous)
                            .strokeBorder(Theme.border, lineWidth: 1)
                            .allowsHitTesting(false)
                    }
            }
        } else {
            content()
        }
    }
}

enum PlanningFormat {
    static func moment(_ milliseconds: Int64) -> Date {
        Date(timeIntervalSince1970: Double(milliseconds) / 1000)
    }
    static func date(_ milliseconds: Int64) -> String {
        moment(milliseconds).formatted(date: .abbreviated, time: .shortened)
    }
    /// "Nov 3", with the year only when it is not this one.
    static func day(_ milliseconds: Int64) -> String {
        let date = moment(milliseconds)
        return Calendar.current.isDate(date, equalTo: .now, toGranularity: .year)
            ? date.formatted(.dateTime.month(.abbreviated).day())
            : date.formatted(.dateTime.month(.abbreviated).day().year())
    }
    static func passage(_ milliseconds: Int64) -> String {
        let seconds = max(0, milliseconds / 1000)
        return "\(seconds / 60):\(String(format: "%02lld", seconds % 60))"
    }
}
