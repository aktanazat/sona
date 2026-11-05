import SwiftUI

/// Everything one generation of the model wrote, in the order a reader scans
/// it. When nothing was, the status line under the title already says why
/// and offers the retry.
struct ArtifactPane: View {
    let store: MeetingsStore
    let snapshot: MeetingReviewSnapshot
    /// For naming the person's own template a generation was written with.
    let templates: MeetingTemplatesStore

    var body: some View {
        if let artifact = snapshot.readableArtifact, let content = artifact.content {
            let notes = MeetingNotesDigest(
                content, transcript: snapshot.transcript, speakerNames: snapshot.speakerNames)
            VStack(alignment: .leading, spacing: 28) {
                if artifact.state == .outOfDate {
                    StaleNotesBand(store: store, what: "These notes")
                }
                if content.plain == true {
                    PlainNotesBand(stoppedBy: content.ledgerFailure)
                }
                if let leftOut = content.leftOut, !leftOut.isEmpty {
                    LeftOutBand(spans: leftOut)
                }
                if let outcome = notes.outcome {
                    section("Outcome") {
                        NoteLine(store: store, line: outcome, bulleted: false) {
                            Text(outcome.text)
                                .font(.system(size: 17, weight: .medium))
                                .foregroundStyle(Theme.ink)
                                .lineSpacing(3)
                        }
                    }
                }
                if !notes.decisions.isEmpty { lines("Decisions", notes.decisions) }
                if !notes.actions.isEmpty { actions(notes.actions, artifactId: artifact.artifactId) }
                if !notes.figures.isEmpty { figures(notes.figures) }
                if !notes.questions.isEmpty { lines("Open questions", notes.questions) }
                if !notes.risks.isEmpty { lines("Risks", notes.risks) }
                if !notes.topics.isEmpty { topics(notes.topics) }
                if !notes.covered.isEmpty { lines("What was covered", notes.covered, counted: false) }
                followUp(content)
                written(artifact)
            }
            .task { await templates.loadIfNeeded() }
        }
    }

    /// A heading of the notes, how much it heads, and what it heads.
    private func section<Content: View>(
        _ title: String, count: String? = nil, @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(title).titleText()
                if let count {
                    Text(count)
                        .font(TypeScale.body(13))
                        .monospacedDigit()
                        .foregroundStyle(Theme.inkTertiary)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
            content()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func lines(
        _ title: String, _ lines: [MeetingNotesDigest.Line], counted: Bool = true
    ) -> some View {
        section(title, count: counted ? "\(lines.count)" : nil) {
            ForEach(lines) { line in
                NoteLine(store: store, line: line) { Text(line.text).bodyText() }
            }
        }
    }

    private func actions(
        _ actions: [MeetingNotesDigest.Action], artifactId: MeetingArtifactId
    ) -> some View {
        let done = actions.filter { store.isDone(artifactId, $0.id) }.count
        return section("Action items", count: "\(done) of \(actions.count) done") {
            ForEach(actions) { action in
                ActionItemRow(store: store, artifactId: artifactId, action: action)
            }
        }
    }

    private func figures(_ figures: [MeetingNotesDigest.Figure]) -> some View {
        section("Key numbers", count: "\(figures.count)") {
            LazyVGrid(
                columns: [GridItem(.adaptive(minimum: 176), spacing: 10, alignment: .topLeading)],
                alignment: .leading,
                spacing: 10
            ) {
                ForEach(figures) { figure in
                    FigureTile(store: store, figure: figure)
                }
            }
        }
    }

    private func topics(_ topics: [MeetingNotesDigest.Topic]) -> some View {
        section("What was covered") {
            VStack(alignment: .leading, spacing: 14) {
                ForEach(topics) { topic in
                    VStack(alignment: .leading, spacing: 2) {
                        NoteLine(store: store, line: topic.line, bulleted: false) {
                            Text(topic.line.text)
                                .font(TypeScale.headline)
                                .foregroundStyle(Theme.ink)
                        }
                        ForEach(Array(topic.points.enumerated()), id: \.offset) { _, point in
                            HStack(alignment: .firstTextBaseline, spacing: 8) {
                                Text("•").bodyText(14, Theme.inkTertiary).accessibilityHidden(true)
                                Text(point).bodyText(14, Theme.inkSecondary)
                            }
                            .padding(.vertical, 2)
                        }
                    }
                }
            }
        }
    }

    private func followUp(_ content: MeetingGeneratedArtifacts) -> some View {
        section("Follow up") {
            Text(content.followUpDraft.text.isEmpty
                ? "The model wrote no follow-up for this meeting."
                : content.followUpDraft.text)
                .bodyText(14, content.followUpDraft.text.isEmpty ? Theme.inkSecondary : Theme.ink)
            Button("Draft follow-up") { store.openFollowUp() }
                .buttonStyle(.compact)
                .padding(.top, 4)
        }
    }

    private func written(_ artifact: MeetingArtifactRevision) -> some View {
        let date = artifact.generatedAtUtcMs.meetingDate
        let template = artifact.customTemplateId.flatMap { templates.name($0) }
        return HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text("Written \(date.short) at \(date.time)" + (template.map { " · \($0)" } ?? ""))
                .metaText()
            if store.canRegenerate {
                Button("Write it again") { store.regenerate() }
                    .buttonStyle(.quiet)
                    .disabled(store.busy)
            }
        }
    }
}

/// Notes the engine would only write as plain text: no sections, nothing
/// to press through to the transcript, and the reason.
private struct PlainNotesBand: View {
    let stoppedBy: MeetingEngineFailureCause?

