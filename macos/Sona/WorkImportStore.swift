import AppKit
import Foundation
import UniformTypeIdentifiers

/// Bringing work in from another tracker: the files or the Linear workspace
/// it reads, what comes over, the core's dry run of it, and every import
/// since, each of which can be undone.
///
/// The preview is the core's own writer run in a transaction it never keeps,
/// so its counts are the import's counts and nothing on this screen guesses.
@MainActor
@Observable
final class WorkImportStore {
    var source: WorkImportSource = .linear {
        didSet {
            guard source != oldValue else { return }
            files = []
            live = false
            report = nil
            previewError = nil
        }
    }
    var options = WorkImportOptions()
    private(set) var files: [URL] = []
    /// Read the Linear workspace through its API instead of files.
    private(set) var live = false

    private(set) var report: WorkImportReport?
    private(set) var previewing = false
    private(set) var previewError: String?

    private(set) var importing = false
    /// What the last import or undo did, in a sentence.
    private(set) var notice: String?
    private(set) var error: String?

    private(set) var history: [WorkImportRecord] = []
    private(set) var reverting: String?

    private(set) var linearKey: SecretState?
    private(set) var keyBusy = false
    private(set) var keyError: String?

    @ObservationIgnored private let core: Core

    init(core: Core) {
        self.core = core
    }

    func start() async {
        await loadHistory()
        await loadLinearKey()
    }

    var hasOrigin: Bool { live || !files.isEmpty }

    /// Everything the preview depends on; a change runs it again.
    var previewKey: PreviewKey {
        PreviewKey(source: source, files: files, live: live, options: options)
    }

    struct PreviewKey: Hashable {
        let source: WorkImportSource
        let files: [URL]
        let live: Bool
        let options: WorkImportOptions
    }

    // MARK: - Where the work comes from

    func chooseFiles() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = true
        panel.message = source.exportName
        panel.allowedContentTypes = source.fileExtensions.compactMap { UTType(filenameExtension: $0) }
        guard panel.runModal() == .OK else { return }
        add(panel.urls)
    }

    /// Files from the picker, a drop, or opened with Sona.
    func add(_ urls: [URL]) {
        let fresh = urls.filter { !files.contains($0) }
        guard !fresh.isEmpty else { return }
        files += fresh
        live = false
        notice = nil
    }

    func readLinearWorkspace() {
        files = []
        live = true
        notice = nil
    }

    func clearOrigin() {
        files = []
        live = false
        report = nil
        previewError = nil
    }

    func saveTemplate() async {
        do {
            let template: String = try await core.request("work_import_template")
            let panel = NSSavePanel()
            panel.nameFieldStringValue = "Sona import template.csv"
            panel.allowedContentTypes = [.commaSeparatedText]
            guard panel.runModal() == .OK, let url = panel.url else { return }
            try template.write(to: url, atomically: true, encoding: .utf8)
            error = nil
        } catch {
            self.error = "The template wasn't saved. \(error.localizedDescription)"
        }
    }

    // MARK: - Preview and import

    func preview() async {
        guard hasOrigin else {
            report = nil
            previewError = nil
            return
        }
        let key = previewKey
        previewing = true
        defer { previewing = false }
        do {
            let result: WorkImportReport = try await core.request("work_import_preview", params(nil))
            guard key == previewKey else { return }
            report = result
            previewError = nil
            if !result.numbersKeepable, options.keepNumbers {
                options.keepNumbers = false
            }
        } catch {
            guard key == previewKey else { return }
            report = nil
            previewError = error.localizedDescription
        }
    }

    func runImport() async {
        guard hasOrigin, !importing else { return }
        importing = true
        notice = nil
        error = nil
        defer { importing = false }
        do {
            let done: WorkImportReport = try await core.request(
                "work_import", params(UUID().uuidString.lowercased()))
            notice = "Imported \(done.totals.madeSentence.lowercased()) from \(done.name)."
            files = []
            live = false
            report = nil
            await loadHistory()
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func params(_ operationId: String?) -> ImportParams {
        ImportParams(
            source: source,
            paths: files.map(\.path),
            live: live,
            options: options,
            operationId: operationId)
    }

    // MARK: - History

    func loadHistory() async {
        do {
            history = try await core.request("work_imports")
        } catch {
            self.error = "Past imports couldn't be read. \(error.localizedDescription)"
        }
    }

    func revert(_ record: WorkImportRecord) async {
        guard reverting == nil else { return }
        reverting = record.id
        notice = nil
        error = nil
        defer { reverting = nil }
        do {
            let reverted: WorkImportReverted = try await core.request(
                "work_import_revert",
                RevertParams(operationId: UUID().uuidString.lowercased(), id: record.id))
            notice = Self.undoneSentence(reverted.undone, name: record.name)
            await loadHistory()
        } catch {
            self.error = error.localizedDescription
        }
    }

    private static func undoneSentence(_ undone: WorkImportUndone, name: String) -> String {
        let tasks = undone.tasks == 1 ? "1 task" : "\(undone.tasks) tasks"
        let projects = undone.projects == 1 ? "1 project" : "\(undone.projects) projects"
        let kept = undone.notesKept == 0
            ? ""
            : undone.notesKept == 1 ? " 1 note you changed stays." : " \(undone.notesKept) notes you changed stay."
        return "Undid \(name): \(tasks) and \(projects) moved to the bin for 30 days.\(kept)"
    }

    // MARK: - The Linear key

    func loadLinearKey() async {
        do {
            linearKey = try await core.request("work_import_linear_key")
            keyError = nil
        } catch {
            keyError = error.localizedDescription
        }
    }

    func saveLinearKey(_ token: String) async -> Bool {
        guard !keyBusy else { return false }
        keyBusy = true
        keyError = nil
        defer { keyBusy = false }
        do {
            linearKey = try await core.request("work_import_linear_key_set", TokenParams(token: token))
            return true
        } catch {
            keyError = error.localizedDescription
            return false
        }
    }

    func removeLinearKey() async {
        guard !keyBusy else { return }
        keyBusy = true
        keyError = nil
        defer { keyBusy = false }
        do {
            linearKey = try await core.request("work_import_linear_key_remove")
            if live { clearOrigin() }
        } catch {
            keyError = error.localizedDescription
        }
    }
}

private struct ImportParams: Encodable {
    let source: WorkImportSource
    let paths: [String]
    let live: Bool
    let options: WorkImportOptions
    let operationId: String?
}

private struct RevertParams: Encodable {
    let operationId: String
    let id: String
}

private struct TokenParams: Encodable {
    let token: String
}
