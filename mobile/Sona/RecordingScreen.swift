import SwiftUI
import UIKit

/// Meeting capture: the input level, elapsed time, one control, and upload status.
struct RecordingScreen: View {
    @ObservedObject var model: AppModel
    /* Observed separately: `AppModel` holds the recorder but does not republish its
     * changes, so a view that watched only the model would never see the clock run. */
    @ObservedObject var recorder: PhoneRecorder
    @ObservedObject var calendar: PhoneCalendar
    @AppStorage(PhoneCalendar.enabledKey) private var calendarEnabled = false
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @ScaledMetric(relativeTo: .largeTitle) private var timerSize: CGFloat = 56
    @ScaledMetric(relativeTo: .body) private var controlSize: CGFloat = 60
    @State private var showsPairing = false

    var body: some View {
        NavigationStack {
            List {
                Section {
                    VStack(spacing: 8) {
                        LevelWaveform(meter: recorder.meter)
                            .padding(.bottom, 8)
                        elapsed
                        status
                    }
                    .frame(maxWidth: .infinity)
                    .listRowBackground(Color.clear)
                    .listRowInsets(EdgeInsets(top: 8, leading: 0, bottom: 16, trailing: 0))
                }
                Section {
                    TextField("meeting.title", text: $model.meetingTitle, prompt: Text("meeting.title").foregroundStyle(Theme.textTertiary))
                        .font(.body)
                        .foregroundStyle(Theme.textPrimary)
                        .frame(minHeight: 44)
                        .accessibilityIdentifier("meeting-title")
                    TextEditor(text: $model.meetingNotes)
                        .font(.body)
                        .foregroundStyle(Theme.textPrimary)
                        .scrollContentBackground(.hidden)
                        .frame(minHeight: 120)
                        .padding(.leading, -5)
                        .overlay(alignment: .topLeading) {
                            if model.meetingNotes.isEmpty {
                                Text("meeting.ownNotes")
                                    .foregroundStyle(Theme.textTertiary)
                                    .padding(.top, 8)
                                    .allowsHitTesting(false)
                                    .accessibilityHidden(true)
                            }
                        }
                        .accessibilityLabel(Text("meeting.ownNotes"))
                        .accessibilityIdentifier("meeting-own-notes")
                } footer: {
                    Text("meeting.notesHelp")
                        .foregroundStyle(Theme.textSecondary)
                }
                .listRowBackground(Theme.surface)
                upcoming
            }
            .listStyle(.insetGrouped)
            .scrollContentBackground(.hidden)
            .scrollDismissesKeyboard(.interactively)
            .background(Theme.background)
            .navigationTitle("tab.recording")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) { pairingLine }
            }
            .safeAreaBar(edge: .bottom) {
                control
                    .padding(.vertical, 8)
            }
            .sheet(isPresented: $showsPairing) {
                PairingScreen(model: model)
            }
            .task { await model.refresh(); await calendar.refresh() }
            .refreshable { await model.refresh(); await calendar.refresh() }
            .onChange(of: scenePhase) { _, phase in
                if phase == .active { Task { await calendar.refresh() } }
            }
        }
        .tint(Theme.accent)
    }

    @ViewBuilder
    private var upcoming: some View {
        Section {
            Toggle("calendar.enable", isOn: $calendarEnabled)
                .foregroundStyle(Theme.textPrimary)
                .frame(minHeight: 44)
                .onChange(of: calendarEnabled) { _, enabled in
                    Task { await calendar.refresh(requestAccess: enabled) }
                }
        } footer: {
            Text("calendar.privacy")
                .foregroundStyle(Theme.textSecondary)
        }
        .listRowBackground(Theme.surface)
        if calendarEnabled {
            Section {
                if calendar.loading {
                    ProgressView()
                } else if let error = calendar.error {
                    Text(error)
                        .font(.footnote)
                        .foregroundStyle(Theme.recording)
                    if let url = URL(string: UIApplication.openSettingsURLString) {
                        Link("calendar.settings", destination: url)
                    }
                } else if calendar.events.isEmpty {
                    Text("calendar.empty")
                        .font(.footnote)
                        .foregroundStyle(Theme.textSecondary)
                } else {
                    ForEach(calendar.events.indices, id: \.self) { index in
                        let event = calendar.events[index]
                        Button {
                            model.meetingTitle = event.title ?? NSLocalizedString("meeting.untitled", comment: "")
                            model.startRecording()
                        } label: {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(event.title ?? NSLocalizedString("meeting.untitled", comment: ""))
                                    .foregroundStyle(Theme.textPrimary)
                                Text(event.startDate.formatted(date: .abbreviated, time: .shortened))
                                    .font(.footnote)
                                    .foregroundStyle(Theme.textSecondary)
                            }
                            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                        }
                        .disabled(recorder.isRecording || model.startingRecording)
                    }
                }
            } header: {
                Text("calendar.upcoming")
                    .textCase(nil)
            }
            .listRowBackground(Theme.surface)
        }
    }

    private var elapsed: some View {
        Text(elapsedText)
            .font(.system(size: timerSize, weight: .regular))
            .monospacedDigit()
            .foregroundStyle(Theme.textPrimary)
            .lineLimit(1)
            .minimumScaleFactor(0.65)
            .accessibilityLabel(Text("a11y.elapsed"))
            .accessibilityValue(elapsedText)
    }

    private var control: some View {
        Button {
            if recorder.isRecording {
                model.stopRecording()
            } else {
                model.startRecording()
            }
        } label: {
            RecordPill(glyph: recorder.isRecording ? .stop : .record, height: controlSize)
        }
        .buttonStyle(PressScaleButtonStyle(reduceMotion: reduceMotion))
        /* A second tap before the microphone answers is not a new recording. */
        .disabled(model.startingRecording)
        .accessibilityLabel(Text(recorder.isRecording ? "a11y.stop" : "a11y.record"))
        .accessibilityIdentifier(recorder.isRecording ? "stop" : "record")
    }

    private var status: some View {
        Text(statusKey)
            .font(.subheadline)
            .foregroundStyle(recorder.isRecording ? Theme.recording : Theme.textSecondary)
            .accessibilityIdentifier("status")
    }

    private var pairingLine: some View {
        Button { showsPairing = true } label: {
            HStack(spacing: 4) {
                Image(systemName: "link")
                Text(model.isPaired ? "pairing.line.paired" : "pairing.line.notPaired")
            }
            .font(.footnote)
            .foregroundStyle(model.isPaired ? Theme.textSecondary : Theme.accent)
        }
        .accessibilityLabel(Text("a11y.pair"))
        .accessibilityValue(Text(model.isPaired ? "pairing.line.paired" : "pairing.line.notPaired"))
        .accessibilityIdentifier("pairing-line")
    }

    private var elapsedText: String {
        let total = Int(recorder.elapsed)
        return String(format: "%02d:%02d", total / 60, total % 60)
    }

    /// One line, and it never claims more than is true: a refused recording is not
    /// waiting for anything the phone will do by itself.
    private var statusKey: LocalizedStringKey {
        if recorder.isRecording { return "status.recording" }
        if let notice = recorder.notice { return notice.key }
        switch model.outbox {
        case .empty: return "status.ready"
        case .uploading: return "status.uploading"
        case .waiting: return "status.waiting"
        case .saved: return "status.saved"
        }
    }
}

