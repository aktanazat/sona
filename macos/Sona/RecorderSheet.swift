import SwiftUI

/// The screen recorder, as a sheet the shell presents:
/// `.sheet(isPresented:) { RecorderSheet(store: store, onClose: { ... }) }`.
///
/// Every phase of src/components/recorder/RecorderDialog.tsx has a state here.
/// The native source picker belongs to the core (src-tauri/src/recorder_macos.rs),
/// so nothing on this sheet draws a screen list: it reports the phase the core
/// publishes and whether a screen was chosen.
struct RecorderSheet: View {
    let store: RecorderStore
    /// The shell's dismissal. The sheet only calls it for a phase that may
    /// close; `store.stop` cancels a preview however the sheet went away.
    var onClose: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Hairline()
            if store.error != nil {
                ErrorNote(store.error)
                    .padding(.horizontal, 24)
                    .padding(.top, 16)
            }
            content
            footer
        }
        .frame(width: 540)
        .background(Theme.page)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusDialog))
        .interactiveDismissDisabled(!store.canClose)
        .task { await store.start() }
        .onDisappear { store.stop() }
    }

    // MARK: - Head

    /// The title, the elapsed clock once there is a capture to time, and the
    /// phase in words with the live dot while recording.
    private var header: some View {
        HStack(alignment: .firstTextBaseline, spacing: 16) {
            Text("Record screen").headlineText()
            Spacer(minLength: 12)
            if store.hasCapture {
                HStack(spacing: 8) {
                    Text("Elapsed").metaText()
                    Text(store.elapsed.clock)
                        .font(TypeScale.mono(13))
                        .foregroundStyle(Theme.ink)
                }
            }
            HStack(spacing: 8) {
                if store.phase == .recording {
                    /* The dialog's own red dot. `LiveDot` wants a start date,
                     * and the core times the recording, not the shell. */
                    Circle().fill(Theme.live).frame(width: 8, height: 8)
                }
                Text(store.phase.label).metaText(Theme.inkSecondary)
            }
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 18)
    }

    // MARK: - Body

    @ViewBuilder private var content: some View {
        switch store.phase {
        case .idle:
            setup
        case .permission:
            panel { RecorderNotice(text: store.permission.request, tone: Theme.accent) }
        case .previewing, .starting, .recording, .paused, .finalizing:
            sources
        case .saved:
            saved
        case .failed:
            panel {
                if let failure = store.failure {
                    RecorderNotice(text: failure.message, tone: Theme.live)
                }
            }
        case .checking, .selectingSource:
            EmptyView()
        }
    }

    /// The setup rows: the screen is always required, and each input is a
    /// switch plus, when there is more than one device, a choice of device.
    private var setup: some View {
        @Bindable var store = store
        return panel {
            CardRow {
                Text("Screen").bodyText()
            } trailing: {
                Text("Required").metaText()
            }
            RecorderDeviceRow(
                title: "Camera",
                devices: store.cameraDevices,
                isOn: $store.cameraEnabled,
                deviceId: $store.cameraDeviceId)
            RecorderDeviceRow(
                title: "Microphone",
                devices: store.microphoneDevices,
                isOn: $store.microphoneEnabled,
                deviceId: $store.microphoneDeviceId)
        }
    }

    /// What the recording is made of, once the core holds a source.
    private var sources: some View {
        panel {
            CardRow { Text("Screen selected").bodyText() }
            if let camera = store.cameraName {
                CardRow {
                    Text("Camera").bodyText()
                } trailing: {
                    Text(camera).metaText()
                }
            }
            if let microphone = store.microphoneName {
                CardRow {
                    Text("Microphone").bodyText()
                } trailing: {
                    Text(microphone).metaText()
                }
            }
            droppedFrames
        }
    }

    /// The saved file: its name, how long it runs, how big it is, and why
    /// Finder refused it when it did.
    private var saved: some View {
        panel {
            CardRow {
                Text(store.snapshot.outputName ?? "Recording saved").bodyText()
            } trailing: {
                Text(savedFacts).font(TypeScale.mono(13)).foregroundStyle(Theme.inkSecondary)
            }
            droppedFrames
            if store.state.revealFailed, let note = store.revealNote {
                RecorderNotice(text: note, tone: Theme.live)
            }
        }
    }

    /// "3:12 · 1920 × 1080".
    private var savedFacts: String {
        let clock = store.elapsed.clock
        guard let dimensions = store.snapshot.dimensions else { return clock }
        return "\(clock) · \(dimensions)"
    }

    /// The frames the writer could not keep up with. Quiet until there are any.
    @ViewBuilder private var droppedFrames: some View {
        if store.droppedFrames > 0 {
            CardRow {
                Text("Dropped frames").bodyText()
            } trailing: {
                Text("\(store.droppedFrames)").font(TypeScale.mono(13)).foregroundStyle(Theme.live)
            }
        }
    }

    private func panel<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        Card(content: content)
            .padding(.horizontal, 24)
            .padding(.top, 20)
            .padding(.bottom, 4)
    }

    // MARK: - Foot

    @ViewBuilder private var footer: some View {
        switch store.phase {
        case .idle:
            bar {
                closeButton
                Button("Choose screen") { store.chooseScreen() }
                    .buttonStyle(.primary)
                    .disabled(store.selectionPending)
            }
        case .permission:
            permissionFooter
        case .previewing:
            bar {
                closeButton
                Button("Change") { store.stopPreview() }
                    .buttonStyle(.secondary)
                /* The dialog moved focus to this button when the phase
                 * arrived; on macOS that is Return. */
                Button("Start recording") { store.startRecording() }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
            }
        case .recording:
            bar {
                Button("Pause") { store.pause() }
                    .buttonStyle(.secondary)
                Button("Stop & save") { store.stopAndSave() }
                    .buttonStyle(.primary)
            }
        case .paused:
            bar {
                Button("Resume") { store.resume() }
                    .buttonStyle(.secondary)
                Button("Stop & save") { store.stopAndSave() }
                    .buttonStyle(.primary)
            }
        case .saved:
            bar {
                Button("Reveal") { store.revealRecording() }
                    .buttonStyle(.secondary)
                Button("Open") { store.openRecording() }
                    .buttonStyle(.secondary)
                doneButton
            }
        case .failed:
            failureFooter
        case .checking, .selectingSource, .starting, .finalizing:
            /* Selecting, starting and finalizing own work that a close would
             * orphan, so they offer nothing; checking is closable. */
            if store.canClose {
                bar { closeButton }
            }
        }
    }

    @ViewBuilder private var permissionFooter: some View {
        if store.state.permissionRequested {
            bar {
                closeButton
                Button("Open System Settings") { store.openPermissionSettings() }
                    .buttonStyle(.secondary)
                Button("Re-check") { store.resolvePermission(request: false) }
                    .buttonStyle(.primary)
                    .disabled(store.permissionPending)
                    .keyboardShortcut(.defaultAction)
            }
        } else {
            bar {
                closeButton
                Button("Grant access") { store.resolvePermission(request: true) }
                    .buttonStyle(.primary)
                    .disabled(store.permissionPending)
                    .keyboardShortcut(.defaultAction)
            }
        }
    }

    /// One way out per failure: grant the permission it names, choose another
    /// source, try the whole preflight again, or just acknowledge it.
    @ViewBuilder private var failureFooter: some View {
        switch store.recovery {
        case .permission:
            if store.recoveryPermission != nil {
                bar {
                    closeButton
                    Button("Re-check") { store.recheckFailedPermission() }
                        .buttonStyle(.primary)
                        .keyboardShortcut(.defaultAction)
                }
            } else {
                bar { doneButton.keyboardShortcut(.defaultAction) }
            }
        case .choose:
            bar {
                closeButton
                Button("Change") { store.clearFailure() }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
            }
        case .retry:
            bar {
                closeButton
                Button("Retry") { store.retry() }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
            }
        case .done:
            bar { doneButton.keyboardShortcut(.defaultAction) }
        }
    }

    private var closeButton: some View {
        Button("Close") { close() }
            .buttonStyle(.secondary)
    }

    private var doneButton: some View {
        Button("Done") { close() }
            .buttonStyle(.primary)
    }

    private func bar<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        VStack(spacing: 0) {
            Hairline()
            HStack(spacing: 10) {
                Spacer(minLength: 0)
                content()
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 16)
        }
        .padding(.top, 20)
    }

    private func close() {
        guard store.requestClose() else { return }
        onClose()
    }
}

