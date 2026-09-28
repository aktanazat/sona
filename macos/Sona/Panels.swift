import SwiftUI

/// The pill that floats above other windows. Idle, the name of the mode the
/// next recording goes under: a press starts it, a right-click picks the
/// mode or puts the pill away for an hour, and a drag carries it to another
/// edge of the screen. On a side edge it stands upright. While you talk, the
/// sound: eleven bars, the last quarter second of the microphone, newest on
/// the right, flat while nothing is being heard, and the minute-left warning
/// when the dictation is about to reach its limit. With the live overlay
/// style, the words as the model hears them sit beside the bars. While the
/// words are worked on, the phase in one word.
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
                    warning
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

    /// The idle pill is not a `Button`: the panel it lives in tells a click
    /// from a drag, and hands only a whole click through.
    private var idle: some View {
        Group {
            if model.settings.settings.hudPillPosition.isVertical {
                VStack(spacing: 6) {
                    micMark
                    UprightText(model.pillMode ?? "Dictate")
                        .font(.system(size: 11, weight: .medium))
                }
                .padding(.horizontal, 6)
                .padding(.vertical, 12)
            } else {
                HStack(spacing: 6) {
                    micMark
                    Text(model.pillMode ?? "Dictate")
                        .font(.system(size: 11, weight: .medium))
                        .lineLimit(1)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
            }
        }
        .foregroundStyle(Theme.onInvert)
        .background(Theme.invert, in: Capsule())
        .contentShape(Capsule())
        .accessibilityElement(children: .ignore)
        .accessibilityAddTraits(.isButton)
        .accessibilityLabel("Start recording")
        .accessibilityHint("Right-click to choose the mode. Drag to another edge of the screen.")
        .accessibilityAction { model.toggleCapture() }
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
            Divider()
            Button("Hide for an hour") { model.hidePillForAnHour() }
        }
    }

    private var micMark: some View {
        Image(systemName: "mic")
            .font(.system(size: 10, weight: .semibold))
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

    /// The last minute of a dictation, said once the core has counted it.
    @ViewBuilder private var warning: some View {
        if let seconds = model.pillWarningSeconds {
            Text(seconds == 60 ? "1 minute left" : "\(seconds) seconds left")
                .font(.system(size: 11, weight: .medium))
                .lineLimit(1)
        }
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

/// A line of text read from bottom to top, as the pill's label runs on a
/// side edge. Turning a view does not turn the room it takes, so the text
/// is measured flat and given a frame of its turned size.
private struct UprightText: View {
    private let text: String
    @State private var flat = CGSize.zero

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(text)
            .lineLimit(1)
            .fixedSize()
            .onGeometryChange(for: CGSize.self) { $0.size } action: { flat = $0 }
            .rotationEffect(.degrees(-90))
            .frame(width: flat.height, height: flat.width)
    }
}

/// Where a floating panel sits on the screen the pointer is on.
enum PanelPlacement: Equatable {
    /// Centred along one edge of the screen.
    case edge(HudPillEdge)
    /// Tucked into the top right corner.
    case topTrailing
}

extension HudPillEdge {
    /// The recording overlay's edge, as a pill edge.
    init(_ position: OverlayPosition) {
        self = switch position {
        case .top: .top
        case .bottom: .bottom
        }
    }

    /// The edge of `frame` a point is nearest to.
    static func nearest(to point: NSPoint, in frame: NSRect) -> HudPillEdge {
        let distances: [(HudPillEdge, CGFloat)] = [
            (.left, point.x - frame.minX),
            (.right, frame.maxX - point.x),
            (.bottom, point.y - frame.minY),
            (.top, frame.maxY - point.y),
        ]
        return distances.min { $0.1 < $1.1 }?.0 ?? .bottom
    }
}

/// The panel underneath: a window that tells a click from a drag before
/// its view hears either, and takes the keyboard only while a drag is on,
/// so Escape can end one without the app ever coming to the front.
private final class DockablePanel: NSPanel {
    /// Sees every event first. `true` keeps it from the views.
    var intercept: (@MainActor (NSEvent) -> Bool)?
    var takesKeys = false

    override var canBecomeKey: Bool { takesKeys }

    override func sendEvent(_ event: NSEvent) {
        if intercept?(event) == true { return }
        super.sendEvent(event)
    }
}

/// The four strips that light up along the screen's edges while the pill
/// is being dragged, the one the pill would land on brightest.
private struct EdgeTargets: View {
    let nearest: HudPillEdge?

    var body: some View {
        ZStack {
            ForEach(HudPillEdge.allCases, id: \.self) { edge in
                Capsule()
                    .fill(edge == nearest ? Theme.accent : Theme.invert)
                    .opacity(edge == nearest ? 0.9 : 0.25)
                    .frame(
                        width: edge.isVertical ? 6 : nil,
                        height: edge.isVertical ? nil : 6)
                    .padding(edge.isVertical ? .vertical : .horizontal, 96)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: edge.alignment)
                    .padding(12)
            }
        }
        .ignoresSafeArea()
        .allowsHitTesting(false)
    }
}

