import SwiftUI
import UniformTypeIdentifiers

/// The main window: the sidebar on the left, one page on the right. Before
/// the core answers, a wait; on a first run, the onboarding flow instead of
/// the pages. Each page puts its title and actions in the window's toolbar,
/// on the line of the close, minimize and zoom buttons.
struct Shell: View {
    @Environment(AppModel.self) private var model
    @State private var sharedNote: ScratchNote?
    @State private var paneWidth: CGFloat = 0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        ZStack {
            if !model.ready {
                waiting.keepsWindowToolbar()
            } else if model.onboarding.step != .done {
                OnboardingView(store: model.onboarding).keepsWindowToolbar()
            } else {
                NavigationSplitView(columnVisibility: .constant(.all)) {
                    Sidebar()
                        .navigationSplitViewColumnWidth(Theme.sidebarWidth)
                        .toolbar(removing: .sidebarToggle)
                } detail: {
                    VStack(spacing: 0) {
                        if model.coreStopped {
                            CoreStoppedBanner()
                        }
                        content
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                    .background(Theme.page.ignoresSafeArea())
                    .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { paneWidth = $0 }
                    .environment(\.paneWidth, paneWidth)
                    .toolbar(removing: .title)
                }
                if model.paletteShown {
                    CommandPalette()
                        .transition(reduceMotion ? .opacity : .opacity.combined(with: .scale(scale: 0.98, anchor: .top)))
                }
            }
        }
        .background(Theme.page.ignoresSafeArea())
        .animation(.easeOut(duration: reduceMotion ? 0.1 : 0.2), value: model.paletteShown)
        .frame(minWidth: 900, minHeight: 640)
        .sheet(item: Bindable(model).sheet) { sheet in
            switch sheet {
            case .chat:
                ChatView(
                    store: model.chat,
                    onClose: { model.sheet = nil },
                    openSettings: {
                        model.sheet = nil
                        model.showSettings(.agents)
                    },
                    pendingRequests: model.agents.pending.count,
                    openRequests: {
                        model.sheet = nil
                        model.showSettings(.agents)
                    })
            case .recorder:
                RecorderSheet(store: model.recorder) { model.sheet = nil }
            case .brief:
                MeetingBriefView(
                    store: model.prep,
                    onClose: { model.sheet = nil },
                    onOpenMeeting: { id in
                        model.sheet = nil
                        model.openMeeting(id)
                    },
                    onSettings: {
                        model.sheet = nil
                        model.showSettings(.meetings)
                    })
            case .whatsNew:
                WhatsNewView(store: model.debug.whatsNew) {
                    model.debug.whatsNew.dismiss()
                    model.sheet = nil
                }
            case let .nudge(id):
                NudgeSheet(id: id).environment(model)
            case let .workBrief(subject):
                WorkBriefSheet(subject: subject).environment(model)
            case let .share(meetingId):
                ShareSheet(meetingId: meetingId).environment(model)
            case let .saveTemplate(id):
                SaveTemplateSheet(id: id).environment(model)
            }
        }
    }

