import Foundation
import SwiftUI

/// How formal a Message, Email or Note rewrite reads: the Mac mode's five tone steps.
/// The rule texts are src-tauri/resources/prompts/tone_*.txt as the Mac renders them.
enum DictationTone: String, CaseIterable, Identifiable {
    case casual
    case semiCasual = "semi_casual"
    case balanced
    case semiFormal = "semi_formal"
    case formal

    var id: String { rawValue }

    /// The Mac modes that carry a tone. Their phone styles keep the Mac mode's id.
    static let styleIDs: Set<String> = ["message", "email", "notes"]

    static let header = """
        [TONE]
        Apply the selected tone rule to the final text while preserving the speaker's meaning and factual limits.

        """

    /// Nil for balanced, which the Mac sends with no tone rule at all.
    var rule: String? {
        switch self {
        case .casual: """
            [TONE_RULE: CASUAL]
            Use relaxed conversational language. Everyday contractions are fine; preserve intentional casing and wording from the transcript.

            """
        case .semiCasual: """
            [TONE_RULE: SEMI_CASUAL]
            Use friendly, natural language. Keep the structure clear without making it stiff or overly formal.

            """
        case .balanced: nil
        case .semiFormal: """
            [TONE_RULE: SEMI_FORMAL]
            Use clear professional language with restrained warmth. Keep the wording courteous and direct.

            """
        case .formal: """
            [TONE_RULE: FORMAL]
            Use precise, professional language. Avoid slang, filler, and unnecessary familiarity.

            """
        }
    }

    var label: LocalizedStringKey {
        switch self {
        case .casual: "tone.casual"
        case .semiCasual: "tone.semiCasual"
        case .balanced: "tone.balanced"
        case .semiFormal: "tone.semiFormal"
        case .formal: "tone.formal"
        }
    }

    private static let block = try? NSRegularExpression(
        pattern: #"\n\n\[TONE\]\n[^\n]*\n\[TONE_RULE: [A-Z_]+\]\n[^\n]*\n?"#)

    /// The tone the Mac wrote into a style's prompt, balanced when it wrote none.
    static func written(in prompt: String) -> Self {
        allCases.first { tone in
            guard let rule = tone.rule, let marker = rule.split(separator: "\n").first else { return false }
            return prompt.contains(marker)
        } ?? .balanced
    }

    /// `prompt` with this tone's rule in place of the one the Mac wrote.
    func applied(to prompt: String) -> String {
        let range = NSRange(prompt.startIndex..., in: prompt)
        let bare = Self.block?.stringByReplacingMatches(in: prompt, range: range, withTemplate: "") ?? prompt
        guard let rule else { return bare }
        return bare + "\n\n" + Self.header + rule
    }

    /// The tone chosen on the phone for `style`, or nil to keep the Mac's.
    static func chosen(for style: String, in defaults: UserDefaults = DictationPreferences.shared) -> Self? {
        defaults.string(forKey: key(style)).flatMap(Self.init(rawValue:))
    }

    static func choose(_ tone: Self, for style: String, in defaults: UserDefaults = DictationPreferences.shared) {
        defaults.set(tone.rawValue, forKey: key(style))
    }

    /// A style's prompt as the phone sends it to the model: with the phone's tone when one
    /// was chosen for a style that has one.
    static func prompt(of style: DictationProfile.Style, in defaults: UserDefaults = DictationPreferences.shared) -> String {
        guard styleIDs.contains(style.id), let tone = chosen(for: style.id, in: defaults) else { return style.prompt }
        return tone.applied(to: style.prompt)
    }

    private static func key(_ style: String) -> String { "sona.dictation.tone.\(style)" }
}

/// Casual to formal in five steps, for a style that has a tone. Without its title row it
/// fits the keyboard's panel; the step's name stays in its accessibility value.
struct ToneSlider: View {
    let style: DictationProfile.Style
    let showsTitle: Bool
    @State private var step: Double

    init(style: DictationProfile.Style, showsTitle: Bool = true) {
        self.style = style
        self.showsTitle = showsTitle
        let tone = DictationTone.chosen(for: style.id) ?? DictationTone.written(in: style.prompt)
        _step = State(initialValue: Double(DictationTone.allCases.firstIndex(of: tone) ?? 2))
    }

    private var tone: DictationTone { DictationTone.allCases[Int(step.rounded())] }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            if showsTitle {
                HStack {
                    Text("tone.title")
                    Spacer()
                    Text(tone.label).foregroundStyle(Theme.textSecondary)
                        .contentTransition(.numericText())
                        .animation(.snappy, value: tone)
                }
                .font(.subheadline)
            }
            Slider(value: $step, in: 0...4, step: 1) {
                Text("tone.title")
            } minimumValueLabel: {
                Text("tone.casual").font(.caption2).foregroundStyle(Theme.textSecondary)
            } maximumValueLabel: {
                Text("tone.formal").font(.caption2).foregroundStyle(Theme.textSecondary)
            }
            .accessibilityValue(Text(tone.label))
            .sensoryFeedback(.selection, trigger: tone)
            .onChange(of: tone) { _, value in DictationTone.choose(value, for: style.id) }
        }
        .accessibilityIdentifier("tone-slider")
    }
}
