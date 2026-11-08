import SwiftUI

/// Privacy: what Sona reads from your other apps, what can leave this Mac,
/// how history is kept, and the two one-time moves that brought data here.
///
/// The page states facts before it offers switches. Every route that can carry
/// anything off the machine is one compact line, and a route whose state could
/// not be read says so rather than guessing.
struct PrivacyView: View {
    let store: PrivacyStore

    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var confirmingRevert = false

    private static let storageAnchor = "history-storage"

    var body: some View {
        Page {
            ScrollViewReader { scroller in
                VStack(alignment: .leading, spacing: 0) {
                    PageTitle("Privacy", subtitle: "What Sona reads, what leaves this Mac, and what it keeps.")
                    ErrorNote(store.error)
                    holdingsSection {
                        withAnimation(reduceMotion ? nil : .smooth) {
                            scroller.scrollTo(Self.storageAnchor, anchor: .top)
                        }
                    }
                    contextSection
                    diagnosticsSection
                    externalSection
                    egressSection
                    storageSection.id(Self.storageAnchor)
                    upstreamSection
                    identitySection
                }
            }
        }
        .task { await store.refreshInventory() }
        .alert("Put the adopted data back?", isPresented: $confirmingRevert) {
            Button("Revert and quit", role: .destructive) {
                Task { await store.revertIdentity() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                "Recordings, models and provider keys move back to the legacy app. "
                    + "The history and settings Sona has now are kept in a backup folder inside Sona's data folder, so nothing is lost. "
                    + "Sona quits when it is done.")
        }
        // Nothing else is safe after a rollback: the core still holds the
        // moved files open, so the one button is the quit.
        .alert(
            "The data is back",
            isPresented: Binding(
                get: { store.identityReverted != nil },
                set: { _ in }),
            presenting: store.identityReverted
        ) { _ in
            Button("Quit Sona") { NSApplication.shared.terminate(nil) }
        } message: { receipt in
            Text(receipt.sentence)
        }
        .alert(
            "Delete older dictations to import?",
            isPresented: Binding(
                get: { store.upstreamRemovalsToConfirm != nil },
                set: { if !$0 { store.declineUpstreamRemovals() } }),
            presenting: store.upstreamRemovalsToConfirm
        ) { removals in
            Button("Import anyway", role: .destructive) {
                Task { await store.startUpstreamImport(removalsAllowed: removals) }
            }
            Button("Cancel", role: .cancel) {}
        } message: { removals in
            Text(removalsSentence(removals))
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
                    detail: "Allow a frontmost browser page URL to be captured when a mode and the ceiling allow it. A meeting's list of open windows also keeps full page addresses.",
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
                CardLine("Nothing leaves this Mac except on the routes listed here, and provider keys stay in the system credential store, never in Sona's settings.")
                if store.cleanupRoute == .failed {
                    PrivacyFailureRow(
                        "AI cleanup setup could not be checked.",
                        retry: "Retry"
                    ) {
                        Task { await store.retryEgress() }
                    }
                } else {
                    CardRow {
                        Text("AI cleanup").bodyText()
                    } trailing: {
                        PrivacyFact(store.cleanupRoute.fact, live: false)
                    }
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
                    CardLine("For a cloud-enabled mode, Sona sends the recorded audio, the selected language, and the words that mode lists directly to the provider. What Sona reads from your other apps, and your keys, stay on this Mac.")
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
                    CardLine("Reading how history is stored…")
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
                    CardLine("The source is left unchanged, and model files are not imported.")
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

    private func removalsSentence(_ removals: Int) -> String {
        let oldest = removals == 1
            ? "the oldest dictation that isn't saved"
            : "the \(removals) oldest dictations that aren't saved"
        return "This import goes over Dictations to keep, so Sona will delete \(oldest). "
            + "To keep everything, cancel and raise Dictations to keep in Library first."
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
                            detail: "Moves recordings, models and keys to the folder they came from, and keeps what Sona wrote since. The legacy app must be closed.",
                            button: "Revert adoption",
                            busy: store.identityBusy
                        ) {
                            confirmingRevert = true
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

// MARK: - What this Mac holds

extension PrivacyView {
    fileprivate func holdingsSection(showStorage: @escaping () -> Void) -> some View {
        PageSection("On this Mac") {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 8)], alignment: .leading, spacing: 8) {
                ForEach(holdingTiles(showStorage: showStorage)) { tile in
                    HoldingTileView(tile: tile)
                }
            }
        }
    }

    private func holdingTiles(showStorage: @escaping () -> Void) -> [HoldingTile] {
        let meetings = part(\.meetings)
        let measuredAt = store.inventory?.measuredAtUtcMs ?? 0
        return [
            HoldingTile(label: "Recordings", meetings, open: { model.go(.meetings) }) { held in
                let length = HoldingFormat.length(held.recordings.durationMs)
                return HoldingFigure(length.value, unit: length.unit, detail: Model.bytes(held.recordings.bytes))
            },
            HoldingTile(label: "Transcripts", meetings, open: { model.go(.meetings) }) { held in
                HoldingFigure(held.transcripts.formatted())
            },
            HoldingTile(label: "Dictations", part(\.dictations), open: { model.go(.library) }) { held in
                HoldingFigure(held.count.formatted(), detail: Model.bytes(held.bytes))
            },
            HoldingTile(label: "Notes", part(\.notes), open: { model.go(.scratchpad) }) { held in
                HoldingFigure(held.count.formatted())
            },
            HoldingTile(label: "Voiceprints", meetings, open: { model.go(.people) }) { held in
                let prints = held.voiceprints
                let voice = HoldingFormat.length(prints.sampleMs)
                let detail = [
                    prints.people == 0 ? nil : "\(voice.value) \(voice.unit) of voice",
                    prints.speakers == 0 ? nil : "\(prints.speakers.formatted()) from meetings",
                ].compactMap { $0 }
                return HoldingFigure(
                    (prints.people + prints.speakers).formatted(),
                    detail: detail.isEmpty ? nil : detail.joined(separator: " · "))
            },
            HoldingTile(label: "Portraits", meetings, open: { model.go(.people) }) { held in
                let cards = held.contactCards
                let detail = [
                    Model.bytes(held.portraits.bytes),
                    cards == 0 ? nil : "\(cards.formatted()) contact \(cards == 1 ? "card" : "cards")",
                ].compactMap { $0 }
                return HoldingFigure(held.portraits.count.formatted(), detail: detail.joined(separator: " · "))
            },
            HoldingTile(label: "Purging", meetings, open: { model.showSettings(.meetings) }) { held in
                HoldingFigure(held.purgingWithinWeek.formatted(), detail: "Next 7 days")
            },
            HoldingTile(label: "Encrypted", whole, open: showStorage) { inventory in
                inventory.historyEncrypted
                    ? HoldingFigure("Yes")
                    : HoldingFigure("No", detail: "Dictation history", tone: Theme.live)
            },
            HoldingTile(label: "Backup", whole, open: { model.showSettings(.sync) }) { inventory in
                let upload = meetings.flatMap(\.value)?.lastCloudUploadAtUtcMs
                return HoldingFigure(
                    inventory.cloudBackup.word,
                    detail: upload.map { "Last \(HoldingFormat.ago($0, now: measuredAt).phrase)" })
            },
            HoldingTile(label: "Phone", part(\.phoneSnapshot), open: { model.showSettings(.sync) }) { held in
                guard let published = held.lastPublishedAtUtcMs else { return HoldingFigure("Never") }
                let ago = HoldingFormat.ago(published, now: measuredAt)
                return HoldingFigure(ago.value, unit: ago.unit, detail: "Last snapshot")
            },
        ]
    }

    /// Nil while the first read is out; a read that failed makes every part
    /// unreadable rather than zero.
    private func part<Value>(_ read: (PrivacyInventory) -> Holding<Value>) -> Holding<Value>? {
        if store.inventoryFailed { return .unavailable(.unavailable) }
        return store.inventory.map(read)
    }

    private var whole: Holding<PrivacyInventory>? {
        part { .available($0) }
    }
}

private extension Holding {
    var value: Value? {
        if case let .available(value) = self { return value }
        return nil
    }
}

private extension CloudBackup {
    var word: String {
        switch self {
        case .off: "Off"
        case .paused: "Paused"
        case .on: "On"
        }
    }
}

private struct HoldingFigure {
    let value: String
    var unit: String?
    var detail: String?
    var tone: Color = Theme.ink

    init(_ value: String, unit: String? = nil, detail: String? = nil, tone: Color = Theme.ink) {
        self.value = value
        self.unit = unit
        self.detail = detail
        self.tone = tone
    }
}

private struct HoldingTile: Identifiable {
    let label: String
    let figure: HoldingFigure
    let open: () -> Void

    var id: String { label }

    /// A dash while the first read is out; a dash and the reason when the
    /// part could not be read; a printed zero only when there is nothing.
    init<Value>(
        label: String, _ holding: Holding<Value>?, open: @escaping () -> Void,
        figure: (Value) -> HoldingFigure
    ) {
        self.label = label
        self.open = open
        switch holding {
        case nil:
            self.figure = HoldingFigure("–", tone: Theme.inkDisabled)
        case let .unavailable(gap):
            self.figure = HoldingFigure("—", detail: gap.word, tone: Theme.inkDisabled)
        case let .available(value):
            self.figure = figure(value)
        }
    }
}

private enum HoldingFormat {
    /// Minutes under an hour, hours above it.
    static func length(_ milliseconds: Double) -> (value: String, unit: String) {
        let minutes = Int(milliseconds / 60_000)
        if minutes < 60 { return (minutes.formatted(), "min") }
        let hours = milliseconds / 3_600_000
        return hours < 10
            ? (hours.formatted(.number.precision(.fractionLength(1))), "h")
            : (Int(hours).formatted(), "h")
    }

    static func ago(_ at: Double, now: Double) -> (value: String, unit: String?, phrase: String) {
        let minutes = Int(max(0, now - at) / 60_000)
        if minutes < 1 { return ("Now", nil, "just now") }
        if minutes < 60 { return ("\(minutes)", "min ago", "\(minutes) min ago") }
        let hours = minutes / 60
        if hours < 48 { return ("\(hours)", "h ago", "\(hours) h ago") }
        let days = hours / 24
        return ("\(days)", "d ago", "\(days) d ago")
    }
}

private struct HoldingTileView: View {
    let tile: HoldingTile
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let figure = tile.figure
        Button(action: tile.open) {
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(figure.value)
                        .font(TypeScale.hero)
                        .monospacedDigit()
                        .foregroundStyle(figure.tone)
                        .contentTransition(.numericText())
                    if let unit = figure.unit {
                        Text(unit)
                            .font(TypeScale.label(13))
                            .foregroundStyle(Theme.inkSecondary)
                    }
                }
                Text(tile.label)
                    .font(TypeScale.label(13))
                    .foregroundStyle(Theme.inkSecondary)
                Text(figure.detail ?? " ")
                    .metaText()
                    .monospacedDigit()
            }
            .lineLimit(1)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .buttonStyle(HoldingTileStyle())
        .animation(reduceMotion ? nil : .smooth(duration: 0.3), value: figure.value)
        .accessibilityLabel(tile.label)
        .accessibilityValue([figure.value, figure.unit, figure.detail].compactMap { $0 }.joined(separator: " "))
    }
}

private struct HoldingTileStyle: ButtonStyle {
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .padding(14)
            .background(
                configuration.isPressed ? Theme.selection : hovering ? Theme.inset : Theme.surface,
                in: RoundedRectangle(cornerRadius: Theme.radiusCard))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard).strokeBorder(Theme.border, lineWidth: 1))
            .contentShape(RoundedRectangle(cornerRadius: Theme.radiusCard))
            .onHover { hovering = $0 }
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
            .animation(.easeOut(duration: 0.12), value: hovering)
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
            .font(TypeScale.body(13))
            .foregroundStyle(live ? Theme.live : Theme.inkSecondary)
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
                Text(title).bodyText(14, enabled ? Theme.ink : Theme.inkDisabled)
                if let detail {
                    Text(detail).metaText()
                }
            }
        } trailing: {
            HStack(spacing: 12) {
                if let fact {
                    Text(fact).metaText()
                }
                Toggle(title, isOn: Binding(get: { isOn }, set: change))
                    .labelsHidden()
                    .toggleStyle(.checkbox)
                    .disabled(!enabled)
            }
        }
    }
}
