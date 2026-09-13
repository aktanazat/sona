import Foundation

/// The prompt library: the saved prompts a reader asks about a record, and the
/// post-processing prompts a dictation mode starts from. Two tables, two sets
/// of commands, one screen — the way `settings/prompts/PromptLibrary.tsx`
/// puts them behind one pair of tabs.
///
/// Every saved-prompt write carries the shared revision, so a second window
/// editing another prompt fences this one. A rejection is not an error to
/// apologise for: it is "read again", which is what `refresh` does.
@MainActor
@Observable
final class PromptsStore {
    /// The saved prompts, newest write last, as the core ordered them.
    private(set) var prompts: [SavedPrompt] = []
    /// The revision every saved-prompt write has to carry.
    private(set) var revision: UInt64 = 0
    private(set) var loading = true
    /// A write is in flight: the rows' own buttons go quiet.
    private(set) var saving = false
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?
    /// The same, for the editor sheet, which covers the page's own note.
    private(set) var draftError: String?

    /// The post-processing prompts, out of the core's settings.
    private(set) var dictation: [PromptDictationEntry] = []
    /// The one a mode without its own prompt starts from.
    private(set) var selectedDictationId: String?
    private(set) var dictationLoading = true

    /// Every record a prompt can be asked about, all three kinds together.
    private(set) var options: [PromptTargetOption] = []
    /// The record the run buttons are about. Nothing until one is picked.
    private(set) var target: PromptTargetOption?
    /// What the saved prompts have already said about that record.
    private(set) var runs: [PromptRun] = []
    private(set) var running = false
    /// What the last run did, in one sentence, until the next one.
    private(set) var runNote: String?

