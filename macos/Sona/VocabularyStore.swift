import AppKit
import Foundation
import UniformTypeIdentifiers

/// Every text rule Sona applies after a transcript, behind one list.
///
/// Four stores keep their own persisted shape and their own commands — a
/// spelling is a `custom_words` entry, a shortcut is a `snippets` record, a
/// rewrite is a `replacements_rules` rule, an emoji is an
/// `emoji_replacements` pair — and nothing here migrates or merges the data.
/// What is merged is the surface: one row shape, one add flow, one owner for
/// "which store does this row belong to".
///
/// Every store's write takes and answers with the whole list, so a row commits
/// on blur and the answer replaces local state. That is why there is no Save
/// button: there is nothing a save could batch that the core does not already
/// take whole.
@MainActor
@Observable
final class VocabularyStore {
    // MARK: Rows

    private(set) var entries: [VocabularyEntry] = []
    private(set) var emoji: [EmojiReplacement] = []
    private(set) var snippets: [SnippetRecord] = []
    private(set) var rewrites: [TextReplacementRule] = []
    private(set) var snippetDrafts: [String: SnippetDraft] = [:]

    // MARK: Switches

    private(set) var spokenEditsEnabled = false
    private(set) var emojiEnabled = false
    private(set) var snippetsEnabled = true
    private(set) var replacementsEnabled = true

    // MARK: Writing samples

    private(set) var samples: [PersonaSample] = []
    private(set) var samplesLoading = true
    /// The sample list could not be read at all, so the editor offers a retry
    /// instead of an empty list that would look like "no samples".
    private(set) var samplesUnreadable = false

    // MARK: State

