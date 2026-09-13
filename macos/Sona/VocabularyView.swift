import SwiftUI

/// Which field holds the keyboard. A row commits when the focus leaves it, so
/// one identity per field is what the commit hangs on.
enum VocabularyFocus: Hashable {
    case ruleLeft(String)
    case ruleRight(String)
    case addLeft
    case addRight
    case sample(String)
}

/// Every text rule Sona applies after a transcript, as one settings tab.
///
/// Four switches say which kinds of rule are in force; one list holds the
/// rules themselves, whichever store they live in. Below them are the writing
/// samples a rewrite is shown as voice-matching examples.
struct VocabularyView: View {
    let store: VocabularyStore

    @State private var newKind: VocabularyRuleKind = .vocabulary
    @State private var newLeft = ""
    @State private var newRight = ""
    @FocusState private var focus: VocabularyFocus?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VocabularyHeading(
                "Vocabulary",
                fact: "Words Sona kept getting wrong until you told it how they are spelled, and every other rule it applies to a finished transcript."
            )
            ErrorNote(store.error)
            if store.error != nil, let retry = store.retry {
                Button("Try again", action: retry)
                    .buttonStyle(.compact)
                    .disabled(store.busy)
                    .padding(.bottom, 20)
            }
            switches
            rulesSection
            samplesSection
        }
        .onChange(of: focus) { previous, _ in
            commit(previous)
        }
        .sheet(isPresented: reviewPresented) {
            if let review = store.review {
                VocabularyImportSheet(
                    review: review,
                    savedCount: store.savedEntryCount,
                    busy: store.busy,
                    onStep: { store.setReviewStep($0) },
                    onClose: { store.closeReview() },
                    onApply: { store.applyImport() }
                )
            }
        }
    }

    // MARK: The four switches

    private var switches: some View {
        Card {
            ToggleRow(
                title: "Obey spoken editing commands",
                detail: "Act on “scratch that”, “delete last word”, “capitalize that”, “quote that” and “new bullet” instead of writing them. English only, and only when the phrase stands alone between two pauses.",
                isOn: Binding(
                    get: { store.spokenEditsEnabled },
                    set: { store.setSpokenEdits($0) }
                )
            )
            ToggleRow(
                title: "Write emoji when you name one",
                detail: "Replace only exact spoken tokens, after vocabulary correction.",
                isOn: Binding(
                    get: { store.emojiEnabled },
                    set: { store.setEmojiEnabled($0) }
                )
            )
            ToggleRow(
                title: "Expand shortcuts",
                isOn: Binding(
                    get: { store.snippetsEnabled },
                    set: { store.setSnippetsEnabled($0) }
                )
            )
            ToggleRow(
                title: "Apply rewrites",
                isOn: Binding(
                    get: { store.replacementsEnabled },
                    set: { store.setReplacementsEnabled($0) }
                )
            )
        }
        .disabled(store.busy)
        .padding(.bottom, 32)
    }

    // MARK: The merged rule list

    private var rulesSection: some View {
        let rules = store.rules
        let problems = store.problems
        return VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Text("Text rules").sectionLabel()
                Spacer()
                Button("Import CSV") { store.importCsv() }
                    .buttonStyle(.compact)
                    .disabled(store.busy)
                Button("Export CSV") { store.exportCsv() }
                    .buttonStyle(.compact)
                    .disabled(store.busy || store.savedEntryCount == 0)
                Button("Restore default rewrites") { store.restoreDefaultRewrites() }
                    .buttonStyle(.compact)
                    .disabled(store.busy)
            }
            Card {
                addRow
                if store.loading {
                    VocabularyBand {
                        Text("Loading text rules").metaText()
                    }
                } else if rules.isEmpty {
                    VocabularyBand {
                        Text("No text rules yet. Add one above and Sona applies it to every transcript.")
                            .metaText()
                    }
                } else {
                    VocabularyBand {
                        VocabularyRuleGrid(
                            kind: Text("Kind").metaText(),
                            left: Text("You say").metaText(),
                            right: Text("Sona writes").metaText(),
                            trailing: EmptyView()
                        )
                    }
                    LazyVStack(spacing: 0) {
                        ForEach(rules) { rule in
                            VocabularyRuleRow(
                                rule: rule,
                                problem: problems[rule.id],
                                busy: store.busy,
                                focus: $focus,
                                onEdit: { side, value in store.edit(rule, side, value) },
                                onCommit: { store.commit(rule) },
                                onToggle: { store.setEnabled(rule, $0) },
                                onRemove: { store.remove(rule) }
                            )
                        }
                    }
                }
                VocabularyBand {
                    Text("Rules match whole words and ignore case. When two fit the same spot the longer one wins.")
                        .metaText()
                }
            }
        }
        .padding(.bottom, 32)
    }

    private var addRow: some View {
        let problem = store.addProblem(kind: newKind, left: newLeft, right: newRight)
        let duplicate = problem != nil && !newLeft.isEmpty && !newRight.isEmpty
        return VocabularyBand {
            VocabularyRuleGrid(
                kind: Picker("", selection: $newKind) {
                    ForEach(VocabularyRuleKind.allCases, id: \.self) { kind in
                        Text(kind.title).tag(kind)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .disabled(store.busy),
                left: VocabularyInput(
                    prompt: newKind.spokenExample,
                    text: $newLeft,
                    key: .addLeft,
                    focus: $focus,
                    invalid: duplicate,
                    onSubmit: addRule
                )
                .disabled(store.busy),
                right: VocabularyInput(
                    prompt: newKind.writtenExample,
                    text: $newRight,
                    key: .addRight,
                    focus: $focus,
                    invalid: false,
                    onSubmit: addRule
                )
                .disabled(store.busy),
                trailing: Button("Add", action: addRule)
                    .buttonStyle(.compact)
                    .disabled(!store.canAdd(kind: newKind, left: newLeft, right: newRight))
            )
            Text(problem ?? newKind.hint)
                .metaText(problem == nil ? Theme.inkTertiary : Theme.live)
        }
    }

    private func addRule() {
        guard store.canAdd(kind: newKind, left: newLeft, right: newRight) else {
            return
        }
        store.add(kind: newKind, left: newLeft, right: newRight)
        newLeft = ""
        newRight = ""
    }

    // MARK: Writing samples

    private var samplesSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Writing samples").sectionLabel()
                Spacer()
                if !store.samplesLoading, !store.samples.isEmpty {
                    // The cap is the reason this count is worth printing: it is
                    // what explains where the Add button goes.
                    Text("\(store.samples.count) / \(PersonaSampleLimit.count)")
                        .metaText()
                        .monospacedDigit()
                }
            }
            Card {
                if store.samplesLoading {
                    VocabularyBand {
                        Text("Loading samples").metaText()
                    }
                } else if store.samplesUnreadable {
                    VocabularyBand {
                        HStack {
                            Text("Could not load samples.").metaText(Theme.live)
                            Spacer()
                            Button("Try again") { store.reloadSamples() }
                                .buttonStyle(.compact)
                        }
                    }
                } else if store.samples.isEmpty {
                    VocabularyBand {
                        HStack(alignment: .firstTextBaseline, spacing: 16) {
                            Text("Paste a few paragraphs you wrote yourself and rewrites will follow your vocabulary, sentence length, and formality.")
                                .metaText()
                            Spacer(minLength: 12)
                            Button("Add sample") { store.addSample() }
                                .buttonStyle(.compact)
                                .disabled(store.busy)
                        }
                    }
                } else {
                    ForEach(Array(store.samples.enumerated()), id: \.element.id) { index, sample in
                        VocabularySampleRow(
                            sample: sample,
                            number: index + 1,
                            busy: store.busy,
                            focus: $focus,
                            onEdit: { store.editSample(id: sample.id, text: $0) },
                            onRemove: { store.removeSample(id: sample.id) }
                        )
                    }
                    if store.samples.count < PersonaSampleLimit.count {
                        VocabularyBand {
                            Button("Add sample") { store.addSample() }
                                .buttonStyle(.compact)
                                .disabled(store.busy)
                        }
                    }
                }
                VocabularyBand {
                    Text("Samples are sent wherever the transcript itself already goes, and nowhere else.")
                        .metaText()
                }
            }
        }
    }

    // MARK: Commit on blur

    private var reviewPresented: Binding<Bool> {
        Binding(
            get: { store.review != nil },
            set: { presented in
                if !presented {
                    store.closeReview()
                }
            }
        )
    }

    /// The field the keyboard just left, written where it belongs.
    private func commit(_ field: VocabularyFocus?) {
        switch field {
        case let .ruleLeft(id), let .ruleRight(id):
            if let rule = store.rules.first(where: { $0.id == id }) {
                store.commit(rule)
            }
        case .sample:
            store.commitSamples()
        case .addLeft, .addRight, .none:
            return
        }
    }
}

