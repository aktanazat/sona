import SwiftUI

/// Settings: a tab strip across the top, one tab below. Each tab is a slice's
/// own page; the ones that come as a column get the page and the title here.
struct SettingsScreen: View {
    @Environment(AppModel.self) private var model
    /// Which edges have a tab hidden past them, so only those edges fade.
    @State private var overflow = StripOverflow()

    var body: some View {
        VStack(spacing: 0) {
            tabs
            tab
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    /// Fifteen tabs outrun a narrow window. The strip scrolls, the chosen
    /// tab is brought into view whenever it changes (a "Meeting settings"
    /// button elsewhere chooses one too), and a fade at an edge says a tab
    /// is hidden past it.
    private var tabs: some View {
        ScrollViewReader { proxy in
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
                        .accessibilityAddTraits(current ? .isSelected : [])
                        .id(place)
                    }
                }
                .padding(.horizontal, Theme.margin)
                .padding(.top, 8)
            }
            .scrollIndicators(.never)
            .onScrollGeometryChange(for: StripOverflow.self) { geometry in
                let offset = geometry.contentOffset.x
                let end = geometry.contentSize.width - geometry.containerSize.width
                return StripOverflow(leading: offset > 1, trailing: offset < end - 1)
            } action: { _, edges in
                overflow = edges
            }
            .mask { edgeFade }
            .overlay(alignment: .bottom) { Hairline() }
            .onAppear { proxy.scrollTo(model.settingsPlace, anchor: .center) }
            .onChange(of: model.settingsPlace) { _, place in
                withAnimation(.snappy) { proxy.scrollTo(place, anchor: .center) }
            }
        }
    }

    private var edgeFade: some View {
        HStack(spacing: 0) {
            LinearGradient(
                colors: [.black.opacity(overflow.leading ? 0 : 1), .black],
                startPoint: .leading, endPoint: .trailing
            )
            .frame(width: Theme.margin)
            Rectangle()
            LinearGradient(
                colors: [.black, .black.opacity(overflow.trailing ? 0 : 1)],
                startPoint: .leading, endPoint: .trailing
            )
            .frame(width: Theme.margin)
        }
        .animation(.easeOut(duration: 0.15), value: overflow.leading)
        .animation(.easeOut(duration: 0.15), value: overflow.trailing)
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

/// The tab strip's overflow: whether a tab is hidden past each edge.
private struct StripOverflow: Equatable {
    var leading = false
    var trailing = false
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
