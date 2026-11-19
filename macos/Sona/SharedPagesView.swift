import SwiftUI
import CoreImage.CIFilterBuiltins

/// The Shared place: drafts waiting for him first, then every link with
/// where it stands.
struct SharedPagesView: View {
    @Environment(AppModel.self) private var model
    @State private var stopping: SharedRecord?
    @State private var discarding: SharedDraftSummary?
    @State private var copied: String?
    @State private var qr: SharedRecord?
    @State private var copyingSummary: SharedRecord?

    private var store: SharedPagesStore { model.shared }

    var body: some View {
        Page {
            PageTitle("Shared", subtitle: subtitle)
            ErrorNote(store.failure)
            if !store.drafts.isEmpty {
                PageSection("Waiting for you") {
                    Card {
                        ForEach(store.drafts) { draft in
                            draftRow(draft)
                        }
                    }
                }
            }
            if !store.loaded {
                Card { CardLine("Reading shared pages…") }
            } else if store.shares.isEmpty, store.drafts.isEmpty {
                Card { CardLine("No published pages yet. Live call links stay on their meeting.") }
            } else if !store.shares.isEmpty {
                PageSection("Links") {
                    Card {
                        ForEach(store.shares) { record in
                            linkRow(record)
                        }
                    }
                }
            }
        }
        .task { await store.refresh() }
        .task(id: store.shares.contains { $0.phase.settling }) {
            guard store.shares.contains(where: { $0.phase.settling }) else { return }
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(10))
                guard !Task.isCancelled else { return }
                await store.refresh()
            }
        }
        .alert("Stop sharing this page?", isPresented: stopBinding, presenting: stopping) { record in
            Button("Cancel", role: .cancel) {}
            Button("Stop sharing", role: .destructive) {
                Task { await store.revoke(record.slug) }
            }
        } message: { _ in
            Text("The page goes offline within about a minute. People who already opened it may have kept a copy.")
        }
        .alert("Discard this draft?", isPresented: discardBinding, presenting: discarding) { draft in
            Button("Cancel", role: .cancel) {}
            Button("Discard", role: .destructive) {
                Task { await store.discard(draft.meetingId) }
            }
        } message: { _ in
            Text("Its edits are deleted. The meeting itself stays as it is.")
        }
        .alert("Copy the link with its first line?", item: $copyingSummary) { record in
            Button("Cancel", role: .cancel) {}
            Button("Copy") {
                if let url = record.url {
                    CloudSyncClipboard.copy("\(record.blurb ?? "")\n\(url)")
                }
            }
        } message: { record in
            Text("\"\(record.blurb ?? "")\" and the link go into the chat. This line stays in the chat even after the page is taken down.")
        }
    }

    private var subtitle: String {
        guard let site = store.site else { return "Meeting pages only people with the link can read" }
        return "Meeting pages at \(site.place), readable only with the link"
    }

    private var stopBinding: Binding<Bool> {
        Binding(get: { stopping != nil }, set: { if !$0 { stopping = nil } })
    }

    private var discardBinding: Binding<Bool> {
        Binding(get: { discarding != nil }, set: { if !$0 { discarding = nil } })
    }

    private func draftRow(_ draft: SharedDraftSummary) -> some View {
        CardRow(action: { model.sheet = .share(draft.meetingId) }) {
            VStack(alignment: .leading, spacing: 4) {
                Text(draft.title ?? "Untitled meeting").bodyText().lineLimit(1)
                Text(draftMeta(draft)).metaText(Theme.inkSecondary)
            }
        } trailing: {
            Button("Discard") { discarding = draft }
                .buttonStyle(.quiet)
                .disabled(store.working == draft.meetingId)
        }
    }

    private func draftMeta(_ draft: SharedDraftSummary) -> String {
        var parts: [String] = []
        if let preparedBy = draft.preparedBy, !preparedBy.isEmpty {
            parts.append("Prepared by \(preparedBy)")
        }
        if let saved = SharedClock.date(draft.savedAt) {
            parts.append("Saved \(saved.short) at \(saved.time)")
        }
        return parts.isEmpty ? "Not shared yet" : parts.joined(separator: " · ")
    }

    private func linkRow(_ record: SharedRecord) -> some View {
        let phase = record.phase
        return CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(record.title ?? "Untitled meeting").bodyText().lineLimit(1)
                if let forPerson = record.forPerson, !forPerson.isEmpty {
                    Text("for \(forPerson)").metaText(Theme.accent)
                }
                Text(record.contents).metaText(Theme.inkSecondary)
                Text(record.dates).metaText()
                if phase == .failed, let error = record.error {
                    Text(error).metaText(Theme.live).textSelection(.enabled)
                }
            }
        } trailing: {
            HStack(spacing: 14) {
                Text(phase.word).metaText(phase.tone)
                if let url = record.url, phase.stoppable {
                    Button(copied == record.slug ? "Copied" : "Copy link") {
                        CloudSyncClipboard.copy(url)
                        copied = record.slug
                    }
                    .buttonStyle(.quiet)
                    Button("Copy with summary") {
                        if let blurb = record.blurb, !blurb.isEmpty {
                            copyingSummary = record
                        } else {
                            CloudSyncClipboard.copy(url)
                        }
                    }
                    .buttonStyle(.quiet)
                    Button { qr = record } label: {
                        Image(systemName: "qrcode")
                    }
                    .buttonStyle(.icon)
                    .help("Show QR code")
                    .accessibilityLabel("Show QR code")
                    .popover(item: $qr) { record in
                        if let url = record.url {
                            ShareQRView(url: url)
                        }
                    }
                    if let link = URL(string: url) {
                        Button("Open") { NSWorkspace.shared.open(link) }
                            .buttonStyle(.quiet)
                    }
                }
                if phase.stoppable {
                    Button("Stop sharing") { stopping = record }
                        .buttonStyle(.compact)
                        .disabled(store.working == record.slug)
                }
            }
        }
    }
}

