import SwiftUI

/// The person's own meeting templates: the settings section that lists and
/// edits them, the sheet one is written in, and the menu every template
/// picker in the app draws its choices from.

// MARK: - Naming a template

/// The rows of every template picker: the row that hands the choice back
/// when the caller offers one, the core's built-ins, then the person's own
/// templates, each group under its own divider.
///
/// Generic over the built-in enum, because three surfaces spell the built-ins
/// with their own type and the same wire names, and a custom choice has to sit
/// beside each of them.
struct MeetingTemplatePickerRows<BuiltIn: Hashable>: View {
    let builtIns: [BuiltIn]
    let builtInLabel: (BuiltIn) -> String
    let templates: MeetingTemplatesStore
    /// The row that means no choice, or nil when there is nothing to hand
    /// the choice back to.
    var none: String? = nil
    /// The current choice, so a custom template the list does not hold yet
    /// still has a row to be shown as: a picker with no row for its value
    /// draws nothing.
    let selection: MeetingTemplateChoice<BuiltIn>?

    var body: some View {
        if let none {
            Section {
                Text(none).tag(Optional<MeetingTemplateChoice<BuiltIn>>.none)
            }
        }
        Section {
            ForEach(builtIns, id: \.self) { builtIn in
                Text(builtInLabel(builtIn))
                    .tag(Optional(MeetingTemplateChoice<BuiltIn>.builtIn(builtIn)))
            }
        }
        if !templates.templates.isEmpty || unlisted != nil {
            Section {
                ForEach(templates.templates) { template in
                    Text(template.name)
                        .tag(Optional(MeetingTemplateChoice<BuiltIn>.custom(template.templateId)))
                }
                if let unlisted {
                    Text(templates.name(unlisted) ?? "Custom template")
                        .tag(Optional(MeetingTemplateChoice<BuiltIn>.custom(unlisted)))
                }
            }
        }
    }

    /// A custom choice the list does not hold: not read yet, or deleted.
    private var unlisted: String? {
        guard case let .custom(templateId)? = selection, templates.template(templateId) == nil else { return nil }
        return templateId
    }
}

/// A menu naming every template a picker can choose, behind a label the
/// caller draws: the header of the notes card.
struct MeetingTemplateMenu<BuiltIn: Hashable, Label: View>: View {
    let builtIns: [BuiltIn]
    let builtInLabel: (BuiltIn) -> String
    let templates: MeetingTemplatesStore
    var none: String? = nil
    @Binding var selection: MeetingTemplateChoice<BuiltIn>?
    @ViewBuilder let label: () -> Label

    var body: some View {
        Menu {
            Picker("Template", selection: $selection) {
                MeetingTemplatePickerRows(
                    builtIns: builtIns, builtInLabel: builtInLabel, templates: templates,
                    none: none, selection: selection)
            }
            .pickerStyle(.inline)
        } label: {
            label()
        }
    }
}

/// A row with a template menu on the right: "Notes template — Sales call".
/// The same row as `ChoiceRow`, with the rows grouped.
struct MeetingTemplateChoiceRow<BuiltIn: Hashable>: View {
    let title: String
    var detail: String? = nil
    let builtIns: [BuiltIn]
    let builtInLabel: (BuiltIn) -> String
    let templates: MeetingTemplatesStore
    /// The row that means no choice, offered only when there is one.
    var none: String? = nil
    @Binding var selection: MeetingTemplateChoice<BuiltIn>?

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).bodyText()
                if let detail {
                    Text(detail).metaText()
                }
            }
        } trailing: {
            Picker(title, selection: $selection) {
                MeetingTemplatePickerRows(
                    builtIns: builtIns, builtInLabel: builtInLabel, templates: templates,
                    none: none, selection: selection)
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .fixedSize()
        }
    }
}

// MARK: - The settings section

/// Which template new notes get by default, and the templates the person
/// wrote themselves.
struct MeetingTemplatesSection: View {
    let store: MeetingSettingsStore
    @State private var editing: MeetingTemplateEditing?

    private var templates: MeetingTemplatesStore { store.templates }

