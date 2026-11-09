import AppKit
import Observation
import SwiftUI
import simd

/// Every color Sona draws comes from three inputs, a base, an accent and a
/// contrast, the way Linear builds its themes: steps in LCH lightness and
/// chroma rather than alpha, drawn in Display P3. Each role resolves per
/// appearance, and Increase Contrast picks the same seeds at full contrast.
enum Theme {
    static var rail: Color { color(.rail) }
    static var page: Color { color(.page) }
    static var inset: Color { color(.inset) }
    static var surface: Color { color(.surface) }
    static var elevated: Color { color(.elevated) }

    static var control: Color { color(.control) }
    static var controlHover: Color { color(.controlHover) }
    static var controlPressed: Color { color(.controlPressed) }
    static var selection: Color { color(.selection) }
    static var wash: Color { color(.wash) }
    /// A selected row on glass or on any surface: twice the hover wash.
    static var washSelected: Color { color(.washSelected) }
    /// Empty bars, progress tracks and loading placeholders.
    static var track: Color { color(.track) }
    /// Dims the window behind the command palette.
    static var scrim: Color { color(.scrim) }

    static var border: Color { color(.border) }
    static var hairline: Color { color(.hairline) }

    static var ink: Color { color(.ink) }
    static var inkSecondary: Color { color(.inkSecondary) }
    static var inkTertiary: Color { color(.inkTertiary) }
    static var inkDisabled: Color { color(.inkDisabled) }

    static var accent: Color { color(.accent) }
    static var accentFill: Color { color(.accentFill) }
    static var accentSoft: Color { color(.accentSoft) }
    static var onAccent: Color { color(.onAccent) }

    static var invert: Color { color(.invert) }
    static var onInvert: Color { color(.onInvert) }

    static var live: Color { color(.live) }
    /// Bookmark marks in a meeting: an open question, a to-do.
    static var markQuestion: Color { color(.markQuestion) }
    static var markTodo: Color { color(.markTodo) }

    static var statusTriage: Color { color(.statusTriage) }
    static var statusBacklog: Color { color(.statusBacklog) }
    static var statusTodo: Color { color(.statusTodo) }
    static var statusInProgress: Color { color(.statusInProgress) }
    static var statusDone: Color { color(.statusDone) }
    static var statusCanceled: Color { color(.statusCanceled) }
    static var statusDuplicate: Color { color(.statusDuplicate) }
    static var priorityUrgent: Color { color(.priorityUrgent) }

    /// The recording window is dark in either appearance, as a heads-up
    /// display over whatever is on screen.
    static var hud: Color { color(.hud) }
    static var hudInk: Color { color(.hudInk) }
    static var hudInkSecondary: Color { color(.hudInkSecondary) }
    static var hudLive: Color { color(.hudLive) }
    static var hudControl: Color { color(.hudControl) }
    static var hudBorder: Color { color(.hudBorder) }

    /// A label's color. Every hue gets the same lightness and chroma, so no
    /// label reads louder than another.
    static func label(hue: Double) -> Color {
        ThemeStore.shared.label(hue: hue)
    }

    /// A monogram's ground, the same in either appearance: dark enough that
    /// white initials stay above 4.5:1.
    static func monogram(hue: Double, chroma: Double) -> Color {
        Color(nsColor: LCH(l: 41, c: chroma, h: hue).nsColor)
    }

    /// Bars, ticks and carets: data marks, a step below the control radii.
    static let radiusMark: CGFloat = 2
    static let radiusKey: CGFloat = 4
    static let radiusRow: CGFloat = 6
    static let radiusControl: CGFloat = 8
    static let radiusCard: CGFloat = 12
    static let radiusDialog: CGFloat = 16

    static let sidebarWidth: CGFloat = 232
    static let sidebarRow: CGFloat = 28
    static let headerHeight: CGFloat = 44
    static let listRow: CGFloat = 36
    static let margin: CGFloat = 28
    static let contentMax: CGFloat = 880

    /// The one curve panels move on, in AppKit and SwiftUI alike: 0.25 s,
    /// quick off the screen's edge and settling without a bounce. With
    /// motion reduced, a panel fades in place for `fade` instead.
    enum Slide {
        static let duration: TimeInterval = 0.25
        static let fade: TimeInterval = 0.15
        private static let points: (Float, Float, Float, Float) = (0.2, 0.8, 0.2, 1)

        static var animation: Animation {
            .timingCurve(Double(points.0), Double(points.1), Double(points.2), Double(points.3), duration: duration)
        }

        static var timing: CAMediaTimingFunction {
            CAMediaTimingFunction(controlPoints: points.0, points.1, points.2, points.3)
        }

