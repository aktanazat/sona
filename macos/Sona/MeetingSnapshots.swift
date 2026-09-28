import AppKit
import CoreGraphics
import Observation
import SwiftUI

private struct MeetingScreenSnapshot: Decodable, Identifiable {
    let snapshotId: String
    let sessionId: MeetingSessionId
    let offsetNs: Int64
    let width: Int
    let height: Int
    var id: String { snapshotId }
}

private struct MeetingScreenSnapshotStatus: Decodable {
    let automaticSetting: Bool
    let turnedOffForMeeting: Bool
    let screenRecordingGranted: Bool
    let state: String
    let count: Int
    let automaticLimit: Int
}

private struct MeetingScreenSnapshotImage: Decodable {
    let pngBase64: String
}

private struct SnapshotSessionParams: Encodable {
    let sessionId: MeetingSessionId
}

private struct SnapshotImageParams: Encodable {
    let sessionId: MeetingSessionId
    let snapshotId: String
    let size: String
}

private struct SnapshotDeleteParams: Encodable {
    let sessionId: MeetingSessionId
    let snapshotId: String
}

private struct SnapshotAutomaticParams: Encodable {
    let sessionId: MeetingSessionId
    let enabled: Bool
}

private struct SnapshotSettingParams: Encodable {
    let enabled: Bool
}

private struct SnapshotSettings: Decodable {
    let meetingScreenSnapshotsEnabled: Bool?
}

private enum SnapshotProblem: String, Decodable {
    case screenRecordingDenied = "screen_recording_denied"
    case noMeetingWindow = "no_meeting_window"
    case notRecording = "not_recording"
    case limitReached = "limit_reached"
    case notFound = "not_found"
    case storageUnavailable = "storage_unavailable"
    case unsupported
    case captureFailed = "capture_failed"

    var message: String {
        switch self {
        case .screenRecordingDenied: "Allow Screen Recording in System Settings, then try again."
        case .noMeetingWindow: "Bring the meeting window onto this screen, then try again."
        case .notRecording: "Resume recording before taking a snapshot."
        case .limitReached: "This meeting has reached its snapshot limit."
        case .notFound: "This snapshot is no longer available."
        case .storageUnavailable: "Sona could not open the saved snapshots. Try again."
        case .unsupported: "Screen snapshots need macOS 14 or later."
        case .captureFailed: "Sona could not take the snapshot. Try again."
        }
    }
}

private func snapshotError(_ error: Error) -> String {
    (error as? CoreError)?.remote(as: SnapshotProblem.self)?.message ?? error.localizedDescription
}

private func openSnapshotPermissionSettings() {
    guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") else { return }
    NSWorkspace.shared.open(url)
}

/// One observer per core. Image bytes stay in the view showing them, not in
/// this app-lifetime store, so closing a meeting releases its pictures.
@MainActor @Observable
private final class MeetingSnapshotStore {
    private static var stores: [ObjectIdentifier: MeetingSnapshotStore] = [:]

    static func shared(_ core: Core) -> MeetingSnapshotStore {
        let key = ObjectIdentifier(core)
        if let store = stores[key] { return store }
        let store = MeetingSnapshotStore(core)
        stores[key] = store
        return store
    }

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var reloadAgain: Set<MeetingSessionId> = []
    var lists: [MeetingSessionId: [MeetingScreenSnapshot]] = [:]
    var statuses: [MeetingSessionId: MeetingScreenSnapshotStatus] = [:]
    var errors: [MeetingSessionId: String] = [:]
    var loading: Set<MeetingSessionId> = []
    var taking: Set<MeetingSessionId> = []
    var changing: Set<MeetingSessionId> = []
    var enabled = false
    var settingsLoaded = false
    var settingsSaving = false
    var settingsError: String?

    private init(_ core: Core) {
        self.core = core
        core.observe("meeting:snapshot-changed") { [weak self] data in
            guard let self,
                  let event = try? Core.payload(data) as MeetingEventPayload,
                  let sessionId = event.sessionId,
                  self.lists[sessionId] != nil || self.statuses[sessionId] != nil || self.loading.contains(sessionId)
            else { return }
            Task { await self.reload(sessionId) }
        }
        core.observe("settings-changed") { [weak self] _ in
            guard let self, self.settingsLoaded else { return }
            Task { await self.loadSettings() }
        }
        core.observe("meeting:removed") { [weak self] data in
            guard let self,
                  let event = try? Core.payload(data) as MeetingEventPayload,
                  let sessionId = event.sessionId
            else { return }
            self.lists.removeValue(forKey: sessionId)
            self.statuses.removeValue(forKey: sessionId)
            self.errors.removeValue(forKey: sessionId)
        }
    }

