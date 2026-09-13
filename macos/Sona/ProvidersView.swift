import SwiftUI

/// Providers: the language model that rewrites a transcript, the address and
/// key that reach it, what the reader has allowed to leave this Mac, the
/// prompts the model is given, and the keys for the cloud transcription routes.
struct ProvidersView: View {
    let store: ProvidersStore

    @State private var baseUrlDraft = ""
    @State private var keyDraft = ""
    @State private var modelDraft = ""
    @State private var showConsent = false
    @State private var promptDraft: PostProcessPromptDraft?
    @State private var pendingDelete: PostProcessPrompt?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ErrorNote(store.error)
            postProcessing
            api
            cloudKeys
            promptLibrary
        }
        .task {
            await store.start()
            baseUrlDraft = store.baseUrl
        }
        .onChange(of: store.baseUrl) { _, value in baseUrlDraft = value }
        .onChange(of: store.selectedProviderId) { _, _ in
            keyDraft = ""
            modelDraft = ""
        }
        .sheet(isPresented: $showConsent) {
            if let provider = store.selectedProvider, let address = store.endpoint.address {
                ProviderConsentSheet(
                    store: store,
                    provider: provider,
                    address: address,
                    isPresented: $showConsent
                )
            }
        }
        .sheet(item: $promptDraft) { draft in
            PostProcessPromptSheet(store: store, draft: draft, editing: $promptDraft)
        }
        .confirmationDialog(
            pendingDelete.map { "Delete \($0.name)" } ?? "",
            isPresented: Binding(
                get: { pendingDelete != nil },
                set: { if !$0 { pendingDelete = nil } }
            ),
            titleVisibility: .visible
        ) {
            Button("Delete", role: .destructive) {
                if let prompt = pendingDelete {
                    Task { _ = await store.deletePrompt(prompt.id) }
                }
                pendingDelete = nil
            }
            Button("Cancel", role: .cancel) { pendingDelete = nil }
        } message: {
            if let prompt = pendingDelete {
                Text(
                    prompt.id == store.selectedPromptId
                        ? "\(prompt.name) is in use. Deleting it moves the selection to the first prompt in the list."
                        : "\(prompt.name) is removed from the library. Modes that already copied its text keep their own prompt."
                )
            }
        }
    }

    // MARK: - Post-processing

    private var postProcessing: some View {
        PageSection("Post-processing") {
            Card {
                ToggleRow(
                    title: "Post-processing",
                    detail: "Rewrite the transcript with a language model after transcription.",
                    isOn: Binding(
                        get: { store.enabled },
                        set: { on in Task { await store.setEnabled(on) } }
                    )
                )
            }
        }
    }

    // MARK: - The provider

    private var api: some View {
        PageSection("API (OpenAI-compatible)") {
            Card {
                providerRow
                if store.isAppleProvider {
                    if store.appleIntelligenceUnavailable {
                        ProviderNoteRow(
                            text: "Apple Intelligence is not available on this device. Requires an Apple Silicon Mac running macOS Tahoe (26.0) or later with Apple Intelligence enabled in System Settings.",
                            tone: Theme.live
                        )
                    }
                    ProviderFieldRow("Model") {
                        Text(store.model.isEmpty ? (store.selectedProvider?.label ?? "") : store.model)
                            .bodyText(15, Theme.inkSecondary)
                    }
                } else {
                    if store.isCustomProvider {
                        baseUrlRow
                        if store.localEndpointUnreachable {
                            ProviderNoteRow(text: "Sona could not reach this provider.", tone: Theme.accent)
                        }
                    }
                    keyRow
                    consentRows
                    modelRow
                    if store.allowsManualModelId {
                        manualModelRow
                    }
                }
            }
        }
    }

    @ViewBuilder private var providerRow: some View {
        if store.providers.isEmpty {
            ProviderFieldRow("Provider") {
                Text("Loading providers").metaText()
            }
        } else {
            ChoiceRow(
                title: "Provider",
                choices: store.providers.map(\.id),
                label: { id in store.providers.first { $0.id == id }?.label ?? id },
                selection: Binding(
                    get: { store.selectedProviderId },
                    set: { id in Task { await store.select(id) } }
                )
            )
        }
    }

    private var baseUrlRow: some View {
        ProviderFieldRow("Base URL") {
            HStack(spacing: 8) {
                InputField(prompt: "https://api.openai.com/v1", text: $baseUrlDraft)
                    .frame(width: 280)
                    .onSubmit { commitBaseUrl() }
                Button("Save", action: commitBaseUrl)
                    .buttonStyle(.secondary)
                    .disabled(store.isSavingBaseUrl || baseUrlDraft.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
    }

    /// The key is written and never read back: the field is always empty, and
    /// the row says what the credential store holds instead.
    private var keyRow: some View {
        ProviderFieldRow("API key", detail: keyDetail) {
            HStack(spacing: 8) {
                SecretKeyField(prompt: "sk-...", text: $keyDraft, onCommit: commitKey)
                    .frame(width: 220)
                    .disabled(store.isSavingSecret || store.isSecretUnavailable)
                Button("Save", action: commitKey)
                    .buttonStyle(.secondary)
                    .disabled(
                        store.isSavingSecret
                            || store.isSecretUnavailable
                            || keyDraft.trimmingCharacters(in: .whitespaces).isEmpty
                    )
                if store.selectedSecretState?.configured == true {
                    Button("Delete") { Task { await store.deleteSecret() } }
                        .buttonStyle(QuietButton(color: Theme.live))
                        .disabled(store.isSavingSecret)
                }
            }
        }
    }

    private var keyDetail: String {
        if let fault = store.selectedSecretState?.lastErrorKind {
            return fault.sentence
        }
        guard let state = store.selectedSecretState else {
            return "Keys stay in the system credential store."
        }
        return state.configured
            ? "A key is saved in the system credential store."
            : "No API key saved."
    }

    @ViewBuilder private var consentRows: some View {
        switch store.endpoint {
        case .local:
            EmptyView()
        case .invalid:
            ProviderNoteRow(
                text: "This provider destination is invalid or local-only, so no remote consent can be recorded.",
                tone: Theme.live
            )
        case let .remote(address):
            ActionRow(
                title: "Remote text transfer",
                detail: store.hasCurrentConsent
                    ? "You allowed text to be sent to \(address)."
                    : "Sona may send meeting and dictation text to \(store.selectedProvider?.label ?? "this provider") at \(address) for cleanup.",
                button: store.hasCurrentConsent ? "Review acknowledgement" : "Acknowledge",
                busy: store.isAcceptingConsent
            ) {
                store.clearConsentError()
                showConsent = true
            }
            if store.endpointChanged, !store.hasCurrentConsent {
                ProviderNoteRow(
                    text: "The address changed. Review and acknowledge the new one before text can leave this Mac.",
                    tone: Theme.accent
                )
            }
        }
    }

    private var modelRow: some View {
        ProviderFieldRow(
            "Model",
            detail: store.modelStatus.isEmpty ? nil : store.modelStatus.joined(separator: " ")
        ) {
            HStack(spacing: 8) {
                Menu {
                    ForEach(store.modelChoices) { choice in
                        Button("\(choice.id)  ·  \(choice.source.label)") {
                            Task { await store.selectModel(choice.id) }
                        }
                    }
                } label: {
                    Text(store.model.isEmpty ? "Search or select a model" : store.model)
                        .bodyText(14, store.model.isEmpty ? Theme.inkTertiary : Theme.ink)
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                .disabled(store.modelChoices.isEmpty || store.isSavingModel)

                Button("Refresh models") { Task { await store.refreshModels() } }
                    .buttonStyle(.secondary)
                    .disabled(store.isDiscovering)
            }
        }
    }

    /// Some servers publish no list at all, so the model has to be typed. The
    /// core says which ones, and the field only appears for those.
    private var manualModelRow: some View {
        ProviderFieldRow(
            "Model ID",
            detail: store.isCustomProvider ? "Enter the model ID your custom server expects." : nil
        ) {
            HStack(spacing: 8) {
                InputField(prompt: "Type a model name", text: $modelDraft)
                    .frame(width: 220)
                    .onSubmit { commitModel() }
                Button("Use", action: commitModel)
                    .buttonStyle(.secondary)
                    .disabled(store.isSavingModel || modelDraft.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
    }

    // MARK: - Cloud transcription keys

    private var cloudKeys: some View {
        PageSection("Cloud transcription keys") {
            Card {
                ProviderNoteRow(text: "Keys stay in the system credential store.")
                ForEach(CloudSttProvider.allCases) { provider in
                    CloudSttKeyRow(store: store, provider: provider)
                }
            }
        }
    }

    // MARK: - Prompts

    private var promptLibrary: some View {
        PageSection("Post-processing prompts") {
            Card {
                if store.prompts.isEmpty {
                    ProviderNoteRow(
                        text: "A prompt tells the model what to do with the transcript, for example: rewrite the following as a short message, keeping every fact: ${output}"
                    )
                } else {
                    ForEach(store.prompts) { prompt in
                        PostProcessPromptRow(
                            prompt: prompt,
                            selected: prompt.id == store.selectedPromptId,
                            busy: store.isSavingPrompt,
                            canDelete: store.prompts.count > 1,
                            onUse: { Task { await store.usePrompt(prompt.id) } },
                            onEdit: {
                                store.clearPromptError()
                                promptDraft = PostProcessPromptDraft(
                                    id: prompt.id,
                                    name: prompt.name,
                                    body: prompt.prompt
                                )
                            },
                            onDelete: { pendingDelete = prompt }
                        )
                    }
                }
                if !store.prompts.isEmpty, store.selectedPromptId == nil {
                    ProviderNoteRow(text: "No prompt selected: every mode uses the prompt it defines.")
                }
                if store.prompts.count == 1 {
                    ProviderNoteRow(
                        text: "Sona keeps at least one prompt, so this one cannot be deleted. Create another first."
                    )
                }
                ActionRow(
                    title: "New prompt",
                    detail: "Write ${output} where the transcript should be inserted.",
                    button: "New prompt",
                    busy: store.isSavingPrompt
                ) {
                    store.clearPromptError()
                    promptDraft = PostProcessPromptDraft(id: "", name: "", body: "")
                }
            }
        }
    }

    // MARK: - Commits

    private func commitBaseUrl() {
        let value = baseUrlDraft
        Task { await store.commitBaseUrl(value) }
    }

    private func commitKey() {
        let value = keyDraft
        keyDraft = ""
        Task { await store.commitSecret(value) }
    }

    private func commitModel() {
        let value = modelDraft
        modelDraft = ""
        Task { await store.selectModel(value) }
    }
}

// MARK: - Rows

/// A row whose right side is one control, and whose left side names it.
struct ProviderFieldRow<Trailing: View>: View {
    let title: String
    let detail: String?
    let trailing: Trailing

    init(_ title: String, detail: String? = nil, @ViewBuilder trailing: () -> Trailing) {
        self.title = title
        self.detail = detail
        self.trailing = trailing()
    }

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).bodyText()
                if let detail {
                    Text(detail).metaText()
                }
            }
        } trailing: {
            trailing
        }
    }
}

/// A sentence that belongs to the section rather than to any one field.
struct ProviderNoteRow: View {
    let text: String
    var tone: Color = Theme.inkTertiary

    var body: some View {
        CardRow {
            Text(text)
                .font(TypeScale.body(13))
                .foregroundStyle(tone)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// A key on its way in. It is never on its way out: nothing in the app can read
/// a stored secret back, so this field starts empty and is emptied on commit.
struct SecretKeyField: View {
    let prompt: String
    @Binding var text: String
    var onCommit: () -> Void = {}

    var body: some View {
        SecureField(text: $text, prompt: Text(prompt).foregroundStyle(Theme.inkTertiary)) {
            Text(prompt)
        }
        .textFieldStyle(.plain)
        .font(TypeScale.body())
        .foregroundStyle(Theme.ink)
        .padding(.horizontal, 12)
        .frame(height: 36)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
        .onSubmit(onCommit)
    }
}

/// One cloud transcription route: its key, what the store knows about it, and
/// the acknowledgement the core requires before it will spend a request.
struct CloudSttKeyRow: View {
    let store: ProvidersStore
    let provider: CloudSttProvider

    @State private var draft = ""
    @State private var showConsent = false

    private var state: SecretState? { store.cloudSecretState(provider) }
    private var saved: Bool { state?.configured == true }
    private var checking: Bool { store.isChecking(provider) }
    private var busy: Bool { store.isBusy(provider) }
    private var consented: Bool { store.settings?.hasCurrentCloudConsent(provider) ?? false }

    private var status: String {
        if checking { return "Checking the system credential store…" }
        if !saved { return "No API key saved." }
        return state?.verified == true ? "API key verified." : "API key saved. It has not been verified."
    }

    var body: some View {
        VStack(spacing: 0) {
            ProviderFieldRow(provider.label, detail: status) {
                HStack(spacing: 8) {
                    SecretKeyField(prompt: "Paste API key", text: $draft, onCommit: save)
                        .frame(width: 200)
                        .disabled(busy)
                    Button("Save key", action: save)
                        .buttonStyle(.secondary)
                        .disabled(busy || draft.trimmingCharacters(in: .whitespaces).isEmpty)
                    if saved, !checking, consented {
                        Button("Verify") { Task { await store.verifyCloudSecret(provider) } }
                            .buttonStyle(.secondary)
                            .disabled(busy)
                    }
                    if saved, !checking {
                        Button("Remove key") { Task { await store.removeCloudSecret(provider) } }
                            .buttonStyle(QuietButton(color: Theme.live))
                            .disabled(busy)
                    }
                }
            }
            if saved, !checking, !consented {
                ActionRow(
                    title: "Audio transfer",
                    detail: "Acknowledge the transfer before verifying this key.",
                    button: "Acknowledge",
                    busy: busy
                ) {
                    showConsent = true
                }
            }
            if let message = store.cloudError(provider) {
                ProviderNoteRow(text: message, tone: Theme.live)
            }
        }
        .sheet(isPresented: $showConsent) {
            CloudSttConsentSheet(store: store, provider: provider, isPresented: $showConsent)
        }
    }

    private func save() {
        let value = draft
        draft = ""
        Task { await store.saveCloudSecret(provider, secret: value) }
    }
}

/// One prompt in the library.
struct PostProcessPromptRow: View {
    let prompt: PostProcessPrompt
    let selected: Bool
    let busy: Bool
    let canDelete: Bool
    let onUse: () -> Void
    let onEdit: () -> Void
    let onDelete: () -> Void

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text(prompt.name).bodyText()
                    if selected {
                        Chip("In use")
                    }
                }
                Text(prompt.prompt)
                    .metaText()
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
        } trailing: {
            HStack(spacing: 8) {
                if !selected {
                    Button("Use", action: onUse)
                        .buttonStyle(.secondary)
                        .disabled(busy)
                }
                Button("Edit", action: onEdit)
                    .buttonStyle(.quiet)
                    .disabled(busy)
                Button("Delete", action: onDelete)
                    .buttonStyle(QuietButton(color: Theme.live))
                    .disabled(busy || !canDelete)
            }
        }
    }
}

// MARK: - Sheets

/// The prompt being written. An empty id means it does not exist yet.
struct PostProcessPromptDraft: Identifiable, Equatable {
    let id: String
    var name: String
    var body: String
}

struct PostProcessPromptSheet: View {
    let store: ProvidersStore
    @State var draft: PostProcessPromptDraft
    @Binding var editing: PostProcessPromptDraft?

    private var incomplete: Bool {
        draft.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || draft.body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(draft.id.isEmpty ? "New prompt" : "Update prompt").headlineText()

            VStack(alignment: .leading, spacing: 6) {
                Text("Prompt name").metaText()
                InputField(prompt: "Name this prompt", text: $draft.name)
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Instructions").metaText()
                TextEditor(text: $draft.body)
                    .font(TypeScale.body(14))
                    .scrollContentBackground(.hidden)
                    .padding(8)
                    .frame(height: 140)
                    .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                    .overlay(
                        RoundedRectangle(cornerRadius: Theme.radiusControl)
                            .strokeBorder(Theme.border, lineWidth: 1)
                    )
                Text("Write ${output} where the transcript should be inserted.").metaText()
            }

            if let message = store.promptError {
                Text(message).font(TypeScale.body(13)).foregroundStyle(Theme.live)
            }

            HStack(spacing: 10) {
                Spacer()
                Button("Cancel") { editing = nil }
                    .buttonStyle(.secondary)
                Button(draft.id.isEmpty ? "Create prompt" : "Update prompt") {
                    let current = draft
                    Task {
                        let done = current.id.isEmpty
                            ? await store.createPrompt(name: current.name, body: current.body)
                            : await store.updatePrompt(id: current.id, name: current.name, body: current.body)
                        if done { editing = nil }
                    }
                }
                .buttonStyle(.primary)
                .disabled(incomplete || store.isSavingPrompt)
            }
        }
        .padding(24)
        .frame(width: 520)
        .background(Theme.page)
    }
}

/// The one screen where text is allowed to leave this Mac, so it names the
/// exact address rather than the provider.
struct ProviderConsentSheet: View {
    let store: ProvidersStore
    let provider: Provider
    let address: String
    @Binding var isPresented: Bool

    @State private var acknowledged = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Remote text transfer").headlineText()
            Text("Sona may send meeting and dictation text to \(provider.label) at \(address) for cleanup.")
                .bodyText(14, Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)

            VStack(alignment: .leading, spacing: 6) {
                Text("Exact address").metaText()
                Text(address)
                    .font(TypeScale.mono(12))
                    .foregroundStyle(Theme.ink)
                    .textSelection(.enabled)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            }

            Toggle(isOn: $acknowledged) {
                Text("I acknowledge that text may be sent to this address.").bodyText(14)
            }
            .toggleStyle(.checkbox)
            .disabled(store.hasCurrentConsent || store.isAcceptingConsent)

            if let message = store.consentError {
                Text(message).font(TypeScale.body(13)).foregroundStyle(Theme.live)
            }

            Text("Sona ties the acknowledgement to this exact address and origin.").metaText()

            HStack(spacing: 10) {
                Spacer()
                Button("Cancel") { isPresented = false }
                    .buttonStyle(.secondary)
                Button("Allow text transfer") {
                    Task {
                        if await store.acceptConsent() { isPresented = false }
                    }
                }
                .buttonStyle(.primary)
                .disabled(!acknowledged || store.hasCurrentConsent || store.isAcceptingConsent)
            }
        }
        .padding(24)
        .frame(width: 520)
        .background(Theme.page)
        .onAppear { acknowledged = store.hasCurrentConsent }
    }
}

