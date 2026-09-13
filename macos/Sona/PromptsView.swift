import SwiftUI

/// Which library is on screen. The saved prompts are asked about a record;
/// the dictation prompts are what a mode rewrites a transcript with.
enum PromptTab: String, CaseIterable, Identifiable {
    case saved
    case dictation

    var id: String { rawValue }

    var label: String {
        switch self {
        case .saved: "Meetings"
        case .dictation: "Dictation"
        }
    }
}

/// A prompt being written, before it is a prompt. An absent `promptId` means
/// it does not exist yet.
struct PromptDraft: Identifiable {
    let id = UUID()
    let library: PromptTab
    var promptId: String?
    var name: String
    var body: String
    /// `nil` is a prose answer; anything else is the schema its answer is
    /// checked against. Unused by the dictation library.
    var schema: String?
    var target: PromptTarget

    static func blank(_ library: PromptTab) -> PromptDraft {
        PromptDraft(library: library, promptId: nil, name: "", body: "", schema: nil, target: .meeting)
    }

    static func of(_ prompt: SavedPrompt) -> PromptDraft {
        PromptDraft(
            library: .saved,
            promptId: prompt.promptId,
            name: prompt.name,
            body: prompt.body,
            schema: prompt.output.schemaText,
            target: prompt.target
        )
    }

    static func of(_ entry: PromptDictationEntry) -> PromptDraft {
        PromptDraft(
            library: .dictation,
            promptId: entry.id,
            name: entry.name,
            body: entry.prompt,
            schema: nil,
            target: .meeting
        )
    }

    /// A prompt needs a name, a prompt, and a schema that is not blank.
    var isComplete: Bool {
        let named = !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let written = !body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let schemaWritten = schema.map { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty } ?? true
        return named && written && schemaWritten
    }
}

/// Both prompt libraries, behind one pair of tabs and one New prompt button.
struct PromptsView: View {
    let store: PromptsStore
    @State private var tab: PromptTab = .saved
    @State private var draft: PromptDraft?
    @State private var pendingDelete: PromptDictationEntry?
    @State private var expanded: Set<String> = []
    @State private var targetKind: PromptTarget = .meeting

    var body: some View {
        Page {
            PageTitle("Prompts", subtitle: Self.subtitle) {
                Button("New prompt") { draft = PromptDraft.blank(tab) }
                    .buttonStyle(.secondary)
            }
            ErrorNote(store.error)
            PromptTabBar(tab: $tab)
                .padding(.bottom, 24)
            switch tab {
            case .saved:
                savedLibrary
                runner
                results
            case .dictation:
                dictationLibrary
            }
        }
        .sheet(item: $draft) { draft in
            PromptEditorSheet(store: store, draft: draft) { self.draft = nil }
        }
        .sheet(item: $pendingDelete) { entry in
            PromptDeleteSheet(store: store, entry: entry, selected: store.selectedDictationId == entry.id) {
                pendingDelete = nil
            }
        }
    }

    private static let subtitle =
        "Questions you can ask again, and the instructions a dictation mode rewrites with."

    // MARK: - Saved prompts

    @ViewBuilder private var savedLibrary: some View {
        PageSection("Prompts") {
            Card {
                if store.loading && store.prompts.isEmpty {
                    PromptLine("Reading your prompts…")
                } else if store.prompts.isEmpty {
                    PromptLine("No prompts yet.")
                } else {
                    ForEach(store.prompts) { prompt in
                        PromptRow(
                            store: store,
                            prompt: prompt,
                            isOpen: expanded.contains(prompt.promptId),
                            toggle: { toggle(prompt.promptId) },
                            edit: { draft = PromptDraft.of(prompt) }
                        )
                    }
                }
            }
        }
    }

    @ViewBuilder private var runner: some View {
        PageSection("Run a prompt") {
            Card {
                ChoiceRow(
                    title: "About",
                    detail: "A prompt is answered only from what Sona already holds about the record you run it on.",
                    choices: PromptTarget.allCases,
                    label: { $0.label },
                    selection: Binding(get: { targetKind }, set: { kind in
                        targetKind = kind
                        Task { await store.choose(nil) }
                    })
                )
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(targetKind.plural).bodyText()
                        Text(recordDetail).metaText()
                    }
                } trailing: {
                    Picker("", selection: recordBinding) {
                        Text("Choose one").tag(PromptTargetOption?.none)
                        ForEach(store.options(of: targetKind)) { option in
                            Text(option.title).tag(PromptTargetOption?.some(option))
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .frame(maxWidth: 320)
                }
                if let note = store.runNote {
                    PromptLine(note)
                }
            }
        }
    }

