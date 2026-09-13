import SwiftUI

/// The first run, as one window: what macOS has to allow, then one model on
/// this Mac. Nothing else is on screen, because nothing else works until both
/// are settled.
///
/// `onFinish` is the integrator's cue to show the app. It fires once, when the
/// store says the flow is over: a returning user's last grant, or a first run's
/// model becoming the active one.
struct OnboardingView: View {
    let store: OnboardingStore
    var onFinish: () -> Void = {}

    var body: some View {
        ZStack {
            Theme.page
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    OnboardingBrand()
                    step
                }
                .frame(maxWidth: 460, alignment: .leading)
                .padding(.horizontal, Theme.margin)
                .padding(.vertical, 64)
                .frame(maxWidth: .infinity, alignment: .center)
            }
            .scrollIndicators(.never)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear {
            // A returning user with everything granted can be past the flow
            // before this view is on screen.
            if store.step == .done {
                onFinish()
            }
        }
        .onChange(of: store.step) { _, step in
            if step == .done {
                onFinish()
            }
        }
    }

    @ViewBuilder
    private var step: some View {
        switch store.step {
        case .probing:
            OnboardingWaitingLine(label: "Checking permissions")
        case .permissions:
            permissions
        case .model:
            picker
        case .done:
            Text("You're set.").titleText()
        }
    }

    private var permissions: some View {
        VStack(alignment: .leading, spacing: 0) {
            if store.permissions.checking {
                OnboardingWaitingLine(label: "Checking permissions")
            } else if store.permissions.allGranted {
                Text("Both granted").titleText()
            } else {
                Text("One-time setup").titleText().padding(.bottom, 10)
                Text("Your system asks before any app can listen or type.")
                    .bodyText(15, Theme.inkSecondary)
                    .padding(.bottom, 24)
                ErrorNote(store.permissions.error)
                VStack(alignment: .leading, spacing: 12) {
                    if store.permissions.microphone != .granted {
                        OnboardingPermissionRow(
                            title: "Microphone access",
                            detail: "Required to hear your voice for transcription.",
                            state: store.permissions.microphone,
                            grant: "Grant permission",
                            onGrant: { store.permissions.grantMicrophone() },
                            onOpenSettings: { store.permissions.openPane(.microphone) },
                            onRecheck: { store.permissions.recheck() }
                        )
                    }
                    if store.permissions.accessibility != .granted, store.permissions.accessibilitySupported {
                        OnboardingPermissionRow(
                            title: "Accessibility access",
                            detail: """
                            Required to type transcribed text into your applications. \
                            The switch to turn on is “Sona”.
                            """,
                            state: store.permissions.accessibility,
                            grant: "Grant permission",
                            onGrant: { store.permissions.grantAccessibility() },
                            onOpenSettings: { store.permissions.openPane(.accessibility) },
                            onRecheck: { store.permissions.recheck() }
                        )
                    }
                }
            }
        }
    }

    private var picker: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Pick a transcription model").titleText().padding(.bottom, 10)
            Text("Models run on this Mac and trade accuracy against speed.")
                .bodyText(15, Theme.inkSecondary)
                .padding(.bottom, 24)
            ErrorNote(store.error)
            if let featured = store.featured {
                card(featured)
            } else {
                Text("No models available").bodyText(14, Theme.inkSecondary)
            }
            if store.otherCount > 0 {
                OnboardingOthers(count: store.otherCount) {
                    if !store.otherOnDisk.isEmpty {
                        OnboardingGroupLabel(label: "Compatible models")
                        ForEach(store.otherOnDisk) { model in card(model) }
                    }
                    if !store.otherDownloads.isEmpty {
                        OnboardingGroupLabel(label: "Available to download")
                        ForEach(store.otherDownloads) { model in card(model) }
                    }
                }
            }
        }
    }

    private func card(_ model: OnboardingModel) -> some View {
        OnboardingModelCard(
            model: model,
            work: store.work(for: model.id),
            progress: store.progress(for: model.id),
            /* Every card except the one being worked on goes quiet while a
             * model is arriving: the reader has already chosen, and the row
             * doing the work is the only one with anything left to say. */
            dimmed: store.isBusy && store.chosenId != model.id,
            onChoose: { store.choose(model) },
            onCancel: { store.cancel(model.id) }
        )
    }
}

/// The banner the app carries while the process that types is not trusted:
/// one sentence and the dialog that lists Sona in the pane that fixes it. Shows
/// nothing once the core is trusted, and nothing on a platform with no such
/// permission.
struct AccessibilityNotice: View {
    let store: PermissionsStore

