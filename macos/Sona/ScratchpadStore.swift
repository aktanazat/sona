import Foundation

/// One note as the core keeps it: the words, the title they earn, and when.
struct ScratchNote: Decodable, Identifiable, Equatable {
    let id: String
    /// The first words of the body, or "Untitled" while it is too short to
    /// name itself. The core derives it on every save.
    var title: String
    var body: String
    var pinned: Bool
    let createdAtMs: Int64
    var updatedAtMs: Int64

    /// The words after the ones the title took, on one line, for a list row.
    /// Empty when the title already says everything.
    var preview: String {
        var lines = body.split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        let titleStem = title.trimmingCharacters(in: CharacterSet(charactersIn: "…"))
        if let first = lines.first, first.hasPrefix(titleStem) {
            lines.removeFirst()
        }
        return lines.joined(separator: " ")
    }
}

/// One saved state of a note. The core lists them newest first; the newest
/// is the note's body now.
struct ScratchNoteVersion: Decodable, Identifiable, Equatable {
    enum Kind: String, Decodable {
        case edit
        case restore
    }

    let id: Int64
    let noteId: String
    let kind: Kind
    let body: String
    let savedAtMs: Int64
}

/// `scratchpad-changed-event`: some note was written or deleted, by this
/// window or another caller.
struct ScratchpadChanged: Decodable {
    let noteId: String
    let deleted: Bool
}

extension CoreEvent {
    static let scratchpadChanged = "scratchpad-changed-event"
}

private struct ScratchQueryParams: Encodable {
    let query: String
}

private struct ScratchIdParams: Encodable {
    let id: String
}

private struct ScratchCreateParams: Encodable {
    let body: String
}

private struct ScratchBodyParams: Encodable {
    let id: String
    let body: String
}

private struct ScratchPinParams: Encodable {
    let id: String
    let pinned: Bool
}

private struct ScratchRestoreParams: Encodable {
    let id: String
    let versionId: Int64
}

/// The scratchpad: the notes, the search over them, and the one note open in
/// the editor with its autosave.
///
/// The editor saves 1.2 seconds after the last keystroke, when it loses
/// focus, and when the note is left. A blank body is never saved: a new note
/// with nothing in it is dropped on leaving, and an existing note emptied
/// keeps its last words. The core refuses blanks too; the editor only avoids
/// asking.
@MainActor
@Observable
final class ScratchpadStore {
    enum Phase: Equatable {
        case loading
        case ready
        case failed(String)
    }

    /// Where the editor's words stand against the store.
    enum SaveState: Equatable {
        case clean
        case unsaved
        case saving
        /// Nothing but whitespace: nothing that will be written.
        case blank
    }

    /// The notes matching `activeQuery`: pinned first, then newest modified.
    private(set) var notes: [ScratchNote] = []
    private(set) var phase: Phase = .loading
    /// The search the rows on screen answer to.
    private(set) var activeQuery = ""
    /// What is in the search field. Typing settles into `activeQuery`.
    var query = "" {
        didSet { scheduleSearch() }
    }

    /// The editor is showing a note, new or saved.
    private(set) var editing = false
    /// The open note's id, once it has one. A new note earns its id with its
    /// first saved words.
    private(set) var openId: String?
    /// The words in the editor.
    private(set) var body = ""
    /// The last state the core confirmed for the open note.
    private(set) var saved: ScratchNote?
    private(set) var saveState: SaveState = .clean
    /// Bumped when a note is opened, so the editor takes focus.
    private(set) var focusRequest = 0
    /// The last thing the core could not do, until the next success.
    private(set) var error: String?

    /// The open note's saved states, once the history is asked for.
    private(set) var versions: [ScratchNoteVersion]?
    private(set) var versionsLoading = false
    private(set) var versionsError: String?
    private(set) var restoring = false

    /// Quit may proceed only when there is neither a write on its way nor
    /// nonblank words the core has not acknowledged.
    var hasPendingSave: Bool {
        inFlight != nil || hasUnsavedWords
    }

    private var hasUnsavedWords: Bool {
        editing && !isBlank(body) && body != saved?.body
    }

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var loaded = false
    @ObservationIgnored private var searchTask: Task<Void, Never>?
    @ObservationIgnored private var saveTask: Task<Void, Never>?
    /// One write at a time, shared by every flush. Its result belongs to all
    /// callers waiting on it, so a failed create cannot look like a saved draft.
    @ObservationIgnored private var inFlight: Task<Bool, Never>?
    /// Changes when a different note takes the editor. A save that comes back
    /// for an earlier one updates the list and leaves the editor alone.
    @ObservationIgnored private var session = UUID()

