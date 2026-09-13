import SwiftUI

/// The things Sona does on its own after a meeting, and what they did.
///
/// Two sections, one model: the switches, and the log of every run. A pass
/// that found nothing still wrote a receipt, and the log is where those
/// belong — "Nothing new to do" is a true sentence about a real run.
struct WorkflowsView: View {
    let store: WorkflowsStore

    var body: some View {
        Page {
            PageTitle("Workflows", subtitle: Self.subtitle)
            ErrorNote(store.error)
            catalogue
            runLog
        }
    }

    private static let subtitle =
        "What Sona does on its own after a meeting, and what each pass did."

    @ViewBuilder private var catalogue: some View {
        PageSection("What Sona does on its own") {
            Card {
                if store.loadingWorkflows && store.entries.isEmpty {
                    WorkflowLine("Reading your workflows…")
                } else if store.entries.isEmpty {
                    WorkflowLine("No workflows to show.")
                } else {
                    ForEach(store.entries) { workflow in
                        WorkflowRow(store: store, workflow: workflow)
                    }
                }
            }
        }
    }

    @ViewBuilder private var runLog: some View {
        PageSection("Activity") {
            Card {
                if !store.receipts.isEmpty {
                    WorkflowWeek(values: workflowRunsPerDay(store.receipts))
                }
                if store.loadingRuns && store.receipts.isEmpty {
                    WorkflowLine("Reading the run log…")
                } else if store.receipts.isEmpty && store.runError == nil {
                    WorkflowLine("No activity yet.")
                } else {
                    ForEach(store.receipts) { receipt in
                        WorkflowRunRow(receipt: receipt)
                    }
                }
                if let message = store.runError {
                    WorkflowRetryRow(message: message, busy: store.loadingRuns) {
                        Task {
                            if store.receipts.isEmpty {
                                await store.loadFirstRunPage()
                            } else {
                                await store.loadMoreRuns()
                            }
                        }
                    }
                } else if store.hasMoreRuns {
                    CardRow {
                        Text("\(store.receipts.count) runs read so far").metaText()
                    } trailing: {
                        Button(store.loadingMore ? "Loading…" : "Show more") {
                            Task { await store.loadMoreRuns() }
                        }
                        .buttonStyle(.secondary)
                        .disabled(store.loadingMore)
                    }
                }
            }
        }
    }
}

/// One workflow: what it does, what it last did, and its switch.
struct WorkflowRow: View {
    let store: WorkflowsStore
    let workflow: WorkflowSummary

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 5) {
                Text(workflow.id.name).bodyText()
                if let detail = workflow.id.detail {
                    Text(detail)
                        .metaText()
                        .fixedSize(horizontal: false, vertical: true)
                }
                lastRun
            }
        } trailing: {
            Toggle("", isOn: binding)
                .labelsHidden()
                .toggleStyle(.switch)
                .tint(Theme.accent)
                .disabled(store.pending != nil)
        }
    }

    @ViewBuilder private var lastRun: some View {
        if let run = workflow.lastRun {
            HStack(spacing: 6) {
                WorkflowStatusGlyph(status: run.status)
                Text(workflowOutcomeText(run))
                    .metaText(Theme.inkSecondary)
                    .lineLimit(1)
                Text("·").metaText(Theme.inkDisabled)
                Text(promptRelativeTime(run.finishedAtUtcMs)).metaText(Theme.inkDisabled)
            }
        } else {
            Text("Not run yet").metaText(Theme.inkDisabled)
        }
    }

    private var binding: Binding<Bool> {
        Binding(
            get: { workflow.enabled },
            set: { enabled in Task { await store.setEnabled(workflow.id, enabled) } }
        )
    }
}

/// One row of the run log: what happened, which workflow, and when.
struct WorkflowRunRow: View {
    let receipt: WorkflowRunReceipt

    var body: some View {
        CardRow {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                WorkflowStatusGlyph(status: receipt.status)
                VStack(alignment: .leading, spacing: 4) {
                    Text(workflowOutcomeText(receipt))
                        .bodyText(14)
                        .fixedSize(horizontal: false, vertical: true)
                    Text("\(receipt.workflowId.name) · \(promptRelativeTime(receipt.finishedAtUtcMs))")
                        .metaText(Theme.inkDisabled)
                    if let failure = receipt.error {
                        Text(failure)
                            .font(TypeScale.body(13))
                            .foregroundStyle(Theme.live)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
    }
}

/// The tick, cross or dash a run ended with.
struct WorkflowStatusGlyph: View {
    let status: WorkflowRunStatus

    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(tone)
            .accessibilityLabel(status.label)
    }

    private var symbol: String {
        switch status {
        case .ok: "checkmark"
        case .failed: "xmark"
        case .skipped, .unknown: "minus"
        }
    }

    private var tone: Color {
        switch status {
        case .ok: Theme.ink
        case .failed: Theme.live
        case .skipped, .unknown: Theme.inkTertiary
        }
    }
}

/// Runs per day over the last seven local days, today last.
struct WorkflowWeek: View {
    let values: [Int]

    var body: some View {
        HStack(alignment: .bottom, spacing: 6) {
            VStack(alignment: .leading, spacing: 4) {
                Text(promptCounted(values.reduce(0, +), "run", "runs")).bodyText(14)
                Text("in the last 7 days").metaText(Theme.inkDisabled)
            }
            Spacer(minLength: 16)
            HStack(alignment: .bottom, spacing: 5) {
                ForEach(Array(values.enumerated()), id: \.offset) { index, value in
                    RoundedRectangle(cornerRadius: 2)
                        .fill(index == values.count - 1 ? Theme.accent : Theme.selection)
                        .frame(width: 10, height: height(value))
                }
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { Hairline() }
    }

    private func height(_ value: Int) -> CGFloat {
        let peak = max(values.max() ?? 0, 1)
        return 3 + 25 * CGFloat(value) / CGFloat(peak)
    }
}

/// What the run log could not read, and the button that tries again.
struct WorkflowRetryRow: View {
    let message: String
    let busy: Bool
    let retry: () -> Void

    var body: some View {
        CardRow {
            Text(message)
                .font(TypeScale.body(14))
                .foregroundStyle(Theme.live)
                .fixedSize(horizontal: false, vertical: true)
        } trailing: {
            Button("Retry", action: retry)
                .buttonStyle(.secondary)
                .disabled(busy)
        }
    }
}

/// One quiet sentence where a row would be too much furniture.
struct WorkflowLine: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(text)
            .metaText()
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 20)
            .padding(.vertical, 14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .overlay(alignment: .bottom) { Hairline() }
    }
}
