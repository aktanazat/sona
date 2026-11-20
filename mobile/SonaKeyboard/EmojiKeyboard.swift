import SwiftUI
import UIKit

/// What the emoji keyboard shows and does, owned by the keyboard controller.
@MainActor
@Observable
final class EmojiModel {
    var recents: [String] = []
    /// The emoji whose skin tones are offered, and where its cell sits.
    var toning: (emoji: EmojiCatalog.Emoji, frame: CGRect)?
    @ObservationIgnored var insert: (String) -> Void = { _ in }
    @ObservationIgnored var memory = EmojiMemory(defaults: .standard)

    func load() { recents = memory.recents }

    func pick(_ glyph: String, base: String) {
        if glyph != base { memory.choose(glyph, for: base) }
        memory.used(glyph)
        insert(glyph)
        toning = nil
    }
}

/// The emoji keyboard: a search field, the emoji in pages by group with the recent ones
/// first, and a bar of groups between ABC and delete. Holding an emoji with skin tones
/// offers them; the one chosen is remembered.
struct EmojiPanel: View {
    let catalog: EmojiCatalog
    let model: EmojiModel
    let typing: TypingModel
    let search: () -> Void
    @State private var group = -1

    private static let rows = 4
    static let cell: CGFloat = 36

    var body: some View {
        VStack(spacing: 0) {
            Button(action: search) {
                HStack(spacing: 6) {
                    Image(systemName: "magnifyingglass")
                    Text("keyboard.emoji.search")
                    Spacer(minLength: 0)
                }
                .font(.system(size: 16))
                .foregroundStyle(Theme.textSecondary)
                .padding(.horizontal, 10)
                .frame(height: 32)
                .background(KeyFace.fill, in: Capsule())
                .contentShape(Capsule())
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 8)
            .padding(.vertical, 2)
            .accessibilityIdentifier("keyboard-emoji-search")
            ScrollViewReader { scroller in
                ScrollView(.horizontal, showsIndicators: false) {
                    LazyHGrid(rows: Array(repeating: GridItem(.fixed(Self.cell), spacing: 0), count: Self.rows), spacing: 0) {
                        if !model.recents.isEmpty {
                            section(-1, model.recents.compactMap { glyph in catalog.entry(for: glyph).map { (glyph, $0) } })
                        }
                        ForEach(EmojiCatalog.groups.indices, id: \.self) { index in
                            section(index, catalog.grouped[index].map { (model.memory.toned($0.glyph), $0) })
                        }
                    }
                    .padding(.horizontal, 6)
                }
                .frame(maxHeight: .infinity)
                .onChange(of: group) { _, value in
                    withAnimation(.snappy(duration: 0.25)) { scroller.scrollTo("group\(value)", anchor: .leading) }
                }
            }
            groupBar
        }
        .coordinateSpace(name: EmojiCell.space)
        .overlay(alignment: .topLeading) {
            GeometryReader { proxy in tonePicker(width: proxy.size.width) }
        }
        .onAppear { model.load() }
    }

    @ViewBuilder
    private func section(_ index: Int, _ items: [(String, EmojiCatalog.Emoji)]) -> some View {
        ForEach(Array(items.enumerated()), id: \.offset) { offset, item in
            EmojiCell(glyph: item.0, entry: item.1, model: model)
                .id(offset == 0 ? "group\(index)" : "\(index)-\(offset)")
        }
    }

    private var groupBar: some View {
        HStack(spacing: 0) {
            barKey(Text(verbatim: KeyLayer.letters.title).font(.system(size: 15)), label: KeyLayer.letters.label) {
                typing.press(.layer(.letters))
            }
            if !model.recents.isEmpty {
                groupButton(-1, symbol: "clock", label: "keyboard.emoji.recent")
            }
            ForEach(EmojiCatalog.groups.indices, id: \.self) { index in
                groupButton(index, symbol: EmojiCatalog.groups[index].symbol, label: EmojiCatalog.groups[index].label)
            }
            barKey(Image(systemName: "delete.left").font(.system(size: 17)), label: "keyboard.delete") {
                typing.touched(.delete)
                typing.press(.delete)
            }
        }
        .frame(height: 34)
        .padding(.horizontal, 4)
    }

