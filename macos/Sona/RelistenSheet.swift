import Foundation
import Observation
import SwiftUI

/// One fix the owner made in one line: the words the transcriber wrote, the
/// words he chose, and the line he chose them in.
struct RelistenFix: Equatable {
    let sessionId: MeetingSessionId
    let segmentId: TranscriptSegmentId
    let original: String
    let replacement: String

    /// The one run of at most three words that differs between the line as
    /// its editor showed it and the saved line, without the punctuation both
    /// share. Nothing when the edit only added or removed words, or changed
    /// more than a short run.
    static func between(sessionId: MeetingSessionId, segmentId: TranscriptSegmentId,
                        shown: String, saved: String) -> RelistenFix? {
        let before = shown.split(whereSeparator: \.isWhitespace)
        let after = saved.split(whereSeparator: \.isWhitespace)
        var lead = 0
        while lead < before.count, lead < after.count, before[lead] == after[lead] { lead += 1 }
        var trail = 0
        while trail < before.count - lead, trail < after.count - lead,
              before[before.count - 1 - trail] == after[after.count - 1 - trail] { trail += 1 }
        let removed = before[lead..<(before.count - trail)]
        let added = after[lead..<(after.count - trail)]
        guard (1...3).contains(removed.count), (1...3).contains(added.count) else { return nil }
        var original = Substring(removed.joined(separator: " "))
        var replacement = Substring(added.joined(separator: " "))
        while let last = original.last, last == replacement.last, !last.isWordCharacter {
            original.removeLast()
            replacement.removeLast()
        }
        while let first = original.first, first == replacement.first, !first.isWordCharacter {
            original.removeFirst()
            replacement.removeFirst()
        }
        guard !original.isEmpty, !replacement.isEmpty, original != replacement else { return nil }
        return RelistenFix(sessionId: sessionId, segmentId: segmentId,
                           original: String(original), replacement: String(replacement))
    }

    fileprivate var request: JSONValue {
        .object([
            "session_id": .string(sessionId),
            "segment_id": .string(segmentId),
            "original_phrase": .string(original),
            "replacement_phrase": .string(replacement),
        ])
    }
}

private extension Character {
    var isWordCharacter: Bool { isLetter || isNumber || self == "_" || self == "'" || self == "\u{2019}" }
}

/// A stretch of a line, and whether it is the misheard words.
struct RelistenPiece: Decodable {
    let text: String
    let misheard: Bool
}

/// A line of another meeting that still reads the way a fix corrected.
struct RelistenLine: Decodable, Identifiable {
    let sessionId: MeetingSessionId
    let segmentId: TranscriptSegmentId
    let meetingTitle: String
    let meetingStartedAtUtcMs: Int64
    let appBundleId: String?
    let pieces: [RelistenPiece]
    /// The speaker, only when the owner confirmed who it is.
    let person: SaidPerson?
    let audioRetained: Bool
    let clipStartOffsetNs: UInt64

    var id: TranscriptSegmentId { segmentId }

    fileprivate var target: JSONValue {
        .object(["session_id": .string(sessionId), "segment_id": .string(segmentId)])
    }
}

/// `transcript_relisten_offer`. A store that cannot be read is its own case,
/// never an empty list.
enum RelistenOffer: Decodable {
    case available(fixedAtUtcMs: Int64, total: Int, allOlder: Bool, lines: [RelistenLine])
    case unavailable

    private enum Key: String, CodingKey { case status, fixedAtUtcMs, total, allOlder, lines }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let status = try container.decode(String.self, forKey: .status)
        switch status {
        case "available":
            self = .available(
                fixedAtUtcMs: try container.decode(Int64.self, forKey: .fixedAtUtcMs),
                total: try container.decode(Int.self, forKey: .total),
                allOlder: try container.decode(Bool.self, forKey: .allOlder),
                lines: try container.decode([RelistenLine].self, forKey: .lines))
        case "unavailable": self = .unavailable
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .status, in: container, debugDescription: "unknown relisten offer status \(status)")
        }
    }
}

private struct RelistenResult: Decodable {
    let offer: RelistenOffer
}

/// Offers one fix to the lines of other meetings that still read the old
/// way, and applies the ones the owner accepts.
@MainActor
@Observable
final class RelistenStore {
    private let core: Core
    private(set) var fix: RelistenFix?
    /// What other meetings hold for `fix`. Nothing while it is read, or when
    /// the line no longer carries the fix.
    private(set) var offer: RelistenOffer?
    private(set) var busy = false
    private(set) var error: String?
    private(set) var fixed = 0
    private(set) var skipped = 0
    /// The review sheet is open.
    var reviewing = false
    @ObservationIgnored private var generation = 0

