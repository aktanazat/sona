import AppKit
import SwiftUI

/// Cloud sync, as sections for Settings > Advanced: the line a reader checks
/// on the way past, the one-time tasks folded away until they are wanted, and
/// what each meeting is doing with the vault.
struct CloudSyncView: View {
    let store: CloudSyncStore
    /// The integrator's way to a meeting's own page, so a conflicted meeting
    /// is one press from the transcript it disagrees about.
    var openMeeting: (String) -> Void = { _ in }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ErrorNote(store.error)
            account
            tasks
            meetings
        }
    }

    // MARK: the account

    private var account: some View {
        PageSection("Cloud sync") {
            Card {
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(store.accountStatus.word).bodyText(15, store.accountStatus.tone)
                        Text(store.service?.reason ?? "Reading this device's configuration…").metaText()
                    }
                } trailing: {
                    HStack(spacing: 12) {
                        if let overview = store.overview, overview.enabled, overview.queuedObjects > 0 {
                            Chip("\(overview.queuedObjects) pending")
                        }
                        if let overview = store.overview, overview.enabled {
                            Button(overview.paused ? "Resume sync" : "Pause sync") {
                                Task { await store.togglePaused() }
                            }
                            .buttonStyle(.secondary)
                            .disabled(store.busy)
                        }
                    }
                }
                if let kind = store.overview?.terminalError {
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Needs attention").bodyText(15, Theme.live)
                            Text(kind.guidance).metaText()
                        }
                    }
                }
                CardRow {
                    Text("Server").bodyText()
                } trailing: {
                    Text(store.service?.endpoint ?? "None configured")
                        .metaText()
                        .textSelection(.enabled)
                }
                if store.overview?.portableMode == true {
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Portable mode").bodyText()
                            Text(CloudSyncErrorKind.portableUnavailable.guidance).metaText()
                        }
                    }
                }
                if let deletions = store.overview?.pendingDeletions, deletions > 0 {
                    CardRow {
                        Text("Removals waiting to reach the vault").bodyText()
                    } trailing: {
                        Text("\(deletions)").metaText()
                    }
                }
            }
        }
    }

    // MARK: setup, recovery, pairing

    private var tasks: some View {
        PageSection("Setup and pairing") {
            Card {
                if let code = store.recoveryCode {
                    // Shown once and never again, so it cannot live behind a
                    // disclosure: the row states the code, and the one
                    // sentence that earns its place says why there is no
                    // second chance.
                    CloudSyncBlock(label: "Save this recovery code now") {
                        VStack(alignment: .leading, spacing: 10) {
                            CloudSyncCodeText(text: code)
                            HStack(spacing: 12) {
                                Text("Sona shows this code once and does not store it.")
                                    .metaText(Theme.accent)
                                Spacer()
                                Button("Copy") { CloudSyncClipboard.copy(code) }
                                    .buttonStyle(.quiet)
                            }
                        }
                    }
                }
                CloudSyncDisclosure(
                    label: "Set up sync",
                    detail: "Deploy your companion Worker first, then use its address and setup secret."
                ) {
                    setupFields
                }
                CloudSyncDisclosure(
                    label: "Recover a sync account",
                    detail: "Use the recovery code from the device that set the vault up."
                ) {
                    recoveryFields
                }
                CloudSyncDisclosure(
                    label: "Pair a device",
                    detail: "Add an iPhone or Watch to this vault, or join a vault another device owns."
                ) {
                    pairingFields
                }
            }
        }
    }

    @ViewBuilder
    private var setupFields: some View {
        @Bindable var store = store
        CloudSyncFieldRow(label: "Server address") {
            InputField(prompt: "https://sona.example.workers.dev", text: $store.endpoint)
        }
        CloudSyncFieldRow(label: "Setup secret") {
            CloudSyncSecretField(prompt: "Setup secret", text: $store.bootstrapSecret)
        }
        CloudSyncTaskAction {
            Button("Set up sync") { Task { await store.bootstrap() } }
                .buttonStyle(.primary)
                .disabled(store.busy || blank(store.endpoint) || store.bootstrapSecret.isEmpty)
        }
    }

    @ViewBuilder
    private var recoveryFields: some View {
        @Bindable var store = store
        CloudSyncFieldRow(label: "Server address") {
            InputField(prompt: "https://sona.example.workers.dev", text: $store.endpoint)
        }
        CloudSyncFieldRow(label: "Recovery code") {
            CloudSyncSecretField(prompt: "Recovery code", text: $store.recoveryInput)
        }
        CloudSyncTaskAction {
            Button("Recover account") { Task { await store.recover() } }
                .buttonStyle(.secondary)
                .disabled(store.busy || blank(store.endpoint) || store.recoveryInput.isEmpty)
        }
    }

    @ViewBuilder
    private var pairingFields: some View {
        @Bindable var store = store
        CloudSyncFieldRow(label: "Server address") {
            InputField(prompt: "https://sona.example.workers.dev", text: $store.endpoint)
        }
        CloudSyncFieldRow(label: "Vault ID") {
            InputField(prompt: "Vault ID", text: $store.vaultId)
        }
        CloudSyncTaskAction {
            Button("Create pairing offer") { Task { await store.createOffer() } }
                .buttonStyle(.secondary)
                .disabled(store.busy || blank(store.endpoint) || blank(store.vaultId))
        }
        if let offer = store.offer {
            PairingOfferCard(offer: offer, busy: store.busy) {
                Task { await store.approveOffer() }
            }
        }
        PairingApproveField(store: store)
        CloudSyncBlock(label: "Offer from another device") {
            VStack(alignment: .leading, spacing: 10) {
                CloudSyncCodeField(
                    text: $store.receivedOffer,
                    prompt: "Paste the offer that device created")
                HStack(spacing: 12) {
                    Button("Paste") { store.receivedOffer = CloudSyncClipboard.text() }
                        .buttonStyle(.quiet)
                    Spacer()
                    Button("Accept offer") { Task { await store.acceptOffer() } }
                        .buttonStyle(.secondary)
                        .disabled(store.busy || blank(store.endpoint) || blank(store.receivedOffer))
                }
            }
        }
    }

    // MARK: the meetings

    private var meetings: some View {
        PageSection("Meetings") {
            Card {
                ActionRow(
                    title: "Import a .sona file",
                    detail: store.importedSessionId.map { "Imported meeting \($0)" },
                    button: "Choose a file",
                    busy: store.busy
                ) {
                    Task { await store.importBundle() }
                }
                if let note = store.statusNote {
                    CloudSyncNote(note, tone: Theme.inkTertiary)
                } else if store.statuses.isEmpty {
                    CloudSyncNote("No meetings have reached the vault yet.", tone: Theme.inkTertiary)
                } else {
                    ScrollView {
                        LazyVStack(spacing: 0) {
                            ForEach(store.statuses) { status in
                                CloudSyncMeetingRow(store: store, status: status, openMeeting: openMeeting)
                            }
                        }
                    }
                    .scrollIndicators(.never)
                    .frame(maxHeight: 460)
                }
            }
        }
    }

    private func blank(_ text: String) -> Bool {
        text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}