        static func animation(reduced: Bool) -> Animation {
            reduced ? .easeInOut(duration: fade) : animation
        }
    }

    private static func color(_ role: ThemeRole) -> Color {
        ThemeStore.shared.colors[role.rawValue]
    }
}

enum ThemeRole: Int, CaseIterable {
    case rail, page, inset, surface, elevated
    case control, controlHover, controlPressed, selection, wash, washSelected, track, scrim
    case border, hairline
    case ink, inkSecondary, inkTertiary, inkDisabled
    case accent, accentFill, accentSoft, onAccent
    case invert, onInvert
    case live, markQuestion, markTodo
    case statusTriage, statusBacklog, statusTodo, statusInProgress, statusDone, statusCanceled, statusDuplicate
    case priorityUrgent
    case hud, hudInk, hudInkSecondary, hudLive, hudControl, hudBorder
}

/// The contrast every color is generated at, kept across launches.
@Observable
final class ThemeStore {
    static let shared = ThemeStore()

    /// Linear's slider runs 30 to 100 and ships at 33; Sona starts a
    /// little crisper.
    static let contrastRange: ClosedRange<Double> = 30...100
    static let defaultContrast: Double = 40

    var contrast: Double {
        didSet {
            UserDefaults.standard.set(contrast, forKey: Self.contrastKey)
            labels = [:]
            colors = Self.colors(contrast: contrast)
        }
    }

    private(set) var colors: [Color]
    @ObservationIgnored private var labels: [Double: Color] = [:]

    private init() {
        let stored = UserDefaults.standard.double(forKey: Self.contrastKey)
        let contrast = Self.contrastRange.contains(stored) ? stored : Self.defaultContrast
        self.contrast = contrast
        colors = Self.colors(contrast: contrast)
    }

    /// Reading `contrast` first ties the caller to it, so a view that drew
    /// a cached label draws again when the contrast moves.
    func label(hue: Double) -> Color {
        let contrast = contrast
        if let cached = labels[hue] { return cached }
        let color = Self.dynamic(Self.seeds(contrast: contrast).map { $0.label(hue: hue) })
        labels[hue] = color
        return color
    }

    private static let contrastKey = "themeContrast"

    private static func seeds(contrast: Double) -> Variants<ThemeSeed> {
        Variants(
            light: .light(contrast: contrast),
            dark: .dark(contrast: contrast),
            lightIncreased: .light(contrast: contrastRange.upperBound),
            darkIncreased: .dark(contrast: contrastRange.upperBound)
        )
    }

    private static func colors(contrast: Double) -> [Color] {
        let seeds = seeds(contrast: contrast)
        return ThemeRole.allCases.map { role in dynamic(seeds.map { $0.value(role) }) }
    }

    private static func dynamic(_ variants: Variants<LCH>) -> Color {
        let resolved = variants.map(\.nsColor)
        return Color(
            nsColor: NSColor(name: nil) { appearance in
                switch appearance.bestMatch(from: appearances) {
                case .darkAqua: resolved.dark
                case .accessibilityHighContrastAqua: resolved.lightIncreased
                case .accessibilityHighContrastDarkAqua: resolved.darkIncreased
                default: resolved.light
                }
            }
        )
    }

    private static let appearances: [NSAppearance.Name] = [
        .aqua, .darkAqua, .accessibilityHighContrastAqua, .accessibilityHighContrastDarkAqua,
    ]
}

private struct Variants<Value> {
    let light: Value
    let dark: Value
    let lightIncreased: Value
    let darkIncreased: Value

    func map<T>(_ transform: (Value) -> T) -> Variants<T> {
        Variants<T>(
            light: transform(light),
            dark: transform(dark),
            lightIncreased: transform(lightIncreased),
            darkIncreased: transform(darkIncreased)
        )
    }
}

/// Linear's three theme inputs. Its own dark theme is base (4.8, 0.7, 272)
/// at contrast 33, and the steps below were measured from the colors it
/// ships at that contrast. Dark controls are the exception: Linear's sit on
/// its page, Sona's mostly sit on cards, where Linear's step left a button
/// 1.5 lightness above the card.
struct ThemeSeed {
    let base: LCH
    let accent: LCH
    let contrast: Double

    static func light(contrast: Double) -> ThemeSeed {
        ThemeSeed(base: LCH(l: 100, c: 0, h: 272), accent: sonaBlue, contrast: contrast)
    }

    static func dark(contrast: Double) -> ThemeSeed {
        ThemeSeed(base: LCH(l: 4.8, c: 0.7, h: 272), accent: sonaBlue, contrast: contrast)
    }

