import AppKit
import SwiftUI

struct PlannerReviewView: View {
    let project: PlanningProject
    let store: ProjectsStore
    let material: AppearanceMaterial
    let openEvidence: (PlanningEvidenceLink) -> Void

    @Environment(\.dismiss) private var dismiss
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var kind: PlanningSourceList = .meeting
    @State private var query = ""
    @State private var loopMeeting: PlanningSourceChoice?
    @State private var sources: [PlanningSourceChoice] = []
    @State private var selected: [PlanningSourceChoice] = []
    @State private var nextCursor: Int64?
    @State private var lookup = UUID()
    @State private var browserRevision = 0
    @State private var loadingSources = true
    @State private var sourceError: String?
    @State private var generationError: String?
    @State private var quietReviewSaveError: String?
    @FocusState private var searchFocused: Bool

    private var search: PlannerReviewSourceSearch {
        PlannerReviewSourceSearch(
            kind: kind, query: query.trimmingCharacters(in: .whitespacesAndNewlines),
            sourceRevision: store.sourcesRevision, browserRevision: browserRevision,
            meetingId: loopMeeting?.sourceId
        )
    }

    private var choosingLoopMeeting: Bool { kind == .loop && loopMeeting == nil }
    private var busy: Bool { store.suggestionsGenerating || store.suggestionActionId != nil }
    private var projectSuggestions: [PlanningSuggestion] {
        (store.suggestions ?? []).filter { $0.projectId == project.id }
    }
    private var unavailableSuggestions: [PlanningSuggestion] {
        (store.suggestions ?? []).filter { $0.projectId == nil }
    }
    private var currentProject: PlanningProject? {
        if let detail = store.detail, detail.project.id == project.id { return detail.project }
        return store.projects?.first { $0.id == project.id }?.project
    }

