import SwiftUI

struct OnboardingView: View {
    let store: OnboardingStore
    var onFinish: () -> Void = {}
    @State private var languageQuery = ""

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
        .onChange(of: store.permissions.allGranted) { _, granted in
            if granted { store.permissionsSettled() }
        }
        .onDisappear {
            if store.practiceState.isBusy {
                store.cancelPractice()
            }
        }
    }

    @ViewBuilder
    private var step: some View {
        switch store.step {
        case .probing:
            if let error = store.error {
                VStack(alignment: .leading, spacing: 12) {
                    ErrorNote(error)
                    Button("Try again") { Task { await store.start() } }.buttonStyle(.primary)
                }
            } else {
                OnboardingWaitingLine(label: "Checking setup")
            }
        case .languages:
            languages
        case .permissions:
            permissions
        case .model:
            picker
        case .practice:
            practice
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
                Text("Allow Sona to hear your voice and type your words.")
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
                            detail: "Required to type transcribed text into your applications.",
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
            BackLink(title: "Languages", action: store.changeLanguages)
                .disabled(store.isBusy)
            Text("Pick a transcription model").titleText().padding(.bottom, 10)
            Text(store.languageSummary)
                .bodyText(15, Theme.inkSecondary)
                .padding(.bottom, 24)
            ErrorNote(store.error)
            if let featured = store.featured {
                card(featured)
            } else {
                VStack(alignment: .leading, spacing: 12) {
                    Text("No compatible models available").bodyText(14, Theme.inkSecondary)
                    Button("Reload models") { Task { await store.loadModels() } }.buttonStyle(.secondary)
                }
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

    private var languages: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Dictation languages").titleText()
            ErrorNote(store.error)
            Card {
                VStack(alignment: .leading, spacing: 12) {
                    Toggle("Auto detect any language", isOn: Binding(
                        get: { store.selectedLanguages.isEmpty },
                        set: { enabled in
                            if enabled { store.useAutomaticLanguage() }
                        }
                    ))
                    .toggleStyle(.checkbox)
                    .disabled(store.selectedLanguages.isEmpty)
                    Text(store.languageSummary).metaText(Theme.inkSecondary)
                    SearchField(prompt: "Search languages", text: $languageQuery)
                }
                .padding(16)
                Hairline()
                if languageMatches.isEmpty {
                    CardLine("No languages found")
                } else {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 0) {
                            ForEach(languageMatches) { option in
                                Toggle(option.name, isOn: Binding(
                                    get: { store.selectedLanguages.contains(option.code) },
                                    set: { _ in store.toggleLanguage(option.code) }
                                ))
                                .toggleStyle(.checkbox)
                                .bodyText(14)
                                .frame(maxWidth: .infinity, minHeight: 34, alignment: .leading)
                                .padding(.horizontal, 16)
                            }
                        }
                        .padding(.vertical, 8)
                    }
                    .frame(height: 238)
                }
            }
            .disabled(store.savingLanguages)
            Button(store.savingLanguages ? "Saving…" : "Continue", action: store.saveLanguages)
                .buttonStyle(.primary)
                .disabled(store.savingLanguages)
        }
    }

    private var languageMatches: [LanguageOption] {
        let query = languageQuery.trimmingCharacters(in: .whitespaces).lowercased()
        let options = LanguageCatalog.selectable.filter { $0.code != "auto" }
        return query.isEmpty ? options : options.filter {
            $0.name.lowercased().contains(query) || $0.code.lowercased().hasPrefix(query)
        }
    }

    private var practiceStatus: String {
        switch store.practiceState {
        case .idle: "Ready to record"
        case .starting: "Starting microphone…"
        case .listening: "Listening · stops after 30 seconds"
        case .processing: "Transcribing…"
        case .delivered: "Words delivered"
        case .cancelling: "Cancelling…"
        case .cancellationFailed: "Could not cancel"
        }
    }

    private var practice: some View {
        @Bindable var store = store
        return VStack(alignment: .leading, spacing: 20) {
            BackLink(title: "Speech model", action: store.backToModels)
                .disabled(store.practiceState.isBusy || store.finishing)
            Text("Try dictation").titleText()
            Text(practiceStatus)
                .bodyText(15, store.practiceState == .listening ? Theme.live : Theme.inkSecondary)
                .accessibilityLabel("Practice status")
                .accessibilityValue(practiceStatus)
            ErrorNote(store.error)
                .accessibilityLabel(store.error ?? "")
            if store.practiceState == .delivered {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Your words").sectionLabel()
                    TextEditor(text: $store.practiceText)
                        .font(TypeScale.body())
                        .foregroundStyle(Theme.ink)
                        .scrollContentBackground(.hidden)
                        .padding(12)
                        .frame(minHeight: 150, maxHeight: 240)
                        .background(Theme.surface)
                        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusControl))
                        .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
                        .accessibilityLabel("Practice transcript")
                }
            }
            HStack(spacing: 16) {
                switch store.practiceState {
                case .idle:
                    Button(store.error == nil ? "Start dictation" : "Try again", action: store.startPractice)
                        .buttonStyle(.primary)
                case .listening:
                    Button("Stop dictation", action: store.stopPractice).buttonStyle(.primary)
                    Button("Cancel", action: store.cancelPractice).buttonStyle(.quiet)
                case .starting, .processing:
                    ProgressView().controlSize(.small)
                    Button("Cancel", action: store.cancelPractice).buttonStyle(.quiet)
                case .cancelling:
                    ProgressView().controlSize(.small)
                case .cancellationFailed:
                    Button("Try cancel again", action: store.cancelPractice).buttonStyle(.primary)
                case .delivered:
                    Button(store.finishing ? "Finishing…" : "Done", action: store.completePractice)
                        .buttonStyle(.primary)
                        .disabled(!store.canFinishPractice)
                    Button("Try again", action: store.startPractice)
                        .buttonStyle(.quiet)
                        .disabled(store.finishing)
                }
            }
        }
        .onChange(of: practiceStatus) { _, status in
            NSAccessibility.post(
                element: NSApplication.shared,
                notification: .announcementRequested,
                userInfo: [.announcement: status, .priority: NSAccessibilityPriorityLevel.medium.rawValue]
            )
        }
        .onChange(of: store.error) { _, message in
            guard let message else { return }
            NSAccessibility.post(
                element: NSApplication.shared,
                notification: .announcementRequested,
                userInfo: [.announcement: message, .priority: NSAccessibilityPriorityLevel.high.rawValue]
            )
        }
    }

    private func card(_ model: OnboardingModel) -> some View {
        OnboardingModelCard(
            model: model,
            work: store.work(for: model.id),
            progress: store.progress(for: model.id),
            dimmed: store.isBusy && store.chosenId != model.id,
            onChoose: { store.choose(model) },
            onCancel: { store.cancel(model.id) }
        )
    }
}

/// The banner the app carries while a permission it was set up with is gone:
/// one sentence per missing permission and the dialog that lists Sona in the
/// pane that fixes it. Shows nothing while both are held, and nothing for
/// accessibility on a platform with no such permission.
struct PermissionsNotice: View {
    let store: PermissionsStore

    var body: some View {
        if missing(store.microphone) || missing(store.accessibility) {
            Card {
                if missing(store.microphone) {
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Sona needs microphone access to hear you.")
                                .bodyText(14)
                            Text("Turn on “Sona” in the Microphone list.")
                                .metaText()
                        }
                    } trailing: {
                        Button("Grant permission") { store.grantMicrophone() }
                            .buttonStyle(.secondary)
                    }
                }
                if missing(store.accessibility) {
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

    private func missing(_ state: PermissionState) -> Bool {
        state == .needed || state == .waiting
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
        case .cancelling: return "Cancelling…"
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
