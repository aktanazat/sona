import SwiftUI

/// Settings: a tab strip across the top, one tab below. Each tab is a slice's
/// own page; the ones that come as a column get the page and the title here.
struct SettingsScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 0) {
            tabs
            tab
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private var tabs: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 24) {
                ForEach(SettingsPlace.allCases) { place in
                    let current = model.settingsPlace == place
                    Button {
                        model.settingsPlace = place
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
            }
            .padding(.horizontal, Theme.margin)
            .padding(.top, 8)
        }
        .scrollIndicators(.never)
        .overlay(alignment: .bottom) { Hairline() }
    }

    @ViewBuilder
    private var tab: some View {
        switch model.settingsPlace {
        case .essentials:
            EssentialsView(store: model.settings) {
                MeetingDetectionEssentials(store: model.meetingSettings)
            }
        case .dictation:
            DictationSettingsView(store: model.settings, onOpenModes: { model.showSettings(.modes) }) {
                SettingsCaptureRows(store: model.settings)
            }
        case .models:
            Page {
                ModelsTab()
                PageSection("Language models") {
                    ProvidersView(store: model.providers)
                }
            }
        case .modes:
            Page {
                PageTitle("Modes", subtitle: "A mode is a prompt and the apps it applies to.")
                ModesView(
                    store: model.modes,
                    openVocabulary: { model.showSettings(.vocabulary) },
                    openPrivacy: { model.showSettings(.privacy) },
                    openProviders: { model.showSettings(.models) })
            }
        case .vocabulary:
            Page {
                VocabularyView(store: model.vocabulary)
            }
        case .prompts:
            PromptsView(store: model.prompts)
        case .workflows:
            WorkflowsView(store: model.workflows)
        case .meetings:
            Page {
                PageTitle("Meetings", subtitle: "When Sona notices a call, what it keeps, and what it does after.")
                MeetingSettingsView(store: model.meetingSettings, openPrompts: { model.showSettings(.prompts) })
            }
        case .agents:
            Page {
                PageTitle("Agents", subtitle: "Coding agents on this Mac may read meetings you share with them, over a local socket, never the network.")
                AgentBridgeView(store: model.agents)
                AgentPairingView(store: model.pairing)
            }
        case .sync:
            Page {
                PageTitle("Sync", subtitle: "Meetings between your devices, end to end encrypted with a key that never leaves them. Pair a phone by pasting the code it shows.")
                CloudSyncView(store: model.cloudSync, openMeeting: model.openMeeting)
            }
        case .privacy:
            PrivacyView(store: model.privacy)
        case .importing:
            ImportView(store: model.imports, openLink: model.open(link:))
        case .documents:
            DocumentsView(store: model.documents)
        case .about:
            AboutView(store: model.about)
        case .debug:
            DebugView(store: model.debug) {
                AnyView(SettingsCaptureRows(store: model.settings))
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