    var body: some View {
        PageSection("Templates") {
            Card {
                if !store.settingsRead || !templates.read {
                    MeetingTemplatesNote("Reading your templates…")
                } else {
                    MeetingTemplateChoiceRow(
                        title: "Default template",
                        detail: "New notes are written this way unless a meeting or its series says otherwise.",
                        builtIns: MeetingNotesTemplate.allCases,
                        builtInLabel: { $0.label },
                        templates: templates,
                        selection: defaultChoice)
                    .disabled(store.notesTemplateSaving)

                    if let failure = templates.loadFailure {
                        ActionRow(title: failure, button: "Try again", busy: templates.saving) {
                            Task { await templates.load() }
                        }
                    } else if templates.templates.isEmpty {
                        MeetingTemplatesNote(
                            "No templates yet. A template tells Sona which sections to write and what goes in each.")
                    } else {
                        ForEach(templates.templates) { template in
                            CardRow {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(template.name).bodyText()
                                    Text(template.sectionsLine).metaText()
                                }
                            } trailing: {
                                Button("Edit") { editing = MeetingTemplateEditing(template: template) }
                                    .buttonStyle(.secondary)
                            }
                        }
                    }

                    CardRow {
                        Text(templates.atLimit
                            ? "That is \(MeetingTemplateLimits.maxTemplates) templates, the most Sona keeps."
                            : "Sona keeps up to \(MeetingTemplateLimits.maxTemplates) templates.")
                            .metaText()
                    } trailing: {
                        Button("New template") { editing = MeetingTemplateEditing(template: nil) }
                            .buttonStyle(.primary)
                            .disabled(templates.atLimit || templates.loadFailure != nil)
                    }
                }
            }
        }
        .sheet(item: $editing) { editing in
            MeetingTemplateEditor(templates: templates, original: editing.template) { self.editing = nil }
        }
    }

    /// The custom default when it names a template the list holds, else the
    /// built-in. A custom choice writes the built-in beside it, which is what
    /// the core falls back to once that template is deleted.
    private var defaultChoice: Binding<MeetingTemplateChoice<MeetingNotesTemplate>?> {
        Binding(
            get: {
                if let templateId = store.settings.notesCustomTemplateId, templates.template(templateId) != nil {
                    return .custom(templateId)
                }
                return .builtIn(store.settings.notesTemplate)
            },
            set: { choice in
                if let choice { Task { await store.setNotesTemplate(choice) } }
            })
    }
}

/// What the editor sheet was opened for: one template, or a new one.
private struct MeetingTemplateEditing: Identifiable {
    let template: MeetingCustomTemplate?

    var id: String { template?.templateId ?? "new" }
}

/// One line of standing fact inside the card.
private struct MeetingTemplatesNote: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        CardRow {
            Text(text).bodyText(14, Theme.inkTertiary)
        }
    }
}

// MARK: - The editor

/// One section as the editor holds it: the wire fields plus an identity, so
/// a row keeps its fields while the rows around it are moved or removed.
private struct MeetingTemplateSectionDraft: Identifiable, Equatable {
    let id = UUID()
    var title = ""
    var instructions = ""

    init() {}

    init(_ section: MeetingTemplateSection) {
        title = section.title
        instructions = section.instructions
    }

    var section: MeetingTemplateSection {
        MeetingTemplateSection(title: title, instructions: instructions)
    }
}

/// Where a template is written: its name, what it is for, and the sections
/// the notes get, each with the instructions its part is written from.
///
/// Save is the one primary action. It stays off while the draft breaks a
/// limit, and the first broken limit is said under the fields once anything
/// has been typed.
private struct MeetingTemplateEditor: View {
    let templates: MeetingTemplatesStore
    /// The template being edited, or nil for a new one.
    let original: MeetingCustomTemplate?
    let close: () -> Void

    @State private var name: String
    @State private var purpose: String
    @State private var sections: [MeetingTemplateSectionDraft]
    @State private var confirmingDelete = false

    init(templates: MeetingTemplatesStore, original: MeetingCustomTemplate?, close: @escaping () -> Void) {
        self.templates = templates
        self.original = original
        self.close = close
        _name = State(initialValue: original?.name ?? "")
        _purpose = State(initialValue: original?.purpose ?? "")
        // A new template opens with one section, because that is the least
        // it can have.
        _sections = State(
            initialValue: original?.sections.map { MeetingTemplateSectionDraft($0) } ?? [MeetingTemplateSectionDraft()])
    }

    private var draft: MeetingCustomTemplateDraft {
        MeetingCustomTemplateDraft(name: name, purpose: purpose, sections: sections.map(\.section))
    }

    /// What the sheet opened with, for saying a problem only once something
    /// was typed over it.
    private var initial: MeetingCustomTemplateDraft {
        original.map { MeetingCustomTemplateDraft($0) }
            ?? MeetingCustomTemplateDraft(sections: [MeetingTemplateSection(title: "", instructions: "")])
    }

    private var problem: String? { draft.problem }