    private static let sonaBlue = LCH(l: 62, c: 64, h: 276)

    func value(_ role: ThemeRole) -> LCH {
        switch role {
        case .rail: neutral(isDark ? -2.3 : 5.2, chroma: isDark ? -0.2 : 0)
        case .page: neutral(isDark ? 0 : 2.8)
        case .inset: neutral(isDark ? 2 : 4.4, chroma: 0.2)
        case .surface: neutral(isDark ? 4.4 : 0, chroma: 0.2)
        case .elevated: neutral(isDark ? 6.4 : 0, chroma: 0.4)
        case .control: neutral(isDark ? 8 : 7, chroma: isDark ? 1.5 : 0.6)
        case .controlHover: neutral(isDark ? 10.5 : 9, chroma: isDark ? 1.4 : 0.8)
        case .controlPressed: neutral(isDark ? 13 : 11, chroma: isDark ? 2 : 1)
        case .selection: tinted(7, chroma: isDark ? 18 : 10)
        case .wash: LCH(l: isDark ? 100 : 0, c: 0, h: 0, alpha: washAlpha)
        case .washSelected: LCH(l: isDark ? 100 : 0, c: 0, h: 0, alpha: 2 * washAlpha)
        case .track: neutral(isDark ? 11.4 : 11, chroma: isDark ? 2 : 1)
        case .scrim: LCH(l: 0, c: 0, h: 0, alpha: 0.18)
        case .border: neutral(isDark ? 9.8 : 10.5, chroma: isDark ? 3 : 0.8)
        case .hairline: neutral(isDark ? 8 : 8.5, chroma: isDark ? 2.3 : 0.5)
        case .ink: text(isDark ? (97.5, 100) : (17, 0), chroma: base.c / 2, ratio: 15)
        case .inkSecondary: text(isDark ? (85.4, 96) : (27.3, 8), chroma: base.c + 4.3, ratio: 9)
        case .inkTertiary: text(isDark ? (59.2, 82) : (46.8, 26), chroma: base.c + 4.3, ratio: 4.5)
        case .inkDisabled: text(isDark ? (43, 66) : (55.5, 40), chroma: base.c + 3.3, ratio: 3)
        case .accent: legible(accent, ratio: 4.5, on: [.page, .inset, .surface, .selection, .accentSoft])
        case .accentFill: LCH(l: isDark ? 48 : 48 - 8 * textReach, c: accent.c + 8, h: accent.h)
        case .accentSoft: tinted(isDark ? 12 : 5.5, chroma: isDark ? 24 : 12)
        case .onAccent: LCH(l: 100, c: 0, h: 0)
        case .invert: value(.ink)
        case .onInvert: value(isDark ? .page : .surface)
        case .live: legible(LCH(l: isDark ? 64 : 49, c: isDark ? 66 : 74, h: 33), ratio: 4.5)
        case .markQuestion: legible(LCH(l: isDark ? 68 : 58, c: 70, h: 52), ratio: 3)
        case .markTodo: legible(LCH(l: isDark ? 71 : 50, c: 48, h: 152), ratio: 3)
        case .statusTriage: legible(LCH(l: isDark ? 68 : 60, c: 72, h: 50), ratio: 3)
        case .statusBacklog: value(.inkTertiary)
        case .statusTodo: value(.inkSecondary)
        case .statusInProgress: legible(LCH(l: isDark ? 80 : 72, c: 80, h: 84), ratio: 3)
        case .statusDone: value(.accent)
        case .statusCanceled, .statusDuplicate: value(.inkDisabled)
        case .priorityUrgent: legible(LCH(l: isDark ? 64 : 55, c: 72, h: 38), ratio: 3)
        case .hud: hudSeed.value(.elevated)
        case .hudInk: hudSeed.value(.ink)
        case .hudInkSecondary: hudSeed.value(.inkTertiary)
        case .hudLive: hudSeed.value(.live)
        case .hudControl, .hudBorder: LCH(l: 100, c: 0, h: 0, alpha: 0.12)
        }
    }

    /// Text-safe at 4.5:1, so a label can color a speaker's name as well as
    /// a dot.
    func label(hue: Double) -> LCH {
        legible(LCH(l: isDark ? 66 + 10 * textReach : 52 - 10 * textReach, c: isDark ? 44 : 48, h: hue), ratio: 4.5)
    }

    private var isDark: Bool { base.l < 50 }

    /// How far each step reaches: 1 at Linear's contrast 33, 3 at 100.
    private var reach: Double { contrast / 33 }

