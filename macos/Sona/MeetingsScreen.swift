import SwiftUI

/// The meetings section's parts: every recorded meeting, what Sona heard
/// across them, the trash, and the way into one. The shell lays them out on
/// the meetings page under what leads to a recording, and hands over to
/// `MeetingReviewView` when a meeting is open.

// MARK: - What the store has to say

/// The line where a write lands: "Deleted. It waits in the trash for 30 days."
/// Also carries the path a ledger was written to, with the way to open it.
struct MeetingsNoticeBand: View {
    let store: MeetingsStore

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let notice = store.notice {
                band(notice, accent: false) {
                    Button("OK") { store.dismissNotice() }
                        .buttonStyle(QuietButton())
                }
            }
            if let error = store.error {
                band(error, accent: true) {
                    Button("Dismiss") { store.dismissError() }
                        .buttonStyle(QuietButton())
                }
            }
            if store.savedLedgerPath != nil {
                band("The ledger is written.", accent: false) {
                    HStack(spacing: 12) {
                        Button("Show in Finder") { store.openSavedLedger() }
                            .buttonStyle(SecondaryButton(compact: true))
                        Button("Dismiss") { store.dismissSavedLedger() }
                            .buttonStyle(QuietButton())
                    }
                }
            }
        }
        .padding(.bottom, store.notice == nil && store.error == nil && store.savedLedgerPath == nil ? 0 : 20)
    }

    private func band<Action: View>(
        _ text: String, accent: Bool, @ViewBuilder action: () -> Action
    ) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 16) {
            Text(text).bodyText(14, accent ? Theme.live : Theme.ink)
            Spacer(minLength: 12)
            action()
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(accent ? Theme.surface : Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.radiusCard)
                .strokeBorder(accent ? Theme.live.opacity(0.4) : Theme.border, lineWidth: 1)
        )
    }
}

/// The list failed to read. The message, and the one button that helps.
struct MeetingsRetryNote: View {
    let message: String
    let retry: () -> Void

    var body: some View {
        PageSection("Meetings") {
            Card {
                CardRow {
                    Text(message).bodyText(15, Theme.live)
                } trailing: {
                    Button("Try again", action: retry)
                        .buttonStyle(SecondaryButton(compact: true))
                }
            }
        }
        .padding(.bottom, 28)
    }
}

// MARK: - The trend

/// What Sona heard over a window: the totals, and one bar a day.
struct MeetingsTrendCard: View {
    let store: MeetingsStore

    var body: some View {
        PageSection("What Sona heard") {
            Card {
                ChoiceRow(
                    title: "Window",
                    detail: store.trend?.span,
                    choices: MeetingTrendRange.allCases,
                    label: { $0.label },
                    selection: range
                )
                if let totals = store.trend?.rangeTotal {
                    CardRow {
                        HStack(alignment: .top, spacing: 40) {
                            Stat(label: "Meetings", value: "\(totals.meetings)")
                            Stat(label: "Captured", value: totals.capturedSpoken)
                            Stat(label: "Lines said", value: "\(totals.transcriptSegments)")
                            Stat(label: "Action items", value: "\(totals.generatedActionItems)")
                        }
                    }
                    if !points.isEmpty {
                        CardRow {
                            MeetingsTrendBars(points: points)
                        }
                    }
                } else if store.trend != nil {
                    CardRow {
                        Text("Sona cannot reach its meeting storage, so there is no trend to show.")
                            .bodyText(14, Theme.inkSecondary)
                    }
                } else {
                    CardRow {
                        Text("Reading the trend…").bodyText(14, Theme.inkTertiary)
                    }
                }
            }
        }
        .padding(.bottom, 28)
    }

    private var points: [MeetingTrendPoint] { store.trend?.points ?? [] }

    private var range: Binding<MeetingTrendRange> {
        Binding(get: { store.trendRange }, set: { store.choose(trendRange: $0) })
    }
}

