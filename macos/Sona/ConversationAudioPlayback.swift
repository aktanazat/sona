import AVFoundation
import Foundation
import Observation
import SwiftUI

struct ConversationAudioClip: Decodable {
    let sessionId: String
    let segmentId: String?
    let startOffsetNs: UInt64
    let endOffsetNs: UInt64
    let contributionOffsetNs: UInt64
    let nextOffsetNs: UInt64?
    let wavBase64: String
}

/// One stretch a reel plays: a line, and the window to hear it in. An open
/// end plays the line the way its own row does.
struct ReelClip {
    let said: SaidMoment
    var startOffsetNs: UInt64?
    var endOffsetNs: UInt64?
}

@MainActor
@Observable
final class ConversationAudioPlayback {
    private let core: Core
    private(set) var sessionId: String?
    private(set) var segmentId: String?
    private(set) var loading = false
    private(set) var playing = false
    private(set) var position: TimeInterval = 0
    private(set) var duration: TimeInterval = 0
    private(set) var startOffsetNs: UInt64 = 0
    private(set) var error: String?
    /// A meeting is capturing. Sound played now would land in its call track
    /// and its microphone, so nothing plays until the capture ends.
    private(set) var heldForRecording = false
    @ObservationIgnored private var player: AVAudioPlayer?
    @ObservationIgnored private var clip: ConversationAudioClip?
    @ObservationIgnored private var request: Task<Void, Never>?
    @ObservationIgnored private var ticker: Task<Void, Never>?
    @ObservationIgnored private var generation = 0
    /// The reel being heard: what asked for it, its clips in order, and the
    /// one playing now. Empty while a single line plays.
    private(set) var reelKey: String?
    private(set) var reelClips: [ReelClip] = []
    private(set) var reelIndex: Int?
    /// How fast reels play, 1 or 1.5. A single line always plays at 1.
    private(set) var reelRate: Float = 1
    /// The reel that was playing when playback failed, so the place that
    /// started it can say why.
    private(set) var failedReelKey: String?

    static let heldNotice = "Plays after the recording ends."

    init(core: Core) {
        self.core = core
        for event in [CoreEvent.meetingSessionChanged, CoreEvent.meetingTranscriptChanged, CoreEvent.meetingRemoved] {
            core.observe(event) { [weak self] line in
                guard let self, let payload: MeetingEventPayload = try? Core.payload(line),
                      payload.sessionId == self.sessionId else { return }
                self.stop()
            }
        }
    }

    func play(sessionId: String, segmentId: String, startOffsetNs: UInt64? = nil) {
        stop()
        error = nil
        self.sessionId = sessionId
        self.segmentId = segmentId
        load(sessionId: sessionId, segmentId: segmentId, offset: startOffsetNs)
    }

    func playBookmark(sessionId: String, offset: UInt64) {
        stop()
        error = nil
        self.sessionId = sessionId
        let contextStart = offset > 4_000_000_000 ? offset - 4_000_000_000 : 0
        load(sessionId: sessionId, segmentId: nil, offset: contextStart)
    }

    func stop() {
        endClip()
        reelKey = nil
        reelClips = []
        reelIndex = nil
        error = nil
        failedReelKey = nil
    }

    /// Stops everything, a reel included, and says why.
    private func fail(_ reason: String) {
        let key = reelKey
        stop()
        error = reason
        failedReelKey = key
    }

    /// Ends the clip that is loading or playing and leaves any reel in place.
    private func endClip() {
        generation += 1
        request?.cancel()
        request = nil
        ticker?.cancel()
        ticker = nil
        player?.stop()
        player = nil
        clip = nil
        sessionId = nil
        segmentId = nil
        loading = false
        playing = false
        position = 0
        duration = 0
        startOffsetNs = 0
    }

    /// Plays the clips one after another from `index`, passing over any whose
    /// audio is gone, and stops after the last. Nothing kept from `index` on
    /// fails, so the place that asked says so.
    func playReel(_ clips: [ReelClip], key: String, from index: Int = 0) {
        stop()
        reelKey = key
        reelClips = clips
        guard let first = playable(from: index, step: 1) else {
            fail("Audio not kept.")
            return
        }
        playClip(at: first)
    }

