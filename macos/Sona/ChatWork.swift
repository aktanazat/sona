import SwiftUI

/// The activity that belongs below a turn's question, as one quiet line.
///
/// While the turn runs the line is a turning ring, what the turn is on, and
/// how long it has been working, with two lines of light under it where the
/// answer will go, as Halcyon's working block has. Once it ends the line is a
/// closed ring and how long it worked. The time is the disclosure, as in
/// Aside and Halcyon: the steps, and whether the corpus was read, fold under
/// it. What the reader can act on stays out of the fold: a turn still
/// waiting offers Cancel, and a failure offers Retry.
struct ChatWorkRow: View {
    let store: ChatStore
    let turn: AgentPanelTurnStatus
    /// The question a retry would ask again, when the failure belongs to the
    /// row above this one.
    let retryMessage: String?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if turn.isRunning {
                /* One clock for the row, anchored at the instant the core
                 * accepted the turn. Every entry is a whole number of seconds
                 * after it, so the count moves once a second on the turn's
                 * own beat, whatever the status reads are doing. */
                TimelineView(.periodic(from: turn.startedAt, by: 1)) { context in
                    lines(context.date)
                }
            } else {
                // A finished turn's numbers are fixed by the core.
                lines(.now)
            }
            if let failure = turn.failure {
                HStack(spacing: 8) {
                    Text(failure.message).metaText(Theme.live)
                    if let retryMessage {
                        Button("Retry") { store.retry(retryMessage) }
                            .buttonStyle(QuietButton(color: Theme.live))
                            .disabled(store.busy)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        // The fold closes with the turn: a finished turn is one line above its answer.
        .onChange(of: turn.isRunning) { _, running in
            if !running { show(false) }
        }
    }

    /// Whether anything is folded under the time.
    private var folds: Bool { store.searchedCorpus || !turn.steps.isEmpty }

    /// A running turn with no answer yet: the work is the last thing in the
    /// thread, so the lines standing in for the answer go under it.
    private var awaitingAnswer: Bool { turn.isRunning && store.workRowIndex == store.rows.count }

    /// Whether the answer was built on the corpus, said once for both places
    /// it can stand.
    private static let corpus = "Looked through your meetings and notes"

    /// The line and what hangs off it, as they read at one instant.
    private func lines(_ now: Date) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                if turn.isRunning {
                    ChatWorkingLine(text: turn.headline)
                    Text("·").metaText().accessibilityHidden(true)
                }
                handle(now)
            }
            if open, folds {
                steps(now).modifier(ChatEntrance())
            }
            if awaitingAnswer {
                ChatSkeleton()
            }
            if turn.isStillWaiting(now) {
                HStack(spacing: 8) {
                    Text("Still waiting…").metaText(Theme.inkSecondary)
                    Button("Cancel") { store.stop() }
                        .buttonStyle(.quiet)
                        .disabled(store.stopping)
                }
            }
        }
    }

    /// The time, and the fold's handle when anything is under it.
    @ViewBuilder
    private func handle(_ now: Date) -> some View {
        if let elapsed = turn.elapsed(now) {
            let spoken = turn.spokenElapsed(now) ?? elapsed
            if folds {
                Button { show(!open) } label: {
                    stamp(elapsed, chevron: true)
                }
                .buttonStyle(.plain)
                .help(open ? "Hide what Sona did" : "Show what Sona did")
                .accessibilityLabel(spoken)
                .accessibilityValue(open ? "Steps shown" : "Steps hidden")
            } else {
                stamp(elapsed, chevron: false)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(spoken)
            }
        } else if store.searchedCorpus {
            /* A finished turn the core never timed has no line to fold
             * under, so the one thing worth saying stands on its own. */
            Text(Self.corpus).metaText()
        }
    }

    /// "Working for 12s", or a finished turn's closed ring and "Worked for
    /// 1m 4s", so the whole finished line is the handle, as in Halcyon.
    private func stamp(_ elapsed: String, chevron: Bool) -> some View {
        HStack(spacing: 6) {
            if !turn.isRunning {
                ChatProgressRing(phase: nil, mark: turn.state == .succeeded ? "checkmark" : "minus")
                    .padding(.trailing, 2)
            }
            Text(elapsed).metaText().monospacedDigit()
            if chevron {
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.inkTertiary)
                    .rotationEffect(.degrees(open ? 90 : 0))
            }
        }
        .contentShape(Rectangle())
    }

    /// What the turn did on the way, as Halcyon's opened fold: whether it
    /// read the corpus, then each step, each with a mark for how it went in
    /// the ring's column and how long it took.
    private func steps(_ now: Date) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            if store.searchedCorpus {
                ChatStepRow(state: nil, title: Self.corpus, isTool: false, time: nil)
            }
            ForEach(turn.steps) { step in
                ChatStepRow(
                    state: step.state,
                    title: step.title,
                    isTool: step.isTool,
                    time: AgentPanelTurnStatus.duration(turn.workedMs(step, now)))
            }
        }
    }

    /// The fold moves on the thread's one curve, and not at all under Reduce
    /// Motion.
    private func show(_ value: Bool) {
        withAnimation(ChatMotion.arrival(reduceMotion)) { open = value }
    }
}

