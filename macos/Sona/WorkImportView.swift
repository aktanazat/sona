import SwiftUI

/// Settings › Import work: bring tasks, projects and their history over from
/// Linear, a spreadsheet or another tracker, see what would happen first, and
/// take an import back.
struct WorkImportView: View {
    @Bindable var store: WorkImportStore

    @State private var key = ""
    @State private var showingAll = false
    @State private var undoing: WorkImportRecord?
    @State private var dragging = false

    var body: some View {
        Page {
            PageTitle("Import work", subtitle: "Tasks and projects from Linear, a spreadsheet or another tracker.") {
                if store.hasOrigin {
                    Button(importLabel) { Task { await store.runImport() } }
                        .buttonStyle(.primary)
                        .disabled(store.report == nil || store.importing || store.previewing)
                }
            }
            ErrorNote(store.error)
            if let notice = store.notice {
                Text(notice)
                    .bodyText(14, Theme.inkSecondary)
                    .padding(.bottom, 20)
            }
            sourceSection
            previewSection
            copySection
            optionsSection
            historySection
        }
        .task { await store.start() }
        .task(id: store.previewKey) {
            try? await Task.sleep(for: .milliseconds(300))
            guard !Task.isCancelled else { return }
            await store.preview()
        }
        .onDisappear { key = "" }
        .confirmationDialog(
            "Undo this import?",
            isPresented: Binding(get: { undoing != nil }, set: { if !$0 { undoing = nil } }),
            presenting: undoing
        ) { record in
            Button("Undo import", role: .destructive) {
                Task { await store.revert(record) }
            }
        } message: { _ in
            Text("Its tasks and projects go to the bin for 30 days. Labels, initiatives and views it made are deleted.")
        }
    }

    // MARK: - Source

    private var sourceSection: some View {
        PageSection("Source") {
            Card {
                ChoiceRow(
                    title: "From",
                    choices: WorkImportSource.allCases,
                    label: \.title,
                    selection: $store.source)
                originRow
                if store.source == .linear {
                    LinearKeyRow(store: store, key: $key)
                }
            }
            .overlay(
                RoundedRectangle(cornerRadius: Theme.radiusCard)
                    .strokeBorder(dragging ? Theme.accent : .clear, lineWidth: 1))
            .dropDestination(for: URL.self) { urls, _ in
                store.add(urls)
                return true
            } isTargeted: { dragging = $0 }
        }
    }

