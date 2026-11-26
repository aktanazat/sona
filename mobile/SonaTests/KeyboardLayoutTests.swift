import CoreGraphics
import Testing

/// Where the keys sit on the iPhone and the iPad.
@MainActor
struct KeyboardLayoutTests {
    private static func width(_ row: [KeyCap]) -> Double {
        row.reduce(0) { $0 + $1.units }
    }

    private static func model(wide: Bool, language: TypingLanguage = .english, layer: KeyLayer = .letters) -> TypingModel {
        let model = TypingModel()
        model.wide = wide
        model.language = language
        model.layer = layer
        return model
    }

    @Test(
        "every iPad row is as wide as the others, for plain, email and search fields",
        arguments: [TypingLanguage.english, .russian], [KeyLayer.letters, .numbers, .symbols]
    )
    func iPadRowsLineUp(language: TypingLanguage, layer: KeyLayer) {
        for numberRow in [false, true] {
            for (extraKeys, domain) in [([String](), false), (["@", "."], true), (["."], false)] {
                let model = Self.model(wide: true, language: language, layer: layer)
                model.numberRow = numberRow
                model.showsLanguageKey = true
                model.extraKeys = extraKeys
                model.showsDomainKey = domain
                let widths = (model.rows + [model.bottomRow(below: .keys)]).map { (Self.width($0) * 1000).rounded() / 1000 }
                #expect(Set(widths).count == 1, "\(language) \(layer) number row \(numberRow) \(extraKeys): \(widths)")
            }
        }
    }

    @Test("the iPad's letters have delete and return at the row ends, shift at both ends, and a key to hide the keyboard")
    func iPadKeysWhereTheSystemHasThem() {
        let model = Self.model(wide: true)
        let rows = model.rows + [model.bottomRow(below: .keys)]
        #expect(rows[0].last?.key == .delete)
        #expect(rows[1].last?.key == .newLine)
        #expect(rows[2].first?.key == .shift && rows[2].last?.key == .shift)
        #expect(rows[2].map(\.key).contains(.character(",")) && rows[2].map(\.key).contains(.character(".")))
        #expect(rows[3].last?.key == .dismissKeyboard)
        #expect(!rows[3].contains { $0.key == .newLine })
    }

    @Test("over a panel the iPad's bottom row brings return down, and delete over a draft")
    func iPadPanelRows() {
        let model = Self.model(wide: true)
        let tools = model.bottomRow(below: .tools).map(\.key)
        let draft = model.bottomRow(below: .draft).map(\.key)
        #expect(tools.contains(.newLine) && !tools.contains(.delete))
        #expect(draft.contains(.newLine) && draft.contains(.delete))
    }

    @Test("the iPhone's letter rows are as they were before the iPad layout")
    func iPhoneLettersUnchanged() {
        let model = Self.model(wide: false)
        let rows = model.rows + [model.bottomRow(below: .keys)]
        #expect(rows.map { $0.map(\.units) } == [
            Array(repeating: 1, count: 10),
            Array(repeating: 1, count: 9),
            [1.5] + Array(repeating: 1, count: 7) + [1.5],
            [1.25, 1.25, 5, 2.5],
        ])
        #expect(rows[2].first?.key == .shift && rows[2].last?.key == .delete)
        #expect(rows[3].map(\.key) == [.layer(.numbers), .nextKeyboard, .space, .newLine])
        #expect(!rows.joined().contains { $0.indent > 0 })
    }

    @Test("the iPhone's draft row keeps delete and return beside space")
    func iPhoneDraftRowUnchanged() {
        let model = Self.model(wide: false)
        #expect(model.bottomRow(below: .draft).map(\.key) == [.layer(.numbers), .nextKeyboard, .space, .delete, .newLine])
    }

    @Test("on a wide screen the iPad's keys reach across it while the iPhone's stay centred at their width")
    func gridWidth() {
        let size = CGSize(width: 1194, height: 344)
        let pad = Self.model(wide: true)
        let padGrid = KeyGrid(rows: pad.rows, size: size, maxWidth: pad.maxWidth)
        let phone = Self.model(wide: false)
        let phoneGrid = KeyGrid(rows: phone.rows, size: size, maxWidth: phone.maxWidth)
        #expect(padGrid.frames.map(\.frame.maxX).max()! > 1180)
        #expect(padGrid.frames.map(\.frame.minX).min()! < 14)
        let phoneLeft = phoneGrid.frames.map(\.frame.minX).min()!, phoneRight = phoneGrid.frames.map(\.frame.maxX).max()!
        #expect(abs(phoneRight - phoneLeft - KeyGrid.maxWidth) < 1)
        #expect(abs(phoneLeft - (size.width - KeyGrid.maxWidth) / 2) < 1)
    }

    @Test("the iPad's A sits half a key right of Q, so taps and swipes find it under the row above")
    func iPadIndentedRow() throws {
        let model = Self.model(wide: true)
        let grid = KeyGrid(rows: model.rows, size: CGSize(width: 834, height: 198), maxWidth: model.maxWidth)
        let centres = grid.letterCentres
        let q = try #require(centres["q"]), a = try #require(centres["a"])
        #expect(abs(a.x - q.x - grid.unit / 2) < 0.5)
        #expect(grid.key(at: CGPoint(x: 4, y: a.y))?.cap.key == .character("a"))
    }
}
