import SwiftUI
import QuartzCore
import Observation

struct DictationCancelPrompt: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if let promptId = model.cancelConfirmation {
            VStack(alignment: .leading, spacing: 8) {
                Text("Cancel this dictation?")
                    .font(.system(size: 13, weight: .medium))
                    .accessibilityAddTraits(.isHeader)
                HStack(spacing: 8) {
                    Button("Keep recording") { model.answerCancel(promptId, choice: .keep) }
                        .buttonStyle(.plain)
                        .keyboardShortcut(.defaultAction)
                        .padding(.horizontal, 10)
                        .frame(height: 28)
                        .background(Theme.hudControl, in: Capsule())
                    Button("Cancel dictation", role: .destructive) {
                        model.answerCancel(promptId, choice: .cancel)
                    }
                    .buttonStyle(.plain)
                    .keyboardShortcut(.delete, modifiers: .command)
                    .help("Cancel dictation (⌘⌫)")
                    .padding(.horizontal, 8)
                    .frame(height: 28)
                }
                .font(.system(size: 12, weight: .medium))
            }
            .padding(12)
            .foregroundStyle(Theme.hudInk)
            .background(Theme.hud, in: RoundedRectangle(cornerRadius: Theme.radiusDialog))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusDialog).strokeBorder(Theme.hudBorder, lineWidth: 1))
            .environment(\.colorScheme, .dark)
            .padding(8)
            .onExitCommand { model.answerCancel(promptId, choice: .keep) }
        }
    }
}