    var body: some View {
        Card {
            CardRow {
                VStack(alignment: .leading, spacing: 4) {
                    Text(stoppedBy.map { "Plain notes, written because \($0.label)." } ?? "Plain notes.")
                        .bodyText()
                    Text("They have no decisions, action items or links to the transcript.")
                        .metaText(Theme.inkSecondary)
                }
            }
        }
    }
}

/// Stretches of the meeting the notes could not read, so they were written
/// from the rest: when each one is, and why.
private struct LeftOutBand: View {
    let spans: [MeetingLeftOutSpan]

    var body: some View {
        Card {
            CardRow {
                VStack(alignment: .leading, spacing: 4) {
                    Text(spans.count == 1
                        ? "One part of the meeting is not in these notes."
                        : "\(spans.count) parts of the meeting are not in these notes.")
                        .bodyText()
                    ForEach(spans.indices, id: \.self) { index in
                        Text(spans[index].line)
                            .metaText(Theme.inkSecondary)
                    }
                }
            }
        }
    }
}

/// One line of the notes: the words, who was talking, and when. The words
/// lead to the transcript; the time plays the recording from there.
private struct NoteLine<Content: View>: View {
    let store: MeetingsStore
    let line: MeetingNotesDigest.Line
    var bulleted = true
    @ViewBuilder let content: Content

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            if let action = line.moment.flatMap({ store.citationAction($0.citation.segmentId) }) {
                Button(action: action) { words }
                    .buttonStyle(CitedLineStyle())
            } else {
                words
            }
            if let speaker = line.speaker {
                Text(speaker).metaText().lineLimit(1).fixedSize()
            }
            if let moment = line.moment {
                TimeLink(store: store, moment: moment)
            }
        }
    }

    private var words: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            if bulleted {
                Text("•").bodyText(14, Theme.inkTertiary).accessibilityHidden(true)
            }
            content
        }
        .padding(.vertical, 4)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// An action item: its box, its words, and who owns it by when, when the