    @ObservationIgnored private let core: Core

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.promptSettingsChanged) { [weak self] _ in
            Task { await self?.loadDictation() }
        }
    }

    /// The first load: both libraries and the records to run against.
    func start() async {
        async let library: Void = refresh()
        async let postProcessing: Void = loadDictation()
        async let targets: Void = loadTargets()
        _ = await (library, postProcessing, targets)
    }

    // MARK: - Saved prompts

    /// Reads the library again. Also the honest answer to a rejected write.
    func refresh() async {
        loading = true
        defer { loading = false }
        do {
            let list: SavedPromptList = try await core.request("saved_prompt_list")
            apply(list)
            error = nil
        } catch {
            self.error = promptErrorSentence(error)
        }
    }

    /// Writes one prompt, new or edited. `true` when the core committed it,
    /// which is when the editor may close.
    func save(_ draft: PromptDraft) async -> Bool {
        saving = true
        draftError = nil
        defer { saving = false }
        let request = SaveRequest(
            operationId: UUID().uuidString,
            promptId: draft.promptId,
            name: draft.name,
            body: draft.body,
            output: draft.schema.map(PromptOutput.schema) ?? .text,
            target: draft.target,
            expectedRevision: revision
        )
        do {
            let result: SavedPromptMutationResult = try await core.request(
                "saved_prompt_save", ["request": request])
            apply(result.prompts)
            if result.receipt.rejected {
                /* Somebody else moved the revision. The refusal changed
                 * nothing, so the honest response is to show what is true
                 * now. */
                draftError = "Another window changed these. Read again and retry."
                return false
            }
            error = nil
            return true
        } catch {
            draftError = promptErrorSentence(error)
            return false
        }
    }

    /// Deletes one prompt. Its runs stay: a run is its own receipt.
    func delete(_ prompt: SavedPrompt) async {
        saving = true
        error = nil
        defer { saving = false }
        let request = DeleteRequest(
            operationId: UUID().uuidString,
            promptId: prompt.promptId,
            expectedRevision: revision
        )
        do {
            let result: SavedPromptMutationResult = try await core.request(
                "saved_prompt_delete", ["request": request])
            apply(result.prompts)
            if result.receipt.rejected {
                error = "Another window changed these. Read again and retry."
            }
        } catch {
            self.error = promptErrorSentence(error)
        }
    }

    /// Clears whatever the editor said last, so a reopened sheet is clean.
    func clearDraftError() {
        draftError = nil
    }

    // MARK: - Running a prompt

    /// Picks the record the run buttons are about, and reads what the prompts
    /// have already said about it.
    func choose(_ option: PromptTargetOption?) async {
        guard option?.id != target?.id else { return }
        target = option
        runs = []
        runNote = nil
        await loadRuns()
    }

    /// Asks one prompt about the chosen record and keeps the answer.
    ///
    /// A run that produced nothing is still a run: the reason lands beside it
    /// in the log rather than reading as a press that did nothing.
    func run(_ prompt: SavedPrompt) async {
        guard let target else { return }
        running = true
        runNote = nil
        error = nil
        defer { running = false }
        do {
            let run: PromptRun = try await core.request(
                "saved_prompt_run",
                ["request": RunRequest(promptId: prompt.promptId, target: target.ref)])
            if case let .failed(reason) = run.result {
                error = reason.sentence
            } else {
                runNote = "\(prompt.name) ran. The answer is under Prompt results."
            }
            await loadRuns()
        } catch {
            self.error = promptErrorSentence(error)
        }
    }

    /// Reads every run recorded against the chosen record.
    func loadRuns() async {
        guard let target else {
            runs = []
            return
        }
        do {
            let runs: [PromptRun] = try await core.request(
                "saved_prompt_runs", ["target": target.ref])
            self.runs = runs.sorted { $0.producedAtUtcMs > $1.producedAtUtcMs }
        } catch {
            self.error = promptErrorSentence(error)
        }
    }

    /// Reads the records a prompt can be run against: the recent meetings, the
    /// people, and the recurring series the automation roster names.
    func loadTargets() async {
        var found: [PromptTargetOption] = []
        do {
            let page: PromptMeetingPage = try await core.request(
                "meeting_list",
                ["cursorUtcMs": JSONValue.null, "limit": .number(Double(Self.targetPage)), "filter": .null])
            found += page.entries.map { row in
                PromptTargetOption(
                    kind: .meeting,
                    recordId: row.sessionId,
                    title: row.title,
                    detail: promptRelativeTime(row.createdAtUtcMs)
                )
            }
        } catch {
            self.error = promptErrorSentence(error)
        }
        do {
            let people: PromptPeopleList = try await core.request("people_list")
            found += people.entries.map { row in
                PromptTargetOption(
                    kind: .person,
                    recordId: row.person.id,
                    title: row.person.displayName,
                    detail: promptCounted(row.meetingsCount, "meeting", "meetings")
                )
            }
        } catch {
            self.error = promptErrorSentence(error)
        }
        do {
            let roster: PromptSeriesRoster = try await core.request("meeting_automation_roster")
            found += roster.series.map { row in
                PromptTargetOption(
                    kind: .series,
                    recordId: row.seriesKey,
                    title: row.title,
                    detail: promptCounted(row.meetingCount, "meeting", "meetings")
                )
            }
        } catch {
            self.error = promptErrorSentence(error)
        }
        options = found
        if let target, !found.contains(where: { $0.id == target.id }) {
            self.target = nil
            runs = []
        }
    }

    /// The records of one kind, for that kind's menu.
    func options(of kind: PromptTarget) -> [PromptTargetOption] {
        options.filter { $0.kind == kind }
    }

    /// The prompt a run was asked from, while it still exists.
    func prompt(_ promptId: String) -> SavedPrompt? {
        prompts.first { $0.promptId == promptId }
    }

    // MARK: - Post-processing prompts

    /// Reads the post-processing library out of the core's settings. The
    /// commands that write it answer with nothing, so this is how a write is
    /// confirmed — and it is what `settings-changed` re-runs.
    func loadDictation() async {
        dictationLoading = true
        defer { dictationLoading = false }
        do {
            let settings: PromptAppSettings = try await core.request("get_app_settings")
            dictation = settings.postProcessPrompts ?? []
            selectedDictationId = settings.postProcessSelectedPromptId
            error = nil
        } catch {
            self.error = promptErrorSentence(error)
        }
    }

    /// Writes one post-processing prompt, new or edited. `true` when it landed.
    func saveDictation(_ draft: PromptDraft) async -> Bool {
        saving = true
        draftError = nil
        defer { saving = false }
        let name = draft.name.trimmingCharacters(in: .whitespacesAndNewlines)
        let body = draft.body.trimmingCharacters(in: .whitespacesAndNewlines)
        do {
            if let id = draft.promptId {
                try await core.request(
                    "update_post_process_prompt", ["id": id, "name": name, "prompt": body])
            } else {
                try await core.request(
                    "add_post_process_prompt", ["name": name, "prompt": body])
            }
            await loadDictation()
            return true
        } catch {
            draftError = promptErrorSentence(error)
            return false
        }
    }

    /// Deletes one post-processing prompt. The core keeps at least one, so the
    /// last row's delete never fires.
    func deleteDictation(_ entry: PromptDictationEntry) async -> Bool {
        saving = true
        error = nil
        defer { saving = false }
        do {
            try await core.request("delete_post_process_prompt", ["id": entry.id])
            await loadDictation()
            return true
        } catch {
            self.error = promptErrorSentence(error)
            return false
        }
    }

    /// Makes one post-processing prompt the one modes start from.
    func useDictation(_ entry: PromptDictationEntry) async {
        saving = true
        error = nil
        defer { saving = false }
        do {
            try await core.request("set_post_process_selected_prompt", ["id": entry.id])
            await loadDictation()
        } catch {
            self.error = promptErrorSentence(error)
        }
    }

    // MARK: - Wire shapes

    /// How many meetings the target picker reads. The picker is a menu, not a
    /// history: a page of recent records is what a reader scans.
    private static let targetPage = 50

    private struct SaveRequest: Encodable {
        let operationId: String
        let promptId: String?
        let name: String
        let body: String
        let output: PromptOutput
        let target: PromptTarget
        let expectedRevision: UInt64

        enum CodingKeys: String, CodingKey {
            case operationId = "operation_id"
            case promptId = "prompt_id"
            case name
            case body
            case output
            case target
            case expectedRevision = "expected_revision"
        }
    }

    private struct DeleteRequest: Encodable {
        let operationId: String
        let promptId: String
        let expectedRevision: UInt64

        enum CodingKeys: String, CodingKey {
            case operationId = "operation_id"
            case promptId = "prompt_id"
            case expectedRevision = "expected_revision"
        }
    }

    private struct RunRequest: Encodable {
        let promptId: String
        let target: PromptTargetRef

        enum CodingKeys: String, CodingKey {
            case promptId = "prompt_id"
            case target
        }
    }

    private func apply(_ list: SavedPromptList) {
        prompts = list.prompts
        revision = list.revision
    }
}
