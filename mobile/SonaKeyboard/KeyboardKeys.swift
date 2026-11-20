import SwiftUI
import UIKit

enum KeyLayer: Equatable {
    case letters, numbers, symbols

    var title: String {
        switch self {
        case .letters: "ABC"
        case .numbers: "123"
        case .symbols: "#+="
        }
    }

    var label: LocalizedStringKey {
        switch self {
        case .letters: "keyboard.letters"
        case .numbers: "keyboard.numbers"
        case .symbols: "keyboard.symbols"
        }
    }
}

enum Shift: Equatable {
    case off, once, locked

    var symbol: String {
        switch self {
        case .off: "shift"
        case .once: "shift.fill"
        case .locked: "capslock.fill"
        }
    }
}

enum Key: Hashable {
    case character(String)
    case shift
    case delete
    case layer(KeyLayer)
    case nextKeyboard
    case language
    case space
    case newLine
}

/// One key and its width, in key units: a row of ten letters is ten units wide.
struct KeyCap: Hashable {
    let key: Key
    var units = 1.0
}

/// A pressed letter and the key it sits on.
struct KeyCallout: Equatable {
    let text: String
    let anchor: CGRect
}

/// A held letter's accented forms, laid out one key wide each from `originX`.
struct AccentChoice: Equatable {
    let anchor: CGRect
    let options: [String]
    let originX: CGFloat
    var selected = 0
}

/// Where every key sits: keys share one unit width, each row is centred, and the block is
/// never wider than `maxWidth`, so a wide display gets centred keys instead of stretched ones.
struct KeyGrid {
    static let maxWidth: CGFloat = 620

    let unit: CGFloat
    let rowHeight: CGFloat
    let origin: CGPoint
    let frames: [(cap: KeyCap, frame: CGRect)]

    init(rows: [[KeyCap]], size: CGSize) {
        let columns = max(10, rows.map { $0.reduce(0) { $0 + $1.units } }.max() ?? 10)
        let width = min(size.width, Self.maxWidth)
        let unit = width / columns
        let rowHeight = size.height / CGFloat(max(rows.count, 1))
        let origin = CGPoint(x: (size.width - width) / 2, y: 0)
        self.unit = unit
        self.rowHeight = rowHeight
        self.origin = origin
        frames = rows.enumerated().flatMap { index, row in
            var x = origin.x + (width - row.reduce(0) { $0 + $1.units } * unit) / 2
            return row.map { cap in
                defer { x += cap.units * unit }
                return (cap, CGRect(x: x, y: rowHeight * CGFloat(index), width: cap.units * unit, height: rowHeight))
            }
        }
    }

    /// The key under `point`, or the nearest key in that row when the point falls in a gap.
    func key(at point: CGPoint) -> (cap: KeyCap, frame: CGRect)? {
        if let hit = frames.first(where: { $0.frame.contains(point) }) { return hit }
        guard rowHeight > 0, !frames.isEmpty else { return nil }
        let rows = Int((frames.map(\.frame.maxY).max() ?? 0) / rowHeight)
        let row = min(max(Int(point.y / rowHeight), 0), rows - 1)
        return frames.filter { Int($0.frame.minY / rowHeight) == row }
            .min { abs($0.frame.midX - point.x) < abs($1.frame.midX - point.x) }
    }
}

/// The return key's face for the field's return key type.
struct ReturnKey: Equatable {
    var title: String?
    var prominent = false

    init(_ type: UIReturnKeyType) {
        switch type {
        case .go: title = "keyboard.return.go"
        case .google, .yahoo, .search: title = "keyboard.return.search"
        case .join: title = "keyboard.return.join"
        case .next: title = "keyboard.return.next"
        case .route: title = "keyboard.return.route"
        case .send: title = "keyboard.return.send"
        case .done: title = "keyboard.return.done"
        case .emergencyCall: title = "keyboard.return.emergencyCall"
        case .continue: title = "keyboard.return.continue"
        case .default: title = nil
        @unknown default: title = nil
        }
        prominent = title != nil && type != .next
    }
}

/// What the typing keys show, set by the keyboard controller and read by the key views.
@MainActor
@Observable
final class TypingModel {
    var layer = KeyLayer.letters
    var language = TypingLanguage.english
    var shift = Shift.off
    var suggestions: Suggestions?
    var returnKey = ReturnKey(.default)
    var extraKeys: [String] = []
    var showsNextKeyboard = true
    var showsLanguageKey = false
    var pressed: Set<Key> = []
    var trail: [CGPoint] = []
    var accentChoice: AccentChoice?
    /// The letter under a finger, shown enlarged above its key as the system keyboard does.
    var callout: KeyCallout?
    @ObservationIgnored var press: (Key) -> Void = { _ in }
    @ObservationIgnored var choose: (String) -> Void = { _ in }
    @ObservationIgnored var swipe: ([CGPoint], KeyGrid) -> Void = { _, _ in }

