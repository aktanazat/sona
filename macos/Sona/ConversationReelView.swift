import SwiftUI

/// "From your talks" on a person's page: what was promised, decided and
/// asked in your last talks with them, read from the core when the page
/// opens. No moments, no section. Leaving the page stops its reel.
struct PersonReelSection: View {
    let personId: String
    /// The page's revision: a new one reads the reel again.
    let revision: UInt64
    @Environment(AppModel.self) private var model
    @State private var read = Read.loading

    private enum Read {
        case loading
        case ready(ConversationReel)
        case failed
    }

    private struct ReadKey: Equatable {
        let personId: String
        let revision: UInt64
    }

    private var key: String { "person:\(personId)" }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            switch read {
            case .loading:
                EmptyView()
            case let .ready(reel):
                if !reel.moments.isEmpty {
                    PageSection("From your talks") {
                        ConversationReelView(key: key, moments: reel.moments)
                    }
                }
            case .failed:
                PageSection("From your talks") {
                    Card {
                        CardRow {
                            Text("The recorded moments could not be read.").bodyText(14, Theme.inkSecondary)
                        } trailing: {
                            Button("Retry") { Task { await load() } }.buttonStyle(.compact)
                        }
                    }
                }
            }
        }
        .task(id: ReadKey(personId: personId, revision: revision)) { await load() }
        .onDisappear {
            if model.meetings.audio.reelKey == key { model.meetings.audio.stop() }
        }
    }

    private func load() async {
        do {
            read = .ready(try await model.core.request("conversation_reel_for_person", ["personId": personId]))
        } catch {
            read = .failed
        }
    }
}

/// A reel's moments top to bottom, oldest first, and the one control that
/// plays them in order through the shared player. Space plays or pauses,
/// the up and down arrows move between moments, and return opens the
/// meeting there.
struct ConversationReelView: View {
    /// Which reel this is to the shared player.
    let key: String
    let moments: [ReelMoment]
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @FocusState private var focused: Bool
    /// The moment return opens and Play starts from while the reel is not
    /// playing, back to the first once it stops.
    @State private var selected = 0

    private static let rowPadding: CGFloat = 8
    private static let faceSize: CGFloat = 28
    private static let faceGap: CGFloat = 10
    /// Where a row's words start, so the days between rows line up with them.
    private static let textInset = rowPadding + faceSize + faceGap