/// Where pages go: the private repository Sona pushes to and the address
/// its Pages site serves. Shown until both are saved.
struct SharedSiteSetup: View {
    @Environment(AppModel.self) private var model
    @State private var remote = ""
    @State private var address = ""

    private var ready: Bool {
        remote.trimmingCharacters(in: .whitespaces).hasPrefix("https://")
            && address.trimmingCharacters(in: .whitespaces).hasPrefix("https://")
    }

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 10) {
                InputField(prompt: "GitHub repository, like https://github.com/you/pages.git", text: $remote)
                InputField(prompt: "Page address, like https://example.com/m", text: $address)
                if let failure = model.shared.siteFailure {
                    Text(failure).metaText(Theme.live).textSelection(.enabled)
                }
                HStack {
                    Spacer()
                    Button(model.shared.savingSite ? "Saving…" : "Save") {
                        Task { await model.shared.saveSite(remote: remote, address: address) }
                    }
                    .buttonStyle(.secondary)
                    .disabled(!ready || model.shared.savingSite)
                }
            }
            .padding(16)
        }
    }
}

/// A QR code of the full link, crisp squares on a white quiet zone.
struct ShareQRView: View {
    let url: String

    var body: some View {
        VStack {
            if let image = ShareQRView.image(url) {
                Image(nsImage: image)
                    .interpolation(.none)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(width: 200, height: 200)
            }
        }
        .padding(16)
    }

    private static func image(_ url: String) -> NSImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(url.utf8)
        filter.correctionLevel = "M"
        guard let code = filter.outputImage else { return nil }
        // Readers need four modules of white around the code; one module is one unit here.
        let framed = code.composited(over: CIImage(color: .white).cropped(to: code.extent.insetBy(dx: -4, dy: -4)))
        let scale = 400 / framed.extent.width
        let scaled = framed.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        guard let cg = CIContext().createCGImage(scaled, from: scaled.extent) else { return nil }
        return NSImage(cgImage: cg, size: NSSize(width: 400, height: 400))
    }
}