    private(set) var loading = true
    /// One write at a time: every command answers with the authoritative list,
    /// so two in flight would let the slower answer overwrite the newer one.
    private(set) var busy = false
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?
    /// The write that produced `error`, for the retry beside it.
    private(set) var retry: (@MainActor () -> Void)?
    private(set) var review: VocabularyImportReview?
    /// How many pairs the settings file holds, which is what an export writes
    /// and what an import would replace.
    private(set) var savedEntryCount = 0

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var syncedEntries: [VocabularyEntry] = []
    @ObservationIgnored private var syncedEmoji: [EmojiReplacement] = []
    @ObservationIgnored private var settingsLoaded = false
    @ObservationIgnored private var listsLoaded = false

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.vocabularySettingsChanged) { [weak self] _ in
            self?.settingsChanged()
        }
    }

    /// Called once by the integrator after the core is up.
    func start() async {
        await loadSettings()
        await loadLists()
        await loadSamples()
    }

    // MARK: The merged list

    /// The rows, grouped by store in a fixed order. Each row carries its kind,
    /// so the grouping is a reading aid rather than a second source of truth.
    var rules: [VocabularyRule] {
        var rows: [VocabularyRule] = []
        rows.reserveCapacity(entries.count + snippets.count + rewrites.count + emoji.count)
        for (index, entry) in entries.enumerated() {
            rows.append(VocabularyRule(
                id: "vocabulary:\(index)",
                kind: .vocabulary,
                address: .vocabulary(index),
                left: entry.spoken,
                right: entry.written,
                enabled: nil
            ))
        }
        for snippet in snippets {
            let draft = draft(for: snippet)
            rows.append(VocabularyRule(
                id: "snippet:\(snippet.id)",
                kind: .snippet,
                address: .snippet(snippet.id),
                left: draft.trigger,
                right: draft.expansion,
                enabled: snippet.enabled
            ))
        }
        for (index, rule) in rewrites.enumerated() {
            rows.append(VocabularyRule(
                id: "replacement:\(index)",
                kind: .replacement,
                address: .replacement(index),
                left: rule.spoken,
                right: rule.written,
                enabled: rule.enabled
            ))
        }
        for (index, pair) in emoji.enumerated() {
            rows.append(VocabularyRule(
                id: "emoji:\(index)",
                kind: .emoji,
                address: .emoji(index),
                left: pair.spoken,
                right: pair.written,
                enabled: nil
            ))
        }
        return rows
    }

    /// Exactly what the core would refuse, named on the row that would be
    /// refused: an incomplete pair is dropped on write and a colliding key
    /// rejects the whole list, so checking here turns a silent deletion into a
    /// sentence under the field.
    var problems: [String: String] {
        let rows = rules
        var found: [String: String] = [:]
        for rule in rows {
            if VocabularyKey.trimmed(rule.left).isEmpty || VocabularyKey.trimmed(rule.right).isEmpty {
                found[rule.id] = "Fill in both sides, or delete the rule."
                continue
            }
            let collides = rows.contains { other in
                guard other.id != rule.id, other.kind == rule.kind else {
                    return false
                }
                return rule.kind == .snippet
                    ? VocabularyKey.trigger(other.left) == VocabularyKey.trigger(rule.left)
                    : VocabularyKey.spoken(other.left) == VocabularyKey.spoken(rule.left)
            }
            if collides {
                found[rule.id] = "Another rule of this kind already uses this phrase."
            }
        }
        return found
    }

    /// Whether an add of this kind and pair may be written, and the one
    /// sentence under the add row.
    func addProblem(kind: VocabularyRuleKind, left: String, right: String) -> String? {
        let spoken = VocabularyKey.trimmed(left)
        if spoken.isEmpty || VocabularyKey.trimmed(right).isEmpty {
            return left.isEmpty && right.isEmpty ? nil : "Fill in both sides, or delete the rule."
        }
        let collides = rules.contains { rule in
            guard rule.kind == kind else {
                return false
            }
            return kind == .snippet
                ? VocabularyKey.trigger(rule.left) == VocabularyKey.trigger(spoken)
                : VocabularyKey.spoken(rule.left) == VocabularyKey.spoken(spoken)
        }
        return collides ? "Another rule of this kind already uses this phrase." : nil
    }

    func canAdd(kind: VocabularyRuleKind, left: String, right: String) -> Bool {
        guard !busy else {
            return false
        }
        if VocabularyKey.trimmed(left).isEmpty || VocabularyKey.trimmed(right).isEmpty {
            return false
        }
        return addProblem(kind: kind, left: left, right: right) == nil
    }

    // MARK: Editing a row

    func edit(_ rule: VocabularyRule, _ side: VocabularyRuleSide, _ value: String) {
        switch rule.address {
        case let .vocabulary(index):
            guard entries.indices.contains(index) else {
                return
            }
            if side == .left {
                entries[index].spoken = value
            } else {
                entries[index].written = value
            }
        case let .emoji(index):
            guard emoji.indices.contains(index) else {
                return
            }
            if side == .left {
                emoji[index].spoken = value
            } else {
                emoji[index].written = value
            }
        case let .replacement(index):
            guard rewrites.indices.contains(index) else {
                return
            }
            if side == .left {
                rewrites[index].spoken = value
            } else {
                rewrites[index].written = value
            }
        case let .snippet(id):
            guard let snippet = snippets.first(where: { $0.id == id }) else {
                return
            }
            var draft = draft(for: snippet)
            if side == .left {
                draft.trigger = value
            } else {
                draft.expansion = value
            }
            snippetDrafts[id] = draft
        }
    }

    /// A row commits on blur and on Return. A row the core would refuse is
    /// kept local until it reads as a rule.
    func commit(_ rule: VocabularyRule) {
        guard !busy, problems[rule.id] == nil else {
            return
        }
        switch rule.address {
        case .vocabulary:
            writeEntries(entries)
        case .emoji:
            writeEmoji(emoji)
        case .replacement:
            writeRewrites(rewrites)
        case let .snippet(id):
            guard let snippet = snippets.first(where: { $0.id == id }) else {
                return
            }
            let draft = draft(for: snippet)
            let trigger = VocabularyKey.trimmed(draft.trigger)
            let expansion = VocabularyKey.trimmed(draft.expansion)
            // Snippets are the one store written per record, so an untouched
            // row has nothing to send and a blur must not bump `updated_at`.
            guard trigger != snippet.trigger || expansion != snippet.expansion else {
                return
            }
            var next = snippet
            next.trigger = trigger
            next.expansion = expansion
            writeSnippets { [core] in
                try await core.request("upsert_snippet", SnippetRequest(snippet: next))
            }
        }
    }

    func remove(_ rule: VocabularyRule) {
        switch rule.address {
        case let .vocabulary(index):
            guard entries.indices.contains(index) else {
                return
            }
            var next = entries
            next.remove(at: index)
            writeEntries(next)
        case let .emoji(index):
            guard emoji.indices.contains(index) else {
                return
            }
            var next = emoji
            next.remove(at: index)
            writeEmoji(next)
        case let .replacement(index):
            guard rewrites.indices.contains(index) else {
                return
            }
            var next = rewrites
            next.remove(at: index)
            writeRewrites(next)
        case let .snippet(id):
            writeSnippets { [core] in
                try await core.request("delete_snippet", SnippetIdRequest(snippetId: id))
            }
        }
    }

    /// The per-rule switch, which only rewrites and shortcuts have.
    func setEnabled(_ rule: VocabularyRule, _ enabled: Bool) {
        switch rule.address {
        case let .replacement(index):
            guard rewrites.indices.contains(index) else {
                return
            }
            var next = rewrites
            next[index].enabled = enabled
            writeRewrites(next)
        case let .snippet(id):
            writeSnippets { [core] in
                try await core.request(
                    "set_snippet_enabled",
                    SnippetToggleRequest(snippetId: id, enabled: enabled)
                )
            }
        case .vocabulary, .emoji:
            return
        }
    }

    func add(kind: VocabularyRuleKind, left: String, right: String) {
        let spoken = VocabularyKey.trimmed(left)
        // A rewrite's written side may be leading or trailing whitespace on
        // purpose, so only its spoken side is trimmed.
        let written = kind == .replacement ? right : VocabularyKey.trimmed(right)
        guard !spoken.isEmpty, !written.isEmpty else {
            return
        }
        switch kind {
        case .vocabulary:
            // The core's own single-pair upsert, which is what "learn this
            // spelling" means everywhere else in the app.
            write({ [weak self] in
                guard let self else {
                    return
                }
                try await core.request(
                    "add_vocabulary_correction",
                    VocabularyCorrectionRequest(spoken: spoken, written: written, scope: .global)
                )
                // The upsert answers with the one pair it stored; the list is
                // what the file now holds, in the file's own order.
                let list: [VocabularyEntry] = try await core.request(
                    "list_vocabulary_entries",
                    VocabularyScopeRequest(scope: .global)
                )
                entries = VocabularyDraft.mergeAppliedCsv(entries, list)
                syncedEntries = list
                savedEntryCount = list.count
            }, retry: { [weak self] in
                self?.add(kind: kind, left: left, right: right)
            })
        case .emoji:
            writeEmoji(emoji + [EmojiReplacement(spoken: spoken, written: written)])
        case .replacement:
            writeRewrites(rewrites + [
                TextReplacementRule(spoken: spoken, written: written, enabled: true),
            ])
        case .snippet:
            let draft = SnippetRecord.draft(trigger: spoken, expansion: written)
            writeSnippets { [core] in
                try await core.request("upsert_snippet", SnippetRequest(snippet: draft))
            }
        }
    }

    // MARK: The four switches

    func setSpokenEdits(_ enabled: Bool) {
        writeSetting(
            "update_spoken_edits_enabled",
            enabled: enabled,
            failure: "Couldn't change spoken editing commands. Try again.",
            retry: { [weak self] in self?.setSpokenEdits(enabled) }
        )
    }

    func setEmojiEnabled(_ enabled: Bool) {
        writeSetting(
            "update_emoji_replacements_enabled",
            enabled: enabled,
            failure: "Couldn't change emoji writing. Try again.",
            retry: { [weak self] in self?.setEmojiEnabled(enabled) }
        )
    }

    func setSnippetsEnabled(_ enabled: Bool) {
        writeSetting(
            "set_snippets_enabled",
            enabled: enabled,
            failure: "Couldn't change shortcut expansion. Try again.",
            retry: { [weak self] in self?.setSnippetsEnabled(enabled) }
        )
    }

    func setReplacementsEnabled(_ enabled: Bool) {
        writeSetting(
            "update_text_replacements_enabled",
            enabled: enabled,
            failure: "Couldn't change rewrites. Try again.",
            retry: { [weak self] in self?.setReplacementsEnabled(enabled) }
        )
    }

    /// Restores the shipped starter library of rewrites, discarding edits.
    func restoreDefaultRewrites() {
        write({ [weak self] in
            guard let self else {
                return
            }
            rewrites = try await core.request("reset_text_replacements")
        }, retry: { [weak self] in
            self?.restoreDefaultRewrites()
        })
    }

    // MARK: The CSV round trip

    /// Picks a file, reads it here, and asks the core what it would do. The
    /// webview read this through a file input; on the shell it is one panel and
    /// `String(contentsOf:)`.
    func importCsv() {
        guard !busy else {
            return
        }
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.allowedContentTypes = [.commaSeparatedText, .text]
        panel.message = "Choose a CSV of spoken and written pairs."
        guard panel.runModal() == .OK, let url = panel.url else {
            return
        }
        previewCsv(at: url)
    }

    private func previewCsv(at url: URL) {
        write({ [weak self] in
            guard let self else {
                return
            }
            let csv = try String(contentsOf: url, encoding: .utf8)
            let preview: VocabularyCsvPreview = try await core.request(
                "preview_vocabulary_csv",
                VocabularyCsvRequest(scope: .global, csvText: csv)
            )
            review = VocabularyImportReview(csv: csv, preview: preview, step: .review)
        }, retry: { [weak self] in
            self?.previewCsv(at: url)
        })
    }

    func setReviewStep(_ step: VocabularyImportStep) {
        review?.step = step
    }

    func closeReview() {
        review = nil
    }

    /// Nothing is written until this. The core replaces the persisted list with
    /// the CSV rows, so local rows the CSV does not define are merged back
    /// instead of silently discarded.
    func applyImport() {
        guard let pending = review, pending.preview.canApply else {
            return
        }
        write({ [weak self] in
            guard let self else {
                return
            }
            let applied: [VocabularyEntry] = try await core.request(
                "apply_vocabulary_csv",
                VocabularyCsvRequest(scope: .global, csvText: pending.csv)
            )
            entries = VocabularyDraft.mergeAppliedCsv(entries, applied)
            syncedEntries = applied
            savedEntryCount = applied.count
            review = nil
        }, retry: { [weak self] in
            self?.applyImport()
        })
    }

    /// Asks the core for the CSV, then writes it where the person says. The
    /// webview handed this to a download; on the shell it is a save panel.
    func exportCsv() {
        write({ [weak self] in
            guard let self else {
                return
            }
            let csv: String = try await core.request(
                "export_vocabulary_csv",
                VocabularyScopeRequest(scope: .global)
            )
            let panel = NSSavePanel()
            panel.allowedContentTypes = [.commaSeparatedText]
            panel.nameFieldStringValue = "sona-vocabulary.csv"
            guard panel.runModal() == .OK, let url = panel.url else {
                return
            }
            try csv.write(to: url, atomically: true, encoding: .utf8)
        }, retry: { [weak self] in
            self?.exportCsv()
        })
    }

    // MARK: Writing samples

    func reloadSamples() {
        Task { await loadSamples() }
    }

    /// A blank row is kept local until it has text: the core drops blank
    /// samples, so saving one would make the row disappear under the cursor.
    func addSample() {
        guard samples.count < PersonaSampleLimit.count else {
            return
        }
        let id = "sample_\(Int(Date().timeIntervalSince1970 * 1000))_\(samples.count)"
        samples.append(PersonaSample(id: id, text: ""))
    }

    func editSample(id: String, text: String) {
        guard let index = samples.firstIndex(where: { $0.id == id }) else {
            return
        }
        samples[index].text = text
    }

    /// Saving answers with the normalized list, so the editor takes the answer.
    func commitSamples() {
        let next = samples
        write({ [weak self] in
            guard let self else {
                return
            }
            samples = try await core.request(
                "save_persona_samples",
                PersonaSamplesRequest(samples: next)
            )
        }, retry: { [weak self] in
            self?.commitSamples()
        })
    }

    func removeSample(id: String) {
        let next = samples.filter { $0.id != id }
        write({ [weak self] in
            guard let self else {
                return
            }
            samples = try await core.request(
                "save_persona_samples",
                PersonaSamplesRequest(samples: next)
            )
        }, retry: { [weak self] in
            self?.removeSample(id: id)
        })
    }

    // MARK: Loading

    private func fetchSettings() async throws {
        let settings: VocabularySettingsSnapshot = try await core.request("get_app_settings")
        apply(settings)
    }

    private func loadSettings() async {
        do {
            try await fetchSettings()
            error = nil
            retry = nil
        } catch {
            self.error = error.localizedDescription
            retry = { [weak self] in
                Task { await self?.loadSettings() }
            }
        }
        settingsLoaded = true
        loading = !(settingsLoaded && listsLoaded)
    }

    private func loadLists() async {
        do {
            let savedEntries: [VocabularyEntry] = try await core.request(
                "list_vocabulary_entries",
                VocabularyScopeRequest(scope: .global)
            )
            let savedSnippets: [SnippetRecord] = try await core.request("list_snippets")
            let savedRewrites: [TextReplacementRule] = try await core.request("get_text_replacements")
            entries = VocabularyDraft.resolveRefresh(
                current: entries,
                previousSaved: syncedEntries,
                incomingSaved: savedEntries
            )
            syncedEntries = savedEntries
            snippets = savedSnippets
            snippetDrafts = [:]
            rewrites = savedRewrites
            error = nil
            retry = nil
        } catch {
            self.error = error.localizedDescription
            retry = { [weak self] in
                Task { await self?.loadLists() }
            }
        }
        listsLoaded = true
        loading = !(settingsLoaded && listsLoaded)
    }

    private func loadSamples() async {
        samplesLoading = true
        do {
            samples = try await core.request("get_persona_samples")
            samplesUnreadable = false
        } catch {
            samplesUnreadable = true
            self.error = error.localizedDescription
        }
        samplesLoading = false
    }

    private func apply(_ settings: VocabularySettingsSnapshot) {
        let savedWords = settings.customWords ?? []
        let savedEmoji = settings.emojiReplacements ?? []
        entries = VocabularyDraft.resolveRefresh(
            current: entries,
            previousSaved: syncedEntries,
            incomingSaved: savedWords
        )
        syncedEntries = savedWords
        emoji = VocabularyDraft.resolveRefresh(
            current: emoji,
            previousSaved: syncedEmoji,
            incomingSaved: savedEmoji
        )
        syncedEmoji = savedEmoji
        savedEntryCount = savedWords.count
        spokenEditsEnabled = settings.spokenEditsEnabled ?? false
        emojiEnabled = settings.emojiReplacementsEnabled ?? false
        snippetsEnabled = settings.snippetsEnabled ?? true
        replacementsEnabled = settings.replacementsEnabled ?? true
    }

    /// `settings-changed` carries no useful payload, so the whole snapshot is
    /// re-read. A refresh never discards the row under the cursor.
    private func settingsChanged() {
        Task { await loadSettings() }
    }

    // MARK: Writes

    private func writeEntries(_ next: [VocabularyEntry]) {
        write({ [weak self] in
            guard let self else {
                return
            }
            let saved: [VocabularyEntry] = try await core.request(
                "update_vocabulary_entries",
                VocabularyEntriesRequest(scope: .global, entries: next)
            )
            entries = saved
            syncedEntries = saved
            savedEntryCount = saved.count
        }, retry: { [weak self] in
            self?.writeEntries(next)
        })
    }

    private func writeEmoji(_ next: [EmojiReplacement]) {
        write({ [weak self] in
            guard let self else {
                return
            }
            let saved: [EmojiReplacement] = try await core.request(
                "update_emoji_replacements",
                EmojiReplacementsRequest(replacements: next)
            )
            emoji = saved
            syncedEmoji = saved
        }, retry: { [weak self] in
            self?.writeEmoji(next)
        })
    }

    private func writeRewrites(_ next: [TextReplacementRule]) {
        write({ [weak self] in
            guard let self else {
                return
            }
            rewrites = try await core.request(
                "save_text_replacements",
                TextReplacementsRequest(rules: next)
            )
        }, retry: { [weak self] in
            self?.writeRewrites(next)
        })
    }

    /// Every snippet command answers with the whole new list, so the editor
    /// takes the answer instead of following up with `list_snippets`.
    private func writeSnippets(_ command: @escaping () async throws -> [SnippetRecord]) {
        write({ [weak self] in
            guard let self else {
                return
            }
            let saved = try await command()
            snippets = saved
            // The answer is the truth, so every draft is spent.
            snippetDrafts = [:]
        }, retry: { [weak self] in
            self?.writeSnippets(command)
        })
    }

    private func writeSetting(
        _ method: String,
        enabled: Bool,
        failure: String,
        retry: @escaping @MainActor () -> Void
    ) {
        write({ [weak self] in
            guard let self else {
                return
            }
            do {
                try await core.request(method, VocabularyEnabledRequest(enabled: enabled))
            } catch {
                throw VocabularyWriteFailure(reason: "\(failure) \(error.localizedDescription)")
            }
            // The switch lives in the settings file, so the switch's new
            // value is read back rather than assumed.
            try await fetchSettings()
        }, retry: retry)
    }

    private func write(
        _ work: @escaping () async throws -> Void,
        retry: @escaping @MainActor () -> Void
    ) {
        guard !busy else {
            return
        }
        busy = true
        error = nil
        self.retry = nil
        Task {
            do {
                try await work()
                error = nil
                self.retry = nil
            } catch {
                self.error = error.localizedDescription
                self.retry = retry
            }
            busy = false
        }
    }

    private func draft(for snippet: SnippetRecord) -> SnippetDraft {
        snippetDrafts[snippet.id]
            ?? SnippetDraft(trigger: snippet.trigger, expansion: snippet.expansion)
    }
}

/// A switch the core refused, with the sentence that says which switch.
struct VocabularyWriteFailure: LocalizedError {
    let reason: String

    var errorDescription: String? { reason }
}

struct VocabularyScopeRequest: Encodable {
    let scope: VocabularyScope
}

struct VocabularyEntriesRequest: Encodable {
    let scope: VocabularyScope
    let entries: [VocabularyEntry]
}

struct VocabularyCsvRequest: Encodable {
    let scope: VocabularyScope
    let csvText: String
}

struct VocabularyCorrectionRequest: Encodable {
    let spoken: String
    let written: String
    let scope: VocabularyScope
}

struct EmojiReplacementsRequest: Encodable {
    let replacements: [EmojiReplacement]
}

struct TextReplacementsRequest: Encodable {
    let rules: [TextReplacementRule]
}

struct SnippetRequest: Encodable {
    let snippet: SnippetRecord
}

struct SnippetIdRequest: Encodable {
    let snippetId: String
}

struct SnippetToggleRequest: Encodable {
    let snippetId: String
    let enabled: Bool
}

struct PersonaSamplesRequest: Encodable {
    let samples: [PersonaSample]
}

struct VocabularyEnabledRequest: Encodable {
    let enabled: Bool
}