    /// Linear's hover is 3% white on its dark page. Black over a light page
    /// moves lightness less than white over a dark one, so light takes 0.045
    /// for the same step: 4.7 lightness in either appearance at contrast 40.
    private var washAlpha: Double { (isDark ? 0.035 : 0.045) * reach }

    /// How far text has moved from Linear's tiers toward black and white.
    private var textReach: Double { min(max((contrast - 33) / 67, 0), 1) }

    private var hudSeed: ThemeSeed { isDark ? self : .dark(contrast: contrast) }

    private func neutral(_ step: Double, chroma: Double = 0) -> LCH {
        LCH(l: lightness(step * reach), c: max(base.c + chroma, 0), h: base.h)
    }

    /// Accent tints grow half as fast as the neutral steps, so accent text
    /// on them keeps its color at high contrast.
    private func tinted(_ step: Double, chroma: Double) -> LCH {
        LCH(l: lightness(step * (1 + reach) / 2), c: chroma, h: accent.h)
    }

    private func lightness(_ step: Double) -> Double {
        min(max(base.l + (isDark ? step : -step), 0), 100)
    }

    private func text(_ tiers: (at33: Double, at100: Double), chroma: Double, ratio: Double) -> LCH {
        legible(
            LCH(l: tiers.at33 + (tiers.at100 - tiers.at33) * textReach, c: chroma, h: base.h),
            ratio: ratio)
    }

    /// Moves a color away from the backgrounds it sits on, only as far as it
    /// takes to reach a WCAG contrast ratio against each of them.
    private func legible(_ color: LCH, ratio: Double, on roles: [ThemeRole] = [.page, .inset, .surface]) -> LCH {
        let backgrounds = roles.map { value($0).luminance }
        let lightness = if isDark {
            max(color.l, LCH.lightness(luminance: ratio * (backgrounds.reduce(0, max) + 0.05) - 0.05))
        } else {
            min(color.l, LCH.lightness(luminance: (backgrounds.reduce(1, min) + 0.05) / ratio - 0.05))
        }
        return LCH(l: lightness, c: color.c, h: color.h, alpha: color.alpha)
    }
}

/// A color in CIE LCH, D50, as CSS Color 4 defines it.
struct LCH {
    let l: Double
    let c: Double
    let h: Double
    var alpha: Double = 1

    /// Relative luminance. In Lab it follows from lightness alone, so every
    /// hue at one lightness contrasts the same against a background.
    var luminance: Double {
        l > 8 ? pow((l + 16) / 116, 3) : l * 27 / 24389
    }

    static func lightness(luminance: Double) -> Double {
        let y = min(max(luminance, 0), 1)
        return y > 216.0 / 24389 ? 116 * cbrt(y) - 16 : y * 24389 / 27
    }

    /// The color in Display P3. Outside that gamut, chroma comes down by
    /// bisection while lightness and hue hold.
    var nsColor: NSColor {
        var chroma = c
        if !Self.fits(Self.linearP3(l: l, c: c, h: h)) {
            var low = 0.0
            var high = c
            for _ in 0..<24 {
                let mid = (low + high) / 2
                if Self.fits(Self.linearP3(l: l, c: mid, h: h)) { low = mid } else { high = mid }
            }
            chroma = low
        }
        let rgb = Self.linearP3(l: l, c: chroma, h: h)
        return NSColor(
            displayP3Red: Self.encode(rgb.x), green: Self.encode(rgb.y), blue: Self.encode(rgb.z),
            alpha: CGFloat(alpha))
    }

    private static func linearP3(l: Double, c: Double, h: Double) -> SIMD3<Double> {
        let angle = h * .pi / 180
        let fy = (l + 16) / 116
        let fx = fy + c * cos(angle) / 500
        let fz = fy - c * sin(angle) / 200
        let xyz = SIMD3(
            uncompand(fx) * whiteD50.x,
            l > 8 ? fy * fy * fy : l * 27 / 24389,
            uncompand(fz) * whiteD50.z)
        return d50ToLinearP3 * xyz
    }

    private static func uncompand(_ f: Double) -> Double {
        let cube = f * f * f
        return cube > 216.0 / 24389 ? cube : (116 * f - 16) * 27 / 24389
    }

    private static func fits(_ rgb: SIMD3<Double>) -> Bool {
        rgb.min() >= -0.0001 && rgb.max() <= 1.0001
    }

    private static func encode(_ linear: Double) -> CGFloat {
        let v = min(max(linear, 0), 1)
        return CGFloat(v <= 0.0031308 ? 12.92 * v : 1.055 * pow(v, 1 / 2.4) - 0.055)
    }

    private static let whiteD50 = SIMD3(0.3457 / 0.3585, 1, (1 - 0.3457 - 0.3585) / 0.3585)

