import AppKit
import QuickLook
import QuickLookThumbnailing
import SwiftUI
import UniformTypeIdentifiers

extension WorkAttachmentSource {
    static func reading(_ pasteboard: NSPasteboard) -> [WorkAttachmentSource] {
        let files = pasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
        if !files.isEmpty { return files.map { .file($0) } }
        if let data = pasteboard.data(forType: .png) ?? pasteboard.data(forType: .tiff).flatMap(png) {
            return [.data(data, fileName: pastedName())]
        }
        if let text = pasteboard.string(forType: .URL) ?? pasteboard.string(forType: .string), let link = linkText(text) {
            return [.link(link)]
        }
        return []
    }

    static func loading(_ providers: [NSItemProvider]) async -> [WorkAttachmentSource] {
        var sources: [WorkAttachmentSource] = []
        for provider in providers {
            if let source = await load(provider) { sources.append(source) }
        }
        return sources
    }

    static func linkText(_ text: String) -> String? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.contains(where: \.isWhitespace), let url = URL(string: trimmed),
              let scheme = url.scheme?.lowercased(), linkSchemes.contains(scheme) else { return nil }
        if scheme == "mailto" { return url.absoluteString.count > "mailto:".count ? url.absoluteString : nil }
        return url.host() == nil ? nil : url.absoluteString
    }

    private static func load(_ provider: NSItemProvider) async -> WorkAttachmentSource? {
        if provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier),
           let url = await url(from: provider, type: .fileURL) {
            return .file(url)
        }
        if provider.hasItemConformingToTypeIdentifier(UTType.image.identifier),
           let data = await imageData(from: provider) {
            return .data(data, fileName: pastedName(provider.suggestedName))
        }
        if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier),
           let url = await url(from: provider, type: .url), let link = linkText(url.absoluteString) {
            return .link(link)
        }
        return nil
    }

    private static func url(from provider: NSItemProvider, type: UTType) async -> URL? {
        await withCheckedContinuation { done in
            provider.loadItem(forTypeIdentifier: type.identifier) { item, _ in
                if let data = item as? Data {
                    done.resume(returning: URL(dataRepresentation: data, relativeTo: nil))
                } else {
                    done.resume(returning: item as? URL)
                }
            }
        }
    }

    private static func imageData(from provider: NSItemProvider) async -> Data? {
        await withCheckedContinuation { done in
            provider.loadDataRepresentation(forTypeIdentifier: UTType.image.identifier) { data, _ in
                done.resume(returning: data.flatMap(png))
            }
        }
    }

    private static func png(_ data: Data) -> Data? {
        NSBitmapImageRep(data: data)?.representation(using: .png, properties: [:])
    }

    private static func pastedName(_ suggested: String? = nil) -> String {
        if let suggested, !suggested.isEmpty {
            return (suggested as NSString).deletingPathExtension + ".png"
        }
        let format = DateFormatter()
        format.locale = Locale(identifier: "en_US_POSIX")
        format.dateFormat = "yyyy-MM-dd 'at' HH.mm.ss"
        return "Pasted image \(format.string(from: .now)).png"
    }
}

extension AppModel {
    func attachPasteboard(to itemId: String) -> Bool {
        let sources = WorkAttachmentSource.reading(.general)
        guard !sources.isEmpty else { return false }
        Task { await attach(sources, to: itemId) }
        return true
    }

    func attach(_ sources: [WorkAttachmentSource], to itemId: String) async {
        let repeats = await work.attach(sources, to: itemId)
        guard repeats > 0, work.mutationError == nil else { return }
        workFocus.announce(repeats == sources.count ? "Already attached" : "Some were already attached", undoes: false)
    }

    func chooseAttachments(for itemId: String) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = true
        panel.prompt = "Attach"
        panel.begin { [weak self] response in
            MainActor.assumeIsolated {
                guard let self, response == .OK, !panel.urls.isEmpty else { return }
                let sources = panel.urls.map { WorkAttachmentSource.file($0) }
                Task { await self.attach(sources, to: itemId) }
            }
        }
    }
}

struct AttachmentDrop: ViewModifier {
    let itemId: String
    let number: Int64
    @Environment(AppModel.self) private var model
    @State private var targeted = false