/// Recent input loudness as thin bars, newest on the right. Silence draws a dotted line.
struct LevelWaveform: View {
    @ObservedObject var meter: InputLevelMeter
    var height: CGFloat = 56

    var body: some View {
        Canvas { context, size in
            let levels = meter.levels
            let pitch = size.width / CGFloat(levels.count)
            let bar = max(1.5, pitch * 0.4)
            var path = Path()
            for (index, level) in levels.enumerated() {
                let length = max(bar, CGFloat(level) * size.height)
                path.addRoundedRect(
                    in: CGRect(
                        x: CGFloat(index) * pitch + (pitch - bar) / 2,
                        y: (size.height - length) / 2,
                        width: bar,
                        height: length
                    ),
                    cornerSize: CGSize(width: bar / 2, height: bar / 2)
                )
            }
            context.fill(path, with: .foreground)
        }
        .foregroundStyle(Theme.textTertiary)
        .frame(height: height)
        .accessibilityHidden(true)
    }
}

/// The only motion in the app: a press settles the control slightly.
struct PressScaleButtonStyle: ButtonStyle {
    let reduceMotion: Bool

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(reduceMotion || !configuration.isPressed ? 1 : 0.96)
            .animation(reduceMotion ? nil : .easeOut(duration: 0.12), value: configuration.isPressed)
    }
}

/// First run, once: what is recorded and where it goes, then one button.
struct ConsentScreen: View {
    @ObservedObject var model: AppModel

    var body: some View {
        ScrollView {
            VStack(spacing: 16) {
                Text("consent.title")
                    .font(.title2.weight(.semibold))
                    .foregroundStyle(Theme.textPrimary)
                    .padding(.top, 48)
                    .padding(.bottom, 8)
                Text("consent.body1")
                Text("consent.body2")
                Text("consent.notifications")
                    .font(.footnote)
                    .foregroundStyle(Theme.textTertiary)
            }
            .font(.body)
            .foregroundStyle(Theme.textSecondary)
            .multilineTextAlignment(.center)
            .padding(.horizontal, 28)
        }
        .safeAreaInset(edge: .bottom) {
            Button { model.acceptConsent() } label: {
                Text("consent.action")
                    .font(.body.weight(.semibold))
                    .frame(maxWidth: .infinity, minHeight: 36)
            }
            .buttonStyle(.borderedProminent)
            .buttonBorderShape(.capsule)
            .controlSize(.large)
            .tint(Theme.accent)
            .padding(.horizontal, 28)
            .padding(.bottom, 16)
            .accessibilityIdentifier("consent-start")
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
    }
}
