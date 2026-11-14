import SwiftUI

/// Appearance: the language, the theme, its contrast, and whether the
/// sidebar and floating panels are see-through.
struct AppearanceView: View {
    let store: AboutStore

    var body: some View {
        Page {
            PageTitle("Appearance", subtitle: "")
            ErrorNote(store.error)

            Card {
                ChoiceRow(
                    title: "Language",
                    detail: "The language the assistant answers in and the menu bar menu uses. This window stays in English.",
                    choices: AppLanguage.all,
                    label: { $0.label },
                    selection: Binding(
                        get: { store.language },
                        set: { store.setLanguage($0.code) }
                    )
                )
                ChoiceRow(
                    title: "Theme",
                    choices: AppearanceTheme.allCases,
                    label: { $0.label },
                    selection: Binding(
                        get: { store.theme },
                        set: { store.setTheme($0) }
                    )
                )
                ContrastRow()
                ToggleRow(
                    title: "See-through sidebar and panels",
                    detail: store.material == store.materialChoice
                        ? nil
                        : "Not see-through while Reduce Transparency or Increase Contrast is on.",
                    isOn: Binding(
                        get: { store.materialChoice == .glass },
                        set: { store.setMaterial($0 ? .glass : .solid) }
                    )
                )
            }
        }
    }
}

/// The contrast every color is generated at. While Increase Contrast is on,
/// the system's highest setting wins and the slider has nothing to move.
private struct ContrastRow: View {
    @Bindable private var theme = ThemeStore.shared
    @Environment(\.colorSchemeContrast) private var schemeContrast

    var body: some View {
        let increased = schemeContrast == .increased
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text("Contrast").bodyText()
                if increased {
                    Text("Highest while Increase Contrast is on.").metaText()
                }
            }
        } trailing: {
            Slider(value: $theme.contrast, in: ThemeStore.contrastRange, step: 1)
                .frame(width: 128)
                .tint(Theme.accent)
                .disabled(increased)
                .accessibilityLabel("Contrast")
        }
    }
}
