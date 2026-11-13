import AppKit
import SwiftUI

/// Copies the link teammates open to follow the recording meeting's doc,
/// board and transcript, and to edit the doc and the board. The key is in
/// the link, so only someone with it can read what the meeting shares.
struct LiveShareLinkButton: View {
    let sessionId: MeetingSessionId
    @Environment(AppModel.self) private var model
    @State private var copying = false
    @State private var copied = false
    @State private var failure: String?

    var body: some View {
        Button(action: copy) {
            Label(copied ? "Copied" : "Copy link", systemImage: copied ? "checkmark" : "link")
        }
        .buttonStyle(.secondary)
        .disabled(copying)
        .help("Anyone with the link can follow this meeting and edit its doc and board.")
        .popover(isPresented: Binding(get: { failure != nil }, set: { if !$0 { failure = nil } }),
                 arrowEdge: .bottom) {
            Text(failure ?? "")
                .bodyText(13, Theme.inkSecondary)
                .frame(width: 240, alignment: .leading)
                .padding(12)
        }
        .task(id: copied) {
            guard copied else { return }
            do { try await Task.sleep(for: .seconds(2)) } catch { return }
            copied = false
        }
    }

    private func copy() {
        copying = true
        Task {
            defer { copying = false }
            do {
                let link: String = try await model.core.request(
                    "meeting_live_share_link", ["sessionId": sessionId])
                let board = NSPasteboard.general
                board.clearContents()
                board.setString(link, forType: .string)
                copied = true
            } catch {
                failure = switch (error as? CoreError)?.remote(as: CloudSyncErrorKind.self) {
                case .setupRequired: "Turn on cloud sync in Settings to share a link."
                case .conflict: "A link can be made only while the meeting records."
                case let kind?: kind.guidance
                case nil: "The link could not be made. Try again."
                }
            }
        }
    }
}