    func reload(_ sessionId: MeetingSessionId) async {
        guard loading.insert(sessionId).inserted else {
            reloadAgain.insert(sessionId)
            return
        }
        defer { loading.remove(sessionId) }
        repeat {
            reloadAgain.remove(sessionId)
            do {
                let params = SnapshotSessionParams(sessionId: sessionId)
                let snapshots: [MeetingScreenSnapshot] = try await core.request("meeting_snapshot_list", params)
                let status: MeetingScreenSnapshotStatus = try await core.request("meeting_snapshot_status", params)
                lists[sessionId] = snapshots
                statuses[sessionId] = status
            } catch {
                errors[sessionId] = snapshotError(error)
            }
        } while reloadAgain.contains(sessionId)
    }

    func take(_ sessionId: MeetingSessionId) async {
        guard taking.insert(sessionId).inserted else { return }
        errors.removeValue(forKey: sessionId)
        defer { taking.remove(sessionId) }
        if !CGPreflightScreenCaptureAccess() {
            _ = CGRequestScreenCaptureAccess()
        }
        do {
            let _: MeetingScreenSnapshot = try await core.request(
                "meeting_snapshot_take", SnapshotSessionParams(sessionId: sessionId)
            )
            await reload(sessionId)
        } catch {
            errors[sessionId] = snapshotError(error)
        }
    }

    func remove(_ snapshot: MeetingScreenSnapshot) async -> Bool {
        guard changing.insert(snapshot.sessionId).inserted else { return false }
        errors.removeValue(forKey: snapshot.sessionId)
        defer { changing.remove(snapshot.sessionId) }
        do {
            try await core.request("meeting_snapshot_delete", SnapshotDeleteParams(
                sessionId: snapshot.sessionId, snapshotId: snapshot.snapshotId
            ))
            await reload(snapshot.sessionId)
            return true
        } catch {
            errors[snapshot.sessionId] = snapshotError(error)
            return false
        }
    }

    func automatic(_ sessionId: MeetingSessionId, enabled: Bool) async {
        guard changing.insert(sessionId).inserted else { return }
        errors.removeValue(forKey: sessionId)
        defer { changing.remove(sessionId) }
        do {
            let status: MeetingScreenSnapshotStatus = try await core.request(
                "meeting_snapshot_automatic_set", SnapshotAutomaticParams(sessionId: sessionId, enabled: enabled)
            )
            statuses[sessionId] = status
        } catch { errors[sessionId] = snapshotError(error) }
    }

    func loadSettings() async {
        do {
            let settings: SnapshotSettings = try await core.request("get_app_settings")
            enabled = settings.meetingScreenSnapshotsEnabled ?? false
            settingsLoaded = true
            settingsError = nil
        } catch { settingsError = snapshotError(error) }
    }

    func setEnabled(_ value: Bool) async {
        guard !settingsSaving else { return }
        settingsSaving = true
        defer { settingsSaving = false }
        if value && !CGPreflightScreenCaptureAccess() {
            _ = CGRequestScreenCaptureAccess()
        }
        do {
            try await core.request("change_meeting_screen_snapshots_enabled_setting", SnapshotSettingParams(enabled: value))
            enabled = value
            settingsError = nil
        } catch { settingsError = snapshotError(error) }
    }
}

struct MeetingSnapshotMenuItem: View {
    @Environment(AppModel.self) private var model
    let sessionId: MeetingSessionId

    var body: some View {
        let store = MeetingSnapshotStore.shared(model.core)
        Button(store.taking.contains(sessionId) ? "Taking snapshot…" : "Snapshot screen") {
            Task { await store.take(sessionId) }
        }
        .disabled(store.taking.contains(sessionId))
    }
}

/// The live call's small stack of pictures and its automatic-capture switch.
struct MeetingSnapshotStrip: View {
    @Environment(AppModel.self) private var model
    let sessionId: MeetingSessionId
    @State private var selected: MeetingScreenSnapshot?

