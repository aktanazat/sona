import SwiftUI

struct OnboardingView: View {
    let store: OnboardingStore
    var onFinish: () -> Void = {}
    @State private var languageQuery = ""

    var body: some View {
        GeometryReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    OnboardingBrand()
                    step
                }
                .padding(36)
                .frame(width: 480, alignment: .leading)
                .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusDialog))
                .overlay(RoundedRectangle(cornerRadius: Theme.radiusDialog).strokeBorder(Theme.border, lineWidth: 1))
                .padding(.vertical, 48)
                .frame(maxWidth: .infinity, minHeight: proxy.size.height)
            }
            .scrollIndicators(.never)
        }
        .background(Theme.page)
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
                    Button("Try again") { Task { await store.start() } }.buttonStyle(.wide)
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
            Text("You're set.").heroText()
        }
    }

    private var permissions: some View {
        VStack(alignment: .leading, spacing: 0) {
            if store.permissions.checking {
                OnboardingWaitingLine(label: "Checking permissions")
            } else if store.permissions.allGranted {
                Text("Both granted").heroText()
            } else {
                OnboardingHeading(title: "One-time setup", line: "Allow Sona to hear your voice and type your words.")
                ErrorNote(store.permissions.error)
                VStack(alignment: .leading, spacing: 0) {
                    if store.permissions.microphone != .granted {
                        OnboardingPermissionRow(
                            separated: false,
                            icon: "mic",
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
                            separated: store.permissions.microphone != .granted,
                            icon: "accessibility",
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
            OnboardingHeading(title: "Pick a transcription model", line: store.languageSummary)
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
        VStack(alignment: .leading, spacing: 0) {
            OnboardingHeading(title: "Dictation languages", line: store.languageSummary)
            ErrorNote(store.error)
            VStack(alignment: .leading, spacing: 12) {
                Toggle("Auto detect any language", isOn: Binding(
                    get: { store.selectedLanguages.isEmpty },
                    set: { enabled in
                        if enabled { store.useAutomaticLanguage() }
                    }
                ))
                .toggleStyle(.checkbox)
                .disabled(store.selectedLanguages.isEmpty)
                SearchField(prompt: "Search languages", text: $languageQuery)
                Group {
                    if languageMatches.isEmpty {
                        Text("No languages found")
                            .metaText()
                            .padding(16)
                            .frame(maxWidth: .infinity, alignment: .leading)
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
                                    .frame(maxWidth: .infinity, minHeight: 32, alignment: .leading)
                                    .padding(.horizontal, 14)
                                }
                            }
                            .padding(.vertical, 6)
                        }
                        .frame(height: 238)
                    }
                }
                .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            }
            .disabled(store.savingLanguages)
            Button(store.savingLanguages ? "Saving…" : "Continue", action: store.saveLanguages)
                .buttonStyle(.wide)
                .disabled(store.savingLanguages)
                .padding(.top, 28)
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
        return VStack(alignment: .leading, spacing: 0) {
            BackLink(title: "Speech model", action: store.backToModels)
                .disabled(store.practiceState.isBusy || store.finishing)
            Text("Try dictation").heroText().padding(.bottom, 10)
            Text(practiceStatus)
                .bodyText(14, store.practiceState == .listening ? Theme.live : Theme.inkSecondary)
                .accessibilityLabel("Practice status")
                .accessibilityValue(practiceStatus)
                .padding(.bottom, 24)
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
                        .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                        .accessibilityLabel("Practice transcript")
                }
                .padding(.bottom, 24)
            }
            VStack(spacing: 12) {
                switch store.practiceState {
                case .idle:
                    Button(store.error == nil ? "Start dictation" : "Try again", action: store.startPractice)
                        .buttonStyle(.wide)
                case .listening:
                    Button("Stop dictation", action: store.stopPractice).buttonStyle(.wide)
                    Button("Cancel", action: store.cancelPractice).buttonStyle(.quiet)
                case .starting, .processing:
                    ProgressView().controlSize(.small)
                    Button("Cancel", action: store.cancelPractice).buttonStyle(.quiet)
                case .cancelling:
                    ProgressView().controlSize(.small)
                case .cancellationFailed:
                    Button("Try cancel again", action: store.cancelPractice).buttonStyle(.wide)
                case .delivered:
                    Button(store.finishing ? "Finishing…" : "Done", action: store.completePractice)
                        .buttonStyle(.wide)
                        .disabled(!store.canFinishPractice)
                    Button("Try again", action: store.startPractice)
                        .buttonStyle(.quiet)
                        .disabled(store.finishing)
                }
            }
            .frame(maxWidth: .infinity)
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
            .font(TypeScale.label(13))
            .foregroundStyle(Theme.inkTertiary)
            .padding(.bottom, 28)
    }
}

/// A step's big title and the one gray line under it.
private struct OnboardingHeading: View {
    let title: String
    let line: String

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title).heroText().accessibilityAddTraits(.isHeader)
            Text(line).bodyText(14, Theme.inkSecondary)
        }
        .padding(.bottom, 24)
    }
}