    /// The core is starting, or could not. A failed start is not the end:
    /// the same button the stopped-core banner has sits here too.
    private var waiting: some View {
        VStack(spacing: 12) {
            if let error = model.coreError, !model.coreRestarting {
                Text("Sona's engine could not start.").bodyText(15)
                Text(error).bodyText(13, Theme.inkSecondary)
                Button("Try again") { model.restartCore() }
                    .buttonStyle(.primary)
                    .padding(.top, 8)
            } else {
                ProgressView()
                Text("Starting…").bodyText(13, Theme.inkSecondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder
    private var content: some View {
        if model.showingSettings {
            SettingsScreen()
        } else {
            switch model.place {
            case .capture:
                CaptureScreen()
                    .overlay(alignment: .bottomLeading) { UndoToast() }
            case .activity:
                ActivityScreen()
            case .library:
                LibraryScreen(
                    store: model.library,
                    importAudio: { model.showSettings(.importing) },
                    addCorrection: { left, right in
                        model.vocabulary.add(kind: .vocabulary, left: left, right: right)
                    })
            case .meetings:
                MeetingsPlace()
            case .people:
                PeopleScreen(
                    store: model.people,
                    openMeeting: model.openMeeting,
                    openContribution: { sessionId, atMilliseconds in
                        model.meetings.open(sessionId, atMilliseconds: atMilliseconds)
                        model.go(.meetings)
                    },
                    ingestDocument: { model.showSettings(.importing) },
                    deleteDocument: { id in Task { await model.documents.delete(id: id) } })
            case .scratchpad:
                ScratchpadView(store: model.scratchpad, share: { note in
                    Task {
                        await model.scratchpad.flush()
                        guard let saved = model.scratchpad.saved,
                              saved.id == note.id,
                              saved.body == model.scratchpad.body else { return }
                        sharedNote = saved
                    }
                }, openSource: { model.open(link: $0) })
                    .sheet(item: $sharedNote) { note in
                        CloudNoteShareView(note: note)
                    }
            case .projects:
                ProjectsView(
                    store: model.projects,
                    people: model.plannerPeople,
                    peopleUnavailable: model.people.entries == nil || model.people.listFailed,
                    material: model.about.material,
                    photoRevision: model.people.photoRevision,
                    openEvidence: model.openPlanningEvidence,
                    discussProject: model.discussProject,
                    planWithSona: model.planWithSona,
                    showCalendar: { model.go(.calendar) })
            case .calendar:
                PlannerCalendarView(
                    store: model.projects,
                    material: model.about.material,
                    openProject: model.openProject,
                    openMeeting: model.openMeeting,
                    showProjects: { model.go(.projects) })
            case .tasks:
                TasksScreen()
            case .triage:
                TriageScreen()
            case .today:
                MorningPlanView()
            case .shared:
                SharedPagesView()
            case .inbox:
                InboxScreen()
            case .archive:
                TaskArchiveScreen()
            case .loops:
                LoopsScreen()
            case .initiatives:
                InitiativesScreen(store: model.projects, people: model.plannerPeople, openProject: model.openProject)
            }
        }
    }
}

/// Home's three faces, the overview, the numbers and every dictation, as the
/// start of Home's bar.
struct HomeTabs: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        SegmentedSwitch(items: [SwitchItem(id: Place.capture, title: "Overview", icon: "house"),
                                SwitchItem(id: Place.activity, title: "Activity", icon: "chart.bar"),
                                SwitchItem(id: Place.library, title: "Library", icon: "books.vertical")],
                        selection: model.place) { model.go($0) }
    }
}

/// The core stopped under the shell. The pages keep what they had; nothing
/// that reads the core works until it is started again, so the way to do
/// that sits with the news, on every page.
struct CoreStoppedBanner: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Image(systemName: "exclamationmark.triangle")
                .font(.system(size: 13))
                .foregroundStyle(Theme.inkSecondary)
            VStack(alignment: .leading, spacing: 2) {
                Text("Sona's engine stopped.").bodyText(14)
                Text(detail).bodyText(13, Theme.inkSecondary)
            }
            Spacer(minLength: 12)
            if model.coreRestarting {
                ProgressView().controlSize(.small)
                    .accessibilityLabel("Restarting the engine")
            } else {
                Button(model.coreError == nil ? "Restart" : "Try again") { model.restartCore() }
                    .buttonStyle(.primary)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard).strokeBorder(Theme.border, lineWidth: 1))
        .padding(.horizontal, Theme.margin)
        .padding(.top, 12)
        .accessibilityElement(children: .contain)
    }

    /// What was lost, or why the restart did not take.
    private var detail: String {
        if model.coreRestarting { return "Starting it again…" }
        if let error = model.coreError { return error }
        return "Anything in flight was lost. Restart to keep going."
    }
}

