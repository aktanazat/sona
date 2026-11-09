import SwiftUI

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
                    openPerson: model.openPerson,
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
            }
        }
    }
}

/// Home's two faces, the overview and every dictation, as the start of
/// Home's bar.
struct HomeTabs: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        SegmentedSwitch(items: [SwitchItem(id: Place.capture, title: "Overview", icon: "house"),
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
            PageTitle("Meetings", subtitle: subtitle) {
                HStack(spacing: 8) {
                    Menu {
                        Button("Export all as CSV…") { meetings.exportAllCsv() }
                            .disabled(meetings.busy)
                    } label: {
                        Image(systemName: "ellipsis")
                    }
                    .menuStyle(.button)
                    .buttonStyle(.icon)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .help("Meeting library options")
                    .accessibilityLabel("Meeting library options")
                    Button { meetings.openTrash() } label: {
                        Image(systemName: "trash")
                    }
                    .buttonStyle(.icon)
                    .help("Deleted meetings")
                    .accessibilityLabel("Deleted")
                    Button { live.importMeeting() } label: {
                        Image(systemName: "square.and.arrow.down")
                    }
                    .buttonStyle(.icon)
                    .help("Import recording")
                    .accessibilityLabel("Import recording")
                    .disabled(live.importing)
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

    /// What Sona has recorded in total, once the trend says.
    private var subtitle: String {
        guard let allTime = model.meetings.trend?.allTime, allTime.meetings > 0 else {
            return ""
        }
        let meetings = allTime.meetings == 1 ? "1 meeting" : "\(allTime.meetings) meetings"
        return "\(meetings) recorded · \(allTime.capturedSpoken) captured"
    }
}

struct Sidebar: View {
    @Environment(AppModel.self) private var model

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
                    ForEach(Place.sidebar) { place in
                        SidebarItem(
                            icon: place.icon,
                            title: place.title,
                            selected: model.place.parent == place && !model.showingSettings,
                            count: count(place),
                            live: place == .capture && model.capture != .idle
                                || place == .meetings && model.live.live != nil
                        ) {
                            model.go(place)
                        }
                    }
                    gap
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
        }
        .padding(.horizontal, Space.m)
        .frame(width: Theme.sidebarWidth)
        .frame(maxHeight: .infinity, alignment: .top)
        .sidebarSurface(model.about.material)
        .keepsWindowToolbar()
    }

    /// The small live number beside a row: open tasks in active projects,
    /// people waiting to be confirmed.
    private func count(_ place: Place) -> SidebarCount? {
        switch place {
        case .projects:
            SidebarCount(value: model.projects.activeOpenTaskCount, meaning: "open tasks")
        case .people:
            SidebarCount(value: Int(model.people.entries?.reduce(0) { $0 + $1.suggestedCount } ?? 0),
                         meaning: "to confirm")
        case .triage:
            SidebarCount(value: model.work.counts?.triage ?? 0, meaning: "to triage")
        case .tasks:
            SidebarCount(value: model.work.counts.map { $0.mineOverdue + $0.mineDueToday } ?? 0,
                         meaning: "due today or late")
        case .capture, .library, .meetings, .scratchpad, .calendar, .today:
            nil
        }
    }

    private var gap: some View {
        Spacer().frame(height: Space.l)
    }
}

private extension View {
    /// The sidebar is navigation, the layer Liquid Glass is for: with the
    /// glass material on macOS 26 the split view floats it as the system's
    /// own glass pane, the window's buttons inside it. Otherwise it keeps the
    /// opaque rail.
    @ViewBuilder
    func sidebarSurface(_ material: AppearanceMaterial) -> some View {
        if material == .glass, #available(macOS 26.0, *) {
            self
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
