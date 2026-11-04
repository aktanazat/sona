import SwiftUI

struct PhoneConversationScreen: View {
    let meeting: PhoneMeeting
    private let turns: [PhoneMeeting.Turn]
    private let initialTurnID: String?
    @State private var showDialogue: Bool
    @State private var showOriginal = false

    init(meeting: PhoneMeeting, initialOffsetNanoseconds: UInt64? = nil) {
        self.meeting = meeting
        let turns = meeting.turns
        self.turns = turns
        initialTurnID = initialOffsetNanoseconds.flatMap { offset in
            turns.last(where: { $0.segment.start_offset_ns <= offset })?.id ?? turns.first?.id
        }
        _showDialogue = State(initialValue: initialOffsetNanoseconds != nil)
    }

    var body: some View {
        ScrollViewReader { scroll in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 24) {
                    context
                    Picker("meeting.content", selection: $showDialogue) {
                        Text("library.notes").tag(false)
                        Text("meeting.transcript").tag(true)
                    }
                    .pickerStyle(.segmented)
                    if showDialogue {
                        dialogue
                    } else {
                        PhoneConversationNotes(meeting: meeting)
                    }
                }
                .padding(Theme.pageInset)
            }
            .background(Theme.background)
            .navigationTitle(meeting.session.title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar(.hidden, for: .tabBar)
            .toolbar {
                ShareLink(item: shareText) {
                    Label("library.share", systemImage: "square.and.arrow.up")
                }
            }
            .onAppear {
                if let initialTurnID { scroll.scrollTo(initialTurnID, anchor: .top) }
            }
        }
        .tint(Theme.accent)
    }

    private var context: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(meeting.date.formatted(date: .abbreviated, time: .shortened))
                .font(.subheadline)
                .foregroundStyle(Theme.textSecondary)
            if let kind = meeting.conversationKindLabel {
                Text(kind)
                    .font(.headline)
                    .foregroundStyle(Theme.textPrimary)
            }
            PhoneConversationSource(meeting: meeting)
            if let place = meeting.conversation_context?.location {
                VStack(alignment: .leading, spacing: 8) {
                    Text(place.name)
                        .font(.body.weight(.medium))
                        .foregroundStyle(Theme.textPrimary)
                        .textSelection(.enabled)
                    if let url = place.mapsURL {
                        Link(destination: url) {
                            Label(String(localized: "conversation.openMaps", defaultValue: "Open in Maps"), systemImage: "map")
                                .frame(minHeight: 44, alignment: .leading)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(16)
                .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.controlRadius))
            }
        }
    }

    @ViewBuilder private var dialogue: some View {
        if let purged = meeting.transcript_purged_at_utc_ms {
            VStack(alignment: .leading, spacing: 4) {
                Text(String(localized: "conversation.transcriptRemoved", defaultValue: "Transcript deleted"))
                    .foregroundStyle(Theme.textSecondary)
                Text(Date(timeIntervalSince1970: Double(purged) / 1000), style: .date)
                    .font(.footnote)
                    .foregroundStyle(Theme.textTertiary)
            }
        } else if turns.isEmpty {
            Text("meeting.noTranscript")
                .foregroundStyle(Theme.textSecondary)
        } else {
            Picker(String(localized: "conversation.wording", defaultValue: "Wording"), selection: $showOriginal) {
                Text(String(localized: "conversation.corrected", defaultValue: "Corrected")).tag(false)
                Text(String(localized: "conversation.original", defaultValue: "Original")).tag(true)
            }
            .pickerStyle(.segmented)
            ForEach(turns) { turn in
                VStack(alignment: .leading, spacing: 0) {
                    PhoneConversationTurn(turn: turn, showOriginal: showOriginal)
                    Divider().overlay(Theme.border)
                }
                .id(turn.id)
            }
        }
        Text(String(localized: "conversation.noAudio", defaultValue: "Audio is not included in this copy."))
            .font(.footnote)
            .foregroundStyle(Theme.textSecondary)
    }

    private var shareText: String {
        let generated = meeting.savedNotes
        let personal = ([meeting.user_notes?.body ?? ""] + (meeting.manual_notes ?? []).map(\.body))
            .filter { !$0.isEmpty }.joined(separator: "\n\n")
        return [meeting.session.title, generated, personal, showOriginal ? meeting.originalTranscript : meeting.transcript]
            .filter { !$0.isEmpty }.joined(separator: "\n\n")
    }
}