private extension HudPillEdge {
    var alignment: Alignment {
        switch self {
        case .top: .top
        case .bottom: .bottom
        case .left: .leading
        case .right: .trailing
        }
    }
}

/// One drag of the pill, from the press to the drop.
private enum PillDrag {
    /// The mouse is down and has not moved far. Nothing has happened yet:
    /// the release will make it a click.
    case pressed(at: NSPoint)
    /// The pill follows the pointer. `frame` and `at` are where the panel and
    /// the pointer were when the drag began; `nearest` is the edge it would
    /// drop on now.
    case dragging(frame: NSRect, at: NSPoint, nearest: HudPillEdge?)
}

/// A window that floats above other windows and on every space, without
/// taking the keyboard away from the app the words are going into, which a
/// SwiftUI `Window` scene would. It hosts one view for its whole life, a
/// view that reads its own state, so a change redraws in place: nothing
/// here swaps the view, and the panel moves only when it is shown after
/// being hidden, when its placement changes, when the view's size does, or
/// when it is dragged to another edge.
@MainActor
final class FloatingPanel {
    private let panel: DockablePanel
    private let host: NSHostingView<AnyView>
    private var placement: PanelPlacement?
    /// The point the placement pins, fixed when the panel is placed: its
    /// top right corner, or the middle of the edge it stands on. A resize
    /// grows the panel away from it, so the panel stays on its screen.
    private var anchor = NSPoint.zero
    private var drag: PillDrag?
    /// The edge strips, made the first time a drag begins.
    private var targets: NSPanel?
    private var targetsHost: NSHostingView<EdgeTargets>?
    /// While on, a left press on the panel is the panel's: a release without
    /// travel is a click, and travel is a drag to another edge. Off, presses
    /// reach the views as they always did.
    var dockable = false
    /// A click, while `dockable`.
    var onClick: (@MainActor () -> Void)?
    /// A drop on an edge, while `dockable`.
    var onDock: (@MainActor (HudPillEdge) -> Void)?
    /// A press must travel this far to be a drag rather than a click.
    private static let dragSlop: CGFloat = 4
    private static let escape: UInt16 = 53

