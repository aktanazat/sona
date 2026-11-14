import SwiftUI

struct CameraWatermarkSettingsView: View {
    @State private var camera = CameraWatermarkController.shared

    var body: some View {
        PageSection("Camera recording label") {
            Card {
                ToggleRow(
                    title: "Show “Recording with Sona” on my video",
                    detail: "Shows only while Sona records.",
                    isOn: Binding(get: { camera.enabled }, set: { camera.setEnabled($0) }))
                VStack(alignment: .leading, spacing: 12) {
                    Text("Install Sona Camera, approve it in macOS, then pick it as the camera in your call app.")
                        .bodyText(13, Theme.inkSecondary)
                    Text(camera.status).bodyText(14)
                    if let streamStatus = camera.streamStatus {
                        Text(streamStatus).bodyText(13, Theme.inkSecondary)
                    }
                    if let error = camera.error {
                        Text(error).bodyText(13, Theme.live).textSelection(.enabled)
                    }
                    HStack(spacing: 10) {
                        if camera.installed {
                            Button("Remove Sona Camera") { camera.uninstall() }
                                .buttonStyle(.secondary).disabled(camera.busy)
                        } else {
                            Button(camera.busy ? "Installing…" : "Install Sona Camera") { camera.install() }
                                .buttonStyle(.secondary).disabled(camera.busy)
                        }
                        Button("System Settings") { camera.openExtensionSettings() }
                            .buttonStyle(.quiet)
                        Spacer()
                        Button("Refresh") { camera.refresh() }.buttonStyle(.quiet).disabled(camera.busy)
                    }
                    Picker("Physical camera", selection: Binding(get: { camera.cameraID }, set: { camera.selectCamera($0) })) {
                        Text("Choose a camera…").tag("")
                        ForEach(camera.cameras) { item in Text(item.name).tag(item.id) }
                        if !camera.cameraID.isEmpty, !camera.cameras.contains(where: { $0.id == camera.cameraID }) {
                            Text("Selected camera is disconnected").tag(camera.cameraID)
                        }
                    }
                    .font(TypeScale.body(14))
                    if camera.cameras.isEmpty {
                        Text("No physical camera was found. Connect one and press Refresh.").bodyText(13, Theme.inkSecondary)
                    }
                    if camera.cameraAccess != .authorized {
                        Button(camera.cameraAccess == .notDetermined ? "Allow camera access…" : "Open Camera privacy settings") {
                            camera.requestCameraAccess()
                        }
                        .buttonStyle(.secondary)
                        Text("macOS camera permission is separate from installing the extension. Approving permission does not start video.")
                            .bodyText(13, Theme.inkSecondary)
                    }
                }
                .padding(20)
            }
        }
        .task { camera.refresh() }
    }
}
