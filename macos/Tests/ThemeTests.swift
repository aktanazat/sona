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
                for text in [ThemeRole.ink, .inkSecondary, .inkTertiary, .accent, .live, .dueSoon] {
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

    /// The keyboard highlight once vanished into the task list's group
    /// headers, which drew the same hover wash. The bands sit on `inset`,
    /// which has to stay clearly the quieter of the two.
    func testHighlightedRowsStepFurtherFromThePageThanGroupBands() {
        for (appearance, seed) in appearances {
            for contrast in contrasts {
                let theme = seed(contrast)
                let band = abs(theme.value(.inset).l - theme.value(.page).l)
                XCTAssertGreaterThanOrEqual(hoverStep(in: theme), 1.5 * band, "\(appearance), contrast \(contrast)")
            }
        }
    }

    /// At one alpha the wash moved a light page 3.6 lightness and a dark one
    /// 4.7, so hovered and highlighted rows read faint in light.
    func testHoverWashStepsTheSameInLightAndDark() {
        for contrast in contrasts {
            let steps = appearances.map { hoverStep(in: $0.1(contrast)) }
            XCTAssertEqual(steps[0], steps[1], accuracy: 1, "contrast \(contrast)")
        }
    }

    private func hoverStep(in theme: ThemeSeed) -> Double {
        let page = theme.value(.page)
        return abs(lightness(of: theme.value(.wash), over: page) - page.l)
    }

    /// The window server blends gamma-encoded values, which is why black over
    /// a light page moves lightness less than white over a dark one.
    private func lightness(of overlay: LCH, over background: LCH) -> Double {
        let blended = encoded(background.luminance) * (1 - overlay.alpha) + encoded(overlay.luminance) * overlay.alpha
        return LCH.lightness(luminance: decoded(blended))
    }

    private func encoded(_ luminance: Double) -> Double {
        luminance <= 0.0031308 ? 12.92 * luminance : 1.055 * pow(luminance, 1 / 2.4) - 0.055
    }

    private func decoded(_ value: Double) -> Double {
        value <= 0.04045 ? value / 12.92 : pow((value + 0.055) / 1.055, 2.4)
    }

    private func ratio(_ a: LCH, _ b: LCH) -> Double {
        let lighter = max(a.luminance, b.luminance)
        let darker = min(a.luminance, b.luminance)
        return (lighter + 0.05) / (darker + 0.05)
    }
}
