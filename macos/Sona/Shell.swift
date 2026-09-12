import SwiftUI

/// The main window: the sidebar on the left, one page on the right.
struct Shell: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        ZStack {
            HStack(spacing: 0) {
                Sidebar()
                content
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            if model.paletteShown {
                CommandPalette()
            }
        }
        .background(Theme.page.ignoresSafeArea())
        .frame(minWidth: 1040, minHeight: 700)
        .sheet(isPresented: Bindable(model).chatShown) {
            ChatSheet()
        }
        .background(
            Group {
                Button("") { model.paletteShown.toggle() }.keyboardShortcut("k", modifiers: .command)
                Button("") { model.toggleCapture() }.keyboardShortcut("r", modifiers: .command)
                Button("") { model.showingSettings = true }.keyboardShortcut(",", modifiers: .command)
            }
            .hidden()
        )
    }

    @ViewBuilder
    private var content: some View {
        if model.showingSettings {
            SettingsScreen()
        } else {
            switch model.place {
            case .capture: CaptureScreen()
            case .library: LibraryScreen()
            case .meetings: MeetingsScreen()
            case .people: PeopleScreen()
            }
        }
    }
}

/// Wordmark, search, chat, the four places, settings. The traffic lights sit
/// above the wordmark, so the column starts below them.
struct Sidebar: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "waveform")
                    .font(.system(size: 15, weight: .semibold))
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
            SidebarItem(icon: "bubble.left", title: "Chat", shortcut: nil, selected: false) {
                model.chatShown = true
            }

            Spacer().frame(height: 16)

            ForEach(Place.allCases) { place in
                SidebarItem(
                    icon: place.icon,
                    title: place.title,
                    shortcut: nil,
                    selected: model.place == place && !model.showingSettings,
                    live: place == .capture && model.capture != .idle
                ) {
                    model.go(place)
                }
            }
            SidebarItem(icon: "gearshape", title: "Settings", shortcut: nil, selected: model.showingSettings) {
                model.showingSettings = true
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
                if let shortcut {
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