    var rows: [[KeyCap]] {
        switch layer {
        case .letters:
            let letters = language.letterRows
            return [
                Self.caps(letters[0]),
                Self.caps(letters[1]),
                [KeyCap(key: .shift, units: 1.5)] + Self.caps(letters[2]) + [KeyCap(key: .delete, units: 1.5)],
            ]
        case .numbers:
            return [
                Self.caps("1234567890"),
                Self.caps("-/:;()$&@\""),
                [KeyCap(key: .layer(.symbols), units: 1.5)] + Self.caps(".,?!'", units: 1.4)
                    + [KeyCap(key: .delete, units: 1.5)],
            ]
        case .symbols:
            return [
                Self.caps("[]{}#%^*+="),
                Self.caps("_\\|~<>€£¥•"),
                [KeyCap(key: .layer(.numbers), units: 1.5)] + Self.caps(".,?!'", units: 1.4)
                    + [KeyCap(key: .delete, units: 1.5)],
            ]
        }
    }

    func bottomRow(withDelete: Bool) -> [KeyCap] {
        var leading = [KeyCap(key: .layer(layer == .letters ? .numbers : .letters), units: 1.5)]
        if showsNextKeyboard { leading.append(KeyCap(key: .nextKeyboard, units: 1.25)) }
        if showsLanguageKey { leading.append(KeyCap(key: .language, units: 1.25)) }
        if !withDelete { leading += extraKeys.map { KeyCap(key: .character($0)) } }
        var trailing = [KeyCap(key: .newLine, units: 2)]
        if withDelete { trailing.insert(KeyCap(key: .delete, units: 1.5), at: 0) }
        let used = (leading + trailing).reduce(0) { $0 + $1.units }
        return leading + [KeyCap(key: .space, units: 10 - used)] + trailing
    }

    /// What a character key types with the current shift.
    func face(of text: String) -> String {
        layer == .letters && shift != .off ? text.uppercased() : text
    }

    private static func caps(_ characters: String, units: Double = 1) -> [KeyCap] {
        characters.map { KeyCap(key: .character(String($0)), units: units) }
    }
}

/// Rows of keys laid out by `KeyGrid`. Without `taps` the faces only show what a
/// `KeySurface` over them is doing.
struct KeyRows: View {
    let model: TypingModel
    let rows: [[KeyCap]]
    var taps = true

    var body: some View {
        GeometryReader { proxy in
            let grid = KeyGrid(rows: rows, size: proxy.size)
            ForEach(Array(grid.frames.enumerated()), id: \.offset) { _, key in
                KeyButton(cap: key.cap, model: model, taps: taps)
                    .frame(width: key.frame.width, height: key.frame.height)
                    .position(x: key.frame.midX, y: key.frame.midY)
            }
        }
    }
}

private struct KeyButton: View {
    let cap: KeyCap
    let model: TypingModel
    let taps: Bool

    var body: some View {
        if !taps {
            label.modifier(KeyFace(pressed: model.pressed.contains(cap.key)))
                .accessibilityHidden(true)
        } else if cap.key == .delete {
            DeleteKey(press: model.press)
        } else {
            Button { model.press(cap.key) } label: { label }
                .buttonStyle(TypingKeyStyle(prominent: cap.key == .newLine && model.returnKey.prominent))
                .accessibilityAddTraits(.isKeyboardKey)
        }
    }

    @ViewBuilder private var label: some View {
        switch cap.key {
        case let .character(text):
            Text(verbatim: model.face(of: text))
                .font(.system(size: KeyFace.letterSize))
        case .shift:
            Image(systemName: model.shift.symbol)
                .font(.system(size: 18))
                .accessibilityLabel(model.shift == .locked ? Text("keyboard.capsLock") : Text("keyboard.shift"))
                .accessibilityAddTraits(model.shift == .off ? [] : .isSelected)
        case let .layer(layer):
            Text(verbatim: layer.title)
                .font(.system(size: 16))
                .accessibilityLabel(Text(layer.label))
        case .nextKeyboard:
            Image(systemName: "globe")
                .font(.system(size: 18))
                .accessibilityLabel(Text("keyboard.next"))
        case .language:
            Text(verbatim: model.language.code)
                .font(.system(size: 16))
                .accessibilityLabel(Text("keyboard.language \(model.language.name)"))
        case .space:
            Text("keyboard.space")
                .font(.system(size: 16))
        case .newLine:
            if let title = model.returnKey.title {
                Text(LocalizedStringKey(title)).font(.system(size: 16)).lineLimit(1).minimumScaleFactor(0.6)
            } else {
                Image(systemName: "return")
                    .font(.system(size: 18))
                    .accessibilityLabel(Text("keyboard.return"))
            }
        case .delete:
            Image(systemName: "delete.left")
                .font(.system(size: 18))
        }
    }
}