/// The meetings place: the live screen while one records, the gate between
/// a press and a capture, one meeting's review, or the home page: what leads
/// to a recording, then every recording there has been.
struct MeetingsPlace: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let live = model.live
        let meetings = model.meetings
        if live.live != nil {
            MeetingLiveView(
                store: live, onOpenReview: model.openMeeting,
                onOpenRecalled: { model.openPast($0.sessionId, segmentId: $0.segmentId) })
        } else if live.gate != nil {
            MeetingStartGateView(store: live)
        } else if meetings.openSessionId != nil {
            MeetingReviewView(
                store: meetings, settings: model.meetingSettings, openPerson: model.openPerson,
                openMeetingSettings: { model.showSettings(.meetings) },
                askAgent: { question in
                    model.chat.editDraft(question)
                    model.sheet = .chat
                })
        } else {
            home
        }
    }

    private var home: some View {
        let live = model.live
        let meetings = model.meetings
        return Page {
            PageTitle("Meetings", subtitle: "") {
                HStack(spacing: 8) {
                    Menu {
                        Button("Import recording…") { live.importMeeting() }
                            .disabled(live.importing)
                        Button("Export all as CSV…") { meetings.exportAllCsv() }
                            .disabled(meetings.busy)
                        Divider()
                        Button("Deleted meetings") { meetings.openTrash() }
                    } label: {
                        Image(systemName: "ellipsis")
                    }
                    .menuStyle(.button)
                    .buttonStyle(.icon)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .help("Import, export and deleted meetings")
                    .accessibilityLabel("Meeting library options")
                    Button(live.starting ? "Starting…" : "Record…") { live.startManual() }
                        .buttonStyle(.primary)
                        .disabled(live.starting || live.pending != nil)
                }
            }
            ErrorNote(live.error)
            if let notice = live.notice {
                Text(notice).bodyText(13, Theme.inkSecondary).padding(.bottom, 16)
            }
            if let engine = live.engineWarning {
                // Every engine warning is fixed under Settings → Meetings; the
                // Apple Intelligence switch and its download are one pane
                // further, in System Settings.
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.inkSecondary)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("No notes will be written for the next meeting.").bodyText(14)
                        Text(engine.sentence).bodyText(13, Theme.inkSecondary)
                    }
                    Spacer(minLength: 12)
                    HStack(spacing: 8) {
                        if engine.settingsPaneBlocker != nil {
                            Button("Open System Settings") { live.openAppleIntelligenceSettings() }
                                .buttonStyle(.compact)
                        }
                        Button("Meeting settings") { model.showSettings(.meetings) }
                            .buttonStyle(.compact)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
                .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard).strokeBorder(Theme.border, lineWidth: 1))
                .padding(.bottom, 16)
            }
            if let running = live.active?.snapshot {
                // A recording the panel started — a standing series, a call
                // the person accepted from a notification — runs with no live
                // screen open. The row is the way in; the panel keeps its Stop.
                PageSection("Recording now") {
                    Card {
                        CardRow(action: { live.open(running.sessionId) }) {
                            HStack(spacing: 8) {
                                LiveDot(state: running.phase == .capturingPaused ? .idle : .recording(since: Date()))
                                Text(running.title).bodyText(14)
                            }
                        } trailing: {
                            Text(running.phase == .capturingPaused ? "Paused" : "Open")
                                .metaText(Theme.inkSecondary)
                        }
                    }
                }
            }
            MeetingSuggestionsView(store: live)
            MeetingStartCountdownView(store: live)
            MeetingRecoveryView(store: live, onOpenSession: model.openMeeting)
            UpcomingView(store: live) { model.showSettings(.meetings) }
            MeetingsNoticeBand(store: meetings)
            if let message = meetings.listError {
                MeetingsRetryNote(message: message) { meetings.retry() }
            }
            MeetingsFolderBar(store: meetings) { folder in
                model.chat.scope(to: folder)
                model.sheet = .chat
            }
            MeetingsFilterCard(store: meetings)
            MeetingsFeed(store: meetings)
            MeetingsPager(store: meetings)
            MeetingsTrendCard(store: meetings)
        }
        .sheet(isPresented: Binding(get: { meetings.trashOpen }, set: { if !$0 { meetings.closeTrash() } })) {
            MeetingsTrashSheet(store: meetings)
        }
        .task { await live.loadUpcoming() }
        .task { await live.watchEngine() }
    }
}

