import SwiftUI

/// The speech models on disk and in the catalog.
struct ModelsTab: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        SettingsIntro("\(model.activeModel?.name ?? "No model") is listening.",
                      fact: "Speech is transcribed on this Mac by the model below. Language models shape the words afterwards and never hear the audio.")
        PageSection("Speech") {
            Card {
                ForEach(model.models) { entry in
                    ModelRow(entry: entry)
                }
            }
        }
        HStack {
            Spacer()
            Button("Rescan disk") { model.rescanModels() }.buttonStyle(.secondary)
        }
        .padding(.bottom, 32)
    }
}

private struct ModelRow: View {
    @Environment(AppModel.self) private var model
    let entry: Model

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(entry.name).bodyText()
                Text(entry.meta).metaText()
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
                    Button("Use") { model.use(entry) }.buttonStyle(.compact)
                    Button("Remove") { model.remove(entry) }.buttonStyle(.quiet)
                }
            case let .downloading(fraction, downloaded):
                HStack(spacing: 12) {
                    Text("\(Int(fraction * 100))% · \(downloaded)").metaText(Theme.ink)
                    Button("Cancel") { model.cancelDownload(entry) }.buttonStyle(.quiet)
                }
            case .available:
                Button("Download") { model.download(entry) }.buttonStyle(.compact)
            }
        }
    }
}
