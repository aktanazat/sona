import AppKit
import SwiftUI

enum Theme {
    static let page = dynamic(light: 0xFFFFFF, dark: 0x111214)
    static let rail = dynamic(light: 0xF7F7F7, dark: 0x17181A)
    static let surface = dynamic(light: 0xFFFFFF, dark: 0x1B1C1E)
    static let inset = dynamic(light: 0xF7F7F7, dark: 0x202226)
    static let selection = dynamic(light: 0xE8EAED, dark: 0x2A2D31)
    static let wash = dynamic(light: 0x202124, dark: 0xECEFF1, alpha: 0.045, darkAlpha: 0.055)

    static let ink = dynamic(light: 0x202124, dark: 0xECEFF1)
    static let inkSecondary = dynamic(light: 0x555B61, dark: 0xB2B8BE)
    static let inkTertiary = dynamic(light: 0x666D74, dark: 0x9DA5AE)
    static let inkDisabled = dynamic(light: 0x858B92, dark: 0x69717A)

    static let border = dynamic(light: 0xDCDFE3, dark: 0x35393F)
    static let hairline = dynamic(light: 0xE8EAED, dark: 0x2B2E33)

    static let accent = dynamic(light: 0x2463A6, dark: 0x8BBCEC)
    static let accentSoft = dynamic(light: 0xEDF3FA, dark: 0x1C2A39)
    static let onAccent = dynamic(light: 0xFFFFFF, dark: 0x111214)

    static let invert = dynamic(light: 0x202124, dark: 0xECEFF1)
    static let onInvert = dynamic(light: 0xFFFFFF, dark: 0x111214)

    static let live = dynamic(light: 0xC93C42, dark: 0xFF6369)

    static let radiusKey: CGFloat = 3
    static let radiusControl: CGFloat = 5
    static let radiusCard: CGFloat = 6
    static let radiusPanel: CGFloat = 10
    static let radiusDialog: CGFloat = 12

    static let sidebarWidth: CGFloat = 184
    static let margin: CGFloat = 24
    static let contentMax: CGFloat = 1040

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
    static let title: Font = .system(size: 22, weight: .semibold)
    static let hero: Font = .system(size: 24, weight: .semibold)
    static let stat: Font = .system(size: 24, weight: .semibold)
    static let headline: Font = .system(size: 15, weight: .medium)
    static func body(_ size: CGFloat = 14) -> Font { .system(size: size, weight: .regular) }
    static func label(_ size: CGFloat = 14) -> Font { .system(size: size, weight: .medium) }
    static func mono(_ size: CGFloat = 13) -> Font { .system(size: size, weight: .medium, design: .monospaced) }
}

extension View {
    func titleText() -> some View {
        font(TypeScale.title).foregroundStyle(Theme.ink).tracking(-0.2)
    }

    func heroText() -> some View {
        font(TypeScale.hero).foregroundStyle(Theme.ink).tracking(-0.2)
    }

    func headlineText() -> some View {
        font(TypeScale.headline).foregroundStyle(Theme.ink)
    }

    func bodyText(_ size: CGFloat = 14, _ color: Color = Theme.ink) -> some View {
        font(TypeScale.body(size)).foregroundStyle(color).lineSpacing(size * 0.16)
    }

    func metaText(_ color: Color = Theme.inkTertiary) -> some View {
        font(TypeScale.body(13)).foregroundStyle(color)
    }

    func sectionLabel() -> some View {
        font(TypeScale.label(13)).foregroundStyle(Theme.inkSecondary)
    }
}