struct HUDPill: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(spacing: 8) {
            if model.settings.settings.overlayPosition == .top {
                capsule
            }
            if showsMeetingSuggestion {
                meetingSuggestion
                    .transition(.opacity.animation(.easeInOut(duration: reduceMotion ? 0.1 : 0.2)))
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

    private var showsMeetingSuggestion: Bool {
        guard model.meetingSuggested, presentation.feedback == nil else { return false }
        if case .recording = presentation.capture { return true }
        return false
    }

    /// Offered once a dictation reaches three minutes: end it as any other
    /// and start a meeting for the rest.
    private var meetingSuggestion: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Meeting mode suits long recordings.")
                .font(.system(size: 12))
                .foregroundStyle(Theme.hudInkSecondary)
            Button(action: model.finishIntoMeeting) {
                Text("Finish and start a meeting")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Theme.hudInk)
                    .padding(.horizontal, 10)
                    .frame(height: 24)
                    .background(Theme.hudControl, in: Capsule())
                    .contentShape(Capsule())
            }
            .buttonStyle(.plain)
        }
        .frame(width: 212, alignment: .leading)
        .padding(12)
        .background(Theme.hud, in: RoundedRectangle(cornerRadius: Theme.radiusDialog))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusDialog).strokeBorder(Theme.hudBorder, lineWidth: 1))
        .environment(\.colorScheme, .dark)
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
                .frame(width: showsSlowConnection ? 236 : 176, height: 36)
                .animation(reduceMotion ? nil : .easeInOut(duration: 0.2), value: phase)
                .background(Theme.hud, in: Capsule())
                .overlay(Capsule().strokeBorder(Theme.hudBorder, lineWidth: 1))
            }
        }
        .foregroundStyle(Theme.hudInk)
        .environment(\.colorScheme, .dark)
    }

    @ViewBuilder private var activeContent: some View {
        if let feedback = presentation.feedback {
            if feedback == .held {
                Button(action: model.pasteHeldDictation) {
                    feedbackLabel(feedback)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityHint("Pastes the words into the window in front.")
            } else if case let .learned(word) = feedback {
                HStack(spacing: 6) {
                    Text(word.learned.written)
                        .font(.system(size: 12, weight: .medium))
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .accessibilityLabel(feedback.label)
                    Spacer(minLength: 4)
                    Button { model.undoLearnedWord(word) } label: {
                        Text("Undo")
                            .font(.system(size: 12, weight: .medium))
                            .padding(.horizontal, 10)
                            .frame(height: 24)
                            .background(Theme.hudControl, in: Capsule())
                            .contentShape(Capsule())
                    }
                    .buttonStyle(.plain)
                    .accessibilityHint("Removes the spelling Sona just learned.")
                }
                .padding(.leading, 14)
                .padding(.trailing, 6)
            } else {
                feedbackLabel(feedback)
                    .accessibilityElement(children: .combine)
            }
        } else {
            HStack(spacing: 6) {
                control("Cancel", symbol: "xmark") { model.cancelCapture() }
                switch presentation.capture {
                case .recording:
                    RecordingWaveform(meter: model.meter)
                        .frame(width: 55, height: 18)
                    if showsSlowConnection {
                        Text(Self.slowConnection)
                            .font(.system(size: 11, weight: .medium))
                            .fixedSize()
                            .frame(maxWidth: .infinity)
                    } else {
                        status
                            .font(.system(size: 11, weight: .medium))
                            .frame(minWidth: 32)
                    }
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
                        Text(model.slowConnectionStage == nil ? Self.phaseLabel(kind) : Self.slowConnection)
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

    private func feedbackLabel(_ feedback: PillFeedback) -> some View {
        HStack(spacing: 8) {
            Image(systemName: feedback.symbol)
                .font(.system(size: 13, weight: .semibold))
            Text(feedback.label)
                .font(.system(size: 12, weight: .medium))
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
        .overlay(Capsule().strokeBorder(Theme.hudBorder, lineWidth: 1))
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
            .background(Theme.hud, in: RoundedRectangle(cornerRadius: Theme.radiusDialog))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusDialog).strokeBorder(Theme.hudBorder, lineWidth: 1))
    }

    /// The pill's status while the network holds up a cloud stage.
    private static let slowConnection = "Slow connection"

    /// A recording held up by the network, unless its last minute is
    /// counting down: the countdown says the recording is about to stop.
    private var showsSlowConnection: Bool {
        guard case .recording = presentation.capture, presentation.feedback == nil else { return false }
        return model.slowConnectionStage != nil && model.pillWarningSeconds == nil
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

/// Content read from bottom to top, as a label runs on a side edge.
/// Turning a view does not turn the room it takes, so the content is
/// measured flat and given a frame of its turned size.
struct UprightText<Content: View>: View {
    private let content: Content
    @State private var flat = CGSize.zero

    init(@ViewBuilder _ content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        content
            .lineLimit(1)
            .fixedSize()
            .onGeometryChange(for: CGSize.self) { $0.size } action: { flat = $0 }
            .rotationEffect(.degrees(-90))
            .frame(width: flat.height, height: flat.width)
    }
}

extension UprightText where Content == Text {
    init(_ text: String) {
        self.init { Text(text) }
    }
}

/// Where a floating panel sits on the screen the pointer is on.
enum PanelPlacement: Equatable {
    /// Centred along one edge of the screen.
    case edge(HudPillEdge)
    /// Tucked into the top right corner.
    case topTrailing
    /// Flush against the right edge, centred top to bottom.
    case flushRight
    /// Centred left to right, its top a quarter of the way down, where
    /// Spotlight sits.
    case upperCenter
}

/// How a floating panel comes and goes.
enum PanelArrival {
    /// It fades in and out where it stands, and eases to a new size.
    case fade
    /// It slides in from the right edge of its screen and leaves the same
    /// way, frame and alpha together on `Theme.Slide`. Its content animates
    /// its own changes, so the window only makes room for them at once.
    case slide
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
    /// Hears the keyboard go to another window.
    var resigned: (@MainActor () -> Void)?

    override var canBecomeKey: Bool { takesKeys }

    override func sendEvent(_ event: NSEvent) {
        if intercept?(event) == true { return }
        super.sendEvent(event)
    }

    override func resignKey() {
        super.resignKey()
        resigned?()
    }
}

/// Hears the pointer enter and leave a view, and the presses on it. The
/// panels never become key and usually sit over another app, which only an
/// `.activeAlways` tracking area still hears.
private struct PointerTracking: NSViewRepresentable {
    let inside: (Bool) -> Void
    let press: () -> Void

    func makeNSView(context: Context) -> TrackingView {
        TrackingView()
    }

    func updateNSView(_ view: TrackingView, context: Context) {
        view.inside = inside
        view.press = press
    }

    final class TrackingView: NSView {
        var inside: ((Bool) -> Void)?
        var press: (() -> Void)?
        private var monitor: Any?

        override init(frame: NSRect) {
            super.init(frame: frame)
            addTrackingArea(NSTrackingArea(
                rect: .zero,
                options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect],
                owner: self,
                userInfo: nil))
        }

        required init?(coder: NSCoder) { nil }

        override func hitTest(_ point: NSPoint) -> NSView? { nil }

        override func mouseEntered(with event: NSEvent) {
            inside?(true)
        }

        override func mouseExited(with event: NSEvent) {
            inside?(false)
        }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if let monitor {
                NSEvent.removeMonitor(monitor)
                self.monitor = nil
            }
            guard window != nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { [weak self] event in
                MainActor.assumeIsolated {
                    guard let self, event.window === self.window,
                          self.bounds.contains(self.convert(event.locationInWindow, from: nil))
                    else { return }
                    self.press?()
                }
                return event
            }
        }
    }
}

extension View {
    /// `inside` hears the pointer enter and leave this view and `press` a
    /// press on it, while another app is in front too.
    func onPointer(inside: @escaping (Bool) -> Void, press: @escaping () -> Void) -> some View {
        background(PointerTracking(inside: inside, press: press))
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
    private let arrival: PanelArrival
    /// Takes the keyboard while shown, for a panel that is typed in. Hiding
    /// gives it back to the app in front.
    private let keyed: Bool
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
    /// Where a slide is headed while one runs. Bumping `glideRevision` lets
    /// only the latest slide's end say it is over.
    private var glideTarget: NSRect?
    private var glideRevision = 0
    /// A shrink waiting out the transition of the view on its way out.
    private var settling: Task<Void, Never>?
    /// While on, a left press on the panel is the panel's: a release without
    /// travel is a click, and travel is a drag to another edge. Off, presses
    /// reach the views as they always did.
    var dockable = false
    /// A click, while `dockable`.
    var onClick: (@MainActor () -> Void)?
    /// A drop on an edge, while `dockable`.
    var onDock: (@MainActor (HudPillEdge) -> Void)?
    /// The keyboard went to another window while a keyed panel was up.
    var onResignKey: (@MainActor () -> Void)?
    /// A press must travel this far to be a drag rather than a click.
    private static let dragSlop: CGFloat = 4
    private static let escape: UInt16 = 53

    private var contentAlignment: Alignment {
        guard let placement else { return .center }
        switch placement {
        case let .edge(edge): return edge.alignment
        case .topTrailing: return .topTrailing
        case .flushRight: return .trailing
        case .upperCenter: return .top
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

    init(_ content: some View, arrival: PanelArrival = .fade, keyed: Bool = false) {
        self.arrival = arrival
        self.keyed = keyed
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
        panel.resigned = { [weak self] in
            guard let self, self.keyed, self.shown else { return }
            self.onResignKey?()
        }
    }

    /// Floats the panel at `placement` on the screen the pointer is on. A
    /// panel already up at the same placement stays exactly where it is,
    /// unless `anew` asks for it to be floated again all the same.
    func show(at placement: PanelPlacement, anew: Bool = false) {
        defer {
            if keyed {
                panel.takesKeys = true
                panel.makeKey()
            }
        }
        let appearing = !panel.isVisible
        guard anew || !shown || placement != self.placement else { return }
        if drag != nil { cancelDrag() }
        shown = true
        visibilityRevision += 1
        panel.ignoresMouseEvents = false
        self.placement = placement
        let screen = NSScreen.screens.first(where: { $0.frame.contains(NSEvent.mouseLocation) }) ?? NSScreen.main
        if let screen {
            anchor = Self.anchor(for: placement, on: screen.visibleFrame)
        }
        let target = frame(host.fittingSize, at: placement)
        switch arrival {
        case .fade:
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
        case .slide:
            if appearing {
                let start = Self.reduceMotion ? target : Self.offstage(target, beyond: screen?.frame ?? target)
                panel.setFrame(start, display: false)
                panel.alphaValue = 0
                panel.orderFrontRegardless()
            }
            glide(to: target, alpha: 1)
        }
    }

    func hide() {
        guard shown else { return }
        if drag != nil { cancelDrag() }
        shown = false
        visibilityRevision += 1
        let revision = visibilityRevision
        panel.ignoresMouseEvents = true
        if keyed {
            panel.takesKeys = false
            panel.orderOut(nil)
            panel.orderFrontRegardless()
        }
        switch arrival {
        case .fade:
            NSAnimationContext.runAnimationGroup { context in
                context.duration = Self.fadeDuration
                panel.animator().alphaValue = 0
            } completionHandler: { [weak self] in
                MainActor.assumeIsolated {
                    guard let self, !self.shown, self.visibilityRevision == revision else { return }
                    self.panel.orderOut(nil)
                }
            }
        case .slide:
            let exit = Self.reduceMotion ? panel.frame : Self.offstage(panel.frame, beyond: screen?.frame ?? panel.frame)
            glide(to: exit, alpha: 0)
        }
    }

    private func fit(_ size: CGSize) {
        guard let placement, shown else { return }
        if case .dragging = drag { return }
        let target = frame(size, at: placement)
        switch arrival {
        case .fade:
            move(to: target)
        case .slide:
            if let glideTarget {
                if glideTarget != target { glide(to: target, alpha: 1) }
            } else {
                settle(to: target)
            }
        }
    }

    private static var fadeDuration: TimeInterval {
        NSWorkspace.shared.accessibilityDisplayShouldReduceMotion ? 0.1 : 0.18
    }

    private static var reduceMotion: Bool {
        NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
    }

    /// A sliding panel's one motion: frame and alpha together on the panel
    /// curve. With motion reduced, the frame lands at once and only the
    /// alpha moves. A slide that ends with the panel hidden orders it out.
    private func glide(to target: NSRect, alpha: CGFloat) {
        settling?.cancel()
        glideRevision += 1
        let revision = glideRevision
        glideTarget = target
        let reduced = Self.reduceMotion
        if reduced { panel.setFrame(target, display: true) }
        NSAnimationContext.runAnimationGroup { context in
            context.duration = reduced ? Theme.Slide.fade : Theme.Slide.duration
            context.timingFunction = reduced ? CAMediaTimingFunction(name: .easeInEaseOut) : Theme.Slide.timing
            if !reduced { panel.animator().setFrame(target, display: true) }
            panel.animator().alphaValue = alpha
        } completionHandler: { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.glideRevision == revision else { return }
                self.glideTarget = nil
                if !self.shown { self.panel.orderOut(nil) }
            }
        }
    }

    /// SwiftUI draws a view on its way out until its transition ends, so a
    /// panel shrinking to new content keeps room for both until then.
    private func settle(to target: NSRect) {
        settling?.cancel()
        let room = panel.frame.union(target)
        panel.setFrame(room, display: true)
        guard room != target else { return }
        settling = Task { [weak self] in
            try? await Task.sleep(for: .seconds(Theme.Slide.duration))
            guard !Task.isCancelled, let self else { return }
            self.panel.setFrame(target, display: true)
        }
    }

    /// `frame`, moved right until it clears `screen`'s right edge.
    private static func offstage(_ frame: NSRect, beyond screen: NSRect) -> NSRect {
        frame.offsetBy(dx: max(0, screen.maxX - frame.minX), dy: 0)
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
        case .flushRight: NSPoint(x: visible.maxX, y: visible.midY)
        case .upperCenter: NSPoint(x: visible.midX, y: visible.maxY - visible.height / 4)
        }
    }

    private func frame(_ size: CGSize, at placement: PanelPlacement) -> NSRect {
        let origin = switch placement {
        case .edge(.bottom): NSPoint(x: anchor.x - size.width / 2, y: anchor.y)
        case .edge(.top), .upperCenter: NSPoint(x: anchor.x - size.width / 2, y: anchor.y - size.height)
        case .edge(.left): NSPoint(x: anchor.x, y: anchor.y - size.height / 2)
        case .edge(.right), .flushRight: NSPoint(x: anchor.x - size.width, y: anchor.y - size.height / 2)
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
