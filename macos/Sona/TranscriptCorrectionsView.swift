import Foundation
import Observation
import SwiftUI

private enum TranscriptCorrectionScope: Hashable {
    case meeting(String)
    case person(String)
}

private enum CorrectionKind: String, Codable, CaseIterable, Identifiable {
    case phrase
    case name
    case term
    case dialect
    case codeSwitching = "code_switching"

    var id: String { rawValue }
    var title: String {
        switch self {
        case .phrase: "Phrase"
        case .name: "Name"
        case .term: "Specialist term"
        case .dialect: "Dialect"
        case .codeSwitching: "Code-switching"
        }
    }
    var needsContext: Bool { self == .dialect || self == .codeSwitching }
}

private struct CorrectionContext: Decodable {
    enum Kind: String, Decodable, CaseIterable, Identifiable {
        case person
        case conversation
        case surroundingPhrase = "surrounding_phrase"

        var id: String { rawValue }
        var title: String {
            switch self {
            case .person: "This person's confirmed voices"
            case .conversation: "This conversation"
            case .surroundingPhrase: "When the line contains…"
            }
        }
    }
    let kind: Kind
    let phrase: String?

    var request: JSONValue {
        var value: [String: JSONValue] = ["kind": .string(kind.rawValue)]
        if kind == .surroundingPhrase, let phrase { value["phrase"] = .string(phrase) }
        return .object(value)
    }
    var title: String {
        if kind == .surroundingPhrase, let phrase { return "When the line contains “\(phrase)”" }
        return kind.title
    }
}

private enum CorrectionStatus: String, Decodable {
    case active
    case paused
    case sourceChanged = "source_changed"
    case conflicting

    var title: String {
        switch self {
        case .active: "Applying"
        case .paused: "Paused"
        case .sourceChanged: "Source voice needs confirmation"
        case .conflicting: "Conflicting phrases · review before applying"
        }
    }
}

private struct PersonTranscriptCorrection: Decodable, Identifiable {
    let correctionId: String
    let personId: String
    let sourceSessionId: String
    let sourceSegmentId: String
    let sourceStartOffsetNs: Int64
    let sourceAudioRetained: Bool
    let originalText: String
    let correctedText: String
    let originalPhrase: String
    let replacementPhrase: String
    let kind: CorrectionKind
    let context: CorrectionContext
    let enabled: Bool
    let status: CorrectionStatus
    var id: String { correctionId }
}

private struct PersonTranscriptCorrectionList: Decodable {
    let revision: Int
    let rules: [PersonTranscriptCorrection]
}

private struct CorrectableTranscriptSegment: Decodable, Identifiable {
    let segmentId: String
    let startOffsetNs: Int64
    let personId: String?
    let personName: String?
    let identityConfirmed: Bool
    let audioRetained: Bool
    let rawText: String
    let correctedText: String
    let explicitlyEdited: Bool
    var id: String { segmentId }

    var label: String {
        "\(startOffsetNs.meetingOffsetClock) · \(personName ?? "Unknown voice") · \(rawText.prefix(72))"
    }
}

private struct PersonTranscriptCorrectionReview: Decodable {
    let sessionId: String
    let meetingRevision: Int
    let correctionRevision: Int
    let canEdit: Bool
    let segments: [CorrectableTranscriptSegment]
    let rules: [PersonTranscriptCorrection]
}

private struct PersonTranscriptCorrectionMutation: Decodable {
    let corrections: PersonTranscriptCorrectionList
}

@MainActor
@Observable
private final class TranscriptCorrectionsStore {
    private let core: Core
    let relisten: RelistenStore
    private var scope: TranscriptCorrectionScope?
    private var generation = 0
    private(set) var review: PersonTranscriptCorrectionReview?
    private(set) var rules: [PersonTranscriptCorrection] = []
    private(set) var revision = 0
    private(set) var loading = true
    private(set) var busy = false
    private(set) var error: String?

    init(core: Core) {
        self.core = core
        relisten = RelistenStore(core: core)
    }

