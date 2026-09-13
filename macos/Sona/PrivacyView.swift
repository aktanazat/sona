import SwiftUI

/// Privacy: what Sona reads from your other apps, what can leave this Mac,
/// how history is kept, and the two one-time moves that brought data here.
///
/// The page states facts before it offers switches. Every route that can carry
/// anything off the machine is one compact line, and a route whose state could
/// not be read says so rather than guessing.
struct PrivacyView: View {
    let store: PrivacyStore

    var body: some View {
        Page {
            PageTitle("Privacy", subtitle: "What Sona reads, what leaves this Mac, and what it keeps.")
            ErrorNote(store.error)
            contextSection
            diagnosticsSection
            externalSection
            egressSection
            storageSection
            upstreamSection
            identitySection
        }
    }

    // MARK: - Context capture

    private var contextSection: some View {
        PageSection("Context for AI cleanup") {
            Card {
                ChoiceRow(
                    title: "Global context ceiling",
                    detail: store.contextCeiling.sentence,
                    choices: ContextPolicy.allCases,
                    label: \.label,
                    selection: Binding(
                        get: { store.contextCeiling },
                        set: { ceiling in Task { await store.setContextCeiling(ceiling) } }))
                .disabled(store.ceilingBusy)
                ToggleRow(
                    title: "Include browser URLs",
                    detail: "Allow a frontmost browser page URL to be captured when a mode and the ceiling allow it.",
                    isOn: Binding(
                        get: { store.urlCaptureEnabled },
                        set: { enabled in Task { await store.setUrlCapture(enabled) } }))
                .disabled(store.urlCaptureBusy)
            }
        }
    }

    // MARK: - Diagnostics

    @ViewBuilder
    private var diagnosticsSection: some View {
        if let diagnostics = store.diagnostics {
            PageSection("Context diagnostics") {
                Card {
                    CardRow {
                        Text("Accessibility").bodyText()
                    } trailing: {
                        PrivacyFact(
                            diagnostics.accessibility.word,
                            live: diagnostics.accessibility == .denied)
                    }
                    ForEach(Array(diagnostics.sources.enumerated()), id: \.offset) { _, source in
                        CardRow {
                            Text(source.label).bodyText()
                        } trailing: {
                            PrivacyFact(source.status.word, live: source.status.isRefusal)
                        }
                    }
                    ActionRow(
                        title: "Sources",
                        detail: "What the last capture could reach on this Mac.",
                        button: "Refresh",
                        busy: store.diagnosticsBusy
                    ) {
                        Task { await store.refreshDiagnostics() }
                    }
                }
            }
        }
    }

    // MARK: - External access

    private var externalSection: some View {
        PageSection("What other programs may do") {
            Card {
                ToggleRow(
                    title: "External access",
                    detail: "Let another program on this Mac read the corpus through Sona's query plane.",
                    isOn: Binding(
                        get: { store.externalQueryEnabled },
                        set: { enabled in Task { await store.setExternalQuery(enabled) } }))
                ToggleRow(
                    title: "External changes",
                    detail: "Let another program close a loop or change a meeting, not only read one.",
                    isOn: Binding(
                        get: { store.externalMutationsEnabled },
                        set: { enabled in Task { await store.setExternalMutations(enabled) } }))
            }
            .disabled(store.externalBusy)
        }
    }

    // MARK: - Egress

    private var egressSection: some View {
        PageSection("What leaves this Mac") {
            Card {
                CardRow {
                    Text("Nothing leaves this Mac except on the routes listed here, and provider keys stay in the system credential store, never in Sona's settings.")
                        .bodyText(14, Theme.inkSecondary)
                }
                CardRow {
                    Text("AI cleanup").bodyText()
                } trailing: {
                    PrivacyFact(store.cleanupRoute.fact, live: store.cleanupRoute == .failed)
                }
                if store.transcriptionRoute == .failed {
                    PrivacyFailureRow(
                        "Cloud transcription setup could not be checked.",
                        retry: "Retry"
                    ) {
                        Task { await store.retryEgress() }
                    }
                } else {
                    CardRow {
                        Text("Cloud transcription").bodyText()
                    } trailing: {
                        PrivacyFact(store.transcriptionRoute.fact, live: false)
                    }
                }
                // The chip above names the route. This names the payload, and
                // it is the one sentence in the app that itemises what leaves
                // the machine — so while a cloud route exists it is read.
                if case .providers = store.transcriptionRoute {
                    CardRow {
                        Text("For a cloud-enabled mode, Sona sends the recorded audio, the selected language, and the words that mode lists directly to the provider. What Sona reads from your other apps, and your keys, stay on this Mac.")
                            .bodyText(14, Theme.inkSecondary)
                    }
                }
                if store.cloudSyncFailed {
                    PrivacyFailureRow(
                        "Sona could not read the cloud sync configuration.",
                        retry: "Retry"
                    ) {
                        Task { await store.reloadCloudSync() }
                    }
                } else {
                    CardRow {
                        Text("Cloud sync").bodyText()
                    } trailing: {
                        PrivacyFact(store.cloudSync?.fact ?? "…", live: store.cloudSync?.error != nil)
                    }
                    if let sentence = store.cloudSync?.sentence {
                        CardRow {
                            Text(sentence).bodyText(14, Theme.live)
                        }
                    }
                }
            }
        }
    }