    func body(content: Content) -> some View {
        content
            .onDrop(of: [.fileURL, .image, .url], isTargeted: $targeted) { providers in
                let model = model
                let itemId = itemId
                Task {
                    let sources = await WorkAttachmentSource.loading(providers)
                    guard !sources.isEmpty else { return }
                    await model.attach(sources, to: itemId)
                }
                return true
            }
            .overlay {
                if targeted {
                    RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous)
                        .strokeBorder(Theme.accent, style: StrokeStyle(lineWidth: 2, dash: [6, 4]))
                        .background(Theme.accentSoft.opacity(0.5), in: RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous))
                        .overlay {
                            Label("Attach to #\(number)", systemImage: "paperclip")
                                .font(TypeScale.label(14))
                                .foregroundStyle(Theme.accent)
                                .padding(.horizontal, 14)
                                .frame(height: Theme.controlHeight + 8)
                                .background(Theme.elevated, in: Capsule())
                                .elevation(.medium)
                        }
                        .padding(8)
                        .allowsHitTesting(false)
                        .transition(.opacity)
                }
            }
            .animation(Motion.quick, value: targeted)
    }
}

struct DetailAttachments: View {
    let row: WorkItemRow
    let attachments: [WorkAttachment]?
    @Environment(AppModel.self) private var model
    @State private var highlighted: String?
    @State private var preview: URL?
    @State private var addingLink = false
    @State private var renaming: WorkAttachment?
    @State private var renameText = ""
    @FocusState private var focused: Bool