    func load(_ target: TranscriptCorrectionScope) async {
        if scope != target {
            review = nil
            rules = []
            error = nil
            relisten.clear()
        }
        scope = target
        generation += 1
        let requestGeneration = generation
        loading = true
        defer { if requestGeneration == generation { loading = false } }
        do {
            switch target {
            case let .meeting(sessionId):
                let result: PersonTranscriptCorrectionReview = try await core.request(
                    "transcript_correction_review", ["sessionId": JSONValue.string(sessionId)])
                guard requestGeneration == generation, !Task.isCancelled else { return }
                review = result
                rules = result.rules
                revision = result.correctionRevision
            case let .person(personId):
                let result: PersonTranscriptCorrectionList = try await core.request(
                    "transcript_correction_list", ["personId": JSONValue.string(personId)])
                guard requestGeneration == generation, !Task.isCancelled else { return }
                rules = result.rules
                revision = result.revision
            }
            error = nil
        } catch {
            guard requestGeneration == generation, !Task.isCancelled else { return }
            self.error = reason(error)
        }
    }

    func saveOnce(_ segment: CorrectableTranscriptSegment, text: String) async -> Bool {
        guard !busy, let review, review.canEdit else { return false }
        busy = true
        error = nil
        defer { busy = false }
        do {
            let result: MeetingMutationResult = try await core.request(
                "meeting_segment_edit",
                MeetingRequest.segmentEdit(review.sessionId, revision: review.meetingRevision,
                    segmentId: segment.segmentId, replacementText: text, removed: false))
            guard result.receipt.result == .committed else {
                let refusal = result.receipt.refusal
                if let scope { await load(scope) }
                error = refusal
                return false
            }
            if let scope { await load(scope) }
            if let fix = RelistenFix.between(sessionId: review.sessionId, segmentId: segment.segmentId,
                                             shown: segment.correctedText, saved: text) {
                relisten.check(fix)
            } else {
                relisten.clear()
            }
            return true
        } catch {
            let message = reason(error)
            if let scope { await load(scope) }
            self.error = message
            return false
        }
    }

    func teach(_ segment: CorrectableTranscriptSegment, original: String, replacement: String,
               kind: CorrectionKind, context: CorrectionContext) async -> Bool {
        guard let review, review.canEdit, segment.identityConfirmed,
              let personId = segment.personId else { return false }
        let taught = await mutate("transcript_correction_teach", request: [
            "person_id": .string(personId),
            "session_id": .string(review.sessionId),
            "segment_id": .string(segment.segmentId),
            "expected_meeting_revision": .number(Double(review.meetingRevision)),
            "expected_correction_revision": .number(Double(revision)),
            "original_phrase": .string(original),
            "replacement_phrase": .string(replacement),
            "kind": .string(kind.rawValue),
            "context": context.request,
        ])
        if taught {
            relisten.check(RelistenFix(sessionId: review.sessionId, segmentId: segment.segmentId,
                                       original: original, replacement: replacement))
        }
        return taught
    }

    func update(_ rule: PersonTranscriptCorrection, original: String, replacement: String,
                kind: CorrectionKind, context: CorrectionContext, enabled: Bool, expectedRevision: Int) async -> Bool {
        await mutate("transcript_correction_update", request: [
            "correction_id": .string(rule.correctionId),
            "person_id": .string(rule.personId),
            "expected_correction_revision": .number(Double(expectedRevision)),
            "original_phrase": .string(original),
            "replacement_phrase": .string(replacement),
            "kind": .string(kind.rawValue),
            "context": context.request,
            "enabled": .bool(enabled),
        ])
    }

    func setEnabled(_ rule: PersonTranscriptCorrection, _ enabled: Bool) async {
        _ = await update(rule, original: rule.originalPhrase, replacement: rule.replacementPhrase,
                         kind: rule.kind, context: rule.context, enabled: enabled, expectedRevision: revision)
    }

    func remove(_ rule: PersonTranscriptCorrection) async -> Bool {
        await mutate("transcript_correction_remove", request: [
            "correction_id": .string(rule.correctionId),
            "person_id": .string(rule.personId),
            "expected_correction_revision": .number(Double(revision)),
        ])
    }

    private func mutate(_ command: String, request: [String: JSONValue]) async -> Bool {
        guard !busy else { return false }
        busy = true
        error = nil
        defer { busy = false }
        do {
            let _: PersonTranscriptCorrectionMutation = try await core.request(
                command, ["request": JSONValue.object(request)])
            if let scope { await load(scope) }
            return true
        } catch {
            let message = reason(error)
            if let scope { await load(scope) }
            self.error = message
            return false
        }
    }

