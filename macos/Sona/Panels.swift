import SwiftUI

/// The pill that floats above other windows. Idle, the name of the mode the
/// next recording goes under: a press starts it, a right-click picks the
/// mode. While you talk, the sound: eleven bars, the last quarter second of
/// the microphone, newest on the right, flat while nothing is being heard.
/// With the live overlay style, the words as the model hears them sit beside
/// the bars. While the words are worked on, the phase in one word.
struct HUDPill: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Group {
            switch model.capture {
            case .idle:
                idle
            case .recording:
                pill {
                    bars
                    liveWords
                }
            case let .working(kind):
                pill {
                    ProgressView()
                        .controlSize(.mini)
                        .tint(Theme.onInvert)
                    Text(Self.phaseLabel(kind))
                        .font(.system(size: 11, weight: .medium))
                    liveWords
                }
            }
        }
        .padding(8)
    }

    private var idle: some View {
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
        .accessibilityLabel("Start recording")
        .accessibilityHint("Right-click to choose the mode.")
        .contextMenu {
            ForEach(model.pillModes) { mode in
                Button {
                    model.choosePillMode(mode.id)
                } label: {
                    if mode.active {
                        Label(mode.name, systemImage: "checkmark")
                    } else {
                        Text(mode.name)
                    }
                }
                .disabled(mode.active)
            }
        }
    }

    private var bars: some View {
        HStack(spacing: 3) {
            ForEach(Array(model.meter.history.enumerated()), id: \.offset) { _, level in
                RoundedRectangle(cornerRadius: 1)
                    .fill(Theme.onInvert)
                    .frame(width: 2, height: 2 + 12 * level)
            }
        }
        .frame(height: 14)
        .accessibilityLabel("Recording")
    }

    /// The words so far, under the live style. The frame is fixed so the
    /// pill does not grow with every word; before any arrive it says so.
    @ViewBuilder private var liveWords: some View {
        if model.settings.settings.overlayStyle == .live {
            Text(model.liveText.isEmpty ? "Listening…" : model.liveText)
                .font(.system(size: 12))
                .opacity(model.liveText.isEmpty ? 0.6 : 1)
                .lineLimit(2)
                .truncationMode(.head)
                .frame(width: 360, alignment: .leading)
        }
    }

    private func pill(@ViewBuilder content: () -> some View) -> some View {
        HStack(spacing: 10, content: content)
            .foregroundStyle(Theme.onInvert)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(Theme.invert, in: Capsule())
    }

    /// The core's work kinds, said for the pill.
    private static func phaseLabel(_ kind: String) -> String {
        switch kind {
        case "transcribing": "Transcribing…"
        case "polishing": "Polishing…"
        default: kind.capitalized + "…"
        }
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

/// What a keystroke in the field can land on, in the order the list shows them.
private enum PaletteRow: Identifiable {
    case action(PaletteAction)
    case hit(QueryRow)
    /// The next page of the same question, when the plane said there is one.
    case more
    case ask(String)

    var id: String {
        switch self {
        case let .action(action): "action:\(action.id)"
        case let .hit(hit): "hit:\(hit.link)"
        case .more: "more"
        case .ask: "ask"
        }
    }
}

/// What the plane has said about the text in the field. The rows shown are
/// always the answer to the question that is there now: an earlier answer
/// stays visible while the next is on its way, dimmed and unreachable, so
/// return can never open a row from a question no longer in the field.
private enum Lookup {
    /// Under two letters: the plane is not asked.
    case none
    /// Asked about the current text, not answered. `earlier` is the last
    /// answer, kept on screen so the list does not blink between keystrokes.
    case pending(earlier: [QueryRow])
    case answered(Answer)
    case failed(String)

    struct Answer {
        var rows: [QueryRow]
        var next: QueryCursor?
        var reason: QueryPageReason?
        /// The next page is on its way; the rows here stay live meanwhile.
        var loadingMore = false
        /// The next page did not arrive.
        var moreFailed = false
    }

    var earlier: [QueryRow] {
        switch self {
        case .none, .failed: []
        case let .pending(earlier): earlier
        case let .answered(answer): answer.rows
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
    @State private var lookup = Lookup.none
    @State private var selection = 0
    @State private var search: Task<Void, Never>?
    @FocusState private var focused: Bool

    /// A single letter matches half the corpus, so it is not a query yet.
    private static let minimumQuery = 2
    /// One page: the newest dozen that matched. Recency orders the plane.
    private static let limit: Int64 = 12
    private static let kinds: [(kind: QueryRowKind, label: String)] = [
        (.meeting, "Meetings"), (.person, "People"), (.dictation, "Dictations"), (.loop, "Loops"),
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
                    if case .pending = lookup {
                        ProgressView().controlSize(.small)
                    }
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
                                    let index = section.live ? rows.firstIndex { $0.id == row.id } ?? 0 : -1
                                    PaletteRowView(row: row, selected: index == selection) {
                                        selection = index
                                        run(row)
                                    }
                                    .id(row.id)
                                    .opacity(section.live ? 1 : 0.4)
                                    .allowsHitTesting(section.live)
                                }
                            }
                            footnote
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
            .onAppear {
                focused = true
                query = model.takePaletteSeed()
            }
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
    /// plane's rows by kind, then the one ask row. A section is live when its
    /// rows answer the question in the field now.
    private var sections: [(label: String, rows: [PaletteRow], live: Bool)] {
        var sections: [(label: String, rows: [PaletteRow], live: Bool)] = []
        let navigation = actions.filter { $0.group == .navigation }.map(PaletteRow.action)
        let verbs = actions.filter { $0.group == .actions }.map(PaletteRow.action)
        if !navigation.isEmpty { sections.append(("Navigation", navigation, true)) }
        if !verbs.isEmpty { sections.append(("Actions", verbs, true)) }
        let hits = lookup.earlier
        let live = if case .pending = lookup { false } else { true }
        for (kind, label) in Self.kinds {
            let rows = hits.filter { $0.kind == kind }.map(PaletteRow.hit)
            if !rows.isEmpty { sections.append((label, rows, live)) }
        }
        if case let .answered(answer) = lookup, answer.next != nil, !answer.loadingMore, !answer.moreFailed {
            sections.append(("More", [.more], true))
        }
        if model.canAsk, !trimmed.isEmpty {
            sections.append(("Ask", [.ask(trimmed)], true))
        }
        return sections
    }

    /// The rows a keystroke can land on.
    private var rows: [PaletteRow] { sections.filter(\.live).flatMap(\.rows) }

    /// Under the rows: what the plane could not do, or the one sentence a
    /// settled search with nothing to show is allowed. Never a verdict while
    /// the plane is still being asked.
    @ViewBuilder
    private var footnote: some View {
        switch lookup {
        case .none, .pending:
            EmptyView()
        case let .failed(reason):
            note(reason, retry: "Try again") { schedule(query, now: true) }
        case let .answered(answer):
            if answer.moreFailed {
                note("The next page did not arrive.", retry: "Try again") { loadMore() }
            } else if answer.loadingMore {
                note("Loading more…")
            }
            if answer.reason == .semanticUnavailable {
                note(
                    "Only exact words matched: the meaning model is not on this Mac yet.",
                    retry: "Search again"
                ) { schedule(query, now: true) }
            } else if answer.rows.isEmpty, rows.isEmpty {
                note("Nothing matched “\(trimmed)”.")
            }
        }
    }

    private func note(_ text: String, retry: String? = nil, action: @escaping () -> Void = {}) -> some View {
        HStack(spacing: 12) {
            Text(text).metaText()
            if let retry {
                Spacer(minLength: 0)
                Button(retry, action: action).buttonStyle(.compact)
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 14)
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
        if case .more = row {
            loadMore()
            return
        }
        model.paletteShown = false
        switch row {
        case let .action(action): action.run()
        case let .hit(hit): model.open(link: hit.link)
        case let .ask(question): model.ask(question)
        case .more: break
        }
    }

    /// Long enough to swallow a typed word, short enough to feel like the list
    /// is keeping up: 150 ms after the last keystroke, one page from the plane.
    /// `now` skips the pause for a retry the reader asked for by hand.
    private func schedule(_ text: String, now: Bool = false) {
        search?.cancel()
        let query = text.trimmingCharacters(in: .whitespaces)
        guard query.count >= Self.minimumQuery else {
            lookup = .none
            return
        }
        lookup = .pending(earlier: lookup.earlier)
        search = Task {
            if !now {
                try? await Task.sleep(for: .milliseconds(150))
            }
            guard !Task.isCancelled else { return }
            do {
                let page = try await ask(query, after: nil)
                guard !Task.isCancelled else { return }
                lookup = .answered(.init(rows: page.entries, next: page.nextCursor, reason: page.reason))
            } catch {
                guard !Task.isCancelled else { return }
                lookup = .failed(Self.sentence(for: error))
            }
        }
    }

    /// The next page of the same question, appended under the rows already
    /// here. A cursor the corpus no longer knows starts the question over.
    private func loadMore() {
        guard case let .answered(answer) = lookup, let next = answer.next, !answer.loadingMore else { return }
        var waiting = answer
        waiting.loadingMore = true
        waiting.moreFailed = false
        lookup = .answered(waiting)
        let query = trimmed
        search = Task {
            do {
                let page = try await ask(query, after: next)
                guard !Task.isCancelled else { return }
                var grown = answer
                grown.rows += page.entries
                grown.next = page.nextCursor
                grown.reason = page.reason ?? answer.reason
                lookup = .answered(grown)
            } catch {
                guard !Task.isCancelled else { return }
                if let failure = error as? CoreError, failure.remote(as: QueryFailure.self) == .unknownCursor {
                    schedule(query, now: true)
                    return
                }
                var failed = answer
                failed.moreFailed = true
                lookup = .answered(failed)
            }
        }
    }

    private func ask(_ query: String, after cursor: QueryCursor?) async throws -> QuerySearchPage {
        try await model.core.request(
            "sona_query_search",
            [
                "scope": "all",
                "query": .string(query),
                "limit": .number(Double(Self.limit)),
                "cursor": try cursor.map { try JSONValue($0) } ?? .null,
            ] as [String: JSONValue])
    }

    private static func sentence(for error: Error) -> String {
        if let failure = error as? CoreError, let code = failure.remote(as: QueryFailure.self) {
            return code.sentence
        }
        return "Search is unavailable right now."
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
                            Text(hit.when.relativeDay)
                            if !hit.snippet.isEmpty {
                                Text("·")
                                Text(hit.snippet).lineLimit(1)
                            }
                        }
                        .metaText()
                    }
                case .more:
                    Text("Show more results").bodyText(15, Theme.inkSecondary)
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
