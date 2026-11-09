import SwiftUI

/// A sentence read as a change to tasks: what happens, to which tasks, and
/// the first few of them by name.
struct PaletteCommandPreview: View {
    let command: WorkCommand
    var shown = 4

    var body: some View {
        VStack(alignment: .leading, spacing: Space.xs) {
            Text(command.summary).bodyText()
            Text(([Self.counted(command.items.count)] + command.filters).joined(separator: " · "))
                .metaText()
                .lineLimit(1)
            VStack(alignment: .leading, spacing: Space.xxs) {
                ForEach(command.items.prefix(shown)) { item in
                    HStack(spacing: Space.s) {
                        TaskStatusIcon(status: item.status, size: 12)
                        Text("S-\(item.number)").metaText().monospacedDigit()
                        Text(item.title).bodyText(13, Theme.inkSecondary).lineLimit(1)
                    }
                }
                if command.items.count > shown {
                    Text("and \(command.items.count - shown) more").metaText()
                }
            }
            .padding(.top, Space.xxs)
        }
        .accessibilityElement(children: .combine)
    }

    static func counted(_ count: Int) -> String {
        count == 1 ? "1 task" : "\(count) tasks"
    }

    /// Who said the line, where, and on which day.
    static func meta(_ line: WorkAnswerLine) -> String {
        [line.speaker, line.title, Date(timeIntervalSince1970: Double(line.whenUtcMs) / 1000).relativeDay]
            .compactMap { $0 }
            .filter { !$0.isEmpty }
            .joined(separator: " · ")
    }
}

/// A watched word and the last line that said it.
struct PaletteWatchSummary: View {
    let watch: WorkWatch

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(watch.personName.map { "“\(watch.phrase)” from \($0)" } ?? "“\(watch.phrase)”").bodyText()
            if let latest = watch.hits.first {
                Text("“\(latest.line.quote)” · \(PaletteCommandPreview.meta(latest.line))")
                    .metaText()
                    .lineLimit(1)
            } else {
                Text("Not heard yet").metaText()
            }
        }
    }
}

/// The words Sona listens for when this person speaks, and a field to add
/// one. A watched word said in a processed meeting sends one notification.
struct PersonWatchSection: View {
    let personId: String
    let name: String
    @Environment(AppModel.self) private var model
    @State private var phrase = ""

    var body: some View {
        let watches = model.work.watches.filter { $0.personId == personId }
        PageSection("Watching") {
            Card {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(watches) { watch in
                        PersonWatchRow(watch: watch)
                        Hairline()
                    }
                    HStack(spacing: Space.m) {
                        Image(systemName: "eye")
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.inkTertiary)
                            .frame(width: 16)
                            .accessibilityHidden(true)
                        TextField(
                            "", text: $phrase,
                            prompt: Text("A word \(name) might say").foregroundStyle(Theme.inkTertiary)
                        )
                        .textFieldStyle(.plain)
                        .font(TypeScale.body(14))
                        .onSubmit(add)
                        .accessibilityLabel("Watch for a word \(name) says")
                    }
                    .padding(.horizontal, 16)
                    .frame(height: 40)
                }
            }
        }
    }

    private func add() {
        model.work.watch(phrase, personId: personId, personName: name)
        phrase = ""
    }
}

private struct PersonWatchRow: View {
    let watch: WorkWatch
    @Environment(AppModel.self) private var model
    @State private var hovering = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.m) {
            Text("“\(watch.phrase)”").bodyText().frame(minWidth: 80, alignment: .leading)
            if let latest = watch.hits.first {
                Button { model.open(link: latest.line.link) } label: {
                    Text("“\(latest.line.quote)” · \(PaletteCommandPreview.meta(latest.line))")
                        .metaText(Theme.inkSecondary)
                        .lineLimit(1)
                }
                .buttonStyle(.plain)
                .help("Open where it was said")
            } else {
                Text("Not heard yet").metaText()
            }
            Spacer(minLength: 0)
            if let latest = watch.hits.first {
                if let play = latest.line.play {
                    Button {
                        model.meetings.audio.play(
                            sessionId: play.sessionId, segmentId: play.segmentId, startOffsetNs: play.startOffsetNs)
                    } label: {
                        Image(systemName: "play.fill").font(.system(size: 10))
                    }
                    .buttonStyle(.compact)
                    .help("Play the line")
                    .accessibilityLabel("Play the line")
                }
                if latest.taskId == nil {
                    Button("Make task") { Task { await model.work.task(from: latest) } }
                        .buttonStyle(.compact)
                }
            }
            Button { model.work.unwatch(watch.id) } label: {
                Image(systemName: "xmark").font(.system(size: 10, weight: .semibold))
            }
            .buttonStyle(.plain)
            .foregroundStyle(Theme.inkTertiary)
            .opacity(hovering ? 1 : 0)
            .help("Stop watching")
            .accessibilityLabel("Stop watching “\(watch.phrase)”")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .onHover { hovering = $0 }
    }
}

extension AppModel {
    static let watchOpenedEvent = "work-watch-opened"

    /// A watch notification: a click opens where it was said; ▶ plays the
    /// line, or opens it when there is no audio to play.
    func watchOpened(_ line: Data) {
        guard let event = try? Core.payload(line) as WorkWatchOpenedEvent else { return }
        let said = event.hit.line
        if event.gesture == .play, let play = said.play {
            meetings.audio.play(sessionId: play.sessionId, segmentId: play.segmentId, startOffsetNs: play.startOffsetNs)
            return
        }
        open(link: said.link)
    }
}