    private func reason(_ error: Error) -> String {
        guard let failure = error as? CoreError else { return error.localizedDescription }
        switch failure.remote(as: MeetingCommandError.self) {
        case .consentRequired: return "Confirm this source voice before teaching or resuming."
        case .invalidRequest: return "Use exact whole phrases from the captured line and choose a context."
        case .invalidTransition: return "Review the existing phrase or choose a different context."
        case .staleRevision: return "The transcript changed. Review it and try again."
        case let .some(reason): return reason.label
        case .none: return failure.localizedDescription
        }
    }
}

struct TranscriptCorrectionsView: View {
    private let scope: TranscriptCorrectionScope
    private let onListenSource: ((String, String) -> Void)?
    @State private var store: TranscriptCorrectionsStore
    @State private var selectedSegmentId: String?
    @State private var editingRule: PersonTranscriptCorrection?
    @State private var removingRule: PersonTranscriptCorrection?

    init(core: Core, sessionId: String, onListenSource: ((String, String) -> Void)? = nil) {
        scope = .meeting(sessionId)
        self.onListenSource = onListenSource
        _store = State(initialValue: TranscriptCorrectionsStore(core: core))
    }

    init(core: Core, personId: String, onListenSource: ((String, String) -> Void)? = nil) {
        scope = .person(personId)
        self.onListenSource = onListenSource
        _store = State(initialValue: TranscriptCorrectionsStore(core: core))
    }

    var body: some View {
        PageSection("Transcript corrections") {
            Card {
                if let error = store.error {
                    CardRow {
                        Text(error).metaText(Theme.live)
                    } trailing: {
                        Button("Reload") { Task { await store.load(scope) } }
                            .buttonStyle(.quiet)
                            .disabled(store.busy)
                    }
                }
                if store.loading && store.rules.isEmpty && store.review == nil {
                    CardLine("Reading corrections…")
                } else {
                    if let review = store.review { meetingEditor(review) }
                    if store.rules.isEmpty {
                        CardLine("No learned phrases.")
                    } else {
                        ForEach(store.rules) { rule in ruleRow(rule) }
                    }
                }
            }
        }
        .task(id: scope) {
            selectedSegmentId = nil
            await store.load(scope)
        }
        .sheet(item: $editingRule) { rule in
            LearnedCorrectionEditor(store: store, rule: rule, onListenSource: onListenSource) {
                editingRule = nil
            }
        }
        .relistenSheet(store.relisten)
        .alert("Remove this learned phrase?", isPresented: Binding(
            get: { removingRule != nil }, set: { if !$0 { removingRule = nil } })
        ) {
            Button("Remove", role: .destructive) {
                if let rule = removingRule { Task { _ = await store.remove(rule) } }
                removingRule = nil
            }
            Button("Cancel", role: .cancel) { removingRule = nil }
        } message: {
            Text("The captured transcript and one-off edits stay unchanged.")
        }
    }

