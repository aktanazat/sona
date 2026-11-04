import SwiftUI

/// The meeting's open commitments that nothing in the planner cites yet.
/// Each one goes to a project as the user's own task, or is set aside as not
/// work; with none left the section is gone.
struct UntrackedActionsSection: View {
    let store: MeetingsStore

    var body: some View {
        switch store.untracked {
        case .reading:
            EmptyView()
        case let .unavailable(reason):
            PageSection("Not tracked yet") {
                Card {
                    CardRow {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("Sona could not check what is already tracked.").bodyText()
                            Text(reason).metaText(Theme.inkSecondary)
                        }
                    } trailing: {
                        Button("Try again") { store.retryUntracked() }
                            .buttonStyle(SecondaryButton(compact: true))
                    }
                }
            }
        case let .read(untracked):
            if !untracked.actions.isEmpty {
                PageSection("Not tracked yet") {
                    Card {
                        ForEach(untracked.actions) { action in
                            row(action, projects: untracked.projects)
                        }
                    }
                }
            }
        }
    }

    private func row(_ action: MeetingLoopRow, projects: [PlanningProject]) -> some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(action.text).bodyText(14)
                HStack(spacing: 10) {
                    Text(action.direction.label).metaText()
                    if action.direction != .mine, let owner = action.owner {
                        Text(owner).metaText()
                    }
                    Text(action.atMs.meetingMsClock).metaText()
                }
            }
        } trailing: {
            HStack(spacing: 12) {
                Menu {
                    if projects.isEmpty {
                        Text("No open projects")
                    }
                    ForEach(projects) { project in
                        Button(project.title) { store.track(action, in: project) }
                    }
                } label: {
                    Text("Track in…")
                        .font(TypeScale.label(13))
                        .foregroundStyle(Theme.accent)
                }
                .menuStyle(.borderlessButton)
                .tint(Theme.accent)
                .menuIndicator(.hidden)
                .fixedSize()
                Button("Not work") { store.setAside(action) }
                    .buttonStyle(QuietButton())
            }
            .disabled(store.untrackedBusy)
        }
    }
}
