import SwiftUI

/// The speech models on disk and in the catalog.
struct ModelsTab: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        SettingsIntro("\(model.activeModel?.name ?? "No model") is listening.",
                      fact: "Speech is transcribed on this Mac by the model below. Language models shape the words afterwards and never hear the audio.")
        PageSection("Speech") {
            Card {
                if !model.modelsLoaded {
                    CardLine("Reading your models…")
                } else if let failure = model.modelsError {
                    CardRow {
                        Text(failure)
                            .font(TypeScale.body(14))
                            .foregroundStyle(Theme.live)
                            .fixedSize(horizontal: false, vertical: true)
                    } trailing: {
                        Button("Retry") { model.reloadModels() }.buttonStyle(.secondary)
                    }
                } else if model.models.isEmpty {
                    CardLine("No models to show. Rescan the disk, or check the connection and try again.")
                } else {
                    ForEach(model.models) { entry in
                        ModelRow(entry: entry, operation: model.modelOperations[entry.id])
                    }
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

/// One model: what it is, what is being done to it, and the one action
/// that is open. A phase in flight closes the action that started it, so
/// a slow server cannot be asked twice.
private struct ModelRow: View {
    @Environment(AppModel.self) private var model
    let entry: Model
    let operation: ModelOperation?

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(entry.name).bodyText()
                Text(entry.meta).metaText()
                if case let .downloading(fraction, _) = entry.status, operation?.label == nil {
                    Meter(fraction: fraction).frame(width: 280).padding(.top, 8)
                }
                if let failure = operation?.failure {
                    Text(failure)
                        .font(TypeScale.body(13))
                        .foregroundStyle(Theme.live)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.top, 4)
                }
            }
        } trailing: {
            if let phase = operation?.label {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text(phase).metaText(Theme.ink)
                    if case .downloading = entry.status {
                        Button("Cancel") { model.cancelDownload(entry) }.buttonStyle(.quiet)
                    }
                }
            } else if operation?.failure != nil {
                HStack(spacing: 12) {
                    Button("Retry") { retry() }.buttonStyle(.compact)
                    Button("Dismiss") { model.dismissModelFailure(entry.id) }.buttonStyle(.quiet)
                }
            } else {
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

    /// The same act that failed: a model on disk is loaded again, one that
    /// is not is downloaded again.
    private func retry() {
        switch entry.status {
        case .downloaded, .active: model.use(entry)
        case .available, .downloading: model.download(entry)
        }
    }
}
