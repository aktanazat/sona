import SwiftUI

struct ChatModelPicker: View {
    let store: ChatStore
    var openSettings: () -> Void
    @State private var presented = false
    @State private var search = ""
    @State private var provider = ""
    @State private var hoveredModel: String?
    @State private var hovering = false
    @FocusState private var searchFocused: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        Button { presented.toggle() } label: {
            HStack(spacing: 8) {
                if let model = store.selectedModel {
                    ModelProviderMark(provider: model.provider)
                        .frame(width: 18, height: 18)
                }
                VStack(alignment: .leading, spacing: 1) {
                    Text(store.selectedModel?.name ?? "Sona agent")
                        .headlineText()
                        .lineLimit(1)
                    Text(effortCaption)
                        .metaText(Theme.inkSecondary)
                        .lineLimit(1)
                }
                Image(systemName: "chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.inkTertiary)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(hovering ? Theme.wash : Color.clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .contentShape(RoundedRectangle(cornerRadius: Theme.radiusControl))
            .frame(maxWidth: 260)
        }
        .buttonStyle(.quiet)
        .onHover { hovering = $0 }
        .disabled(store.modelPickerDisabled)
        .help("Choose a model and thinking effort")
        .accessibilityLabel("Model: \(store.selectedModel?.name ?? "Server default"). \(effortCaption)")
        .popover(isPresented: $presented, arrowEdge: .bottom) {
            picker
                .onExitCommand { presented = false }
                .task {
                    provider = store.selectedModel?.provider ?? store.modelCatalog?.models.first?.provider ?? ""
                    search = ""
                    searchFocused = true
                    await store.loadModels()
                    if provider.isEmpty {
                        provider = store.selectedModel?.provider ?? store.modelCatalog?.models.first?.provider ?? ""
                    }
                }
        }
    }

    private var effortCaption: String {
        guard let effort = store.effectiveModelSelection?.thinkingEffort else {
            guard let model = store.selectedModel else { return "Choose model and thinking" }
            return model.thinking.isEmpty ? "No adjustable thinking" : "Default thinking"
        }
        return effort == "off" ? "Thinking off" : "\(AgentModel.effortName(effort)) thinking"
    }

    private var picker: some View {
        VStack(spacing: 0) {
            VStack(spacing: 10) {
                providerPicker
                searchField
            }
            .padding(12)
            modelList.frame(height: 176)
            Hairline()
            pickerFooter
        }
        .frame(width: 360)
        .background(Theme.surface)
        .tint(Theme.accent)
    }

    private var searchField: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 12, weight: .regular))
                .foregroundStyle(Theme.inkTertiary)
                .accessibilityHidden(true)
            TextField("Find a model", text: $search, prompt: Text("Find a model").foregroundStyle(Theme.inkTertiary))
                .textFieldStyle(.plain)
                .bodyText(13)
                .accessibilityLabel("Find a model")
                .focused($searchFocused)
                .onSubmit {
                    if let model = filteredModels.first { store.selectModel(model) }
                }
        }
        .padding(.horizontal, 10)
        .frame(height: 30)
        .background(Theme.inset, in: Capsule())
        .overlay(Capsule().strokeBorder(searchFocused ? Theme.accent : Theme.border, lineWidth: 1))
    }

    private var providerPicker: some View {
        HStack(spacing: 4) {
            ForEach(["openai-codex", "anthropic"], id: \.self) { id in
                if let model = store.modelCatalog?.models.first(where: { $0.provider == id }) {
                    let selected = provider == id
                    Button {
                        provider = id
                        search = ""
                        hoveredModel = nil
                        searchFocused = true
                    } label: {
                        HStack(spacing: 7) {
                            ModelProviderMark(provider: id).frame(width: 15, height: 15)
                            Text(model.providerName).font(TypeScale.label(13))
                        }
                        .foregroundStyle(selected ? Theme.ink : Theme.inkSecondary)
                        .frame(maxWidth: .infinity)
                        .frame(height: 28)
                        .background(selected ? Theme.surface : Color.clear, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
                        .contentShape(RoundedRectangle(cornerRadius: Theme.radiusKey))
                        .animation(reduceMotion ? nil : .easeOut(duration: 0.12), value: selected)
                    }
                    .buttonStyle(.quiet)
                    .accessibilityLabel("\(model.providerName) provider")
                    .accessibilityAddTraits(selected ? [.isSelected] : [])
                }
            }
        }
        .padding(3)
        .frame(height: 34)
        .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
    }

    private var filteredModels: [AgentModel] {
        (store.modelCatalog?.models ?? []).filter {
            $0.provider == provider && (search.isEmpty
                || $0.name.localizedCaseInsensitiveContains(search)
                || $0.id.localizedCaseInsensitiveContains(search))
        }.sorted {
            let order = $0.name.localizedStandardCompare($1.name)
            return order == .orderedSame ? $0.id < $1.id : order == .orderedDescending
        }
    }

    @ViewBuilder
    private var modelList: some View {
        if let error = store.modelError {
            VStack(spacing: 12) {
                Text(error)
                    .bodyText(13, Theme.inkSecondary)
                    .multilineTextAlignment(.center)
                if store.modelCatalog == nil {
                    Button("Open connection settings") {
                        presented = false
                        openSettings()
                    }
                    .buttonStyle(.compact)
                }
            }
            .padding(20)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if store.modelCatalog?.models.isEmpty != false {
            VStack(spacing: 12) {
                if store.loadingModels {
                    ProgressView().controlSize(.small)
                    Text("Loading models…").bodyText(13, Theme.inkSecondary)
                } else {
                    Button("Open connection settings") {
                        presented = false
                        openSettings()
                    }
                    .buttonStyle(.compact)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            let matches = filteredModels
            ScrollView {
                VStack(spacing: 2) {
                    ForEach(matches) { model in modelRow(model) }
                    if matches.isEmpty {
                        Text("No models match your search.")
                            .bodyText(13, Theme.inkSecondary)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 20)
                    }
                }
                .padding(.horizontal, 8)
            }
            .id(provider)
            .scrollIndicators(.visible)
        }
    }

    private func modelRow(_ model: AgentModel) -> some View {
        let selected = store.modelSelection?.model == model.id
        let version = modelVersion(model)
        return Button { store.selectModel(model) } label: {
            HStack(spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(model.name).bodyText(13).lineLimit(1)
                    if let version {
                        Text(version).metaText()
                    }
                }
                Spacer(minLength: 4)
                selectedMark.opacity(selected ? 1 : 0)
            }
            .padding(.horizontal, 10)
            .frame(height: 44)
            .background(selected ? Theme.selection : hoveredModel == model.id ? Theme.wash : Color.clear,
                        in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .contentShape(RoundedRectangle(cornerRadius: Theme.radiusControl))
        }
        .buttonStyle(.quiet)
        .disabled(store.modelPickerDisabled)
        .onHover { hoveredModel = $0 ? model.id : nil }
        .help(model.id)
        .accessibilityLabel(version.map { "\(model.name), \($0)" } ?? model.name)
        .accessibilityAddTraits(selected ? [.isSelected] : [])
    }

    private func modelVersion(_ model: AgentModel) -> String? {
        let date = model.id.suffix(8)
        guard date.count == 8, date.allSatisfy(\.isNumber) else { return nil }
        return "\(date.prefix(4))-\(date.dropFirst(4).prefix(2))-\(date.suffix(2))"
    }

    private var pickerFooter: some View {
        HStack(spacing: 10) {
            Button {
                if let catalog = store.modelCatalog,
                   let model = catalog.models.first(where: { $0.id == catalog.defaultSelection?.model }) {
                    provider = model.provider
                }
                search = ""
                store.selectModel(nil)
            } label: {
                Text("Server default").font(TypeScale.body(12))
            }
            .buttonStyle(.quiet)
            .foregroundStyle(store.modelSelection == nil ? Theme.accent : Theme.inkSecondary)
            .disabled(store.modelPickerDisabled || store.modelCatalog == nil)
            .accessibilityLabel("Use server default")
            .help("Use the model and thinking effort configured on your server")
            Spacer(minLength: 4)
            if let model = store.selectedModel, model.provider == provider, !model.thinking.isEmpty {
                Menu {
                    Picker("Thinking effort", selection: Binding(
                        get: { store.effectiveModelSelection?.thinkingEffort ?? "" },
                        set: { store.selectEffort($0) }
                    )) {
                        ForEach(model.thinking, id: \.self) { effort in
                            Text(AgentModel.effortName(effort)).tag(effort)
                        }
                    }
                    .pickerStyle(.inline)
                } label: {
                    Text("\(store.effectiveModelSelection?.thinkingEffort.map(AgentModel.effortName) ?? "Default") thinking")
                        .font(TypeScale.body(12))
                }
                .menuStyle(.borderlessButton)
                .tint(Theme.inkSecondary)
                .fixedSize()
                .disabled(store.modelPickerDisabled)
                .accessibilityLabel("Thinking effort")
            }
            ZStack {
                Button { Task { await store.loadModels(force: true) } } label: {
                    Image(systemName: "arrow.clockwise").font(.system(size: 12))
                }
                .buttonStyle(.quiet)
                .disabled(store.loadingModels || store.savingModel)
                .opacity(store.loadingModels || store.savingModel ? 0 : 1)
                .help("Refresh models")
                .accessibilityLabel("Refresh models")
                if store.loadingModels || store.savingModel {
                    ProgressView().controlSize(.small)
                }
            }
            .frame(width: 18, height: 18)
        }
        .padding(.horizontal, 12)
        .frame(height: 44)
    }

    private var selectedMark: some View {
        Image(systemName: "checkmark").font(.system(size: 12, weight: .semibold))
            .foregroundStyle(Theme.accent)
            .accessibilityHidden(true)
    }
}

private struct ModelProviderMark: View {
    let provider: String

    var body: some View {
        Image(provider == "anthropic" ? "ClaudeMark" : "OpenAIMark")
            .resizable()
            .renderingMode(.template)
            .scaledToFit()
            .accessibilityHidden(true)
    }
}