private struct PhoneConversationTurn: View {
    let turn: PhoneMeeting.Turn
    let showOriginal: Bool
    var highlighted = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    speaker
                    Spacer(minLength: 8)
                    timestamp
                }
                VStack(alignment: .leading, spacing: 4) {
                    speaker
                    timestamp
                }
            }
            if !showOriginal && turn.isRemoved {
                Text(String(localized: "conversation.removedInReview", defaultValue: "Removed in review"))
                    .font(.body)
                    .foregroundStyle(Theme.textSecondary)
            } else {
                Text(showOriginal ? turn.originalText : turn.correctedText)
                    .font(.body)
                    .foregroundStyle(Theme.textPrimary)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
            if turn.isEdited && (showOriginal || !turn.isRemoved) {
                Text(turn.isRemoved
                     ? String(localized: "conversation.removedInReview", defaultValue: "Removed in review")
                     : String(localized: "conversation.manualEdit", defaultValue: "Manual edit"))
                    .font(.caption)
                    .foregroundStyle(Theme.textSecondary)
            }
        }
        .padding(.vertical, 12)
        .padding(.horizontal, highlighted ? 12 : 0)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(highlighted ? Theme.inset : Color.clear,
                    in: RoundedRectangle(cornerRadius: Theme.controlRadius))
        .accessibilityElement(children: .contain)
    }

    private var speaker: some View {
        Text(turn.speakerName)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(Theme.textPrimary)
            .fixedSize(horizontal: false, vertical: true)
    }

    private var timestamp: some View {
        Text(turn.timestamp)
            .font(.footnote.monospacedDigit())
            .foregroundStyle(Theme.textSecondary)
            .fixedSize()
    }
}

private struct PhoneConversationSource: View {
    let meeting: PhoneMeeting

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let source = meeting.captureSourceLabel {
                Text(source)
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
            }
            if let applications = meeting.conversation_context?.application_bundle_ids, !applications.isEmpty {
                let names = applications.map(PhoneMeeting.applicationName).joined(separator: ", ")
                Text(String(localized: "conversation.selectedApps", defaultValue: "Selected apps: \(names)"))
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
                    .textSelection(.enabled)
            }
            if let calendar = meeting.conversation_context?.calendar_name, !calendar.isEmpty {
                Text(calendar)
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
            }
            if let calendarPlace = meeting.conversation_context?.calendar_location, !calendarPlace.isEmpty,
               calendarPlace != meeting.conversation_context?.location?.name {
                Text(String(localized: "conversation.calendarPlace", defaultValue: "Calendar place: \(calendarPlace)"))
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
            }
        }
    }
}

