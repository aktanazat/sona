import SwiftUI
import UIKit

/// Meeting capture: elapsed time, one control, and upload status.
struct RecordingScreen: View {
    @ObservedObject var model: AppModel
    /* Observed separately: `AppModel` holds the recorder but does not republish its
     * changes, so a view that watched only the model would never see the clock run. */
    @ObservedObject var recorder: PhoneRecorder
    @ObservedObject var calendar: PhoneCalendar
    @AppStorage(PhoneCalendar.enabledKey) private var calendarEnabled = false
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @ScaledMetric(relativeTo: .largeTitle) private var timerSize: CGFloat = 44
    @ScaledMetric(relativeTo: .body) private var controlSize: CGFloat = 96
    @State private var showsPairing = false

    var body: some View {
        PhonePage("tab.recording") {
            let layout = dynamicTypeSize.isAccessibilitySize
                ? AnyLayout(VStackLayout(alignment: .leading, spacing: 16))
                : AnyLayout(HStackLayout(spacing: 20))
            layout {
                VStack(alignment: .leading, spacing: 4) {
                    elapsed
                    status
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                control
            }
            Divider().overlay(Theme.border)
            VStack(alignment: .leading, spacing: 10) {
                TextField("meeting.title", text: $model.meetingTitle)
                    .font(.body.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)
                    .padding(.horizontal, 12)
                    .frame(minHeight: 44)
                    .background(Theme.surface)
                    .clipShape(RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous)
                            .strokeBorder(Theme.border, lineWidth: 0.5)
                    }
                    .accessibilityIdentifier("meeting-title")
                Text("meeting.ownNotes")
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)
                TextEditor(text: $model.meetingNotes)
                    .font(.body)
                    .foregroundStyle(Theme.textPrimary)
                    .frame(minHeight: 120)
                    .scrollContentBackground(.hidden)
                    .padding(8)
                    .background(Theme.surface)
                    .clipShape(RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous)
                            .strokeBorder(Theme.border, lineWidth: 0.5)
                    }
                    .accessibilityLabel(Text("meeting.ownNotes"))
                    .accessibilityIdentifier("meeting-own-notes")
                Text("meeting.notesHelp")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
            }
            Divider().overlay(Theme.border)
            upcoming
            pairingLine
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

    private var upcoming: some View {
        VStack(alignment: .leading, spacing: 8) {
            Toggle("calendar.enable", isOn: $calendarEnabled)
                .frame(minHeight: 44)
                .onChange(of: calendarEnabled) { _, enabled in
                    Task { await calendar.refresh(requestAccess: enabled) }
                }
            Text("calendar.privacy").font(.footnote).foregroundStyle(Theme.textSecondary)
            if calendarEnabled {
                Text("calendar.upcoming")
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)
                if calendar.loading { ProgressView() }
                else if let error = calendar.error {
                    Text(error).font(.footnote).foregroundStyle(Theme.recording)
                    if let url = URL(string: UIApplication.openSettingsURLString) {
                        Link("calendar.settings", destination: url)
                    }
                } else if calendar.events.isEmpty {
                    Text("calendar.empty").font(.footnote).foregroundStyle(Theme.textSecondary)
                } else {
                    ForEach(calendar.events.indices, id: \.self) { index in
                        let event = calendar.events[index]
                        Button {
                            model.meetingTitle = event.title ?? NSLocalizedString("meeting.untitled", comment: "")
                            model.startRecording()
                        } label: {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(event.title ?? NSLocalizedString("meeting.untitled", comment: ""))
                                    .font(.body.weight(.medium))
                                    .foregroundStyle(Theme.textPrimary)
                                Text(event.startDate.formatted(date: .abbreviated, time: .shortened))
                                    .font(.footnote).foregroundStyle(Theme.textSecondary)
                            }
                            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                            .padding(.vertical, 6)
                        }
                        .disabled(recorder.isRecording || model.startingRecording)
                        .overlay(alignment: .bottom) {
                            Rectangle().fill(Theme.border).frame(height: 0.5)
                        }
                    }
                }
            }
        }
    }

    private var elapsed: some View {
        Text(elapsedText)
            .font(.system(size: timerSize, weight: .light))
            .monospacedDigit()
            .foregroundStyle(Theme.textPrimary)
            .lineLimit(1)
            .minimumScaleFactor(0.65)
            .accessibilityLabel(Text("a11y.elapsed"))
            .accessibilityValue(elapsedText)
    }

    /// One round control. A filled dot records; a square stops. Flat, no shadow.
    private var control: some View {
        Button {
            if recorder.isRecording {
                model.stopRecording()
            } else {
                model.startRecording()
            }
        } label: {
            ZStack {
                Circle()
                    .strokeBorder(Theme.border, lineWidth: 1)
                if recorder.isRecording {
                    RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous)
                        .fill(Theme.recording)
                        .frame(width: controlSize * 0.34, height: controlSize * 0.34)
                } else {
                    Circle()
                        .fill(Theme.recording)
                        .frame(width: controlSize * 0.62, height: controlSize * 0.62)
                }
            }
            .frame(width: controlSize, height: controlSize)
            .contentShape(Circle())
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
            HStack(spacing: 8) {
                Image(systemName: "link")
                    .accessibilityHidden(true)
                Text(model.isPaired ? "pairing.line.paired" : "pairing.line.notPaired")
                Spacer()
                Image(systemName: "chevron.right")
                    .font(.caption)
                    .foregroundStyle(Theme.textTertiary)
                    .accessibilityHidden(true)
            }
            .font(.footnote)
            .foregroundStyle(model.isPaired ? Theme.textSecondary : Theme.accent)
            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
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
        VStack(alignment: .leading, spacing: 20) {
            Text("consent.title")
                .font(.title2.weight(.semibold))
                .foregroundStyle(Theme.textPrimary)
                .padding(.top, 44)
            Text("consent.body1")
                .font(.body)
                .foregroundStyle(Theme.textSecondary)
            Text("consent.body2")
                .font(.body)
                .foregroundStyle(Theme.textSecondary)
            Text("consent.notifications")
                .font(.footnote)
                .foregroundStyle(Theme.textTertiary)
            Spacer()
            Button { model.acceptConsent() } label: {
                Text("consent.action")
                    .font(.body.weight(.medium))
                    .foregroundStyle(Theme.onAccent)
                    .frame(maxWidth: .infinity)
                    .frame(height: 50)
                    .background(
                        RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous)
                            .fill(Theme.accent)
                    )
            }
            .accessibilityIdentifier("consent-start")
        }
        .padding(.horizontal, 28)
        .padding(.bottom, 32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
    }
}