/// One bar a day, scaled to the loudest day in the window. A day with nothing
/// recorded keeps its place as a hairline.
struct MeetingsTrendBars: View {
    let points: [MeetingTrendPoint]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .bottom, spacing: 3) {
                ForEach(points) { point in
                    RoundedRectangle(cornerRadius: 2)
                        .fill(point.meetings > 0 ? Theme.accent : Theme.selection)
                        .frame(height: max(2, 56 * fraction(point)))
                        .help("\(point.localDate): \(label(point))")
                }
            }
            .frame(height: 56, alignment: .bottom)
            if let first = points.first, let last = points.last {
                HStack {
                    Text(first.localDate).metaText()
                    Spacer(minLength: 12)
                    Text(last.localDate).metaText()
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var peak: Int64 {
        max(1, points.map(\.verifiedCapturedDurationMs).max() ?? 1)
    }

    private func fraction(_ point: MeetingTrendPoint) -> Double {
        Double(point.verifiedCapturedDurationMs) / Double(peak)
    }

    private func label(_ point: MeetingTrendPoint) -> String {
        let meetings = point.meetings == 1 ? "1 meeting" : "\(point.meetings) meetings"
        return "\(meetings), \((TimeInterval(point.verifiedCapturedDurationMs) / 1000).spoken)"
    }
}

// MARK: - Search and filters

struct MeetingsFilterCard: View {
    let store: MeetingsStore

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            SearchField(prompt: "Search titles", text: query)
            Card {
                ChoiceRow(
                    title: "State",
                    choices: MeetingStatusFilter.allCases,
                    label: { $0.label },
                    selection: status
                )
                ChoiceRow(
                    title: "When",
                    choices: MeetingTimeWindow.allCases,
                    label: { $0.label },
                    selection: window
                )
            }
        }
        .padding(.bottom, 28)
    }

    private var query: Binding<String> {
        Binding(get: { store.query }, set: { store.search($0) })
    }

    private var status: Binding<MeetingStatusFilter> {
        Binding(get: { store.status }, set: { store.choose(status: $0) })
    }

    private var window: Binding<MeetingTimeWindow> {
        Binding(get: { store.window }, set: { store.choose(window: $0) })
    }
}

// MARK: - Folders

struct MeetingsFolderBar: View {
    let store: MeetingsStore
    let ask: (MeetingFolder) -> Void
    @State private var naming = false
    @State private var renaming: MeetingFolder?
    @State private var defaults: MeetingFolder?
    @State private var deleting: MeetingFolder?