    /// Reads a reel, then plays it. The read counts as loading, and a read
    /// that fails says so where the player speaks.
    func playReel(reading command: String, _ arguments: [String: JSONValue], key: String) {
        stop()
        reelKey = key
        loading = true
        let expected = generation
        request = Task { [weak self] in
            guard let self else { return }
            do {
                let reel: ConversationReel = try await core.request(command, arguments)
                guard expected == generation, !Task.isCancelled else { return }
                playReel(reel.moments.map(\.clip), key: key)
            } catch {
                guard expected == generation, !Task.isCancelled else { return }
                fail("The recorded moments could not be read.")
            }
        }
    }

    /// The next playable clip, or the one before. Back from the first starts
    /// it again; there is nothing after the last.
    func stepReel(forward: Bool) {
        guard let reelIndex else { return }
        let target = forward
            ? playable(from: reelIndex + 1, step: 1)
            : playable(from: reelIndex - 1, step: -1) ?? reelIndex
        guard let target else { return }
        playClip(at: target)
    }

    /// Switches reels between 1× and 1.5×, the clip playing now included.
    func toggleReelRate() {
        reelRate = reelRate == 1 ? 1.5 : 1
        if reelKey != nil { player?.rate = reelRate }
    }

    /// The first clip from `index` on, moving by `step`, whose audio is kept.
    private func playable(from index: Int, step: Int) -> Int? {
        var index = index
        while reelClips.indices.contains(index) {
            if reelClips[index].said.audioRetained { return index }
            index += step
        }
        return nil
    }

    /// Plays one clip of the reel; none left ends the reel.
    private func playClip(at index: Int?) {
        guard let index else {
            stop()
            return
        }
        endClip()
        error = nil
        let said = reelClips[index].said
        reelIndex = index
        sessionId = said.sessionId
        segmentId = said.segmentId
        load(sessionId: said.sessionId, segmentId: said.segmentId,
             offset: reelClips[index].startOffsetNs, end: reelClips[index].endOffsetNs)
    }

    /// Holds playback while a meeting captures, stopping what is playing, and
    /// lets it go when the capture ends.
    func hold(_ capturing: Bool) {
        guard capturing != heldForRecording else { return }
        heldForRecording = capturing
        if capturing {
            if sessionId != nil || loading {
                fail(Self.heldNotice)
            }
        } else if error == Self.heldNotice {
            error = nil
        }
    }

    /// Stops and says why when a meeting is capturing.
    private func refusedWhileHeld() -> Bool {
        guard heldForRecording else { return false }
        fail(Self.heldNotice)
        return true
    }

    func toggle() {
        guard let player else { return }
        if playing {
            player.pause()
            playing = false
            ticker?.cancel()
        } else {
            guard let sessionId else { return }
            let expected = generation
            request = Task { [weak self] in
                guard let self else { return }
                do {
                    let retained: Bool = try await core.request("conversation_audio_retained", ["sessionId": sessionId])
                    guard expected == generation, !Task.isCancelled else { return }
                    guard retained else { fail("Recording is no longer retained."); return }
                    if player.currentTime >= player.duration { player.currentTime = 0 }
                    resume()
                } catch {
                    guard expected == generation else { return }
                    fail("Recording could not be read.")
                }
            }
        }
    }

    func seek(_ seconds: TimeInterval) {
        guard seconds.isFinite, let player else { return }
        player.currentTime = min(max(seconds, 0), player.duration)
        position = player.currentTime
    }

