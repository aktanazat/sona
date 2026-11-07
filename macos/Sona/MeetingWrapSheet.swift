import SwiftUI

/// `MeetingWrap`: who was there, where, the plans heard and the promises
/// still open, as the meeting holds them now.
struct MeetingWrap: Decodable {
    let people: [MeetingWrapPerson]
    let kind: ConversationKind
    let place: MeetingWrapPlace?
    let plans: [HeardPlan]
    let mine: [MeetingLoopRow]
    let owed: [MeetingLoopRow]

    var setting: String? {
        var parts: [String] = []
        if kind != .unspecified { parts.append(kind.label) }
        if let place { parts.append(place.fromInvite ? "\(place.name), from the invite" : place.name) }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

struct MeetingWrapPerson: Decodable, Identifiable {
    let personId: MeetingPersonId
    let displayName: String
    let firstHeard: ArtifactCitation?
    var id: MeetingPersonId { personId }
}

struct MeetingWrapPlace: Decodable {
    let name: String
    let fromInvite: Bool
}

/// After a meeting: who was there, where, the plans heard, the promises
/// each way with the line that made them, and the follow-up. Every draft
/// waits for a tap, and nothing here sends.
struct MeetingWrapSheet: View {
    let core: Core
    let store: MeetingsStore
    let sessionId: MeetingSessionId
    let title: String
    let revision: Int
    @State private var wrap: MeetingWrap?
    @State private var plans: [HeardPlan] = []
    @State private var failure: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(title).titleText().accessibilityAddTraits(.isHeader)
                    if let setting = wrap?.setting { Text(setting).metaText(Theme.inkSecondary) }
                }
                Spacer(minLength: 20)
                Button("Close") { store.closeWrap() }.buttonStyle(.secondary)
            }
            .padding(24)
            Hairline()
            ErrorNote(store.error).padding(.horizontal, 24)
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    if let failure { Text(failure).metaText(Theme.live) }
                    if let wrap { recap(wrap) }
                    followUp
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(24)
            }
            Hairline()
            VStack(spacing: 12) {
                ConversationAudioBar(audio: store.audio)
                HStack(spacing: 12) {
                    Spacer(minLength: 0)
                    Button("Copy for chat") { store.copyFollowUp() }
                        .buttonStyle(.secondary)
                        .disabled(store.followUp?.body.isEmpty != false || store.pending != nil)
                    Button(store.pending == "Opening Mail" ? "Opening…" : "Open in Mail") { store.mailFollowUp() }
                        .buttonStyle(.primary)
                        .disabled(store.followUp?.body.isEmpty != false || store.pending != nil)
                }
            }
            .padding(24)
        }
        .frame(width: 600, height: 640)
        .background(Theme.page)
        .task(id: "\(sessionId):\(revision):\(store.heardPlansRevision)") { await load() }
    }

    @ViewBuilder
    private func recap(_ wrap: MeetingWrap) -> some View {
        if !wrap.people.isEmpty {
            section("Who was there") {
                ForEach(wrap.people) { person in
                    HStack(spacing: 8) {
                        Text(person.displayName).bodyText(14)
                        Spacer(minLength: 8)
                        if let segmentId = person.firstHeard?.segmentId {
                            PlaySaidButton(store: store, segmentId: segmentId)
                        }
                    }
                }
            }
        }
        if !plans.isEmpty {
            HeardPlanList(core: core, store: store, plans: $plans)
        }
        if !wrap.mine.isEmpty {
            section(MeetingLoopDirection.mine.label) { promises(wrap.mine) }
        }
        if !wrap.owed.isEmpty {
            section(MeetingLoopDirection.waitingOn.label) { promises(wrap.owed) }
        }
    }

    private func promises(_ rows: [MeetingLoopRow]) -> some View {
        ForEach(rows) { row in
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(row.text).bodyText(14)
                    Spacer(minLength: 8)
                    if let segmentId = row.citations.first?.segmentId {
                        PlaySaidButton(store: store, segmentId: segmentId)
                    }
                }
                if row.direction == .waitingOn, let owner = row.owner ?? row.speaker {
                    Text(owner).metaText()
                }
                if let quote = row.quote {
                    Text("“\(quote)”").bodyText(13, Theme.inkSecondary).lineLimit(3)
                }
            }
        }
    }

    private var followUp: some View {
        section("Follow-up") {
            if store.drafting {
                ProgressView("Writing the draft…")
            } else if let draft = store.followUp {
                Text(draft.body.isEmpty ? "No follow-up is needed for this meeting." : draft.body)
                    .bodyText()
                    .textSelection(.enabled)
                Text(draft.source.label).metaText(Theme.inkSecondary)
                Text(draft.mailContextStatus).metaText(Theme.inkSecondary)
            } else {
                Button("Write the follow-up") { store.draftFollowUp() }
                    .buttonStyle(.compact)
            }
        }
    }

    private func section<Content: View>(_ label: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(label).sectionLabel()
            content()
        }
    }

    private func load() async {
        do {
            let result: MeetingWrap = try await core.request("meeting_wrap", ["sessionId": sessionId])
            guard !Task.isCancelled else { return }
            wrap = result
            plans = result.plans
            failure = nil
        } catch {
            guard !Task.isCancelled else { return }
            failure = "The wrap could not be read."
        }
    }
}
