import SwiftUI
import UIKit

enum KeyLayer: Equatable {
    case letters, numbers, symbols, emoji

    var title: String {
        switch self {
        case .letters: "ABC"
        case .numbers: "123"
        case .symbols: "#+="
        case .emoji: ""
        }
    }

    var label: LocalizedStringKey {
        switch self {
        case .letters: "keyboard.letters"
        case .numbers: "keyboard.numbers"
        case .symbols: "keyboard.symbols"
        case .emoji: "keyboard.emoji"
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
    /// ".com", held for the other endings.
    case domain
}

/// One key and its width, in key units: a row of ten letters is ten units wide.
struct KeyCap: Hashable {
    let key: Key
    var units = 1.0
}

/// A held key's choices above it, laid out one key wide each from `originX`.
struct AccentChoice: Equatable {
    let anchor: CGRect
    let options: [String]
    let originX: CGFloat
    var selected = 0
}

/// The enlarged letter shown above a key while a finger is on it.
struct KeyPreview: Equatable {
    let text: String
    let frame: CGRect
}

/// Where every key sits: keys share one unit width, each row is centred, and the block is
/// never wider than `maxWidth`, so a wide display gets centred keys instead of stretched ones.
struct KeyGrid {
    static let maxWidth: CGFloat = 620

    let unit: CGFloat
    let rowHeight: CGFloat
    let frames: [(cap: KeyCap, frame: CGRect)]

    init(rows: [[KeyCap]], size: CGSize) {
        let columns = max(10, rows.map { $0.reduce(0) { $0 + $1.units } }.max() ?? 10)
        let width = min(size.width, Self.maxWidth)
        let unit = width / columns
        let rowHeight = size.height / CGFloat(max(rows.count, 1))
        let origin = CGPoint(x: (size.width - width) / 2, y: 0)
        self.unit = unit
        self.rowHeight = rowHeight
        frames = rows.enumerated().flatMap { index, row in
            var x = origin.x + (width - row.reduce(0) { $0 + $1.units } * unit) / 2
            return row.map { cap in
                defer { x += cap.units * unit }
                return (cap, CGRect(x: x, y: rowHeight * CGFloat(index), width: cap.units * unit, height: rowHeight))
            }
        }
    }

    var letterCentres: [Character: CGPoint] {
        var centres: [Character: CGPoint] = [:]
        for (cap, frame) in frames {
            guard case .character(let text) = cap.key, let letter = text.first, letter.isLetter else { continue }
            centres[letter] = CGPoint(x: frame.midX, y: frame.midY)
        }
        return centres
    }

    /// The key under `point`, or the nearest key in that row when the point falls in a gap.
    func key(at point: CGPoint) -> (cap: KeyCap, frame: CGRect)? {
        if let hit = frames.first(where: { $0.frame.contains(point) }) { return hit }
        guard rowHeight > 0, !frames.isEmpty else { return nil }
        let rows = Int(((frames.map(\.frame.maxY).max() ?? 0) / rowHeight).rounded())
        let row = min(max(Int(point.y / rowHeight), 0), rows - 1)
        let top = CGFloat(row) * rowHeight
        return frames.filter { $0.frame.minY <= top + 1 && $0.frame.maxY >= top + rowHeight - 1 }
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
    static let domains = [".com", ".net", ".org", ".edu", ".io", ".co"]

    var layer = KeyLayer.letters
    var language = TypingLanguage.english
    var englishLayout = EnglishKeyboardLayout.qwerty
    var shift = Shift.off
    var suggestions: Suggestions?
    var hasTyped = false
    var returnKey = ReturnKey(.default)
    var extraKeys: [String] = []
    var showsDomainKey = false
    var showsNextKeyboard = true
    var showsLanguageKey = false
    var numberRow = false
    var pressed: Set<Key> = []
    var preview: KeyPreview?
    /// The space bar is moving the cursor; the keys go blank like the system keyboard's.
    var trackpad = false
    var trail: [CGPoint] = []
    var accentChoice: AccentChoice?
    /// The emoji search being typed, while the letter keys type into it.
    var emojiQuery: String?
    @ObservationIgnored var press: (Key) -> Void = { _ in }
    @ObservationIgnored var choose: (String) -> Void = { _ in }
    @ObservationIgnored var swipe: ([CGPoint], KeyGrid) -> Void = { _, _ in }
    /// Moves the cursor by characters; returns how far it actually moved.
    @ObservationIgnored var moveCursor: (Int) -> Int = { _ in 0 }
    /// Deletes the word before the cursor and returns it, for a slide left on delete.
    @ObservationIgnored var deleteWord: () -> String? = { nil }
    /// Puts back text a slide on delete took, as the slide comes back.
    @ObservationIgnored var restore: (String) -> Void = { _ in }
    /// The click and tap a key gives when a finger lands on it.
    @ObservationIgnored var touched: (Key) -> Void = { _ in }

    /// Rows in the letter layout, bottom row included; every layer shares their height so
    /// switching layers never moves the keyboard.
    var rowCount: Int { numberRow ? 5 : 4 }
    var showsNumberRow: Bool { numberRow && emojiQuery == nil }
    var letterGrid: KeyboardLetterGrid { language.letterGrid(englishLayout: englishLayout) }

    /// The rows above the bottom row.
    var rows: [[KeyCap]] {
        switch layer {
        case .letters:
            let grid = letterGrid
            let letters = grid.rows
            let side = Double(grid.columns - letters[2].count) / 2
            let digits = showsNumberRow ? [Self.caps("1234567890")] : []
            return digits + [
                Self.caps(letters[0]),
                Self.caps(letters[1]),
                [KeyCap(key: .shift, units: side)] + Self.caps(letters[2]) + [KeyCap(key: .delete, units: side)],
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
        case .emoji:
            return []
        }
    }

    /// The bottom row: layer switch, emoji, the field's extra keys, space and return, as wide
    /// as the rows above it.
    func bottomRow(withDelete: Bool) -> [KeyCap] {
        var leading = [KeyCap(key: .layer(layer == .letters ? .numbers : .letters), units: 1.25)]
        if showsNextKeyboard { leading.append(KeyCap(key: .nextKeyboard, units: 1.25)) }
        if showsLanguageKey, withDelete || extraKeys.isEmpty { leading.append(KeyCap(key: .language, units: 1.25)) }
        leading.append(KeyCap(key: .layer(.emoji), units: 1.25))
        if !withDelete {
            leading += extraKeys.map { KeyCap(key: .character($0)) }
            if showsDomainKey { leading.append(KeyCap(key: .domain, units: 1.5)) }
        }
        var trailing = [KeyCap(key: .newLine, units: 2.5)]
        if withDelete { trailing.insert(KeyCap(key: .delete, units: 1.5), at: 0) }
        let used = (leading + trailing).reduce(0) { $0 + $1.units }
        let width = rows.map { $0.reduce(0) { $0 + $1.units } }.max() ?? 10
        return leading + [KeyCap(key: .space, units: max(max(width, 10) - used, 2))] + trailing
    }

    /// What a character key types with the current shift.
    func face(of text: String) -> String {
        guard layer == .letters, shift != .off else { return text }
        return text.uppercased(with: Locale(identifier: language.rawValue))
    }

    private static func caps(_ characters: String, units: Double = 1) -> [KeyCap] {
        characters.map { KeyCap(key: .character(String($0)), units: units) }
    }
}

/// Rows of key faces laid out by `KeyGrid`. A `KeySurface` over them does the touching.
struct KeyRows: View {
    let model: TypingModel
    let rows: [[KeyCap]]

    var body: some View {
        GeometryReader { proxy in
            let grid = KeyGrid(rows: rows, size: proxy.size)
            ForEach(Array(grid.frames.enumerated()), id: \.offset) { _, key in
                KeyButton(cap: key.cap, model: model)
                    .frame(width: key.frame.width, height: key.frame.height)
                    .position(x: key.frame.midX, y: key.frame.midY)
            }
        }
    }
}

private struct KeyButton: View {
    let cap: KeyCap
    let model: TypingModel

    var body: some View {
        label
            .opacity(model.trackpad ? 0 : 1)
            .modifier(KeyFace(
                pressed: model.pressed.contains(cap.key) && !isCharacter,
                prominent: cap.key == .newLine && model.returnKey.prominent,
                trackpad: model.trackpad,
                inset: inset
            ))
            .accessibilityHidden(true)
    }

    private var isCharacter: Bool {
        switch cap.key {
        case .character: true
        default: false
        }
    }

    /// Shift and delete beside the third letter row are drawn narrower than they reach, as on
    /// the system keyboard: the gap to the letters still types them.
    private var inset: EdgeInsets {
        guard cap.units == 1.5, model.layer != .emoji else { return EdgeInsets() }
        let side = KeyFace.gap / 2 + 9
        switch cap.key {
        case .shift, .layer(.symbols), .layer(.numbers) where model.layer != .letters:
            return EdgeInsets(top: 0, leading: 0, bottom: 0, trailing: side)
        case .delete:
            return EdgeInsets(top: 0, leading: side, bottom: 0, trailing: 0)
        default:
            return EdgeInsets()
        }
    }

    @ViewBuilder private var label: some View {
        switch cap.key {
        case let .character(text):
            Text(verbatim: model.face(of: text))
                .font(.system(size: model.layer == .letters ? 25 : 24))
        case .shift:
            Image(systemName: model.shift.symbol)
                .font(.system(size: 19, weight: .medium))
        case let .layer(layer):
            if layer == .emoji {
                Image(systemName: "face.smiling").font(.system(size: 21))
            } else {
                Text(verbatim: layer.title)
                    .font(.system(size: 17))
            }
        case .nextKeyboard:
            Image(systemName: "globe").font(.system(size: 19))
        case .language:
            Text(verbatim: model.language.code).font(.system(size: 16))
        case .space:
            Color.clear
        case .newLine:
            if let title = model.returnKey.title {
                Text(LocalizedStringKey(title)).font(.system(size: 17)).lineLimit(1).minimumScaleFactor(0.6)
                    .padding(.horizontal, 4)
            } else {
                Image(systemName: "return").font(.system(size: 20))
            }
        case .delete:
            Image(systemName: "delete.left").font(.system(size: 19))
        case .domain:
            Text(verbatim: TypingModel.domains[0]).font(.system(size: 17))
        }
    }
}

/// Keys as the system keyboard draws them on iOS 26 and later: solid rounded keys on the
/// keyboard's own glass backdrop, white in light and grey in dark, darker while pressed.
/// Glass is kept for the controls that float over the keys, as Apple's guidance has it.
struct KeyFace: ViewModifier {
    static let gap: CGFloat = 6
    static let rowGap: CGFloat = 11
    static let radius: CGFloat = 8.5
    static let fill = Color(UIColor { $0.userInterfaceStyle == .dark ? UIColor(white: 0.27, alpha: 1) : .white })
    static let pressedFill = Color(UIColor {
        $0.userInterfaceStyle == .dark
            ? UIColor(white: 0.44, alpha: 1)
            : UIColor(red: 0.70, green: 0.72, blue: 0.76, alpha: 1)
    })

    let pressed: Bool
    var prominent = false
    var trackpad = false
    var inset = EdgeInsets()

    func body(content: Content) -> some View {
        content
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .foregroundStyle(prominent ? Color.white : Theme.textPrimary)
            .background(background, in: RoundedRectangle(cornerRadius: Self.radius, style: .continuous))
            .padding(inset)
            .padding(.horizontal, Self.gap / 2)
            .padding(.vertical, Self.rowGap / 2)
    }

    private var background: Color {
        if trackpad { return Self.fill.opacity(0.35) }
        if prominent { return pressed ? Theme.accent.opacity(0.7) : Theme.accent }
        return pressed ? Self.pressedFill : Self.fill
    }
}

/// The letter under a finger, enlarged above its key as the system keyboard shows it: the
/// key's own shape grows a wider head with the letter in it.
struct KeyPreviewBalloon: View {
    let preview: KeyPreview
    let bounds: CGSize

    var body: some View {
        let key = preview.frame.insetBy(dx: KeyFace.gap / 2, dy: KeyFace.rowGap / 2)
        let headWidth = key.width + 22
        let headHeight = key.height + 10
        let headX = min(max(key.midX - headWidth / 2, 1), bounds.width - headWidth - 1)
        let top = key.minY - headHeight - 8
        let shape = KeyBalloon(key: key, head: CGRect(x: headX, y: top, width: headWidth, height: headHeight))
        ZStack(alignment: .topLeading) {
            shape.fill(KeyFace.fill)
                .shadow(color: .black.opacity(0.18), radius: 3, y: 1)
                .shadow(color: .black.opacity(0.08), radius: 10, y: 4)
            Text(verbatim: preview.text)
                .font(.system(size: 36))
                .foregroundStyle(Theme.textPrimary)
                .frame(width: headWidth, height: headHeight)
                .offset(x: headX, y: top)
        }
        .frame(width: bounds.width, height: bounds.height, alignment: .topLeading)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// A key with a wider rounded head above it, joined by smooth shoulders.
private struct KeyBalloon: Shape {
    let key: CGRect
    let head: CGRect

    func path(in rect: CGRect) -> Path {
        let radius = KeyFace.radius
        let neck = key.minY + 6
        var path = Path()
        path.move(to: CGPoint(x: key.minX, y: key.maxY - radius))
        path.addLine(to: CGPoint(x: key.minX, y: neck))
        path.addCurve(
            to: CGPoint(x: head.minX, y: head.maxY - radius),
            control1: CGPoint(x: key.minX, y: neck - 8),
            control2: CGPoint(x: head.minX, y: head.maxY - radius + 6)
        )
        path.addLine(to: CGPoint(x: head.minX, y: head.minY + radius + 2))
        path.addQuadCurve(to: CGPoint(x: head.minX + radius + 2, y: head.minY), control: CGPoint(x: head.minX, y: head.minY))
        path.addLine(to: CGPoint(x: head.maxX - radius - 2, y: head.minY))
        path.addQuadCurve(to: CGPoint(x: head.maxX, y: head.minY + radius + 2), control: CGPoint(x: head.maxX, y: head.minY))
        path.addLine(to: CGPoint(x: head.maxX, y: head.maxY - radius))
        path.addCurve(
            to: CGPoint(x: key.maxX, y: neck),
            control1: CGPoint(x: head.maxX, y: head.maxY - radius + 6),
            control2: CGPoint(x: key.maxX, y: neck - 8)
        )
        path.addLine(to: CGPoint(x: key.maxX, y: key.maxY - radius))
        path.addQuadCurve(to: CGPoint(x: key.maxX - radius, y: key.maxY), control: CGPoint(x: key.maxX, y: key.maxY))
        path.addLine(to: CGPoint(x: key.minX + radius, y: key.maxY))
        path.addQuadCurve(to: CGPoint(x: key.minX, y: key.maxY - radius), control: CGPoint(x: key.minX, y: key.maxY))
        path.closeSubpath()
        return path
    }
}

/// A held key's choices above it; the finger slides to one and lets go.
struct AccentPopup: View {
    let choice: AccentChoice

    var body: some View {
        let padding: CGFloat = 4
        let radius = KeyFace.radius + 3
        HStack(spacing: 0) {
            ForEach(Array(choice.options.enumerated()), id: \.offset) { index, option in
                Text(verbatim: option)
                    .font(.system(size: option.count > 1 ? 17 : 25))
                    .frame(width: choice.anchor.width, height: choice.anchor.height - KeyFace.rowGap)
                    .foregroundStyle(index == choice.selected ? Color.white : Theme.textPrimary)
                    .background(
                        index == choice.selected ? Theme.accent : Color.clear,
                        in: RoundedRectangle(cornerRadius: radius - padding, style: .continuous)
                    )
            }
        }
        .padding(padding)
        .background(KeyFace.fill, in: RoundedRectangle(cornerRadius: radius, style: .continuous))
        .shadow(color: .black.opacity(0.18), radius: 3, y: 1)
        .shadow(color: .black.opacity(0.1), radius: 12, y: 4)
        .offset(x: choice.originX - 4, y: choice.anchor.minY - choice.anchor.height + 2)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// Three words in equal slots, as the system keyboard's bar has them. While a word is typed:
/// the word as typed in quotes, the correction space would apply, and the others; an emoji
/// that stands for the word takes the last slot. After a space: the likely next words. At a
/// word corrected earlier: the word as it was typed, to put it back.
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
                    Rectangle().fill(Theme.textPrimary.opacity(0.16)).frame(width: 1, height: 24)
                }
                Button { choose(candidate.word) } label: {
                    Text(verbatim: candidate.quoted ? "\u{201C}\(candidate.word)\u{201D}" : candidate.word)
                        .font(.system(size: 17, weight: candidate.highlighted ? .semibold : .regular))
                        .foregroundStyle(candidate.quoted ? Theme.textSecondary : Theme.textPrimary)
                        .lineLimit(1)
                        .minimumScaleFactor(0.6)
                        .padding(.horizontal, 4)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(
                            candidate.highlighted ? KeyFace.fill : Color.clear,
                            in: RoundedRectangle(cornerRadius: KeyFace.radius, style: .continuous)
                        )
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("keyboard-suggestion-\(index)")
            }
        }
        .padding(.vertical, 4)
        .animation(nil, value: suggestions)
    }

    private var candidates: [Candidate] {
        if let restore = suggestions.restore {
            return [Candidate(word: restore, quoted: true), Candidate(word: suggestions.typed, highlighted: true)]
        }
        let leading = suggestions.correction.map {
            [Candidate(word: suggestions.typed, quoted: true), Candidate(word: $0, highlighted: true)]
        } ?? []
        let others = suggestions.others.filter { $0 != suggestions.correction }.map { Candidate(word: $0) }
        let words = Array((leading + others).prefix(suggestions.emoji == nil ? 3 : 2))
        return words + (suggestions.emoji.map { [Candidate(word: $0)] } ?? [])
    }
}
