import SwiftUI

/// The pill that floats above other windows. While you talk, only the sound:
/// eleven bars, the last quarter second of the microphone, newest on the
/// right, flat while nothing is being heard. Idle, the name of the mode the
/// next recording goes under, and a press starts it.
struct HUDPill: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Group {
            if model.capture == .idle {
                Button(action: model.toggleCapture) {
                    HStack(spacing: 6) {
                        Image(systemName: "mic")
                            .font(.system(size: 10, weight: .semibold))
                        Text(model.pillMode ?? "Dictate")
                            .font(.system(size: 11, weight: .medium))
                            .lineLimit(1)
                    }
                    .foregroundStyle(Theme.onInvert)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .background(Theme.invert, in: Capsule())
                }
                .buttonStyle(.plain)
            } else {
                HStack(spacing: 3) {
                    ForEach(Array(model.meter.history.enumerated()), id: \.offset) { _, level in
                        RoundedRectangle(cornerRadius: 1)
                            .fill(Theme.onInvert)
                            .frame(width: 2, height: 2 + 12 * level)
                    }
                }
                .frame(height: 14)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(Theme.invert, in: Capsule())
            }
        }
        .padding(8)
    }
}

/// Where a floating panel sits on the screen the pointer is on.
enum PanelPlacement: Equatable {
    /// Centred along the top or bottom edge.
    case edge(OverlayPosition)
    /// Tucked into the top right corner.
    case topTrailing
}

/// A window that floats above other windows and on every space, without
/// taking the keyboard away from the app the words are going into, which a
/// SwiftUI `Window` scene would. Shown with a fresh view each time; the
/// hosting view keeps it, so a `show` with the same content redraws in
/// place.
@MainActor
final class FloatingPanel {
    private var panel: NSPanel?
    private var host: NSHostingView<AnyView>?

    func show(_ content: some View, at placement: PanelPlacement) {
        let panel = panel ?? make()
        self.panel = panel
        host?.rootView = AnyView(content)
        panel.setContentSize(host?.fittingSize ?? .zero)
        if let screen = NSScreen.screens.first(where: { $0.frame.contains(NSEvent.mouseLocation) }) ?? NSScreen.main {
            let visible = screen.visibleFrame
            let size = panel.frame.size
            let origin = switch placement {
            case .edge(.bottom):
                NSPoint(x: visible.midX - size.width / 2, y: visible.minY + 24)
            case .edge(.top):
                NSPoint(x: visible.midX - size.width / 2, y: visible.maxY - size.height - 24)
            case .topTrailing:
                NSPoint(x: visible.maxX - size.width - 16, y: visible.maxY - size.height - 16)
            }
            panel.setFrameOrigin(origin)
        }
        panel.orderFrontRegardless()
    }

    func hide() {
        panel?.orderOut(nil)
    }

    private func make() -> NSPanel {
        let panel = NSPanel(
            contentRect: .zero,
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: true)
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.hidesOnDeactivate = false
        let host = NSHostingView(rootView: AnyView(EmptyView()))
        host.sizingOptions = .intrinsicContentSize
        panel.contentView = host
        self.host = host
        return panel
    }
}

/// One row of the palette that does something: a place to go or a verb.
struct PaletteAction: Identifiable {
    enum Group {
        case navigation, actions
    }

    let id: String
    let group: Group
    let title: String
    let run: @MainActor () -> Void
}

/// One answer from the query plane: a meeting, a person, a dictation, a loop.
/// `link` is a `sona://` address the core knows how to open.
struct PaletteHit: Decodable, Identifiable {
    let kind: String
    let id: String
    let title: String
    let snippet: String
    let whenUtcMs: Int64
    let link: String
}

private struct PalettePage: Decodable {
    let entries: [PaletteHit]
}

/// What a keystroke in the field can land on, in the order the list shows them.
private enum PaletteRow: Identifiable {
    case action(PaletteAction)
    case hit(PaletteHit)
    case ask(String)

    var id: String {
        switch self {
        case let .action(action): "action:\(action.id)"
        case let .hit(hit): "hit:\(hit.link)"
        case .ask: "ask"
        }
    }
}

/// ⌘K. A floating panel: one field, the places and verbs that match, then the
/// meetings, people, dictations and loops the query plane found, and the ask
/// row when the agent is allowed to hear the question. The corpus rows are
/// never filtered by the letters typed: the plane matched them, sometimes by
/// meaning, and a title need not share one letter with the question.
struct CommandPalette: View {
    @Environment(AppModel.self) private var model
    @State private var query = ""
    @State private var hits: [PaletteHit] = []
    @State private var searchFailed = false
    @State private var selection = 0
    @State private var search: Task<Void, Never>?
    @FocusState private var focused: Bool

    /// A single letter matches half the corpus, so it is not a query yet.
    private static let minimumQuery = 2
    /// One page: the newest dozen that matched. Recency orders the plane.
    private static let limit: Int64 = 12
    private static let kinds: [(kind: String, label: String)] = [
        ("meeting", "Meetings"), ("person", "People"), ("dictation", "Dictations"), ("loop", "Loops"),
    ]