    var body: some View {
        let list = (attachments ?? []).filter { $0.commentId == nil }
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                DetailHeading(title: "Attachments")
                if !list.isEmpty {
                    Text(verbatim: "\(list.count)").metaText().monospacedDigit()
                }
                Spacer(minLength: 0)
                addMenu
            }
            if list.isEmpty {
                emptyLine
            } else {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150, maximum: 220), spacing: 10)], alignment: .leading, spacing: 10) {
                    ForEach(list) { attachment in
                        AttachmentTile(
                            attachment: attachment,
                            highlighted: highlighted == attachment.id,
                            open: { open(attachment) },
                            choose: {
                                highlighted = attachment.id
                                focused = true
                            },
                            menu: { menu(for: attachment) })
                    }
                }
                .focusable(interactions: .edit)
                .focused($focused)
                .focusEffectDisabled()
                .onKeyPress(.space) { previewHighlighted(in: list) }
                .onKeyPress(.return) { act(on: list, open) }
                .onKeyPress(.delete) { act(on: list, remove) }
                .onKeyPress(.leftArrow) { move(-1, in: list) }
                .onKeyPress(.rightArrow) { move(1, in: list) }
                .quickLookPreview($preview, in: list.compactMap(\.fileURL))
            }
        }
        .onChange(of: focused) { _, now in
            if !now { highlighted = nil }
        }
        .onChange(of: addingLink) { _, now in
            guard !now else { return }
            DispatchQueue.main.async {
                NSApp.keyWindow?.makeFirstResponder(nil)
                if !list.isEmpty { focused = true }
            }
        }
        .onChange(of: model.workFocus.pageAsk, initial: true) { _, ask in
            guard ask == TaskPageAsk(id: row.id, part: .link) else { return }
            addingLink = true
            DispatchQueue.main.async { model.workFocus.pageAsk = nil }
        }
        .onChange(of: row.id) {
            highlighted = nil
            preview = nil
        }
        .alert("Rename attachment", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("Title", text: $renameText)
            Button("Rename") { rename() }
            Button("Cancel", role: .cancel) { renaming = nil }
        }
    }

    private var addMenu: some View {
        Menu {
            Button("Add files…") { model.chooseAttachments(for: row.id) }
            Button("Add link…") { addingLink = true }
        } label: {
            Image(systemName: "plus")
        }
        .menuStyle(.button)
        .buttonStyle(.icon)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Attach files or a link. Drop files on the page or paste with ⌘V.")
        .accessibilityLabel("Attach")
        .popover(isPresented: $addingLink, arrowEdge: .bottom) {
            AddLinkForm(itemId: row.id) { addingLink = false }
        }
    }

    private var emptyLine: some View {
        HStack(spacing: 10) {
            Image(systemName: "paperclip")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.inkTertiary)
                .frame(width: 16)
            Text("Drop files here or paste with ⌘V").bodyText(13, Theme.inkTertiary)
            Spacer(minLength: 8)
            Button("Choose files") { model.chooseAttachments(for: row.id) }
                .buttonStyle(QuietButton(color: Theme.accent, compact: true))
            Button("Add link") { addingLink = true }
                .buttonStyle(QuietButton(color: Theme.accent, compact: true))
        }
        .padding(.horizontal, 8)
        .frame(minHeight: 36)
        .background {
            RoundedRectangle(cornerRadius: Theme.radiusRow)
                .strokeBorder(Theme.border, style: StrokeStyle(lineWidth: 1, dash: [4, 3]))
        }
    }

    @ViewBuilder
    private func menu(for attachment: WorkAttachment) -> some View {
        Button("Open") { open(attachment) }
        if let file = attachment.fileURL {
            Button("Quick Look") {
                highlighted = attachment.id
                preview = file
            }
            Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([file]) }
            Button("Copy path") { copy(file.path(percentEncoded: false)) }
        }
        if let url = attachment.url {
            Button("Copy link") { copy(url) }
        }
        Divider()
        Button("Rename…") {
            renameText = attachment.title
            renaming = attachment
        }
        Button("Remove", role: .destructive) { remove(attachment) }
    }

    private func open(_ attachment: WorkAttachment) {
        guard let url = attachment.fileURL ?? attachment.linkURL else { return }
        NSWorkspace.shared.open(url)
    }

    private func remove(_ attachment: WorkAttachment) {
        let work = model.work
        let focus = model.workFocus
        Task {
            if await work.detach(attachment) { focus.announce(work.undoTitle) }
        }
    }

    private func rename() {
        guard let attachment = renaming else { return }
        renaming = nil
        let title = renameText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty, title != attachment.title else { return }
        let work = model.work
        Task { _ = await work.renameAttachment(attachment, to: title) }
    }

    private func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
        model.workFocus.announce("Copied", undoes: false)
    }

    private func previewHighlighted(in list: [WorkAttachment]) -> KeyPress.Result {
        if preview != nil {
            preview = nil
            return .handled
        }
        guard let current = list.first(where: { $0.id == highlighted }) else { return .ignored }
        if let file = current.fileURL {
            preview = file
        } else {
            open(current)
        }
        return .handled
    }

    private func act(on list: [WorkAttachment], _ action: (WorkAttachment) -> Void) -> KeyPress.Result {
        guard let current = list.first(where: { $0.id == highlighted }) else { return .ignored }
        action(current)
        return .handled
    }

    private func move(_ delta: Int, in list: [WorkAttachment]) -> KeyPress.Result {
        guard !list.isEmpty else { return .ignored }
        let index = list.firstIndex { $0.id == highlighted }.map { $0 + delta } ?? 0
        highlighted = list[min(max(index, 0), list.count - 1)].id
        if preview != nil { preview = list.first { $0.id == highlighted }?.fileURL }
        return .handled
    }
}

