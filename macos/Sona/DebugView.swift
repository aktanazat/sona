import SwiftUI

/// The Debug page: instrumentation, sorted into the four things people come
/// here to debug.
///
/// The capture group's microphone and sound rows belong to the settings slice,
/// which ships them as one set of CardRows. Pass them in through `captureRows`
/// and they mount at the top of the Capture card, above the recording buffer;
/// the default renders nothing, so this page stands alone as well.
struct DebugView: View {
    let store: DebugStore
    var captureRows: () -> AnyView = { AnyView(EmptyView()) }

    var body: some View {
        Page {
            PageTitle("Debug", subtitle: "Instrumentation. Nothing here changes what Sona transcribes.")
            ErrorNote(store.error)

            PageSection("Capture") {
                Card {
                    captureRows()
                    RecordingBufferRow(store: store)
                }
            }

            PageSection("Text delivery") {
                Card {
                    WordCorrectionRow(store: store)
                }
            }

            PageSection("Diagnostics") {
                Card {
                    KeyboardDiagnosticRows(store: store)
                    ActionRow(
                        title: "Preview the release note",
                        detail: "Open the latest bundled release note without marking it as seen.",
                        button: "Open"
                    ) {
                        store.previewReleaseNote()
                    }
                    if let note = store.whatsNew.error {
                        CardRow {
                            Text(note).bodyText(14, Theme.live)
                        }
                    }
                }
            }

            PageSection("Logging") {
                Card {
                    ChoiceRow(
                        title: "Log level",
                        choices: [LogLevel.error, .warn, .info, .debug, .trace],
                        label: { $0.label },
                        selection: Binding(
                            get: { store.logLevel },
                            set: { store.setLogLevel($0) }
                        )
                    )
                    .disabled(store.settings == nil)
                    LogViewerRow(store: store)
                }
            }

            PageSection("Debug mode") {
                Card {
                    ToggleRow(
                        title: "Debug mode",
                        detail: "Keeps this page in Settings and raises what the core logs. ⌘⇧D toggles it.",
                        isOn: Binding(
                            get: { store.debugMode },
                            set: { store.setDebugMode($0) }
                        )
                    )
                    .disabled(store.settings == nil)
                }
            }
        }
        .sheet(isPresented: Binding(
            get: { store.whatsNew.note != nil },
            set: { shown in if !shown { store.whatsNew.dismiss() } }
        )) {
            WhatsNewView(store: store.whatsNew)
        }
    }
}

// MARK: - Capture

/// Extra time to keep recording after the key comes up, to catch trailing
/// audio. Zero is the default and the reset target.
private struct RecordingBufferRow: View {
    let store: DebugStore
    @State private var value: Double = 0

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 10) {
                    Text("Extra recording buffer").bodyText()
                    Text("\(Int(value))ms").metaText(Theme.inkSecondary)
                }
                Text("Extra time (in milliseconds) to keep recording after you release the key, to catch trailing audio. Set it to 0 for none.")
                    .metaText()
                    .fixedSize(horizontal: false, vertical: true)
            }
        } trailing: {
            HStack(spacing: 12) {
                Slider(value: $value, in: 0...1500, step: 50) { editing in
                    guard !editing else { return }
                    store.setRecordingBuffer(Int(value))
                }
                .frame(width: 160)
                .tint(Theme.accent)
                if Int(value) != store.defaultRecordingBufferMs {
                    Button("Reset") {
                        store.resetRecordingBuffer()
                        value = Double(store.defaultRecordingBufferMs)
                    }
                    .buttonStyle(.quiet)
                }
            }
        }
        .onAppear { value = Double(store.recordingBufferMs) }
        .onChange(of: store.recordingBufferMs) { _, stored in
            value = Double(stored)
        }
    }
}

// MARK: - Text delivery

/// The fuzzy word-correction threshold. A Whisper decoder is handed the
/// vocabulary as its prompt and never reads this, so for those models the row
/// says so and both controls go quiet rather than answering a drag with
/// nothing.
private struct WordCorrectionRow: View {
    let store: DebugStore
    @State private var value = DebugStore.defaultWordCorrectionThreshold

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 10) {
                    Text("Word correction threshold")
                        .bodyText(15, store.promptedModel ? Theme.inkDisabled : Theme.ink)
                    Text(store.promptedModel ? "Not applied" : String(format: "%.2f", value))
                        .metaText(Theme.inkSecondary)
                }
                if store.promptedModel {
                    Text("Whisper models receive your vocabulary in the decoder's prompt, so this threshold applies to Parakeet and Moonshine instead.")
                        .metaText()
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        } trailing: {
            HStack(spacing: 12) {
                Slider(value: $value, in: 0...1, step: 0.01) { editing in
                    guard !editing else { return }
                    store.setWordCorrectionThreshold(value)
                }
                .frame(width: 160)
                .tint(Theme.accent)
                .disabled(store.promptedModel)
                if value != store.defaultThreshold, !store.promptedModel {
                    Button("Reset") {
                        store.resetWordCorrectionThreshold()
                        value = store.defaultThreshold
                    }
                    .buttonStyle(.quiet)
                }
            }
        }
        .onAppear { value = store.wordCorrectionThreshold }
        .onChange(of: store.wordCorrectionThreshold) { _, stored in
            value = stored
        }
    }
}