private struct PhoneConversationNotes: View {
    let meeting: PhoneMeeting

    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            if let own = meeting.user_notes?.body, !own.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                noteSection("meeting.ownNotes") { Text(own).textSelection(.enabled) }
            }
            if let manual = meeting.manual_notes, !manual.isEmpty {
                noteSection("library.notes") {
                    ForEach(manual) { note in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(note.body).textSelection(.enabled)
                            if let offset = note.start_offset_ns {
                                NavigationLink {
                                    PhoneConversationScreen(meeting: meeting, initialOffsetNanoseconds: offset)
                                } label: {
                                    Text(PhoneMeeting.timestamp(offset))
                                        .font(.footnote.monospacedDigit())
                                        .frame(minHeight: 44, alignment: .leading)
                                }
                            }
                        }
                    }
                }
            }
            if let artifact = meeting.savedArtifact, let content = artifact.content {
                VStack(alignment: .leading, spacing: 20) {
                    HStack(alignment: .firstTextBaseline) {
                        Text(String(localized: "conversation.generatedNotes", defaultValue: "Generated notes"))
                            .font(.headline)
                        Spacer()
                        if artifact.state != "current" {
                            Text(String(localized: "conversation.outOfDate", defaultValue: "Out of date"))
                                .font(.footnote)
                                .foregroundStyle(Theme.textSecondary)
                        }
                    }
                    summary(content)
                    if !content.outline.isEmpty {
                        noteSection("meeting.topics") {
                            ForEach(Array(content.outline.enumerated()), id: \.offset) { _, topic in
                                VStack(alignment: .leading, spacing: 8) {
                                    cited(topic.title)
                                    if let detail = topic.detail { cited(detail) }
                                }
                            }
                        }
                    }
                    citedSection("meeting.decisions", content.decisions)
                    if !content.action_items.isEmpty {
                        noteSection("meeting.actions") {
                            ForEach(Array(content.action_items.enumerated()), id: \.offset) { _, action in
                                VStack(alignment: .leading, spacing: 4) {
                                    cited(action.text)
                                    let detail = [action.owner_text, action.due_text].compactMap { $0 }.filter { !$0.isEmpty }
                                    if !detail.isEmpty {
                                        Text(detail.joined(separator: " · "))
                                            .font(.footnote)
                                            .foregroundStyle(Theme.textSecondary)
                                    }
                                }
                            }
                        }
                    }
                    citedSection("meeting.questions", content.key_questions)
                    citedSection("meeting.risks", content.risks)
                    if !content.follow_up_draft.text.isEmpty {
                        noteSection("meeting.followUp") { cited(content.follow_up_draft) }
                    }
                }
            } else if !hasPersonalNotes && (meeting.questions ?? []).isEmpty {
                Text(String(localized: "conversation.noSavedNotes", defaultValue: "No saved notes."))
                    .foregroundStyle(Theme.textSecondary)
            }
            if let answers = meeting.questions, !answers.isEmpty {
                noteSection("meeting.questions") {
                    ForEach(answers) { answer in
                        VStack(alignment: .leading, spacing: 8) {
                            if let question = answer.question {
                                Text(question).font(.body.weight(.medium)).textSelection(.enabled)
                            }
                            if let body = answer.answer { Text(body).textSelection(.enabled) }
                            if answer.state != "supported" || answer.provisional == true {
                                Text(answerState(answer))
                                    .font(.footnote)
                                    .foregroundStyle(Theme.textSecondary)
                            }
                            ForEach(Array(answer.citations.enumerated()), id: \.offset) { _, citation in
                                answerSource(citation)
                            }
                        }
                    }
                }
            }
        }
        .font(.body)
        .foregroundStyle(Theme.textPrimary)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var hasPersonalNotes: Bool {
        !(meeting.user_notes?.body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true) ||
            !(meeting.manual_notes ?? []).isEmpty
    }

    private func noteSection<Content: View>(_ title: LocalizedStringKey, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(title).font(.headline)
            content()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder private func citedSection(_ title: LocalizedStringKey, _ values: [PhoneMeeting.Text]) -> some View {
        if !values.isEmpty {
            noteSection(title) {
                ForEach(Array(values.enumerated()), id: \.offset) { _, value in cited(value) }
            }
        }
    }

    @ViewBuilder private func summary(_ content: PhoneMeeting.Content) -> some View {
        if let trace = content.summary_trace, !trace.isEmpty {
            ForEach(Array(content.summary.text.components(separatedBy: "\n").enumerated()), id: \.offset) { index, line in
                if !line.isEmpty {
                    cited(PhoneMeeting.Text(text: line, citations: trace.filter { $0.line == index }.map(\.anchor)))
                }
            }
            let traced = Set(trace.map(\.anchor))
            sources((content.summary.citations ?? []).filter { !traced.contains($0) })
        } else {
            cited(content.summary)
        }
    }

    private func cited(_ value: PhoneMeeting.Text) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            if !value.text.isEmpty { Text(value.text).textSelection(.enabled) }
            sources(value.citations ?? [])
        }
    }

    private func sources(_ citations: [PhoneMeeting.Citation]) -> some View {
        ForEach(Array(citations.enumerated()), id: \.offset) { _, citation in
            NavigationLink {
                PhoneConversationEvidenceScreen(meeting: meeting, citation: citation)
            } label: {
                Label(PhoneMeeting.timestamp(citation.start_offset_ns), systemImage: "text.quote")
                    .font(.footnote.monospacedDigit())
                    .frame(minHeight: 44, alignment: .leading)
            }
            .accessibilityLabel(String(localized: "conversation.sourceAt",
                                       defaultValue: "Source at \(PhoneMeeting.timestamp(citation.start_offset_ns))"))
        }
    }

    @ViewBuilder private func answerSource(_ citation: PhoneMeeting.AnswerCitation) -> some View {
        if citation.session_id == meeting.id && citation.kind == "transcript" {
            NavigationLink {
                PhoneConversationEvidenceScreen(meeting: meeting, segmentID: citation.entity_id,
                                                offsetNanoseconds: citation.start_offset_ns)
            } label: {
                Label(citation.start_offset_ns.map(PhoneMeeting.timestamp) ??
                      String(localized: "conversation.source", defaultValue: "Source"), systemImage: "text.quote")
                    .font(.footnote.monospacedDigit())
                    .frame(minHeight: 44, alignment: .leading)
            }
        } else if citation.session_id == meeting.id && citation.kind == "manual_note",
                  let note = meeting.manual_notes?.first(where: { $0.note_id == citation.entity_id }) {
            NavigationLink {
                ScrollView { Text(note.body).textSelection(.enabled).padding(Theme.pageInset) }
                    .background(Theme.background)
                    .navigationTitle("library.note")
            } label: {
                Label(String(localized: "conversation.sourceNote", defaultValue: "Source note"), systemImage: "text.quote")
                    .font(.footnote)
                    .frame(minHeight: 44, alignment: .leading)
            }
        } else if citation.session_id == meeting.id && citation.kind == "title" {
            Text(meeting.session.title).font(.footnote).foregroundStyle(Theme.textSecondary)
        } else {
            Text(String(localized: "conversation.sourceUnavailable", defaultValue: "Source unavailable in this copy."))
                .font(.footnote).foregroundStyle(Theme.textSecondary)
        }
    }

    private func answerState(_ answer: PhoneMeeting.Answer) -> String {
        if answer.provisional == true {
            return String(localized: "conversation.provisional", defaultValue: "Provisional answer")
        }
        switch answer.state {
        case "out_of_date": return String(localized: "conversation.outOfDate", defaultValue: "Out of date")
        case "insufficient_evidence": return String(localized: "conversation.insufficientEvidence", defaultValue: "Not enough evidence")
        case "forgotten": return String(localized: "conversation.answerForgotten", defaultValue: "Answer deleted")
        default: return String(localized: "conversation.answerUnavailable", defaultValue: "Answer unavailable")
        }
    }
}