// MARK: - one meeting

/// Sharing a meeting is a task, not a setting, so it stays one row until it
/// is asked for — with the sync state on that row, because that is the reason
/// a reader would open it.
private struct CloudSyncMeetingRow: View {
    let store: CloudSyncStore
    let status: CloudSyncMeetingStatus
    let openMeeting: (String) -> Void
    @State private var open = false
    @State private var confirmingRevoke = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            CardRow(action: toggle) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(store.title(for: status.sessionId)).bodyText()
                    Text(facts).metaText()
                }
            } trailing: {
                HStack(spacing: 10) {
                    Text(status.state.word).metaText(status.state.tone)
                    Image(systemName: open ? "chevron.up" : "chevron.down")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.inkTertiary)
                }
            }
            if open {
                actions
                share
            }
        }
        .sheet(isPresented: $confirmingRevoke) {
            CloudShareRevokeSheet(busy: store.busy) {
                confirmingRevoke = false
                Task { await store.revokeBrowserShare() }
            } cancel: {
                confirmingRevoke = false
            }
        }
    }

    /// What the state word cannot say: how many shares exist, and whether a
    /// retry is already scheduled.
    private var facts: String {
        var parts = ["\(status.shareCount) \(status.shareCount == 1 ? "share" : "shares")"]
        if let retryAt = status.retryAt {
            parts.append("retry at \(CloudSyncClock.moment(retryAt))")
        }
        if let revision = status.remoteRevisionId {
            parts.append("cloud copy \(revision)")
        }
        return parts.joined(separator: " · ")
    }

    private var actions: some View {
        CloudSyncTaskAction {
            Button("Open meeting") { openMeeting(status.sessionId) }
                .buttonStyle(.quiet)
            Spacer()
            if status.state.retryable {
                Button("Retry sync") { Task { await store.retry(status.sessionId) } }
                    .buttonStyle(.secondary)
                    .disabled(store.busy)
            }
            if status.state == .conflict {
                Button("Keep this device's copy") {
                    Task { await store.resolveConflict(status.sessionId, choice: .keepLocal) }
                }
                .buttonStyle(.secondary)
                .disabled(store.busy)
                Button("Use the cloud copy") {
                    Task { await store.resolveConflict(status.sessionId, choice: .useRemote) }
                }
                .buttonStyle(.secondary)
                .disabled(store.busy)
            }
        }
    }

    @ViewBuilder
    private var share: some View {
        @Bindable var store = store
        CloudSyncFieldRow(label: "Share expiry") {
            DatePicker(
                "",
                selection: $store.shareExpiry,
                in: Date.now...,
                displayedComponents: [.date, .hourAndMinute]
            )
            .labelsHidden()
            .datePickerStyle(.field)
        }
        CloudSyncTaskAction {
            Spacer()
            Button("Export .sona file") { Task { await store.exportBundle(status.sessionId) } }
                .buttonStyle(.secondary)
                .disabled(store.busy)
            Button("Create browser share") { Task { await store.createBrowserShare(status.sessionId) } }
                .buttonStyle(.secondary)
                .disabled(store.busy)
        }
        if let bundle = store.bundle, bundle.sessionId == status.sessionId {
            CloudSyncNote(
                "Saved to \(bundle.result.filePath) · expires \(CloudSyncClock.moment(bundle.result.expiresAtUtcMs))",
                tone: Theme.inkSecondary)
        }
        if let link = store.browserShare, link.sessionId == status.sessionId {
            CloudShareLinkBlock(link: link.result, busy: store.busy) {
                confirmingRevoke = true
            }
        }
    }

    private func toggle() {
        open.toggle()
        if open {
            Task { await store.refreshStatus(status.sessionId) }
        }
    }
}