/// The head of the tab: its name and one line of plain fact.
private struct VocabularyHeading: View {
    let title: String
    let fact: String

    init(_ title: String, fact: String) {
        self.title = title
        self.fact = fact
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).headlineText()
            Text(fact).bodyText(15, Theme.inkSecondary)
        }
        .padding(.bottom, 20)
    }
}

/// A band inside a card: the card's own padding and the hairline under it, for
/// rows that hold more than one line.
private struct VocabularyBand<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            content
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { Hairline() }
    }
}

/// One grid template for the column names, the add row and every rule, so the
/// kind, the two fields and the trailing controls line up down the list. The
/// trailing column is fixed because its switch comes and goes with the kind.
private struct VocabularyRuleGrid<Kind: View, Left: View, Right: View, Trailing: View>: View {
    let kind: Kind
    let left: Left
    let right: Right
    let trailing: Trailing

    var body: some View {
        HStack(spacing: 10) {
            kind.frame(width: 104, alignment: .leading)
            left.frame(maxWidth: .infinity, alignment: .leading)
            right.frame(maxWidth: .infinity, alignment: .leading)
            HStack(spacing: 8) {
                Spacer(minLength: 0)
                trailing
            }
            .frame(width: 132, alignment: .trailing)
        }
    }
}

/// Literal text the person typed for the machine: a trigger, a replacement, a
/// spoken phrase. Set a step down from prose so a long list stays scannable.
private struct VocabularyInput: View {
    let prompt: String
    @Binding var text: String
    let key: VocabularyFocus
    @FocusState.Binding var focus: VocabularyFocus?
    var invalid = false
    let onSubmit: () -> Void