    private func load(sessionId: String, segmentId: String?, offset: UInt64?, end: UInt64? = nil) {
        guard !refusedWhileHeld() else { return }
        loading = true
        let expected = generation
        request = Task { [weak self] in
            guard let self else { return }
            do {
                let result: ConversationAudioClip = try await core.request("conversation_audio_clip", [
                    "sessionId": JSONValue.string(sessionId), "segmentId": segmentId.map { .string($0) } ?? .null,
                    "startOffsetNs": offset.map { .number(Double($0)) } ?? .null,
                    "endOffsetNs": end.map { .number(Double($0)) } ?? .null,
                ])
                guard expected == generation, !Task.isCancelled else { return }
                guard result.sessionId == sessionId, result.segmentId == segmentId,
                      let data = Data(base64Encoded: result.wavBase64) else {
                    throw CocoaError(.fileReadCorruptFile)
                }
                let made = try AVAudioPlayer(data: data)
                if reelKey != nil {
                    made.enableRate = true
                    made.rate = reelRate
                }
                made.prepareToPlay()
                let retained: Bool = try await core.request("conversation_audio_retained", ["sessionId": sessionId])
                guard expected == generation, !Task.isCancelled else { return }
                guard retained else { fail("Recording is no longer retained."); return }
                player = made
                clip = result
                startOffsetNs = result.startOffsetNs
                duration = made.duration
                position = 0
                loading = false
                resume()
            } catch {
                guard expected == generation, !Task.isCancelled else { return }
                let refusal = (error as? CoreError)?.remote(as: MeetingCommandError.self)
                if refusal == .notFound {
                    let retained: Bool? = try? await core.request("conversation_audio_retained", ["sessionId": sessionId])
                    guard expected == generation, !Task.isCancelled else { return }
                    if retained == false {
                        fail("Audio not kept.")
                        return
                    }
                }
                fail(refusal == .captureLeaseBusy
                    ? Self.heldNotice : refusal?.label ?? "Recording could not be played.")
            }
        }
    }

    private func resume() {
        guard !refusedWhileHeld() else { return }
        guard let player, player.play() else {
            fail("Audio output is unavailable.")
            return
        }
        playing = true
        ticker?.cancel()
        let expected = generation
        ticker = Task { [weak self] in
            var ticks = 0
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(100))
                guard !Task.isCancelled, let self, expected == generation else { return }
                position = player.currentTime
                ticks += 1
                if ticks.isMultiple(of: 10), let sessionId {
                    do {
                        let retained: Bool = try await core.request("conversation_audio_retained", ["sessionId": sessionId])
                        guard !Task.isCancelled, expected == generation else { return }
                        if !retained { fail("Recording is no longer retained."); return }
                    } catch {
                        guard expected == generation else { return }
                        fail("Recording could not be read.")
                        return
                    }
                }
                if !player.isPlaying {
                    playing = false
                    position = duration
                    if let reelIndex {
                        playClip(at: playable(from: reelIndex + 1, step: 1))
                    } else if let next = clip?.nextOffsetNs, let sessionId {
                        load(sessionId: sessionId, segmentId: segmentId, offset: next)
                    }
                    return
                }
            }
        }
    }
}

struct ConversationAudioBar: View {
    let audio: ConversationAudioPlayback

    var body: some View {
        if audio.loading {
            HStack(spacing: 10) {
                ProgressView().controlSize(.small)
                Text("Reading recording…").metaText()
                Button("Cancel") { audio.stop() }.buttonStyle(.quiet)
            }
            .padding(.vertical, 8)
        } else if audio.sessionId != nil {
            HStack(spacing: 12) {
                Button { audio.toggle() } label: {
                    Image(systemName: audio.playing ? "pause.fill" : "play.fill")
                        .frame(width: 24, height: 24)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(audio.playing ? "Pause recording" : "Play recording")
                Slider(value: Binding(get: { audio.position }, set: { audio.seek($0) }), in: 0...max(audio.duration, 0.001))
                    .accessibilityLabel("Recording position")
                Text((Int64(clamping: audio.startOffsetNs) + Int64(audio.position * 1_000_000_000)).meetingOffsetClock)
                    .monospacedDigit().metaText()
                Button { audio.stop() } label: { Image(systemName: "xmark") }
                    .buttonStyle(.plain).accessibilityLabel("Close playback")
            }
            .padding(.vertical, 8)
        } else if let error = audio.error {
            Text(error).metaText(audio.heldForRecording ? Theme.inkSecondary : Theme.live).padding(.vertical, 8)
        }
    }
}