private struct PhoneConversationEvidenceScreen: View {
    let meeting: PhoneMeeting
    let segmentID: String
    let offsetNanoseconds: UInt64?
    private let turns: [PhoneMeeting.Turn]
    private let earlierRevision: Bool
    @State private var showOriginal = false

    init(meeting: PhoneMeeting, citation: PhoneMeeting.Citation) {
        self.init(meeting: meeting, segmentID: citation.segment_id, offsetNanoseconds: citation.start_offset_ns)
    }

    init(meeting: PhoneMeeting, segmentID: String, offsetNanoseconds: UInt64?) {
        self.meeting = meeting
        self.segmentID = segmentID
        self.offsetNanoseconds = offsetNanoseconds
        let segment = meeting.transcript_segments.first { $0.segment_id == segmentID }
        earlierRevision = segment != nil && segment?.transcript_revision_id != meeting.session.current_transcript_revision_id
        let revision = meeting.turns(revisionID: segment?.transcript_revision_id)
        if let index = revision.firstIndex(where: { $0.id == segmentID }) {
            turns = Array(revision[max(0, index - 2)...min(revision.count - 1, index + 2)])
        } else {
            turns = []
        }
    }

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 16) {
                if let offset = offsetNanoseconds {
                    Text(PhoneMeeting.timestamp(offset))
                        .font(.title3.monospacedDigit())
                }
                if let purged = meeting.transcript_purged_at_utc_ms {
                    Text(String(localized: "conversation.transcriptRemoved", defaultValue: "Transcript deleted"))
                    Text(Date(timeIntervalSince1970: Double(purged) / 1000), style: .date)
                        .font(.footnote).foregroundStyle(Theme.textSecondary)
                } else if turns.isEmpty {
                    Text(String(localized: "conversation.sourceUnavailable", defaultValue: "Source unavailable in this copy."))
                } else {
                    if earlierRevision {
                        Text(String(localized: "conversation.earlierTranscript", defaultValue: "Earlier transcript"))
                            .font(.footnote).foregroundStyle(Theme.textSecondary)
                    }
                    Picker(String(localized: "conversation.wording", defaultValue: "Wording"), selection: $showOriginal) {
                        Text(String(localized: "conversation.corrected", defaultValue: "Corrected")).tag(false)
                        Text(String(localized: "conversation.original", defaultValue: "Original")).tag(true)
                    }
                    .pickerStyle(.segmented)
                    ForEach(turns) { turn in
                        PhoneConversationTurn(turn: turn, showOriginal: showOriginal, highlighted: turn.id == segmentID)
                    }
                    if !earlierRevision, let focus = turns.first(where: { $0.id == segmentID }) {
                        NavigationLink {
                            PhoneConversationScreen(meeting: meeting,
                                initialOffsetNanoseconds: offsetNanoseconds ?? focus.segment.start_offset_ns)
                        } label: {
                            Text(String(localized: "conversation.fullDialogue", defaultValue: "Full dialogue"))
                                .frame(minHeight: 44, alignment: .leading)
                        }
                    }
                }
                Text(String(localized: "conversation.noAudio", defaultValue: "Audio is not included in this copy."))
                    .font(.footnote).foregroundStyle(Theme.textSecondary)
            }
            .foregroundStyle(Theme.textPrimary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Theme.pageInset)
        }
        .background(Theme.background)
        .navigationTitle(String(localized: "conversation.source", defaultValue: "Source"))
        .navigationBarTitleDisplayMode(.inline)
    }
}

