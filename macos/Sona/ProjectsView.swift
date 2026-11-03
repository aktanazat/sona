import SwiftUI

struct ProjectsView: View {
    let store: ProjectsStore
    let people: [PlanningPersonChoice]
    var peopleUnavailable = false
    var material: AppearanceMaterial = .solid
    let openEvidence: (PlanningEvidenceLink) -> Void
    let openPerson: (String) -> Void
    let discussProject: (PlanningProject) -> Void
    let planWithSona: () -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var query = ""
    @State private var creation: PlanningCreation?
    @State private var sourceLink: PlanningLinkRequest?
    @State private var reviewProject: PlanningProject?

    var body: some View {
        Page {
            if let id = store.selectedProjectId {
                projectPage(id)
            } else {
                projectList
            }
        }
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

    private var filteredProjects: [PlanningProjectSummary] {
        let rows = store.projects ?? []
        let search = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !search.isEmpty else { return rows }
        return rows.filter {
            $0.project.title.localizedCaseInsensitiveContains(search)
                || ($0.project.description?.localizedCaseInsensitiveContains(search) ?? false)
        }
    }

    private var projectList: some View {
        VStack(alignment: .leading, spacing: 0) {
            PageTitle("Projects", subtitle: "") {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 10) {
                        planProjectButton
                        newProjectButton
                    }
                    VStack(alignment: .trailing, spacing: 8) {
                        planProjectButton
                        newProjectButton
                    }
                }
            }
            if store.projects != nil {
                SearchField(prompt: "Search projects", text: $query)
                    .accessibilityLabel("Search projects")
                    .padding(.bottom, 16)
            }
            if let error = store.listError {
                PlannerFailure(message: error) { Task { await store.loadProjects() } }
                    .padding(.bottom, 16)
            }
            if store.projects == nil {
                if store.listLoading { PlannerLoadingRows(label: "Loading projects") }
            } else if filteredProjects.isEmpty {
                Card(material: material) { CardLine(query.isEmpty ? "No projects yet." : "No matching projects.") }
            } else {
                Card(material: material) {
                    ForEach(filteredProjects) { summary in
                        CardRow(action: { store.open(summary.id) }) {
                            VStack(alignment: .leading, spacing: 7) {
                                Text(summary.project.title).headlineText()
                                    .fixedSize(horizontal: false, vertical: true)
                                HStack(spacing: 10) {
                                    Text(summary.project.status.label)
                                    Text("\(summary.completedTaskCount) done · \(summary.openTaskCount) open")
                                        .monospacedDigit()
                                }
                                .metaText(Theme.inkSecondary)
                                if let due = summary.nextDueAtMs {
                                    Text("Next due \(PlanningFormat.date(due))").metaText()
                                }
                            }
                        } trailing: {
                            HStack(spacing: 14) {
                                let total = summary.openTaskCount + summary.completedTaskCount
                                if total > 0 {
                                    ProgressView(value: Double(summary.completedTaskCount), total: Double(total))
                                        .progressViewStyle(.circular)
                                        .controlSize(.large)
                                        .tint(Theme.accent)
                                        .frame(width: 42, height: 42)
                                        .accessibilityHidden(true)
                                        .animation(reduceMotion ? nil : .easeOut(duration: 0.25), value: summary.completedTaskCount)
                                }
                                Image(systemName: "chevron.right")
                                    .font(.system(size: 11, weight: .semibold))
                                    .foregroundStyle(Theme.inkTertiary)
                                    .accessibilityHidden(true)
                            }
                        }
                    }
                }
            }
        }
    }

    private var planProjectButton: some View {
        Button("Plan with Sona", systemImage: "bubble.left") { planWithSona() }
            .buttonStyle(.secondary)
    }

    private var newProjectButton: some View {
        Button("New project", systemImage: "plus") { creation = .project }
            .buttonStyle(.primary)
    }

    @ViewBuilder
    private func projectPage(_ id: String) -> some View {
        BackLink(title: "Projects") { store.closeProject() }
        if let detail = store.detail, detail.project.id == id {
            projectContent(detail)
        } else if let error = store.detailError {
            PlannerFailure(message: error) { Task { await store.loadDetail(id) } }
        } else {
            PlannerLoadingRows(label: "Loading project")
        }
    }

    private func projectContent(_ detail: PlanningProjectDetail) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            PageTitle(detail.project.title, subtitle: "") {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 10) {
                        reviewLinksButton(detail.project)
                        discussProjectButton(detail.project)
                    }
                    VStack(alignment: .trailing, spacing: 8) {
                        reviewLinksButton(detail.project)
                        discussProjectButton(detail.project)
                    }
                }
            }
            if let description = detail.project.description, !description.isEmpty {
                Text(description).bodyText(14, Theme.inkSecondary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.bottom, 16)
            }
            PlannerNavigationSurface(material: material) {
                HStack(alignment: .center, spacing: 16) {
                    Menu(detail.project.status.label) {
                        ForEach(PlanningProjectStatus.allCases) { status in
                            Button(status.label) {
                                Task { await store.setProjectStatus(detail.project, status) }
                            }
                            .disabled(status == detail.project.status)
                        }
                    }
                    .accessibilityLabel("Project status: \(detail.project.status.label)")
                    .disabled(store.mutating || store.detailLoading)
                    Spacer()
                    Button {
                        Task { await store.loadDetail(detail.project.id) }
                    } label: { Image(systemName: "arrow.clockwise") }
                        .buttonStyle(.quiet)
                        .accessibilityLabel("Refresh project")
                        .help("Refresh project")
                        .disabled(store.detailLoading)
                }
            }
            .padding(.bottom, 20)
            if let error = store.mutationError {
                PlannerFailure(message: error, retryTitle: "Refresh") {
                    store.clearMutationError()
                    Task { await store.loadDetail(detail.project.id) }
                }
                .padding(.bottom, 20)
            }
            progress(detail)
                .padding(.bottom, 28)
            milestones(detail)
            tasks(detail)
            evidence(detail)
        }
    }

    private func reviewLinksButton(_ project: PlanningProject) -> some View {
        Button("Review links", systemImage: "link") { reviewProject = project }
            .buttonStyle(.secondary)
            .disabled(store.mutating)
    }

    private func discussProjectButton(_ project: PlanningProject) -> some View {
        Button("Talk to Sona", systemImage: "bubble.left") { discussProject(project) }
            .buttonStyle(.secondary)
    }

    private func progress(_ detail: PlanningProjectDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Tasks").sectionLabel()
                Spacer()
                Text("\(detail.completedTaskCount) done · \(detail.openTaskCount) open")
                    .metaText(Theme.inkSecondary)
                    .monospacedDigit()
            }
            if detail.countedTaskCount > 0 {
                ProgressView(value: Double(detail.completedTaskCount), total: Double(detail.countedTaskCount))
                    .tint(Theme.accent)
                    .accessibilityLabel("Completed tasks")
                    .accessibilityValue("\(detail.completedTaskCount) of \(detail.countedTaskCount)")
            }
            let canceled = detail.tasks.filter { $0.status == .canceled }.count
            if canceled > 0 { Text("\(canceled) canceled").metaText() }
        }
    }

    private func milestones(_ detail: PlanningProjectDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Milestones").sectionLabel().accessibilityAddTraits(.isHeader)
                Spacer()
                Button("Add milestone", systemImage: "plus") { creation = .milestone(detail.project.id) }
                    .buttonStyle(.quiet)
                    .disabled(store.mutating)
            }
            Card(material: material) {
                if detail.milestones.isEmpty { CardLine("No milestones yet.") }
                ForEach(detail.milestones) { milestone in
                    CardRow {
                        VStack(alignment: .leading, spacing: 5) {
                            Text(milestone.title).bodyText()
                                .fixedSize(horizontal: false, vertical: true)
                            if let due = milestone.dueAtMs {
                                Text(PlanningFormat.date(due)).metaText(Theme.inkSecondary)
                            }
                        }
                    } trailing: {
                        VStack(alignment: .trailing, spacing: 6) {
                            statusMenu(milestone.status, title: milestone.title) { status in
                                Task { await store.setMilestoneStatus(milestone, status) }
                            }
                            Button("Link source") {
                                linkSource(to: .milestone, id: milestone.id, title: milestone.title, detail: detail)
                            }
                            .buttonStyle(.quiet)
                            .accessibilityLabel("Link source to milestone \(milestone.title)")
                            .disabled(store.mutating || store.detailLoading)
                        }
                    }
                }
            }
        }
        .padding(.bottom, 28)
    }

    private func tasks(_ detail: PlanningProjectDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Work").sectionLabel().accessibilityAddTraits(.isHeader)
                Spacer()
                Button("Add task", systemImage: "plus") {
                    creation = .task(detail.project.id, detail.milestones)
                }
                .buttonStyle(.quiet)
                .disabled(store.mutating)
            }
            Card(material: material) {
                if detail.tasks.isEmpty { CardLine("No tasks yet.") }
                ForEach(detail.tasks) { task in
                    CardRow {
                        HStack(alignment: .top, spacing: 10) {
                            Image(systemName: task.status == .done ? "checkmark.circle.fill" : "circle")
                                .font(.system(size: 15))
                                .foregroundStyle(task.status == .done ? Theme.accent : Theme.inkTertiary)
                                .padding(.top, 2)
                                .accessibilityHidden(true)
                            VStack(alignment: .leading, spacing: 6) {
                                Text(task.title).bodyText()
                                    .fixedSize(horizontal: false, vertical: true)
                                if let details = task.details, !details.isEmpty {
                                    Text(details).bodyText(13, Theme.inkSecondary)
                                        .textSelection(.enabled)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                                if let milestoneId = task.milestoneId,
                                   let milestone = detail.milestones.first(where: { $0.id == milestoneId }) {
                                    Text(milestone.title).metaText()
                                }
                                ownerLabel(task)
                                if let due = task.dueAtMs {
                                    Text(PlanningFormat.date(due)).metaText(Theme.inkSecondary)
                                }
                            }
                        }
                    } trailing: {
                        VStack(alignment: .trailing, spacing: 6) {
                            statusMenu(task.status, title: task.title) { status in
                                Task { await store.setTaskStatus(task, status) }
                            }
                            Button("Link source") {
                                linkSource(to: .task, id: task.id, title: task.title, detail: detail)
                            }
                            .buttonStyle(.quiet)
                            .accessibilityLabel("Link source to task \(task.title)")
                            .disabled(store.mutating || store.detailLoading)
                        }
                    }
                }
            }
        }
        .padding(.bottom, 28)
    }

    @ViewBuilder
    private func ownerLabel(_ task: PlanningTask) -> some View {
        if task.ownerIsSelf {
            Text("Me").metaText(Theme.inkSecondary)
        } else if let id = task.ownerPersonId, task.ownerAvailable,
                  let person = people.first(where: { $0.id == id }) {
            Button(person.name) { openPerson(id) }
                .buttonStyle(.quiet)
                .accessibilityLabel("Open \(person.name)")
        } else if task.ownerPersonId != nil || !task.ownerAvailable {
            Text("Person unavailable").metaText()
        }
    }

    private func statusMenu(_ current: PlanningWorkStatus, title: String,
                            change: @escaping (PlanningWorkStatus) -> Void) -> some View {
        Menu(current.label) {
            ForEach(PlanningWorkStatus.allCases) { status in
                Button(status.label) { change(status) }.disabled(status == current)
            }
        }
        .fixedSize()
        .accessibilityLabel("Status of \(title): \(current.label)")
        .disabled(store.mutating || store.detailLoading)
    }

    private func evidence(_ detail: PlanningProjectDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Sources").sectionLabel().accessibilityAddTraits(.isHeader)
                Spacer()
                Button("Link source", systemImage: "link") {
                    linkSource(to: .project, id: detail.project.id, title: detail.project.title, detail: detail)
                }
                .buttonStyle(.quiet)
                .disabled(store.mutating || store.detailLoading)
            }
            Card(material: material) {
                if detail.evidence.isEmpty { CardLine("No linked sources.") }
                ForEach(detail.evidence) { link in
                    CardRow(action: link.available ? { openEvidence(link) } : nil) {
                        VStack(alignment: .leading, spacing: 5) {
                            Text(link.available ? (link.title ?? sourceLabel(link.sourceKind)) : "Source unavailable")
                                .bodyText(14, link.available ? Theme.ink : Theme.inkTertiary)
                                .fixedSize(horizontal: false, vertical: true)
                            if let target = targetTitle(link, detail: detail) { Text(target).metaText() }
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
    static func date(_ milliseconds: Int64) -> String {
        Date(timeIntervalSince1970: Double(milliseconds) / 1000).formatted(date: .abbreviated, time: .shortened)
    }
    static func passage(_ milliseconds: Int64) -> String {
        let seconds = max(0, milliseconds / 1000)
        return "\(seconds / 60):\(String(format: "%02lld", seconds % 60))"
    }
}