    /// CSS Color 4's XYZ to linear Display P3 matrix after its Bradford D50
    /// to D65 adaptation.
    private static let d50ToLinearP3 = simd_double3x3(rows: [
        SIMD3(446124.0 / 178915, -333277.0 / 357830, -72051.0 / 178915),
        SIMD3(-14852.0 / 17905, 63121.0 / 35810, 423.0 / 17905),
        SIMD3(11844.0 / 330415, -50337.0 / 660830, 316169.0 / 330415),
    ]) * simd_double3x3(rows: [
        SIMD3(0.955473421488075, -0.02309845494876471, 0.06325924320057072),
        SIMD3(-0.0283697093338637, 1.0099953980813041, 0.021041441191917323),
        SIMD3(0.012314014864481998, -0.020507649298898964, 1.330365926242124),
    ])
}

/// How things move. Under Reduce Motion, movement becomes an opacity change
/// at the same duration, and settling springs go away.
enum Motion {
    static let hoverOut: Animation = .easeOut(duration: 0.15)
    static let quick: Animation = .timingCurve(0.215, 0.61, 0.355, 1, duration: 0.1)
    static let regular: Animation = .timingCurve(0.165, 0.84, 0.44, 1, duration: 0.25)

    /// How lists and charts settle when what they show changes.
    static func settle(reduced: Bool) -> Animation? {
        reduced ? nil : .spring(duration: 0.35, bounce: 0.1)
    }
}

/// Linear's three shadows. Dark mode needs deeper ones, since a shadow has
/// less light to take away.
enum Elevation {
    case low, medium, high

    fileprivate func shadow(dark: Bool) -> (opacity: Double, radius: CGFloat, y: CGFloat) {
        switch (self, dark) {
        case (.low, false): (0.09, 2, 1)
        case (.medium, false): (0.09, 6, 3)
        case (.high, false): (0.12, 12, 7)
        case (.low, true): (0.1, 2, 2)
        case (.medium, true): (0.2, 12, 4)
        case (.high, true): (0.35, 16, 7)
        }
    }
}

private struct ElevationShadow: ViewModifier {
    let level: Elevation
    @Environment(\.colorScheme) private var colorScheme

    func body(content: Content) -> some View {
        let shadow = level.shadow(dark: colorScheme == .dark)
        content.shadow(color: .black.opacity(shadow.opacity), radius: shadow.radius, y: shadow.y)
    }
}

/// The spacing scale, on a 4-point grid.
enum Space {
    static let xxs: CGFloat = 2
    static let xs: CGFloat = 4
    static let s: CGFloat = 8
    static let m: CGFloat = 12
    static let l: CGFloat = 16
}

enum TypeScale {
    static let title: Font = .system(size: 15, weight: .semibold)
    static let hero: Font = .system(size: 26, weight: .semibold)
    static let stat: Font = .system(size: 18, weight: .semibold)
    static let headline: Font = .system(size: 14, weight: .semibold)
    static let list: Font = .system(size: 13)
    static let listTitle: Font = .system(size: 13, weight: .medium)
    static let meta: Font = .system(size: 12).monospacedDigit()
    static func body(_ size: CGFloat = 14) -> Font { .system(size: size, weight: .regular) }
    static func label(_ size: CGFloat = 14) -> Font { .system(size: size, weight: .regular) }
    static func mono(_ size: CGFloat = 13) -> Font { .system(size: size, weight: .regular, design: .monospaced) }
}

extension View {
    func titleText() -> some View {
        font(TypeScale.title).foregroundStyle(Theme.ink)
    }

    func heroText() -> some View {
        font(TypeScale.hero).foregroundStyle(Theme.ink).tracking(-0.3)
    }

    func headlineText() -> some View {
        font(TypeScale.headline).foregroundStyle(Theme.ink)
    }

    func bodyText(_ size: CGFloat = 14, _ color: Color = Theme.ink) -> some View {
        font(TypeScale.body(size)).foregroundStyle(color).lineSpacing(size * 0.16)
    }

    func metaText(_ color: Color = Theme.inkTertiary) -> some View {
        font(TypeScale.meta).foregroundStyle(color)
    }

    func sectionLabel() -> some View {
        font(.system(size: 13, weight: .semibold)).foregroundStyle(Theme.ink)
    }

    /// Hover feedback lands at once and fades out over 0.15 s, so the
    /// pointer never waits on it.
    func hoverFade(_ hovering: Bool) -> some View {
        animation(hovering ? nil : Motion.hoverOut, value: hovering)
    }

    func elevation(_ level: Elevation) -> some View {
        modifier(ElevationShadow(level: level))
    }
}
