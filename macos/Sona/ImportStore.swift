import AppKit
import Foundation
import UniformTypeIdentifiers

/// Bringing recordings in from disk, and watching them land.
///
/// The picker on its own was the whole import surface: one file, no sight of
/// it after the click. Here a chosen file becomes a row, the row follows the
/// core's own job, and the jobs keep reporting after the dialog closes —
/// Library's list and this one are the same jobs, so the two can never
/// disagree about what is running.
@MainActor
@Observable
final class ImportStore {
    /// Every file import the core knows about: the ones queued before the
    /// shell opened, and the ones started here.
    private(set) var jobs: [AudioImportJob] = []
    /// The dialog's own list, one row per chosen file.
    private(set) var rows: [ImportRow] = []
    /// Where a file the OS handed to Sona ended up. Nobody was asked on that
    /// route, so it is the one import that has to say where it went.
    private(set) var routed: AudioImportRouted?
    private(set) var running = false
    private(set) var error: String?

    /// A finished dictation import has a history row behind it; the shell
    /// reloads Library on this.
    var jobCompleted: (Int64) -> Void = { _ in }

    @ObservationIgnored private let core: Core
    /// The jobs already reported as finished, so a repeated update does not
    /// reload Library twice.
    @ObservationIgnored private var completed = Set<Int64>()

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.audioImportUpdateEvent) { [weak self] line in
            guard let update: AudioImportUpdate = try? Core.payload(line) else { return }
            self?.apply(update.job)
        }
        core.observe(CoreEvent.audioImportRoutedEvent) { [weak self] line in
            guard let routed: AudioImportRouted = try? Core.payload(line) else { return }
            self?.routed = routed
        }
    }

    func start() async {
        do {
            jobs = try await core.request("list_audio_import_jobs")
            error = nil
        } catch {
            self.error = "Could not load file import status. \(error.localizedDescription)"
        }
    }

    // MARK: - The dialog's list

    /// A fresh opening is a fresh list, not the last import's leftovers.
    func clearRows() {
        rows = []
        error = nil
    }

    /// The native picker, multi-select, limited to what the core's importer
    /// reads. A dismissed picker is somebody changing their mind, not a failure.
    func chooseFiles() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = true
        panel.message = "Audio and video files"
        panel.allowedContentTypes = ImportMedia.extensions.compactMap {
            UTType(filenameExtension: $0)
        }
        guard panel.runModal() == .OK else { return }
        add(paths: panel.urls.map(\.path))
    }

    /// Files from the picker, or dropped on the window by the OS.
    func add(paths: [String]) {
        rows = ImportMedia.add(paths, to: rows)
    }

    /// The files still waiting to be handed to the core.
    var pending: Int {
        rows.filter { $0.state == .ready }.count
    }

    /// Hand every ready row to the core, in the order they were chosen. Each
    /// one is queued there and keeps reporting through its job, so the dialog
    /// can be closed over a run without stopping anything.
    func runImport() async {
        guard !running else { return }
        let paths = rows.filter { $0.state == .ready }.map(\.path)
        guard !paths.isEmpty else { return }
        running = true
        defer { running = false }
        var refused = false
        for path in paths {
            do {
                let job: AudioImportJob = try await core.request("import_audio_file", ["path": path])
                update(path) { row in
                    row.state = .queued
                    row.jobId = job.id
                    row.failure = nil
                }
                apply(job)
            } catch {
                refused = true
                update(path) { row in
                    row.state = .failed
                    row.failure = nil
                }
            }
        }
        // One report per run, not one per file: the rows already name which
        // file was refused.
        error = refused ? "Couldn't start the file import. Try again." : nil
    }

    // MARK: - The queue

    /// The jobs a reader can still stop.
    var live: [AudioImportJob] {
        jobs.filter { !$0.cancelRequested && $0.status.isRunning }
    }

    /// The one sentence that describes a run in progress, for the list header.
    var liveSentence: String? {
        let live = live
        if live.count > 1 { return "Transcribing \(live.count) files" }
        guard let first = live.first else { return nil }
        return "\(first.fileName) · \(first.status.word)"
    }

    func cancel(_ job: AudioImportJob) async {
        guard job.canCancel else { return }
        do {
            let updated: AudioImportJob = try await core.request(
                "cancel_audio_import", ["jobId": job.id])
            apply(updated)
            error = nil
        } catch {
            self.error = "Could not cancel the file import. \(error.localizedDescription)"
        }
    }

    // MARK: - The routed notice

    /// Open the meeting or dictation a routed file became.
    func openRouted() async {
        guard let link = routed?.link else { return }
        do {
            _ = try await core.request("sona_open_link", ["link": link]) as Bool
            routed = nil
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    func dismissRouted() {
        routed = nil
    }

    // MARK: - Updates

    private func apply(_ job: AudioImportJob) {
        var next = jobs.filter { $0.id != job.id }
        next.append(job)
        jobs = next.sorted { $0.id < $1.id }
        if let index = rows.firstIndex(where: { $0.jobId == job.id }) {
            let state = ImportMedia.rowState(for: job)
            rows[index].state = state.state
            rows[index].failure = state.failure
        }
        if job.status == .done, !completed.contains(job.id) {
            completed.insert(job.id)
            jobCompleted(job.id)
        }
    }

    private func update(_ path: String, _ change: (inout ImportRow) -> Void) {
        guard let index = rows.firstIndex(where: { $0.path == path }) else { return }
        change(&rows[index])
    }
}

/// The documents Sona was given to read: what is there, one more, one fewer.
///
/// A document is text a person handed over deliberately — a brief, a spec, a
/// page of notes — and the list is scoped to a person when one is open and to
/// the whole corpus when none is.
@MainActor
@Observable
final class DocumentStore {
    private(set) var documents: [DocumentEntry] = []
    /// The revision the next mutation must name; the core refuses a stale one.
    private(set) var revision = 0
    private(set) var loadFailed = false
    private(set) var busy = false
    private(set) var error: String?

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var personId: String?

    init(core: Core) {
        self.core = core
    }

    func start() async {
        await load(personId: nil)
    }

    /// Everything, or one person's. The scope is remembered, so a mutation
    /// reloads the same list it changed.
    func load(personId: String?) async {
        self.personId = personId
        do {
            let result: DocumentListResult = try await core.request(
                "doc_list", ["personId": personId.map(JSONValue.string) ?? .null])
            documents = result.entries
            revision = result.revision
            loadFailed = false
            error = nil
        } catch {
            loadFailed = true
            self.error = "Documents couldn't be loaded. \(error.localizedDescription)"
        }
    }

    /// Read one text file from disk into the corpus. The operation id is the
    /// core's idempotency key: the same import twice is one document.
    func importDocument() async {
        guard !busy else { return }
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.message = "Text documents"
        panel.allowedContentTypes = DocumentMedia.extensions.compactMap {
            UTType(filenameExtension: $0)
        }
        guard panel.runModal() == .OK, let url = panel.url else { return }
        busy = true
        defer { busy = false }
        do {
            let request: [String: JSONValue] = [
                "path": .string(url.path),
                "operation_id": .string(UUID().uuidString),
            ]
            let _: DocumentMutationResult = try await core.request("doc_ingest", ["request": request])
            error = nil
            await load(personId: personId)
        } catch {
            self.error = error.localizedDescription
        }
    }

    func delete(id: String) async {
        guard !busy, !loadFailed else { return }
        busy = true
        defer { busy = false }
        do {
            let request: [String: JSONValue] = [
                "document_id": .string(id),
                "expected_revision": .number(Double(revision)),
            ]
            let _: DocumentMutationResult = try await core.request("doc_delete", ["request": request])
            error = nil
            await load(personId: personId)
        } catch {
            self.error = error.localizedDescription
        }
    }
}

/// The query plane: one search over everything Sona holds, what happened
/// since you last looked, the evidence bundle behind one question, and the
/// `sona://` addresses all three carry.
///
/// Opening an address goes through the core deliberately: the core owns what
/// an address means and which surface it wakes, so a row here lands exactly
/// where the OS handing the app that URL would land. Anything the core cannot
/// route itself comes back as `query:link-requested`, which the shell routes.
@MainActor
@Observable
final class QueryStore {
    static let searchLimit = 12

    var scope: QueryScope = .all
    private(set) var rows: [QueryRow] = []
    /// Why the page reads the way it does, when something other than the
    /// corpus decided it.
    private(set) var reason: QueryPageReason?
    private(set) var events: [QueryEvent] = []
    private(set) var searching = false
    private(set) var error: String?

    /// A `sona://` noun the core has no navigation of its own for. The
    /// integrator sets this and routes it to the right screen.
    var onLink: (QueryLinkTarget) -> Void = { _ in }
    /// The last request, kept so a screen mounted after the event still sees it.
    private(set) var linkRequest: QueryLinkTarget?

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var cursor: QueryCursor?
    @ObservationIgnored private var query = ""
    @ObservationIgnored private var eventCursor: String?

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.queryLinkRequested) { [weak self] line in
            guard let request: QueryLinkRequest = try? Core.payload(line) else { return }
            self?.linkRequest = request.target
            self?.onLink(request.target)
        }
    }

    func start() async {
        await loadEvents()
    }

    /// One search against the plane, from the top.
    func search(_ text: String) async {
        query = text
        cursor = nil
        await page(replacing: true)
    }

    /// The next page of the current search. Silent when the last page was the end.
    func loadMore() async {
        guard cursor != nil else { return }
        await page(replacing: false)
    }

    private func page(replacing: Bool) async {
        guard !searching else { return }
        searching = true
        defer { searching = false }
        do {
            let page: QuerySearchPage = try await core.request(
                "sona_query_search",
                [
                    "scope": .string(scope.rawValue),
                    "query": .string(query),
                    "limit": .number(Double(Self.searchLimit)),
                    "cursor": cursor.map { (try? JSONValue($0)) ?? .null } ?? .null,
                ] as [String: JSONValue])
            rows = replacing ? page.entries : rows + page.entries
            cursor = page.nextCursor
            reason = page.reason
            error = nil
        } catch let failure as CoreError {
            let code = failure.remote(as: QueryFailure.self)
            // A cursor the corpus no longer knows is not a dead end: the first
            // page still exists, and that is where a reader belongs.
            if code == .unknownCursor, !replacing {
                cursor = nil
                await page(replacing: true)
                return
            }
            error = code?.sentence ?? failure.localizedDescription
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// What happened, newest first, since the last line this store saw.
    func loadEvents() async {
        do {
            let page: QueryEventsPage = try await core.request(
                "sona_query_events",
                [
                    "afterId": eventCursor.map(JSONValue.string) ?? .null,
                    "limit": .number(Double(Self.searchLimit)),
                ] as [String: JSONValue])
            events = page.entries + events
            eventCursor = page.nextCursor
            error = nil
        } catch let failure as CoreError {
            error = failure.remote(as: QueryFailure.self)?.sentence ?? failure.localizedDescription
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// The evidence for one question: the corpus card and the rows that
    /// matched, quoted with their addresses. A turn sent without this would be
    /// answered from the model's own priors and cited to nothing.
    func pack(question: String) async -> QueryPack? {
        do {
            let pack: QueryPack = try await core.request("sona_query_pack", ["question": question])
            error = nil
            return pack
        } catch let failure as CoreError {
            error = failure.remote(as: QueryFailure.self)?.sentence ?? failure.localizedDescription
            return nil
        } catch {
            self.error = error.localizedDescription
            return nil
        }
    }

    /// Open one `sona://` address. Returns what the core did with it: false
    /// means the address named nothing this app can open.
    @discardableResult
    func open(link: String) async -> Bool {
        do {
            let opened: Bool = try await core.request("sona_open_link", ["link": link])
            error = nil
            return opened
        } catch {
            self.error = error.localizedDescription
            return false
        }
    }

    /// Marks the last link request handled, so a screen does not act on it twice.
    func clearLinkRequest() {
        linkRequest = nil
    }
}
