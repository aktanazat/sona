import SwiftUI

/// `MeetingRecordingTail`: the end of a recording where only one voice,
/// heard on this Mac's microphone, is left.
struct MeetingRecordingTail: Decodable, Equatable {
    let fromOffsetNs: Int64
    let recordingEndOffsetNs: Int64
    let remainingSpeakerId: SpeakerId
    let lastOtherSegmentId: TranscriptSegmentId
    let segmentIds: [TranscriptSegmentId]
    let cut: Bool

    /// At least two: a shorter tail is not offered.
    var minutes: Int {
        Int((Double(recordingEndOffsetNs - fromOffsetNs) / 60_000_000_000).rounded())
    }
}

/// The recording kept going after the others stopped talking: an offer to
/// take the rest out of the transcript, only on a tap, and a way to put it
/// back.
struct RecordingTailSection: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot
    let core: Core
    @AppStorage("meetings.keptRecordingTails") private var kept = ""
    @State private var tail: MeetingRecordingTail?
    @State private var confirming = false

    private var sessionId: MeetingSessionId { snapshot.session.sessionId }

    var body: some View {
        Group {
            if let tail, store.editable, tail.cut || !isKept {
                PageSection("End of the recording") { content(tail) }
                    .alert("Cut the last \(minutesText(tail)) from the transcript?", isPresented: $confirming) {
                        Button("Cut", role: .destructive) { store.setLinesRemoved(tail.segmentIds, removed: true) }
                        Button("Cancel", role: .cancel) {}
                    } message: {
                        Text("\(linesText(tail)) after \(tail.fromOffsetNs.meetingOffsetClock) leave the transcript, and the notes will need updating. The audio stays. You can put the lines back here.")
                    }
            }
        }
        .task(id: snapshot.session.revision) { await load() }
    }

    @ViewBuilder
    private func content(_ tail: MeetingRecordingTail) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            if tail.cut {
                Text("You cut the last \(minutesText(tail)): \(linesText(tail).lowercased()) after \(tail.fromOffsetNs.meetingOffsetClock) are out of the transcript.")
                    .bodyText(14).fixedSize(horizontal: false, vertical: true)
                Button("Put them back") { store.setLinesRemoved(tail.segmentIds, removed: false) }
                    .buttonStyle(SecondaryButton(compact: true))
                    .disabled(store.busy)
            } else {
                Text("The recording ran \(minutesText(tail)) after \(otherName(tail)) last spoke, at \(tail.fromOffsetNs.meetingOffsetClock). Only \(store.speakerName(tail.remainingSpeakerId)) is heard after that.")
                    .bodyText(14).fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 12) {
                    Button { store.playContribution(tail.lastOtherSegmentId) } label: {
                        Label("Their last words", systemImage: "play.fill")
                    }
                    .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                    .disabled(!store.canPlay(tail.lastOtherSegmentId))
                    if let first = tail.segmentIds.first {
                        Button { store.playContribution(first) } label: {
                            Label("What came after", systemImage: "play.fill")
                        }
                        .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                        .disabled(!store.canPlay(first))
                    }
                    Spacer(minLength: 12)
                    Button("Keep") { kept += " \(sessionId)" }
                        .buttonStyle(.quiet)
                    Button("Cut the last \(minutesText(tail))…") { confirming = true }
                        .buttonStyle(SecondaryButton(compact: true))
                        .disabled(store.busy)
                }
            }
        }
    }

    private var isKept: Bool { kept.split(separator: " ").contains { $0 == sessionId } }

    private func minutesText(_ tail: MeetingRecordingTail) -> String {
        "\(tail.minutes) minutes"
    }

    private func linesText(_ tail: MeetingRecordingTail) -> String {
        tail.segmentIds.count == 1 ? "The 1 line" : "The \(tail.segmentIds.count) lines"
    }

    private func otherName(_ tail: MeetingRecordingTail) -> String {
        let speaker = snapshot.transcript.first { $0.base.segmentId == tail.lastOtherSegmentId }?.assignedSpeakerId
        return store.speakerName(speaker)
    }

    private func load() async {
        do {
            let result: MeetingRecordingTail? = try await core.request(
                "meeting_recording_tail", MeetingRequest.session(sessionId))
            guard !Task.isCancelled else { return }
            tail = result
        } catch {
            guard !Task.isCancelled else { return }
            tail = nil
        }
    }
}