    var body: some View {
        PageSection("Folders") {
            Card {
                CardRow {
                    Text("Keep related meetings together.").bodyText(14, Theme.inkSecondary)
                } trailing: {
                    Button("New folder") {
                        renaming = nil
                        naming = true
                    }
                    .buttonStyle(.compact)
                    .disabled(store.foldersLoading || store.folderBusy)
                }
                if store.foldersLoading {
                    CardLine("Reading folders…")
                } else if let error = store.foldersError {
                    ActionRow(title: error, button: "Try again", action: store.retryFolders)
                } else {
                    ChatFlowRow(spacing: 8) {
                        folderChip("All meetings", selected: store.folderId == nil) {
                            store.choose(folder: nil)
                        }
                        ForEach(store.folders) { folder in
                            folderChip("\(folder.name) · \(folder.meetingCount)", selected: store.folderId == folder.folderId) {
                                store.choose(folder: folder.folderId)
                            }
                        }
                    }
                    .padding(16)
                    if store.folders.isEmpty {
                        CardLine("No folders yet. Create one, then add meetings from their menus.")
                    }
                    if let folder = store.chosenFolder {
                        CardRow {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(folder.name).headlineText()
                                Text(folder.countLabel).metaText()
                            }
                        } trailing: {
                            HStack(spacing: 12) {
                                Button("Ask about these meetings") { ask(folder) }
                                    .buttonStyle(.compact)
                                Menu("Folder options") {
                                    Button("Rename") {
                                        renaming = folder
                                        naming = true
                                    }
                                    Button("Template and prompts") { defaults = folder }
                                    Divider()
                                    Button("Delete folder", role: .destructive) { deleting = folder }
                                }
                                .menuStyle(.borderlessButton)
                                .fixedSize()
                                .disabled(store.folderBusy)
                            }
                        }
                    }
                    if let error = store.folderError {
                        CardLine(error)
                    }
                }
            }
        }
        .sheet(isPresented: $naming) {
            MeetingFolderNameSheet(store: store, folder: renaming)
        }
        .sheet(item: $defaults) { folder in
            MeetingFolderDefaultsSheet(store: store, folder: folder)
        }
        .alert("Delete this folder?", isPresented: Binding(
            get: { deleting != nil }, set: { if !$0 { deleting = nil } }), presenting: deleting
        ) { folder in
            Button("Delete folder", role: .destructive) {
                store.deleteFolder(folder)
                deleting = nil
            }
            Button("Keep it", role: .cancel) { deleting = nil }
        } message: { _ in
            Text("The meetings in it stay.")
        }
    }

    private func folderChip(_ title: String, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title)
                .font(TypeScale.label(13))
                .foregroundStyle(selected ? Theme.accent : Theme.inkSecondary)
                .lineLimit(1)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(selected ? Theme.accentSoft : Theme.surface, in: Capsule())
                .overlay(Capsule().strokeBorder(selected ? Theme.accent : Theme.border, lineWidth: 1))
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
        .help(title)
    }
}

private struct MeetingFolderNameSheet: View {
    let store: MeetingsStore
    let folder: MeetingFolder?
    @Environment(\.dismiss) private var dismiss
    @State private var name: String

    init(store: MeetingsStore, folder: MeetingFolder?) {
        self.store = store
        self.folder = folder
        _name = State(initialValue: folder?.name ?? "")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(folder == nil ? "New folder" : "Rename folder").headlineText()
            InputField(prompt: "Folder name", text: $name)
                .accessibilityLabel("Folder name")
            Text("Use 1–80 characters and a name no other folder has.").metaText()
            if let error = store.folderError {
                ErrorNote(error)
            }
            if !name.isEmpty, let problem = store.folderNameProblem(name, except: folder?.folderId) {
                Text(problem).metaText(Theme.live)
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.buttonStyle(.quiet)
                Button(store.folderBusy ? "Saving…" : "Save") {
                    Task {
                        let saved: Bool
                        if let folder {
                            saved = await store.renameFolder(folder, to: name)
                        } else {
                            saved = await store.createFolder(named: name) != nil
                        }
                        if saved { dismiss() }
                    }
                }
                .buttonStyle(.primary)
                .disabled(store.folderBusy || store.folderNameProblem(name, except: folder?.folderId) != nil)
            }
        }
        .padding(24)
        .frame(width: 440)
        .background(Theme.page)
        .interactiveDismissDisabled(store.folderBusy)
        .onAppear { store.clearFolderError() }
    }
}

private struct MeetingFolderDefaultsSheet: View {
    let store: MeetingsStore
    let folder: MeetingFolder
    @Environment(\.dismiss) private var dismiss
    @State private var template: MeetingNotesTemplate?
    @State private var promptIds: [String]