    private static let searchPause = Duration.milliseconds(200)
    private static let savePause = Duration.milliseconds(1200)

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.scratchpadChanged) { [weak self] line in
            guard let change: ScratchpadChanged = try? Core.payload(line) else { return }
            Task { await self?.changed(change) }
        }
    }

    // MARK: - The list

    /// The first load, and the read the screen falls back on after a failure.
    func start() async {
        if !loaded { phase = .loading }
        await load()
    }

    private func load() async {
        do {
            let list: [ScratchNote] = try await core.request(
                "scratchpad_list", ScratchQueryParams(query: activeQuery))
            notes = list
            loaded = true
            phase = .ready
        } catch {
            phase = .failed(error.localizedDescription)
        }
    }

    private func scheduleSearch() {
        searchTask?.cancel()
        let typed = query
        searchTask = Task { [weak self] in
            try? await Task.sleep(for: Self.searchPause)
            guard !Task.isCancelled, let self, self.query == typed, self.activeQuery != typed else { return }
            self.activeQuery = typed
            await self.load()
        }
    }

    /// Puts one note the core just confirmed where the list keeps it, or
    /// drops it when it no longer matches the search.
    private func merge(_ note: ScratchNote) {
        notes.removeAll { $0.id == note.id }
        guard matches(note, activeQuery) else { return }
        notes.append(note)
        notes.sort { lhs, rhs in
            if lhs.pinned != rhs.pinned { return lhs.pinned }
            if lhs.updatedAtMs != rhs.updatedAtMs { return lhs.updatedAtMs > rhs.updatedAtMs }
            return lhs.createdAtMs > rhs.createdAtMs
        }
    }

    /// The core's rule, so a merged row lands as the next read would put it:
    /// the trimmed phrase, regardless of case, in the title or the body.
    private func matches(_ note: ScratchNote, _ query: String) -> Bool {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if needle.isEmpty { return true }
        return note.title.lowercased().contains(needle) || note.body.lowercased().contains(needle)
    }

    private func changed(_ change: ScratchpadChanged) async {
        await load()
        guard editing, change.noteId == openId else { return }
        if change.deleted {
            leaveEditor()
            return
        }
        // Words of our own in the editor outrank whatever another caller
        // wrote; they are saved over it next.
        guard saveState == .clean, inFlight == nil,
            let note = notes.first(where: { $0.id == change.noteId })
        else { return }
        saved = note
        if body != note.body {
            body = note.body
            versions = nil
        }
    }

    // MARK: - The editor

    /// Opens one note only after the current draft is saved. A failed save
    /// leaves its words and error in the editor.
    func open(_ note: ScratchNote) async {
        let current = session
        guard await flush(), current == session, !hasPendingSave else { return }
        take(notes.first(where: { $0.id == note.id }) ?? note)
    }

    /// An empty editor. The note is only made once it has words.
    func newNote() async {
        let current = session
        guard await flush(), current == session, !hasPendingSave else { return }
        take(nil)
    }

    private func take(_ note: ScratchNote?) {
        session = UUID()
        openId = note?.id
        saved = note
        body = note?.body ?? ""
        saveState = .clean
        versions = nil
        versionsError = nil
        error = nil
        editing = true
        focusRequest += 1
    }

    /// Bring a retained draft back into focus after quitting was cancelled.
    func focusDraft() {
        guard editing else { return }
        focusRequest += 1
    }

    /// Every keystroke, and every dictation the system lands in the editor.
    func typeBody(_ text: String) {
        guard editing, text != body else { return }
        body = text
        saveState = isBlank(text) ? .blank : (text == saved?.body ? .clean : .unsaved)
        scheduleSave()
    }

    private func scheduleSave() {
        saveTask?.cancel()
        saveTask = Task { [weak self] in
            try? await Task.sleep(for: Self.savePause)
            guard !Task.isCancelled, let self else { return }
            _ = await self.flush()
        }
    }

    /// Drains the current draft, including words entered during a write.
    /// Every caller awaits the same write before starting another, so a new
    /// note earns exactly one id. A failed save keeps the draft and reports
    /// failure to any caller about to replace it or shut down the core.
    @discardableResult
    func flush() async -> Bool {
        let current = session
        while current == session {
            saveTask?.cancel()
            saveTask = nil
            if let task = inFlight {
                let succeeded = await task.value
                if inFlight == task { inFlight = nil }
                guard current == session else { return false }
                guard succeeded else {
                    saveTask?.cancel()
                    saveTask = nil
                    return false
                }
                // Re-read the body after every await, not the snapshot the
                // completed write carried.
                continue
            }
            guard hasUnsavedWords else { return true }
            let text = body
            let id = openId
            saveState = .saving
            inFlight = Task { await self.save(session: current, id: id, text: text) }
        }
        return false
    }

    private func save(session current: UUID, id: String?, text: String) async -> Bool {
        do {
            let note: ScratchNote =
                if let id {
                    try await core.request("scratchpad_update_body", ScratchBodyParams(id: id, body: text))
                } else {
                    try await core.request("scratchpad_create", ScratchCreateParams(body: text))
                }
            merge(note)
            guard current == session else { return false }
            openId = note.id
            saved = note
            error = nil
            saveState = body == text ? .clean : (isBlank(body) ? .blank : .unsaved)
            return true
        } catch {
            guard current == session else { return false }
            saveState = isBlank(body) ? .blank : .unsaved
            self.error = error.localizedDescription
            return false
        }
    }

    /// Losing the page does not discard a failed save. An empty new editor
    /// is still dropped, and clearing a saved note still keeps its last words.
    func leave() async {
        let current = session
        guard await flush(), current == session, !hasPendingSave else { return }
        if editing, openId == nil, isBlank(body) {
            leaveEditor()
        }
    }

    /// Back to the list only when leaving loses no unsaved words.
    func close() async {
        let current = session
        guard await flush(), current == session, !hasPendingSave else { return }
        leaveEditor()
    }

    private func leaveEditor() {
        saveTask?.cancel()
        saveTask = nil
        session = UUID()
        editing = false
        openId = nil
        saved = nil
        body = ""
        saveState = .clean
        versions = nil
    }

    // MARK: - Pin, delete, history

    func togglePin() async {
        guard let id = openId, let note = saved else { return }
        do {
            let pinned: ScratchNote = try await core.request(
                "scratchpad_set_pinned", ScratchPinParams(id: id, pinned: !note.pinned))
            merge(pinned)
            if openId == id { saved = pinned }
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// Removes the open note and its history, after the alert agreed.
    func delete() async {
        guard let id = openId else {
            leaveEditor()
            return
        }
        saveTask?.cancel()
        _ = await inFlight?.value
        do {
            try await core.request("scratchpad_delete", ScratchIdParams(id: id))
            notes.removeAll { $0.id == id }
            error = nil
            if openId == id { leaveEditor() }
        } catch {
            self.error = error.localizedDescription
        }
    }

    func loadVersions() async {
        let current = session
        versionsLoading = true
        versionsError = nil
        defer { versionsLoading = false }
        // Read history only after the current words have reached it. A
        // failed flush must not present an older version as "Current".
        guard await flush() else {
            if current == session { versionsError = error }
            return
        }
        guard current == session, !hasPendingSave else { return }
        guard let id = openId else {
            versions = []
            return
        }
        do {
            let list: [ScratchNoteVersion] = try await core.request(
                "scratchpad_versions", ScratchIdParams(id: id))
            guard current == session else { return }
            versions = list
        } catch {
            guard current == session else { return }
            versionsError = error.localizedDescription
        }
    }

    /// Save first, then restore through the same write slot as autosave.
    /// Typing during the restore stays in the editor and is saved afterward,
    /// rather than being replaced by an older reply.
    func restore(_ version: ScratchNoteVersion) async {
        guard !restoring, let id = openId, version.noteId == id else { return }
        let current = session
        restoring = true
        defer { restoring = false }
        guard await flush() else {
            if current == session { versionsError = error }
            return
        }
        guard current == session, !hasPendingSave else { return }
        let text = body
        let task = Task {
            await self.restoreSavedVersion(version, session: current, replacing: text)
        }
        inFlight = task
        let restored = await task.value
        if inFlight == task { inFlight = nil }
        guard restored, current == session else { return }
        await loadVersions()
    }

    private func restoreSavedVersion(
        _ version: ScratchNoteVersion, session current: UUID, replacing text: String
    ) async -> Bool {
        do {
            let note: ScratchNote = try await core.request(
                "scratchpad_restore_version",
                ScratchRestoreParams(id: version.noteId, versionId: version.id))
            merge(note)
            guard current == session else { return false }
            saved = note
            if body == text { body = note.body }
            saveState = body == note.body ? .clean : (isBlank(body) ? .blank : .unsaved)
            error = nil
            versionsError = nil
            return true
        } catch {
            guard current == session else { return false }
            self.error = error.localizedDescription
            versionsError = error.localizedDescription
            return false
        }
    }

    private func isBlank(_ text: String) -> Bool {
        text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}
