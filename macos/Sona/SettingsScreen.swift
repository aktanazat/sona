import SwiftUI

/// Settings: a tab strip across the top, one tab of cards below.
struct SettingsScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 0) {
            tabs
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    tab
                }
                .frame(maxWidth: 880, alignment: .leading)
                .padding(.horizontal, Theme.margin)
                .padding(.top, 32)
                .padding(.bottom, 64)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .scrollIndicators(.never)
        }
    }

    private var tabs: some View {
        HStack(spacing: 28) {
            ForEach(SettingsPlace.allCases) { place in
                let current = model.settingsPlace == place
                Button {
                    model.settingsPlace = place
                    model.selectedMode = nil
                } label: {
                    Text(place.title)
                        .font(TypeScale.label(15))
                        .foregroundStyle(current ? Theme.ink : Theme.inkSecondary)
                        .padding(.vertical, 14)
                        .overlay(alignment: .bottom) {
                            Rectangle().fill(current ? Theme.ink : .clear).frame(height: 2)
                        }
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            Spacer()
        }
        .padding(.horizontal, Theme.margin)
        .padding(.top, 8)
        .overlay(alignment: .bottom) { Hairline() }
    }

    @ViewBuilder
    private var tab: some View {
        @Bindable var model = model
        switch model.settingsPlace {
        case .essentials:
            SettingsIntro("Essentials", fact: "How Sona hears you and where it starts.")
            Card {
                CardRow {
                    Text("Transcribe shortcut").bodyText()
                } trailing: {
                    Shortcut(model.pushToTalk)
                }
                ToggleRow(title: "Push to talk", detail: "Hold, speak, release. Off means press once to start and once to stop.", isOn: $model.pasteAutomatically)
                ChoiceRow(title: "Microphone", value: $model.inputDevice, choices: ["MacBook Pro Microphone", "AirPods Pro", "Scarlett 2i2"])
                ChoiceRow(title: "Language", value: $model.language, choices: ["English", "German", "Spanish", "French", "Japanese"])
                ToggleRow(title: "Sounds", detail: "A short tone when recording starts and stops.", isOn: $model.soundOnStart)
                ToggleRow(title: "Launch at login", isOn: $model.launchAtLogin)
                ToggleRow(title: "Show in the menu bar", detail: "The dot shows what the microphone is doing.", isOn: $model.showInMenuBar)
                ToggleRow(title: "Floating pill while recording", detail: "The sound waves, above other windows, while you talk.", isOn: $model.hudPill)
                ToggleRow(title: "Notice when I join a meeting", detail: "Zoom, Meet, Teams and FaceTime. Sona asks before it records.", isOn: $model.detectMeetings)
                ToggleRow(title: "Announce recording in the call", detail: "Types a one-line disclosure into the call's chat.", isOn: $model.announceMeetings)
            }
            .padding(.bottom, 32)
            PageSection("Shortcuts") {
                Card {
                    ShortcutRow(title: "Toggle recording", detail: "Press once to start, once to stop.", value: model.toggleShortcut)
                    ShortcutRow(title: "Record the current call", detail: nil, value: model.meetingShortcut)
                    ShortcutRow(title: "Search", detail: nil, value: "⌘ K")
                    ShortcutRow(title: "Cancel", detail: "Drop what was heard since the key went down.", value: "Esc")
                }
            }
        case .models:
            ModelsTab()
        case .modes:
            ModesTab()
        case .vocabulary:
            SettingsIntro("Vocabulary", fact: "Words Sona kept getting wrong until you told it how they are spelled.")
            Card {
                ForEach(SampleData.vocabulary) { entry in
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(entry.word).bodyText()
                            Text("was heard as “\(entry.heardAs)”").metaText()
                        }
                    } trailing: {
                        HStack(spacing: 16) {
                            Text("\(entry.uses) uses").metaText()
                            Button("Remove") {}.buttonStyle(.quiet)
                        }
                    }
                }
                CardRow {
                    InputField(prompt: "Add a word", text: .constant("")).frame(width: 320)
                } trailing: {
                    Button("Add") {}.buttonStyle(.compact)
                }
            }
        case .prompts:
            SettingsIntro("Prompts", fact: "Questions you ask of a meeting often enough to keep.")
            Card {
                ForEach(SampleData.prompts) { prompt in
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(prompt.name).bodyText()
                            Text(prompt.text).metaText()
                        }
                    } trailing: {
                        HStack(spacing: 16) {
                            Text("\(prompt.runs) runs").metaText()
                            Button("Edit") {}.buttonStyle(.compact)
                        }
                    }
                }
            }
        case .workflows:
            SettingsIntro("Workflows", fact: "What happens on its own when a meeting ends.")
            Card {
                ForEach(SampleData.workflows) { workflow in
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(workflow.name).bodyText()
                            Text("\(workflow.trigger) · \(workflow.steps) steps").metaText()
                        }
                    } trailing: {
                        Toggle("", isOn: .constant(workflow.enabled))
                            .labelsHidden()
                            .toggleStyle(.switch)
                            .tint(Theme.accent)
                    }
                }
            }
        case .agents:
            SettingsIntro("Agents", fact: "Coding agents on this Mac may read meetings you share with them, over a local socket, never the network.")
            Card {
                ForEach(SampleData.agents) { agent in
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(agent.name).bodyText()
                            Text(agent.transport).metaText()
                        }
                    } trailing: {
                        HStack(spacing: 16) {
                            Text(agent.lastSeen).metaText()
                            Button("Revoke") {}.buttonStyle(.quiet)
                        }
                    }
                }
            }
        case .advanced:
            SettingsIntro("Advanced", fact: "Sync, storage, recovery, and what to send when something is wrong.")
            Card {
                ToggleRow(title: "Sync history between your Macs", detail: "Off. End-to-end encrypted with a key that never leaves your devices.", isOn: $model.cloudSync)
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Devices").bodyText()
                        Text("This Mac only.").metaText()
                    }
                } trailing: {
                    Button("Pair a phone") {}.buttonStyle(.compact)
                }
                ToggleRow(title: "Check for updates", detail: "Once a day. Nothing else is sent.", isOn: $model.autoUpdate)
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Recordings folder").bodyText()
                        Text("~/Library/Application Support/Sona/recordings").metaText()
                    }
                } trailing: {
                    Button("Open") {}.buttonStyle(.compact)
                }
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Trash").bodyText()
                        Text("Two meetings, recoverable for 30 days.").metaText()
                    }
                } trailing: {
                    Button("Restore") {}.buttonStyle(.compact)
                }
            }
            .padding(.bottom, 32)
            PageSection("Debug") {
                Card {
                    ToggleRow(title: "Verbose logs", detail: "Off unless you are chasing a bug.", isOn: $model.verboseLogs)
                    CardRow {
                        Text("Logs").bodyText()
                    } trailing: {
                        Button("Open folder") {}.buttonStyle(.compact)
                    }
                    CardRow {
                        Text("Version 1.1.0 · Whisper Large v3 · macOS 26.6").bodyText(15, Theme.inkSecondary)
                    } trailing: {
                        Button("Copy") {}.buttonStyle(.quiet)
                    }
                }
            }
        }
    }
}

/// The head of a settings tab: the tab's name and one line of plain fact.
struct SettingsIntro: View {
    let title: String
    let fact: String

    init(_ title: String, fact: String) {
        self.title = title
        self.fact = fact
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).headlineText()
            Text(fact).bodyText(15, Theme.inkSecondary)
        }
        .padding(.bottom, 20)
    }
}

private struct ChoiceRow: View {
    let title: String
    @Binding var value: String
    let choices: [String]

    var body: some View {
        CardRow {
            Text(title).bodyText()
        } trailing: {
            Menu {
                ForEach(choices, id: \.self) { choice in
                    Button(choice) { value = choice }
                }
            } label: {
                HStack(spacing: 8) {
                    Text(value)
                    Image(systemName: "chevron.down").font(.system(size: 10, weight: .semibold))
                }
            }
            .menuStyle(.button)
            .buttonStyle(.compact)
            .menuIndicator(.hidden)
        }
    }
}

private struct ShortcutRow: View {
    let title: String
    let detail: String?
    let value: String

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).bodyText()
                if let detail {
                    Text(detail).metaText()
                }
            }
        } trailing: {
            Shortcut(value)
        }
    }
}