/// The link, what its viewer can see, and the one action here that has to be
/// confirmed.
private struct CloudShareLinkBlock: View {
    let link: CloudShareBrowserResult
    let busy: Bool
    let revoke: () -> Void

    var body: some View {
        CloudSyncBlock(label: "Browser share") {
            VStack(alignment: .leading, spacing: 10) {
                if let url = URL(string: link.shareUrl) {
                    Link(link.shareUrl, destination: url)
                        .font(TypeScale.mono(12))
                        .foregroundStyle(Theme.accent)
                } else {
                    CloudSyncCodeText(text: link.shareUrl)
                }
                Text("Expires \(CloudSyncClock.moment(link.expiresAtUtcMs))").metaText()
                Text(link.trustDisclosure).metaText(Theme.inkSecondary)
                HStack(spacing: 12) {
                    Button("Copy link") { CloudSyncClipboard.copy(link.shareUrl) }
                        .buttonStyle(.quiet)
                    Spacer()
                    Button("Revoke browser share", action: revoke)
                        .buttonStyle(.secondary)
                        .disabled(busy)
                }
            }
        }
    }
}

/// Revoking kills a link other people already hold, and nothing in the button
/// says so. The dialog states the consequence, so the row does not.
private struct CloudShareRevokeSheet: View {
    let busy: Bool
    let revoke: () -> Void
    let cancel: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Revoke browser share").headlineText()
            Text("Anyone holding this link loses access the moment you revoke it.")
                .bodyText(14, Theme.inkSecondary)
            HStack(spacing: 12) {
                Spacer()
                Button("Cancel", action: cancel).buttonStyle(.secondary)
                Button("Revoke", action: revoke).buttonStyle(.primary).disabled(busy)
            }
        }
        .padding(24)
        .frame(width: 420, alignment: .leading)
        .background(Theme.page)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusDialog))
    }
}

