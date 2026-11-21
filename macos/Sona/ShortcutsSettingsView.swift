import SwiftUI

/// Every key Sona listens for, each editable in place. The page lists what
/// the core carries, so a binding a later version adds shows up here without
/// a change to this page.
struct ShortcutsSettingsView: View {
    let store: SettingsStore

    /// Bindings the sections below place by hand; everything else the core
    /// carries is a mode's or lands under Other.
    private static let placed: Set<String> = [
        "transcribe", "cancel", "send", "command",
        "work_quick_entry", "work_ramble", "meeting_attach_window",
    ]

    var body: some View {
        Page {
            PageTitle("Shortcuts", subtitle: "") {
                EmptyView()
            }
            ErrorNote(store.error)
            PageSection("Dictation") {
                Card {
                    ShortcutCaptureRow(store: store, id: "transcribe")
                    ToggleRow(
                        title: "Push to talk",
                        detail: BindingCopy.pushToTalkDetail(store.settings.pushToTalk),
                        isOn: Binding(
                            get: { store.settings.pushToTalk },
                            set: { value in Task { await store.setPushToTalk(value) } }
                        )
                    )
                    .disabled(store.isBusy("push_to_talk"))
                    ShortcutCaptureRow(store: store, id: "cancel")
                    ShortcutCaptureRow(store: store, id: "send")
                    if store.settings.commandModeEnabled {
                        ShortcutCaptureRow(store: store, id: "command")
                    }
                }
            }
            if !modeIds.isEmpty {
                PageSection("Modes") {
                    Card {
                        ForEach(modeIds, id: \.self) { id in
                            ShortcutCaptureRow(store: store, id: id)
                        }
                    }
                }
            }
            PageSection("Tasks") {
                Card {
                    ShortcutCaptureRow(store: store, id: "work_quick_entry")
                    ShortcutCaptureRow(store: store, id: "work_ramble")
                }
            }
            PageSection("Meetings") {
                Card {
                    ShortcutCaptureRow(store: store, id: "meeting_attach_window")
                }
            }
            if !otherIds.isEmpty {
                PageSection("Other") {
                    Card {
                        ForEach(otherIds, id: \.self) { id in
                            ShortcutCaptureRow(store: store, id: id)
                        }
                    }
                }
            }
        }
    }

    private var modeIds: [String] {
        store.settings.bindings.values
            .filter { $0.id.hasPrefix("mode/") }
            .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
            .map(\.id)
    }

    private var otherIds: [String] {
        store.settings.bindings.values
            .filter { !Self.placed.contains($0.id) && !$0.id.hasPrefix("mode/") }
            .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
            .map(\.id)
    }
}
