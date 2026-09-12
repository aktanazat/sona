import SwiftUI

/// The speech models on disk and the language models Sona may call.
struct ModelsTab: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        SettingsIntro("\(model.activeModel?.name ?? "No model") is listening.",
                      fact: "Speech is transcribed on this Mac by the model below. Language models shape the words afterwards and never hear the audio.")
        PageSection("Speech") {
            Card {
                ForEach(SampleData.models) { entry in
                    ModelRow(entry: entry)
                }
            }
        }
        PageSection("Language") {
            Card {
                ForEach(SampleData.providers) { provider in
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(provider.name).bodyText()
                            Text(provider.detail).metaText()
                        }
                    } trailing: {
                        if provider.connected {
                            Chip("Connected")
                        } else {
                            Button("Connect") {}.buttonStyle(.compact)
                        }
                    }
                }
            }
        }
        HStack {
            Spacer()
            Button("Rescan disk") {}.buttonStyle(.secondary)
        }
    }
}

private struct ModelRow: View {
    let entry: Model

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(entry.name).bodyText()
                Text("\(entry.family) · \(entry.size)").metaText()
                if case let .downloading(fraction, _) = entry.status {
                    Meter(fraction: fraction).frame(width: 280).padding(.top, 8)
                }
            }
        } trailing: {
            switch entry.status {
            case .active:
                Chip("Active")
            case .downloaded:
                HStack(spacing: 12) {
                    Button("Use") {}.buttonStyle(.compact)
                    Button("Remove") {}.buttonStyle(.quiet)
                }
            case let .downloading(fraction, downloaded):
                HStack(spacing: 12) {
                    Text("\(Int(fraction * 100))% · \(downloaded)").metaText(Theme.ink)
                    Button("Cancel") {}.buttonStyle(.quiet)
                }
            case .available:
                Button("Download") {}.buttonStyle(.compact)
            }
        }
    }
}

/// How your words are shaped after they are heard, per app.
struct ModesTab: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if let mode = model.selectedMode {
            ModeDetail(mode: mode)
        } else {
            SettingsIntro("Say it once. Sona shapes it.",
                          fact: "A mode is a prompt and the apps it applies to. Note is on now; the others switch by shortcut or by the app in front.")
            PageSection("Modes") {
                Card {
                    ForEach(SampleData.modes) { mode in
                        CardRow {
                            model.selectedMode = mode
                        } leading: {
                            VStack(alignment: .leading, spacing: 4) {
                                HStack(spacing: 10) {
                                    Text(mode.name).bodyText()
                                    if mode.active {
                                        Chip("On")
                                    }
                                }
                                Text(mode.description).metaText()
                            }
                        } trailing: {
                            HStack(spacing: 24) {
                                Text(mode.apps).metaText()
                                Shortcut(mode.shortcut)
                            }
                        }
                    }
                }
            }
            HStack {
                Spacer()
                Button("New mode") {}.buttonStyle(.primary)
            }
        }
    }
}

struct ModeDetail: View {
    @Environment(AppModel.self) private var model
    let mode: Mode
    @State private var prompt: String

    init(mode: Mode) {
        self.mode = mode
        _prompt = State(initialValue: mode.prompt)
    }

    var body: some View {
        BackLink(title: "Modes") { model.selectedMode = nil }
        PageTitle(mode.name, subtitle: mode.description) {
            HStack(spacing: 10) {
                Button(mode.active ? "On" : "Use now") {}.buttonStyle(.primary)
                Button("Duplicate") {}.buttonStyle(.secondary)
                Button("Delete") {}.buttonStyle(.quiet)
            }
        }
        PageSection("Prompt") {
            Card {
                TextEditor(text: $prompt)
                    .font(TypeScale.body(15))
                    .foregroundStyle(Theme.ink)
                    .scrollContentBackground(.hidden)
                    .padding(16)
                    .frame(minHeight: 140, alignment: .topLeading)
                if mode.prompt.isEmpty {
                    CardRow {
                        Text("Empty means verbatim: the words are pasted exactly as heard.").metaText()
                    }
                }
            }
        }
        PageSection("Applies to") {
            Card {
                CardRow {
                    Text(mode.apps).bodyText()
                } trailing: {
                    Button("Change") {}.buttonStyle(.compact)
                }
                CardRow {
                    Text("Shortcut").bodyText()
                } trailing: {
                    Shortcut(mode.shortcut)
                }
            }
        }
    }
}