    init(core: Core) { self.core = core }

    func check(_ fix: RelistenFix) {
        self.fix = fix
        fixed = 0
        skipped = 0
        reload()
    }

    func reload() {
        guard let fix else { return }
        offer = nil
        error = nil
        generation += 1
        let requestGeneration = generation
        Task {
            let fresh = await read(fix)
            guard requestGeneration == generation else { return }
            offer = fresh
        }
    }

    func clear() {
        generation += 1
        fix = nil
        offer = nil
        error = nil
        reviewing = false
    }

    func decide(accept: [RelistenLine] = [], skip: [RelistenLine] = []) async {
        guard let fix, !busy else { return }
        busy = true
        error = nil
        defer { busy = false }
        generation += 1
        let requestGeneration = generation
        do {
            let result: RelistenResult = try await core.request("transcript_relisten_decide", [
                "decision": JSONValue.object([
                    "fix": fix.request,
                    "accept": .array(accept.map(\.target)),
                    "skip": .array(skip.map(\.target)),
                ]),
            ])
            guard requestGeneration == generation else { return }
            if case let .available(_, _, _, lines) = result.offer {
                let left = Set(lines.map(\.id))
                fixed += accept.filter { !left.contains($0.id) }.count
                skipped += skip.filter { !left.contains($0.id) }.count
            }
            offer = result.offer
        } catch {
            guard requestGeneration == generation else { return }
            self.error = (error as? CoreError)?.remote(as: MeetingCommandError.self)?.label
                ?? "Those lines could not be changed."
            let fresh = await read(fix)
            guard requestGeneration == generation else { return }
            offer = fresh
        }
    }

    private func read(_ fix: RelistenFix) async -> RelistenOffer? {
        do {
            return try await core.request("transcript_relisten_offer", ["fix": fix.request])
        } catch {
            switch (error as? CoreError)?.remote(as: MeetingCommandError.self) {
            case .notFound, .invalidRequest: return nil
            default: return .unavailable
            }
        }
    }
}

/// The quiet line under a saved fix: how many lines of other meetings still
/// read the old way, and the way in to them.
struct RelistenPrompt: View {
    let store: RelistenStore
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        switch store.offer {
        case let .available(_, total, allOlder, _) where total > 0:
            HStack(spacing: 6) {
                Text("Sounds like this in \(total) \(allOlder ? "older" : "other") \(total == 1 ? "line" : "lines")")
                    .metaText(Theme.inkSecondary)
                    .contentTransition(reduceMotion ? .opacity : .numericText(value: Double(total)))
                    .animation(Motion.settle(reduced: reduceMotion), value: total)
                Text("·").metaText()
                Button("Review") { store.reviewing = true }.buttonStyle(QuietButton(color: Theme.accent))
                Spacer(minLength: 0)
            }
        case .unavailable:
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.circle").foregroundStyle(Theme.inkTertiary)
                    .accessibilityHidden(true)
                Text("Other meetings could not be checked.").metaText()
                Button("Try again") { store.reload() }.buttonStyle(.quiet)
                Spacer(minLength: 0)
            }
        default:
            EmptyView()
        }
    }
}

extension View {
    /// Presents the review sheet whenever `store` asks for it.
    func relistenSheet(_ store: RelistenStore) -> some View {
        sheet(isPresented: Binding(get: { store.reviewing }, set: { store.reviewing = $0 })) {
            RelistenSheet(store: store)
        }
    }
}

/// The lines of other meetings that still read the way the owner corrected,
/// each to hear, accept or skip.
struct RelistenSheet: View {
    let store: RelistenStore
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var playing: TranscriptSegmentId?

