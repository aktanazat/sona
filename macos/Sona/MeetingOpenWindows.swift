import AppKit
import SwiftUI

/// `MeetingOpenWindow`: a window that was in front while the meeting
/// recorded, offered until it is attached as one of the meeting's links.
struct MeetingOpenWindow: Decodable, Identifiable {
    let itemId: String
    let label: String
    let appName: String?
    let documentPath: String?
    let url: String?
    let host: String?
    let firstOffsetNs: Int64
    let lastOffsetNs: Int64
    let attachedAtUtcMs: Int64?
    var id: String { itemId }

    /// Where Open takes him: the file, or the page when its full address
    /// was kept.
    var target: URL? {
        if let documentPath { return URL(fileURLWithPath: documentPath) }
        return url.flatMap(URL.init(string:))
    }

    var meta: String {
        let when = firstOffsetNs == lastOffsetNs
            ? firstOffsetNs.meetingOffsetClock
            : "\(firstOffsetNs.meetingOffsetClock) to \(lastOffsetNs.meetingOffsetClock)"
        return [host ?? appName, when].compactMap { $0 }.joined(separator: " · ")
    }
}

private enum OpenWindowAttachment: String, Decodable {
    case document, link
}

private struct OpenWindowParams: Encodable {
    let sessionId: MeetingSessionId
    let itemId: String
}

/// What was open while this meeting recorded. Attaching keeps a window as
/// one of the meeting's documents or links; Dismiss deletes the rest, and
/// whatever is left is deleted after 7 days.
struct MeetingOpenWindowsCard: View {
    let core: Core
    let store: MeetingsStore
    let session: MeetingSessionSnapshot
    @State private var windows: [MeetingOpenWindow] = []
    @State private var failure: String?
    @State private var busy = false

    private var offered: [MeetingOpenWindow] { windows.filter { $0.attachedAtUtcMs == nil } }
    private var links: [MeetingOpenWindow] { windows.filter { $0.attachedAtUtcMs != nil } }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            if store.editable, !offered.isEmpty { offer }
            ForEach(links) { link in row(link) }
            if let failure { Text(failure).metaText(Theme.live) }
        }
        .padding(.bottom, windows.isEmpty && failure == nil ? 0 : 16)
        .disabled(busy)
        .task(id: "\(session.sessionId):\(session.revision)") { await load() }
    }

    private var offer: some View {
        VStack(alignment: .leading, spacing: 10) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Open during this call").bodyText(14)
                Text("What you don't attach is deleted after 7 days.").metaText()
            }
            ForEach(offered) { window in
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(window.label).bodyText(13).lineLimit(1)
                        Text(window.meta).metaText().lineLimit(1)
                    }
                    Spacer(minLength: 8)
                    Button("Attach") { attach([window]) }
                        .buttonStyle(QuietButton(compact: true))
                        .accessibilityLabel("Attach \(window.label)")
                }
            }
            HStack(spacing: 12) {
                Button("Dismiss") { dismiss() }.buttonStyle(.quiet)
                if offered.count > 1 {
                    Button("Attach all") { attach(offered) }.buttonStyle(.quiet)
                }
            }
        }
    }

    private func row(_ link: MeetingOpenWindow) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: link.documentPath == nil ? "link" : "doc")
                .foregroundStyle(Theme.inkSecondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(link.label).bodyText(14).lineLimit(1)
                Text(link.meta).metaText().lineLimit(1)
            }
            Spacer(minLength: 8)
            if let target = link.target {
                Button("Open") { NSWorkspace.shared.open(target) }
                    .buttonStyle(.quiet)
                    .accessibilityLabel("Open \(link.label)")
            }
            if store.editable {
                Button("Remove") { remove(link) }
                    .buttonStyle(.quiet)
                    .accessibilityLabel("Remove \(link.label)")
            }
        }
    }

    private func attach(_ chosen: [MeetingOpenWindow]) {
        busy = true
        Task {
            defer { busy = false }
            var documents = false
            do {
                for window in chosen {
                    let kept: OpenWindowAttachment = try await core.request(
                        "meeting_open_window_attach",
                        OpenWindowParams(sessionId: session.sessionId, itemId: window.itemId))
                    documents = documents || kept == .document
                }
                failure = nil
            } catch {
                failure = "This window could not be attached."
            }
            if documents { store.documentsChanged() }
            await load()
        }
    }

    private func remove(_ link: MeetingOpenWindow) {
        busy = true
        Task {
            defer { busy = false }
            do {
                try await core.request(
                    "meeting_open_window_remove",
                    OpenWindowParams(sessionId: session.sessionId, itemId: link.itemId))
                failure = nil
            } catch {
                failure = "This link could not be removed."
            }
            await load()
        }
    }

    private func dismiss() {
        busy = true
        Task {
            defer { busy = false }
            do {
                try await core.request("meeting_open_windows_dismiss", ["sessionId": session.sessionId])
                failure = nil
            } catch {
                failure = "The list could not be cleared."
            }
            await load()
        }
    }

    private func load() async {
        do {
            let result: [MeetingOpenWindow] = try await core.request(
                "meeting_open_windows", ["sessionId": session.sessionId])
            guard !Task.isCancelled else { return }
            windows = result
            failure = nil
        } catch {
            guard !Task.isCancelled else { return }
            failure = "The windows open during this meeting could not be read."
        }
    }
}