private struct AttachmentTile<Menu: View>: View {
    let attachment: WorkAttachment
    let highlighted: Bool
    let open: () -> Void
    let choose: () -> Void
    @ViewBuilder let menu: () -> Menu
    @State private var hovering = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusControl, style: .continuous)
        VStack(alignment: .leading, spacing: 0) {
            AttachmentThumbnail(attachment: attachment)
                .frame(height: 92)
                .frame(maxWidth: .infinity)
                .clipped()
            Rectangle().fill(Theme.hairline).frame(height: 1)
            VStack(alignment: .leading, spacing: 2) {
                Text(attachment.title)
                    .font(TypeScale.label(12))
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let subtitle = attachment.subtitle, !subtitle.isEmpty {
                    Text(subtitle).metaText(Theme.inkSecondary).lineLimit(1)
                }
                Text(meta).metaText().lineLimit(1)
                ForEach(facts, id: \.key) { fact in
                    Text("\(fact.key): \(fact.value)").metaText(Theme.inkSecondary).lineLimit(1)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
        }
        .background(Theme.surface, in: shape)
        .clipShape(shape)
        .overlay(shape.strokeBorder(highlighted ? Theme.accent : Theme.border, lineWidth: highlighted ? 2 : 1))
        .overlay(alignment: .topTrailing) {
            SwiftUI.Menu { menu() } label: { Image(systemName: "ellipsis") }
                .menuStyle(.button)
                .buttonStyle(.icon)
                .menuIndicator(.hidden)
                .fixedSize()
                .background(Theme.elevated.opacity(0.9), in: RoundedRectangle(cornerRadius: Theme.radiusRow))
                .padding(4)
                .opacity(hovering ? 1 : 0)
                .help("More")
                .accessibilityLabel("More for \(attachment.title)")
        }
        .contentShape(shape)
        .onHover { hovering = $0 }
        .onTapGesture(count: 2, perform: open)
        .onTapGesture(perform: choose)
        .contextMenu { menu() }
        .help(help)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(attachment.title)
        .accessibilityValue(([attachment.subtitle, meta].compactMap { $0 } + facts.map { "\($0.key): \($0.value)" })
            .joined(separator: ", "))
        .accessibilityAddTraits(highlighted ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction(named: "Open", open)
    }

    private var meta: String {
        let who = attachment.createdBy.display
        let what: String?
        if attachment.kind == .link {
            what = attachment.linkURL.flatMap { $0.host() ?? ($0.scheme == "mailto" ? "Email" : nil) }
        } else {
            what = attachment.size.map { ByteCountFormatter.string(fromByteCount: $0, countStyle: .file) }
        }
        return [what, who].compactMap { $0 }.joined(separator: " · ")
    }

    private var facts: [(key: String, value: String)] {
        attachment.metadata.sorted { $0.key < $1.key }
    }

    private var help: String {
        let source = attachment.url ?? attachment.originalName ?? attachment.title
        let keys = attachment.kind == .link ? "Double-click to open" : "Space to preview, double-click to open"
        let lines = [attachment.subtitle, source, "Added by \(attachment.createdBy.display), \(PlanningFormat.date(attachment.createdAtMs))", keys]
        return lines.compactMap { $0 }.joined(separator: "\n")
    }
}

/// The files and links put on one comment, under its words. A click opens
/// one; its menu shows it in Finder or takes it off.
struct CommentFiles: View {
    let files: [WorkAttachment]
    @Environment(AppModel.self) private var model

    var body: some View {
        ChatFlowRow(spacing: 6) {
            ForEach(files) { file in
                Button { open(file) } label: {
                    FileChip(title: file.title, icon: icon(file))
                }
                .buttonStyle(.plain)
                .help(file.originalName ?? file.url ?? file.title)
                .contextMenu {
                    Button("Open") { open(file) }
                    if let url = file.fileURL {
                        Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([url]) }
                    }
                    Divider()
                    Button("Remove", role: .destructive) { remove(file) }
                }
            }
        }
    }

    private func icon(_ file: WorkAttachment) -> String {
        switch file.kind {
        case .image: "photo"
        case .link: "link"
        case .file: "doc"
        }
    }

    private func open(_ file: WorkAttachment) {
        guard let url = file.fileURL ?? file.linkURL else { return }
        NSWorkspace.shared.open(url)
    }

    private func remove(_ file: WorkAttachment) {
        let work = model.work
        let focus = model.workFocus
        Task {
            if await work.detach(file) { focus.announce(work.undoTitle) }
        }
    }
}

/// A file's name in a small pill, with an optional button to take it off.
struct FileChip: View {
    let title: String
    let icon: String
    var remove: (() -> Void)?

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(Theme.inkTertiary)
            Text(title)
                .font(TypeScale.label(12))
                .foregroundStyle(Theme.ink)
                .lineLimit(1)
                .truncationMode(.middle)
            if let remove {
                Button(action: remove) {
                    Image(systemName: "xmark").font(.system(size: 9, weight: .semibold))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Theme.inkTertiary)
                .help("Remove \(title)")
                .accessibilityLabel("Remove \(title)")
            }
        }
        .padding(.horizontal, 8)
        .frame(height: 24)
        .background(Theme.inset, in: Capsule())
        .overlay(Capsule().strokeBorder(Theme.hairline))
        .accessibilityElement(children: .combine)
    }
}

