import AppKit
import SwiftUI

private enum ProblemReportPhase {
    case editing
    case saving
    case saved(URL)
    case failed(String)

    var saving: Bool {
        if case .saving = self { return true }
        return false
    }
}

struct ProblemReportSheet: View {
    let historyID: Int64?
    @Environment(AppModel.self) private var model
    @State private var details = ""
    @State private var includeLogs = false
    @State private var phase = ProblemReportPhase.editing
    @State private var pending: Task<Void, Never>?

    private var title: LocalizedStringResource {
        historyID == nil ? "Report a problem" : "Report a dictation problem"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: Space.l) {
                ProblemReportHeading(title: title)
                ProblemReportFields(details: $details, includeLogs: $includeLogs)
                    .disabled(phase.saving)
                ProblemReportStatus(phase: phase)
            }
            .padding(24)
            Hairline()
            HStack(spacing: Space.m) {
                if case let .saved(url) = phase {
                    Button("Show in Finder") {
                        NSWorkspace.shared.activateFileViewerSelecting([url])
                    }
                    .buttonStyle(.quiet)
                    if let issueURL = URL(string: AboutStore.repositoryURL + "/issues/new") {
                        Link("Open GitHub issue", destination: issueURL)
                    }
                }
                Spacer(minLength: 0)
                Button("Close") { model.sheet = nil }
                    .buttonStyle(.quiet)
                    .keyboardShortcut(.cancelAction)
                Button(phase.saving ? "Saving…" : "Save Report", action: save)
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
                    .disabled(phase.saving)
            }
            .padding(.horizontal, 24)
            .frame(minHeight: 56)
        }
        .frame(width: 580)
        .background(Theme.page)
        .onChange(of: details) { _, _ in edited() }
        .onChange(of: includeLogs) { _, _ in edited() }
        .onDisappear { pending?.cancel() }
    }

    private func edited() {
        if case .saved = phase { phase = .editing }
    }

    private func save() {
        let context = ProblemReportContext(historyID: historyID)
        let description = details
        let logsRequested = includeLogs
        phase = .saving
        pending = Task {
            defer { if !Task.isCancelled { pending = nil } }
            do {
                try Task.checkCancellation()
                let directory: String?
                if logsRequested {
                    directory = try await model.core.request("get_log_dir_path")
                } else {
                    directory = nil
                }
                try Task.checkCancellation()
                let destination = try await ProblemReportFile.save(
                    context: context, description: description, logDirectory: directory)
                try Task.checkCancellation()
                phase = .saved(destination)
            } catch is CancellationError {
                return
            } catch {
                guard !Task.isCancelled else { return }
                phase = .failed(error.localizedDescription)
            }
        }
    }
}

private struct ProblemReportHeading: View {
    let title: LocalizedStringResource

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            Text(title).titleText().accessibilityAddTraits(.isHeader)
            Text("Save a local report for review. Nothing is uploaded.")
                .metaText(Theme.inkSecondary)
        }
    }
}

private struct ProblemReportFields: View {
    @Binding var details: String
    @Binding var includeLogs: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            Text("What went wrong?").bodyText(14)
            TextEditor(text: $details)
                .font(.body)
                .frame(minHeight: 120, maxHeight: 220)
                .accessibilityLabel("What went wrong?")
            Toggle("Include recent logs", isOn: $includeLogs)
                .toggleStyle(.checkbox)
            Text("Logs can contain text and file paths from your work. Audio files are not included.")
                .metaText(Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

private struct ProblemReportStatus: View {
    let phase: ProblemReportPhase

    var body: some View {
        switch phase {
        case .editing:
            EmptyView()
        case .saving:
            HStack(spacing: Space.s) {
                ProgressView().controlSize(.small)
                Text("Preparing the report…").metaText()
            }
        case .saved:
            Text("Report saved. Review it before attaching it to a public GitHub issue.")
                .metaText(Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
        case let .failed(reason):
            Text("The report could not be saved: \(reason)")
                .metaText(Theme.live)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}
