import AppKit
import SwiftUI

enum Theme {
    static let page = dynamic(light: 0xF5F5F5, dark: 0x2C2D2D)
    static let rail = dynamic(light: 0xEBEBEB, dark: 0x3B3C3C)
    static let surface = dynamic(light: 0xFFFFFF, dark: 0x3A3B3B)
    static let inset = dynamic(light: 0xF0F0F0, dark: 0x333434)
    static let control = dynamic(light: 0xE6E6E7, dark: 0x4A4B4B)
    static let selection = dynamic(light: 0xDCDCDD, dark: 0x505151)
    static let wash = dynamic(light: 0x000000, dark: 0xFFFFFF, alpha: 0.045, darkAlpha: 0.06)

    static let ink = dynamic(light: 0x1D1D1F, dark: 0xF5F5F5)
    static let inkSecondary = dynamic(light: 0x5E5E60, dark: 0xB4B5B5)
    static let inkTertiary = dynamic(light: 0x6E6E73, dark: 0xA8A9A9)
    static let inkDisabled = dynamic(light: 0x8A8A8E, dark: 0x7A7B7B)

    static let border = dynamic(light: 0xE0E0E1, dark: 0x4A4B4B)
    static let hairline = dynamic(light: 0xE4E4E5, dark: 0x484949)

    static let accent = dynamic(light: 0x0A6BDB, dark: 0x4C9AFF)
    static let accentFill = dynamic(light: 0x0A6BDB, dark: 0x1F6FEB)
    static let accentSoft = dynamic(light: 0xE6EFFB, dark: 0x2B3A52)
    static let onAccent = Color.white

    static let invert = dynamic(light: 0x1D1D1F, dark: 0xF5F5F5)
    static let onInvert = dynamic(light: 0xFFFFFF, dark: 0x1D1D1F)

    static let live = dynamic(light: 0xD7362E, dark: 0xFF6961)

    static let tileHome = Color(nsColor: NSColor(hex: 0xF0703C, alpha: 1))
    static let tilePlace = Color(nsColor: NSColor(hex: 0x3E8BF7, alpha: 1))
    static let tileSetting = dynamic(light: 0x8E8E93, dark: 0x5E5F5F)

    /// The recording window is dark in either appearance, as a heads-up
    /// display over whatever is on screen.
    static let hud = Color(nsColor: NSColor(hex: 0x2F3030, alpha: 1))
    static let hudBar = Color(nsColor: NSColor(hex: 0x262727, alpha: 1))
    static let hudKey = Color(nsColor: NSColor(hex: 0x4A4B4B, alpha: 1))
    static let hudInk = Color(nsColor: NSColor(hex: 0xF2F2F2, alpha: 1))
    static let hudInkSecondary = Color(nsColor: NSColor(hex: 0xA6A7A7, alpha: 1))
    static let hudLive = Color(nsColor: NSColor(hex: 0xFF6961, alpha: 1))

    static let radiusKey: CGFloat = 5
    static let radiusControl: CGFloat = 8
    static let radiusCard: CGFloat = 14
    static let radiusPanel: CGFloat = 14
    static let radiusDialog: CGFloat = 18

    static let sidebarWidth: CGFloat = 200
    static let margin: CGFloat = 28
    static let contentMax: CGFloat = 880

    private static func dynamic(light: UInt32, dark: UInt32, alpha: CGFloat = 1, darkAlpha: CGFloat? = nil) -> Color {
        Color(
            nsColor: NSColor(name: nil) { appearance in
                let isDark = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
                return NSColor(hex: isDark ? dark : light, alpha: isDark ? (darkAlpha ?? alpha) : alpha)
            }
        )
    }
}

private extension NSColor {
    convenience init(hex: UInt32, alpha: CGFloat) {
        self.init(
            srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255,
            alpha: alpha
        )
    }
}

enum TypeScale {
    static let title: Font = .system(size: 15, weight: .semibold)
    static let hero: Font = .system(size: 26, weight: .semibold)
    static let stat: Font = .system(size: 18, weight: .semibold)
    static let headline: Font = .system(size: 14, weight: .semibold)
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
        font(TypeScale.body(12)).foregroundStyle(color)
    }

    func sectionLabel() -> some View {
        font(.system(size: 13, weight: .semibold)).foregroundStyle(Theme.ink)
    }
}
