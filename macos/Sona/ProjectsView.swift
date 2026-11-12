import SwiftUI

enum ProjectsLayout: String {
    case list, board, timeline

    var choice: ProjectsViewChoice {
        switch self {
        case .list: .list
        case .board: .board
        case .timeline: .timeline
        }
    }
}

/// The ways to look at planned work. Calendar is its own place, so choosing
/// it leaves the projects list.
enum ProjectsViewChoice: Hashable {
    case list, board, timeline, calendar

    static var items: [SwitchItem<ProjectsViewChoice>] {
        [
            SwitchItem(id: .list, title: "List", icon: "list.bullet"),
            SwitchItem(id: .board, title: "Board", icon: "rectangle.split.3x1"),
            SwitchItem(id: .timeline, title: "Timeline", icon: "chart.bar.xaxis"),
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
    let discussProject: (PlanningProject) -> Void
    let planWithSona: () -> Void
    let showCalendar: () -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(AppModel.self) private var model
    @State private var pageTab = ProjectPageTab.overview
    @State private var showBin = false
    @AppStorage("projects.layout") private var layout = ProjectsLayout.list
    @State private var query = ""
    @State private var creation: PlanningCreation?
    @State private var sourceLink: PlanningLinkRequest?
    @State private var reviewProject: PlanningProject?
    @State private var cursor: String?
    @State private var collapsed: Set<PlanningProjectStatus> = []
    @State private var ownerFilter: String?
    @State private var peeking = false
    @State private var editingMilestone: PlanningMilestone?
    @State private var discussedMilestone: PlanningMilestone?
    @State private var titleDraft: String?
    @State private var convertingMilestone: PlanningMilestone?
    @State private var deletingMilestone: PlanningMilestone?
    @State private var templatingProject: PlanningProject?
    @State private var pickingUpdateDay = false
    @FocusState private var keysFocused: Bool

    var body: some View {
        Group {
            if let id = store.selectedProjectId {
                projectPage(id)
                    .transition(.opacity)
            } else {
                Page { projectList }
                    .overlay(alignment: .trailing) {
                        if peeking, layout == .list, let cursor, let detail = store.details[cursor] {
                            ProjectPeek(store: store, detail: detail, people: people, labels: model.work.labels) {
                                peeking = false
                            }
                            .transition(reduceMotion ? .opacity : .move(edge: .trailing).combined(with: .opacity))
                        }
                    }
                    .animation(Motion.regular, value: peeking)
                    .transition(.opacity)
            }
        }
        .animation(Motion.settle(reduced: reduceMotion), value: store.selectedProjectId)
        .task { await store.start() }
        .task(id: store.selectedProjectId) {
            if let id = store.selectedProjectId { await store.loadDetail(id) }
        }
        .onChange(of: store.selectedProjectId) { _, _ in
            ownerFilter = nil
            pageTab = .overview
            peeking = false
            titleDraft = nil
            model.workFocus.open(nil, in: .projects)
        }
        .overlay(alignment: .bottomLeading) { ProjectBinToast(store: store) }
        .onChange(of: store.detail?.pulse.owners) { _, owners in
            if let id = ownerFilter, !(owners ?? []).contains(where: { $0.personId == id }) { ownerFilter = nil }
        }
        .sheet(item: $creation) { target in
            PlanningCreateSheet(target: target, store: store, people: people, peopleUnavailable: peopleUnavailable)
        }
        .sheet(item: $sourceLink) { request in
            PlanningLinkSourceSheet(request: request, store: store, material: material)
        }
        .sheet(item: $editingMilestone) { milestone in
            MilestoneEditSheet(milestone: milestone, store: store)
        }
        .sheet(item: $discussedMilestone) { milestone in
            MilestoneCommentsSheet(milestone: milestone, threads: store.comments)
        }
        .sheet(item: $templatingProject) { project in
            ProjectTemplateSaveSheet(project: project, store: store)
        }
        .sheet(item: $reviewProject) { project in
            PlannerReviewView(project: project, store: store, material: material) { link in
                reviewProject = nil
                openEvidence(link)
            }
        }
        .alert("Convert to a project?", isPresented: Binding(get: { convertingMilestone != nil },
                                                             set: { if !$0 { convertingMilestone = nil } }),
               presenting: convertingMilestone) { milestone in
            Button("Convert") { Task { await store.convertMilestone(milestone) } }
            Button("Cancel", role: .cancel) {}
        } message: { milestone in
            Text("“\(milestone.title)” becomes a project and takes its tasks with it.")
        }
        .alert("Delete this milestone?", isPresented: Binding(get: { deletingMilestone != nil },
                                                              set: { if !$0 { deletingMilestone = nil } }),
               presenting: deletingMilestone) { milestone in
            Button("Delete", role: .destructive) { Task { await store.deleteMilestone(milestone) } }
            Button("Cancel", role: .cancel) {}
        } message: { milestone in
            Text("“\(milestone.title)” and its comments are deleted for good. Its tasks stay in the project.")
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
        let cutoff = filter.updateBefore?.cutoffMs(now: .now)
        return (store.projects ?? []).filter { summary in
            if let status = filter.status, summary.project.status != status { return false }
            if filter.overdueOnly, (store.details[summary.id]?.overdueTaskCount(at: now) ?? 0) == 0 { return false }
            if filter.mineOnly, store.details[summary.id]?.hasMyOpenWork != true { return false }
            if let cutoff, let latest = summary.latestUpdateAtMs, latest >= cutoff { return false }
            if let link = filter.link, !link.keeps(summary.id, store.dependencies) { return false }
            if let initiative = filter.initiative, !initiative.keeps(store.initiative(of: summary.id)?.id) { return false }
            guard !search.isEmpty else { return true }
            return summary.project.title.localizedCaseInsensitiveContains(search)
                || (summary.project.description?.localizedCaseInsensitiveContains(search) ?? false)
        }
    }

    // MARK: List

    private var projectList: some View {
        VStack(alignment: .leading, spacing: 0) {
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
                Card {
                    CardLine(store.projects?.isEmpty == true ? "No projects yet." : "No matching projects.")
                }
            } else if layout == .board {
                board
            } else if layout == .timeline {
                ProjectsTimeline(store: store, projects: visibleProjects)
            } else {
                ScrollViewReader { proxy in list(proxy) }
            }
            if showBin {
                ProjectBinList(store: store).padding(.top, 28)
            }
        }
        .pageBar {
            headerTitle
        } trailing: {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 10) { headerControls(compact: false) }
                HStack(spacing: 10) { headerControls(compact: true) }
            }
            .popover(isPresented: $pickingUpdateDay, arrowEdge: .bottom) {
                ProjectDayPicker(start: .now) { day in
                    store.filter.updateBefore = .day(day)
                    pickingUpdateDay = false
                }
            }
        }
    }