    init(_ content: some View) {
        let panel = DockablePanel(
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
        self.panel = panel
        self.host = host
        host.rootView = AnyView(
            content
                .fixedSize()
                .onGeometryChange(for: CGSize.self) { $0.size } action: { [weak self] size in
                    self?.fit(size)
                })
        panel.intercept = { [weak self] event in self?.intercept(event) ?? false }
    }

    /// Floats the panel at `placement` on the screen the pointer is on. A
    /// panel already up at the same placement stays exactly where it is,
    /// unless `anew` asks for it to be floated again all the same.
    func show(at placement: PanelPlacement, anew: Bool = false) {
        guard anew || !panel.isVisible || placement != self.placement else { return }
        if drag != nil { cancelDrag() }
        self.placement = placement
        if let screen = NSScreen.screens.first(where: { $0.frame.contains(NSEvent.mouseLocation) }) ?? NSScreen.main {
            anchor = Self.anchor(for: placement, on: screen.visibleFrame)
        }
        panel.setFrame(frame(host.fittingSize, at: placement), display: false)
        panel.orderFrontRegardless()
    }

    func hide() {
        if drag != nil { cancelDrag() }
        panel.orderOut(nil)
    }

    /// The view's size changed, which is the only time this is called. The
    /// panel takes the new size around the same anchor, in one move, even
    /// when AppKit already grew it from the hosting view's intrinsic size.
    private func fit(_ size: CGSize) {
        guard let placement, panel.isVisible else { return }
        if case .dragging = drag { return }
        let target = frame(size, at: placement)
        if target != panel.frame { panel.setFrame(target, display: true) }
    }

    private static func anchor(for placement: PanelPlacement, on visible: NSRect) -> NSPoint {
        switch placement {
        case .edge(.bottom): NSPoint(x: visible.midX, y: visible.minY + 24)
        case .edge(.top): NSPoint(x: visible.midX, y: visible.maxY - 24)
        case .edge(.left): NSPoint(x: visible.minX + 24, y: visible.midY)
        case .edge(.right): NSPoint(x: visible.maxX - 24, y: visible.midY)
        case .topTrailing: NSPoint(x: visible.maxX - 16, y: visible.maxY - 16)
        }
    }

    private func frame(_ size: CGSize, at placement: PanelPlacement) -> NSRect {
        let origin = switch placement {
        case .edge(.bottom): NSPoint(x: anchor.x - size.width / 2, y: anchor.y)
        case .edge(.top): NSPoint(x: anchor.x - size.width / 2, y: anchor.y - size.height)
        case .edge(.left): NSPoint(x: anchor.x, y: anchor.y - size.height / 2)
        case .edge(.right): NSPoint(x: anchor.x - size.width, y: anchor.y - size.height / 2)
        case .topTrailing: NSPoint(x: anchor.x - size.width, y: anchor.y - size.height)
        }
        return NSRect(origin: origin, size: size)
    }

    // MARK: Dragging to an edge

    /// Every event the panel gets, before its views. While the panel is
    /// dockable a left press is the panel's: a release without travel is a
    /// click, travel moves the panel, and Escape ends the move where it began.
    private func intercept(_ event: NSEvent) -> Bool {
        switch event.type {
        case .leftMouseDown:
            guard dockable, drag == nil else { return false }
            drag = .pressed(at: NSEvent.mouseLocation)
            return true
        case .leftMouseDragged:
            switch drag {
            case nil:
                return false
            case let .pressed(start):
                let point = NSEvent.mouseLocation
                if hypot(point.x - start.x, point.y - start.y) >= Self.dragSlop {
                    beginDrag(at: start)
                    follow(point)
                }
                return true
            case .dragging:
                follow(NSEvent.mouseLocation)
                return true
            }
        case .leftMouseUp:
            switch drag {
            case nil:
                return false
            case .pressed:
                drag = nil
                onClick?()
                return true
            case let .dragging(_, _, nearest):
                drop(on: nearest ?? HudPillEdge.nearest(to: NSEvent.mouseLocation, in: bounds))
                return true
            }
        case .keyDown:
            guard case .dragging = drag else { return false }
            if event.keyCode == Self.escape { cancelDrag() }
            return true
        default:
            return false
        }
    }

    /// The screen the panel is on: the one holding its middle, else the
    /// pointer's, else the main one.
    private var screen: NSScreen? {
        let middle = NSPoint(x: panel.frame.midX, y: panel.frame.midY)
        return NSScreen.screens.first { $0.frame.contains(middle) }
            ?? NSScreen.screens.first { $0.frame.contains(NSEvent.mouseLocation) }
            ?? NSScreen.main
    }

    /// The room the pill can dock in: its screen's, or its own when no
    /// screen claims it.
    private var bounds: NSRect { screen?.visibleFrame ?? panel.frame }

    private func beginDrag(at start: NSPoint) {
        drag = .dragging(frame: panel.frame, at: start, nearest: nil)
        panel.takesKeys = true
        panel.makeKey()
        showTargets(in: bounds)
    }

    private func follow(_ point: NSPoint) {
        guard case let .dragging(frame, start, was) = drag else { return }
        panel.setFrameOrigin(NSPoint(x: frame.origin.x + point.x - start.x, y: frame.origin.y + point.y - start.y))
        let room = bounds
        let nearest = HudPillEdge.nearest(to: point, in: room)
        if nearest != was {
            drag = .dragging(frame: frame, at: start, nearest: nearest)
            targetsHost?.rootView = EdgeTargets(nearest: nearest)
        }
        // The pill crossed to another screen: the strips follow it there.
        if let targets, targets.frame != room {
            targets.setFrame(room, display: true)
        }
    }

    /// The drop: the panel snaps to the edge and the owner is told, so the
    /// edge outlives this launch.
    private func drop(on edge: HudPillEdge) {
        endDrag()
        show(at: .edge(edge), anew: true)
        onDock?(edge)
    }

    /// Escape, or a hide mid-drag: back where the drag began.
    private func cancelDrag() {
        let frame = if case let .dragging(frame, _, _) = drag { frame } else { panel.frame }
        endDrag()
        panel.setFrame(frame, display: true)
    }

    /// The drag is over, whichever way. Ordering the panel out is what gives
    /// the keyboard back to the app in front; it is ordered in again by
    /// whatever placed it next.
    private func endDrag() {
        drag = nil
        targets?.orderOut(nil)
        panel.takesKeys = false
        panel.orderOut(nil)
        panel.orderFrontRegardless()
    }

    private func showTargets(in room: NSRect) {
        let targets = self.targets ?? makeTargets()
        targetsHost?.rootView = EdgeTargets(nearest: nil)
        targets.setFrame(room, display: false)
        targets.order(.below, relativeTo: panel.windowNumber)
    }

    private func makeTargets() -> NSPanel {
        let targets = NSPanel(
            contentRect: .zero,
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: true)
        targets.isFloatingPanel = true
        targets.level = .floating
        targets.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        targets.isOpaque = false
        targets.backgroundColor = .clear
        targets.hasShadow = false
        targets.hidesOnDeactivate = false
        targets.ignoresMouseEvents = true
        let host = NSHostingView(rootView: EdgeTargets(nearest: nil))
        targets.contentView = host
        self.targets = targets
        targetsHost = host
        return targets
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
