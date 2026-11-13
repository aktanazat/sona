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
    case space
    case newLine
}

/// One key and its width, in tenths of the keyboard's width.
struct KeyCap: Hashable {
    let key: Key
    var units = 1.0
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
    var shift = Shift.off
    var suggestions: Suggestions?
    var hasTyped = false
    var returnKey = ReturnKey(.default)
    var extraKeys: [String] = []
    var showsNextKeyboard = true
    @ObservationIgnored var press: (Key) -> Void = { _ in }
    @ObservationIgnored var choose: (String) -> Void = { _ in }

    var rows: [[KeyCap]] {
        switch layer {
        case .letters:
            [
                Self.caps("qwertyuiop"),
                Self.caps("asdfghjkl"),
                [KeyCap(key: .shift, units: 1.5)] + Self.caps("zxcvbnm") + [KeyCap(key: .delete, units: 1.5)],
            ]
        case .numbers:
            [
                Self.caps("1234567890"),
                Self.caps("-/:;()$&@\""),
                [KeyCap(key: .layer(.symbols), units: 1.5)] + Self.caps(".,?!'", units: 1.4)
                    + [KeyCap(key: .delete, units: 1.5)],
            ]
        case .symbols:
            [
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
        leading += extraKeys.map { KeyCap(key: .character($0)) }
        var trailing = [KeyCap(key: .newLine, units: 2)]
        if withDelete { trailing.insert(KeyCap(key: .delete, units: 1.5), at: 0) }
        let used = (leading + trailing).reduce(0) { $0 + $1.units }
        return leading + [KeyCap(key: .space, units: 10 - used)] + trailing
    }

    private static func caps(_ characters: String, units: Double = 1) -> [KeyCap] {
        characters.map { KeyCap(key: .character(String($0)), units: units) }
    }
}

/// Rows of keys, each key as wide as its share of the keyboard.
struct KeyRows: View {
    let model: TypingModel
    let rows: [[KeyCap]]

    var body: some View {
        GeometryReader { proxy in
            let unit = proxy.size.width / 10
            VStack(spacing: 0) {
                ForEach(rows.indices, id: \.self) { index in
                    HStack(spacing: 0) {
                        ForEach(rows[index], id: \.key) { cap in
                            KeyButton(cap: cap, model: model).frame(width: unit * cap.units)
                        }
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
        }
    }
}

private struct KeyButton: View {
    let cap: KeyCap
    let model: TypingModel

    var body: some View {
        if cap.key == .delete {
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
            Text(verbatim: model.layer == .letters && model.shift != .off ? text.uppercased() : text)
                .font(.system(size: 23))
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
        case .space:
            Text("keyboard.space").font(.system(size: 16))
        case .newLine:
            if let title = model.returnKey.title {
                Text(LocalizedStringKey(title)).font(.system(size: 16)).lineLimit(1).minimumScaleFactor(0.6)
            } else {
                Image(systemName: "return")
                    .font(.system(size: 18))
                    .accessibilityLabel(Text("keyboard.return"))
            }
        case .delete:
            EmptyView()
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

private struct KeyFace: ViewModifier {
    let pressed: Bool
    var prominent = false

    func body(content: Content) -> some View {
        content
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .foregroundStyle(prominent ? Color.white : Theme.textPrimary)
            .background(
                prominent ? Theme.accent.opacity(pressed ? 0.7 : 1) : pressed ? Theme.border : Theme.surface,
                in: RoundedRectangle(cornerRadius: 6, style: .continuous)
            )
            .padding(.horizontal, 3)
            .padding(.vertical, 5)
            .contentShape(Rectangle())
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
