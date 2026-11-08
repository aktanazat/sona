import XCTest

/// Every color is generated from a seed at the contrast the person picks, so
/// these hold across the whole range the contrast control offers.
final class ThemeTests: XCTestCase {
    private let appearances = [("light", ThemeSeed.light(contrast:)), ("dark", ThemeSeed.dark(contrast:))]
    private let contrasts = Array(stride(
        from: ThemeStore.contrastRange.lowerBound, through: ThemeStore.contrastRange.upperBound, by: 5))

    func testTextReachesFourAndAHalfToOneOnEveryBackground() {
        // legible() solves some roles to exactly 4.5, which rounding can land a hair under.
        let floor = 4.5 - 1e-9
        for (appearance, seed) in appearances {
            for contrast in contrasts {
                let theme = seed(contrast)
                for text in [ThemeRole.ink, .inkSecondary, .inkTertiary, .accent, .live] {
                    for background in [ThemeRole.page, .inset, .surface] {
                        XCTAssertGreaterThanOrEqual(
                            ratio(theme.value(text), theme.value(background)), floor,
                            "\(text) on \(background), \(appearance), contrast \(contrast)")
                    }
                }
            }
        }
    }

    /// Secondary buttons, chips and key caps sit on cards. In dark they once
    /// sat 1.5 lightness above the card and all but disappeared.
    func testControlsStandApartFromTheCardsTheySitOn() {
        for (appearance, seed) in appearances {
            for contrast in contrasts {
                let theme = seed(contrast)
                XCTAssertGreaterThanOrEqual(
                    abs(theme.value(.control).l - theme.value(.surface).l), 3,
                    "\(appearance), contrast \(contrast)")
            }
        }
    }

    private func ratio(_ a: LCH, _ b: LCH) -> Double {
        let lighter = max(a.luminance, b.luminance)
        let darker = min(a.luminance, b.luminance)
        return (lighter + 0.05) / (darker + 0.05)
    }
}