    @ViewBuilder private var results: some View {
        if let target = store.target {
            PageSection("Prompt results") {
                Card {
                    if store.runs.isEmpty {
                        PromptLine("Nothing has been asked about \(target.title) yet.")
                    } else {
                        ForEach(store.runs) { run in
                            PromptRunRow(
                                store: store,
                                run: run,
                                name: store.prompt(run.promptId)?.name
                            )
                        }
                    }
                }
            }
        }
    }

    private var recordDetail: String {
        let count = store.options(of: targetKind).count
        if count == 0 {
            return "Nothing recorded yet to ask about."
        }
        return store.target.map { "\($0.title) · \($0.detail)" }
            ?? promptCounted(count, "record", "records") + " to choose from"
    }

    private var recordBinding: Binding<PromptTargetOption?> {
        Binding(
            get: { store.target },
            set: { option in Task { await store.choose(option) } }
        )
    }

    private func toggle(_ promptId: String) {
        if expanded.contains(promptId) {
            expanded.remove(promptId)
        } else {
            expanded.insert(promptId)
        }
    }

    // MARK: - Dictation prompts

    @ViewBuilder private var dictationLibrary: some View {
        PageSection("Post-processing prompts") {
            Card {
                if store.dictationLoading && store.dictation.isEmpty {
                    PromptLine("Loading prompts…")
                } else if store.dictation.isEmpty {
                    PromptLine(
                        "A prompt tells the model what to do with the transcript, for example: rewrite the following as a short message, keeping every fact: ${output}"
                    )
                } else {
                    ForEach(store.dictation) { entry in
                        PromptDictationRow(
                            store: store,
                            entry: entry,
                            selected: store.selectedDictationId == entry.id,
                            isLast: store.dictation.count <= 1,
                            edit: { draft = PromptDraft.of(entry) },
                            remove: { pendingDelete = entry }
                        )
                    }
                }
                if !store.dictation.isEmpty && store.selectedDictationId == nil {
                    PromptLine("No prompt selected: every mode uses the prompt it defines.")
                }
                if store.dictation.count == 1 {
                    PromptLine(
                        "Sona keeps at least one prompt, so this one cannot be deleted. Create another first."
                    )
                }
            }
        }
    }
}

/// The two tabs, as a line of labels with the live one underlined.
struct PromptTabBar: View {
    @Binding var tab: PromptTab

    var body: some View {
        HStack(spacing: 24) {
            ForEach(PromptTab.allCases) { candidate in
                Button { tab = candidate } label: {
                    VStack(spacing: 8) {
                        Text(candidate.label)
                            .font(TypeScale.label())
                            .foregroundStyle(candidate == tab ? Theme.ink : Theme.inkSecondary)
                        Rectangle()
                            .fill(candidate == tab ? Theme.ink : .clear)
                            .frame(height: 2)
                    }
                    .fixedSize()
                }
                .buttonStyle(.plain)
            }
            Spacer()
        }
        .overlay(alignment: .bottom) { Hairline() }
    }
}

/// One quiet sentence in a card, where a row would be too much furniture.
struct PromptLine: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(text)
            .metaText()
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 20)
            .padding(.vertical, 14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .overlay(alignment: .bottom) { Hairline() }
    }
}

/// One saved prompt: a name and what it is about, and the body one click
/// away. The page stays the same height however many questions are kept.
struct PromptRow: View {
    let store: PromptsStore
    let prompt: SavedPrompt
    let isOpen: Bool
    let toggle: () -> Void
    let edit: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            CardRow(action: toggle) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(prompt.name).bodyText()
                    Text(fact).metaText()
                }
            } trailing: {
                Image(systemName: isOpen ? "chevron.up" : "chevron.down")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Theme.inkTertiary)
            }
            if isOpen {
                VStack(alignment: .leading, spacing: 14) {
                    Text(prompt.body)
                        .bodyText(14)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                    if let schema = prompt.output.schemaText {
                        Text(schema)
                            .font(TypeScale.mono(12))
                            .foregroundStyle(Theme.inkSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                    }
                    HStack(spacing: 16) {
                        Button(runTitle) { Task { await store.run(prompt) } }
                            .buttonStyle(.compact)
                            .disabled(store.target == nil || store.running || store.saving)
                        Button("Edit", action: edit)
                            .buttonStyle(.quiet)
                            .disabled(store.saving)
                        Button("Delete") { Task { await store.delete(prompt) } }
                            .buttonStyle(QuietButton(color: Theme.live))
                            .disabled(store.saving)
                    }
                }
                .padding(.horizontal, 20)
                .padding(.bottom, 16)
                .frame(maxWidth: .infinity, alignment: .leading)
                .overlay(alignment: .bottom) { Hairline() }
            }
        }
    }

    private var fact: String {
        let updated = promptRelativeTime(prompt.updatedAtUtcMs)
        return "\(prompt.target.label) · edited \(updated)"
    }

    private var runTitle: String {
        store.target.map { "Run on \($0.title)" } ?? "Run"
    }
}

