import SwiftUI
import QuartzCore
import Observation

struct HUDPill: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(spacing: 8) {
            if model.settings.settings.overlayPosition == .top {
                capsule
            }
            if showsLiveWords {
                liveWords
                    .transition(.opacity.animation(.easeInOut(duration: reduceMotion ? 0.1 : 0.2)))
            }
            if model.settings.settings.overlayPosition != .top {
                capsule
            }
        }
        .padding(8)
    }

    private var presentation: PillPresentation { model.pillPresentation }

    private var phase: String {
        if let feedback = presentation.feedback { return feedback.label }
        switch presentation.capture {
        case .idle: return "idle"
        case .recording: return "recording"
        case let .working(kind): return kind
        }
    }

    private var showsLiveWords: Bool {
        guard model.settings.settings.overlayStyle == .live else { return false }
        if case .recording = presentation.capture { return true }
        return false
    }

    private var capsule: some View {
        Group {
            if presentation.capture == .idle, presentation.feedback == nil {
                idle
            } else {
                ZStack {
                    activeContent
                        .id(phase)
                        .transition(.opacity)
                }
                .frame(width: 176, height: 36)
                .animation(reduceMotion ? nil : .easeInOut(duration: 0.2), value: phase)
                .background(Theme.hud, in: Capsule())
                .overlay(Capsule().strokeBorder(.white.opacity(0.12), lineWidth: 1))
            }
        }
        .foregroundStyle(Theme.hudInk)
        .environment(\.colorScheme, .dark)
    }

    @ViewBuilder private var activeContent: some View {
        if let feedback = presentation.feedback {
            HStack(spacing: 8) {
                Image(systemName: feedback.symbol)
                    .font(.system(size: 13, weight: .semibold))
                Text(feedback.label)
                    .font(.system(size: 12, weight: .medium))
            }
            .accessibilityElement(children: .combine)
        } else {
            HStack(spacing: 6) {
                control("Cancel", symbol: "xmark") { model.cancelCapture() }
                switch presentation.capture {
                case .recording:
                    RecordingWaveform(meter: model.meter)
                        .frame(width: 55, height: 18)
                    status
                        .font(.system(size: 11, weight: .medium))
                        .frame(minWidth: 32)
                    control("Stop recording", symbol: "stop.fill") { model.stopCapture() }
                case let .working(kind):
                    HStack(spacing: 8) {
                        if reduceMotion {
                            Image(systemName: "ellipsis")
                        } else {
                            ProgressView()
                                .controlSize(.mini)
                                .tint(Theme.hudInk)
                        }
                        Text(Self.phaseLabel(kind))
                            .font(.system(size: 12, weight: .medium))
                            .lineLimit(1)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.trailing, 12)
                case .idle:
                    EmptyView()
                }
            }
            .padding(.horizontal, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var idle: some View {
        Group {
            if model.settings.settings.hudPillPosition.isVertical {
                VStack(spacing: 6) {
                    micMark
                    UprightText(model.pillMode ?? "Dictate")
                        .font(.system(size: 11, weight: .medium))
                        .frame(maxHeight: 76)
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 12)
            } else {
                HStack(spacing: 6) {
                    micMark
                    Text(model.pillMode ?? "Dictate")
                        .font(.system(size: 11, weight: .medium))
                        .lineLimit(1)
                }
                .padding(.horizontal, 12)
                .frame(height: 28)
                .frame(maxWidth: 112)
            }
        }
        .background(Theme.hud, in: Capsule())
        .overlay(Capsule().strokeBorder(.white.opacity(0.12), lineWidth: 1))
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
            Button("Hide between dictations") {
                Task { await model.settings.setHudPillEnabled(false) }
            }
        }
    }

    private var micMark: some View {
        Image(systemName: "mic.fill")
            .font(.system(size: 10, weight: .medium))
    }

    private var liveWords: some View {
        Text(model.liveText.isEmpty ? "Listening…" : model.liveText)
            .font(.system(size: 13))
            .foregroundStyle(model.liveText.isEmpty ? Theme.hudInkSecondary : Theme.hudInk)
            .lineLimit(2)
            .truncationMode(.head)
            .frame(width: 212, height: 36, alignment: .leading)
            .padding(12)
            .background(Theme.hud, in: RoundedRectangle(cornerRadius: 20))
            .overlay(RoundedRectangle(cornerRadius: 20).strokeBorder(.white.opacity(0.12), lineWidth: 1))
    }

    @ViewBuilder private var status: some View {
        if let seconds = model.pillWarningSeconds {
            Text("\(seconds)s")
                .foregroundStyle(Theme.hudLive)
                .accessibilityLabel("\(seconds) seconds left")
        } else if case let .recording(since) = presentation.capture {
            TimelineView(.periodic(from: since, by: 1)) { context in
                Text(context.date.timeIntervalSince(since).clock)
                    .monospacedDigit()
            }
            .foregroundStyle(Theme.hudInkSecondary)
        }
    }

    private func control(_ title: String, symbol: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(Theme.hudInkSecondary)
                .frame(width: 26, height: 26)
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .help(title)
        .accessibilityLabel(title)
    }

    private static func phaseLabel(_ kind: String) -> String {
        switch kind {
        case "transcribing": "Transcribing"
        case "polishing": "Polishing"
        default: "Working"
        }
    }
}

private struct RecordingWaveform: View {
    let meter: LevelMeter
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        HStack(spacing: 2) {
            ForEach(0..<LevelMeter.width, id: \.self) { index in
                Capsule()
                    .fill(Theme.hudInk)
                    .frame(width: 3, height: 18)
                    .scaleEffect(y: max(0.15, meter.history[index]), anchor: .center)
            }
        }
        .animation(reduceMotion ? nil : .linear(duration: 0.08), value: meter.history)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Listening")
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
@Observable
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
    private var shown = false
    private var visibilityRevision = 0
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

    private var contentAlignment: Alignment {
        guard let placement else { return .center }
        switch placement {
        case let .edge(edge): return edge.alignment
        case .topTrailing: return .topTrailing
        }
    }

    private struct Surface<Content: View>: View {
        let content: Content
        weak var panel: FloatingPanel?

        var body: some View {
            content
                .fixedSize()
                .onGeometryChange(for: CGSize.self) { $0.size } action: { [weak panel = panel] size in
                    panel?.fit(size)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: panel?.contentAlignment ?? .center)
        }
    }

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
        host.rootView = AnyView(Surface(content: content, panel: self))
        panel.intercept = { [weak self] event in self?.intercept(event) ?? false }
    }

    /// Floats the panel at `placement` on the screen the pointer is on. A
    /// panel already up at the same placement stays exactly where it is,
    /// unless `anew` asks for it to be floated again all the same.
    func show(at placement: PanelPlacement, anew: Bool = false) {
        let appearing = !panel.isVisible
        guard anew || !shown || placement != self.placement else { return }
        if drag != nil { cancelDrag() }
        shown = true
        visibilityRevision += 1
        panel.ignoresMouseEvents = false
        self.placement = placement
        if let screen = NSScreen.screens.first(where: { $0.frame.contains(NSEvent.mouseLocation) }) ?? NSScreen.main {
            anchor = Self.anchor(for: placement, on: screen.visibleFrame)
        }
        let target = frame(host.fittingSize, at: placement)
        if appearing {
            panel.setFrame(target, display: false)
            panel.alphaValue = 0
            panel.orderFrontRegardless()
        } else {
            move(to: target)
        }
        NSAnimationContext.runAnimationGroup { context in
            context.duration = Self.fadeDuration
            panel.animator().alphaValue = 1
        }
    }

    func hide() {
        guard shown else { return }
        if drag != nil { cancelDrag() }
        shown = false
        visibilityRevision += 1
        let revision = visibilityRevision
        panel.ignoresMouseEvents = true
        NSAnimationContext.runAnimationGroup { context in
            context.duration = Self.fadeDuration
            panel.animator().alphaValue = 0
        } completionHandler: { [weak self] in
            MainActor.assumeIsolated {
                guard let self, !self.shown, self.visibilityRevision == revision else { return }
                self.panel.orderOut(nil)
            }
        }
    }

    private func fit(_ size: CGSize) {
        guard let placement, shown else { return }
        if case .dragging = drag { return }
        move(to: frame(size, at: placement))
    }

    private static var fadeDuration: TimeInterval {
        NSWorkspace.shared.accessibilityDisplayShouldReduceMotion ? 0.1 : 0.18
    }

    private func move(to target: NSRect) {
        guard target != panel.frame else { return }
        NSAnimationContext.runAnimationGroup { context in
            context.duration = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion ? 0 : 0.22
            context.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
            panel.animator().setFrame(target, display: true)
        }
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
        move(to: frame)
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
        (.meeting, "Meetings"), (.person, "People"), (.note, "Notes"),
        (.dictation, "Dictations"), (.loop, "Loops"),
    ]

    var body: some View {
        let panelShape = RoundedRectangle(cornerRadius: Theme.radiusDialog)
        ZStack(alignment: .top) {
            Color.black.opacity(0.18)
                .ignoresSafeArea()
                .onTapGesture { model.paletteShown = false }
            VStack(spacing: 0) {
                HStack(spacing: 10) {
                    Image(systemName: "magnifyingglass")
                        .font(.system(size: 15, weight: .regular))
                        .foregroundStyle(Theme.inkTertiary)
                    TextField("", text: $query, prompt: Text("Search or ask").foregroundStyle(Theme.inkTertiary))
                        .textFieldStyle(.plain)
                        .font(TypeScale.body(17))
                        .foregroundStyle(Theme.ink)
                        .focused($focused)
                        .onSubmit { runSelected() }
                        .onKeyPress(.upArrow) { move(-1); return .handled }
                        .onKeyPress(.downArrow) { move(1); return .handled }
                    if case .pending = lookup {
                        ProgressView().controlSize(.small)
                    }
                }
                .padding(.horizontal, 20)
                .frame(height: 56)
                Hairline()
                ScrollViewReader { proxy in
                    ScrollView {
                        VStack(alignment: .leading, spacing: 2) {
                            ForEach(sections, id: \.label) { section in
                                Text(section.label)
                                    .font(.system(size: 12, weight: .semibold))
                                    .foregroundStyle(Theme.inkTertiary)
                                    .accessibilityAddTraits(.isHeader)
                                    .padding(.horizontal, 20)
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
                Hairline()
                keys
            }
            .frame(width: 600)
            .background {
                if model.about.material == .glass {
                    panelShape.fill(Theme.surface)
                }
            }
            .clipShape(panelShape)
            .padding(model.about.material == .glass ? 6 : 0)
            .background {
                if model.about.material == .solid {
                    panelShape
                        .fill(Theme.surface)
                        .overlay(panelShape.strokeBorder(Theme.border, lineWidth: 1))
                        .shadow(color: .black.opacity(0.22), radius: 28, y: 14)
                } else if #available(macOS 26.0, *) {
                    Color.clear
                        .glassEffect(.regular, in: panelShape)
                } else {
                    panelShape
                        .fill(.regularMaterial)
                        .overlay(panelShape.strokeBorder(Theme.border, lineWidth: 1))
                        .shadow(color: .black.opacity(0.22), radius: 28, y: 14)
                }
            }
            .padding(.top, 96)
            .onAppear {
                query = model.takePaletteSeed()
                DispatchQueue.main.async { focused = true }
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
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    /// The keys the palette answers to, as the footer of the panel.
    private var keys: some View {
        HStack(spacing: 6) {
            Spacer(minLength: 0)
            KeyCap("↑")
            KeyCap("↓")
            Text("Select").metaText(Theme.inkSecondary).padding(.trailing, 10)
            KeyCap("↩")
            Text("Open").metaText(Theme.inkSecondary).padding(.trailing, 10)
            KeyCap("esc")
            Text("Close").metaText(Theme.inkSecondary)
        }
        .padding(.horizontal, 16)
        .frame(height: 40)
        .background(Theme.inset)
        .accessibilityElement(children: .combine)
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
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: icon)
                    .font(.system(size: 14, weight: .regular))
                    .foregroundStyle(Theme.inkSecondary)
                    .frame(width: 20)
                    .accessibilityHidden(true)
                switch row {
                case let .action(action):
                    Text(action.title).bodyText()
                case let .hit(hit):
                    VStack(alignment: .leading, spacing: 2) {
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
                    Text("Show more results").bodyText(14, Theme.inkSecondary)
                case let .ask(question):
                    Text("Ask Sona: \(question)").bodyText()
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(minHeight: 38)
            .background(fill, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .padding(.horizontal, 8)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
    }

    private var fill: Color {
        if selected { return Theme.selection }
        return hovering ? Theme.wash : .clear
    }

    private var icon: String {
        switch row {
        case let .action(action):
            switch action.id {
            case "go-settings": "gearshape"
            case "toggle-recording": "mic"
            case "record-meeting": "video"
            case "record-screen": "rectangle.dashed.badge.record"
            case "open-chat": "bubble.left"
            case "import-audio": "square.and.arrow.down"
            default: Place.allCases.first { "go-\($0.rawValue)" == action.id }?.icon ?? "arrow.right"
            }
        case let .hit(hit):
            switch hit.kind {
            case .meeting: "video"
            case .dictation: "waveform"
            case .note: "square.and.pencil"
            case .person: "person"
            case .series: "calendar"
            case .loop: "arrow.triangle.2.circlepath"
            case .receipt: "doc.text"
            }
        case .more: "ellipsis"
        case .ask: "bubble.left"
        }
    }
}