    private var headerTitle: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            PageHeading(title: "Projects")
            if let total = store.projects?.count, total > 0 {
                let filtered = store.filter.isActive || !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                Text(filtered ? "\(visibleProjects.count) of \(total)" : "\(total)").metaText().monospacedDigit()
            }
        }
    }

    /// A narrow window keeps "Plan with Sona" as its icon.
    @ViewBuilder
    private func headerControls(compact: Bool) -> some View {
        SearchField(prompt: "Search", text: $query)
            .frame(width: 160)
            .accessibilityLabel("Search projects")
        filterMenu
        SegmentedSwitch(items: ProjectsViewChoice.items, selection: layout.choice,
                        iconOnly: true) { choice in
            switch choice {
            case .list: layout = .list
            case .board: layout = .board
            case .timeline: layout = .timeline
            case .calendar: showCalendar()
            }
        }
        if compact {
            Button { planWithSona() } label: {
                Image(systemName: "bubble.left")
            }
            .buttonStyle(.icon)
            .help("Plan with Sona")
            .accessibilityLabel("Plan with Sona")
        } else {
            Button("Plan with Sona", systemImage: "bubble.left") { planWithSona() }
                .buttonStyle(.secondary)
        }
        Button { showBin.toggle() } label: { Image(systemName: "trash") }
            .buttonStyle(.icon)
            .background(showBin ? Theme.accentSoft : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .help(showBin ? "Hide recently deleted" : "Recently deleted")
            .accessibilityLabel("Recently deleted")
            .accessibilityAddTraits(showBin ? .isSelected : [])
        Button("New project", systemImage: "plus") { creation = .project }
            .buttonStyle(.primary)
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
            Section("Latest update before") {
                Picker("Latest update before", selection: Binding(get: { store.filter.updateBefore },
                                                                  set: { store.filter.updateBefore = $0 })) {
                    Text("Any time").tag(PlanningUpdateBefore?.none)
                    ForEach(PlanningUpdateBefore.spans, id: \.self) { span in
                        Text(span.label).tag(PlanningUpdateBefore?.some(span))
                    }
                    if let picked = store.filter.updateBefore, !PlanningUpdateBefore.spans.contains(picked) {
                        Text(picked.label).tag(PlanningUpdateBefore?.some(picked))
                    }
                }
                .pickerStyle(.inline)
                .labelsHidden()
                Button("Pick a day…") { pickingUpdateDay = true }
            }
            Section("Dependencies") {
                Picker("Dependencies", selection: Binding(get: { store.filter.link }, set: { store.filter.link = $0 })) {
                    Text("Any").tag(PlanningLinkFilter?.none)
                    ForEach(PlanningLinkFilter.allCases) { link in
                        Text(link.label).tag(PlanningLinkFilter?.some(link))
                    }
                }
                .pickerStyle(.inline)
                .labelsHidden()
            }
            if !store.initiatives.isEmpty {
                Section("Initiative") {
                    Picker("Initiative", selection: Binding(get: { store.filter.initiative },
                                                            set: { store.filter.initiative = $0 })) {
                        Text("Any").tag(PlanningInitiativeFilter?.none)
                        Text("No initiative").tag(PlanningInitiativeFilter?.some(.outside))
                        ForEach(store.initiatives) { initiative in
                            Text(initiative.name).tag(PlanningInitiativeFilter?.some(.within(initiative.id)))
                        }
                    }
                    .pickerStyle(.inline)
                    .labelsHidden()
                }
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
        .frame(width: Theme.controlHeight, height: Theme.controlHeight)
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
            if let before = store.filter.updateBefore {
                filterChip("Latest update before \(before.label)") { store.filter.updateBefore = nil }
            }
            if let link = store.filter.link {
                filterChip(link.label) { store.filter.link = nil }
            }
            if let initiative = store.filter.initiative {
                filterChip(initiativeLabel(initiative)) { store.filter.initiative = nil }
            }
        }
    }

    private func initiativeLabel(_ filter: PlanningInitiativeFilter) -> String {
        switch filter {
        case .outside: "No initiative"
        case .within(let id): store.initiatives.first { $0.id == id }?.name ?? "Initiative"
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
            .frame(height: Theme.compactHeight)
            .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
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
        return Card {
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
        .onKeyPress(characters: CharacterSet(charactersIn: "j"), phases: .down) { press in
            press.modifiers.isEmpty ? moveCursor(1, in: rows, proxy: proxy) : .ignored
        }
        .onKeyPress(characters: CharacterSet(charactersIn: "k"), phases: .down) { press in
            press.modifiers.isEmpty ? moveCursor(-1, in: rows, proxy: proxy) : .ignored
        }
        .onKeyPress(.return) {
            guard let cursor, rows.contains(where: { $0.id == ProjectListRow.projectId(cursor) }) else { return .ignored }
            store.open(cursor)
            return .handled
        }
        .onKeyPress(characters: CharacterSet(charactersIn: "c"), phases: .down, action: createProject)
        .onKeyPress(.space) {
            guard cursor != nil else { return .ignored }
            peeking.toggle()
            return .handled
        }
        .onKeyPress(.escape) {
            guard peeking else { return .ignored }
            peeking = false
            return .handled
        }
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
        let detail = store.details[summary.id]
        let percent = detail?.progress?.percent
            ?? (total > 0 ? Int(summary.completedTaskCount * 100 / total) : 0)
        let priority = TaskPriority(summary.project.priority ?? 0)
        let labels = model.workLabels
        let tags = (summary.project.labelIds ?? []).compactMap { labels[$0] }
        return DenseRow(highlighted: cursor == summary.id, action: { store.open(summary.id) },
                        hover: { if $0 { cursor = summary.id } }) {
            PriorityIcon(priority: priority)
                .frame(width: 16)
                .help(priority.label)
            ProjectStatusIcon(status: summary.project.status, progress: Double(percent) / 100)
            if let icon = summary.project.icon { Text(icon).font(.system(size: 13)) }
            Text(summary.project.title)
                .font(TypeScale.body(13.5).weight(.medium))
                .foregroundStyle(Theme.ink)
                .lineLimit(1)
            if total == 0 { Text("No tasks").metaText() }
            if summary.project.leadIsSelf == true {
                Text("Lead Me").metaText().lineLimit(1)
            } else if let lead = summary.project.leadPersonId.flatMap({ id in people.first { $0.id == id } }) {
                Text("Lead \(lead.name)").metaText().lineLimit(1)
            }
            if let text = summary.project.summary {
                ProjectRowSummary(text: text)
            } else {
                Spacer(minLength: 12)
            }
            if !tags.isEmpty {
                HStack(spacing: 4) {
                    ForEach(tags.prefix(2)) { LabelPill(label: $0) }
                }
                .layoutPriority(-1)
            }
            AvatarStack(faces: detail.map(owners) ?? [], size: 18, revision: photoRevision)
                .frame(width: 62, alignment: .trailing)
            ProjectHealthPill(health: detail?.latestUpdate?.health, compact: true)
                .frame(width: 84, alignment: .leading)
            HStack(spacing: 6) {
                ProgressRing(fraction: Double(percent) / 100, size: 14, lineWidth: 2.25)
                Text("\(percent)%").metaText().monospacedDigit()
            }
            .frame(width: 64, alignment: .trailing)
            .help("\(summary.completedTaskCount) of \(total) tasks done; started tasks count a quarter")
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(percent)% complete")
            Group {
                if let target = summary.project.targetAtMs {
                    Text(ProjectDates.text(target, summary.project.targetResolution)).metaText().lineLimit(1)
                        .help("Target \(PlanningFormat.date(target))")
                } else {
                    Text("No target").metaText(Theme.inkDisabled)
                }
            }
            .frame(width: 92, alignment: .trailing)
        }
        .contextMenu {
            moveMenu(summary.project)
            Divider()
            ProjectDependencyMenu(store: store, project: summary.project)
            Divider()
            Button("Delete project", role: .destructive) { Task { await store.binProject(summary.project) } }
        }
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

    /// The project's page, or a task opened from its Tasks tab in its place.
    /// The tab it left comes back on Esc.
    private func projectPage(_ id: String) -> some View {
        let focus = model.workFocus
        let opened = focus.opened(in: .projects)
        return ZStack(alignment: .bottomLeading) {
            if let opened {
                TaskDetailView(id: opened, place: .projects)
            } else {
                projectBody(id)
            }
            UndoToast()
        }
        .overlay(alignment: .trailing) {
            if focus.peeking, opened == nil, focus.projectList != nil, let task = focus.highlighted,
               let row = model.workRows([task]).first {
                TaskPeek(row: row, place: .projects)
                    .transition(reduceMotion ? .opacity : .move(edge: .trailing).combined(with: .opacity))
            }
        }
        .overlay(alignment: .bottom) {
            if focus.selecting, opened == nil, focus.projectList != nil {
                BulkActionBar(place: .projects)
                    .padding(.bottom, 20)
                    .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(Motion.regular, value: focus.peeking)
        .animation(Motion.regular, value: focus.selecting)
        .modifier(WorkSheets())
    }

    private func projectBody(_ id: String) -> some View {
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
                    .buttonStyle(.secondary)
                    .disabled(store.mutating)
                Button("Talk to Sona", systemImage: "bubble.left") { discussProject(detail.project) }
                    .buttonStyle(.secondary)
                let starred = model.work.isFavorite(.project, detail.project.id)
                Button {
                    model.work.setFavorite(.project, detail.project.id, !starred)
                } label: { Image(systemName: starred ? "star.fill" : "star") }
                    .buttonStyle(.icon)
                    .accessibilityLabel(starred ? "Remove from favorites" : "Add to favorites")
                    .help(starred ? "Remove from favorites" : "Add to favorites")
                Button {
                    Task { await store.loadDetail(id) }
                } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(.icon)
                    .accessibilityLabel("Refresh project")
                    .help("Refresh project")
                    .disabled(store.detailLoading)
                Button {
                    templatingProject = detail.project
                } label: { Image(systemName: "square.on.square") }
                    .buttonStyle(.icon)
                    .accessibilityLabel("Save as template")
                    .help("Save as template")
                    .disabled(store.mutating)
                Button {
                    Task { await store.binProject(detail.project) }
                } label: { Image(systemName: "trash") }
                    .buttonStyle(.icon)
                    .accessibilityLabel("Delete project")
                    .help("Delete project. It stays in Recently deleted for 30 days.")
                    .disabled(store.mutating)
                Menu {
                    ProjectDependencyMenu(store: store, project: detail.project)
                } label: { Image(systemName: "ellipsis") }
                    .menuStyle(.borderlessButton)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .frame(width: Theme.controlHeight, height: Theme.controlHeight)
                    .help("More")
                    .accessibilityLabel("More project actions")
            }
        }
    }

    private func projectContent(_ detail: PlanningProjectDetail) -> some View {
        let now = now
        return VStack(alignment: .leading, spacing: 0) {
            header(detail)
                .padding(.top, 6)
                .padding(.bottom, 20)
            if let error = store.mutationError {
                PlannerFailure(message: error, retryTitle: "Refresh") {
                    store.clearMutationError()
                    Task { await store.loadDetail(detail.project.id) }
                }
                .padding(.bottom, 20)
            }
            SegmentedSwitch(items: ProjectPageTab.items, selection: pageTab) { pageTab = $0 }
                .fixedSize()
                .padding(.bottom, 20)
            switch pageTab {
            case .overview:
                HStack(alignment: .top, spacing: 24) {
                    VStack(alignment: .leading, spacing: 0) {
                        ProjectDescriptionEditor(text: detail.project.description, busy: store.mutating) { text in
                            Task {
                                await store.saveProject(detail.project,
                                                        PlanningProjectSave(description: text.isEmpty ? .clear : .set(text)))
                            }
                        }
                        .padding(.bottom, 28)
                        latestUpdate(detail, now: now)
                        if let progress = detail.progress {
                            VStack(alignment: .leading, spacing: 10) {
                                Text("Progress").sectionLabel().accessibilityAddTraits(.isHeader)
                                Card {
                                    ProjectProgressChart(progress: progress, targetAtMs: detail.project.targetAtMs)
                                        .padding(16)
                                }
                            }
                            .padding(.bottom, 28)
                        } else if let series = detail.burnUp(until: .now) {
                            VStack(alignment: .leading, spacing: 10) {
                                Text("Progress").sectionLabel().accessibilityAddTraits(.isHeader)
                                Card { BurnUpChart(series: series).padding(16) }
                            }
                            .padding(.bottom, 28)
                        }
                        milestones(detail, now: now)
                        evidence(detail)
                        PlanningCommentsView(threads: store.comments,
                                             subject: PlanningCommentSubject(kind: .project, id: detail.project.id))
                            .padding(.top, 28)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    if !model.workFocus.panelHidden {
                        ProjectPropertiesPanel(detail: detail, people: people, labels: model.work.labels) { change in
                            Task { await store.saveProject(detail.project, change) }
                        }
                        .frame(width: 260)
                    }
                }
            case .tasks:
                VStack(alignment: .leading, spacing: 12) {
                    numbers(detail.pulse, now: now)
                    ownerFaces(detail.pulse)
                }
                .padding(.bottom, 28)
                tasks(detail)
            case .updates:
                ProjectUpdatesView(store: store, detail: detail, people: people)
            case .insights:
                WorkInsightsView(projectId: detail.project.id, labels: model.work.labels, people: people)
            }
        }
        .animation(Motion.settle(reduced: reduceMotion), value: detail)
        .focusable()
        .focusEffectDisabled()
        .focused($keysFocused)
        .onKeyPress(characters: CharacterSet(charactersIn: "c"), phases: .down) { press in
            guard !typing, press.modifiers.isEmpty, titleDraft == nil, !store.mutating else { return .ignored }
            creation = .task(detail.project.id, detail.milestones)
            return .handled
        }
        .onKeyPress(.escape) {
            if titleDraft != nil {
                titleDraft = nil
            } else if !typing {
                store.closeProject()
            } else {
                return .ignored
            }
            return .handled
        }
        .onAppear { keysFocused = true }
    }

    /// True while a text field or editor on the page takes the keys.
    private var typing: Bool { NSApp.keyWindow?.firstResponder is NSText }

    private func header(_ detail: PlanningProjectDetail) -> some View {
        let pulse = detail.pulse
        let counted = pulse.done + pulse.open
        let fraction = detail.progress.map { Double($0.percent) / 100 }
            ?? (counted > 0 ? Double(pulse.done) / Double(counted) : 0)
        let project = detail.project
        return VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .center, spacing: 16) {
                ZStack {
                    ProgressRing(fraction: fraction, size: 52, lineWidth: 4.5)
                    Text(counted > 0 ? ProgressRing.percent(fraction) : "—")
                        .font(TypeScale.label(12).weight(.semibold))
                        .monospacedDigit()
                        .foregroundStyle(counted > 0 ? Theme.ink : Theme.inkTertiary)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(counted > 0 ? "Progress, \(pulse.done) of \(counted) done" : "Progress, no tasks")
                VStack(alignment: .leading, spacing: 8) {
                    ProjectTitleField(title: project.title, draft: $titleDraft) { title in
                        Task { await store.saveProject(project, PlanningProjectSave(title: title)) }
                    }
                    .pageHeadline()
                    ProjectSummaryField(summary: project.summary) { summary in
                        Task {
                            await store.saveProject(project, PlanningProjectSave(
                                summary: summary.isEmpty ? .clear : .set(summary)))
                        }
                    }
                    HStack(spacing: 12) {
                        ProjectStatusPicker(project: project, progress: fraction) { change in
                            Task { await store.saveProject(project, change) }
                        } label: {
                            HStack(spacing: 6) {
                                ProjectStatusIcon(status: project.status, progress: fraction)
                                Text(store.statusTitle(of: project))
                                    .font(TypeScale.label(12.5))
                                    .foregroundStyle(Theme.ink)
                                Image(systemName: "chevron.down")
                                    .font(.system(size: 8, weight: .bold))
                                    .foregroundStyle(Theme.inkTertiary)
                            }
                            .padding(.horizontal, 10)
                            .frame(height: Theme.compactHeight)
                            .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                        }
                        if let start = project.startAtMs {
                            let verb = start <= now ? "Started" : "Starts"
                            Text("\(verb) \(ProjectDates.text(start, project.startResolution))").metaText()
                        } else {
                            Text("Created \(PlanningFormat.day(project.createdAtMs))").metaText()
                        }
                        ProjectHealthPill(health: detail.latestUpdate?.health)
                        if detail.updateMissing {
                            Button("Update due") { pageTab = .updates }
                                .buttonStyle(.quiet)
                        }
                    }
                }
            }
        }
    }

    private func latestUpdate(_ detail: PlanningProjectDetail, now: Int64) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            sectionHeader("Latest update", count: 0) {
                Button(detail.latestUpdate == nil ? "Write update" : "All updates") { pageTab = .updates }
                    .buttonStyle(.quiet)
            }
            if let update = detail.latestUpdate {
                ProjectUpdateCard(update: update, now: now)
            } else {
                Card { CardLine("No update yet.") }
            }
        }
        .padding(.bottom, 28)
    }

    private func numbers(_ pulse: PlanningProjectPulse, now: Int64) -> some View {
        HStack(spacing: 10) {
            NumberTile("\(pulse.open)", label: "Open")
            NumberTile("\(pulse.overdue)", label: "Overdue", tone: pulse.overdue > 0 ? Theme.live : Theme.ink)
            if let due = pulse.nextDueAtMs {
                NumberTile(PlanningFormat.dueDay(due, now: now), label: "Next due")
                    .help(PlanningFormat.date(due))
            } else {
                NumberTile("—", label: "Next due", tone: Theme.inkTertiary)
                    .accessibilityRepresentation { Text("No next due date") }
            }
            NumberTile("\(pulse.meetingsThisWeek)", label: "Meetings, last 7 days")
        }
    }

    @ViewBuilder
    private func ownerFaces(_ pulse: PlanningProjectPulse) -> some View {
        if peopleUnavailable, !pulse.owners.isEmpty {
            Text("People unavailable.").metaText()
        } else if !pulse.owners.isEmpty {
            let active = activeOwner(pulse)?.id
            ChatFlowRow(spacing: 6) {
                ForEach(pulse.owners, id: \.personId) { owner in
                    ownerFace(owner, active: active == owner.personId)
                }
            }
        }
    }

    @ViewBuilder
    private func ownerFace(_ owner: PlanningOwnerCount, active: Bool) -> some View {
        let tasks = owner.openTasks == 1 ? "task" : "tasks"
        if let person = people.first(where: { $0.id == owner.personId }) {
            Button {
                withAnimation(Motion.settle(reduced: reduceMotion)) {
                    ownerFilter = active ? nil : owner.personId
                }
            } label: {
                faceChip(count: owner.openTasks, selected: active) {
                    Avatar(face: AvatarFace(kind: .person(owner.personId), name: person.name), size: 20,
                           revision: photoRevision)
                }
            }
            .buttonStyle(.plain)
            .help(active ? "Show all tasks" : "Show \(person.name)'s open \(tasks)")
            .accessibilityLabel("Show \(person.name)'s \(owner.openTasks) \(tasks)")
            .accessibilityAddTraits(active ? .isSelected : [])
        } else {
            faceChip(count: owner.openTasks, selected: false) {
                Image(systemName: "person.crop.circle.badge.questionmark")
                    .font(.system(size: 15))
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(width: 20, height: 20)
            }
            .help("Person unavailable")
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Person unavailable, \(owner.openTasks) open \(tasks)")
        }
    }

    private func faceChip<Face: View>(count: Int, selected: Bool, @ViewBuilder face: () -> Face) -> some View {
        HStack(spacing: 5) {
            face()
            Text("\(count)")
                .font(TypeScale.label(12.5))
                .monospacedDigit()
                .foregroundStyle(selected ? Theme.ink : Theme.inkSecondary)
        }
        .padding(.leading, 3)
        .padding(.trailing, 10)
        .frame(height: Theme.controlHeight)
        .background(selected ? Theme.selection : Color.clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(selected ? Color.clear : Theme.border, lineWidth: 1))
        .contentShape(RoundedRectangle(cornerRadius: Theme.radiusControl))
    }

    private func activeOwner(_ pulse: PlanningProjectPulse) -> PlanningPersonChoice? {
        guard let id = ownerFilter, !peopleUnavailable, pulse.owners.contains(where: { $0.personId == id }) else {
            return nil
        }
        return people.first { $0.id == id }
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
        let ordered = detail.orderedMilestones
        return VStack(alignment: .leading, spacing: 10) {
            sectionHeader("Milestones", count: detail.milestones.count) {
                Button("Add milestone", systemImage: "plus") { creation = .milestone(detail.project.id) }
                    .buttonStyle(.quiet)
                    .disabled(store.mutating)
            }
            Card {
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
                            .draggable(milestone.id) {
                                Text(milestone.title).bodyText(13).padding(6)
                            }
                            .dropDestination(for: String.self) { ids, _ in
                                let order = ordered.map(\.id)
                                guard let moved = ids.first, moved != milestone.id,
                                      let from = order.firstIndex(of: moved),
                                      let to = order.firstIndex(of: milestone.id) else { return false }
                                var changed = order
                                changed.remove(at: from)
                                changed.insert(moved, at: to)
                                Task { await store.reorderMilestones(projectId: detail.project.id, ids: changed) }
                                return true
                            }
                    }
                }
            }
            if ordered.count > 1 {
                Text("Drag a milestone to change its order.").metaText()
            }
        }
        .padding(.bottom, 28)
    }

    private func milestoneRow(_ milestone: PlanningMilestone, detail: PlanningProjectDetail, now: Int64) -> some View {
        let overdue = milestone.isOverdue(at: now)
        let progress = detail.progress(of: milestone)
        return DenseRow(action: { editingMilestone = milestone }) {
            workStatusPicker(milestone.status, title: milestone.title) { status in
                Task { await store.setMilestoneStatus(milestone, status) }
            } icon: { status in
                MilestoneMark(status: status, overdue: overdue && status == milestone.status)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(milestone.title).bodyText(13.5).lineLimit(1)
                if let description = milestone.description, !description.isEmpty {
                    Text(description).metaText().lineLimit(1)
                }
            }
            .padding(.vertical, 8)
            .help(milestone.description.map { "\(milestone.title)\n\($0)" } ?? milestone.title)
            if progress?.nextOpen == true {
                Text("Next")
                    .font(TypeScale.label(11).weight(.semibold))
                    .foregroundStyle(Theme.accent)
                    .padding(.horizontal, 6)
                    .frame(height: 18)
                    .background(Theme.accentSoft, in: Capsule())
                    .accessibilityLabel("Next open milestone")
            }
            Spacer(minLength: 12)
            if let heat = detail.pulse.milestones.first(where: { $0.milestoneId == milestone.id }) {
                heatLabel(heat, now: now)
            }
            if let progress, progress.scope > 0 {
                Text("\(progress.percent)%").metaText().monospacedDigit()
                    .help("\(progress.done) done, \(progress.started) started of \(progress.scope) tasks")
            }
            dueLabel(milestone.dueAtMs, overdue: overdue)
            linkButton(milestone.title) {
                linkSource(to: .milestone, id: milestone.id, title: milestone.title, detail: detail)
            }
            Button { discussedMilestone = milestone } label: {
                Image(systemName: "bubble.left").font(.system(size: 11, weight: .semibold))
            }
            .buttonStyle(.quiet)
            .help("Comments")
            .accessibilityLabel("Comments on \(milestone.title)")
        }
        .contextMenu {
            Button("Edit milestone…") { editingMilestone = milestone }
            Button("Comments…") { discussedMilestone = milestone }
            Divider()
            Button("Convert to project…") { convertingMilestone = milestone }
            Button("Delete milestone…", role: .destructive) { deletingMilestone = milestone }
        }
    }

    private func heatLabel(_ heat: PlanningMilestonePulse, now: Int64) -> some View {
        let days = max(0, PlanningFormat.days(from: heat.lastMeetingAtMs, to: now))
        let meetings = heat.meetings == 1 ? "1 meeting" : "\(heat.meetings) meetings"
        let latest = days == 0 ? "today" : "\(days) d"
        let ago = days == 0 ? "today" : days == 1 ? "1 day ago" : "\(days) days ago"
        return Text("\(meetings) · \(latest)")
            .metaText()
            .monospacedDigit()
            .lineLimit(1)
            .help("Last meeting \(PlanningFormat.date(heat.lastMeetingAtMs))")
            .accessibilityLabel("\(meetings), latest \(ago)")
    }

    /// The project's tasks as the Tasks screen lists them, with filters and
    /// a display of this project's own; an owner's face narrows them to
    /// that person's open tasks.
    private func tasks(_ detail: PlanningProjectDetail) -> some View {
        let id = detail.project.id
        let now = model.work.now
        let person = activeOwner(detail.pulse)
        let draft = model.workFocus.projectListing(id)
        let rows = (model.work.items ?? []).filter { row in
            row.projectId == id && (person.map { row.ownerPersonId == $0.id && row.status.isOpen } ?? true)
        }
        let groups = model.listedGroups(rows, draft: draft, now: now)
        let shown = Set(groups.flatMap(\.rows).map(\.id)).count
        return VStack(alignment: .leading, spacing: 10) {
            sectionHeader("Tasks", count: shown) {
                if person != nil {
                    Button("Show all") {
                        withAnimation(Motion.settle(reduced: reduceMotion)) { ownerFilter = nil }
                    }
                    .buttonStyle(.quiet)
                }
                TaskListingButtons(draft: draft)
                Button("Add task", systemImage: "plus") {
                    creation = .task(id, detail.milestones)
                }
                .buttonStyle(.quiet)
                .disabled(store.mutating)
            }
            if let person {
                Text("Showing \(person.name)'s open tasks.").metaText()
            }
            TaskFilterBar(draft: draft)
            if shown == 0 {
                Card { CardLine(rows.isEmpty ? "No tasks yet." : "No tasks match these filters.") }
                    .onAppear { model.workFocus.show([]) }
            } else {
                TaskListingBody(groups: groups, draft: draft, now: now, scrolls: false)
            }
        }
        .padding(.bottom, 28)
        .onAppear { model.workFocus.projectList = id }
        .onDisappear {
            let focus = model.workFocus
            if focus.projectList == id { focus.projectList = nil }
            focus.peeking = false
            focus.renaming = nil
            focus.clearSelection()
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
                Button("New note", systemImage: "square.and.pencil") { model.newNote(about: detail.project) }
                    .buttonStyle(.quiet)
                    .disabled(store.mutating || store.detailLoading)
                Button("Link source", systemImage: "link") {
                    linkSource(to: .project, id: detail.project.id, title: detail.project.title, detail: detail)
                }
                .buttonStyle(.quiet)
                .disabled(store.mutating || store.detailLoading)
            }
            Card {
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
        case .dictation: "Dictation"
        case .link: "Link"
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
                    .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, style: StrokeStyle(lineWidth: 1, dash: [4, 4])))
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
                        .foregroundStyle(PlanningDue.isPast(due, at: now) ? Theme.live : Theme.inkSecondary)
                        .help(PlanningFormat.date(due))
                }
                if !faces.isEmpty { AvatarStack(faces: faces, size: 18, revision: revision) }
            }
        }
        .padding(12)
        .background(hovering ? Theme.wash : Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
        .contentShape(RoundedRectangle(cornerRadius: Theme.radiusControl))
        .onHover { hovering = $0 }
        .hoverFade(hovering)
        .onTapGesture(perform: open)
        .allowsWindowActivationEvents()
        .draggable(summary.id) {
            Text(summary.project.title)
                .font(TypeScale.body(13).weight(.medium))
                .foregroundStyle(Theme.ink)
                .padding(.horizontal, 12)
                .frame(height: 30)
                .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
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
        case .dictation: "Selected dictation"
        case .link: "Selected link"
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

/// A project's summary in its row, in the room the row leaves: whole when it
/// fits, cut short when some room is left, and left out when none is.
private struct ProjectRowSummary: View {
    let text: String

    var body: some View {
        ViewThatFits(in: .horizontal) {
            Text(text).metaText().lineLimit(1)
            Text(text).metaText().lineLimit(1)
                .frame(idealWidth: 96, maxWidth: .infinity, alignment: .leading)
            Color.clear.frame(width: 0, height: 0)
        }
        .padding(.trailing, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct PlanningDraft: Equatable {
    var title = ""
    var summary = ""
    var body = ""
    var dueAt: Date?
    var milestoneId: String?
    var owner: PlanningOwner = .unassigned
    var templateId: String?
    var start = Date()
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
    @State private var deletingTemplate: PlanningProjectTemplate?
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
                        VStack(alignment: .leading, spacing: 7) {
                            Text("Summary").sectionLabel()
                            InputField(prompt: "Add a short summary…", text: $draft.summary)
                                .accessibilityLabel("Summary")
                        }
                        if !store.projectTemplates.isEmpty {
                            HStack {
                                Picker("Template", selection: $draft.templateId) {
                                    Text("None").tag(String?.none)
                                    ForEach(store.projectTemplates) { Text($0.name).tag(Optional($0.id)) }
                                }
                                if let template {
                                    Button("Delete template", systemImage: "trash") { deletingTemplate = template }
                                        .labelStyle(.iconOnly)
                                        .buttonStyle(.borderless)
                                        .help("Delete the template “\(template.name)”")
                                }
                            }
                            .confirmationDialog("Delete the template “\(deletingTemplate?.name ?? "")”?",
                                                isPresented: Binding(get: { deletingTemplate != nil },
                                                                     set: { if !$0 { deletingTemplate = nil } }),
                                                presenting: deletingTemplate) { doomed in
                                Button("Delete template", role: .destructive) {
                                    Task { await deleteTemplate(doomed) }
                                }
                            } message: { doomed in
                                Text("\(templateContents(doomed)) Projects made from it stay.")
                            }
                        }
                        bodyField(template == nil ? "Description" : "Description, in place of the template's")
                        if let template {
                            DatePicker("Starts", selection: $draft.start, displayedComponents: .date)
                            Text(templateContents(template)).metaText()
                        }
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
        .task { if case .project = target { await store.loadProjectTemplates() } }
        .transaction {
            if reduceMotion { $0.animation = nil; $0.disablesAnimations = true }
        }
    }

    private var sheetHeight: CGFloat {
        switch target {
        case .project: store.projectTemplates.isEmpty ? 420 : 480
        case .milestone: 300
        case .task: 500
        }
    }

    private var template: PlanningProjectTemplate? {
        store.projectTemplates.first { $0.id == draft.templateId }
    }

    private func templateContents(_ template: PlanningProjectTemplate) -> String {
        let milestones = template.fields.milestones.count
        let tasks = template.fields.tasks.count
        return "\(milestones) \(milestones == 1 ? "milestone" : "milestones") and \(tasks) \(tasks == 1 ? "task" : "tasks"), dated from the start day."
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

    private func deleteTemplate(_ doomed: PlanningProjectTemplate) async {
        error = nil
        do {
            try await store.deleteProjectTemplate(id: doomed.id, operationId: UUID().uuidString)
            if draft.templateId == doomed.id { draft.templateId = nil }
        } catch {
            self.error = error.localizedDescription
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
                let summary = draft.summary.trimmingCharacters(in: .whitespacesAndNewlines)
                if let template {
                    _ = try await store.createProject(template: template, title: title,
                                                      summary: summary.isEmpty ? nil : summary,
                                                      description: body.isEmpty ? nil : body,
                                                      start: draft.start, operationId: operationId)
                } else {
                    _ = try await store.createProject(title: title, summary: summary.isEmpty ? nil : summary,
                                                      description: body.isEmpty ? nil : body, operationId: operationId)
                }
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

/// Saves a project as a template under a name, refilling the template of
/// that name when there is one.
private struct ProjectTemplateSaveSheet: View {
    let project: PlanningProject
    let store: ProjectsStore
    @Environment(\.dismiss) private var dismiss
    @State private var name: String
    @State private var operationId = UUID().uuidString
    @State private var saving = false
    @State private var error: String?
    @FocusState private var nameFocused: Bool

    init(project: PlanningProject, store: ProjectsStore) {
        self.project = project
        self.store = store
        _name = State(initialValue: project.title)
    }

    private var trimmed: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }

    private var replaced: PlanningProjectTemplate? {
        store.projectTemplates.first { $0.name.lowercased() == trimmed.lowercased() }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Save as template").titleText().accessibilityAddTraits(.isHeader)
            VStack(alignment: .leading, spacing: 7) {
                Text("Name").sectionLabel()
                InputField(prompt: "Template name", text: $name)
                    .accessibilityLabel("Name")
                    .focused($nameFocused)
            }
            Text("Keeps the description, lead, members, milestones and tasks with their priorities, labels and estimates.")
                .metaText()
                .fixedSize(horizontal: false, vertical: true)
            if let replaced { Text("Replaces the template “\(replaced.name)”.").metaText() }
            if let error { ErrorNote(error) }
            Spacer(minLength: 0)
            HStack {
                Button("Cancel") { dismiss() }
                    .buttonStyle(.quiet)
                    .keyboardShortcut(.cancelAction)
                    .disabled(saving)
                Spacer()
                if saving { ProgressView().controlSize(.small).accessibilityLabel("Saving") }
                Button("Save") { Task { await save() } }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
                    .disabled(saving || store.mutating || trimmed.isEmpty)
            }
        }
        .padding(24)
        .frame(width: 420, height: 280)
        .background(Theme.page)
        .interactiveDismissDisabled(saving)
        .onChange(of: name) { _, _ in operationId = UUID().uuidString; error = nil }
        .onAppear { nameFocused = true }
        .task { await store.loadProjectTemplates() }
    }

    private func save() async {
        guard !saving, !trimmed.isEmpty else { return }
        saving = true
        error = nil
        defer { saving = false }
        do {
            try await store.saveProjectTemplate(name: trimmed, projectId: project.id, operationId: operationId)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }
    }
}

/// A milestone's name, description and due day, changed together through
/// the save `work milestone-save` makes.
private struct MilestoneEditSheet: View {
    let milestone: PlanningMilestone
    let store: ProjectsStore
    @Environment(\.dismiss) private var dismiss
    @State private var title: String
    @State private var details: String
    @State private var due: Date?

    init(milestone: PlanningMilestone, store: ProjectsStore) {
        self.milestone = milestone
        self.store = store
        _title = State(initialValue: milestone.title)
        _details = State(initialValue: milestone.description ?? "")
        _due = State(initialValue: milestone.dueAtMs.map(PlanningFormat.moment))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Edit milestone").titleText().accessibilityAddTraits(.isHeader)
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 7) {
                    Text("Title").sectionLabel()
                    InputField(prompt: "Title", text: $title)
                        .accessibilityLabel("Title")
                }
                VStack(alignment: .leading, spacing: 7) {
                    Text("Description").sectionLabel()
                    TextField("Description", text: $details, axis: .vertical)
                        .lineLimit(3...6)
                        .textFieldStyle(.roundedBorder)
                        .font(TypeScale.body())
                        .accessibilityLabel("Description")
                }
                PlanningDateField(date: $due)
            }
            Spacer(minLength: 0)
            HStack {
                Button("Cancel") { dismiss() }
                    .buttonStyle(.quiet)
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Save") { save() }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
                    .disabled(store.mutating || title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(24)
        .frame(width: 460, height: 380)
        .background(Theme.page)
    }

    private func save() {
        let name = title.trimmingCharacters(in: .whitespacesAndNewlines)
        let text = details.trimmingCharacters(in: .whitespacesAndNewlines)
        let dueMs = due.map(ProjectsStore.milliseconds)
        let change = PlanningMilestoneSave(
            title: name == milestone.title ? nil : name,
            description: text == (milestone.description ?? "") ? nil : text,
            dueAtMs: dueMs == milestone.dueAtMs ? nil : dueMs.map { .set($0) } ?? .clear)
        dismiss()
        guard change != PlanningMilestoneSave() else { return }
        Task { await store.saveMilestone(milestone, change) }
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
                        RoundedRectangle(cornerRadius: Theme.radiusKey).fill(Theme.track).frame(width: 180, height: 14)
                        RoundedRectangle(cornerRadius: Theme.radiusKey).fill(Theme.track).frame(width: 120, height: 12)
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
    /// Local calendar days from one moment to another, negative when `end` comes first.
    static func days(from start: Int64, to end: Int64) -> Int {
        let calendar = Calendar.current
        return calendar.dateComponents([.day], from: calendar.startOfDay(for: moment(start)),
                                       to: calendar.startOfDay(for: moment(end))).day ?? 0
    }
    /// "Today", "Tomorrow", or "In 3 days" within the week ahead, then the day.
    static func dueDay(_ milliseconds: Int64, now: Int64) -> String {
        let ahead = days(from: now, to: milliseconds)
        return switch ahead {
        case 0: "Today"
        case 1: "Tomorrow"
        case 2...6: "In \(ahead) days"
        default: day(milliseconds)
        }
    }
    static func passage(_ milliseconds: Int64) -> String {
        let seconds = max(0, milliseconds / 1000)
        return "\(seconds / 60):\(String(format: "%02lld", seconds % 60))"
    }
}