    private static let clipSeconds: TimeInterval = 2

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            header
            if playing != nil, let message = audio.error {
                Text(message).metaText(Theme.live)
            }
            if let error = store.error {
                Text(error).metaText(Theme.live)
            }
            content
            footer
        }
        .padding(24)
        .frame(width: 600)
        .background(Theme.page)
        .onChange(of: audio.position) { _, position in
            guard let playing, audio.segmentId == playing, position >= Self.clipSeconds else { return }
            audio.stop()
        }
        .onDisappear {
            if let playing, audio.segmentId == playing { audio.stop() }
        }
    }

    private var audio: ConversationAudioPlayback { model.meetings.audio }

    private var recording: Bool { audio.heldForRecording || model.capture != .idle }

    private var lines: [RelistenLine] {
        if case let .available(_, _, _, lines) = store.offer { lines } else { [] }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let fix = store.fix {
                Text("\u{201C}\(fix.original)\u{201D} \u{2192} \u{201C}\(fix.replacement)\u{201D}").titleText()
            }
            if case let .available(fixedAtUtcMs, total, _, _) = store.offer {
                HStack(spacing: 6) {
                    Text("From your fix on \(PlanningFormat.day(fixedAtUtcMs))")
                    if total > 0 {
                        Text("·")
                        Text("\(total) \(total == 1 ? "line" : "lines")")
                            .contentTransition(reduceMotion ? .opacity : .numericText(value: Double(total)))
                    }
                }
                .animation(Motion.settle(reduced: reduceMotion), value: total)
                .metaText(Theme.inkSecondary)
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch store.offer {
        case let .available(_, total, _, lines) where !lines.isEmpty:
            ScrollView {
                VStack(spacing: 0) {
                    ForEach(lines) { line in
                        row(line).transition(.opacity)
                    }
                }
                .animation(Motion.settle(reduced: reduceMotion), value: lines.map(\.id))
            }
            .frame(maxHeight: 420)
            if total > lines.count {
                Text("Showing the newest \(lines.count).").metaText()
            }
        case .available:
            Text("Nothing left to review.").bodyText(14, Theme.inkSecondary)
        case .unavailable:
            HStack(spacing: 8) {
                Text("Other meetings could not be checked.").bodyText(14, Theme.inkSecondary)
                Button("Try again") { store.reload() }.buttonStyle(.quiet)
            }
        case nil:
            ProgressView().controlSize(.small)
        }
    }

    private var footer: some View {
        HStack(spacing: 12) {
            if store.fixed + store.skipped > 0 {
                Text("\(store.fixed) fixed · \(store.skipped) skipped")
                    .metaText()
                    .contentTransition(reduceMotion ? .opacity : .numericText())
                    .animation(Motion.settle(reduced: reduceMotion), value: store.fixed + store.skipped)
            }
            Spacer(minLength: 0)
            Button("Done") { store.reviewing = false }.buttonStyle(.quiet).keyboardShortcut(.cancelAction)
            if !lines.isEmpty {
                Button("Accept all shown") { Task { await store.decide(accept: lines) } }
                    .buttonStyle(.primary)
                    .disabled(store.busy)
            }
        }
    }

    private func row(_ line: RelistenLine) -> some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 6) {
                    AppMark(bundleId: line.appBundleId, size: 16)
                    if let person = line.person {
                        PersonPortrait(personId: person.personId, name: person.displayName, size: 18,
                                       revision: model.people.photoRevision)
                        Text(person.displayName).metaText(Theme.inkSecondary)
                        Text("·").metaText()
                    }
                    Text(line.meetingTitle).metaText(Theme.inkSecondary).lineLimit(1)
                    Text("·").metaText()
                    Text(PlanningFormat.day(line.meetingStartedAtUtcMs)).metaText()
                }
                Text(sentence(line)).bodyText(14).textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            HStack(spacing: 12) {
                listen(line)
                Button("Skip") { Task { await store.decide(skip: [line]) } }
                    .buttonStyle(.quiet)
                Button("Accept") { Task { await store.decide(accept: [line]) } }
                    .buttonStyle(.compact)
            }
            .disabled(store.busy)
        }
        .padding(.vertical, 12)
        .overlay(alignment: .bottom) { Hairline() }
    }

    private func listen(_ line: RelistenLine) -> some View {
        let active = playing == line.segmentId && audio.segmentId == line.segmentId
            && (audio.playing || audio.loading)
        return Button {
            if active {
                audio.stop()
                playing = nil
            } else {
                playing = line.segmentId
                audio.play(sessionId: line.sessionId, segmentId: line.segmentId,
                           startOffsetNs: line.clipStartOffsetNs)
            }
        } label: {
            Image(systemName: active ? "stop.fill" : "play.fill")
                .font(.system(size: 11, weight: .semibold))
                .frame(width: 22, height: 22)
        }
        .buttonStyle(.quiet)
        .disabled(!line.audioRetained || recording)
        .help(line.audioRetained ? (recording ? "Plays after the recording ends." : "Hear these words")
            : "Recording not retained.")
        .accessibilityLabel(active ? "Stop" : "Hear these words")
    }

    private func sentence(_ line: RelistenLine) -> AttributedString {
        var text = AttributedString()
        for piece in line.pieces {
            if piece.misheard {
                var heard = AttributedString(piece.text)
                heard.foregroundColor = Theme.live
                heard.strikethroughStyle = Text.LineStyle(pattern: .solid, color: Theme.live)
                var fixed = AttributedString(" " + (store.fix?.replacement ?? ""))
                fixed.foregroundColor = Theme.ink
                text += heard + fixed
            } else {
                var plain = AttributedString(piece.text)
                plain.foregroundColor = Theme.inkSecondary
                text += plain
            }
        }
        return text
    }
}
