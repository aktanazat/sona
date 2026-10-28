import SwiftUI
import UIKit

struct BoardScreen: View {
    @ObservedObject var model: AppModel
    @State private var open: BoardTile?
    @State private var showsPairing = false

    var body: some View {
        NavigationStack {
            List {
                let sections = model.board?.sections() ?? []
                if sections.isEmpty {
                    empty
                        .listRowSeparator(.hidden)
                        .listRowBackground(Theme.background)
                } else {
                    ForEach(sections) { section in
                        Section {
                            ForEach(section.tiles) { tile in
                                Button { open = tile } label: {
                                    BoardTileView(model: model, tile: tile)
                                }
                                .buttonStyle(.plain)
                                .accessibilityElement(children: .combine)
                                .accessibilityHint(Text("a11y.thought"))
                                .accessibilityIdentifier("tile-\(tile.id)")
                                .listRowBackground(Theme.background)
                                .listRowSeparatorTint(Theme.border)
                                .listRowInsets(
                                    EdgeInsets(top: 8, leading: Theme.pageInset, bottom: 8, trailing: Theme.pageInset)
                                )
                            }
                        } header: {
                            header(section)
                        }
                        .textCase(nil)
                    }
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .background(Theme.background)
            .navigationTitle("tab.board")
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(Theme.background, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
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
                    .frame(width: 6, height: 6)
                    .accessibilityHidden(true)
                Text(cluster.name)
            } else {
                Text("board.unsorted")
            }
            Spacer()
            Text(verbatim: "\(section.tiles.count)")
                .monospacedDigit()
                .foregroundStyle(Theme.textTertiary)
        }
        .font(.subheadline.weight(.medium))
        .foregroundStyle(Theme.textSecondary)
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
    }

    private var empty: some View {
        VStack(alignment: .leading, spacing: 8) {
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
        .accessibilityIdentifier("board-empty")
    }

    @ViewBuilder
    private var status: some View {
        switch model.boardState {
        case .idle:
            EmptyView()
        case .syncing, .offline:
            Text(model.boardState == .syncing ? "board.syncing" : "board.offline")
                .font(.footnote)
                .foregroundStyle(Theme.textSecondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, Theme.pageInset)
                .padding(.vertical, 10)
                .background(Theme.surface)
                .overlay(alignment: .top) {
                    Rectangle().fill(Theme.border).frame(height: 0.5)
                }
                .accessibilityIdentifier("board-status")
        }
    }
}

private struct BoardTileView: View {
    @ObservedObject var model: AppModel
    let tile: BoardTile

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            if let image = tile.thought.images.first {
                AttachmentImage(
                    model: model, objectId: tile.id, head: tile.head, image: image, maxSide: 600
                )
                .frame(width: 64, height: 64)
                .clipShape(RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous))
                .overlay {
                    RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous)
                        .strokeBorder(Theme.border, lineWidth: 0.5)
                }
                .accessibilityHidden(true)
            }
            VStack(alignment: .leading, spacing: 5) {
                (tile.title.map { Text(verbatim: $0) } ?? fallbackTitle)
                    .font(.body.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(3)
                if let summary = tile.card?.summary, !summary.isEmpty {
                    Text(summary)
                        .font(.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                        .lineLimit(2)
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
        }
        .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
        .contentShape(Rectangle())
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
                                            .foregroundStyle(Theme.textSecondary)
                                            .padding(.horizontal, 8)
                                            .padding(.vertical, 4)
                                            .background(Theme.inset)
                                            .clipShape(RoundedRectangle(cornerRadius: 4))
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
                .padding(Theme.pageInset)
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