private struct OpenWindowsSettings: Decodable {
    let meetingOpenWindowsEnabled: Bool?
}

private struct OpenWindowsSettingParams: Encodable {
    let enabled: Bool
}

struct MeetingOpenWindowsSettingsSection: View {
    @Environment(AppModel.self) private var model
    @State private var enabled = true
    @State private var loaded = false
    @State private var saving = false
    @State private var failure: String?

    var body: some View {
        PageSection("Open windows") {
            Card {
                ToggleRow(
                    title: "Note the windows open during a meeting",
                    detail: "Review offers them to attach as documents or links. The list stays on this Mac. What you don't attach is deleted after 7 days, or right away when this is off.",
                    isOn: Binding(get: { enabled }, set: { value in Task { await set(value) } })
                )
                .disabled(!loaded || saving)
                ShortcutCaptureRow(store: model.settings, id: "meeting_attach_window")
            }
            ErrorNote(failure)
        }
        .task { await load() }
    }

    private func load() async {
        do {
            let settings: OpenWindowsSettings = try await model.core.request("get_app_settings")
            enabled = settings.meetingOpenWindowsEnabled ?? true
            loaded = true
            failure = nil
        } catch {
            failure = "This setting could not be read."
        }
    }

    private func set(_ value: Bool) async {
        guard !saving else { return }
        saving = true
        defer { saving = false }
        do {
            try await model.core.request(
                "change_meeting_open_windows_enabled_setting", OpenWindowsSettingParams(enabled: value))
            enabled = value
            failure = nil
        } catch {
            failure = "This setting could not be saved."
        }
    }
}

/// `MeetingWindowAttachedEvent`: what the attach-window shortcut took.
private struct WindowAttachedEvent: Decodable {
    enum Outcome: String, Decodable {
        case document, link
        case notRecording = "not_recording"
        case passwordField = "password_field"
        case privateWindow = "private_window"
        case nothingToAttach = "nothing_to_attach"
        case accessibilityDenied = "accessibility_denied"
        case failed
    }

    let outcome: Outcome
    let label: String?
    let appName: String?

    var headline: String {
        switch outcome {
        case .document, .link: "Attached \(label ?? appName ?? "the window")"
        default: "Nothing attached"
        }
    }

    var detail: String {
        switch outcome {
        case .document: "Added to the meeting's documents."
        case .link: "Kept with the meeting as a link."
        case .notRecording: "Attach works while a meeting records."
        case .passwordField: "A password field is in focus."
        case .privateWindow: "Private windows are never attached."
        case .nothingToAttach: "This window has no title, file or page."
        case .accessibilityDenied: "Sona needs Accessibility access to read the window."
        case .failed: "The window could not be attached."
        }
    }
}

/// The small card the attach-window shortcut shows for a moment at the top
/// right of the screen.
@MainActor @Observable
final class WindowAttachedCard {
    fileprivate private(set) var event: WindowAttachedEvent?
    @ObservationIgnored private lazy var panel = FloatingPanel(WindowAttachedCardView(card: self))
    @ObservationIgnored private var dismissal: Task<Void, Never>?

    init(core: Core) {
        core.observe("meeting:window-attached") { [weak self] data in
            guard let self, let event = try? Core.payload(data) as WindowAttachedEvent else { return }
            self.show(event)
        }
    }

    private func show(_ event: WindowAttachedEvent) {
        self.event = event
        panel.show(at: .topTrailing)
        dismissal?.cancel()
        dismissal = Task { [weak self] in
            try? await Task.sleep(for: .seconds(2.5))
            guard !Task.isCancelled else { return }
            self?.panel.hide()
        }
    }
}

private struct WindowAttachedCardView: View {
    let card: WindowAttachedCard

    var body: some View {
        if let event = card.event {
            VStack(alignment: .leading, spacing: 4) {
                Text(event.headline).bodyText(13).lineLimit(1)
                Text(event.detail).metaText()
            }
            .padding(PanelSurface.inset)
            .frame(width: PanelSurface.width, alignment: .leading)
            .panelSurface(docked: false)
            .padding(8)
        }
    }
}
