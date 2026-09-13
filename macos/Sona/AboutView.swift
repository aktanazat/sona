import SwiftUI

/// The About page: the once-per-install choices, the running build and the
/// update preference, the source and its licenses, and the two paths that get
/// opened when something has gone wrong.
struct AboutView: View {
    let store: AboutStore

    var body: some View {
        Page {
            PageTitle("About Sona", subtitle: "What this build is, where it came from, and where it keeps things.")
            ErrorNote(store.error)

            PageSection("Appearance") {
                Card {
                    ChoiceRow(
                        title: "App language",
                        choices: AppLanguage.all,
                        label: { $0.label },
                        selection: Binding(
                            get: { store.language },
                            set: { store.setLanguage($0.code) }
                        )
                    )
                    ChoiceRow(
                        title: "Appearance",
                        choices: AppearanceTheme.allCases,
                        label: { $0.label },
                        selection: Binding(
                            get: { store.theme },
                            set: { store.setTheme($0) }
                        )
                    )
                    ChoiceRow(
                        title: "Material",
                        detail: "Solid keeps Sona's surfaces opaque. Glass lets the desktop show through the top bar, the command palette, and the recording HUD.",
                        choices: AppearanceMaterial.allCases,
                        label: { $0.label },
                        selection: Binding(
                            get: { store.material },
                            set: { store.setMaterial($0) }
                        )
                    )
                }
            }

            PageSection("Version and updates") {
                Card {
                    AboutVersionRow(store: store)
                    ToggleRow(
                        title: "Check for updates automatically",
                        detail: "Sona never installs anything on its own.",
                        isOn: Binding(
                            get: { store.updateCheckEnabled },
                            set: { store.setUpdateCheckEnabled($0) }
                        )
                    )
                    .disabled(store.settings == nil || store.savingPreference)
                    ToggleRow(
                        title: "Show what's new after an update",
                        detail: "Opens the release note once, the first time a new build runs.",
                        isOn: Binding(
                            get: { store.showWhatsNewOnUpdate },
                            set: { store.setShowWhatsNewOnUpdate($0) }
                        )
                    )
                    .disabled(store.settings == nil)
                    if let status = store.status {
                        AboutStatusRow(status: status, store: store)
                    }
                }
            }

            PageSection("Source") {
                Card {
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Repository").bodyText()
                            Text(AboutStore.repositoryLicense).metaText()
                        }
                    } trailing: {
                        HStack(spacing: 12) {
                            // The scheme is the one part of the URL that tells
                            // the reader nothing; the button still opens it whole.
                            Text(AboutStore.repositoryURL.replacingOccurrences(of: "https://", with: ""))
                                .font(TypeScale.body(13))
                                .foregroundStyle(Theme.inkSecondary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                                .textSelection(.enabled)
                            Button("Open") { store.openRepository() }
                                .buttonStyle(.compact)
                        }
                    }
                    ActionRow(title: "Open-source licenses", button: "Open licenses") {
                        store.openLicenseNotices()
                    }
                }
            }

            PageSection("Files") {
                Card {
                    if let directoryError = store.directoryError {
                        CardRow {
                            Text("Couldn't load the directory: \(directoryError)")
                                .bodyText(14, Theme.live)
                        }
                    } else {
                        AboutPathRow(
                            label: "App data directory",
                            detail: store.portable ? "Portable install: Sona keeps its data beside the app." : nil,
                            path: store.appDirectory
                        ) {
                            store.openAppDataDirectory()
                        }
                        AboutPathRow(label: "Log directory", detail: nil, path: store.logDirectory) {
                            store.openLogDirectory()
                        }
                    }
                    ActionRow(title: "Recordings", button: "Open") {
                        store.openRecordingsFolder()
                    }
                }
            }
        }
    }
}

/// Version and the act of checking it are one question, so they are one row:
/// the running build on the left of the button that asks GitHub for a newer one.
private struct AboutVersionRow: View {
    let store: AboutStore

    var body: some View {
        CardRow {
            Text("Version").bodyText()
        } trailing: {
            HStack(spacing: 12) {
                if let version = store.displayVersion {
                    Text("v\(version)")
                        .font(TypeScale.mono(14))
                        .foregroundStyle(Theme.ink)
                        .textSelection(.enabled)
                } else {
                    // No value exists, so the slot dims. Red is for faults.
                    Text("Unavailable").metaText()
                }
                Button(store.checking ? "Checking…" : "Check now") { store.checkNow() }
                    .buttonStyle(.compact)
                    .disabled(store.checking)
            }
        }
    }
}

/// The one line the update surface has to say, and at most one act.
private struct AboutStatusRow: View {
    let status: AboutStore.Status
    let store: AboutStore

    var body: some View {
        CardRow {
            Text(status.text).bodyText(14, colour)
        } trailing: {
            switch status.act {
            case .retry:
                Button("Retry") { store.checkNow() }
                    .buttonStyle(.compact)
                    .disabled(store.checking)
            case let .viewRelease(url):
                Button("View release") { AboutStatusRow.open(url) }
                    .buttonStyle(.compact)
            case nil:
                EmptyView()
            }
        }
    }

    private var colour: Color {
        switch status.tone {
        case .muted: Theme.inkTertiary
        case .info: Theme.accent
        case .danger: Theme.live
        }
    }

    private static func open(_ url: String) {
        guard let link = URL(string: url) else { return }
        NSWorkspace.shared.open(link)
    }
}

/// An absolute path and the button that opens it. The tail of a path is the
/// part that identifies it, so it wraps rather than truncating.
private struct AboutPathRow: View {
    let label: String
    let detail: String?
    let path: String?
    let open: () -> Void

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 6) {
                Text(label).bodyText()
                if let detail {
                    Text(detail).metaText()
                }
                if let path {
                    Text(path)
                        .font(TypeScale.mono(12))
                        .foregroundStyle(Theme.inkSecondary)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                } else {
                    Text("Loading…").metaText()
                }
            }
        } trailing: {
            Button("Open", action: open)
                .buttonStyle(.compact)
                .disabled(path == nil)
        }
    }
}
