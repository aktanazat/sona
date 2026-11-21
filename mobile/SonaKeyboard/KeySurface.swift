import SwiftUI
import UIKit

/// The touch layer over every key row. One view sees every finger, so a tap types the key
/// it lifts on, a second finger types the first one's key at once as the system keyboard
/// does, a drag across letters is a swipe, a held letter offers its accents, the space bar
/// held or dragged moves the cursor, a slide left from delete takes whole words, and a
/// slide from 123 or shift onto a key types it and springs back.
struct KeySurface: UIViewRepresentable {
    let model: TypingModel
    let rows: [[KeyCap]]
    let swipes: Bool

    func makeUIView(context: Context) -> KeyTouchView { KeyTouchView(model: model) }

    func updateUIView(_ view: KeyTouchView, context: Context) {
        view.rows = rows
        view.swipes = swipes
    }
}

@MainActor
final class KeyTouchView: UIView {
    private final class Finger {
        enum Mode { case key, swipe, trackpad, deleteSlide }

        var key: (cap: KeyCap, frame: CGRect)?
        let start: CGPoint
        var path: [CGPoint]
        var mode = Mode.key
        var hold: Task<Void, Never>?
        var repeating: Task<Void, Never>?
        /// Deletes the held delete key has made, so lifting does not delete once more.
        var repeats = 0
        /// The layer a finger that went down on 123 or #+= came from, to spring back to.
        var returnLayer: KeyLayer?
        var moved = false
        var anchorX: CGFloat
        /// Words a slide on delete took, last first out as the slide comes back.
        var deleted: [String] = []

        init(key: (cap: KeyCap, frame: CGRect)?, start: CGPoint) {
            self.key = key
            self.start = start
            path = [start]
            anchorX = start.x
        }
    }

    /// Points of travel on the space bar per character the cursor moves.
    private static let trackpadStep: CGFloat = 9

    private let model: TypingModel
    private var fingers: [UITouch: Finger] = [:]
    private var grid = KeyGrid(rows: [], size: .zero)
    private var keyElements: [UIAccessibilityElement]?
    private let ticks = UISelectionFeedbackGenerator()
    var rows: [[KeyCap]] = [] {
        didSet { if rows != oldValue { relayout() } }
    }
    var swipes = false