    private func meetingEditor(_ review: PersonTranscriptCorrectionReview) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            DisclosureGroup("Review a line") {
                VStack(alignment: .leading, spacing: 12) {
                    if review.segments.isEmpty {
                        Text("No transcript lines.").metaText()
                    } else {
                        Picker("Line", selection: $selectedSegmentId) {
                            Text("Choose a line").tag(String?.none)
                            ForEach(review.segments) { segment in
                                Text(segment.label).tag(Optional(segment.segmentId))
                            }
                        }
                        .labelsHidden()
                        .accessibilityLabel("Transcript line")
                        if let segment = review.segments.first(where: { $0.segmentId == selectedSegmentId }) {
                            TranscriptCorrectionLineEditor(store: store, segment: segment,
                                sessionId: review.sessionId, canEdit: review.canEdit,
                                onListenSource: onListenSource)
                                .id("\(segment.segmentId):\(review.meetingRevision)")
                        }
                    }
                }
                .padding(.top, 10)
            }
            .font(TypeScale.body(13))
            .foregroundStyle(Theme.ink)
        }
        .padding(16)
        .overlay(alignment: .bottom) { Hairline().padding(.horizontal, 16) }
    }

    private func ruleRow(_ rule: PersonTranscriptCorrection) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("\(rule.originalPhrase) → \(rule.replacementPhrase)")
                .bodyText(14).textSelection(.enabled)
            if case .meeting = scope,
               let person = store.review?.segments.first(where: { $0.personId == rule.personId })?.personName {
                Text(person).metaText(Theme.inkSecondary)
            }
            Text("\(rule.kind.title) · \(rule.context.title)").metaText(Theme.inkSecondary)
            Text(rule.status.title).metaText(rule.status == .conflicting ? Theme.live : Theme.inkSecondary)
            HStack(spacing: 14) {
                Button("Review & edit") { editingRule = rule }.buttonStyle(.quiet)
                    .disabled(store.busy)
                Button(rule.enabled ? "Pause" : "Resume") {
                    Task { await store.setEnabled(rule, !rule.enabled) }
                }
                .buttonStyle(.quiet)
                .disabled(store.busy || (!rule.enabled && rule.status == .sourceChanged))
                Button("Remove") { removingRule = rule }
                    .buttonStyle(QuietButton(color: Theme.live)).disabled(store.busy)
                Spacer(minLength: 0)
            }
            DisclosureGroup("Captured line · \(rule.sourceStartOffsetNs.meetingOffsetClock)") {
                VStack(alignment: .leading, spacing: 8) {
                    Text(rule.originalText).bodyText(13, Theme.inkSecondary).textSelection(.enabled)
                    sourceListen(rule)
                }
                .padding(.top, 6)
            }
            .font(TypeScale.body(12)).foregroundStyle(Theme.inkSecondary)
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { Hairline().padding(.horizontal, 16) }
    }

    @ViewBuilder
    private func sourceListen(_ rule: PersonTranscriptCorrection) -> some View {
        if rule.sourceAudioRetained, let onListenSource {
            Button("Listen at source", systemImage: "play.fill") {
                onListenSource(rule.sourceSessionId, rule.sourceSegmentId)
            }.buttonStyle(.quiet)
        } else if !rule.sourceAudioRetained {
            Text("Recording not retained.").metaText()
        }
    }
}

private struct CorrectionScopeFields: View {
    @Binding var kind: CorrectionKind
    @Binding var contextKind: CorrectionContext.Kind
    @Binding var contextPhrase: String

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Picker("Wording", selection: $kind) {
                ForEach(CorrectionKind.allCases) { value in Text(value.title).tag(value) }
            }
            Picker("Apply to", selection: $contextKind) {
                ForEach(CorrectionContext.Kind.allCases.filter { !kind.needsContext || $0 != .person }) { value in
                    Text(value.title).tag(value)
                }
            }
            if contextKind == .surroundingPhrase {
                InputField(prompt: "Exact phrase in the captured line", text: $contextPhrase)
                    .accessibilityLabel("Required surrounding phrase from the captured line")
            }
        }
        .onChange(of: kind) { _, value in
            if value.needsContext && contextKind == .person { contextKind = .conversation }
        }
    }
}

private struct TranscriptCorrectionLineEditor: View {
    let store: TranscriptCorrectionsStore
    let segment: CorrectableTranscriptSegment
    let sessionId: String
    let canEdit: Bool
    let onListenSource: ((String, String) -> Void)?
    @State private var text = ""
    @State private var original = ""
    @State private var replacement = ""
    @State private var kind: CorrectionKind = .phrase
    @State private var contextKind: CorrectionContext.Kind = .conversation
    @State private var contextPhrase = ""
    @State private var savedOnce = false
    @State private var taught = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Captured").sectionLabel()
                Spacer(minLength: 0)
                if segment.audioRetained, let onListenSource {
                    Button("Listen at source", systemImage: "play.fill") {
                        onListenSource(sessionId, segment.segmentId)
                    }.buttonStyle(.quiet)
                }
            }
            Text(segment.rawText).bodyText(14, Theme.inkSecondary).textSelection(.enabled)
            if !segment.audioRetained { Text("Recording not retained.").metaText() }
            TextField("Corrected line", text: $text, axis: .vertical)
                .lineLimit(2...8).textFieldStyle(.roundedBorder)
                .font(TypeScale.body(14)).accessibilityLabel("Corrected line, this occurrence only")
            Button(savedOnce ? "Saved this line" : "Save this line only") {
                Task { savedOnce = await store.saveOnce(segment, text: text) }
            }
            .buttonStyle(.compact)
            .disabled(!canEdit || store.busy || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                || text == segment.correctedText)
            if segment.explicitlyEdited { Text("One-off edit · not a learned phrase").metaText() }
            if store.relisten.fix?.segmentId == segment.segmentId {
                RelistenPrompt(store: store.relisten)
            }
            Hairline()
            if let name = segment.personName, segment.personId != nil, segment.identityConfirmed {
                DisclosureGroup("Teach a phrase for \(name)") {
                    VStack(alignment: .leading, spacing: 10) {
                        InputField(prompt: "Captured phrase", text: $original)
                            .accessibilityLabel("Exact phrase in the captured transcript")
                        InputField(prompt: "Intended phrase", text: $replacement)
                            .accessibilityLabel("Intended wording")
                        CorrectionScopeFields(kind: $kind, contextKind: $contextKind, contextPhrase: $contextPhrase)
                        Button(taught ? "Learned" : "Teach for \(name)") {
                            Task {
                                taught = await store.teach(segment, original: original, replacement: replacement,
                                    kind: kind, context: CorrectionContext(kind: contextKind,
                                        phrase: contextKind == .surroundingPhrase ? contextPhrase : nil))
                            }
                        }
                        .buttonStyle(.primary)
                        .disabled(!canEdit || store.busy || original.isEmpty || replacement.isEmpty
                            || original == replacement || (contextKind == .surroundingPhrase && contextPhrase.isEmpty))
                    }
                    .padding(.top, 10)
                }
                .font(TypeScale.body(13)).foregroundStyle(Theme.ink)
            } else {
                Text("Confirm this source voice before teaching.").metaText(Theme.inkSecondary)
            }
            if !canEdit { Text("This meeting cannot be edited.").metaText(Theme.inkSecondary) }
        }
        .onAppear { text = segment.correctedText }
    }
}

