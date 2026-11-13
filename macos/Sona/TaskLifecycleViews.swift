import SwiftUI

/// Tasks archived and tasks in Recently deleted, each with a way back.
struct TaskArchiveScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let store = model.work
        ZStack(alignment: .bottomLeading) {
            VStack(spacing: 0) {
                WorkBar {
                    BackLink(title: "Tasks") { model.go(.tasks) }
                    Text("Archive").sectionLabel()
                } trailing: {
                    EmptyView()
                }
                ScrollView {
                    VStack(alignment: .leading, spacing: 28) {
                        section("Recently deleted") {
                            if let error = store.archiveError {
                                CardLine(error)
                            } else if let deleted = store.archive?.deleted {
                                if deleted.isEmpty {
                                    CardLine("Nothing deleted in the last 30 days.")
                                } else {
                                    ForEach(deleted) { binned in
                                        row(binned.item, note: left(binned))
                                    }
                                }
                            } else {
                                CardLine("Loading…")
                            }
                        }
                        section("Archived") {
                            if let archived = store.archive?.archived {
                                if archived.isEmpty {
                                    CardLine("No archived tasks.")
                                } else {
                                    ForEach(archived) { item in
                                        row(item, note: item.archivedAtMs.map {
                                            "Archived \(TaskDates.label($0, now: store.now).lowercased())"
                                        })
                                    }
                                }
                            } else if store.archiveError == nil {
                                CardLine("Loading…")
                            }
                        }
                    }
                    .frame(maxWidth: Theme.contentMax, alignment: .leading)
                    .padding(Theme.margin)
                    .frame(maxWidth: .infinity)
                }
            }
            UndoToast()
        }
        .task { await store.loadArchive() }
    }

    private func section<Content: View>(_ title: String, @ViewBuilder _ content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title).sectionLabel().accessibilityAddTraits(.isHeader)
            Card { content() }
        }
    }

    private func left(_ binned: WorkBinned) -> String {
        let days = max(0, PlanningFormat.days(from: model.work.now, to: binned.purgeAtMs))
        return days <= 1 ? "Deleted for good tomorrow" : "Deleted for good in \(days) days"
    }

    private func row(_ item: WorkItem, note: String?) -> some View {
        let lit = model.workFocus.archiveHighlighted == item.id
        return HStack(spacing: 10) {
            TaskStatusIcon(status: item.status)
            Text("#\(item.number)").font(TypeScale.mono(12)).foregroundStyle(Theme.inkTertiary)
            Text(item.title).bodyText(13.5).lineLimit(1)
            Spacer()
            if let note {
                Text(note).metaText()
            }
            Button("Restore") { model.changeWork { model.work.restoreFromArchive(item.id) } }
                .buttonStyle(.secondary)
                .help("Restore  #")
        }
        .padding(.horizontal, 16)
        .frame(height: Theme.listRow)
        .background(lit ? Theme.controlHover : Color.clear)
        .contentShape(Rectangle())
        .onHover { inside in
            if inside { model.workFocus.archiveHighlighted = item.id }
        }
    }
}

/// The star, the bell and the description's history, in a task page's bar.
struct TaskLifecycleButtons: View {
    let row: WorkItemRow
    @Environment(AppModel.self) private var model
    @State private var versions: [WorkDescriptionVersion]?

    var body: some View {
        let store = model.work
        let asked = model.workFocus.pageAsk
        let starred = store.isFavorite(.task, row.id)
        let subscription = store.subscriptions[row.id]
        Button {
            store.subscribe(row.id, subscription == nil)
        } label: {
            HStack(spacing: 5) {
                Image(systemName: subscription == nil ? "bell" : "bell.fill")
                Text(subscription?.reason.label ?? "Subscribe")
            }
        }
        .buttonStyle(QuietButton(compact: true))
        .help(subscription == nil ? "Hear about changes in the inbox" : "Stop hearing about changes")
        WorkBarButton(icon: starred ? "star.fill" : "star",
                      help: starred ? "Remove from favorites" : "Add to favorites", on: starred) {
            store.setFavorite(.task, row.id, !starred)
        }
        .accessibilityLabel(starred ? "Remove from favorites" : "Add to favorites")
        WorkBarButton(icon: "clock.arrow.circlepath", help: "Description history", on: versions != nil) {
            if versions == nil {
                Task { versions = await store.descriptionVersions(row.id) }
            } else {
                versions = nil
            }
        }
        .accessibilityLabel("Description history")
        .popover(isPresented: Binding(get: { versions != nil }, set: { if !$0 { versions = nil } }),
                 arrowEdge: .bottom) {
            DescriptionHistory(row: row, versions: versions ?? []) { versions = nil }
                .environment(model)
        }
        .onChange(of: asked, initial: true) {
            guard let asked, asked.id == row.id, asked.part == .history else { return }
            model.workFocus.pageAsk = nil
            Task { versions = await store.descriptionVersions(row.id) }
        }
    }
}

private struct DescriptionHistory: View {
    let row: WorkItemRow
    let versions: [WorkDescriptionVersion]
    let done: () -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Description history").sectionLabel()
            if versions.isEmpty {
                Text("No edits yet.").metaText()
            } else if versions.count > 4 {
                ScrollView { list }.frame(height: 380)
            } else {
                list
            }
        }
        .padding(14)
        .frame(width: 400, alignment: .leading)
    }

    private var list: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(versions.enumerated()), id: \.element.id) { index, version in
                if index > 0 { Hairline() }
                entry(version, current: index == 0)
            }
        }
    }

    private func entry(_ version: WorkDescriptionVersion, current: Bool) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                Text(version.author).font(TypeScale.label(12.5)).foregroundStyle(Theme.ink)
                Text(PlanningFormat.moment(version.createdAtMs).formatted(date: .abbreviated, time: .shortened))
                    .metaText()
                Spacer(minLength: 8)
                if current {
                    Text("Current").metaText()
                } else {
                    Button("Restore") {
                        model.changeWork { model.work.restoreDescription(version) }
                        done()
                    }
                    .buttonStyle(QuietButton(compact: true))
                    .disabled(version.body == row.details)
                }
            }
            Text(version.body ?? "No description")
                .bodyText(12.5, version.body == nil ? Theme.inkTertiary : Theme.inkSecondary)
                .lineLimit(3)
        }
        .padding(.vertical, 8)
    }
}