    init(model: TypingModel) {
        self.model = model
        super.init(frame: .zero)
        isMultipleTouchEnabled = true
        backgroundColor = .clear
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func layoutSubviews() {
        super.layoutSubviews()
        relayout()
    }

    private func relayout() {
        grid = KeyGrid(rows: rows, size: bounds.size)
        keyElements = nil
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        KeyboardFeel.touching(true)
        for touch in touches {
            for (other, finger) in fingers where finger.mode == .key && finger.repeating == nil
                && finger.returnLayer == nil && model.accentChoice == nil && !isModifier(finger.key?.cap.key) {
                end(other, finger, typing: true)
            }
            let point = touch.location(in: self)
            let finger = Finger(key: grid.key(at: point), start: point)
            fingers[touch] = finger
            guard let key = finger.key?.cap.key else { continue }
            model.touched(key)
            model.pressed.insert(key)
            switch key {
            case .delete:
                finger.repeating = repeatDelete(finger)
            case .layer(.numbers), .layer(.symbols), .layer(.letters):
                finger.returnLayer = model.layer
                model.press(key)
            case .shift:
                model.press(key)
            case .space:
                holdForTrackpad(finger)
            default:
                showPreview(finger)
                holdForChoices(finger)
            }
        }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            guard let finger = fingers[touch] else { continue }
            let point = touch.location(in: self)
            let travel = CGPoint(x: point.x - finger.start.x, y: point.y - finger.start.y)
            if hypot(travel.x, travel.y) > 4 { finger.moved = true }
            if var choice = model.accentChoice {
                choice.selected = min(max(Int((point.x - choice.originX) / choice.anchor.width), 0), choice.options.count - 1)
                if choice.selected != model.accentChoice?.selected { ticks.selectionChanged() }
                model.accentChoice = choice
                continue
            }
            switch finger.mode {
            case .trackpad:
                moveCursor(finger, to: point)
            case .deleteSlide:
                slideDelete(finger, to: point)
            case .swipe:
                finger.path.append(point)
                model.trail = finger.path
            case .key:
                follow(finger, to: point, travel: travel)
            }
        }
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            guard let finger = fingers[touch] else { continue }
            end(touch, finger, typing: true)
        }
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            guard let finger = fingers[touch] else { continue }
            end(touch, finger, typing: false)
        }
    }

    /// A finger still on a key: it may start a swipe, a word slide on delete, the trackpad,
    /// or move to another key, which then is the one it types.
    private func follow(_ finger: Finger, to point: CGPoint, travel: CGPoint) {
        finger.path.append(point)
        guard let key = finger.key?.cap.key else { return }
        switch key {
        case .character where swipes && finger.returnLayer == nil:
            if hypot(travel.x, travel.y) > grid.unit * 0.9 {
                finger.mode = .swipe
                finger.hold?.cancel()
                model.pressed = []
                model.preview = nil
                model.trail = finger.path
                return
            }
        case .delete where finger.repeats == 0 && -travel.x > grid.unit * 0.6:
            finger.mode = .deleteSlide
            finger.repeating?.cancel()
            finger.anchorX = point.x
            slideDelete(finger, to: point)
            return
        case .space where abs(travel.x) > 10:
            startTrackpad(finger, at: point)
            return
        default:
            break
        }
        guard finger.mode == .key, let under = grid.key(at: point), under.cap != finger.key?.cap,
              key != .delete, key != .space
        else { return }
        if finger.returnLayer == nil, case .layer = key { return }
        if key == .shift, finger.returnLayer == nil {
            finger.returnLayer = model.layer
        }
        finger.key.map { model.pressed.remove($0.cap.key) }
        finger.key = under
        model.pressed.insert(under.cap.key)
        showPreview(finger)
        holdForChoices(finger)
    }

    private func end(_ touch: UITouch, _ finger: Finger, typing: Bool) {
        fingers[touch] = nil
        if fingers.isEmpty { KeyboardFeel.touching(false) }
        finger.hold?.cancel()
        finger.repeating?.cancel()
        finger.key.map { model.pressed.remove($0.cap.key) }
        model.preview = nil
        if let choice = model.accentChoice {
            model.accentChoice = nil
            if typing { model.press(.character(choice.options[choice.selected])) }
            return
        }
        switch finger.mode {
        case .swipe:
            model.trail = []
            if typing { model.swipe(finger.path, grid) }
        case .trackpad:
            withAnimation(.easeOut(duration: 0.18)) { model.trackpad = false }
        case .deleteSlide:
            break
        case .key:
            guard typing, let key = finger.key?.cap.key else { return }
            if let returnLayer = finger.returnLayer {
                guard finger.moved, isTyping(key) else { return }
                model.press(key)
                KeyboardFeel.typed(since: touch.timestamp)
                if model.layer != returnLayer, returnLayer != .emoji { model.press(.layer(returnLayer)) }
                return
            }
            switch key {
            case .shift, .layer(.numbers), .layer(.symbols), .layer(.letters):
                return
            case .delete where finger.repeats > 0:
                return
            default:
                model.press(key)
            }
            KeyboardFeel.typed(since: touch.timestamp)
        }
    }

    private func isTyping(_ key: Key) -> Bool {
        switch key {
        case .character, .domain, .space, .newLine: true
        default: false
        }
    }

    private func isModifier(_ key: Key?) -> Bool {
        switch key {
        case .shift?, .layer?: true
        default: false
        }
    }

    private func showPreview(_ finger: Finger) {
        guard let key = finger.key else { model.preview = nil; return }
        switch key.cap.key {
        case let .character(text) where model.layer != .emoji:
            model.preview = KeyPreview(text: model.face(of: text), frame: key.frame)
        default:
            model.preview = nil
        }
    }

    /// A held letter offers its accents, and a held .com the other endings.
    private func holdForChoices(_ finger: Finger) {
        finger.hold?.cancel()
        guard let key = finger.key else { return }
        let options: [String]
        switch key.cap.key {
        case let .character(text) where model.layer == .letters:
            guard let letter = text.first, let accents = model.language.accents[letter] else { return }
            options = ([text] + accents.map(String.init)).map(model.face(of:))
        case .domain:
            options = TypingModel.domains
        default:
            return
        }
        finger.hold = Task { [weak self] in
            try? await Task.sleep(for: .seconds(0.4))
            guard !Task.isCancelled, let self, finger.mode == .key else { return }
            let width = CGFloat(options.count) * key.frame.width
            let originX = min(max(key.frame.minX, 4), bounds.width - width - 4)
            model.pressed.remove(key.cap.key)
            model.preview = nil
            ticks.selectionChanged()
            model.accentChoice = AccentChoice(anchor: key.frame, options: options, originX: originX)
        }
    }

    private func holdForTrackpad(_ finger: Finger) {
        finger.hold = Task { [weak self] in
            try? await Task.sleep(for: .seconds(0.45))
            guard !Task.isCancelled, let self, finger.mode == .key, !finger.moved else { return }
            startTrackpad(finger, at: finger.start)
        }
    }

    private func startTrackpad(_ finger: Finger, at point: CGPoint) {
        finger.hold?.cancel()
        finger.mode = .trackpad
        finger.anchorX = point.x
        model.pressed = []
        ticks.selectionChanged()
        withAnimation(.easeOut(duration: 0.18)) { model.trackpad = true }
    }

    private func moveCursor(_ finger: Finger, to point: CGPoint) {
        let steps = Int((point.x - finger.anchorX) / Self.trackpadStep)
        guard steps != 0 else { return }
        finger.anchorX += CGFloat(steps) * Self.trackpadStep
        if model.moveCursor(steps) != 0 { ticks.selectionChanged() }
    }

    /// Each further stretch of travel left takes one more word; travelling back puts the
    /// last one taken back.
    private func slideDelete(_ finger: Finger, to point: CGPoint) {
        let wanted = max(0, Int((finger.anchorX - point.x) / (grid.unit * 0.7)) + 1)
        while finger.deleted.count < wanted, let word = model.deleteWord() {
            finger.deleted.append(word)
            ticks.selectionChanged()
        }
        while finger.deleted.count > wanted, let word = finger.deleted.popLast() {
            model.restore(word)
            ticks.selectionChanged()
        }
    }

    /// Held, delete starts repeating; after a dozen characters it takes whole words, as the
    /// system keyboard speeds up. While the field is still behind the keyboard's last edit no
    /// word is taken, so it goes on a character at a time.
    private func repeatDelete(_ finger: Finger) -> Task<Void, Never> {
        Task { [model] in
            try? await Task.sleep(for: .seconds(0.45))
            while !Task.isCancelled {
                if finger.repeats < 12 || model.deleteWord() == nil {
                    model.press(.delete)
                }
                finger.repeats += 1
                try? await Task.sleep(for: .seconds(finger.repeats < 12 ? 0.09 : 0.22))
            }
        }
    }

    override var accessibilityElements: [Any]? {
        get {
            if let keyElements { return keyElements }
            let elements = grid.frames.map { key in
                KeyElement(container: self, key: key.cap.key, model: model, frame: key.frame)
            }
            keyElements = elements
            return elements
        }
        set { keyElements = newValue as? [UIAccessibilityElement] }
    }
}