    private var audio: ConversationAudioPlayback { model.meetings.audio }
    /// The moment the player is on, while the reel is this one.
    private var current: Int? { audio.reelKey == key ? audio.reelIndex : nil }
    private var playable: Int { moments.filter(\.said.audioRetained).count }

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 10) {
                header
                timeline
                    .padding(.horizontal, -Self.rowPadding)
            }
            .padding(16)
        }
        .overlay {
            RoundedRectangle(cornerRadius: Theme.radiusCard)
                .strokeBorder(focused ? Theme.accent : .clear, lineWidth: 2)
        }
        .focusable(interactions: .activate)
        .focused($focused)
        .onKeyPress(.space) { playOrPause(); return .handled }
        .onKeyPress(.upArrow) { step(forward: false); return .handled }
        .onKeyPress(.downArrow) { step(forward: true); return .handled }
        .onKeyPress(.return) { openMeeting(); return .handled }
        .onChange(of: current) { _, index in
            guard let index else {
                selected = 0
                return
            }
            if moments.indices.contains(index) { selected = index }
        }
        .onChange(of: moments.count) { _, count in selected = min(selected, max(count - 1, 0)) }
    }

    private var header: some View {
        HStack(spacing: 10) {
            if playable > 0 {
                Button(action: playOrPause) {
                    Label(playTitle, systemImage: audio.playing && current != nil ? "pause.fill" : "play.fill")
                }
                .buttonStyle(PrimaryButton(compact: true))
                .disabled(audio.heldForRecording)
            }
            Text(meta).metaText(metaColor)
            if playable > 0, !held {
                Text("·").metaText()
                Button(audio.reelRate == 1 ? "1×" : "1.5×") { audio.toggleReelRate() }
                    .buttonStyle(QuietButton(compact: true))
                    .accessibilityLabel("Speed \(audio.reelRate == 1 ? "normal" : "one and a half times")")
            }
            Spacer(minLength: 0)
        }
    }

    /// One row per moment, with the days between two meetings on a line of
    /// its own.
    private var timeline: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(Array(moments.enumerated()), id: \.element.id) { index, moment in
                if index > 0, let gap = gap(from: moments[index - 1], to: moment) {
                    Text(gap)
                        .metaText()
                        .padding(.leading, Self.textInset)
                        .padding(.vertical, 4)
                        .accessibilityHidden(true)
                }
                row(moment, at: index)
            }
        }
    }

    private func row(_ moment: ReelMoment, at index: Int) -> some View {
        let now = current == index
        return Button { audio.playReel(moments.map(\.clip), key: key, from: index) } label: {
            HStack(alignment: .top, spacing: Self.faceGap) {
                face(moment.said)
                VStack(alignment: .leading, spacing: 4) {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        if let speaker = moment.said.speaker {
                            Text(speaker).bodyText(13).lineLimit(1)
                        }
                        Text("\(moment.said.meetingStarted.short) · \(moment.kind.word)").metaText().lineLimit(1)
                    }
                    Text("“\(moment.said.text)”")
                        .bodyText(13, Theme.inkSecondary)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                    if moment.said.audioRetained {
                        track(index).padding(.top, 2)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(Self.rowPadding)
            .background(now ? Theme.accentSoft : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay {
                RoundedRectangle(cornerRadius: Theme.radiusControl)
                    .strokeBorder(focused && current == nil && selected == index ? Theme.accent : .clear, lineWidth: 1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!moment.said.audioRetained || audio.heldForRecording)
        .animation(Motion.settle(reduced: reduceMotion), value: now)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(spoken(moment))
        .accessibilityValue(moment.said.audioRetained ? (now && audio.playing ? "Playing" : "") : "Audio not kept")
        .accessibilityAddTraits(.isButton)
        .accessibilityAction(named: "Open meeting") { model.open(link: moment.said.link) }
    }

    /// The face of the person the line was confirmed as, and only that.
    @ViewBuilder
    private func face(_ said: SaidMoment) -> some View {
        if let person = said.person {
            PersonPortrait(
                personId: person.personId, name: person.displayName, size: Self.faceSize,
                revision: model.people.photoRevision)
        } else {
            Image(systemName: "waveform")
                .font(.system(size: 12, weight: .regular))
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: Self.faceSize, height: Self.faceSize)
                .background(Theme.inset, in: Circle())
                .accessibilityHidden(true)
        }
    }

    /// How far the reel has got through this moment: full behind the one
    /// playing, filling on it, empty ahead of it.
    private func track(_ index: Int) -> some View {
        let played: Double = if let current {
            index < current ? 1 : index > current ? 0
                : audio.duration > 0 ? min(max(audio.position / audio.duration, 0), 1) : 0
        } else {
            0
        }
        return Capsule()
            .fill(Theme.hairline)
            .overlay(alignment: .leading) {
                GeometryReader { proxy in
                    Capsule().fill(Theme.accent).frame(width: proxy.size.width * played)
                }
            }
            .frame(width: 180, height: 2)
            .animation(reduceMotion ? nil : .linear(duration: 0.1), value: played)
            .accessibilityHidden(true)
    }

    private var held: Bool { audio.heldForRecording }

    private var playTitle: String {
        guard current != nil else { return "Play" }
        if audio.playing { return "Pause" }
        return audio.loading ? "Stop" : "Play"
    }

    /// How many talks and how long they play, how many of them can, or why
    /// nothing plays now.
    private var meta: String {
        if held { return ConversationAudioPlayback.heldNotice }
        if audio.failedReelKey == key, let error = audio.error { return error }
        let talks = Set(moments.map(\.said.sessionId)).count
        let talkCount = talks == 1 ? "1 talk" : "\(talks) talks"
        if playable == 0 { return "\(talkCount) · Audio not kept" }
        if playable < moments.count { return "\(talkCount) · \(playable) of \(moments.count) can play" }
        let total = moments.reduce(UInt64(0)) { $0 + $1.playableNs }
        return "\(talkCount) · \(Int64(clamping: total).meetingOffsetClock)"
    }

    private var metaColor: Color {
        if held { return Theme.inkSecondary }
        guard audio.failedReelKey == key, let error = audio.error else { return Theme.inkTertiary }
        return error == ConversationAudioPlayback.heldNotice ? Theme.inkSecondary : Theme.live
    }

    private func playOrPause() {
        guard playable > 0, !held else { return }
        guard current != nil else {
            audio.playReel(moments.map(\.clip), key: key, from: selected)
            return
        }
        if audio.loading { audio.stop() } else { audio.toggle() }
    }

    private func step(forward: Bool) {
        if current != nil {
            audio.stepReel(forward: forward)
        } else {
            selected = min(max(selected + (forward ? 1 : -1), 0), moments.count - 1)
        }
    }

    private func openMeeting() {
        let index = current ?? selected
        guard moments.indices.contains(index) else { return }
        model.open(link: moments[index].said.link)
    }

    /// "12 days later", between moments a day or more apart.
    private func gap(from earlier: ReelMoment, to later: ReelMoment) -> String? {
        let calendar = Calendar.current
        let days = calendar.dateComponents(
            [.day],
            from: calendar.startOfDay(for: earlier.said.meetingStarted),
            to: calendar.startOfDay(for: later.said.meetingStarted)
        ).day ?? 0
        guard days > 0 else { return nil }
        return days == 1 ? "1 day later" : "\(days) days later"
    }

    /// "Ada, 2 Oct, promised: …".
    private func spoken(_ moment: ReelMoment) -> String {
        let words = "\(moment.kind.word.lowercased()): \(moment.said.text)"
        return [moment.said.speaker, moment.said.meetingStarted.short, words].compactMap { $0 }.joined(separator: ", ")
    }
}