    private func groupButton(_ index: Int, symbol: String, label: LocalizedStringKey) -> some View {
        Button {
            group = index
        } label: {
            Image(systemName: symbol)
                .font(.system(size: 15, weight: group == index ? .semibold : .regular))
                .foregroundStyle(group == index ? Theme.textPrimary : Theme.textSecondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(group == index ? KeyFace.fill.opacity(0.9) : Color.clear, in: Circle().inset(by: 3))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(label))
    }

    private func barKey(_ face: some View, label: LocalizedStringKey, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            face.foregroundStyle(Theme.textPrimary)
                .frame(width: 46, height: 32)
                .background(KeyFace.fill, in: RoundedRectangle(cornerRadius: KeyFace.radius, style: .continuous))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(label))
    }

    @ViewBuilder
    private func tonePicker(width bounds: CGFloat) -> some View {
        if let toning = model.toning {
            let options = [toning.emoji.glyph] + toning.emoji.tones
            let width = CGFloat(options.count) * Self.cell + 8
            HStack(spacing: 0) {
                ForEach(options, id: \.self) { option in
                    Button { model.pick(option, base: toning.emoji.glyph) } label: {
                        Text(verbatim: option).font(.system(size: 30)).frame(width: Self.cell, height: Self.cell + 4)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(4)
            .background(KeyFace.fill, in: RoundedRectangle(cornerRadius: KeyFace.radius + 3, style: .continuous))
            .shadow(color: .black.opacity(0.2), radius: 10, y: 3)
            .offset(
                x: min(max(toning.frame.midX - width / 2, 4), bounds - width - 4),
                y: max(toning.frame.minY - Self.cell - 14, 0)
            )
            .transition(.scale(scale: 0.9, anchor: .bottom).combined(with: .opacity))
        }
    }
}

private struct EmojiCell: View {
    static let space = "emoji"

    let glyph: String
    let entry: EmojiCatalog.Emoji
    let model: EmojiModel

    var body: some View {
        GeometryReader { proxy in
            Text(verbatim: glyph)
                .font(.system(size: 28))
                .frame(width: proxy.size.width, height: proxy.size.height)
                .contentShape(Rectangle())
                .onTapGesture { model.pick(glyph, base: entry.glyph) }
                .onLongPressGesture(minimumDuration: 0.35) {
                    guard !entry.tones.isEmpty else { return model.pick(glyph, base: entry.glyph) }
                    UISelectionFeedbackGenerator().selectionChanged()
                    withAnimation(.snappy(duration: 0.2)) { model.toning = (entry, proxy.frame(in: .named(Self.space))) }
                }
        }
        .frame(width: 40, height: EmojiPanel.cell)
        .accessibilityElement()
        .accessibilityLabel(Text(verbatim: entry.name))
        .accessibilityAddTraits(.isButton)
        .accessibilityAction { model.pick(glyph, base: entry.glyph) }
    }
}

/// The emoji search while it is typed: the query on the left, the emoji it finds beside it.
struct EmojiSearchBar: View {
    let query: String
    let results: [EmojiCatalog.Emoji]
    let memory: EmojiMemory
    let pick: (String, String) -> Void
    let close: () -> Void

    var body: some View {
        HStack(spacing: 6) {
            HStack(spacing: 4) {
                Image(systemName: "magnifyingglass").foregroundStyle(Theme.textSecondary)
                Text(verbatim: query).lineLimit(1).truncationMode(.head)
                Rectangle().fill(Theme.accent).frame(width: 2, height: 18)
            }
            .font(.system(size: 16))
            .padding(.horizontal, 10)
            .frame(width: 132, height: 32, alignment: .leading)
            .background(KeyFace.fill, in: Capsule())
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 0) {
                    if results.isEmpty, !query.isEmpty {
                        Text("keyboard.emoji.none").font(.footnote).foregroundStyle(Theme.textSecondary).padding(.leading, 6)
                    }
                    ForEach(results, id: \.glyph) { emoji in
                        let glyph = memory.toned(emoji.glyph)
                        Button { pick(glyph, emoji.glyph) } label: {
                            Text(verbatim: glyph).font(.system(size: 28)).frame(width: 40, height: 40)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(Text(verbatim: emoji.name))
                    }
                }
            }
            .scrollEdgeEffectHidden(true, for: .all)
            Button(action: close) {
                Image(systemName: "xmark.circle.fill").font(.system(size: 20)).foregroundStyle(Theme.textSecondary)
                    .frame(width: 36, height: 40)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text("keyboard.emoji.close"))
        }
    }
}
