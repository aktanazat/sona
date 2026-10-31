import SwiftUI

/// The speech models on disk and in the catalog.
struct ModelsTab: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        PageTitle("Models", subtitle: "\(model.activeModel?.name ?? "No model") is listening. Speech is transcribed on this Mac by the model below. Language models shape the words afterwards and never hear the audio.") {
            Button { model.rescanModels() } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(.circle)
            .help("Rescan disk")
            .accessibilityLabel("Rescan disk")
        }
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
                        Button("Retry") { model.reloadModels() }.buttonStyle(.compact)
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
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: "checkmark")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.accent)
                    .frame(width: 14)
                    .opacity(entry.status == .active ? 1 : 0)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text(entry.name).headlineText()
                    if !entry.detail.isEmpty {
                        Text(entry.detail).metaText(Theme.inkSecondary)
                    }
                    if case let .downloading(fraction, _) = entry.status, operation?.label == nil {
                        Meter(fraction: fraction).frame(maxWidth: 280).padding(.top, 6)
                    }
                    if let failure = operation?.failure {
                        Text(failure)
                            .font(TypeScale.body(13))
                            .foregroundStyle(Theme.live)
                            .fixedSize(horizontal: false, vertical: true)
                            .padding(.top, 4)
                    }
                }
            }
        } trailing: {
            HStack(spacing: 12) {
                if showsSize {
                    Text(entry.size).metaText().monospacedDigit()
                }
                actions
            }
        }
    }

    @ViewBuilder
    private var actions: some View {
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
                Text("Active").metaText(Theme.accent)
            case .downloaded:
                HStack(spacing: 8) {
                    Button("Use") { model.use(entry) }.buttonStyle(.compact)
                    Button { model.remove(entry) } label: {
                        Image(systemName: "trash")
                    }
                    .buttonStyle(.circle)
                    .help("Remove")
                    .accessibilityLabel("Remove")
                }
            case let .downloading(fraction, downloaded):
                HStack(spacing: 12) {
                    Text("\(Int(fraction * 100))% · \(downloaded)").metaText(Theme.ink).monospacedDigit()
                    Button("Cancel") { model.cancelDownload(entry) }.buttonStyle(.quiet)
                }
            case .available:
                Button { model.download(entry) } label: {
                    Image(systemName: "arrow.down")
                }
                .buttonStyle(.circle)
                .help("Download")
                .accessibilityLabel("Download")
            }
        }
    }

    /// A download in flight already says how much of the size has arrived.
    private var showsSize: Bool {
        if case .downloading = entry.status { return false }
        return true
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