    private var quietReview: QuietReviewState? { store.quietReview(for: project.id) }
    private var quietReviewSelectionSaved: Bool {
        let chosen = Set(selected.map(\.id))
        let saved = Set((quietReview?.selectedSources ?? []).map { "\($0.kind.rawValue):\($0.id)" })
        return chosen == saved
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Review links").titleText().accessibilityAddTraits(.isHeader)
                if let currentProject {
                    Text(currentProject.title).bodyText(13, Theme.inkSecondary)
                        .lineLimit(2)
                }
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    sourceBrowser
                        .disabled(busy)
                    quietReviewControls
                    generationControls
                    Hairline()
                    pendingReview
                }
                .padding(2)
            }
            HStack {
                Spacer()
                Button("Done") { dismiss() }
                    .buttonStyle(.secondary)
                    .keyboardShortcut(.cancelAction)
                    .disabled(busy)
            }
        }
        .padding(24)
        .frame(width: 660)
        .frame(minHeight: 480, idealHeight: 640, maxHeight: 720)
        .background(Theme.page)
        .interactiveDismissDisabled(busy)
        .onAppear { searchFocused = true }
        .task {
            await store.beginReview()
            await store.loadQuietReviewGrants()
        }
        .task(id: search) { await loadSources(reset: true) }
        .onDisappear { store.endReview() }
        .onChange(of: kind) { _, _ in
            loopMeeting = nil
            query = ""
            clearSourceResults()
            browserRevision += 1
        }
        .onChange(of: store.sourcesRevision) { _, _ in
            selected = []
            loopMeeting = nil
            clearSourceResults()
            browserRevision += 1
            generationError = nil
        }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            store.invalidateSuggestions()
            selected = []
            loopMeeting = nil
            clearSourceResults()
            browserRevision += 1
            Task {
                await store.loadSuggestions()
                await store.loadQuietReviewGrants()
            }
        }
        .transaction {
            if reduceMotion { $0.animation = nil; $0.disablesAnimations = true }
        }
    }

    private var sourceBrowser: some View {
        VStack(alignment: .leading, spacing: 12) {
            PlannerNavigationSurface(material: material) {
                HStack(spacing: 12) {
                    Picker("Source type", selection: $kind) {
                        ForEach(PlanningSourceList.allCases) { Text($0.label).tag($0) }
                    }
                    .frame(width: 150)
                    SearchField(prompt: choosingLoopMeeting ? "Search meetings" : "Search sources", text: $query)
                        .accessibilityLabel(choosingLoopMeeting ? "Search saved meetings" : "Search saved \(kind.label.lowercased())")
                        .focused($searchFocused)
                }
            }
            if let loopMeeting, kind == .loop {
                HStack(alignment: .top, spacing: 12) {
                    Button("Meetings", systemImage: "chevron.left") {
                        self.loopMeeting = nil
                        query = ""
                        clearSourceResults()
                    }
                    .buttonStyle(.quiet)
                    Text(loopMeeting.title).bodyText(13, Theme.inkSecondary).lineLimit(2)
                }
            }
            sourceList
            if let sourceError {
                reviewFailure(sourceError) { Task { await loadSources(reset: nextCursor == nil) } }
            }
            DisclosureGroup("Selected sources (\(selected.count)/12)") {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(selected) { source in
                        HStack(alignment: .firstTextBaseline, spacing: 12) {
                            Text(source.title).bodyText(13).lineLimit(2)
                            Spacer(minLength: 0)
                            Button {
                                selected.removeAll { $0.id == source.id }
                                generationError = nil
                            } label: { Image(systemName: "xmark") }
                                .buttonStyle(.quiet)
                                .accessibilityLabel("Remove \(source.title) from selected sources")
                        }
                    }
                }
                .padding(.top, 8)
            }
            .font(TypeScale.body(13))
            .foregroundStyle(Theme.ink)
            .disabled(selected.isEmpty)
        }
    }

    private var sourceList: some View {
        VStack(spacing: 10) {
            List {
                ForEach(sources) { source in
                    if choosingLoopMeeting {
                        Button {
                            loopMeeting = source
                            query = ""
                            clearSourceResults()
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
                    } else {
                        Toggle(isOn: selection(for: source)) { sourceRow(source) }
                            .toggleStyle(.checkbox)
                            .disabled(selected.count == 12 && !selected.contains { $0.id == source.id })
                    }
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .accessibilityLabel("Saved sources")
            .overlay {
                if sources.isEmpty {
                    if loadingSources {
                        Text("Loading sources…").metaText()
                    } else if sourceError == nil {
                        Text(emptySourceMessage).metaText()
                    }
                }
            }
            if nextCursor != nil {
                HStack {
                    if loadingSources { Text("Loading sources…").metaText() }
                    Button("Load more") { Task { await loadSources(reset: false) } }
                        .buttonStyle(.compact)
                        .disabled(loadingSources)
                }
            }
        }
        .frame(height: 210)
    }

    private var emptySourceMessage: String {
        if choosingLoopMeeting { return search.query.isEmpty ? "No saved meetings." : "No matching meetings." }
        if kind == .loop { return search.query.isEmpty ? "No commitments." : "No matching commitments." }
        return search.query.isEmpty ? "No saved \(kind.label.lowercased())." : "No matching sources."
    }

    private func sourceRow(_ source: PlanningSourceChoice) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(source.title).bodyText().lineLimit(3)
            if let date = source.updatedAtMs {
                Text(PlanningFormat.date(date)).metaText(Theme.inkSecondary)
            }
        }
        .padding(.vertical, 5)
        .accessibilityElement(children: .combine)
    }

    private var generationControls: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                if store.suggestionsGenerating {
                    Text("Reviewing selected sources…").metaText()
                } else if let currentProject, currentProject.status != .active {
                    Text("Project \(currentProject.status.label.lowercased()).").metaText()
                }
                Spacer()
                Button(generationError == nil ? "Review selected sources" : "Retry review") {
                    Task { await generate() }
                }
                .buttonStyle(.primary)
                .disabled(busy || store.mutating || selected.isEmpty || currentProject?.status != .active)
            }
            if let generationError {
                Text(generationError).bodyText(13, Theme.live).lineLimit(1).help(generationError)
            }
        }
    }

    private var quietReviewControls: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text("Nighttime review").sectionLabel().accessibilityAddTraits(.isHeader)
                Spacer()
                if let quietReview, quietReview.enabled {
                    Text("On · \(quietReview.selectedSources.count) saved sources").metaText(Theme.inkSecondary)
                    Button("Turn off") { Task { await saveQuietReview(enabled: false) } }
                        .buttonStyle(.quiet)
                        .disabled(store.quietReviewSavingProjectId != nil)
                } else if store.quietReviewStates != nil {
                    Text("Off").metaText(Theme.inkSecondary)
                }
            }
            Text("Runs on this Mac from 22:00–06:00 local time after 10 minutes without input, with Sona open in the menu bar; never while the Mac is off or asleep.")
                .bodyText(12, Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
            if store.quietReviewStates != nil, let error = store.quietReviewLoadError {
                reviewFailure(error) { Task { await store.loadQuietReviewGrants() } }
            }
            if store.quietReviewStates == nil {
                if let error = store.quietReviewLoadError {
                    reviewFailure(error) { Task { await store.loadQuietReviewGrants() } }
                } else {
                    Text("Loading nighttime review…").metaText()
                }
            } else {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    Text(quietReviewCompletion).metaText(Theme.inkSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 0)
                    Button(quietReview?.enabled == true ? "Save selected sources" : "Allow nighttime review") {
                        Task { await saveQuietReview(enabled: true) }
                    }
                    .buttonStyle(.compact)
                    .disabled(store.quietReviewSavingProjectId != nil || store.quietReviewLoadError != nil
                              || selected.isEmpty || currentProject?.status != .active
                              || (quietReview?.enabled == true && quietReviewSelectionSaved))
                }
            }
            if store.quietReviewSavingProjectId == project.id {
                Text("Saving nighttime review…").metaText()
            } else if store.quietReviewRunningProjectId == project.id {
                Text("Reviewing saved sources…").metaText()
            }
            if let status = quietReviewSaveError ?? store.quietReviewStatus[project.id] {
                Text(status).bodyText(12, Theme.inkSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityElement(children: .contain)
    }

    private var quietReviewCompletion: String {
        guard let completedAt = quietReview?.lastCompletedAtMs else { return "No completed nighttime review." }
        let date = Date(timeIntervalSince1970: Double(completedAt) / 1000)
            .formatted(date: .abbreviated, time: .shortened)
        switch quietReview?.lastOutcome {
        case "suggestions_ready": return "Possible links ready · \(date)"
        case "nothing_new": return "Nothing new · \(date)"
        default: return "Last review unavailable."
        }
    }

    private func saveQuietReview(enabled: Bool) async {
        quietReviewSaveError = nil
        do {
            try await store.setQuietReview(projectId: project.id, enabled: enabled, sources: selected)
        } catch {
            quietReviewSaveError = error.localizedDescription
        }
    }

    private var pendingReview: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Review").sectionLabel().accessibilityAddTraits(.isHeader)
                Spacer()
                Button {
                    store.invalidateSuggestions()
                    Task { await store.loadSuggestions() }
                } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(.quiet)
                    .accessibilityLabel("Refresh links")
                    .help("Refresh links")
                    .disabled(busy || store.suggestionsLoading)
            }
            if let error = store.suggestionsError {
                reviewFailure(error) { Task { await store.loadSuggestions() } }
            } else if store.suggestions == nil {
                Text("Loading links…").metaText()
            } else if projectSuggestions.isEmpty {
                Text("No pending links.").metaText()
            } else {
                suggestionCards(projectSuggestions)
            }
            if !unavailableSuggestions.isEmpty {
                Text("Unavailable links").sectionLabel().accessibilityAddTraits(.isHeader)
                suggestionCards(unavailableSuggestions)
            }
            if let error = store.suggestionDecisionError {
                Text(error).bodyText(13, Theme.live).lineLimit(1).help(error)
            }
        }
    }

    private func suggestionCards(_ suggestions: [PlanningSuggestion]) -> some View {
        VStack(spacing: 12) {
            ForEach(suggestions) { suggestion in
                Card {
                    VStack(alignment: .leading, spacing: 12) {
                        Text(suggestion.kind.label).sectionLabel()
                        if suggestion.canLink, let title = suggestion.targetTitle, let evidence = suggestion.evidence {
                            Text(title).headlineText()
                                .textSelection(.enabled)
                                .fixedSize(horizontal: false, vertical: true)
                            if let link = suggestion.sourceLink {
                                Button { openEvidence(link) } label: {
                                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                                        Text(evidence.title).bodyText(13)
                                            .multilineTextAlignment(.leading)
                                            .fixedSize(horizontal: false, vertical: true)
                                        Image(systemName: "arrow.up.right")
                                            .font(.system(size: 10))
                                            .accessibilityHidden(true)
                                    }
                                }
                                .buttonStyle(.quiet)
                                .accessibilityLabel("Open source: \(evidence.title)")
                                .disabled(busy)
                            }
                            if suggestion.source.kind == .meeting, let atMs = suggestion.atMs {
                                Text(PlanningFormat.passage(atMs)).metaText(Theme.inkSecondary)
                            }
                            Text(evidence.text).bodyText(13, Theme.inkSecondary)
                                .textSelection(.enabled)
                                .fixedSize(horizontal: false, vertical: true)
                        } else {
                            Text(suggestion.availability == .changed ? "Source changed" : "Source unavailable")
                                .bodyText(13, Theme.inkSecondary)
                        }
                        HStack(spacing: 12) {
                            if store.suggestionActionId == suggestion.id {
                                Text("Saving decision…").metaText()
                            }
                            Spacer()
                            Button("Dismiss") {
                                Task { await store.decideSuggestion(suggestion, accept: false) }
                            }
                            .buttonStyle(.quiet)
                            .disabled(busy || store.mutating)
                            Button("Link evidence") {
                                Task { await store.decideSuggestion(suggestion, accept: true) }
                            }
                            .buttonStyle(.compact)
                            .disabled(!suggestion.canLink || busy || store.mutating)
                        }
                    }
                    .padding(16)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }

    private func reviewFailure(_ message: String, retry: @escaping () -> Void) -> some View {
        HStack(spacing: 12) {
            Text(message).bodyText(13, Theme.live).lineLimit(1).help(message)
            Spacer(minLength: 0)
            Button("Retry", action: retry).buttonStyle(.compact).disabled(busy)
        }
        .accessibilityElement(children: .contain)
    }

    private func selection(for source: PlanningSourceChoice) -> Binding<Bool> {
        Binding(get: { selected.contains { $0.id == source.id } }, set: { isSelected in
            if isSelected, selected.count < 12 {
                selected.append(source)
            } else if !isSelected {
                selected.removeAll { $0.id == source.id }
            }
            generationError = nil
        })
    }

    private func clearSourceResults() {
        lookup = UUID()
        sources = []
        nextCursor = nil
        sourceError = nil
        loadingSources = true
    }

    private func loadSources(reset: Bool) async {
        let searched = search
        if reset { clearSourceResults() }
        else if loadingSources { return }
        let current = lookup
        let cursor = reset ? nil : nextCursor
        loadingSources = true
        sourceError = nil
        defer { if lookup == current, search == searched { loadingSources = false } }
        do {
            if reset, !searched.query.isEmpty { try await Task.sleep(for: .milliseconds(200)) }
            let page = try await store.sourceCandidates(searched.kind, query: searched.query, after: cursor,
                                                        meetingId: searched.meetingId)
            guard !Task.isCancelled, lookup == current, search == searched else { return }
            if reset { sources = page.entries }
            else {
                let existing = Set(sources.map(\.id))
                sources += page.entries.filter { !existing.contains($0.id) }
            }
            nextCursor = page.nextCursor
        } catch is CancellationError {
        } catch {
            guard !Task.isCancelled, lookup == current, search == searched else { return }
            sourceError = error.localizedDescription
        }
    }

    private func generate() async {
        guard !busy, !selected.isEmpty else { return }
        generationError = nil
        do {
            try await store.generateSuggestions(projectId: project.id, sources: selected)
        } catch {
            generationError = error.localizedDescription
        }
    }
}

private struct PlannerReviewSourceSearch: Equatable {
    let kind: PlanningSourceList
    let query: String
    let sourceRevision: Int
    let browserRevision: Int
    let meetingId: String?
}