// MARK: - Keyboard diagnostic

/// Count-only keyboard capture test: how many key-down, key-up, modifier and
/// mouse events reach Sona in ten seconds, never which keys. Modifier events
/// arriving while key-down stays at zero is the signature of stuck Secure
/// Input.
private struct KeyboardDiagnosticRows: View {
    let store: DebugStore

    var body: some View {
        ActionRow(
            title: "Keyboard diagnostic",
            detail: "Checks whether keyboard events reach Sona. Only event counts are recorded, never which keys you press.",
            button: "Run 10s diagnostic",
            busy: store.diagnosticRunning
        ) {
            store.runDiagnostic()
        }

        if store.diagnosticRunning {
            CardRow {
                Text("Listening… press your shortcut a few times (e.g. Option+Space)").metaText()
            }
        }

        if let message = store.diagnosticMessage {
            CardRow {
                Text(message).bodyText(14, Theme.live)
            } trailing: {
                Button("Retry") { store.runDiagnostic() }
                    .buttonStyle(.compact)
                    .disabled(store.diagnosticRunning)
            }
        }

        if let report = store.diagnosticReport, let verdict = store.diagnosticVerdict {
            CardRow {
                VStack(alignment: .leading, spacing: 12) {
                    Text(verdict.text)
                        .bodyText(14, KeyboardDiagnosticRows.colour(verdict.tone))
                        .fixedSize(horizontal: false, vertical: true)
                    // The counts are the reading; the line above is only what
                    // they were taken to mean.
                    HStack(alignment: .firstTextBaseline, spacing: 18) {
                        DebugFact(label: "Secure input", value: KeyboardDiagnosticVerdict.secureInputLine(report))
                        DebugFact(label: "Key down", value: "\(report.keyDown)")
                        DebugFact(label: "Key up", value: "\(report.keyUp)")
                        DebugFact(label: "Modifiers", value: "\(report.flagsChanged)")
                        DebugFact(label: "Mouse", value: "\(report.mouse)")
                    }
                }
            }
        }
    }

    private static func colour(_ tone: KeyboardDiagnosticVerdict.Tone) -> Color {
        switch tone {
        case .muted: Theme.inkTertiary
        case .warning: Theme.accent
        case .danger: Theme.live
        }
    }
}

/// A label and the number beside it.
private struct DebugFact: View {
    let label: String
    let value: String

    var body: some View {
        HStack(spacing: 6) {
            Text(label).metaText()
            Text(value).font(TypeScale.label(14)).foregroundStyle(Theme.ink)
        }
    }
}

// MARK: - Live logs

/// The live log: what the core writes to `sona.log` while this page is open,
/// at or above the level chosen here.
private struct LogViewerRow: View {
    let store: DebugStore

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Text("Live logs").bodyText()
                    Text("\(store.visibleLines.count) lines").metaText(Theme.inkSecondary)
                    Spacer(minLength: 12)
                    Picker("", selection: Binding(
                        get: { store.filter },
                        set: { store.setFilter($0) }
                    )) {
                        ForEach([LogLevel.trace, .debug, .info, .warn, .error], id: \.self) { level in
                            Text(level == .trace ? "All levels" : "\(level.label) and up").tag(level)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .fixedSize()
                    Button(store.paused ? "Resume" : "Pause") { store.togglePaused() }
                        .buttonStyle(.compact)
                    Button(store.copied ? "Copied" : "Copy") { store.copyLines() }
                        .buttonStyle(.compact)
                        .disabled(store.visibleLines.isEmpty)
                    Button("Clear") { store.clearLines() }
                        .buttonStyle(.compact)
                        .disabled(store.lines.isEmpty)
                }

                if let path = store.logFilePath {
                    Text("Only lines written while this page is open appear here, from \(path).")
                        .metaText()
                        .fixedSize(horizontal: false, vertical: true)
                }

                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        if store.visibleLines.isEmpty {
                            Text("No lines yet.").metaText()
                        } else {
                            ForEach(store.visibleLines) { line in
                                LogLineRow(line: line)
                            }
                        }
                    }
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .defaultScrollAnchor(.bottom)
                .frame(height: 280)
                .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.radiusControl)
                        .strokeBorder(Theme.border, lineWidth: 1)
                )
            }
        }
    }
}

/// One log line: its clock, its level in a fixed column, its message.
private struct LogLineRow: View {
    let line: LogLine

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Text(line.time)
                .font(TypeScale.mono(12))
                .foregroundStyle(Theme.inkTertiary)
            Text(line.tag)
                .font(TypeScale.mono(12))
                .foregroundStyle(tagColour)
                .frame(width: 46, alignment: .leading)
            Text(line.message)
                .font(TypeScale.mono(12))
                .foregroundStyle(messageColour)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// Levels stay in the ink ramp; only a warning or an error takes a hue.
    private var tagColour: Color {
        switch line.level {
        case .error: Theme.live
        case .warn: Theme.accent
        case .info: Theme.inkSecondary
        default: Theme.inkTertiary
        }
    }

    private var messageColour: Color {
        switch line.level {
        case .error: Theme.live
        case .trace: Theme.inkTertiary
        default: Theme.ink
        }
    }
}
