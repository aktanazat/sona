import SwiftUI
import UIKit

/// The touch layer over the upper key rows. One view sees every finger, so a tap types the
/// key it lifts on, a second finger types the first one's key at once as the system keyboard
/// does, a drag across letters is a swipe, and a held letter offers its accents.
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
        var key: (cap: KeyCap, frame: CGRect)?
        let start: CGPoint
        var path: [CGPoint]
        var swiping = false
        var hold: Task<Void, Never>?
        var repeating: Task<Void, Never>?

        init(key: (cap: KeyCap, frame: CGRect)?, start: CGPoint) {
            self.key = key
            self.start = start
            path = [start]
        }
    }

    private let model: TypingModel
    private var fingers: [UITouch: Finger] = [:]
    private var grid = KeyGrid(rows: [], size: .zero)
    private var keyElements: [UIAccessibilityElement]?
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
        for touch in touches {
            for (other, finger) in fingers where !finger.swiping && finger.repeating == nil && model.accentChoice == nil {
                end(other, finger, typing: true)
            }
            let point = touch.location(in: self)
            let finger = Finger(key: grid.key(at: point), start: point)
            fingers[touch] = finger
            guard let key = finger.key?.cap.key else { continue }
            UIDevice.current.playInputClick()
            model.pressed.insert(key)
            showCallout(finger)
            if key == .delete { finger.repeating = repeatDelete() } else { holdForAccents(finger) }
        }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            guard let finger = fingers[touch], finger.repeating == nil else { continue }
            let point = touch.location(in: self)
            if var choice = model.accentChoice {
                choice.selected = min(max(Int((point.x - choice.originX) / grid.unit), 0), choice.options.count - 1)
                model.accentChoice = choice
                continue
            }
            finger.path.append(point)
            if !finger.swiping, swipes, case .character = finger.key?.cap.key,
               hypot(point.x - finger.start.x, point.y - finger.start.y) > grid.unit * 0.9 {
                finger.swiping = true
                finger.hold?.cancel()
                model.pressed = []
                model.callout = nil
            }
            if finger.swiping {
                model.trail = finger.path
            } else if let under = grid.key(at: point), under.cap != finger.key?.cap {
                finger.key.map { model.pressed.remove($0.cap.key) }
                finger.key = under
                model.pressed.insert(under.cap.key)
                showCallout(finger)
                holdForAccents(finger)
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

    private func end(_ touch: UITouch, _ finger: Finger, typing: Bool) {
        fingers[touch] = nil
        finger.hold?.cancel()
        finger.repeating?.cancel()
        finger.key.map { model.pressed.remove($0.cap.key) }
        if model.callout?.anchor == finger.key?.frame { model.callout = nil }
        if let choice = model.accentChoice {
            model.accentChoice = nil
            if typing { model.press(.character(choice.options[choice.selected])) }
        } else if finger.swiping {
            model.trail = []
            if typing { model.swipe(finger.path, grid) }
        } else if typing, finger.repeating == nil, let key = finger.key?.cap.key {
            model.press(key)
        }
    }

    private func holdForAccents(_ finger: Finger) {
        finger.hold?.cancel()
        guard model.layer == .letters, let key = finger.key, case let .character(text) = key.cap.key,
              let letter = text.first, let accents = model.language.accents[letter]
        else { return }
        finger.hold = Task { [weak self] in
            try? await Task.sleep(for: .seconds(0.4))
            guard !Task.isCancelled, let self, !finger.swiping else { return }
            let options = ([text] + accents.map(String.init)).map(model.face(of:))
            let width = CGFloat(options.count) * grid.unit
            let originX = min(max(key.frame.minX, 0), bounds.width - width)
            model.pressed.remove(key.cap.key)
            model.accentChoice = AccentChoice(anchor: key.frame, options: options, originX: originX)
        }
    }

    /// Deletes on touch, then keeps deleting while held, like the system keyboard.
    private func repeatDelete() -> Task<Void, Never> {
        model.press(.delete)
        return Task { [model] in
            try? await Task.sleep(for: .seconds(0.45))
            while !Task.isCancelled {
                model.press(.delete)
                try? await Task.sleep(for: .seconds(0.09))
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
                case .nextKeyboard, .language, .space, .newLine: nil
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

extension KeyTouchView {
    /// A letter under a finger grows above its key; other keys only darken.
    private func showCallout(_ finger: Finger) {
        guard let key = finger.key, case let .character(text) = key.cap.key else {
            model.callout = nil
            return
        }
        model.callout = KeyCallout(text: model.face(of: text), anchor: key.frame)
    }
}