    var body: some View {
        let store = MeetingSnapshotStore.shared(model.core)
        VStack(alignment: .leading, spacing: 10) {
            ErrorNote(store.errors[sessionId])
            if store.taking.contains(sessionId) { ProgressView("Taking snapshot…").controlSize(.small) }
            if let status = store.statuses[sessionId] {
                if status.automaticSetting {
                    HStack {
                        Text(statusText(status)).metaText()
                        Spacer()
                        Button(status.turnedOffForMeeting ? "Turn on for this meeting" : "Turn off for this meeting") {
                            Task { await store.automatic(sessionId, enabled: status.turnedOffForMeeting) }
                        }
                        .buttonStyle(.quiet)
                        .disabled(store.changing.contains(sessionId))
                    }
                }
                if !status.screenRecordingGranted && (status.automaticSetting || store.errors[sessionId] != nil) {
                    Button("Open Screen Recording settings", action: openSnapshotPermissionSettings)
                        .buttonStyle(.quiet)
                }
            }
            if let snapshots = store.lists[sessionId], !snapshots.isEmpty {
                ScrollView(.horizontal) {
                    LazyHStack(alignment: .top, spacing: 12) {
                        ForEach(snapshots) { snapshot in
                            SnapshotThumbnail(snapshot: snapshot, width: 156) { selected = snapshot }
                        }
                    }
                }
                .frame(height: 126)
            }
        }
        .task(id: sessionId) { await store.reload(sessionId) }
        .sheet(item: $selected) { snapshot in SnapshotViewer(snapshot: snapshot) }
    }

    private func statusText(_ status: MeetingScreenSnapshotStatus) -> String {
        if status.turnedOffForMeeting { return "Screen snapshots are off for this meeting." }
        if !status.screenRecordingGranted { return "Screen snapshots need Screen Recording access." }
        switch status.state {
        case "watching": return status.count == 0 ? "Waiting for the shared screen to hold still." : "Saving the shared screen when it changes."
        case "paused": return "Snapshots paused. Bring the meeting window onto this screen."
        case "limit_reached": return "Automatic snapshots are full. You can still take one yourself."
        case "denied": return "Screen snapshots need Screen Recording access."
        case "idle": return "Snapshots pause when recording pauses."
        default: return "Screen snapshots are off."
        }
    }
}

/// Pictures are beside the notes, with the time each was taken. They are not
/// an input to the notes: the current note engines only read text.
struct MeetingSnapshotsSection: View {
    @Environment(AppModel.self) private var model
    let sessionId: MeetingSessionId
    @State private var selected: MeetingScreenSnapshot?

    var body: some View {
        let store = MeetingSnapshotStore.shared(model.core)
        PageSection("Screen snapshots") {
            if store.loading.contains(sessionId) && store.lists[sessionId] == nil {
                ProgressView("Loading snapshots…").controlSize(.small)
            } else if let error = store.errors[sessionId] {
                ErrorNote(error)
                Button("Try again") {
                    store.errors.removeValue(forKey: sessionId)
                    Task { await store.reload(sessionId) }
                }.buttonStyle(.quiet)
            } else if let snapshots = store.lists[sessionId], !snapshots.isEmpty {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 200), alignment: .leading)], alignment: .leading, spacing: 16) {
                    ForEach(snapshots) { snapshot in
                        SnapshotThumbnail(snapshot: snapshot, width: 200) { selected = snapshot }
                    }
                }
                Text("Notes are written from what was said, not these images.").metaText()
            } else {
                Text("No screen snapshots were taken. Use Snapshot screen during a meeting to save one.")
                    .bodyText(14, Theme.inkSecondary)
            }
        }
        .task(id: sessionId) { await store.reload(sessionId) }
        .sheet(item: $selected) { snapshot in SnapshotViewer(snapshot: snapshot) }
    }
}

