import SwiftUI
import UIKit

enum Theme {
    #if os(watchOS)
        static let background = dynamic(light: 0xFFFFFF, dark: 0x0A0A0A)
        static let textSecondary = dynamic(light: 0x666666, dark: 0x8F8F8F)
        static let recording = dynamic(light: 0xE5484D, dark: 0xE5484D)
    #else
        static let background = dynamic(light: 0xFFFFFF, dark: 0x111214)
        static let textSecondary = dynamic(light: 0x555B61, dark: 0xB0B6BD)
        static let recording = dynamic(light: 0xC7353D, dark: 0xFF7A80)
    #endif
    static let surface = dynamic(light: 0xF7F7F7, dark: 0x1B1C1E)
    static let inset = dynamic(light: 0xF3F4F5, dark: 0x202225)
    static let textPrimary = dynamic(light: 0x202124, dark: 0xECEFF1)
    static let textTertiary = dynamic(light: 0x646B72, dark: 0x9BA2AA)
    static let border = dynamic(light: 0xE4E6E8, dark: 0x35383D)
    static let accent = dynamic(light: 0x2463A6, dark: 0x8BBCEC)
    static let onAccent = dynamic(light: 0xFFFFFF, dark: 0x111214)
    static let onRecording = dynamic(light: 0xFFFFFF, dark: 0x111214)
    static let success = dynamic(light: 0x067647, dark: 0x47CD89)

    static let pageInset: CGFloat = 20
    static let controlRadius: CGFloat = 6
    static let cardRadius: CGFloat = 8

    /// A cluster's colour on the board, from the hue the sorter chose for it. The one
    /// colour that comes from data rather than the desktop's tokens.
    static func cluster(hue: Int) -> Color {
        Color(hue: Double(((hue % 360) + 360) % 360) / 360, saturation: 0.55, brightness: 0.8)
    }

    /* watchOS has one appearance and no trait-resolved colours, so the dark twin is
     * the only one it can ever show. */
    #if os(watchOS)
        private static func dynamic(light: UInt32, dark: UInt32) -> Color {
            Color(uiColor: UIColor(hex: dark))
        }
    #else
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
