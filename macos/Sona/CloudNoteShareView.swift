import SwiftUI

/// The same encrypted, expiring links as meetings. With no note, this manages
/// all note links, even after their local notes have been deleted.
struct CloudNoteShareView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let note: ScratchNote?
    @State private var expiry = Date().addingTimeInterval(7 * 24 * 60 * 60)
    @State private var shares: [CloudShareSummary] = []
    @State private var link: CloudShareBrowserResult?
    @State private var loading = true
    @State private var busy = false
    @State private var error: String?
    @State private var copiedShareId: String?
    @State private var revoking: CloudShareSummary?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text(note == nil ? "Note links" : "Share this note").headlineText()
                Spacer()
                Button("Done") { dismiss() }.buttonStyle(.quiet)
                    .keyboardShortcut(.cancelAction)
            }
            if let note {
                Text(note.title).bodyText(15)
                Text("Anyone with the link can read this copy. Later edits do not change it.")
                    .metaText(Theme.inkSecondary)
                DatePicker("Expires", selection: $expiry, in: Date.now...Date.now.addingTimeInterval(30 * 24 * 60 * 60))
                    .datePickerStyle(.field)
                    .disabled(busy)
                HStack {
                    Spacer()
                    Button(busy ? "Working…" : "Create link") { Task { await create() } }
                        .buttonStyle(.primary)
                        .disabled(busy || loading || expiry <= .now)
                }
            }
            Text("Links stay open until they expire or you revoke them, even if you delete the note.")
                .metaText(Theme.inkSecondary)
            ErrorNote(error)
            if let link {
                VStack(alignment: .leading, spacing: 8) {
                    Text(link.shareUrl).font(TypeScale.mono(12)).textSelection(.enabled)
                    Text(link.trustDisclosure).metaText()
                    Button(copiedShareId == link.shareId ? "Link copied" : "Copy link") {
                        CloudSyncClipboard.copy(link.shareUrl)
                        copiedShareId = link.shareId
                    }.buttonStyle(.secondary)
                }
            }
            HStack {
                Text("Shared copies").bodyText(14)
                Spacer()
                Button("Refresh") { Task { await load() } }.buttonStyle(.quiet)
                    .disabled(busy || loading)
            }
            if loading {
                ProgressView("Reading links…").controlSize(.small)
            } else if shares.isEmpty, error == nil {
                Text("No links yet.").metaText()
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        ForEach(shares) { share in
                            HStack(alignment: .firstTextBaseline, spacing: 12) {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(share.state.word).bodyText(14, share.state.tone)
                                    Text(share.detail).metaText()
                                }
                                Spacer()
                                if share.state.revocable {
                                    Button(copiedShareId == share.shareId ? "Copied" : "Copy link") {
                                        Task { await copy(share) }
                                    }
                                    .buttonStyle(.quiet)
                                    .disabled(busy || Double(share.expiresAtUtcMs) <= Date.now.timeIntervalSince1970 * 1_000)
                                    Button("Revoke") { revoking = share }
                                        .buttonStyle(.secondary).disabled(busy)
                                }
                            }
                            Divider().overlay(Theme.hairline)
                        }
                    }
                }
                .frame(maxHeight: 240)
            }
            if model.cloudSync.overview?.paused == true {
                Text("Sync is paused. Resume it in Settings to publish links or finish revoking them.")
                    .metaText(Theme.inkSecondary)
            }
        }
        .padding(24)
        .frame(width: 580, alignment: .leading)
        .background(Theme.page)
        .task { await load() }
        .alert("Revoke this link?", isPresented: Binding(
            get: { revoking != nil }, set: { if !$0 { revoking = nil } }
        ), presenting: revoking) { share in
            Button("Cancel", role: .cancel) {}
            Button("Revoke", role: .destructive) { Task { await revoke(share) } }
        } message: { _ in
            Text("Anyone holding it loses access once the server confirms. Until then, it may still open.")
        }
    }

    private func load() async {
        loading = true
        defer { loading = false }
        do {
            shares = try await model.core.request("cloud_note_share_list", CloudSyncRequest(
                request: CloudNoteShareListBody(noteId: note?.id)))
            error = nil
        } catch {
            show(error)
        }
    }

    private func create() async {
        guard !busy, let note else { return }
        busy = true
        defer { busy = false }
        do {
            link = try await model.core.request("cloud_note_share_create", CloudSyncRequest(
                request: CloudNoteShareCreateBody(noteId: note.id,
                    expiresAtUtcMs: Int64(expiry.timeIntervalSince1970 * 1_000))))
            copiedShareId = nil
            await load()
        } catch {
            show(error)
        }
    }

    private func copy(_ share: CloudShareSummary) async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        do {
            let result: CloudShareBrowserResult = try await model.core.request(
                "cloud_browser_share_link", CloudSyncRequest(request: CloudShareRevokeBody(shareId: share.shareId)))
            CloudSyncClipboard.copy(result.shareUrl)
            copiedShareId = share.shareId
            error = nil
        } catch {
            show(error)
        }
    }

    private func revoke(_ share: CloudShareSummary) async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        do {
            let _: CloudSyncOverview = try await model.core.request("cloud_share_revoke",
                CloudSyncRequest(request: CloudShareRevokeBody(shareId: share.shareId)))
            if link?.shareId == share.shareId { link = nil }
            copiedShareId = nil
            await load()
        } catch {
            show(error)
        }
    }

    private func show(_ failure: Error) {
        error = (failure as? CoreError)?.remote(as: CloudSyncErrorKind.self)?.guidance
            ?? "Could not update sharing. Try again."
    }
}

private struct CloudNoteShareCreateBody: Encodable {
    let noteId: String
    let expiresAtUtcMs: Int64
    enum CodingKeys: String, CodingKey {
        case noteId = "note_id"
        case expiresAtUtcMs = "expires_at_utc_ms"
    }
}

private struct CloudNoteShareListBody: Encodable {
    let noteId: String?
    enum CodingKeys: String, CodingKey { case noteId = "note_id" }
}
