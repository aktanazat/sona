import SwiftUI

/// The Settings window: every page in a sidebar, the chosen one beside it.
struct SettingsWindow: View {
    @Environment(AppModel.self) private var model
    @State private var paneWidth: CGFloat = 0

    var body: some View {
        NavigationSplitView {
            List(SettingsPlace.allCases, selection: Binding(
                get: { model.settingsPlace },
                set: { if let page = $0 { model.settingsPlace = page } })
            ) { page in
                Text(page.title).tag(page)
            }
            .navigationSplitViewColumnWidth(Theme.sidebarWidth)
            .toolbar(removing: .sidebarToggle)
        } detail: {
            Group {
                if model.ready, model.onboarding.step == .done {
                    SettingsScreen()
                } else {
                    Text(model.ready ? "Finish setting up Sona first." : "Starting…")
                        .bodyText(13, Theme.inkSecondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .keepsWindowToolbar()
                }
            }
            .background(Theme.page.ignoresSafeArea())
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { paneWidth = $0 }
            .environment(\.paneWidth, paneWidth)
            .toolbar(removing: .title)
        }
        .frame(minWidth: 900, minHeight: 640)
        .navigationTitle(model.settingsPlace.title)
        .onAppear { model.windowOpened() }
        .onDisappear { model.windowClosed() }
    }
}

/// One settings page. Each page is a slice's own; the ones that come as a
/// column get the page and the title here.
struct SettingsScreen: View {
    @Environment(AppModel.self) private var model
    @State private var quicklink: QuicklinkEdit?

    var body: some View {
        tab.frame(maxWidth: .infinity, maxHeight: .infinity)
            .sheet(item: $quicklink) { QuicklinkEditor(store: model.quicklinks, id: $0.id) }
    }

    /// A meeting a settings page links to opens in the main window.
    private func openMeeting(_ id: MeetingSessionId) {
        model.reveal()
        model.openMeeting(id)
    }

    @ViewBuilder
    private var tab: some View {
        switch model.settingsPlace {
        case .profile:
            ProfileSettingsView()
        case .essentials:
            EssentialsView(store: model.settings, activeInput: model.activeDictationInput) {
                MeetingDetectionEssentials(store: model.meetingSettings)
            }
        case .shortcuts:
            ShortcutsSettingsView(store: model.settings)
        case .appearance:
            AppearanceView(store: model.about)
        case .dictation:
            DictationSettingsView(store: model.settings, onOpenModes: { model.showSettings(.modes) }) {
                SettingsCaptureRows(store: model.settings)
            }
        case .models:
            Page {
                ModelsTab()
                ProvidersView(store: model.providers)
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
                openMeeting: openMeeting,
                openDocuments: { model.showSettings(.documents) })
        case .quicklinks:
            QuicklinksView(store: model.quicklinks,
                           edit: { quicklink = QuicklinkEdit(id: $0) },
                           open: { model.openQuicklink($0) })
        case .meetings:
            Page {
                PageTitle("Meetings", subtitle: "When Sona notices a call, what it keeps, and what it does after.")
                MeetingSettingsView(store: model.meetingSettings, openPrompts: { model.showSettings(.prompts) })
                MeetingPrepSettingsView(store: model.prep)
            }
        case .agents:
            Page {
                PageTitle("Agents", subtitle: "Coding agents on this Mac can read meetings you share with them.") {
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
                CloudSyncView(store: model.cloudSync, openMeeting: openMeeting)
            }
        case .connections:
            ConnectionsView(store: model.connections)
        case .tasks:
            TasksSettingsView()
        case .privacy:
            PrivacyView(store: model.privacy)
        case .importing:
            ImportView(store: model.imports, openLink: model.open(link:))
        case .workImport:
            WorkImportView(store: model.workImport)
        case .documents:
            DocumentsView(store: model.documents)
        case .about:
            AboutView(store: model.about)
        case .debug:
            DebugView(store: model.debug)
        }
    }
}

private struct QuicklinkEdit: Identifiable {
    let id: UUID?
}
