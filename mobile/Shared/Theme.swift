import SwiftUI
import UIKit

enum Theme {
    #if os(watchOS)
        static let background = dynamic(light: 0xFFFFFF, dark: 0x0A0A0A)
        static let textSecondary = dynamic(light: 0x666666, dark: 0x8F8F8F)
        static let recording = dynamic(light: 0xE5484D, dark: 0xE5484D)
    #else
        static let background = dynamic(light: 0xF2F2F2, dark: 0x131313)
        static let textSecondary = dynamic(light: 0x636363, dark: 0xA0A0A0)
        static let recording = dynamic(light: 0xD0281C, dark: 0xFF5A4A)
    #endif
    static let surface = dynamic(light: 0xFFFFFF, dark: 0x232323)
    static let inset = dynamic(light: 0xE6E6E6, dark: 0x2E2E2E)
    static let textPrimary = dynamic(light: 0x111111, dark: 0xF5F5F5)
    static let textTertiary = dynamic(light: 0x6E6E6E, dark: 0x8C8C8C)
    static let border = dynamic(light: 0xE0E0E0, dark: 0x333333)
    static let accent = dynamic(light: 0x007AFF, dark: 0x0A84FF)
    static let recordingFill = dynamic(light: 0xE5372B, dark: 0x5E160F)
    static let onRecording = dynamic(light: 0xFFFFFF, dark: 0xFF5A4A)

    static let pageInset: CGFloat = 20
    static let controlRadius: CGFloat = 8
    static let cardRadius: CGFloat = 26

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

/// The record control's face: a red capsule inside a ring. The caller owns the button.
struct RecordPill: View {
    enum Glyph { case record, stop, busy }

    let glyph: Glyph
    var title: LocalizedStringKey?
    var height: CGFloat = 56
    @Environment(\.isEnabled) private var isEnabled

    var body: some View {
        HStack(spacing: 8) {
            face
            if let title {
                Text(title)
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(1)
            }
        }
        .foregroundStyle(Theme.onRecording)
        .padding(.horizontal, title == nil ? 0 : height * 0.4)
        .frame(minWidth: height * 1.75, minHeight: height, maxHeight: height)
        .background {
            Capsule().fill(Theme.recordingFill).padding(5)
            Capsule().strokeBorder(Theme.recording, lineWidth: 2)
        }
        .opacity(isEnabled ? 1 : 0.4)
        .contentShape(Capsule())
    }

    @ViewBuilder private var face: some View {
        let side = height * 0.3
        switch glyph {
        case .record:
            Circle().fill(Theme.onRecording).frame(width: side, height: side)
        case .stop:
            RoundedRectangle(cornerRadius: side * 0.2, style: .continuous)
                .fill(Theme.onRecording)
                .frame(width: side, height: side)
        case .busy:
            ProgressView().tint(Theme.onRecording)
        }
    }
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
