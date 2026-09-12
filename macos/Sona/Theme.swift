import AppKit
import SwiftUI

/// Sona's own skin, ported from src/styles/theme.css. Light is Porcelain: a
/// barely-warm off-white page, white cards, warm greys, one scarce bronze
/// accent. Dark is its Ink twin, where the canvas is the darkest surface and
/// every step lifts toward the reader. Every colour in the app comes from here.
enum Theme {
    /// The window: page and sidebar alike. `--bg-2`.
    static let page = dynamic(light: 0xFAF9F6, dark: 0x151311)
    /// A card or a control. `--surface-raised`.
    static let surface = dynamic(light: 0xFFFFFF, dark: 0x1C1A17)
    /// The cream band at the head of a card. `--surface-inset`.
    static let inset = dynamic(light: 0xF6F3EA, dark: 0x252220)
    /// The selected sidebar item and a pressed control. `--gray-400` / `--gray-200`.
    static let selection = dynamic(light: 0xEBE7E0, dark: 0x252220)
    /// A hovered row. `--wash-hover`.
    static let wash = dynamic(light: 0x1C1A17, dark: 0xECE7E0, alpha: 0.04)

    static let ink = dynamic(light: 0x1C1A17, dark: 0xECE7E0)
    static let inkSecondary = dynamic(light: 0x5D574F, dark: 0x9B948B)
    static let inkTertiary = dynamic(light: 0x736C63, dark: 0x8F887F)
    static let inkDisabled = dynamic(light: 0x8E877D, dark: 0x66605A)

    /// Object outlines. `--border`.
    static let border = dynamic(light: 0xEBE7E0, dark: 0x322E2A)
    /// Dividers between rows. `--border-subtle`.
    static let hairline = dynamic(light: 0x1C1A17, dark: 0x2B2825, alpha: 0.08, darkAlpha: 1)

    /// The one accent: bronze that reads as warm ink, never a brand blue.
    static let accent = dynamic(light: 0x8B5A2B, dark: 0xD3A06A)
    static let accentSoft = dynamic(light: 0xF4EBE0, dark: 0x2B2218)
    static let onAccent = dynamic(light: 0xFFFFFF, dark: 0x151311)

    /// The primary button: ink fill, page text. `--invert-bg` / `--invert-fg`.
    static let invert = dynamic(light: 0x1C1A17, dark: 0xECE7E0)
    static let onInvert = dynamic(light: 0xFFFFFF, dark: 0x100F0D)

    /// Live state only: the recording dot and clock. `--error`.
    static let live = dynamic(light: 0xC93C42, dark: 0xFF6369)

    /// Controls 10, cards 14, floating panels 16, dialogs 18. The step from
    /// control to panel is what makes a floating surface read as an object.
    static let radiusKey: CGFloat = 5
    static let radiusControl: CGFloat = 10
    static let radiusCard: CGFloat = 14
    static let radiusPanel: CGFloat = 16
    static let radiusDialog: CGFloat = 18

    static let sidebarWidth: CGFloat = 248
    static let margin: CGFloat = 40
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

/// The type scale: the system face throughout, as the old app used it.
enum TypeScale {
    /// A page title: "Library".
    static let title: Font = .system(size: 28, weight: .semibold)
    /// The one big word on the capture page: "Ready".
    static let hero: Font = .system(size: 32, weight: .semibold)
    /// A number in a stat card.
    static let stat: Font = .system(size: 32, weight: .semibold)
    /// A row title or a detail headline.
    static let headline: Font = .system(size: 17, weight: .semibold)
    static func body(_ size: CGFloat = 15) -> Font { .system(size: size, weight: .regular) }
    static func label(_ size: CGFloat = 15) -> Font { .system(size: size, weight: .medium) }
    static func mono(_ size: CGFloat = 13) -> Font { .system(size: size, weight: .medium, design: .monospaced) }
}

extension View {
    func titleText() -> some View {
        font(TypeScale.title).foregroundStyle(Theme.ink).tracking(-0.3)
    }

    func heroText() -> some View {
        font(TypeScale.hero).foregroundStyle(Theme.ink).tracking(-0.4)
    }

    func headlineText() -> some View {
        font(TypeScale.headline).foregroundStyle(Theme.ink)
    }

    func bodyText(_ size: CGFloat = 15, _ color: Color = Theme.ink) -> some View {
        font(TypeScale.body(size)).foregroundStyle(color).lineSpacing(size * 0.3)
    }

    /// The quiet line under a title or beside a row: 14 in tertiary ink.
    func metaText(_ color: Color = Theme.inkTertiary) -> some View {
        font(TypeScale.body(14)).foregroundStyle(color)
    }

    /// "Activity", "What Sona did": the label above a card.
    func sectionLabel() -> some View {
        font(TypeScale.body(15)).foregroundStyle(Theme.inkSecondary)
    }
}
