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
    @ObservationIgnored private var player: AVAudioPlayer?
    @ObservationIgnored private var clip: ConversationAudioClip?
    @ObservationIgnored private var request: Task<Void, Never>?
    @ObservationIgnored private var ticker: Task<Void, Never>?
    @ObservationIgnored private var generation = 0

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
        error = nil
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
                    guard retained else { stop(); error = "Recording is no longer retained."; return }
                    if player.currentTime >= player.duration { player.currentTime = 0 }
                    resume()
                } catch {
                    guard expected == generation else { return }
                    stop()
                    self.error = "Recording could not be read."
                }
            }
        }
    }

    func seek(_ seconds: TimeInterval) {
        guard seconds.isFinite, let player else { return }
        player.currentTime = min(max(seconds, 0), player.duration)
        position = player.currentTime
    }

    private func load(sessionId: String, segmentId: String?, offset: UInt64?) {
        loading = true
        let expected = generation
        request = Task { [weak self] in
            guard let self else { return }
            do {
                let result: ConversationAudioClip = try await core.request("conversation_audio_clip", [
                    "sessionId": JSONValue.string(sessionId), "segmentId": segmentId.map { .string($0) } ?? .null,
                    "startOffsetNs": offset.map { .number(Double($0)) } ?? .null,
                ])
                guard expected == generation, !Task.isCancelled else { return }
                guard result.sessionId == sessionId, result.segmentId == segmentId,
                      let data = Data(base64Encoded: result.wavBase64) else {
                    throw CocoaError(.fileReadCorruptFile)
                }
                let made = try AVAudioPlayer(data: data)
                made.prepareToPlay()
                let retained: Bool = try await core.request("conversation_audio_retained", ["sessionId": sessionId])
                guard expected == generation, !Task.isCancelled else { return }
                guard retained else { stop(); error = "Recording is no longer retained."; return }
                player = made
                clip = result
                startOffsetNs = result.startOffsetNs
                duration = made.duration
                position = 0
                loading = false
                resume()
            } catch {
                guard expected == generation, !Task.isCancelled else { return }
                stop()
                self.error = (error as? CoreError)?.remote(as: MeetingCommandError.self)?.label
                    ?? "Recording could not be played."
            }
        }
    }

    private func resume() {
        guard let player, player.play() else {
            stop()
            error = "Audio output is unavailable."
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
                        if !retained { stop(); error = "Recording is no longer retained."; return }
                    } catch {
                        guard expected == generation else { return }
                        stop()
                        self.error = "Recording could not be read."
                        return
                    }
                }
                if !player.isPlaying {
                    playing = false
                    position = duration
                    if let next = clip?.nextOffsetNs, let sessionId {
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
            Text(error).metaText(Theme.live).padding(.vertical, 8)
        }
    }
}