    private var originRow: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(originTitle)
                    .bodyText()
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(store.hasOrigin ? originDetail : store.source.exportName)
                    .metaText()
            }
            .help(store.files.map(\.path).joined(separator: "\n"))
        } trailing: {
            HStack(spacing: Space.m) {
                if store.source == .csv {
                    Button("Download template") { Task { await store.saveTemplate() } }
                        .buttonStyle(.quiet)
                }
                if store.hasOrigin {
                    Button("Clear") { store.clearOrigin() }
                        .buttonStyle(.quiet)
                }
                Button("Choose files…") { store.chooseFiles() }
                    .buttonStyle(.compact)
            }
        }
    }

    private var originTitle: String {
        if store.live { return "Your Linear workspace" }
        switch store.files.count {
        case 0: return "No files chosen"
        case 1: return store.files[0].lastPathComponent
        default: return store.files.map(\.lastPathComponent).joined(separator: ", ")
        }
    }

    private var originDetail: String {
        if store.live { return "Read through the API with your key" }
        return store.files.count == 1 ? "1 file" : "\(store.files.count) files"
    }

    // MARK: - What comes over

    private var copySection: some View {
        PageSection("Copy") {
            Card {
                CardRow {
                    LazyVGrid(
                        columns: Array(repeating: GridItem(.flexible(), alignment: .leading), count: 4),
                        alignment: .leading,
                        spacing: Space.m
                    ) {
                        ForEach(store.source.copies) { kind in
                            Toggle(kind.title, isOn: $store.options.copy[dynamicMember: kind.keyPath])
                        }
                    }
                    .toggleStyle(.checkbox)
                    .font(TypeScale.body())
                }
            }
        }
    }

    private var optionsSection: some View {
        PageSection("Options") {
            Card {
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Label new tasks").bodyText()
                        Text("Leave empty for no label").metaText()
                    }
                } trailing: {
                    InputField(prompt: "No label", text: labelText)
                        .frame(width: 180)
                }
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Your name there").bodyText()
                        Text("Tasks assigned to it become yours; separate names with commas").metaText()
                    }
                } trailing: {
                    InputField(prompt: "Name", text: meText)
                        .frame(width: 180)
                }
                ToggleRow(
                    title: "Keep their numbers",
                    detail: keepsNumbers ? "AKT-12 becomes S-12" : "Only while Sona has no tasks",
                    isOn: $store.options.keepNumbers)
                    .disabled(!keepsNumbers)
                ToggleRow(
                    title: "Refresh earlier imports",
                    detail: "Update tasks brought in before instead of skipping them",
                    isOn: $store.options.updateExisting)
            }
        }
    }

    private var keepsNumbers: Bool { store.report?.numbersKeepable ?? false }

    private var labelText: Binding<String> {
        Binding(
            get: { store.options.label ?? "" },
            set: {
                let label = $0.trimmingCharacters(in: .whitespaces)
                store.options.label = label.isEmpty ? nil : label
            })
    }

    private var meText: Binding<String> {
        Binding(
            get: { store.options.me.joined(separator: ", ") },
            set: {
                store.options.me = $0.split(separator: ",")
                    .map { $0.trimmingCharacters(in: .whitespaces) }
                    .filter { !$0.isEmpty }
            })
    }

    // MARK: - Preview

    @ViewBuilder
    private var previewSection: some View {
        if store.hasOrigin {
            PageSection("Preview") {
                if let failure = store.previewError {
                    Card {
                        CardRow {
                            Text(failure)
                                .bodyText(14, Theme.live)
                                .fixedSize(horizontal: false, vertical: true)
                                .textSelection(.enabled)
                        }
                    }
                } else if let report = store.report {
                    preview(report)
                } else {
                    Card { CardLine("Reading \(store.live ? "Linear" : "the files")…") }
                }
            }
        }
    }

    private func preview(_ report: WorkImportReport) -> some View {
        let found = report.totals.found
        return VStack(alignment: .leading, spacing: Space.m) {
            Card {
                StateRow(title: "Active", detail: "Open and touched in the last six months", count: found.active, isOn: $store.options.include.active)
                StateRow(title: "Stale", detail: "Open and untouched for six months, or archived there; lands in the archive", count: found.stale, isOn: $store.options.include.stale)
                StateRow(title: "Finished", detail: "Done or canceled; lands in the archive", count: found.completed, isOn: $store.options.include.completed)
            }
            Card {
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(report.totals.madeSentence).bodyText()
                        if let tally = tallySentence(report.totals) {
                            Text(tally).metaText()
                        }
                    }
                }
                ForEach(report.notes, id: \.self) { note in
                    CardLine(note)
                }
                ForEach(shownRows(report)) { row in
                    PreviewRow(row: row)
                }
                if report.rows.count > Self.firstRows {
                    CardRow(action: { showingAll.toggle() }) {
                        Text(showingAll ? "Show fewer" : "Show all \(report.rows.count)")
                            .bodyText(13, Theme.inkSecondary)
                    }
                }
            }
        }
    }

    private static let firstRows = 8

    private func shownRows(_ report: WorkImportReport) -> [WorkImportRow] {
        showingAll ? report.rows : Array(report.rows.prefix(Self.firstRows))
    }

    /// The counts beside what is new: archived, already here, left out.
    private func tallySentence(_ totals: WorkImportTotals) -> String? {
        let tasks = totals.tasks
        var parts: [String] = []
        if tasks.archived > 0 { parts.append("\(tasks.archived) to the archive") }
        if tasks.skipped > 0 { parts.append("\(tasks.skipped) already here") }
        if tasks.updated > 0 { parts.append("\(tasks.updated) updated") }
        if tasks.leftOut > 0 { parts.append("\(tasks.leftOut) left out") }
        if totals.inCycles > 0 { parts.append("\(totals.inCycles) in a cycle") }
        return parts.isEmpty ? nil : "Tasks: " + parts.joined(separator: ", ")
    }

    private var importLabel: String {
        if store.importing { return "Importing…" }
        let tasks = store.report?.totals.tasks.brought ?? 0
        switch tasks {
        case 0: return "Import"
        case 1: return "Import 1 task"
        default: return "Import \(tasks) tasks"
        }
    }

    // MARK: - History

    private var historySection: some View {
        PageSection("Past imports") {
            Card {
                if store.history.isEmpty {
                    CardLine("No imports yet.")
                }
                ForEach(store.history) { record in
                    ImportHistoryRow(record: record, busy: store.reverting == record.id) {
                        undoing = record
                    }
                }
            }
        }
    }
}