private struct SnapshotThumbnail: View {
    @Environment(AppModel.self) private var model
    let snapshot: MeetingScreenSnapshot
    let width: CGFloat
    let open: () -> Void
    @State private var image: NSImage?
    @State private var error: String?
    @State private var loading = false

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Button(action: open) {
                ZStack {
                    Theme.inset
                    if let image {
                        Image(nsImage: image).resizable().scaledToFit()
                    } else if error != nil {
                        Image(systemName: "photo.badge.exclamationmark").foregroundStyle(Theme.inkSecondary)
                    } else {
                        ProgressView().controlSize(.small)
                    }
                }
                .frame(width: width, height: width * 0.56)
                .clipShape(RoundedRectangle(cornerRadius: Theme.radiusKey))
                .overlay(RoundedRectangle(cornerRadius: Theme.radiusKey).strokeBorder(Theme.border))
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Open screen snapshot at \(snapshot.offsetNs.meetingOffsetClock)")
            .contextMenu {
                Button("Delete snapshot", role: .destructive) {
                    Task { _ = await MeetingSnapshotStore.shared(model.core).remove(snapshot) }
                }
            }
            Text(snapshot.offsetNs.meetingOffsetClock)
                .font(TypeScale.mono(12)).foregroundStyle(Theme.inkSecondary)
        }
        .task(id: snapshot.snapshotId) { await load() }
        .help(error ?? "Open snapshot")
    }

    private func load() async {
        guard !loading else { return }
        loading = true
        defer { loading = false }
        do {
            let result: MeetingScreenSnapshotImage = try await model.core.request("meeting_snapshot_image", SnapshotImageParams(
                sessionId: snapshot.sessionId, snapshotId: snapshot.snapshotId, size: "thumbnail"
            ))
            guard let data = Data(base64Encoded: result.pngBase64), let decoded = NSImage(data: data) else {
                error = "This snapshot could not be opened."
                return
            }
            image = decoded
            error = nil
        } catch { self.error = snapshotError(error) }
    }
}

private struct SnapshotViewer: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let snapshot: MeetingScreenSnapshot
    @State private var image: NSImage?
    @State private var error: String?
    @State private var loading = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("Screen snapshot · \(snapshot.offsetNs.meetingOffsetClock)").headlineText()
                Spacer()
                Button("Done") { dismiss() }.buttonStyle(.primary).keyboardShortcut(.cancelAction)
            }
            if let image {
                Image(nsImage: image).resizable().scaledToFit().frame(maxWidth: .infinity, maxHeight: .infinity)
                    .accessibilityLabel("Meeting screen at \(snapshot.offsetNs.meetingOffsetClock)")
            } else if let error {
                ErrorNote(error)
                Button("Try again") { Task { await load() } }.buttonStyle(.quiet)
                Spacer()
            } else {
                ProgressView("Opening snapshot…").frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            ErrorNote(MeetingSnapshotStore.shared(model.core).errors[snapshot.sessionId])
            Button("Delete snapshot", role: .destructive) {
                Task {
                    if await MeetingSnapshotStore.shared(model.core).remove(snapshot) { dismiss() }
                }
            }
            .buttonStyle(.quiet)
            .disabled(MeetingSnapshotStore.shared(model.core).changing.contains(snapshot.sessionId))
        }
        .padding(24)
        .frame(minWidth: 640, idealWidth: 920, minHeight: 440, idealHeight: 640)
        .background(Theme.page)
        .task(id: snapshot.snapshotId) { await load() }
    }

    private func load() async {
        guard !loading else { return }
        loading = true
        error = nil
        defer { loading = false }
        do {
            let result: MeetingScreenSnapshotImage = try await model.core.request("meeting_snapshot_image", SnapshotImageParams(
                sessionId: snapshot.sessionId, snapshotId: snapshot.snapshotId, size: "full"
            ))
            guard let data = Data(base64Encoded: result.pngBase64), let decoded = NSImage(data: data) else {
                error = "This snapshot could not be opened."
                return
            }
            image = decoded
        } catch { self.error = snapshotError(error) }
    }
}

struct MeetingSnapshotSettingsSection: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let store = MeetingSnapshotStore.shared(model.core)
        PageSection("Screen snapshots") {
            Card {
                ToggleRow(
                    title: "Take snapshots of shared screens",
                    detail: "Always get consent before taking snapshots. Sona saves the meeting window when it changes. Images stay on this Mac and are included when you export the meeting.",
                    isOn: Binding(get: { store.enabled }, set: { value in Task { await store.setEnabled(value) } })
                )
                .disabled(!store.settingsLoaded || store.settingsSaving)
            }
            if !store.settingsLoaded && store.settingsError == nil {
                ProgressView("Loading snapshot settings…").controlSize(.small)
            }
            ErrorNote(store.settingsError)
            if store.settingsError != nil {
                Button("Try again") { Task { await store.loadSettings() } }.buttonStyle(.quiet)
            }
            if store.enabled {
                Button("Open Screen Recording settings", action: openSnapshotPermissionSettings).buttonStyle(.quiet)
            }
        }
        .task { await store.loadSettings() }
    }
}
