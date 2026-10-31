import SwiftUI

/// The main window: the sidebar on the left, one page on the right. Before
/// the core answers, a wait; on a first run, the onboarding flow instead of
/// the pages.
struct Shell: View {
    @Environment(AppModel.self) private var model
    @State private var sharedNote: ScratchNote?

    var body: some View {
        ZStack {
            if !model.ready {
                waiting
            } else if model.onboarding.step != .done {
                OnboardingView(store: model.onboarding)
            } else {
                HStack(spacing: 0) {
                    Sidebar()
                    VStack(spacing: 0) {
                        if model.coreStopped {
                            CoreStoppedBanner()
                        }
                        content
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                }
                if model.paletteShown {
                    CommandPalette()
                }
            }
        }
        .background(Theme.page.ignoresSafeArea())
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
                    ingestDocument: { model.showSettings(.importing) },
                    deleteDocument: { id in Task { await model.documents.delete(id: id) } },
                    openVocabulary: { model.showSettings(.vocabulary) })
            case .scratchpad:
                ScratchpadView(store: model.scratchpad, share: { note in
                    Task {
                        await model.scratchpad.flush()
                        guard let saved = model.scratchpad.saved,
                              saved.id == note.id,
                              saved.body == model.scratchpad.body else { return }
                        sharedNote = saved
                    }
                })
                    .sheet(item: $sharedNote) { note in
                        CloudNoteShareView(note: note)
                    }
            }
        }
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
            MeetingLiveView(store: live, onOpenReview: model.openMeeting)
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
                    .buttonStyle(.circle)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .help("Meeting library options")
                    .accessibilityLabel("Meeting library options")
                    Button { meetings.openTrash() } label: {
                        Image(systemName: "trash")
                    }
                    .buttonStyle(.circle)
                    .help("Deleted meetings")
                    .accessibilityLabel("Deleted")
                    Button { live.importMeeting() } label: {
                        Image(systemName: "square.and.arrow.down")
                    }
                    .buttonStyle(.circle)
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

    /// Settings pages with a row of their own above Settings.
    private static let ownRows: [SettingsPlace] = [.modes, .vocabulary, .models]
    private static let settingsPages = SettingsPlace.allCases.filter { !ownRows.contains($0) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Button {
                    model.paletteShown = true
                } label: {
                    HStack(spacing: 6) {
                        Image(systemName: "magnifyingglass")
                            .font(.system(size: 12, weight: .semibold))
                        Text("Search")
                        Spacer(minLength: 4)
                        Text("⌘K").font(TypeScale.body(12))
                    }
                    .font(TypeScale.body(13))
                    .foregroundStyle(Theme.inkTertiary)
                    .padding(.horizontal, 10)
                    .frame(height: 28)
                    .background(Theme.wash, in: Capsule())
                    .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Search")
                Button {
                    model.sheet = .chat
                } label: {
                    Image(systemName: "bubble.left")
                }
                .buttonStyle(.circle)
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
            .padding(.horizontal, 4)
            .padding(.top, 8)
            .padding(.bottom, 16)

            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(Place.allCases) { place in
                        SidebarItem(
                            icon: place.icon,
                            tile: place == .capture ? Theme.tileHome : Theme.tilePlace,
                            title: place.title,
                            selected: model.place == place && !model.showingSettings,
                            live: place == .capture && model.capture != .idle
                                || place == .meetings && model.live.live != nil
                        ) {
                            model.go(place)
                        }
                    }
                    gap
                    settingsRow(.modes, icon: "sparkles", tile: Theme.tilePlace)
                    settingsRow(.vocabulary, icon: "book.closed.fill", tile: Theme.tilePlace)
                    gap
                    settingsRow(.models, icon: "square.stack.3d.up.fill", tile: Theme.tileSetting)
                    SidebarItem(
                        icon: "gearshape.fill",
                        tile: Theme.tileSetting,
                        title: "Settings",
                        selected: false
                    ) {
                        model.showSettings(settingsOpen ? model.settingsPlace : .essentials)
                    }
                    .accessibilityAddTraits(settingsOpen ? .isSelected : [])
                    if settingsOpen {
                        ForEach(Self.settingsPages) { page in
                            SidebarSubItem(title: page.title, selected: model.settingsPlace == page) {
                                model.showSettings(page)
                            }
                        }
                    }
                }
                .padding(.horizontal, 4)
                .padding(.bottom, 12)
            }
            .scrollIndicators(.never)
        }
        .padding(.horizontal, 8)
        .frame(width: Theme.sidebarWidth)
        .frame(maxHeight: .infinity, alignment: .top)
        .background(Theme.rail.ignoresSafeArea())
        .overlay(alignment: .trailing) {
            Rectangle().fill(Theme.border).frame(width: 1).ignoresSafeArea()
        }
    }

    /// Settings is open on one of the pages that sit under it.
    private var settingsOpen: Bool {
        model.showingSettings && !Self.ownRows.contains(model.settingsPlace)
    }

    private var gap: some View {
        Spacer().frame(height: 14)
    }

    private func settingsRow(_ page: SettingsPlace, icon: String, tile: Color) -> some View {
        SidebarItem(
            icon: icon,
            tile: tile,
            title: page.title,
            selected: model.showingSettings && model.settingsPlace == page
        ) {
            model.showSettings(page)
        }
    }
}

private struct SidebarItem: View {
    let icon: String
    let tile: Color
    let title: String
    let selected: Bool
    var live = false
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: icon)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 22, height: 22)
                    .background(tile, in: RoundedRectangle(cornerRadius: 6))
                Text(title)
                    .font(TypeScale.body(14))
                    .foregroundStyle(Theme.ink)
                Spacer(minLength: 4)
                if live {
                    Circle().fill(Theme.live).frame(width: 7, height: 7)
                }
            }
            .padding(.horizontal, 8)
            .frame(height: 34)
            .background(
                selected ? Theme.selection : (hovering ? Theme.wash : .clear),
                in: RoundedRectangle(cornerRadius: 10)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
        .accessibilityAddTraits(selected ? .isSelected : [])
        .accessibilityValue(live ? "In progress" : "")
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
                .font(TypeScale.body(13))
                .foregroundStyle(selected ? Theme.ink : Theme.inkSecondary)
                .padding(.leading, 40)
                .frame(maxWidth: .infinity, minHeight: 28, alignment: .leading)
                .background(
                    selected ? Theme.selection : (hovering ? Theme.wash : .clear),
                    in: RoundedRectangle(cornerRadius: 8)
                )
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