// MARK: - pairing

/// The record this Mac minted, in both the forms a phone can read it: the
/// code to scan, and the text to type. The fingerprint beside it is what the
/// phone must be showing before this offer is approved.
private struct PairingOfferCard: View {
    let offer: PairingOffer
    let busy: Bool
    let approve: () -> Void
    @State private var payload = ""
    @State private var code: NSImage?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .top, spacing: 20) {
                if let code {
                    // `.none`: a resampled code is a code a phone misreads.
                    Image(nsImage: code)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 180, height: 180)
                        .accessibilityLabel(Text("Pairing code for device \(offer.deviceId)"))
                }
                VStack(alignment: .leading, spacing: 8) {
                    Text("Scan this on the device, or type the code below into it.").metaText()
                    PairingFact(label: "Fingerprint", value: offer.fingerprint)
                    PairingFact(label: "Vault", value: offer.vaultId)
                    PairingFact(label: "Device", value: offer.deviceId)
                    PairingFact(label: "Expires", value: CloudSyncClock.moment(offer.expiresAt))
                }
            }
            CloudSyncCodeText(text: payload)
            HStack(spacing: 12) {
                Button("Copy code") { CloudSyncClipboard.copy(payload) }
                    .buttonStyle(.quiet)
                Spacer()
                Button("Approve this offer", action: approve)
                    .buttonStyle(.secondary)
                    .disabled(busy)
            }
        }
        .padding(20)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { Hairline() }
        .task(id: offer) {
            let text = offer.json
            payload = text
            code = PairingCode.image(text, side: 180)
        }
    }
}

private struct PairingFact: View {
    let label: String
    let value: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(label).metaText()
            Text(value)
                .font(TypeScale.mono(12))
                .foregroundStyle(Theme.ink)
                .textSelection(.enabled)
        }
    }
}

/// Approving a device that cannot be handed a vault root any other way: it
/// shows its code, the operator pastes it here, and the fingerprint beside
/// the label is what this Mac read out of that paste — never the one the
/// paste carried. Comparing it against the device is the whole check, which
/// is why the line asking for that comparison sits under the field.
private struct PairingApproveField: View {
    let store: CloudSyncStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            CloudSyncBlock(
                label: "Code from the device you are adding",
                fact: store.candidate.derivedFingerprint
            ) {
                VStack(alignment: .leading, spacing: 10) {
                    CloudSyncCodeField(
                        text: Binding(get: { store.candidateOffer }, set: { store.readCandidate($0) }),
                        prompt: "Paste the code the device shows")
                    if let line = store.candidate.line {
                        Text(line).metaText(store.candidate.tone)
                    }
                    HStack(spacing: 12) {
                        Button("Paste") { store.readCandidate(CloudSyncClipboard.text()) }
                            .buttonStyle(.quiet)
                        Spacer()
                        Button("Approve device") { Task { await store.approveCandidate() } }
                            .buttonStyle(.secondary)
                            .disabled(store.busy || store.candidate.offer == nil)
                    }
                }
            }
        }
    }
}