/// The one wide blue button that moves a step forward.
private struct WideButton: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 14, weight: .semibold))
            .foregroundStyle(Theme.onAccent)
            .frame(maxWidth: .infinity)
            .frame(height: 40)
            .background(Theme.accentFill.opacity(configuration.isPressed ? 0.8 : 1), in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .contentShape(RoundedRectangle(cornerRadius: Theme.radiusControl))
            .opacity(isEnabled ? 1 : 0.45)
    }
}

private extension ButtonStyle where Self == WideButton {
    static var wide: WideButton { WideButton() }
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
    let separated: Bool
    let icon: String
    let title: String
    let detail: String
    let state: PermissionState
    let grant: String
    let onGrant: () -> Void
    let onOpenSettings: () -> Void
    let onRecheck: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 16) {
            Image(systemName: icon)
                .font(.system(size: 20, weight: .regular))
                .foregroundStyle(Theme.accent)
                .frame(width: 28)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Text(title).headlineText()
                    if state == .waiting {
                        Text("Waiting…").metaText()
                    }
                }
                Text(detail).bodyText(13, Theme.inkSecondary)
                    .fixedSize(horizontal: false, vertical: true)
                if state == .waiting {
                    HStack(spacing: 10) {
                        Button("Open System Settings", action: onOpenSettings)
                            .buttonStyle(.compact)
                        Button("Re-check", action: onRecheck)
                            .buttonStyle(.quiet)
                    }
                    .padding(.top, 8)
                }
            }
            Spacer(minLength: 12)
            if state != .waiting {
                Button(grant, action: onGrant)
                    .buttonStyle(PrimaryButton(compact: true))
            }
        }
        .padding(.vertical, 16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .top) {
            if separated { Hairline() }
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
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline) {
                Text(model.name).headlineText()
                Spacer(minLength: 12)
                if work == .downloadable {
                    Text(model.sizeText).metaText().monospacedDigit()
                }
            }
            Text(model.description).bodyText(13, Theme.inkSecondary)
            if let label = workLabel {
                VStack(alignment: .leading, spacing: 8) {
                    /* A bar only where there is something to measure: a
                     * download knows its own length, and verifying,
                     * unpacking and loading do not. */
                    if let progress {
                        Meter(fraction: progress)
                    }
                    HStack(spacing: 10) {
                        Text(label).metaText(Theme.inkSecondary).monospacedDigit()
                        if work == .downloading {
                            Button("Cancel", action: onCancel).buttonStyle(.quiet)
                        }
                    }
                }
                .padding(.top, 6)
            } else if !dimmed {
                Button(work == .available ? "Use this model" : "Download · \(model.sizeText)", action: onChoose)
                    .buttonStyle(.wide)
                    .padding(.top, 10)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
        .opacity(dimmed ? 0.5 : 1)
        .padding(.bottom, 8)
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
        .padding(.top, 16)
    }
}

private struct OnboardingGroupLabel: View {
    let label: String

    var body: some View {
        Text(label).sectionLabel().padding(.top, 4).padding(.bottom, 8)
    }
}
