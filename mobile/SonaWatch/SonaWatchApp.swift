import SwiftUI

@main
struct SonaWatchApp: App {
    @StateObject private var recorder = WatchRecorder()

    var body: some Scene {
        WindowGroup {
            WatchRecordingView(recorder: recorder)
                .onOpenURL { url in
                    if url == WatchLink.record, !recorder.isRecording { recorder.toggle() }
                }
        }
    }
}

/// The watch app is the button. It fills the screen, it shows the clock while it runs,
/// and there is nothing else to reach.
struct WatchRecordingView: View {
    @ObservedObject var recorder: WatchRecorder
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        Button {
            recorder.toggle()
        } label: {
            VStack(spacing: 6) {
                if recorder.isRecording {
                    Text(elapsedText)
                        .font(.title2.weight(.light))
                        .monospacedDigit()
                        .foregroundStyle(Theme.recording)
                    Text("watch.recording")
                        .font(.caption2)
                        .foregroundStyle(Theme.textSecondary)
                } else {
                    Circle()
                        .fill(Theme.recording)
                        .frame(width: 42, height: 42)
                    Text(recorder.status)
                        .font(.caption2)
                        .foregroundStyle(Theme.textSecondary)
                        .multilineTextAlignment(.center)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(recorder.isRecording ? "a11y.watch.stop" : "a11y.watch.record"))
        .animation(reduceMotion ? nil : .easeOut(duration: 0.18), value: recorder.isRecording)
        .background(Theme.background)
        .ignoresSafeArea()
    }

    private var elapsedText: String {
        let total = Int(recorder.elapsed)
        return String(format: "%02d:%02d", total / 60, total % 60)
    }
}