/// The three acknowledgements the core stores together for a cloud route. It
/// checks them together too, so they are given together.
struct CloudSttConsentSheet: View {
    let store: ProvidersStore
    let provider: CloudSttProvider
    @Binding var isPresented: Bool

    @State private var acknowledged = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Use \(provider.label) for cloud transcription").headlineText()
            Text("These acknowledgements are fixed for this provider. Declining keeps every mode on its current local engine.")
                .bodyText(14, Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)

            CloudSttConsentClause(
                title: "Audio transfer",
                detail: "Recorded audio from a cloud mode is sent directly to \(provider.label) for transcription."
            )
            CloudSttConsentClause(
                title: "What the provider sees",
                detail: "The provider receives the mode's audio, its language, and the words you listed. What Sona reads from your other apps, and your keys, stay on this Mac."
            )
            CloudSttConsentClause(
                title: "Local fallback",
                detail: "If the cloud provider cannot return a trustworthy result, Sona can use your selected local model."
            )

            Toggle(isOn: $acknowledged) {
                Text("I acknowledge all three.").bodyText(14)
            }
            .toggleStyle(.checkbox)

            if let message = store.cloudError(provider) {
                Text(message).font(TypeScale.body(13)).foregroundStyle(Theme.live)
            }

            HStack(spacing: 10) {
                Spacer()
                Button("Keep local") { isPresented = false }
                    .buttonStyle(.secondary)
                Button("Accept and use cloud") {
                    Task {
                        await store.acceptCloudConsent(provider)
                        if store.settings?.hasCurrentCloudConsent(provider) == true {
                            isPresented = false
                        }
                    }
                }
                .buttonStyle(.primary)
                .disabled(!acknowledged || store.isBusy(provider))
            }
        }
        .padding(24)
        .frame(width: 520)
        .background(Theme.page)
    }
}

struct CloudSttConsentClause: View {
    let title: String
    let detail: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).bodyText(14)
            Text(detail)
                .metaText()
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
