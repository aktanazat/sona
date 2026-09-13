import SwiftUI

/// The main window: the sidebar on the left, one page on the right. Before
/// the core answers, a wait; on a first run, the onboarding flow instead of
/// the pages.
struct Shell: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        ZStack {
            if !model.ready {
                waiting
            } else if model.onboarding.step != .done {
                OnboardingView(store: model.onboarding)
            } else {
                HStack(spacing: 0) {
                    Sidebar()
                    content
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
                if model.paletteShown {
                    CommandPalette()
                }
            }
        }
        .background(Theme.page.ignoresSafeArea())
        .frame(minWidth: 1040, minHeight: 700)
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
            case .whatsNew:
                WhatsNewView(store: model.debug.whatsNew) {
                    model.debug.whatsNew.dismiss()
                    model.sheet = nil
                }
            }
        }
        .background {
            Group {
                Button("") { model.paletteShown.toggle() }.keyboardShortcut("k", modifiers: .command)
                Button("") { model.toggleCapture() }.keyboardShortcut("r", modifiers: .command)
                Button("") { model.showSettings(model.settingsPlace) }.keyboardShortcut(",", modifiers: .command)
            }
            .hidden()
        }
    }

    /// The core is starting, or could not.
    private var waiting: some View {
        VStack(spacing: 12) {
            if let error = model.coreError {
                Text("Sona's engine could not start.").bodyText(15)
                Text(error).bodyText(13, Theme.inkSecondary)
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
            }
        }
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
            MeetingReviewView(store: meetings, openPerson: model.openPerson)
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
                    Button("Deleted") { meetings.openTrash() }
                        .buttonStyle(.secondary)
                    Button("Import recording") { live.importMeeting() }
                        .buttonStyle(.secondary)
                        .disabled(live.importing)
                    Button(live.starting ? "Starting…" : "Record") { live.startManual() }
                        .buttonStyle(.primary)
                        .disabled(live.starting || live.pending != nil)
                }
            }
            ErrorNote(live.error)
            if let notice = live.notice {
                Text(notice).bodyText(13, Theme.inkSecondary).padding(.bottom, 16)
            }
            if let warning = live.engine?.warning {
                Text(warning).bodyText(13, Theme.live).padding(.bottom, 16)
            }
            MeetingSuggestionsView(store: live)
            MeetingStartCountdownView(store: live)
            MeetingRecoveryView(store: live, onOpenSession: model.openMeeting)
            UpcomingView(store: live) { model.showSettings(.meetings) }
            MeetingsNoticeBand(store: meetings)
            if let message = meetings.listError {
                MeetingsRetryNote(message: message) { meetings.retry() }
            }
            MeetingsTrendCard(store: meetings)
            MeetingsFilterCard(store: meetings)
            MeetingsFeed(store: meetings)
            MeetingsPager(store: meetings)
        }
        .sheet(isPresented: Binding(get: { meetings.trashOpen }, set: { if !$0 { meetings.closeTrash() } })) {
            MeetingsTrashSheet(store: meetings)
        }
        .task { await live.loadUpcoming() }
    }

    /// What Sona has recorded in total, once the trend says.
    private var subtitle: String {
        guard let allTime = model.meetings.trend?.allTime, allTime.meetings > 0 else {
            return "Record a meeting on this Mac."
        }
        let meetings = allTime.meetings == 1 ? "1 meeting" : "\(allTime.meetings) meetings"
        return "\(meetings) recorded · \(allTime.capturedSpoken) captured"
    }
}

/// Wordmark, search, chat, the four places, settings. The traffic lights sit
/// above the wordmark, so the column starts below them.
struct Sidebar: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Image("Mark")
                Text("Sona")
                    .font(.system(size: 17, weight: .semibold))
            }
            .foregroundStyle(Theme.ink)
            .padding(.leading, 12)
            .padding(.top, 52)
            .padding(.bottom, 28)

            SidebarItem(icon: "magnifyingglass", title: "Search", shortcut: "⌘ K", selected: false) {
                model.paletteShown = true
            }
            SidebarItem(
                icon: "bubble.left", title: "Chat", shortcut: nil, selected: false,
                badge: model.agents.pending.count
            ) {
                model.sheet = .chat
            }

            Spacer().frame(height: 16)

            ForEach(Place.allCases) { place in
                SidebarItem(
                    icon: place.icon,
                    title: place.title,
                    shortcut: nil,
                    selected: model.place == place && !model.showingSettings,
                    live: place == .capture && model.capture != .idle
                        || place == .meetings && model.live.live != nil
                ) {
                    model.go(place)
                }
            }
            SidebarItem(icon: "gearshape", title: "Settings", shortcut: nil, selected: model.showingSettings) {
                model.showSettings(model.settingsPlace)
            }
            Spacer()
        }
        .padding(.horizontal, 12)
        .frame(width: Theme.sidebarWidth)
        .frame(maxHeight: .infinity)
        .overlay(alignment: .trailing) {
            Rectangle().fill(Theme.border).frame(width: 1)
        }
    }
}

private struct SidebarItem: View {
    let icon: String
    let title: String
    let shortcut: String?
    let selected: Bool
    var live = false
    var badge = 0
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: icon)
                    .font(.system(size: 15, weight: .regular))
                    .foregroundStyle(Theme.inkSecondary)
                    .frame(width: 18)
                Text(title)
                    .font(TypeScale.body(16))
                    .foregroundStyle(Theme.ink)
                if live {
                    Circle().fill(Theme.live).frame(width: 7, height: 7)
                }
                Spacer()
                if badge > 0 {
                    Text("\(badge)")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.onInvert)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Theme.invert, in: Capsule())
                } else if let shortcut {
                    Text(shortcut).metaText()
                }
            }
            .padding(.horizontal, 12)
            .frame(height: 44)
            .background(
                selected ? Theme.selection : (hovering ? Theme.wash : .clear),
                in: RoundedRectangle(cornerRadius: 12)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
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