/// One line in the opened fold: a mark for how the step went, sitting in the
/// ring's column, its words, and how long it took.
private struct ChatStepRow: View {
    /// Nil for the corpus line, which is a fact rather than a step.
    let state: AgentPanelStepState?
    let title: String
    /// A tool's name is a machine's name, so it reads as one, in mono.
    let isTool: Bool
    let time: String?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: symbol)
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(failed ? Theme.live : Theme.inkTertiary)
                .frame(width: 16)
                .accessibilityHidden(true)
            Text(title)
                .font(isTool ? TypeScale.mono(12) : TypeScale.body(14))
                .foregroundStyle(failed ? Theme.live : Theme.inkSecondary)
                .lineLimit(1)
            Spacer(minLength: 8)
            if let time {
                Text(time).metaText().monospacedDigit()
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel([title, spoken, time].compactMap { $0 }.joined(separator: ", "))
    }

    private var failed: Bool { state == .failed }

    private var symbol: String {
        switch state {
        case nil: "text.magnifyingglass"
        case .running: "circle.dotted"
        case .done: "checkmark"
        case .failed: "minus"
        }
    }

    private var spoken: String? {
        switch state {
        case nil: nil
        case .running: "in progress"
        case .done: "done"
        case .failed: "failed"
        }
    }
}

/// A live turn's ring and the line it is on, drawn the way Aside draws a
/// model at work: the ring goes round and a band of full ink sweeps across
/// quieter words every two seconds, both from one frame clock, so the row
/// reads as alive while nothing on it jumps. Reduce Motion stops the clock:
/// the arc rests at twelve and the words stay still.
private struct ChatWorkingLine: View {
    let text: String
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 60, paused: reduceMotion)) { context in
            let time = reduceMotion ? 0 : context.date.timeIntervalSinceReferenceDate
            HStack(spacing: 8) {
                ChatProgressRing(phase: time.truncatingRemainder(dividingBy: Self.spin) / Self.spin)
                Text(text)
                    .metaText(reduceMotion ? Theme.inkSecondary : Theme.inkTertiary)
                    .lineLimit(1)
                    .overlay {
                        if !reduceMotion {
                            Text(text)
                                .metaText(Theme.ink)
                                .lineLimit(1)
                                .mask { Self.band(Self.pass(time)) }
                                .accessibilityHidden(true)
                        }
                    }
            }
        }
    }

    /// The band of full ink, `phase` of the way through its pass: from wholly
    /// before the words to wholly past them.
    private static func band(_ phase: Double) -> some View {
        GeometryReader { proxy in
            LinearGradient(colors: [.clear, .black, .clear], startPoint: .leading, endPoint: .trailing)
                .frame(width: proxy.size.width * 0.6)
                .offset(x: proxy.size.width * CGFloat(phase * 1.6 - 0.6))
        }
    }

    /// How far through its pass the light is at `time`, from 0 to 1. The
    /// skeleton reads the same beat, so the headline and the lines under it
    /// shine together.
    fileprivate static func pass(_ time: TimeInterval) -> Double {
        time.truncatingRemainder(dividingBy: sweep) / sweep
    }

    /// One turn of the ring, in seconds.
    private static let spin: TimeInterval = 1
    /// One pass of the band, in seconds: Aside's two.
    private static let sweep: TimeInterval = 2
}

/// Two lines standing in for the words on their way, as in Halcyon, with the
/// working line's light passing along them on the same beat. With Reduce
/// Motion the light stays off and the lines simply wait.
private struct ChatSkeleton: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 60, paused: reduceMotion)) { context in
            let centre: Double? = reduceMotion
                ? nil
                : ChatWorkingLine.pass(context.date.timeIntervalSinceReferenceDate) * 1.8 - 0.4
            VStack(alignment: .leading, spacing: 8) {
                line(centre)
                line(centre).padding(.trailing, 84)
            }
        }
        .padding(.top, 2)
        .accessibilityHidden(true)
    }

    private func line(_ centre: Double?) -> some View {
        Capsule()
            .fill(Theme.ink.opacity(0.06))
            .overlay {
                if let centre {
                    Capsule().fill(LinearGradient(
                        colors: [.clear, Theme.ink.opacity(0.12), .clear],
                        startPoint: UnitPoint(x: centre - 0.4, y: 0.5),
                        endPoint: UnitPoint(x: centre + 0.4, y: 0.5)))
                }
            }
            .frame(height: 8)
    }
}

/// Aside's sixteen-point ring: a faint full track and an arc over it, drawn
/// from twelve o'clock. A running turn reports no fraction done, so its arc
/// is a quarter that goes round rather than one that fills. A finished ring
/// is closed and carries a mark, as Halcyon's does, because a closed ring
/// alone reads as an empty checkbox: a check for an answer, a dash for a turn
/// that ended without one.
private struct ChatProgressRing: View {
    /// How far round the arc has gone, from 0 to 1, while the turn runs; nil
    /// once it is over.
    var phase: Double?
    /// The finished ring's symbol.
    var mark: String?

    var body: some View {
        ZStack {
            if let phase {
                Circle().stroke(Theme.ink.opacity(0.1), lineWidth: 2)
                Circle()
                    .trim(from: 0, to: 0.25)
                    .stroke(Theme.ink.opacity(0.7), style: StrokeStyle(lineWidth: 2, lineCap: .round))
                    .rotationEffect(.degrees(phase * 360 - 90))
            } else {
                Circle().stroke(Theme.inkTertiary, lineWidth: 1.5)
                if let mark {
                    Image(systemName: mark)
                        .font(.system(size: 6, weight: .heavy))
                        .foregroundStyle(Theme.inkTertiary)
                }
            }
        }
        .frame(width: 12, height: 12)
        .frame(width: 16, height: 16)
        .accessibilityHidden(true)
    }
}