private struct LearnedCorrectionEditor: View {
    let store: TranscriptCorrectionsStore
    let rule: PersonTranscriptCorrection
    let onListenSource: ((String, String) -> Void)?
    let close: () -> Void
    @State private var original = ""
    @State private var replacement = ""
    @State private var kind: CorrectionKind = .phrase
    @State private var contextKind: CorrectionContext.Kind = .conversation
    @State private var contextPhrase = ""
    @State private var enabled = false
    @State private var expectedRevision = 0

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Learned phrase").titleText()
            HStack {
                Text("Captured · \(rule.sourceStartOffsetNs.meetingOffsetClock)").sectionLabel()
                Spacer(minLength: 0)
                if rule.sourceAudioRetained, let onListenSource {
                    Button("Listen", systemImage: "play.fill") {
                        onListenSource(rule.sourceSessionId, rule.sourceSegmentId)
                    }.buttonStyle(.quiet)
                }
            }
            ScrollView {
                Text(rule.originalText).bodyText(13, Theme.inkSecondary).textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxHeight: 140)
            InputField(prompt: "Captured phrase", text: $original)
                .accessibilityLabel("Exact phrase in the captured transcript")
            InputField(prompt: "Intended phrase", text: $replacement)
                .accessibilityLabel("Intended wording")
            CorrectionScopeFields(kind: $kind, contextKind: $contextKind, contextPhrase: $contextPhrase)
            Toggle("Apply learned phrase", isOn: $enabled)
                .toggleStyle(.switch).font(TypeScale.body(13))
            if rule.status == .sourceChanged {
                Text("Confirm the source voice before resuming.").metaText(Theme.inkSecondary)
            }
            if let error = store.error { Text(error).metaText(Theme.live) }
            HStack(spacing: 12) {
                Button("Cancel", action: close).buttonStyle(.quiet).keyboardShortcut(.cancelAction)
                Spacer(minLength: 0)
                Button("Save") {
                    Task {
                        if await store.update(rule, original: original, replacement: replacement, kind: kind,
                            context: CorrectionContext(kind: contextKind,
                                phrase: contextKind == .surroundingPhrase ? contextPhrase : nil), enabled: enabled,
                            expectedRevision: expectedRevision) {
                            close()
                        }
                    }
                }
                .buttonStyle(.primary).keyboardShortcut(.defaultAction)
                .disabled(store.busy || original.isEmpty || replacement.isEmpty || original == replacement
                    || (contextKind == .surroundingPhrase && contextPhrase.isEmpty)
                    || (enabled && rule.status == .sourceChanged))
            }
        }
        .padding(24).frame(width: 480).background(Theme.page)
        .onAppear {
            original = rule.originalPhrase
            replacement = rule.replacementPhrase
            kind = rule.kind
            contextKind = rule.context.kind
            contextPhrase = rule.context.phrase ?? ""
            enabled = rule.enabled
            expectedRevision = store.revision
        }
    }
}