struct Sidebar: View {
    @Environment(AppModel.self) private var model
    @State private var draggedFavorite: WorkFavoriteEntry?
    @State private var favoriteDrop: FavoriteDrop?
    @State private var endTargeted = false
    @State private var renamingFolder: String?
    @AppStorage("sidebar.closedFavoriteFolders") private var closedFolders = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Button {
                    model.paletteShown = true
                } label: {
                    HStack(spacing: Space.s) {
                        Image(systemName: "magnifyingglass")
                            .font(.system(size: 12, weight: .semibold))
                            .frame(width: 16)
                        Text("Search")
                        Spacer(minLength: 4)
                        Text("⌘K").font(TypeScale.body(12))
                    }
                    .font(TypeScale.list)
                    .foregroundStyle(Theme.inkTertiary)
                    .padding(.leading, Space.s)
                    .padding(.trailing, 10)
                    .frame(height: Theme.sidebarRow)
                    .background(Theme.wash, in: RoundedRectangle(cornerRadius: Theme.radiusRow))
                    .contentShape(RoundedRectangle(cornerRadius: Theme.radiusRow))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Search")
                Button {
                    model.sheet = .chat
                } label: {
                    Image(systemName: "bubble.left")
                }
                .buttonStyle(.icon)
                .overlay(alignment: .topTrailing) {
                    if model.agents.pending.count > 0 {
                        Text("\(model.agents.pending.count)")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(Theme.onAccent)
                            .padding(.horizontal, 4)
                            .frame(minWidth: 16, minHeight: 16)
                            .background(Theme.accentFill, in: Capsule())
                            .offset(x: 4, y: -4)
                            .allowsHitTesting(false)
                    }
                }
                .accessibilityLabel("Chat")
                .accessibilityValue(model.agents.pending.isEmpty ? "" : "\(model.agents.pending.count) waiting")
            }
            .frame(height: Theme.headerHeight)
            .padding(.bottom, Space.xs)

