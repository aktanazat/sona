import SwiftUI

/// The first page: what the microphone is doing, the chord that drives it,
/// the words as they land, and under that the overview: the mode, what needs
/// a decision, what is coming, this week's numbers, what happened lately.
struct CaptureScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Page {
            SecureInputBanner(store: model.secureInput)
            AccessibilityNotice(store: model.onboarding.permissions)
            hero.padding(.bottom, 32)
            if let notice = model.notice {
                Card {
                    CardRow(action: model.dismissNotice) {
                        Text(notice).bodyText(15, Theme.inkSecondary)
                    } trailing: {
                        Image(systemName: "xmark").font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(Theme.inkTertiary)
                    }
                }
                .padding(.bottom, 32)
            }
            if let error = model.coreError {
                Card {
                    CardRow {
                        Text(error).bodyText(15, Theme.inkSecondary)
                    }
                }
                .padding(.bottom, 32)
            }
            OverviewView(
                store: model.overview,
                openMeeting: model.openMeeting,
                openModes: { model.showSettings(.modes) },
                series: seriesActions)
        }
    }

    /// The "Ready" card: one big word, the chord, one action.
    private var hero: some View {
        Card {
            VStack(alignment: .leading, spacing: 14) {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    switch model.capture {
                    case .idle:
                        Text("Ready").heroText()
                    case let .recording(since):
                        TimelineView(.periodic(from: since, by: 1)) { context in
                            Text("Recording \(context.date.timeIntervalSince(since).clock)")
                                .heroText()
                                .monospacedDigit()
                        }
                        Circle().fill(Theme.live).frame(width: 10, height: 10)
                    case let .working(kind):
                        Text(kind.capitalized).heroText()
                    }
                }
                HStack(spacing: 8) {
                    if let chord = model.settings.binding("transcribe")?.currentBinding {
                        Button { model.showSettings(.essentials) } label: {
                            Shortcut(chord)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Change recording shortcut")
                        .help("Change recording shortcut")
                    }
                    Text(hint).bodyText(15, Theme.inkSecondary)
                }
                if !model.liveText.isEmpty, model.capture != .idle {
                    Text(model.liveText)
                        .bodyText(16)
                        .lineLimit(3)
                        .frame(maxWidth: 640, alignment: .leading)
                }
                HStack(spacing: 10) {
                    Button {
                        model.toggleCapture()
                    } label: {
                        Label(model.capture == .idle ? "Start recording" : "Stop", systemImage: model.capture == .idle ? "mic" : "stop.fill")
                    }
                    .buttonStyle(.primary)
                    if model.capture == .idle {
                        Button {
                            model.sheet = .recorder
                        } label: {
                            Label("Record the screen", systemImage: "record.circle")
                        }
                        .buttonStyle(.secondary)
                        Button {
                            model.showSettings(.importing)
                        } label: {
                            Label("Import audio", systemImage: "waveform.badge.plus")
                        }
                        .buttonStyle(.secondary)
                    } else {
                        Button("Cancel") { model.cancelCapture() }.buttonStyle(.secondary)
                    }
                }
                .padding(.top, 6)
            }
            .padding(24)
        }
    }

    private var hint: String {
        switch model.capture {
        case .idle:
            let hold = model.settings.settings.pushToTalk ? "hold to talk" : "tap to toggle"
            return "\(hold) · \(model.activeModel?.name ?? "No model")"
        case .recording:
            return "release to paste · \(model.settings.settings.selectedMicrophone ?? "Default microphone")"
        case .working:
            return "the words land in the app in front"
        }
    }

    /// The three standing decisions an upcoming row offers, answered by
    /// meeting settings and mapped into the overview's own shape.
    private var seriesActions: OverviewSeriesActions {
        let store = model.meetingSettings
        return OverviewSeriesActions(
            setAlwaysRecord: { key, on, revision in
                Self.write(try await store.setAlwaysRecord(seriesKey: key, alwaysRecord: on, revision: revision), key)
            },
            setTemplate: { key, template, revision in
                let mapped = template.flatMap { MeetingSeriesTemplate(rawValue: $0.rawValue) }
                return Self.write(try await store.setTemplate(seriesKey: key, template: mapped, revision: revision), key)
            },
            setDigestIncluded: { key, included, revision in
                Self.write(try await store.setDigestIncluded(seriesKey: key, included: included, revision: revision), key)
            })
    }

    private static func write(_ mutation: MeetingSeriesMutation, _ key: String) -> OverviewSeriesWrite {
        let stored = mutation.preferences
        let seriesKey = stored.seriesKey ?? key
        return OverviewSeriesWrite(
            seriesKey: seriesKey,
            series: OverviewSeries(
                seriesKey: seriesKey,
                alwaysRecord: stored.alwaysRecord,
                template: stored.template.flatMap { OverviewTemplate(rawValue: $0.rawValue) },
                digestIncluded: stored.digestIncluded),
            revision: stored.revision)
    }
}