    init(store: MeetingsStore, folder: MeetingFolder) {
        self.store = store
        self.folder = folder
        _template = State(initialValue: folder.template)
        _promptIds = State(initialValue: folder.promptIds)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Template and prompts").headlineText()
            Text("For meetings in “\(folder.name)”. A meeting or calendar series can choose a different template.")
                .metaText()
            Card {
                CardRow {
                    Text("Notes template").bodyText()
                } trailing: {
                    Picker("Notes template", selection: $template) {
                        Text("Use app default").tag(Optional<MeetingNotesTemplate>.none)
                        ForEach(MeetingNotesTemplate.allCases, id: \.self) { value in
                            Text(value.label).tag(Optional(value))
                        }
                    }
                    .labelsHidden()
                    .fixedSize()
                }
            }
            Text("Choose up to five saved meeting prompts. They run after the notes are ready.").metaText()
            ScrollView {
                Card {
                    if store.meetingPromptsLoading {
                        CardLine("Reading saved prompts…")
                    } else if let error = store.meetingPromptsError {
                        ActionRow(title: error, button: "Try again") {
                            Task { await loadPrompts() }
                        }
                    } else if store.meetingPrompts.isEmpty {
                        CardLine("No meeting prompts yet. Save one in Prompts, then return here.")
                    } else {
                        ForEach(store.meetingPrompts) { prompt in
                            ToggleRow(title: prompt.name, isOn: Binding(
                                get: { promptIds.contains(prompt.promptId) },
                                set: { selected in
                                    if selected {
                                        promptIds.append(prompt.promptId)
                                    } else {
                                        promptIds.removeAll { $0 == prompt.promptId }
                                    }
                                }))
                                .disabled(!promptIds.contains(prompt.promptId) && promptIds.count >= MeetingFolderLimit.prompts)
                        }
                    }
                }
            }
            .frame(maxHeight: 260)
            ErrorNote(store.folderError)
            HStack {
                Text("\(promptIds.count) of \(MeetingFolderLimit.prompts) prompts").metaText()
                Spacer()
                Button("Cancel") { dismiss() }.buttonStyle(.quiet)
                Button(store.folderBusy ? "Saving…" : "Save") {
                    Task {
                        if await store.setFolderDefaults(folder, template: template, promptIds: promptIds) {
                            dismiss()
                        }
                    }
                }
                .buttonStyle(.primary)
                .disabled(store.folderBusy || store.meetingPromptsLoading || store.meetingPromptsError != nil)
            }
        }
        .padding(24)
        .frame(width: 520)
        .background(Theme.page)
        .interactiveDismissDisabled(store.folderBusy)
        .task {
            store.clearFolderError()
            await loadPrompts()
        }
    }

    private func loadPrompts() async {
        await store.loadMeetingPrompts()
        if store.meetingPromptsError == nil {
            let available = Set(store.meetingPrompts.map(\.promptId))
            promptIds.removeAll { !available.contains($0) }
        }
    }
}

/// One membership read when opened, rather than one read per meeting row.
struct MeetingFilingSheet: View {
    let store: MeetingsStore
    @State private var name = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Add to folder").headlineText()
                    Text(store.filing?.title ?? "").metaText()
                }
                Spacer()
                Button("Done") { store.closeFiling() }
                    .buttonStyle(.primary)
                    .disabled(store.folderBusy)
            }
            Text("A meeting can sit in more than one folder. Turn a folder off to take it out.").metaText()
            ScrollView {
                Card {
                    if store.foldersLoading {
                        CardLine("Reading folders…")
                    } else if let error = store.foldersError {
                        ActionRow(title: error, button: "Try again", action: store.retryFolders)
                    } else if let error = store.filing?.error {
                        ActionRow(title: error, button: "Try again", action: store.retryFiling)
                    } else if let ids = store.filing?.folderIds {
                        if store.folders.isEmpty {
                            CardLine("No folders yet. Create one below.")
                        } else {
                            ForEach(store.folders) { folder in
                                ToggleRow(title: folder.name, detail: folder.countLabel, isOn: Binding(
                                    get: { ids.contains(folder.folderId) },
                                    set: { store.setFiled(folder, $0) }))
                                    .disabled(store.folderBusy)
                            }
                        }
                    } else {
                        CardLine("Reading where this meeting is filed…")
                    }
                }
            }
            .frame(maxHeight: 260)
            ErrorNote(store.folderError)
            HStack(spacing: 12) {
                InputField(prompt: "New folder name", text: $name)
                    .accessibilityLabel("New folder name")
                Button("Create and add") {
                    Task {
                        if await store.createFolderAndFile(named: name) { name = "" }
                    }
                }
                .buttonStyle(.secondary)
                .disabled(store.folderBusy || store.foldersLoading || store.foldersError != nil
                    || store.filing?.folderIds == nil || store.folderNameProblem(name) != nil)
            }
            Text("Use 1–80 characters and a name no other folder has.").metaText()
            if !name.isEmpty, let problem = store.folderNameProblem(name) {
                Text(problem).metaText(Theme.live)
            }
        }
        .padding(24)
        .frame(width: 520)
        .background(Theme.page)
        .interactiveDismissDisabled(store.folderBusy)
    }
}