            ScrollView {
                VStack(alignment: .leading, spacing: Space.xxs) {
                    let favorites = model.work.favorites
                    let favoriteShown = favorites.contains(where: favoriteOpen)
                    ForEach(Place.sidebar) { place in
                        SidebarItem(
                            icon: place.icon,
                            title: place.title,
                            selected: model.place.parent == place && !model.showingSettings && !favoriteShown,
                            count: count(place),
                            live: place == .capture && model.capture != .idle
                                || place == .meetings && model.live.live != nil
                        ) {
                            model.go(place)
                        }
                    }
                    favoritesSection
                    gap
                        .overlay(alignment: .top) {
                            if endTargeted { Theme.accent.frame(height: 2).allowsHitTesting(false) }
                        }
                        .onDrop(of: [.utf8PlainText], isTargeted: $endTargeted) { _ in
                            guard let dragged = draggedFavorite else { return false }
                            model.work.moveFavorite(dragged, into: nil, at: nil)
                            draggedFavorite = nil
                            return true
                        }
                    SidebarItem(
                        icon: Place.loops.icon,
                        title: Place.loops.title,
                        selected: model.place == .loops && !model.showingSettings && !favoriteShown
                    ) {
                        model.go(.loops)
                    }
                    SidebarItem(
                        icon: "gearshape",
                        title: "Settings",
                        selected: false
                    ) {
                        model.showSettings(model.showingSettings ? model.settingsPlace : .essentials)
                    }
                    .accessibilityAddTraits(model.showingSettings ? .isSelected : [])
                    if model.showingSettings {
                        ForEach(SettingsPlace.allCases) { page in
                            SidebarSubItem(title: page.title, selected: model.settingsPlace == page) {
                                model.showSettings(page)
                            }
                        }
                    }
                }
                .padding(.bottom, Space.m)
            }
            .scrollIndicators(.never)
            SidebarProfileRow()
                .padding(.bottom, Space.m)
        }
        .padding(.horizontal, Space.m)
        .frame(width: Theme.sidebarWidth)
        .frame(maxHeight: .infinity, alignment: .top)
        .sidebarSurface(model.about.material)
        .keepsWindowToolbar()
    }

    private func favoriteOpen(_ favorite: WorkFavorite) -> Bool {
        guard !model.showingSettings else { return false }
        switch favorite.kind {
        case .task:
            return model.place == .tasks && model.workFocus.opened(in: .tasks) == favorite.targetId
                && model.workRows([favorite.targetId]).first != nil
        case .project:
            return model.place == .projects && model.projects.selectedProjectId == favorite.targetId
                && model.work.projects.contains { $0.id == favorite.targetId }
        case .view:
            return model.place.parent == .tasks && model.workFocus.tab == .view(favorite.targetId)
                && model.work.views.contains { $0.id == favorite.targetId }
        case .person:
            return model.place == .people && model.people.route == .person(favorite.targetId)
        case .unknown:
            return false
        }
    }

    /// Favorites at the top and folders of them, in the user's order. A row
    /// drags to another place, into a folder, or out of one.
    @ViewBuilder
    private var favoritesSection: some View {
        let store = model.work
        let top = store.favoriteTop
        if !top.isEmpty {
            Text("Favorites")
                .font(TypeScale.meta)
                .foregroundStyle(Theme.inkTertiary)
                .padding(.horizontal, Space.s)
                .padding(.top, Space.m)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
                .contextMenu { Button("New Folder") { newFolder(holding: nil) } }
                .accessibilityAddTraits(.isHeader)
        }
        ForEach(top, id: \.self) { entry in
            switch entry {
            case let .favorite(kind, id):
                if let favorite = store.favorites.first(where: { $0.kind == kind && $0.targetId == id }) {
                    favoriteRow(favorite)
                        .modifier(favoritePlace(entry, in: nil))
                        .contextMenu { favoriteMenu(favorite) }
                }
            case let .folder(id):
                if let folder = store.favoriteFolders.first(where: { $0.id == id }) {
                    let open = !closedFolders.split(separator: ",").contains(Substring(id))
                    FavoriteFolderRow(
                        folder: folder,
                        open: open,
                        dropInto: favoriteDrop == .into(id),
                        renaming: Binding(get: { renamingFolder == id }, set: { if !$0 { renamingFolder = nil } }),
                        toggle: { toggleFolder(id) },
                        rename: { store.renameFavoriteFolder(id, $0) })
                    .modifier(favoritePlace(entry, in: nil))
                    .contextMenu {
                        Button("Rename…") { renamingFolder = id }
                        Button("Delete Folder") { store.deleteFavoriteFolder(id) }
                    }
                    if open {
                        ForEach(store.favorites(in: id), id: \.self) { favorite in
                            favoriteRow(favorite)
                                .padding(.leading, Space.l)
                                .modifier(favoritePlace(favorite.entry, in: id))
                                .contextMenu { favoriteMenu(favorite) }
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func favoriteMenu(_ favorite: WorkFavorite) -> some View {
        let store = model.work
        Menu("Move to Folder") {
            ForEach(store.favoriteFolders.sorted { $0.position < $1.position }) { folder in
                Button(folder.name) { store.moveFavorite(favorite.entry, into: folder.id, at: nil) }
                    .disabled(favorite.folderId == folder.id)
            }
            if !store.favoriteFolders.isEmpty { Divider() }
            Button("New Folder") { newFolder(holding: favorite.entry) }
        }
        if favorite.folderId != nil {
            Button("Move Out of Folder") { store.moveFavorite(favorite.entry, into: nil, at: nil) }
        }
        Divider()
        Button("Remove from Favorites") { store.setFavorite(favorite.kind, favorite.targetId, false) }
    }

    private func favoritePlace(_ entry: WorkFavoriteEntry, in folder: String?) -> FavoritePlace {
        FavoritePlace(entry: entry, folder: folder, dragged: $draggedFavorite, drop: $favoriteDrop) { dragged, drop in
            landFavorite(dragged, drop)
        }
    }

    private func landFavorite(_ dragged: WorkFavoriteEntry, _ drop: FavoriteDrop) {
        let store = model.work
        switch drop {
        case let .into(folder):
            store.moveFavorite(dragged, into: folder, at: nil)
        case let .beside(target, folder, after):
            let order = folder.map { store.favorites(in: $0).map(\.entry) } ?? store.favoriteTop
            guard let at = order.filter({ $0 != dragged }).firstIndex(of: target) else { return }
            store.moveFavorite(dragged, into: folder, at: after ? at + 1 : at)
        }
    }

    private func newFolder(holding entry: WorkFavoriteEntry?) {
        Task {
            if let folder = await model.work.createFavoriteFolder("New folder", holding: entry) {
                renamingFolder = folder.id
            }
        }
    }

    private func toggleFolder(_ id: String) {
        var closed = closedFolders.split(separator: ",").map(String.init)
        if let at = closed.firstIndex(of: id) {
            closed.remove(at: at)
        } else {
            closed.append(id)
        }
        closedFolders = closed.joined(separator: ",")
    }

    @ViewBuilder
    private func favoriteRow(_ favorite: WorkFavorite) -> some View {
        let focus = model.workFocus
        switch favorite.kind {
        case .task:
            if let row = model.workRows([favorite.targetId]).first {
                SidebarItem(icon: "star", title: row.title, selected: favoriteOpen(favorite)) {
                    model.openTask(row.id)
                }
            }
        case .project:
            if let project = model.work.projects.first(where: { $0.id == favorite.targetId }) {
                SidebarItem(icon: "star", title: project.title, selected: favoriteOpen(favorite)) {
                    model.openProject(project.id)
                }
            }
        case .view:
            if let view = model.work.views.first(where: { $0.id == favorite.targetId }) {
                SidebarItem(icon: "star", title: view.name, selected: favoriteOpen(favorite)) {
                    model.go(.tasks)
                    focus.switchTab(.view(view.id))
                }
            }
        case .person:
            if let person = model.people.entries?.first(where: { $0.person.id == favorite.targetId })?.person {
                SidebarItem(icon: "star", title: person.displayName, selected: favoriteOpen(favorite)) {
                    model.openPerson(person.id)
                }
            }
        case .unknown:
            EmptyView()
        }
    }

    /// The small live number beside a row: what needs the reader there.
    private func count(_ place: Place) -> SidebarCount? {
        switch place {
        case .people:
            SidebarCount(value: Int(model.people.entries?.reduce(0) { $0 + $1.suggestedCount } ?? 0),
                         meaning: "to confirm")
        case .shared:
            SidebarCount(value: model.shared.drafts.count, meaning: "drafts to check")
        case .tasks:
            SidebarCount(value: model.work.counts.map { $0.mineOverdue + $0.mineDueToday } ?? 0,
                         meaning: "due today or late")
        case .inbox:
            SidebarCount(value: model.work.unread + (model.work.counts?.triage ?? 0), meaning: "unread or to triage")
        case .capture, .library, .activity, .meetings, .scratchpad, .calendar, .today, .projects, .archive, .loops,
             .initiatives, .triage:
            nil
        }
    }

    private var gap: some View {
        Spacer().frame(height: Space.l)
    }
}

/// Where a dragged favorite or folder lands: beside a row in that row's
/// folder or at the top, or into a folder.
private enum FavoriteDrop: Equatable {
    case beside(WorkFavoriteEntry, folder: String?, after: Bool)
    case into(String)
}

/// Makes a favorites row draggable and a place to drop another: the upper
/// half lands before it, the lower half after it, and a favorite over the
/// lower part of a folder row lands inside that folder.
private struct FavoritePlace: ViewModifier, DropDelegate {
    let entry: WorkFavoriteEntry
    let folder: String?
    @Binding var dragged: WorkFavoriteEntry?
    @Binding var drop: FavoriteDrop?
    let land: (WorkFavoriteEntry, FavoriteDrop) -> Void

    func body(content: Content) -> some View {
        content
            .overlay(alignment: shown?.after == true ? .bottom : .top) {
                if shown != nil {
                    Theme.accent.frame(height: 2).allowsHitTesting(false)
                }
            }
            .onDrag {
                dragged = entry
                return NSItemProvider(object: "sona favorite" as NSString)
            }
            .onDrop(of: [.utf8PlainText], delegate: self)
    }

    private var shown: (target: WorkFavoriteEntry, after: Bool)? {
        guard case let .beside(target, folder, after) = drop, target == entry, folder == self.folder else { return nil }
        return (target, after)
    }

    private func landing(_ info: DropInfo) -> FavoriteDrop? {
        guard let dragged, dragged != entry else { return nil }
        let y = info.location.y
        switch (dragged, entry) {
        case (.folder, _) where folder != nil:
            return nil
        case let (.favorite, .folder(id)) where y > Theme.sidebarRow / 3:
            return .into(id)
        default:
            return .beside(entry, folder: folder, after: y > Theme.sidebarRow / 2)
        }
    }

    func validateDrop(info: DropInfo) -> Bool { landing(info) != nil }

    func dropUpdated(info: DropInfo) -> DropProposal? {
        drop = landing(info)
        return DropProposal(operation: .move)
    }

    func dropExited(info: DropInfo) { drop = nil }

    func performDrop(info: DropInfo) -> Bool {
        defer { (drop, dragged) = (nil, nil) }
        guard let dragged, let landing = landing(info) else { return false }
        land(dragged, landing)
        return true
    }
}

/// A favorites folder: a chevron that opens and closes it, and its name,
/// edited in place on Rename.
private struct FavoriteFolderRow: View {
    let folder: WorkFavoriteFolder
    let open: Bool
    let dropInto: Bool
    @Binding var renaming: Bool
    let toggle: () -> Void
    let rename: (String) -> Void
    @State private var name = ""
    @State private var hovering = false
    @FocusState private var editing: Bool

    var body: some View {
        HStack(spacing: Space.s) {
            Image(systemName: open ? "folder" : "folder.fill")
                .font(.system(size: 13))
                .frame(width: 16)
            if renaming {
                TextField("Folder name", text: $name)
                    .textFieldStyle(.plain)
                    .font(TypeScale.list)
                    .focused($editing)
                    .onAppear {
                        name = folder.name
                        editing = true
                    }
                    .onSubmit {
                        rename(name)
                        renaming = false
                    }
                    .onExitCommand { renaming = false }
                    .onChange(of: editing) { _, now in if !now { renaming = false } }
            } else {
                Text(folder.name)
                    .font(TypeScale.list)
                    .lineLimit(1)
            }
            Spacer(minLength: 4)
            Image(systemName: "chevron.right")
                .font(.system(size: 10, weight: .semibold))
                .rotationEffect(.degrees(open ? 90 : 0))
                .foregroundStyle(Theme.inkTertiary)
        }
        .foregroundStyle(Theme.inkSecondary)
        .padding(.horizontal, Space.s)
        .frame(height: Theme.sidebarRow)
        .background(dropInto || hovering ? Theme.wash : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusRow))
        .hoverFade(hovering)
        .contentShape(Rectangle())
        .onTapGesture { if !renaming { toggle() } }
        .onHover { hovering = $0 }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityLabel("Folder \(folder.name)")
        .accessibilityValue(open ? "Open" : "Closed")
        .accessibilityAction { toggle() }
    }
}

/// How far the floating sidebar sits inside the window's edges.
private let sidebarInset: CGFloat = 6

private extension View {
    /// The sidebar is navigation, the layer Liquid Glass is for: with the
    /// glass material it floats as a glass pane inset from the window's
    /// edges, its corners concentric with the window's, over the page's own
    /// color. The split view's own sidebar is flat on macOS 27, so the pane
    /// is drawn here. Otherwise it keeps the opaque rail.
    @ViewBuilder
    func sidebarSurface(_ material: AppearanceMaterial) -> some View {
        if material == .glass, #available(macOS 26.0, *) {
            background {
                ZStack {
                    Theme.page
                    Color.clear
                        .glassEffect(.regular, in: ConcentricRectangle(
                            corners: .concentric(minimum: .fixed(Theme.radiusCard)), isUniform: true))
                        .padding(sidebarInset)
                }
                .ignoresSafeArea()
            }
        } else {
            background(Theme.rail.ignoresSafeArea())
        }
    }
}

private struct SidebarCount {
    let value: Int
    let meaning: String
}

/// One place in the sidebar: a monochrome glyph lined up under the window's
/// close button, the name, and a count when there is one.
private struct SidebarItem: View {
    let icon: String
    let title: String
    let selected: Bool
    var count: SidebarCount?
    var live = false
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: Space.s) {
                Image(systemName: icon)
                    .font(.system(size: 13))
                    .frame(width: 16)
                Text(title)
                    .font(TypeScale.list)
                Spacer(minLength: 4)
                if let count, count.value > 0 {
                    Text("\(count.value)")
                        .font(TypeScale.meta)
                        .foregroundStyle(Theme.inkTertiary)
                        .contentTransition(.numericText(value: Double(count.value)))
                }
                if live {
                    Circle().fill(Theme.live).frame(width: 7, height: 7)
                }
            }
            .foregroundStyle(selected ? Theme.ink : Theme.inkSecondary)
            .padding(.horizontal, Space.s)
            .frame(height: Theme.sidebarRow)
            .background(
                selected ? Theme.washSelected : (hovering ? Theme.wash : .clear),
                in: RoundedRectangle(cornerRadius: Theme.radiusRow)
            )
            .hoverFade(hovering)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
        .accessibilityAddTraits(selected ? .isSelected : [])
        .accessibilityValue(live ? "In progress" : count.flatMap { $0.value > 0 ? "\($0.value) \($0.meaning)" : nil } ?? "")
        .onHover { hovering = $0 }
    }
}

/// A page under Settings, lined up with the titles above it.
private struct SidebarSubItem: View {
    let title: String
    let selected: Bool
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(TypeScale.list)
                .foregroundStyle(selected ? Theme.ink : Theme.inkSecondary)
                .padding(.leading, Space.s + 16 + Space.s)
                .frame(maxWidth: .infinity, minHeight: Theme.sidebarRow, alignment: .leading)
                .background(
                    selected ? Theme.washSelected : (hovering ? Theme.wash : .clear),
                    in: RoundedRectangle(cornerRadius: Theme.radiusRow)
                )
                .hoverFade(hovering)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
        .onHover { hovering = $0 }
    }
}

/// The elapsed clock. It ticks once a second and never animates between values.
struct CaptureClock: View {
    let state: CaptureState
    var idleText: String

    var body: some View {
        switch state {
        case .idle:
            Text(idleText)
        case let .recording(since):
            TimelineView(.periodic(from: since, by: 1)) { context in
                Text("Recording \(context.date.timeIntervalSince(since).clock)")
            }
        case let .working(kind):
            Text(kind.capitalized)
        }
    }
}