// MARK: - the parts a settings card is made of

/// A one-time task, folded away until it is wanted.
private struct CloudSyncDisclosure<Content: View>: View {
    let label: String
    var detail: String?
    @ViewBuilder var content: () -> Content
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            CardRow(action: { open.toggle() }) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(label).bodyText()
                    if let detail {
                        Text(detail).metaText()
                    }
                }
            } trailing: {
                Image(systemName: open ? "chevron.up" : "chevron.down")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Theme.inkTertiary)
            }
            if open {
                content()
            }
        }
    }
}

/// A labelled control on its own row: "Server address — [input]".
private struct CloudSyncFieldRow<Control: View>: View {
    let label: String
    @ViewBuilder var control: () -> Control

    var body: some View {
        CardRow {
            Text(label).bodyText()
        } trailing: {
            control().frame(width: 280)
        }
    }
}

/// A label with something wide under it: a code, a link, a paste field. The
/// fact sits at the end of the label's line, where the old panel put it.
private struct CloudSyncBlock<Content: View>: View {
    let label: String
    var fact: String?
    @ViewBuilder var content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text(label).bodyText()
                Spacer(minLength: 16)
                if let fact {
                    Text(fact)
                        .font(TypeScale.mono(12))
                        .foregroundStyle(Theme.ink)
                        .textSelection(.enabled)
                }
            }
            content()
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { Hairline() }
    }
}

/// The action a task's fields lead up to, on its own row.
private struct CloudSyncTaskAction<Content: View>: View {
    @ViewBuilder var content: () -> Content

    var body: some View {
        HStack(spacing: 12) {
            content()
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .trailing)
        .overlay(alignment: .bottom) { Hairline() }
    }
}

/// One quiet sentence on its own row: why a list is empty, where a file went.
private struct CloudSyncNote: View {
    let text: String
    let tone: Color

    init(_ text: String, tone: Color) {
        self.text = text
        self.tone = tone
    }

    var body: some View {
        Text(text)
            .metaText(tone)
            .textSelection(.enabled)
            .padding(.horizontal, 20)
            .padding(.vertical, 14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .overlay(alignment: .bottom) { Hairline() }
    }
}

/// A code as the core stated it: monospaced, selectable, never editable.
private struct CloudSyncCodeText: View {
    let text: String

    var body: some View {
        Text(text)
            .font(TypeScale.mono(12))
            .foregroundStyle(Theme.ink)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(10)
            .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
    }
}

/// The field a code arrives in.
private struct CloudSyncCodeField: View {
    @Binding var text: String
    let prompt: String

    var body: some View {
        TextEditor(text: $text)
            .font(TypeScale.mono(12))
            .foregroundStyle(Theme.ink)
            .scrollContentBackground(.hidden)
            .padding(8)
            .frame(height: 92)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
            .overlay(alignment: .topLeading) {
                if text.isEmpty {
                    Text(prompt)
                        .font(TypeScale.mono(12))
                        .foregroundStyle(Theme.inkTertiary)
                        .padding(.horizontal, 13)
                        .padding(.vertical, 16)
                        .allowsHitTesting(false)
                }
            }
    }
}

/// A secret that is typed once and never read back.
private struct CloudSyncSecretField: View {
    let prompt: String
    @Binding var text: String

    var body: some View {
        SecureField(prompt, text: $text, prompt: Text(prompt).foregroundStyle(Theme.inkTertiary))
            .textFieldStyle(.plain)
            .font(TypeScale.body())
            .foregroundStyle(Theme.ink)
            .padding(.horizontal, 12)
            .frame(height: 36)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// The clipboard, for the codes that move between this Mac and a phone.
enum CloudSyncClipboard {
    static func copy(_ text: String) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
    }

    static func text() -> String {
        NSPasteboard.general.string(forType: .string) ?? ""
    }
}