// MARK: - The list itself

struct MeetingsFeed: View {
    let store: MeetingsStore
    @State private var pendingDelete: MeetingHistorySummary?

    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            if store.loading, store.entries.isEmpty {
                MeetingsSkeleton()
            } else if store.entries.isEmpty {
                Card {
                    CardRow {
                        Text(store.emptyLine).bodyText(15, Theme.inkSecondary)
                    }
                }
            } else {
                ForEach(store.groups) { group in
                    PageSection(group.heading) {
                        Card {
                            ForEach(group.items) { entry in
                                MeetingsRow(
                                    entry: entry,
                                    open: { store.open(entry.sessionId) },
                                    export: { store.export(entry.sessionId, format: $0) },
                                    ledger: { store.exportLedger(entry.sessionId) },
                                    file: { store.openFiling(entry.sessionId, title: entry.title) },
                                    delete: { pendingDelete = entry }
                                )
                            }
                        }
                    }
                }
            }
        }
        .alert("Delete this meeting?", isPresented: confirming, presenting: pendingDelete) { entry in
            Button("Delete", role: .destructive) {
                store.delete(entry.sessionId)
                pendingDelete = nil
            }
            Button("Keep it", role: .cancel) { pendingDelete = nil }
        } message: { entry in
            Text("“\(entry.title)” goes to the trash for 30 days, then Sona removes it for good.")
        }
        .sheet(isPresented: Binding(get: { store.filing != nil }, set: { if !$0 { store.closeFiling() } })) {
            MeetingFilingSheet(store: store)
        }
    }

    private var confirming: Binding<Bool> {
        Binding(get: { pendingDelete != nil }, set: { if !$0 { pendingDelete = nil } })
    }
}

/// One meeting: when it started, what it was, what Sona heard, and how long
/// it ran. The menu carries filing, exports, and deletion.
struct MeetingsRow: View {
    let entry: MeetingHistorySummary
    let open: () -> Void
    let export: (MeetingExportFormat) -> Void
    let ledger: () -> Void
    let file: () -> Void
    let delete: () -> Void

    var body: some View {
        CardRow(action: open) {
            HStack(alignment: .top, spacing: 16) {
                Text(entry.date.time)
                    .font(TypeScale.mono(12))
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(width: 62, alignment: .leading)
                VStack(alignment: .leading, spacing: 4) {
                    Text(entry.title).headlineText()
                    if let headline = entry.headlineText {
                        Text(headline).metaText(Theme.inkSecondary).lineLimit(2)
                    }
                    HStack(spacing: 10) {
                        if let attention = entry.attention {
                            Text(attention.text)
                                .font(TypeScale.label(12))
                                .foregroundStyle(attention.urgent ? Theme.live : Theme.accent)
                        }
                        if entry.captureCompleteness == .partial {
                            Text("Partial recording").metaText()
                        }
                        if let sources = entry.sources, !sources.isEmpty {
                            Text(sources.map(\.label).joined(separator: " + ")).metaText()
                        }
                    }
                }
            }
        } trailing: {
            HStack(spacing: 14) {
                if let duration = entry.durationShort {
                    Text(duration).font(TypeScale.mono(12)).foregroundStyle(Theme.inkSecondary)
                }
                Menu {
                    Button("Add to folder…", action: file)
                    Divider()
                    Button("Export as Markdown") { export(.markdown) }
                    Button("Export as JSON") { export(.json) }
                    if entry.hasLedger {
                        Button("Save the ledger as HTML", action: ledger)
                    }
                    Divider()
                    Button("Delete", role: .destructive, action: delete)
                } label: {
                    Image(systemName: "ellipsis")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.inkSecondary)
                        .frame(width: 28, height: 24)
                        .contentShape(Rectangle())
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
            }
        }
    }
}