    var body: some View {
        TextField(prompt, text: $text, prompt: Text(prompt).foregroundStyle(Theme.inkTertiary))
            .textFieldStyle(.plain)
            .font(TypeScale.body(13))
            .foregroundStyle(Theme.ink)
            .focused($focus, equals: key)
            .onSubmit(onSubmit)
            .padding(.horizontal, 10)
            .frame(height: 32)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.radiusControl)
                    .strokeBorder(invalid ? Theme.live : Theme.border, lineWidth: 1)
            )
    }
}

/// One rule. The switch is state rather than an action, so it stays visible
/// beside the delete control.
private struct VocabularyRuleRow: View {
    let rule: VocabularyRule
    let problem: String?
    let busy: Bool
    @FocusState.Binding var focus: VocabularyFocus?
    let onEdit: (VocabularyRuleSide, String) -> Void
    let onCommit: () -> Void
    let onToggle: (Bool) -> Void
    let onRemove: () -> Void

    var body: some View {
        VocabularyBand {
            VocabularyRuleGrid(
                kind: Text(rule.kind.title).metaText(Theme.inkSecondary),
                left: VocabularyInput(
                    prompt: rule.kind.spokenExample,
                    text: Binding(get: { rule.left }, set: { onEdit(.left, $0) }),
                    key: .ruleLeft(rule.id),
                    focus: $focus,
                    invalid: problem != nil,
                    onSubmit: onCommit
                )
                .disabled(busy),
                right: VocabularyInput(
                    prompt: rule.kind.writtenExample,
                    text: Binding(get: { rule.right }, set: { onEdit(.right, $0) }),
                    key: .ruleRight(rule.id),
                    focus: $focus,
                    onSubmit: onCommit
                )
                .disabled(busy),
                trailing: HStack(spacing: 8) {
                    if let enabled = rule.enabled {
                        Toggle("", isOn: Binding(get: { enabled }, set: onToggle))
                            .labelsHidden()
                            .toggleStyle(.switch)
                            .controlSize(.mini)
                            .tint(Theme.accent)
                            .disabled(busy)
                    }
                    Button {
                        onRemove()
                    } label: {
                        Image(systemName: "trash")
                            .font(.system(size: 13, weight: .medium))
                    }
                    .buttonStyle(.quiet)
                    .disabled(busy)
                    .help("Delete \(rule.left)")
                }
            )
            if let problem {
                Text(problem).metaText(Theme.live)
            }
        }
    }
}

/// One sample of the person's own prose, set as prose.
private struct VocabularySampleRow: View {
    let sample: PersonaSample
    let number: Int
    let busy: Bool
    @FocusState.Binding var focus: VocabularyFocus?
    let onEdit: (String) -> Void
    let onRemove: () -> Void

