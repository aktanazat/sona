import SwiftUI
import UIKit

/// The desktop's Porcelain/Ink tokens, ported literally from
/// `src/styles/theme.css`. These are the only colours in the app; nothing else
/// introduces one.
enum Theme {
    /// `--surface-1`
    static let background = dynamic(light: 0xFFFFFF, dark: 0x0A0A0A)
    /// `--surface-2`
    static let surface = dynamic(light: 0xFAFAFA, dark: 0x050505)
    /// `--surface-inset`
    static let inset = dynamic(light: 0xF5F5F5, dark: 0x111111)
    /// `--text-primary`
    static let textPrimary = dynamic(light: 0x171717, dark: 0xEDEDED)
    /// `--text-secondary`
    static let textSecondary = dynamic(light: 0x666666, dark: 0x8F8F8F)
    /// `--text-tertiary`
    static let textTertiary = dynamic(light: 0x999999, dark: 0x5C5C5C)
    /// `--border-subtle`: an alpha hairline in light, a solid grey in dark.
    static let border = hairline
    /// `--accent`
    static let accent = dynamic(light: 0x0070F3, dark: 0x0070F3)
    /// `--on-accent`
    static let onAccent = dynamic(light: 0xFFFFFF, dark: 0xFFFFFF)
    /// `--red-700`, the recording colour
    static let recording = dynamic(light: 0xE5484D, dark: 0xE5484D)
    /// `--success`
    static let success = dynamic(light: 0x067647, dark: 0x47CD89)

    /// `--radius-control`
    static let controlRadius: CGFloat = 8
    /// `--radius-card`
    static let cardRadius: CGFloat = 12

    /// A cluster's colour on the board, from the hue the sorter chose for it. The one
    /// colour that comes from data rather than the desktop's tokens.
    static func cluster(hue: Int) -> Color {
        Color(hue: Double(((hue % 360) + 360) % 360) / 360, saturation: 0.55, brightness: 0.8)
    }

    /* watchOS has one appearance and no trait-resolved colours, so the dark twin is
     * the only one it can ever show. */
    #if os(watchOS)
        private static let hairline = Color(uiColor: UIColor(hex: 0x1F1F1F))

        private static func dynamic(light: UInt32, dark: UInt32) -> Color {
            Color(uiColor: UIColor(hex: dark))
        }
    #else
        private static let hairline = Color(
            uiColor: UIColor { traits in
                traits.userInterfaceStyle == .dark
                    ? UIColor(hex: 0x1F1F1F)
                    : UIColor(white: 0, alpha: 0.08)
            }
        )

        private static func dynamic(light: UInt32, dark: UInt32) -> Color {
            Color(
                uiColor: UIColor { traits in
                    UIColor(hex: traits.userInterfaceStyle == .dark ? dark : light)
                }
            )
        }
    #endif
}

private extension UIColor {
    convenience init(hex: UInt32) {
        self.init(
            red: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255,
            alpha: 1
        )
    }
}
