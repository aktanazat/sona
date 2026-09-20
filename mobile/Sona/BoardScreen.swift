import SwiftUI
import UIKit

/// The catalogue: every thought the sorter has filed, in its cluster's colour, and the
/// ones still waiting at the top.
struct BoardScreen: View {
    @ObservedObject var model: AppModel
    @State private var open: BoardTile?
    @State private var showsPairing = false

    private let columns = [
        GridItem(.flexible(), spacing: 12, alignment: .top),
        GridItem(.flexible(), spacing: 12, alignment: .top),
    ]

    var body: some View {
        NavigationStack {
            ScrollView {
                let sections = model.board?.sections() ?? []
                if sections.isEmpty {
                    empty
                } else {
                    LazyVGrid(columns: columns, alignment: .leading, spacing: 12) {
                        ForEach(sections) { section in
                            Section {
                                ForEach(section.tiles) { tile in
                                    Button { open = tile } label: {
                                        BoardTileView(
                                            model: model, tile: tile, hue: section.cluster?.hue
                                        )
                                    }
                                    .buttonStyle(.plain)
                                    .accessibilityLabel(Text("a11y.thought"))
                                    .accessibilityIdentifier("tile-\(tile.id)")
                                }
                            } header: {
                                header(section)
                            }
                        }
                    }
                    .padding(16)
                }
            }
            .background(Theme.background)
            .navigationTitle("tab.board")
            .refreshable { await model.refreshBoard() }
            .safeAreaInset(edge: .bottom) { status }
            .sheet(item: $open) { tile in
                ThoughtDetailScreen(model: model, tile: tile)
            }
            .sheet(isPresented: $showsPairing) {
                PairingScreen(model: model)
            }
            .task { await model.refreshBoard() }
        }
        .tint(Theme.accent)
    }

    private func header(_ section: BoardSection) -> some View {
        HStack(spacing: 8) {
            if let cluster = section.cluster {
                Circle()
                    .fill(Theme.cluster(hue: cluster.hue))
                    .frame(width: 8, height: 8)
                Text(cluster.name)
            } else {
                Text("board.unsorted")
            }
            Text(verbatim: "\(section.tiles.count)")
                .foregroundStyle(Theme.textTertiary)
        }
        .font(.subheadline.weight(.semibold))
        .foregroundStyle(Theme.textSecondary)
        .padding(.top, 8)
        .accessibilityElement(children: .combine)
    }

    private var empty: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(model.isPaired ? "board.empty" : "board.notPaired")
                .font(.subheadline)
                .foregroundStyle(Theme.textSecondary)
            if !model.isPaired {
                Button("pairing.line.hint") { showsPairing = true }
                    .frame(minHeight: 44)
                    .accessibilityIdentifier("board-pair")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(24)
        .accessibilityIdentifier("board-empty")
    }

    /// One floating line, only while there is something to say about the vault.
    @ViewBuilder
    private var status: some View {
        switch model.boardState {
        case .idle:
            EmptyView()
        case .syncing, .offline:
            Text(model.boardState == .syncing ? "board.syncing" : "board.offline")
                .font(.footnote)
                .foregroundStyle(Theme.textSecondary)
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
                .glassEffect()
                .padding(.bottom, 8)
                .accessibilityIdentifier("board-status")
        }
    }
}

/// One card: image first, then the words, then what kind of thought it was.
private struct BoardTileView: View {
    @ObservedObject var model: AppModel
    let tile: BoardTile
    let hue: Int?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let image = tile.thought.images.first {
                AttachmentImage(
                    model: model, objectId: tile.id, head: tile.head, image: image, maxSide: 600
                )
                .aspectRatio(aspect(image), contentMode: .fit)
                .clipShape(RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous))
            }
            (tile.title.map { Text(verbatim: $0) } ?? fallbackTitle)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Theme.textPrimary)
                .lineLimit(3)
            if let summary = tile.card?.summary, !summary.isEmpty {
                Text(summary)
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
                    .lineLimit(4)
            }
            HStack(spacing: 6) {
                Image(systemName: glyph)
                if tile.thought.images.count > 1 {
                    Text(verbatim: "\(tile.thought.images.count)")
                }
                Spacer()
                if tile.card?.pinned == true {
                    Image(systemName: "pin.fill")
                        .accessibilityLabel(Text("board.pinned"))
                }
            }
            .font(.caption)
            .foregroundStyle(Theme.textTertiary)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(hue.map { Theme.cluster(hue: $0).opacity(0.12) } ?? Theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous)
                .strokeBorder(Theme.border, lineWidth: 1)
        )
    }

    private var fallbackTitle: Text {
        if tile.thought.audio != nil { return Text("board.voice") }
        if tile.thought.link != nil { return Text("board.link") }
        let count = tile.thought.images.count
        if count > 1 {
            return Text(String(format: NSLocalizedString("board.photos", comment: ""), count))
        }
        return Text("board.photo")
    }

    private var glyph: String {
        if tile.thought.audio != nil { return "waveform" }
        if tile.thought.link != nil { return "link" }
        if !tile.thought.images.isEmpty { return "photo" }
        return "text.alignleft"
    }
}