/// cited words say so.
private struct ActionItemRow: View {
    let store: MeetingsStore
    let artifactId: MeetingArtifactId
    let action: MeetingNotesDigest.Action
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let done = store.isDone(artifactId, action.id)
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Button {
                store.toggleActionItem(artifactId, action.id, done: !done)
            } label: {
                Image(systemName: done ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 15))
                    .foregroundStyle(done ? Theme.accent : Theme.inkTertiary)
                    .contentTransition(.symbolEffect(.replace))
            }
            .buttonStyle(.plain)
            .accessibilityLabel(action.line.text)
            .accessibilityValue(done ? "Done" : "Not done")
            .accessibilityAddTraits(.isToggle)
            VStack(alignment: .leading, spacing: 2) {
                NoteLine(store: store, line: action.line, bulleted: false) {
                    Text(action.line.text)
                        .bodyText(14, done ? Theme.inkTertiary : Theme.ink)
                        .strikethrough(done, color: Theme.inkTertiary)
                }
                if action.owner != nil || action.due != nil {
                    HStack(spacing: 6) {
                        if let owner = action.owner { Chip(owner) }
                        if let due = action.due { Chip("Due \(due)") }
                    }
                    .padding(.bottom, 4)
                }
            }
        }
        .animation(reduceMotion ? nil : .snappy(duration: 0.2), value: done)
    }
}

/// A figure the notes state, large, over the sentence that states it.
private struct FigureTile: View {
    let store: MeetingsStore
    let figure: MeetingNotesDigest.Figure

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusControl)
        VStack(alignment: .leading, spacing: 6) {
            Text(figure.value)
                .font(TypeScale.stat)
                .monospacedDigit()
                .foregroundStyle(Theme.ink)
                .lineLimit(1)
                .minimumScaleFactor(0.7)
            Text(figure.line.text)
                .metaText(Theme.inkSecondary)
                .lineLimit(2)
                .help(figure.line.text)
            Spacer(minLength: 0)
            HStack(spacing: 8) {
                if let speaker = figure.line.speaker {
                    Text(speaker).metaText().lineLimit(1)
                }
                Spacer(minLength: 0)
                if let moment = figure.line.moment {
                    TimeLink(store: store, moment: moment)
                }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, minHeight: 112, alignment: .topLeading)
        .background(Theme.surface, in: shape)
        .overlay(shape.strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// When a line was said. Plays the recording from there when it was kept;
/// otherwise shows the words at that moment.
private struct TimeLink: View {
    let store: MeetingsStore
    let moment: MeetingNotesDigest.Moment

    var body: some View {
        let segmentId = moment.citation.segmentId
        let playable = store.canPlay(segmentId)
        let words = playable ? "Play from \(moment.clock)" : "Show the words at \(moment.clock)"
        Button {
            if playable {
                store.playContribution(segmentId)
            } else {
                store.jumpTo(segmentId)
            }
        } label: {
            HStack(spacing: 4) {
                if playable {
                    Image(systemName: "play.fill").font(.system(size: 8, weight: .bold))
                }
                Text(moment.clock).font(TypeScale.body(12)).monospacedDigit()
            }
        }
        .buttonStyle(TimeLinkStyle())
        .disabled(!playable && !store.canJumpTo(segmentId))
        .help(words)
        .accessibilityLabel(words)
    }
}

private struct TimeLinkStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusKey)
        let lit = isEnabled && (hovering || configuration.isPressed)
        return configuration.label
            .foregroundStyle(lit ? Theme.ink : Theme.inkSecondary)
            .padding(.horizontal, 6)
            .frame(height: 20)
            .background(configuration.isPressed ? Theme.selection : lit ? Theme.control : Theme.wash, in: shape)
            .contentShape(shape)
            .onHover { hovering = $0 }
            .opacity(isEnabled ? 1 : 0.5)
            .fixedSize()
    }
}

/// The wash spills past the line's text, so the text stays in line with the
/// heading above it.
private struct CitedLineStyle: ButtonStyle {
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .padding(.horizontal, 8)
            .background(
                configuration.isPressed || hovering ? Theme.wash : .clear,
                in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .contentShape(Rectangle())
            .onHover { hovering = $0 }
            .padding(.horizontal, -8)
    }
}