    var body: some View {
        let words = sample.wordCount
        let overLimit = words > PersonaSampleLimit.words
        return VocabularyBand {
            HStack {
                Text("Writing sample \(number)").bodyText(14, Theme.inkSecondary)
                Spacer()
                Text(
                    overLimit
                        ? "\(words) words. Only the first \(PersonaSampleLimit.words) are used."
                        : "\(words) of \(PersonaSampleLimit.words) words"
                )
                .metaText(overLimit ? Theme.accent : Theme.inkTertiary)
            }
            HStack(alignment: .top, spacing: 12) {
                TextEditor(text: Binding(get: { sample.text }, set: onEdit))
                    .focused($focus, equals: .sample(sample.id))
                    .font(TypeScale.body(14))
                    .foregroundStyle(Theme.ink)
                    .scrollContentBackground(.hidden)
                    .padding(8)
                    .frame(minHeight: 96)
                    .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                    .overlay(
                        RoundedRectangle(cornerRadius: Theme.radiusControl)
                            .strokeBorder(Theme.border, lineWidth: 1)
                    )
                    .disabled(busy)
                Button {
                    onRemove()
                } label: {
                    Image(systemName: "trash")
                        .font(.system(size: 13, weight: .medium))
                }
                .buttonStyle(.quiet)
                .disabled(busy)
                .help("Delete sample \(number)")
                .padding(.top, 6)
            }
        }
    }
}

/// Two steps, because a CSV import replaces the saved list: read what the file
/// contains, then confirm the replacement. Nothing is written until the last
/// button.
private struct VocabularyImportSheet: View {
    let review: VocabularyImportReview
    let savedCount: Int
    let busy: Bool
    let onStep: (VocabularyImportStep) -> Void
    let onClose: () -> Void
    let onApply: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Review vocabulary import").headlineText()
                Text("Nothing changes until you apply this preview.").metaText()
            }
            if review.step == .review {
                counts
                if !review.preview.canApply {
                    Text("Fix every invalid, duplicate, or conflicting row before applying.")
                        .metaText(Theme.live)
                }
                if !review.preview.entries.isEmpty {
                    entryTable
                }
            } else {
                Text("Applying replaces the \(savedCount) saved pairs with the \(review.preview.entries.count) pairs from this file.")
                    .bodyText(14, Theme.inkSecondary)
            }
            HStack(spacing: 10) {
                Spacer()
                if review.step == .review {
                    Button("Cancel", action: onClose)
                        .buttonStyle(.secondary)
                        .disabled(busy)
                    Button("Continue") { onStep(.confirm) }
                        .buttonStyle(.primary)
                        .disabled(busy || !review.preview.canApply)
                } else {
                    Button("Back") { onStep(.review) }
                        .buttonStyle(.secondary)
                        .disabled(busy)
                    Button("Apply import", action: onApply)
                        .buttonStyle(.primary)
                        .disabled(busy || !review.preview.canApply)
                }
            }
        }
        .padding(24)
        .frame(width: 520)
        .background(Theme.page)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusDialog))
    }

    /// Only the count that always means something, plus the ones that are a
    /// reason to stop. A row of zeroes is noise the table below already denies.
    private var counts: some View {
        let preview = review.preview
        var facts: [(String, Int)] = [("Valid pairs", preview.validRows)]
        if preview.invalidRows > 0 {
            facts.append(("Invalid rows", preview.invalidRows))
        }
        if preview.duplicateRows > 0 {
            facts.append(("Duplicate rows", preview.duplicateRows))
        }
        if preview.conflictRows > 0 {
            facts.append(("Conflicting rows", preview.conflictRows))
        }
        return HStack(spacing: 18) {
            ForEach(facts, id: \.0) { fact in
                HStack(spacing: 6) {
                    Text(fact.0).metaText()
                    Text("\(fact.1)").font(TypeScale.label(14)).foregroundStyle(Theme.ink)
                }
            }
        }
    }

    private var entryTable: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 12) {
                Text("Spoken phrase").metaText().frame(maxWidth: .infinity, alignment: .leading)
                Text("Written text").metaText().frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            .overlay(alignment: .bottom) { Hairline() }
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(Array(review.preview.entries.enumerated()), id: \.offset) { _, entry in
                        HStack(spacing: 12) {
                            Text(entry.spoken)
                                .bodyText(13)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            Text(entry.written)
                                .bodyText(13, Theme.inkSecondary)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .padding(.horizontal, 14)
                        .padding(.vertical, 6)
                        .overlay(alignment: .bottom) { Hairline() }
                    }
                }
            }
            .frame(maxHeight: 220)
        }
        .background(Theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusCard))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.radiusCard)
                .strokeBorder(Theme.border, lineWidth: 1)
        )
    }
}