/// One key for VoiceOver, which cannot see keys drawn under a single touch view.
private final class KeyElement: UIAccessibilityElement {
    private let key: Key
    private let model: TypingModel

    @MainActor
    init(container: UIView, key: Key, model: TypingModel, frame: CGRect) {
        self.key = key
        self.model = model
        super.init(accessibilityContainer: container)
        accessibilityFrameInContainerSpace = frame
        accessibilityTraits = .keyboardKey
    }

    override var accessibilityLabel: String? {
        get {
            MainActor.assumeIsolated {
                switch key {
                case let .character(text): model.face(of: text)
                case .shift where model.shift == .locked: String(localized: "keyboard.capsLock")
                case .shift: String(localized: "keyboard.shift")
                case .delete: String(localized: "keyboard.delete")
                case .layer(.letters): String(localized: "keyboard.letters")
                case .layer(.numbers): String(localized: "keyboard.numbers")
                case .layer(.symbols): String(localized: "keyboard.symbols")
                case .layer(.emoji): String(localized: "keyboard.emoji")
                case .nextKeyboard: String(localized: "keyboard.next")
                case .language: String(localized: "keyboard.language \(model.language.name)")
                case .space: String(localized: "keyboard.space")
                case .newLine: model.returnKey.title.map { String(localized: String.LocalizationValue($0)) }
                    ?? String(localized: "keyboard.return")
                case .domain: TypingModel.domains[0]
                }
            }
        }
        set {}
    }

    override func accessibilityActivate() -> Bool {
        MainActor.assumeIsolated { model.press(key) }
        return true
    }
}