    var body: some View {
        ZStack(alignment: .top) {
            Color.black.opacity(0.18)
                .ignoresSafeArea()
                .onTapGesture { model.paletteShown = false }
            VStack(spacing: 0) {
                HStack(spacing: 10) {
                    Image(systemName: "magnifyingglass")
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(Theme.inkTertiary)
                    TextField("", text: $query, prompt: Text("Search or ask").foregroundStyle(Theme.inkTertiary))
                        .textFieldStyle(.plain)
                        .font(TypeScale.body(16))
                        .foregroundStyle(Theme.ink)
                        .focused($focused)
                        .onSubmit { runSelected() }
                        .onKeyPress(.upArrow) { move(-1); return .handled }
                        .onKeyPress(.downArrow) { move(1); return .handled }
                    KeyCap("esc")
                }
                .padding(.horizontal, 18)
                .frame(height: 52)
                Hairline()
                ScrollViewReader { proxy in
                    ScrollView {
                        VStack(alignment: .leading, spacing: 0) {
                            ForEach(sections, id: \.label) { section in
                                Text(section.label)
                                    .sectionLabel()
                                    .padding(.horizontal, 18)
                                    .padding(.top, 12)
                                    .padding(.bottom, 4)
                                ForEach(section.rows) { row in
                                    let index = rows.firstIndex { $0.id == row.id } ?? 0
                                    PaletteRowView(row: row, selected: index == selection) {
                                        selection = index
                                        run(row)
                                    }
                                    .id(row.id)
                                }
                            }
                            if let empty {
                                Text(empty)
                                    .metaText()
                                    .padding(.horizontal, 18)
                                    .padding(.vertical, 14)
                            }
                        }
                        .padding(.bottom, 8)
                    }
                    .scrollIndicators(.never)
                    .frame(maxHeight: 420)
                    .onChange(of: selection) { _, index in
                        if rows.indices.contains(index) {
                            proxy.scrollTo(rows[index].id)
                        }
                    }
                }
            }
            .frame(width: 600)
            .background(Theme.surface)
            .clipShape(RoundedRectangle(cornerRadius: Theme.radiusPanel))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusPanel).strokeBorder(Theme.border, lineWidth: 1))
            .shadow(color: .black.opacity(0.18), radius: 24, y: 12)
            .padding(.top, 96)
            .onAppear { focused = true }
            .onExitCommand { model.paletteShown = false }
            .onChange(of: query) { _, text in
                selection = 0
                schedule(text)
            }
            .onDisappear { search?.cancel() }
        }
    }

    private var trimmed: String { query.trimmingCharacters(in: .whitespaces) }

    private var actions: [PaletteAction] {
        let all = model.paletteActions
        guard !trimmed.isEmpty else { return all }
        return all.filter { $0.title.localizedCaseInsensitiveContains(trimmed) }
    }

    /// The sections in the order the list shows them: places, verbs, then the
    /// plane's rows by kind, then the one ask row.
    private var sections: [(label: String, rows: [PaletteRow])] {
        var sections: [(label: String, rows: [PaletteRow])] = []
        let navigation = actions.filter { $0.group == .navigation }.map(PaletteRow.action)
        let verbs = actions.filter { $0.group == .actions }.map(PaletteRow.action)
        if !navigation.isEmpty { sections.append(("Navigation", navigation)) }
        if !verbs.isEmpty { sections.append(("Actions", verbs)) }
        for (kind, label) in Self.kinds {
            let rows = hits.filter { $0.kind == kind }.map(PaletteRow.hit)
            if !rows.isEmpty { sections.append((label, rows)) }
        }
        if model.canAsk, !trimmed.isEmpty {
            sections.append(("Ask", [.ask(trimmed)]))
        }
        return sections
    }

    private var rows: [PaletteRow] { sections.flatMap(\.rows) }

    /// The one sentence a settled search is allowed.
    private var empty: String? {
        if searchFailed { return "Search is unavailable right now." }
        if trimmed.count >= Self.minimumQuery, hits.isEmpty, rows.isEmpty {
            return "Nothing matched “\(trimmed)”."
        }
        return nil
    }

    private func move(_ delta: Int) {
        guard !rows.isEmpty else { return }
        selection = (selection + delta + rows.count) % rows.count
    }

    private func runSelected() {
        guard rows.indices.contains(selection) else { return }
        run(rows[selection])
    }

    private func run(_ row: PaletteRow) {
        model.paletteShown = false
        switch row {
        case let .action(action): action.run()
        case let .hit(hit): model.open(link: hit.link)
        case let .ask(question): model.ask(question)
        }
    }

    /// Long enough to swallow a typed word, short enough to feel like the list
    /// is keeping up: 150 ms after the last keystroke, one page from the plane.
    private func schedule(_ text: String) {
        search?.cancel()
        let query = text.trimmingCharacters(in: .whitespaces)
        guard query.count >= Self.minimumQuery else {
            hits = []
            searchFailed = false
            return
        }
        search = Task {
            try? await Task.sleep(for: .milliseconds(150))
            guard !Task.isCancelled else { return }
            do {
                let page: PalettePage = try await model.core.request(
                    "sona_query_search",
                    ["scope": "all", "query": .string(query), "limit": .number(Double(Self.limit)), "cursor": nil] as [String: JSONValue])
                guard !Task.isCancelled else { return }
                hits = page.entries
                searchFailed = false
            } catch {
                guard !Task.isCancelled else { return }
                hits = []
                searchFailed = true
            }
        }
    }
}

private struct PaletteRowView: View {
    let row: PaletteRow
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                switch row {
                case let .action(action):
                    Text(action.title).bodyText()
                case let .hit(hit):
                    VStack(alignment: .leading, spacing: 3) {
                        Text(hit.title).bodyText().lineLimit(1)
                        HStack(spacing: 6) {
                            Text(Date(timeIntervalSince1970: TimeInterval(hit.whenUtcMs) / 1000).relativeDay)
                            if !hit.snippet.isEmpty {
                                Text("·")
                                Text(hit.snippet).lineLimit(1)
                            }
                        }
                        .metaText()
                    }
                case let .ask(question):
                    Text("Ask Sona: \(question)").bodyText()
                }
                Spacer()
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 8)
            .frame(minHeight: 40)
            .background(selected ? Theme.selection : .clear, in: RoundedRectangle(cornerRadius: 8))
            .padding(.horizontal, 6)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}