/// An image attachment, from the cache or the vault, holding its space while it loads.
private struct AttachmentImage: View {
    @ObservedObject var model: AppModel
    let objectId: String
    let head: BoardHead
    let image: ThoughtManifest.Image
    /// Decode no larger than this on the long edge; nil keeps the stored size.
    var maxSide: CGFloat?
    @State private var loaded: UIImage?

    var body: some View {
        ZStack {
            Theme.inset
            if let loaded {
                Image(uiImage: loaded)
                    .resizable()
                    .scaledToFill()
            }
        }
        .task(id: image.sha256) {
            guard let full = await model.image(objectId: objectId, head: head, image: image) else {
                return
            }
            if let maxSide, max(full.size.width, full.size.height) > maxSide {
                let scale = maxSide / max(full.size.width, full.size.height)
                loaded = await full.byPreparingThumbnail(
                    ofSize: CGSize(width: full.size.width * scale, height: full.size.height * scale)
                )
            } else {
                loaded = full
            }
        }
    }
}

private func aspect(_ image: ThoughtManifest.Image) -> CGFloat {
    guard image.width > 0, image.height > 0 else { return 1 }
    return CGFloat(image.width) / CGFloat(image.height)
}

/// Everything in one thought, and the one way to remove it.
private struct ThoughtDetailScreen: View {
    @ObservedObject var model: AppModel
    let tile: BoardTile
    @Environment(\.dismiss) private var dismiss
    @State private var confirmsDelete = false
    @State private var deleting = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ForEach(Array(tile.thought.images.enumerated()), id: \.offset) { _, image in
                        AttachmentImage(model: model, objectId: tile.id, head: tile.head, image: image)
                            .aspectRatio(aspect(image), contentMode: .fit)
                            .clipShape(
                                RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous)
                            )
                    }
                    if let card = tile.card {
                        Text(card.title)
                            .font(.title3.weight(.semibold))
                            .foregroundStyle(Theme.textPrimary)
                        HStack(spacing: 8) {
                            Circle()
                                .fill(Theme.cluster(hue: card.cluster.hue))
                                .frame(width: 8, height: 8)
                            Text(card.cluster.name)
                        }
                        .font(.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                        if !card.summary.isEmpty {
                            Text(card.summary)
                                .font(.subheadline)
                                .foregroundStyle(Theme.textSecondary)
                        }
                        if !card.tags.isEmpty {
                            ScrollView(.horizontal, showsIndicators: false) {
                                HStack(spacing: 8) {
                                    ForEach(card.tags, id: \.self) { tag in
                                        Text(tag)
                                            .font(.caption)
                                            .padding(.horizontal, 10)
                                            .padding(.vertical, 5)
                                            .background(Theme.inset)
                                            .clipShape(Capsule())
                                    }
                                }
                            }
                        }
                    }
                    if !tile.thought.text.isEmpty {
                        Text(tile.thought.text)
                            .font(.body)
                            .foregroundStyle(Theme.textPrimary)
                            .textSelection(.enabled)
                    }
                    if let link = tile.thought.link, let url = URL(string: link.url) {
                        Link(destination: url) {
                            Label(link.url, systemImage: "link")
                                .font(.subheadline)
                                .lineLimit(2)
                        }
                    }
                    if let audio = tile.thought.audio {
                        Label(
                            String(
                                format: NSLocalizedString("board.audio", comment: ""),
                                ThoughtDetailScreen.clock(audio.duration_ms)
                            ),
                            systemImage: "waveform"
                        )
                        .font(.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                    }
                    Text(
                        String(
                            format: NSLocalizedString("board.captured", comment: ""),
                            Date(timeIntervalSince1970: Double(tile.thought.captured_at_utc_ms) / 1000)
                                .formatted(date: .abbreviated, time: .shortened)
                        )
                    )
                    .font(.caption)
                    .foregroundStyle(Theme.textTertiary)
                }
                .padding(24)
            }
            .background(Theme.background)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("board.done") { dismiss() }
                }
                ToolbarItem(placement: .destructiveAction) {
                    Button("board.delete", role: .destructive) { confirmsDelete = true }
                        .disabled(deleting)
                        .accessibilityIdentifier("thought-delete")
                }
            }
            .confirmationDialog(
                "board.deleteConfirm", isPresented: $confirmsDelete, titleVisibility: .visible
            ) {
                Button("board.delete", role: .destructive) {
                    deleting = true
                    Task {
                        await model.deleteThought(id: tile.id)
                        dismiss()
                    }
                }
            } message: {
                Text("board.deleteDetail")
            }
        }
        .tint(Theme.accent)
    }

    private static func clock(_ ms: Int64) -> String {
        let total = Int(ms / 1000)
        return String(format: "%d:%02d", total / 60, total % 60)
    }
}
