import AppKit
import SwiftUI

struct MeetingDisclosureSettingsView: View {
    let store: MeetingSettingsStore
    @State private var enabled = false
    @State private var message = ""
    @State private var edited = false

    private var valid: Bool {
        let text = message.trimmingCharacters(in: .whitespacesAndNewlines)
        return !text.isEmpty && text.unicodeScalars.count <= 1_000 && !text.contains("\0")
    }

    var body: some View {
        PageSection("Recording notice") {
            Card {
                if store.settingsRead {
                    ToggleRow(
                        title: "Post a notice when recording starts",
                        detail: "Off by default. Posts once only when Sona can identify the joined meeting, its empty chat box, and Send. Your draft is never replaced.",
                        isOn: Binding(get: { enabled }, set: { enabled = $0; edited = true }))
                    .disabled(store.disclosureSaving)
                    VStack(alignment: .leading, spacing: 12) {
                        Text("Notice text").font(TypeScale.label(14)).foregroundStyle(Theme.ink)
                        TextEditor(text: Binding(get: { message }, set: { message = $0; edited = true }))
                            .font(TypeScale.body(14))
                            .foregroundStyle(Theme.ink)
                            .scrollContentBackground(.hidden)
                            .frame(minHeight: 80, maxHeight: 120)
                            .padding(8)
                            .background(Theme.page, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
                            .accessibilityLabel("Recording notice text")
                            .disabled(store.disclosureSaving)
                        Text("Automatic posting is limited to a clearly identified English Zoom meeting chat addressed to Everyone. Other apps and unclear chats offer a copy button. A recording notice does not replace the participants' consent.")
                            .bodyText(13, Theme.inkSecondary)
                        HStack {
                            Text(!valid ? "Use 1–1,000 characters." : edited ? "Not saved yet. Applies to new recordings." : "Saved. The detected-meeting prompt can turn this off for one meeting.")
                                .bodyText(13, Theme.inkSecondary)
                            Spacer()
                            Button(store.disclosureSaving ? "Saving…" : "Save notice") {
                                Task {
                                    if await store.setDisclosure(enabled: enabled, message: message) {
                                        edited = false
                                        seed()
                                    }
                                }
                            }
                            .buttonStyle(.secondary)
                            .disabled(!edited || !valid || store.disclosureSaving)
                        }
                    }
                    .padding(20)
                } else {
                    CardLine("Reading the recording notice…")
                    if store.error != nil {
                        ActionRow(title: "The notice settings could not be read.", button: "Try again") {
                            Task { await store.loadSettings() }
                        }
                    }
                }
            }
        }
        .task(id: store.settingsRead) { if !edited { seed() } }
        .onChange(of: store.settings.disclosureMessage) { _, _ in if !edited { seed() } }
        .onChange(of: store.settings.disclosureEnabled) { _, _ in if !edited { seed() } }
    }

    private func seed() {
        enabled = store.settings.disclosureEnabled
        message = store.settings.disclosureMessage
    }
}

/// The same outcome and manual route on the live screen and the small panel.
struct MeetingDisclosureStatus: View {
    let store: MeetingLiveStore
    let disclosure: MeetingConsentDisclosure
    @State private var showingNotice = false
    @State private var copied = false

    var body: some View {
        if let outcome = disclosure.outcomeLine {
            VStack(alignment: .leading, spacing: 6) {
                Text(outcome).bodyText(12, Theme.inkSecondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let text = disclosure.noticeText {
                    HStack(spacing: 12) {
                        Button(copied ? "Copied" : "Copy notice") {
                            NSPasteboard.general.clearContents()
                            copied = NSPasteboard.general.setString(text, forType: .string)
                        }
                        .buttonStyle(QuietButton(compact: true))
                        Button("Read notice") { showingNotice = true }
                            .buttonStyle(QuietButton(compact: true))
                            .popover(isPresented: $showingNotice) {
                                Text(text).bodyText(14).textSelection(.enabled).padding(20).frame(width: 320)
                            }
                    }
                }
            }
            .onChange(of: disclosure.noticeText) { _, _ in copied = false }
        }
    }
}