/// Deletes on touch, then keeps deleting while held, like the system keyboard.
private struct DeleteKey: View {
    let press: (Key) -> Void
    @GestureState private var isPressed = false
    @State private var repeating: Task<Void, Never>?

    var body: some View {
        Image(systemName: "delete.left")
            .font(.system(size: 18))
            .modifier(KeyFace(pressed: isPressed))
            .gesture(DragGesture(minimumDistance: 0).updating($isPressed) { _, pressed, _ in pressed = true })
            .onChange(of: isPressed) { _, pressed in
                repeating?.cancel()
                repeating = pressed ? repeatDelete() : nil
            }
            .onDisappear { repeating?.cancel() }
            .accessibilityElement()
            .accessibilityLabel(Text("keyboard.delete"))
            .accessibilityAddTraits([.isButton, .isKeyboardKey])
            .accessibilityAction { press(.delete) }
    }

    private func repeatDelete() -> Task<Void, Never> {
        press(.delete)
        return Task {
            try? await Task.sleep(for: .seconds(0.45))
            while !Task.isCancelled {
                press(.delete)
                try? await Task.sleep(for: .seconds(0.09))
            }
        }
    }
}

private struct TypingKeyStyle: ButtonStyle {
    let prominent: Bool

    func makeBody(configuration: Configuration) -> some View {
        configuration.label.modifier(KeyFace(pressed: configuration.isPressed, prominent: prominent))
    }
}

/// Keys are flat fills with a one-point shadow over the system keyboard backdrop, as the
/// system keyboard draws them; the return key carries the accent when the field names an
/// action.
struct KeyFace: ViewModifier {
    /// Letters on the keys and in accent choices; a pressed letter shows larger above its key.
    static let letterSize: CGFloat = 23
    static let calloutSize: CGFloat = 32
    static let fill = Color(uiColor: UIColor { $0.userInterfaceStyle == .dark ? UIColor(white: 0.42, alpha: 1) : .white })
    static let pressedFill = Color(uiColor: UIColor {
        $0.userInterfaceStyle == .dark ? UIColor(white: 0.3, alpha: 1) : UIColor(white: 0.82, alpha: 1)
    })
    let pressed: Bool
    var prominent = false

    func body(content: Content) -> some View {
        content
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .foregroundStyle(prominent ? Color.white : Theme.textPrimary)
            .background(fill, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
            .shadow(color: .black.opacity(0.3), radius: 0, y: 1)
            .padding(.horizontal, 3)
            .padding(.vertical, 5)
            .contentShape(Rectangle())
    }

    private var fill: Color {
        if prominent { return Theme.accent.opacity(pressed ? 0.7 : 1) }
        return pressed ? Self.pressedFill : Self.fill
    }
}

/// Up to three words for the one being typed. A correction shows next to the word as
/// typed, so either can be kept with one tap.
struct SuggestionStrip: View {
    private struct Candidate: Hashable {
        let word: String
        var quoted = false
        var highlighted = false
    }

    let suggestions: Suggestions
    let choose: (String) -> Void

    var body: some View {
        HStack(spacing: 0) {
            ForEach(Array(candidates.enumerated()), id: \.element) { index, candidate in
                if index > 0 {
                    Rectangle().fill(Theme.border).frame(width: 1, height: 22)
                }
                Button { choose(candidate.word) } label: {
                    Text(verbatim: candidate.quoted ? "\u{201C}\(candidate.word)\u{201D}" : candidate.word)
                        .font(.system(size: 17, weight: candidate.highlighted ? .semibold : .regular))
                        .lineLimit(1)
                        .minimumScaleFactor(0.6)
                        .padding(.horizontal, 4)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(
                            candidate.highlighted ? Theme.surface : Color.clear,
                            in: RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous)
                        )
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
    }

    private var candidates: [Candidate] {
        let leading = suggestions.correction.map {
            [Candidate(word: suggestions.typed, quoted: true), Candidate(word: $0, highlighted: true)]
        } ?? []
        let others = suggestions.others.filter { $0 != suggestions.correction }.map { Candidate(word: $0) }
        return Array((leading + others).prefix(3))
    }
}
