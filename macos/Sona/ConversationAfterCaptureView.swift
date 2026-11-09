import Foundation
import SwiftUI

struct ConversationAfterCaptureView: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot
    let core: Core
    @State private var takeaway = ""
    @State private var saving = false
    @State private var saved = false
    @State private var failure: String?
    @State private var showingAllWords = false

    private var uncertain: [TranscriptEffectiveSegment] {
        snapshot.transcript.filter {
            !$0.removed && !$0.edited && $0.base.confidenceMilli.map { $0 < 700 } == true
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if !store.transcriptDeleted {
                MeetingVoiceIdentitySection(sessionId: snapshot.session.sessionId)
                RecordingTailSection(store: store, snapshot: snapshot, core: core)
                if !uncertain.isEmpty {
                    PageSection("Check words") {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(showingAllWords ? uncertain : Array(uncertain.prefix(4))) { segment in
                                HStack(alignment: .firstTextBaseline, spacing: 12) {
                                    Button(segment.base.startOffsetNs.meetingOffsetClock) {
                                        store.playContribution(segment.base.segmentId)
                                    }
                                    .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                                    .disabled(!store.canPlay(segment.base.segmentId))
                                    Text(segment.text).bodyText(14).lineLimit(3)
                                    Spacer(minLength: 0)
                                    Button("Review") { store.jumpTo(segment.base.segmentId) }
                                        .buttonStyle(.quiet)
                                }
                            }
                            if uncertain.count > 4 {
                                Button(showingAllWords ? "Show less" : "Show all \(uncertain.count)") {
                                    showingAllWords.toggle()
                                }
                                .buttonStyle(.quiet)
                            }
                        }
                    }
                }
                TranscriptCorrectionsView(core: core, sessionId: snapshot.session.sessionId,
                    onListenSource: { store.playContribution(sessionId: $0, segmentId: $1) })
                UntrackedActionsSection(store: store)
            }
            PageSection("Takeaway") {
                VStack(alignment: .leading, spacing: 12) {
                    if let summary = snapshot.readableArtifact?.content?.summary, !summary.text.isEmpty {
                        DisclosureGroup("Suggested summary") {
                            VStack(alignment: .leading, spacing: 8) {
                                if snapshot.readableArtifact?.state == .outOfDate {
                                    Text("Summary predates transcript changes.").metaText()
                                }
                                Text(summary.text).bodyText(14, Theme.inkSecondary).textSelection(.enabled)
                                ForEach(summary.citations, id: \.self) { citation in
                                    Button(citation.startOffsetNs.meetingOffsetClock) { store.jumpTo(citation.segmentId) }
                                        .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                                        .disabled(!store.canJumpTo(citation.segmentId))
                                }
                            }
                            .padding(.top, 8)
                        }
                        .font(TypeScale.body(13)).foregroundStyle(Theme.inkSecondary)
                    }
                    TextField("What is worth keeping?", text: $takeaway, axis: .vertical)
                        .lineLimit(2...6).textFieldStyle(.roundedBorder).font(TypeScale.body(14))
                        .accessibilityLabel("Your takeaway")
                        .disabled(!store.editable || saving)
                    HStack(spacing: 12) {
                        Button(saved ? "Saved" : saving ? "Saving…" : "Save takeaway") {
                            Task { await save() }
                        }
                        .buttonStyle(.compact)
                        .disabled(!store.editable || saving || saved || takeaway.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                        if let failure { Text(failure).metaText(Theme.live) }
                    }
                }
            }
        }
        .onChange(of: takeaway) { _, _ in saved = false }
        .onChange(of: snapshot.session.sessionId) { _, _ in
            takeaway = ""
            saved = false
            failure = nil
            showingAllWords = false
        }
    }

    private func save() async {
        let body = takeaway.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !body.isEmpty, !saving else { return }
        saving = true
        failure = nil
        defer { saving = false }
        do {
            let result: MeetingMutationResult = try await core.request("meeting_note_create",
                MeetingRequest.noteCreate(snapshot.session.sessionId, revision: snapshot.session.revision,
                    startOffsetNs: nil, body: body))
            guard result.receipt.result == .committed else {
                failure = result.receipt.refusal
                await store.refreshSnapshot()
                return
            }
            saved = true
            await store.refreshSnapshot()
        } catch {
            failure = (error as? CoreError)?.remote(as: MeetingCommandError.self)?.label ?? "Takeaway could not be saved."
        }
    }
}