    private var busy: Bool { templates.saving }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(original == nil ? "New template" : "Edit template").headlineText()
            Text("Sona writes one part of the notes for each section, in this order, following its instructions.")
                .bodyText(14, Theme.inkSecondary)

            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    MeetingTemplateField(
                        title: "Name", prompt: "Weekly planning", text: $name,
                        limit: MeetingTemplateLimits.nameBytes, oneLine: true, disabled: busy)
                    MeetingTemplateField(
                        title: "What this template is for (optional)",
                        prompt: "Planning meetings where the team picks what to do next.",
                        text: $purpose, limit: MeetingTemplateLimits.purposeBytes, disabled: busy)
                    sectionRows
                }
                .padding(.trailing, 4)
            }
            .frame(maxHeight: 480)

            if let problem, draft != initial {
                Text(problem).bodyText(14, Theme.live)
            }
            ErrorNote(templates.note)

            HStack(spacing: 10) {
                if original != nil {
                    Button("Delete") { confirmingDelete = true }
                        .buttonStyle(QuietButton(color: Theme.live))
                        .disabled(busy)
                }
                Spacer()
                Button("Cancel") { close() }
                    .buttonStyle(.secondary)
                    .disabled(busy)
                Button(busy ? "Saving…" : "Save") { save() }
                    .buttonStyle(.primary)
                    .disabled(problem != nil || busy)
            }
        }
        .padding(24)
        .frame(width: 600)
        .background(Theme.page)
        .onAppear { templates.clearNote() }
        .confirmationDialog("Delete this template?", isPresented: $confirmingDelete, titleVisibility: .visible) {
            Button("Delete", role: .destructive) { delete() }
            Button("Keep it", role: .cancel) {}
        } message: {
            Text("Meetings that used it go back to your default template.")
        }
    }

    private var sectionRows: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline) {
                Text("Sections").bodyText(14)
                Spacer(minLength: 8)
                Text("\(sections.count) of \(MeetingTemplateLimits.maxSections)").metaText()
            }
            ForEach($sections) { $section in
                let index = sections.firstIndex(where: { $0.id == section.id }) ?? 0
                MeetingTemplateSectionRow(
                    section: $section,
                    position: index + 1,
                    count: sections.count,
                    disabled: busy,
                    moveUp: { move(index, by: -1) },
                    moveDown: { move(index, by: 1) },
                    remove: { remove(index) })
            }
            Button("Add section") { sections.append(MeetingTemplateSectionDraft()) }
                .buttonStyle(.secondary)
                .disabled(sections.count >= MeetingTemplateLimits.maxSections || busy)
        }
    }

    private func move(_ index: Int, by offset: Int) {
        let target = index + offset
        guard sections.indices.contains(index), sections.indices.contains(target) else { return }
        sections.swapAt(index, target)
    }

    private func remove(_ index: Int) {
        guard sections.indices.contains(index) else { return }
        sections.remove(at: index)
    }

    private func save() {
        Task {
            if await templates.save(draft, replacing: original?.templateId) != nil {
                close()
            }
        }
    }

    private func delete() {
        guard let templateId = original?.templateId else { return }
        Task {
            if await templates.delete(templateId) {
                close()
            }
        }
    }
}

/// One section: where it sits, its title, and the instructions its part of
/// the notes is written from.
private struct MeetingTemplateSectionRow: View {
    @Binding var section: MeetingTemplateSectionDraft
    let position: Int
    let count: Int
    let disabled: Bool
    let moveUp: () -> Void
    let moveDown: () -> Void
    let remove: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 12) {
                Text("Section \(position)").metaText(Theme.inkSecondary)
                Spacer(minLength: 8)
                Button("Move up", action: moveUp)
                    .buttonStyle(.quiet)
                    .disabled(position == 1 || disabled)
                Button("Move down", action: moveDown)
                    .buttonStyle(.quiet)
                    .disabled(position == count || disabled)
                Button("Remove", action: remove)
                    .buttonStyle(QuietButton(color: Theme.live))
                    .disabled(count == 1 || disabled)
            }
            MeetingTemplateField(
                title: "Title", prompt: "Decisions", text: $section.title,
                limit: MeetingTemplateLimits.sectionTitleBytes, oneLine: true, disabled: disabled)
            MeetingTemplateField(
                title: "Instructions (optional)",
                prompt: "What was decided, one line each, and who decided it.",
                text: $section.instructions,
                limit: MeetingTemplateLimits.sectionInstructionsBytes, disabled: disabled)
        }
        .padding(12)
        .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
    }
}

/// A labelled field with its limit beside it. The count is what the core
/// counts, after trimming, so the hint turns red exactly when Save turns off.
private struct MeetingTemplateField: View {
    let title: String
    let prompt: String
    @Binding var text: String
    let limit: Int
    var oneLine = false
    var disabled = false

    private var used: Int { text.trimmingCharacters(in: .whitespacesAndNewlines).utf8.count }
    private var over: Bool { used > limit }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline) {
                Text(title).bodyText(14)
                Spacer(minLength: 8)
                Text(over ? "\(used - limit) over the limit of \(limit)" : "\(used) of \(limit)")
                    .metaText(over ? Theme.live : Theme.inkTertiary)
            }
            TextField(
                prompt, text: $text, prompt: Text(prompt).foregroundStyle(Theme.inkTertiary),
                axis: oneLine ? .horizontal : .vertical
            )
            .lineLimit(oneLine ? 1...1 : 1...4)
            .textFieldStyle(.plain)
            .font(TypeScale.body())
            .foregroundStyle(Theme.ink)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, minHeight: 36, alignment: .leading)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
            .disabled(disabled)
        }
    }
}