private struct AttachmentThumbnail: View {
    let attachment: WorkAttachment
    @Environment(\.displayScale) private var scale
    @State private var image: NSImage?

    var body: some View {
        ZStack {
            Theme.inset
            if let image {
                if attachment.kind == .image {
                    Image(nsImage: image).resizable().aspectRatio(contentMode: .fill)
                } else {
                    Image(nsImage: image).resizable().aspectRatio(contentMode: .fit).padding(12)
                }
            } else {
                Image(systemName: symbol)
                    .font(.system(size: 22, weight: .light))
                    .foregroundStyle(Theme.inkTertiary)
            }
        }
        .task(id: attachment.path) {
            guard let url = attachment.fileURL else { return }
            image = await Self.thumbnail(url, scale: scale)
        }
    }

    private var symbol: String {
        switch attachment.kind {
        case .link: attachment.linkURL?.scheme == "mailto" ? "envelope" : "link"
        case .image: "photo"
        case .file: "doc"
        }
    }

    private static func thumbnail(_ url: URL, scale: CGFloat) async -> NSImage? {
        let request = QLThumbnailGenerator.Request(
            fileAt: url, size: CGSize(width: 220, height: 92), scale: scale, representationTypes: .all)
        if let made = try? await QLThumbnailGenerator.shared.generateBestRepresentation(for: request) {
            return made.nsImage
        }
        guard FileManager.default.fileExists(atPath: url.path(percentEncoded: false)) else { return nil }
        return NSWorkspace.shared.icon(forFile: url.path(percentEncoded: false))
    }
}

private struct AddLinkForm: View {
    let itemId: String
    let done: () -> Void
    @Environment(AppModel.self) private var model
    @State private var url = ""
    @State private var title = ""
    @State private var subtitle = ""
    @FocusState private var field: Field?

    private enum Field { case url, title, subtitle }

    var body: some View {
        let link = WorkAttachmentSource.linkText(url)
        VStack(alignment: .leading, spacing: 10) {
            Text("Add link").font(TypeScale.label(13)).foregroundStyle(Theme.ink)
            TextField("", text: $url, prompt: Text("https://").foregroundStyle(Theme.inkDisabled))
                .textFieldStyle(.roundedBorder)
                .focused($field, equals: .url)
                .onSubmit(add)
                .accessibilityLabel("Link")
            TextField("", text: $title, prompt: Text("Title (optional)").foregroundStyle(Theme.inkDisabled))
                .textFieldStyle(.roundedBorder)
                .focused($field, equals: .title)
                .onSubmit(add)
                .accessibilityLabel("Title")
            TextField("", text: $subtitle, prompt: Text("Subtitle (optional)").foregroundStyle(Theme.inkDisabled))
                .textFieldStyle(.roundedBorder)
                .focused($field, equals: .subtitle)
                .onSubmit(add)
                .accessibilityLabel("Subtitle")
            if !url.trimmingCharacters(in: .whitespaces).isEmpty, link == nil {
                Text("A link must start with http://, https://, mailto: or sona://.")
                    .metaText(Theme.live)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Spacer()
                Button("Cancel", action: done).keyboardShortcut(.cancelAction)
                Button("Add", action: add)
                    .keyboardShortcut(.defaultAction)
                    .disabled(link == nil)
            }
        }
        .padding(14)
        .frame(width: 320)
        .onAppear {
            if let copied = NSPasteboard.general.string(forType: .string).flatMap(WorkAttachmentSource.linkText) {
                url = copied
                field = .title
            } else {
                field = .url
            }
        }
    }

    private func add() {
        guard let link = WorkAttachmentSource.linkText(url) else { return }
        let named = title.trimmingCharacters(in: .whitespacesAndNewlines)
        let note = subtitle.trimmingCharacters(in: .whitespacesAndNewlines)
        let model = model
        let itemId = itemId
        Task { await model.attach([.link(link, title: named.isEmpty ? nil : named, subtitle: note.isEmpty ? nil : note)], to: itemId) }
        done()
    }
}