/// One answer, with the question's name and when it landed.
struct PromptRunRow: View {
    let store: PromptsStore
    let run: PromptRun
    /// Absent once the prompt behind a run has been deleted.
    let name: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text(name ?? "Deleted prompt").headlineText()
                Spacer(minLength: 12)
                Text(promptRelativeTime(run.producedAtUtcMs)).metaText()
                if let prompt = store.prompt(run.promptId) {
                    Button("Run again") { Task { await store.run(prompt) } }
                        .buttonStyle(.quiet)
                        .disabled(store.running)
                }
            }
            PromptRunBody(result: run.result)
            Text("\(run.modelId) · \(run.modelVersion)").metaText(Theme.inkDisabled)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { Hairline() }
    }
}

/// One answer in whichever of its three shapes it came back as.
struct PromptRunBody: View {
    let result: PromptRunResult

    var body: some View {
        switch result {
        case let .failed(reason):
            Text(reason.sentence)
                .font(TypeScale.body(14))
                .foregroundStyle(Theme.live)
        case let .text(text):
            Text(promptMarkdown(text))
                .bodyText(14)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        case let .json(json):
            VStack(alignment: .leading, spacing: 8) {
                ForEach(promptAnswerRows(json)) { row in
                    HStack(alignment: .firstTextBaseline, spacing: 12) {
                        Text(row.key)
                            .metaText(Theme.inkSecondary)
                            .frame(width: 160, alignment: .leading)
                        Text(row.value)
                            .bodyText(14)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
    }
}

/// One post-processing prompt, with the chip that says a mode starts from it.
struct PromptDictationRow: View {
    let store: PromptsStore
    let entry: PromptDictationEntry
    let selected: Bool
    let isLast: Bool
    let edit: () -> Void
    let remove: () -> Void

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 8) {
                    Text(entry.name).bodyText()
                    if selected {
                        Chip("In use")
                    }
                }
                Text(entry.prompt)
                    .metaText()
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
        } trailing: {
            HStack(spacing: 14) {
                if !selected {
                    Button("Use") { Task { await store.useDictation(entry) } }
                        .buttonStyle(.compact)
                        .disabled(store.saving)
                }
                Button("Edit", action: edit)
                    .buttonStyle(.quiet)
                    .disabled(store.saving)
                Button("Delete", action: remove)
                    .buttonStyle(QuietButton(color: Theme.live))
                    .disabled(store.saving || isLast)
            }
        }
    }
}

/// One prompt, in a dialog. The schema field appears only when the answer is
/// a schema: a JSON box beside a prompt that answers in prose is a field that
/// can only be wrong.
struct PromptEditorSheet: View {
    let store: PromptsStore
    @State private var draft: PromptDraft
    let onFinished: () -> Void