/// While the first page reads: the shape of the rows, no invented content.
struct MeetingsSkeleton: View {
    var body: some View {
        PageSection("Loading") {
            Card {
                ForEach(0..<4, id: \.self) { _ in
                    CardRow {
                        VStack(alignment: .leading, spacing: 8) {
                            RoundedRectangle(cornerRadius: 4)
                                .fill(Theme.selection)
                                .frame(width: 220, height: 14)
                            RoundedRectangle(cornerRadius: 4)
                                .fill(Theme.selection.opacity(0.6))
                                .frame(width: 320, height: 11)
                        }
                    }
                }
            }
        }
    }
}

/// Newer and older, and which page this is. Absent while one page holds
/// everything.
struct MeetingsPager: View {
    let store: MeetingsStore

    var body: some View {
        if store.page > 1 || store.hasMore {
            HStack(spacing: 12) {
                Button("Newer") { store.previousPage() }
                    .buttonStyle(SecondaryButton(compact: true))
                    .disabled(store.page == 1)
                Text("Page \(store.page)").metaText()
                Button("Older") { store.nextPage() }
                    .buttonStyle(SecondaryButton(compact: true))
                    .disabled(!store.hasMore)
            }
            .padding(.top, 28)
        }
    }
}

// MARK: - The trash

/// Deleted meetings wait 30 days. Each row says when it goes, and offers a
/// way back or an immediate, permanent deletion.
struct MeetingsTrashSheet: View {
    let store: MeetingsStore
    @State private var deleting: MeetingTrashEntry?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Deleted meetings").font(TypeScale.headline).foregroundStyle(Theme.ink)
                    Text("Sona keeps a deleted meeting for 30 days, then removes it for good.")
                        .metaText(Theme.inkSecondary)
                }
                Spacer(minLength: 20)
                Button("Done") { store.closeTrash() }
                    .buttonStyle(.secondary)
            }
            .padding(24)
            Hairline()
            if let error = store.error {
                ErrorNote(error).padding(.horizontal, 24)
            }
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if store.trashLoading, store.trash.isEmpty {
                        row { Text("Reading the trash…").bodyText(14, Theme.inkTertiary) }
                    } else if store.trash.isEmpty {
                        row { Text("Nothing deleted.").bodyText(14, Theme.inkSecondary) }
                    } else {
                        ForEach(store.trash) { entry in
                            CardRow {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(entry.title).bodyText()
                                    Text(store.expiry(of: entry)).metaText()
                                }
                            } trailing: {
                                HStack(spacing: 12) {
                                    Button("Restore") { store.restore(entry) }
                                        .buttonStyle(SecondaryButton(compact: true))
                                    Button("Delete forever", role: .destructive) { deleting = entry }
                                        .buttonStyle(QuietButton(color: Theme.live))
                                }
                                .disabled(store.restoring != nil || store.discarding != nil)
                            }
                        }
                    }
                }
            }
        }
        .frame(width: 640, height: 420)
        .background(Theme.page)
        .alert("Delete this meeting forever?", isPresented: Binding(
            get: { deleting != nil }, set: { if !$0 { deleting = nil } }), presenting: deleting
        ) { entry in
            Button("Delete forever", role: .destructive) {
                store.deleteForever(entry)
                deleting = nil
            }
            Button("Keep it", role: .cancel) { deleting = nil }
        } message: { entry in
            Text("“\(entry.title)” will be removed now. You cannot restore it after this.")
        }
    }

    private func row<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        HStack { content() }
            .padding(.horizontal, 20)
            .padding(.vertical, 16)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}