    // MARK: - History storage

    private var storageSection: some View {
        PageSection("History on this device") {
            Card {
                if let failure = store.storageFailure {
                    PrivacyFailureRow(failure, retry: "Retry") {
                        Task { await store.reloadStorage() }
                    }
                } else if let storage = store.storage {
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("History storage").bodyText()
                            if let migrated = storage.migratedDate, storage.isReadable {
                                Text("Encrypted \(migrated.relativeDay) at \(migrated.time)").metaText()
                            }
                        }
                    } trailing: {
                        PrivacyFact(storage.word, live: !storage.isReadable && !storage.isUnlocking)
                    }
                    if let sentence = storage.sentence {
                        CardRow {
                            Text(sentence).bodyText(14, Theme.live)
                        }
                    }
                } else {
                    CardRow {
                        Text("Reading how history is stored…").bodyText(14, Theme.inkSecondary)
                    }
                }
            }
        }
    }

    // MARK: - The legacy app's data

    @ViewBuilder
    private var upstreamSection: some View {
        if let status = store.upstream, status.available {
            PageSection("Import from legacy app") {
                Card {
                    CardRow {
                        Text("The source is left unchanged, and model files are not imported.")
                            .bodyText(14, Theme.inkSecondary)
                    }
                    PrivacyCheckRow(
                        title: "Settings",
                        detail: nil,
                        fact: status.settingsImported ? "Already imported" : nil,
                        isOn: store.upstreamSelection.settings,
                        enabled: status.settingsAvailable && !store.upstreamImporting
                    ) {
                        store.setUpstreamSettings($0)
                    }
                    PrivacyCheckRow(
                        title: "History",
                        detail: nil,
                        fact: "\(status.historyEntries)",
                        isOn: store.upstreamSelection.history,
                        enabled: status.historyEntries > 0 && !store.upstreamImporting
                    ) {
                        store.setUpstreamHistory($0)
                    }
                    PrivacyCheckRow(
                        title: "Recordings",
                        detail: "Recordings are imported only with history.",
                        fact: "\(status.recordingFiles) · \(Model.bytes(UInt64(max(status.recordingBytes, 0))))",
                        isOn: store.upstreamSelection.recordings,
                        enabled: store.upstreamSelection.history
                            && status.recordingFiles > 0
                            && !store.upstreamImporting
                    ) {
                        store.setUpstreamRecordings($0)
                    }
                    upstreamNotices(status)
                    CardRow {
                        Text(store.upstreamImporting ? "Importing" : "Bring the ticked data over.")
                            .bodyText(14, Theme.inkSecondary)
                    } trailing: {
                        HStack(spacing: 12) {
                            Button("Check again") {
                                Task { await store.refreshUpstream() }
                            }
                            .buttonStyle(.secondary)
                            .disabled(store.upstreamBusy || store.upstreamImporting)
                            Button(store.upstreamImporting ? "Importing" : "Import selected data") {
                                Task { await store.startUpstreamImport() }
                            }
                            .buttonStyle(.primary)
                            .disabled(!store.upstreamImportAvailable
                                || !store.upstreamSelectionValid
                                || store.upstreamImporting)
                        }
                    }
                    if status.settingsImported && status.settingsBackupAvailable {
                        ActionRow(
                            title: "Undo the settings import",
                            detail: backupDetail(status),
                            button: "Revert settings",
                            busy: store.upstreamImporting
                        ) {
                            Task { await store.revertUpstreamSettings() }
                        }
                    }
                }
            }
        } else if let failure = store.upstreamFailure {
            PageSection("Import from legacy app") {
                Card {
                    PrivacyFailureRow(failure, retry: "Check again") {
                        Task { await store.refreshUpstream() }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func upstreamNotices(_ status: UpstreamImportStatus) -> some View {
        if status.appState == .running {
            PrivacyNoticeRow("Close the legacy app before importing.", live: true)
        }
        if status.appState == .unverifiable {
            PrivacyNoticeRow("Sona cannot verify that the legacy app is closed on this platform.", live: true)
        }
        if !store.upstreamHasData {
            PrivacyNoticeRow("No settings or history entries are available to import.", live: false)
        } else if !store.upstreamSelectionValid {
            PrivacyNoticeRow("Choose settings or history to import.", live: false)
        }
        if let progress = store.upstreamProgress {
            PrivacyNoticeRow(progress.sentence, live: false)
        }
        if let result = store.upstreamResult {
            PrivacyNoticeRow(result.sentence, live: false)
        }
        if let failure = store.upstreamFailure {
            PrivacyNoticeRow(failure, live: true)
        }
    }

    private func backupDetail(_ status: UpstreamImportStatus) -> String {
        guard let saved = status.backupDate else {
            return "Puts back the settings this Mac had before the import."
        }
        return "Puts back the settings saved \(saved.relativeDay) at \(saved.time)."
    }

    // MARK: - Identity adoption

    @ViewBuilder
    private var identitySection: some View {
        if let receipt = store.identity {
            PageSection("Adopted installation") {
                Card {
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(receipt.mode.word).bodyText()
                            Text(receipt.mode.sentence).metaText()
                        }
                    } trailing: {
                        Text("\(receipt.completed.relativeDay) at \(receipt.completed.time)").metaText()
                    }
                    if let source = receipt.sourceIdentity {
                        CardRow {
                            Text("Came from").bodyText()
                        } trailing: {
                            Text(source).metaText()
                        }
                    }
                    CardRow {
                        Text("Recorded by").bodyText()
                    } trailing: {
                        Text("Sona \(receipt.appVersion)").metaText()
                    }
                    ForEach(receipt.entries) { entry in
                        CardRow {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(entry.path).bodyText(14)
                                Text(entry.size).metaText()
                            }
                        } trailing: {
                            PrivacyFact(entry.action.word, live: entry.action == .failed)
                        }
                    }
                    ForEach(receipt.credentials) { credential in
                        CardRow {
                            Text(credential.account).bodyText(14)
                        } trailing: {
                            PrivacyFact(
                                credential.status.word,
                                live: credential.status == .needsReentry)
                        }
                    }
                    if let failure = store.identityFailure {
                        PrivacyNoticeRow(failure, live: true)
                    }
                    if receipt.canRevert {
                        ActionRow(
                            title: "Put the adopted data back",
                            detail: "Moves history, recordings and keys to the folder they came from. The legacy app must be closed.",
                            button: "Revert adoption",
                            busy: store.identityBusy
                        ) {
                            Task { await store.revertIdentity() }
                        }
                    }
                }
            }
        } else if store.isPortable {
            PageSection("Adopted installation") {
                Card {
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Portable install").bodyText()
                            Text("This copy keeps its data beside itself, so nothing was adopted from another installation.")
                                .metaText()
                        }
                    }
                }
            }
        } else if let failure = store.identityFailure {
            PageSection("Adopted installation") {
                Card {
                    PrivacyFailureRow(failure, retry: "Check again") {
                        Task { await store.refreshIdentity() }
                    }
                }
            }
        }
    }
}