/// One input: its name, the one device it has or the fact that it has none, a
/// switch, and a menu of devices when there is a choice to make.
private struct RecorderDeviceRow: View {
    let title: String
    let devices: [RecorderDevice]
    @Binding var isOn: Bool
    @Binding var deviceId: String?

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).bodyText(15, devices.isEmpty ? Theme.inkDisabled : Theme.ink)
                if let fact {
                    Text(fact).metaText()
                }
            }
        } trailing: {
            HStack(spacing: 12) {
                if isOn, devices.count > 1, deviceId != nil {
                    Picker("", selection: selection) {
                        ForEach(devices) { device in
                            Text(device.name).tag(device.id)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .fixedSize()
                }
                Toggle("", isOn: $isOn)
                    .labelsHidden()
                    .toggleStyle(.switch)
                    .tint(Theme.accent)
                    .disabled(devices.isEmpty)
            }
        }
    }

    /// The one device's name, or "Unavailable" when nothing can serve the input.
    private var fact: String? {
        if devices.isEmpty { return "Unavailable" }
        return devices.count == 1 ? devices[0].name : nil
    }

    private var selection: Binding<String> {
        Binding(
            get: { deviceId ?? devices.first?.id ?? "" },
            set: { deviceId = $0 })
    }
}

/// A sentence the reader has to act on: a permission to grant in bronze, a
/// failure in live red.
private struct RecorderNotice: View {
    let text: String
    let tone: Color

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text(text).font(TypeScale.body(14)).foregroundStyle(tone)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .overlay(alignment: .bottom) { Hairline() }
    }
}
