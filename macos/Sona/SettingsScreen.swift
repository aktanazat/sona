import SwiftUI

/// Settings: the page the sidebar chose. Each page is a slice's own; the
/// ones that come as a column get the page and the title here.
struct SettingsScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        tab.frame(maxWidth: .infinity, maxHeight: .infinity)
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
            WorkflowsView(
                store: model.workflows,
                openMeeting: model.openMeeting,
                openDocuments: { model.showSettings(.documents) })
        case .meetings:
            Page {
                PageTitle("Meetings", subtitle: "When Sona notices a call, what it keeps, and what it does after.")
                MeetingSettingsView(store: model.meetingSettings, openPrompts: { model.showSettings(.prompts) })
                MeetingPrepSettingsView(store: model.prep)
            }
        case .agents:
            Page {
                PageTitle("Agents", subtitle: "Coding agents on this Mac may read meetings you share with them, over a local socket, never the network.") {
                    Button(model.agents.loading ? "Refreshing…" : "Refresh") { model.agents.refreshAll() }
                        .buttonStyle(.compact)
                        .disabled(model.agents.loading)
                }
                AgentBridgeView(store: model.agents)
                AgentPairingView(store: model.pairing)
            }
        case .sync:
            Page {
                PageTitle("Sync", subtitle: "Meetings between your devices, end to end encrypted with a key that never leaves them. Pair a phone by pasting the code it shows.")
                CloudSyncView(store: model.cloudSync, openMeeting: model.openMeeting)
            }
        case .connections:
            ConnectionsView(store: model.connections)
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