/// One of the three kinds of task the export holds, with its count, and
/// whether it comes over.
private struct StateRow: View {
    let title: String
    let detail: String
    let count: Int
    @Binding var isOn: Bool

    var body: some View {
        CardRow {
            Toggle(isOn: $isOn) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(title).bodyText()
                    Text(detail).metaText()
                }
            }
            .toggleStyle(.checkbox)
        } trailing: {
            Text("\(count)")
                .font(TypeScale.list)
                .monospacedDigit()
                .foregroundStyle(isOn ? Theme.ink : Theme.inkTertiary)
        }
    }
}

private struct PreviewRow: View {
    let row: WorkImportRow

    var body: some View {
        CardRow {
            HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                Text(row.sourceRef ?? row.kind.rawValue.capitalized)
                    .metaText()
                    .frame(width: 72, alignment: .leading)
                VStack(alignment: .leading, spacing: 2) {
                    Text(row.title)
                        .bodyText(13)
                        .lineLimit(1)
                        .truncationMode(.tail)
                    if let reason = row.reason {
                        Text(reason).metaText()
                    }
                }
            }
        } trailing: {
            Text(row.action.word)
                .metaText(row.action == .leftOut ? Theme.inkTertiary : Theme.inkSecondary)
        }
    }
}

private struct ImportHistoryRow: View {
    let record: WorkImportRecord
    let busy: Bool
    let undo: () -> Void

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(record.name)
                    .bodyText()
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text("\(record.source.title) · \(record.date.formatted(date: .abbreviated, time: .shortened)) · \(record.totals.madeSentence)")
                    .metaText()
            }
        } trailing: {
            if record.revertedAtMs != nil {
                Text("Undone").metaText()
            } else {
                Button(busy ? "Undoing…" : "Undo", action: undo)
                    .buttonStyle(.compact)
                    .disabled(busy)
            }
        }
    }
}

/// Connecting Linear: the key is handed to the core, which keeps it in
/// Keychain and never gives it back.
private struct LinearKeyRow: View {
    let store: WorkImportStore
    @Binding var key: String

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text("Linear API key").bodyText()
                Text(state).metaText(store.keyError == nil ? Theme.inkTertiary : Theme.live)
            }
        } trailing: {
            if !keychainOff {
                controls.disabled(store.keyBusy)
            }
        }
    }

    private var controls: some View {
        HStack(spacing: Space.m) {
            if store.linearKey?.configured == true {
                Button("Disconnect") { Task { await store.removeLinearKey() } }
                    .buttonStyle(.quiet)
                Button("Read workspace") { store.readLinearWorkspace() }
                    .buttonStyle(.compact)
                    .disabled(store.live)
            } else {
                if let url = URL(string: "https://linear.app/settings/account/security") {
                    Link("Get a key", destination: url)
                        .font(TypeScale.list)
                }
                SecureField("lin_api_…", text: $key)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 180)
                    .accessibilityLabel("Linear API key")
                    .onSubmit(connect)
                Button("Connect", action: connect)
                    .buttonStyle(.compact)
                    .disabled(trimmed.isEmpty)
            }
        }
    }

    private var trimmed: String { key.trimmingCharacters(in: .whitespacesAndNewlines) }

    /// Portable copies of Sona keep no secrets, so there is nothing to type a key into.
    private var keychainOff: Bool { store.linearKey?.lastErrorKind == SecretFault("unavailable") }

    private var state: String {
        if let error = store.keyError { return error }
        guard let key = store.linearKey else { return "Reading Keychain…" }
        if key.configured { return "Saved in Keychain" }
        if let fault = key.lastErrorKind, fault.raw != "not_found" { return fault.sentence }
        return "Not connected"
    }

    private func connect() {
        let token = trimmed
        guard !token.isEmpty else { return }
        Task {
            if await store.saveLinearKey(token) { key = "" }
        }
    }
}