    var body: some View {
        if store.accessibility == .needed || store.accessibility == .waiting {
            Card {
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Sona needs accessibility permissions to type transcribed text.")
                            .bodyText(14)
                        Text("Turn on “Sona” in the Accessibility list.")
                            .metaText()
                    }
                } trailing: {
                    Button("Grant permission") { store.grantAccessibility() }
                        .buttonStyle(.secondary)
                }
            }
        }
    }
}

/// The name of the thing asking, above every step.
private struct OnboardingBrand: View {
    var body: some View {
        Text("Sona")
            .font(TypeScale.label(14))
            .foregroundStyle(Theme.inkSecondary)
            .tracking(1.2)
            .padding(.bottom, 40)
    }
}

/// Work with no length to measure: a word and a spinner, never a bar drawn at
/// an invented position.
private struct OnboardingWaitingLine: View {
    let label: String

    var body: some View {
        HStack(spacing: 10) {
            ProgressView().controlSize(.small)
            Text(label).bodyText(14, Theme.inkSecondary)
        }
    }
}

/// One permission: what it is, why it is needed, and the one button that moves
/// it forward.
///
/// `waiting` keeps two buttons rather than one: after a macOS denial the consent
/// dialog never appears again, so the exact pane and a re-check that restarts
/// the poll are the only two ways out.
private struct OnboardingPermissionRow: View {
    let title: String
    let detail: String
    let state: PermissionState
    let grant: String
    let onGrant: () -> Void
    let onOpenSettings: () -> Void
    let onRecheck: () -> Void

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Text(title).headlineText()
                    if state == .waiting {
                        Text("Waiting…").metaText()
                    }
                }
                Text(detail).bodyText(14, Theme.inkSecondary)
                HStack(spacing: 10) {
                    if state == .waiting {
                        Button("Open System Settings", action: onOpenSettings)
                            .buttonStyle(.secondary)
                        Button("Re-check", action: onRecheck)
                            .buttonStyle(.quiet)
                    } else {
                        Button(grant, action: onGrant)
                            .buttonStyle(.primary)
                    }
                }
                .padding(.top, 2)
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// One model, as a thing you pick: its name, the one sentence that says what
/// picking it costs, and its download size.
private struct OnboardingModelCard: View {
    let model: OnboardingModel
    let work: OnboardingModelWork
    let progress: Double?
    let dimmed: Bool
    let onChoose: () -> Void
    let onCancel: () -> Void

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .firstTextBaseline) {
                    Text(model.name).headlineText()
                    Spacer(minLength: 12)
                    if work == .downloadable {
                        Text(model.sizeText).metaText()
                    }
                }
                Text(model.description).bodyText(14, Theme.inkSecondary)
                if let label = workLabel {
                    VStack(alignment: .leading, spacing: 8) {
                        /* A bar only where there is something to measure: a
                         * download knows its own length, and verifying,
                         * unpacking and loading do not. */
                        if let progress {
                            Meter(fraction: progress)
                        }
                        HStack(spacing: 10) {
                            Text(label).metaText(Theme.inkSecondary)
                            if work == .downloading {
                                Button("Cancel", action: onCancel).buttonStyle(.quiet)
                            }
                        }
                    }
                    .padding(.top, 4)
                } else if !dimmed {
                    Button(work == .available ? "Use this model" : "Download · \(model.sizeText)", action: onChoose)
                        .buttonStyle(.primary)
                        .padding(.top, 6)
                }
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .opacity(dimmed ? 0.5 : 1)
        .padding(.bottom, 12)
    }

    private var workLabel: String? {
        switch work {
        case .downloading:
            guard let progress else { return "Downloading…" }
            return "Downloading \(Int((progress * 100).rounded()))%"
        case .verifying: return "Verifying…"
        case .extracting: return "Extracting…"
        case .switching: return "Switching…"
        case .available, .downloadable: return nil
        }
    }
}

/// Every model that is not the pick, behind one line.
private struct OnboardingOthers<Content: View>: View {
    let count: Int
    @ViewBuilder let content: Content
    @State private var showing = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Button {
                showing.toggle()
            } label: {
                HStack(spacing: 8) {
                    Text("Other models")
                    Text("\(count) more").metaText()
                    Image(systemName: showing ? "chevron.up" : "chevron.down")
                        .font(.system(size: 11, weight: .semibold))
                }
            }
            .buttonStyle(.quiet)
            if showing {
                VStack(alignment: .leading, spacing: 0) {
                    content
                }
            }
        }
        .padding(.top, 20)
    }
}

private struct OnboardingGroupLabel: View {
    let label: String

    var body: some View {
        Text(label).sectionLabel().padding(.bottom, 10)
    }
}