private extension PhoneMeeting {
    var conversationKindLabel: String? {
        switch conversation_context?.kind {
        case "huddle": return String(localized: "conversation.huddle", defaultValue: "Huddle")
        case "online_call": return String(localized: "conversation.call", defaultValue: "Call")
        case "in_person": return String(localized: "conversation.inPerson", defaultValue: "In person")
        case "note_session": return String(localized: "conversation.thought", defaultValue: "Thought")
        case "imported": return String(localized: "conversation.imported", defaultValue: "Imported recording")
        default: return nil
        }
    }

    var captureSourceLabel: String? {
        if (conversation_context?.origin ?? session.origin) == "import" {
            return String(localized: "conversation.importedSource", defaultValue: "Imported source")
        }
        let captured = (source_tracks ?? []).filter { $0.first_offset_ns != nil && $0.last_offset_ns != nil }
        let microphone = conversation_context?.captured_microphone ?? captured.contains { $0.source_kind == "microphone" }
        let systemAudio = conversation_context?.captured_system_audio ?? captured.contains { $0.source_kind == "system_audio" }
        switch (microphone, systemAudio) {
        case (true, true): return String(localized: "conversation.microphoneAndSystem", defaultValue: "Microphone and system audio")
        case (true, false): return String(localized: "conversation.microphone", defaultValue: "Microphone")
        case (false, true): return String(localized: "conversation.systemAudio", defaultValue: "System audio")
        case (false, false): return nil
        }
    }

    static func applicationName(_ bundleID: String) -> String {
        switch bundleID {
        case "com.tinyspeck.slackmacgap": return "Slack"
        case "us.zoom.xos": return "Zoom"
        case "com.microsoft.teams", "com.microsoft.teams2": return "Microsoft Teams"
        case "com.apple.FaceTime": return "FaceTime"
        case "com.apple.phone": return "Phone"
        default: return bundleID
        }
    }
}
