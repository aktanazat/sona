import SwiftUI
import UIKit

/// Meeting capture: elapsed time, one control, and upload status.
struct RecordingScreen: View {
    @ObservedObject var model: AppModel
    /* Observed separately: `AppModel` holds the recorder but does not republish its
     * changes, so a view that watched only the model would never see the clock run. */
    @ObservedObject var recorder: PhoneRecorder
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @ScaledMetric(relativeTo: .largeTitle) private var timerSize: CGFloat = 60
    @ScaledMetric(relativeTo: .body) private var controlSize: CGFloat = 96
    @State private var showsPairing = false

    var body: some View {
        VStack(spacing: 0) {
            elapsed
                .padding(.top, 72)
            Spacer()
            control
            status
                .padding(.top, 20)
            Spacer()
            pairingLine
                .padding(.bottom, 8)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .sheet(isPresented: $showsPairing) {
            PairingScreen(model: model)
        }
        .task { await model.refresh() }
    }

    private var elapsed: some View {
        Text(elapsedText)
            .font(.system(size: timerSize, weight: .light))
            .monospacedDigit()
            .foregroundStyle(recorder.isRecording ? Theme.recording : Theme.textPrimary)
            .animation(reduceMotion ? nil : .easeOut(duration: 0.18), value: recorder.isRecording)
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
            .font(.footnote)
            .foregroundStyle(recorder.isRecording ? Theme.recording : Theme.textSecondary)
            .accessibilityIdentifier("status")
    }

    /// The one other thing on the screen. Unpaired it is the way in; paired it states
    /// the fact and still opens the sheet, because the call-offer switch lives there.
    private var pairingLine: some View {
        Button { showsPairing = true } label: {
            Text(model.isPaired ? "pairing.line.paired" : "pairing.line.notPaired")
                .font(.caption)
                .foregroundStyle(model.isPaired ? Theme.textTertiary : Theme.accent)
        }
        .accessibilityLabel(Text("a11y.pair"))
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