    init(store: PromptsStore, draft: PromptDraft, onFinished: @escaping () -> Void) {
        self.store = store
        _draft = State(initialValue: draft)
        self.onFinished = onFinished
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 6) {
                Text(title).headlineText()
                Text(hint).metaText()
            }
            VStack(alignment: .leading, spacing: 8) {
                Text("Name").bodyText(14, Theme.inkSecondary)
                InputField(prompt: namePlaceholder, text: $draft.name)
            }
            VStack(alignment: .leading, spacing: 8) {
                Text(draft.library == .saved ? "Prompt" : "Instructions").bodyText(14, Theme.inkSecondary)
                PromptTextArea(prompt: bodyPlaceholder, text: $draft.body, minHeight: 120)
                if draft.library == .dictation {
                    Text("Write ${output} where the transcript should be inserted.").metaText()
                }
            }
            if draft.library == .saved {
                savedFields
            }
            if let message = store.draftError {
                Text(message)
                    .font(TypeScale.body(14))
                    .foregroundStyle(Theme.live)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 12) {
                Spacer()
                Button("Cancel") { onFinished() }
                    .buttonStyle(.secondary)
                    .disabled(store.saving)
                Button("Save", action: commit)
                    .buttonStyle(.primary)
                    .disabled(store.saving || !draft.isComplete)
            }
        }
        .padding(28)
        .frame(width: 580)
        .background(Theme.page)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusDialog))
        .onAppear { store.clearDraftError() }
    }

    @ViewBuilder private var savedFields: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Answer").bodyText(14, Theme.inkSecondary)
            Picker("", selection: schemaBinding) {
                Text("Text").tag(false)
                Text("JSON matching a schema").tag(true)
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .frame(maxWidth: 260, alignment: .leading)
        }
        if draft.schema != nil {
            VStack(alignment: .leading, spacing: 8) {
                Text("Schema").bodyText(14, Theme.inkSecondary)
                PromptTextArea(
                    prompt: "{\"type\":\"object\",\"required\":[\"decisions\"],\"properties\":{\"decisions\":{\"type\":\"array\",\"items\":{\"type\":\"string\"}}}}",
                    text: schemaTextBinding,
                    minHeight: 110,
                    monospaced: true
                )
            }
        }
        VStack(alignment: .leading, spacing: 8) {
            Text("About").bodyText(14, Theme.inkSecondary)
            Picker("", selection: $draft.target) {
                ForEach(PromptTarget.allCases) { target in
                    Text(target.label).tag(target)
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .frame(maxWidth: 260, alignment: .leading)
        }
    }

    private var title: String {
        if draft.library == .dictation {
            return draft.promptId == nil ? "New prompt" : "Update prompt"
        }
        return draft.promptId == nil ? "New prompt" : "Edit prompt"
    }

    private var hint: String {
        draft.library == .saved
            ? "The prompt is answered only from what Sona already holds about the record you run it on."
            : "The instruction Sona sends to the model after a dictation."
    }

    private var namePlaceholder: String {
        draft.library == .saved ? "Decisions and owners" : "Name this prompt"
    }

    private var bodyPlaceholder: String {
        draft.library == .saved
            ? "List the decisions and who owns each one."
            : "Improve grammar and clarity for the following text: ${output}"
    }

    private var schemaBinding: Binding<Bool> {
        Binding(
            get: { draft.schema != nil },
            set: { wantsSchema in draft.schema = wantsSchema ? (draft.schema ?? "") : nil }
        )
    }

    private var schemaTextBinding: Binding<String> {
        Binding(
            get: { draft.schema ?? "" },
            set: { draft.schema = $0 }
        )
    }

    private func commit() {
        let draft = draft
        Task {
            let saved = draft.library == .saved
                ? await store.save(draft)
                : await store.saveDictation(draft)
            if saved { onFinished() }
        }
    }
}

/// Deleting a post-processing prompt, and the consequence as the sentence.
struct PromptDeleteSheet: View {
    let store: PromptsStore
    let entry: PromptDictationEntry
    let selected: Bool
    let onFinished: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Delete prompt").headlineText()
            Text(consequence)
                .bodyText(14, Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
            if let message = store.error {
                Text(message)
                    .font(TypeScale.body(14))
                    .foregroundStyle(Theme.live)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 12) {
                Spacer()
                Button("Cancel") { onFinished() }
                    .buttonStyle(.secondary)
                    .disabled(store.saving)
                Button("Delete") {
                    Task {
                        if await store.deleteDictation(entry) { onFinished() }
                    }
                }
                .buttonStyle(.primary)
                .disabled(store.saving)
            }
        }
        .padding(28)
        .frame(width: 460)
        .background(Theme.page)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusDialog))
    }

    private var consequence: String {
        selected
            ? "\(entry.name) is in use. Deleting it moves the selection to the first prompt in the list."
            : "\(entry.name) is removed from the library. Modes that already copied its text keep their own prompt."
    }
}

/// A multi-line field, outlined like `InputField` and quiet when empty.
struct PromptTextArea: View {
    let prompt: String
    @Binding var text: String
    var minHeight: CGFloat = 120
    var monospaced = false

    var body: some View {
        TextEditor(text: $text)
            .font(monospaced ? TypeScale.mono(12) : TypeScale.body(14))
            .foregroundStyle(Theme.ink)
            .scrollContentBackground(.hidden)
            .padding(.horizontal, 8)
            .padding(.vertical, 8)
            .frame(minHeight: minHeight, alignment: .topLeading)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
            .overlay(alignment: .topLeading) {
                if text.isEmpty {
                    Text(prompt)
                        .font(monospaced ? TypeScale.mono(12) : TypeScale.body(14))
                        .foregroundStyle(Theme.inkTertiary)
                        .lineLimit(2)
                        .padding(.horizontal, 13)
                        .padding(.vertical, 16)
                        .allowsHitTesting(false)
                }
            }
    }
}

/// A prompt answer's inline Markdown, so bold and code read as themselves.
func promptMarkdown(_ text: String) -> AttributedString {
    let options = AttributedString.MarkdownParsingOptions(
        interpretedSyntax: .inlineOnlyPreservingWhitespace)
    return (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
}