/// A read-only measurement on the right of a row. Live only when the fact is
/// a refusal a reader has to act on.
private struct PrivacyFact: View {
    let text: String
    let live: Bool

    init(_ text: String, live: Bool) {
        self.text = text
        self.live = live
    }

    var body: some View {
        Text(text)
            .font(TypeScale.body(14))
            .foregroundStyle(live ? Theme.live : Theme.ink)
            .textSelection(.enabled)
    }
}

/// A sentence inside a card: the progress line, the result, a refusal.
private struct PrivacyNoticeRow: View {
    let text: String
    let live: Bool

    init(_ text: String, live: Bool) {
        self.text = text
        self.live = live
    }

    var body: some View {
        CardRow {
            Text(text).bodyText(14, live ? Theme.live : Theme.inkSecondary)
        }
    }
}

/// A failure and the one control that clears it, on one line.
private struct PrivacyFailureRow: View {
    let text: String
    let retry: String
    let action: () -> Void

    init(_ text: String, retry: String, action: @escaping () -> Void) {
        self.text = text
        self.retry = retry
        self.action = action
    }

    var body: some View {
        CardRow {
            Text(text).bodyText(14, Theme.live)
        } trailing: {
            Button(retry, action: action).buttonStyle(.compact)
        }
    }
}

/// A row that ticks one part of an import, with the count it would bring.
private struct PrivacyCheckRow: View {
    let title: String
    var detail: String?
    var fact: String?
    let isOn: Bool
    let enabled: Bool
    let change: (Bool) -> Void

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).bodyText(15, enabled ? Theme.ink : Theme.inkDisabled)
                if let detail {
                    Text(detail).metaText()
                }
            }
        } trailing: {
            HStack(spacing: 12) {
                if let fact {
                    Text(fact).metaText()
                }
                Toggle("", isOn: Binding(get: { isOn }, set: change))
                    .labelsHidden()
                    .toggleStyle(.checkbox)
                    .disabled(!enabled)
            }
        }
    }
}
