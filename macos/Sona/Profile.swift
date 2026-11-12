import AppKit
import Observation
import SwiftUI

private struct ProfilePhoto: Decodable {
    let png: String?
}

/// The user's own photo. The core keeps it; this holds the decoded image.
@Observable @MainActor
final class ProfilePhotoStore {
    private(set) var image: NSImage?
    private(set) var busy = false
    private(set) var error: String?
    @ObservationIgnored private let core: Core
    @ObservationIgnored private var loading = false

    init(core: Core) { self.core = core }

    func load() async {
        guard !loading else { return }
        loading = true
        defer { loading = false }
        do {
            apply(try await core.request("profile_photo_get"))
        } catch {
            self.error = error.localizedDescription
        }
    }

    func choose() {
        guard !busy else { return }
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.png, .jpeg]
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Use photo"
        let chosen: (NSApplication.ModalResponse) -> Void = { [weak self] result in
            guard result == .OK, let url = panel.url else { return }
            Task { @MainActor in await self?.save(url) }
        }
        if let window = NSApp.keyWindow {
            panel.beginSheetModal(for: window, completionHandler: chosen)
        } else {
            panel.begin(completionHandler: chosen)
        }
    }

    func remove() async {
        guard !busy else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            apply(try await core.request("profile_photo_set", ["png": JSONValue.null]))
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func save(_ url: URL) async {
        guard !busy else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            let png = try await Task.detached(priority: .userInitiated) {
                let image = try PortraitImage.decode(PortraitImage.read(url))
                return try PortraitImage.png(PortraitImage.cropped(image, crop: PortraitCrop()))
            }.value
            apply(try await core.request("profile_photo_set", ["png": JSONValue.string(png.base64EncodedString())]))
        } catch {
            self.error = PeopleStore.message(error)
        }
    }

    private func apply(_ photo: ProfilePhoto) {
        image = photo.png.flatMap { Data(base64Encoded: $0) }.flatMap(NSImage.init(data:))
    }
}

/// The user's face: their photo, else the initials of their profile name.
struct SelfPortrait: View {
    let size: CGFloat
    @Environment(AppModel.self) private var model

    var body: some View {
        let initials = model.myName.map(PersonPortrait.initials) ?? ""
        Group {
            if let image = model.profilePhoto.image {
                Image(nsImage: image).resizable().interpolation(.high).scaledToFill()
            } else if initials.isEmpty {
                Image(systemName: "person.fill")
                    .font(.system(size: size * 0.5))
                    .foregroundStyle(Theme.onAccent)
            } else {
                Text(initials)
                    .font(.system(size: size * 0.4, weight: .semibold))
                    .foregroundStyle(Theme.onAccent)
                    .lineLimit(1)
                    .minimumScaleFactor(0.6)
                    .padding(.horizontal, size * 0.08)
            }
        }
        .frame(width: size, height: size)
        .background { MonogramFill(color: Theme.accentFill) }
        .clipShape(Circle())
        .task { await model.profilePhoto.load() }
    }
}

/// The foot of the sidebar: who is using Sona, opening their profile.
struct SidebarProfileRow: View {
    @Environment(AppModel.self) private var model
    @State private var hovering = false

    var body: some View {
        let selected = model.showingSettings && model.settingsPlace == .profile
        let name = model.myName
        Button {
            model.showSettings(.profile)
        } label: {
            HStack(spacing: Space.s) {
                SelfPortrait(size: 20)
                Text(name ?? "Add your profile")
                    .font(TypeScale.list)
                    .lineLimit(1)
                Spacer(minLength: 4)
            }
            .foregroundStyle(selected ? Theme.ink : Theme.inkSecondary)
            .padding(.horizontal, Space.s)
            .frame(height: Theme.sidebarRow)
            .background(
                selected ? Theme.washSelected : (hovering ? Theme.wash : .clear),
                in: RoundedRectangle(cornerRadius: Theme.radiusRow)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(name.map { "Profile, \($0)" } ?? "Profile")
        .accessibilityAddTraits(selected ? .isSelected : [])
        .onHover { hovering = $0 }
        .task {
            if model.prep.preferences == nil { await model.prep.loadPreferences() }
        }
    }
}

/// Settings > Profile: the user's photo, name, email and what Sona should
/// know about their work, and the voice Sona learns from their dictations.
struct ProfileSettingsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Page {
            PageTitle("Profile", subtitle: "Helps Sona choose what matters in notes, briefs and follow-ups.")
            ErrorNote(model.prep.settingsError)
            if let preferences = model.prep.preferences {
                ProfileForm(store: model.prep, photo: model.profilePhoto, initial: preferences.aboutMe)
            } else if model.prep.preferencesLoading {
                ProgressView("Reading your profile…").padding(.vertical, 16)
            } else {
                Button("Read your profile again") { Task { await model.prep.loadPreferences() } }
                    .buttonStyle(.secondary)
            }
        }
        .task {
            await model.prep.loadPreferences()
            await model.prep.loadUserVoice()
        }
    }
}

private struct ProfileForm: View {
    let store: MeetingPrepStore
    let photo: ProfilePhotoStore
    @State private var draft: MeetingAboutMe
    @State private var baseline: MeetingAboutMe
    @State private var askingVoiceConsent = false
    @FocusState private var focus: Field?

    private enum Field { case name, email, role, company, context }

    init(store: MeetingPrepStore, photo: ProfilePhotoStore, initial: MeetingAboutMe) {
        self.store = store
        self.photo = photo
        _draft = State(initialValue: initial)
        _baseline = State(initialValue: initial)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: Space.l) {
                SelfPortrait(size: 64)
                VStack(alignment: .leading, spacing: Space.s) {
                    HStack(spacing: Space.s) {
                        Button(photo.image == nil ? "Choose photo…" : "Change photo…", action: photo.choose)
                            .buttonStyle(.compact)
                        if photo.image != nil {
                            Button("Remove") { Task { await photo.remove() } }
                                .buttonStyle(.quiet)
                        }
                        if photo.busy { ProgressView().controlSize(.small) }
                    }
                    .disabled(photo.busy)
                    ErrorNote(photo.error)
                }
            }
            .padding(.bottom, Space.l)
            PageSection("About you") {
                Card {
                    CardRow {
                        VStack(alignment: .leading, spacing: 12) {
                            TextField("Name", text: $draft.name).textFieldStyle(.roundedBorder)
                                .focused($focus, equals: .name)
                                .accessibilityLabel("Your name")
                            TextField("Email", text: $draft.email).textFieldStyle(.roundedBorder)
                                .textContentType(.emailAddress)
                                .focused($focus, equals: .email)
                                .accessibilityLabel("Your email")
                            TextField("Role", text: $draft.role).textFieldStyle(.roundedBorder)
                                .focused($focus, equals: .role)
                                .accessibilityLabel("Your role")
                            TextField("Company", text: $draft.company).textFieldStyle(.roundedBorder)
                                .focused($focus, equals: .company)
                                .accessibilityLabel("Your company")
                            TextField("What should Sona know about your work?", text: $draft.context, axis: .vertical)
                                .lineLimit(3...6).textFieldStyle(.roundedBorder)
                                .focused($focus, equals: .context)
                                .accessibilityLabel("Context about your work")
                            Text("Your name, role, company and context go to your selected meeting model. Your email does not.")
                                .metaText(Theme.inkSecondary)
                        }
                        .onSubmit { focus = nil }
                    }
                }
            }
            PageSection("Your voice") {
                Card {
                    CardRow {
                        VStack(alignment: .leading, spacing: 6) {
                            if let voice = store.userVoice {
                                Text("Sona knows your voice from \(voice.dictationCount) \(voice.dictationCount == 1 ? "dictation" : "dictations"). The voiceprint stays on this Mac.")
                                    .bodyText(13, Theme.inkSecondary)
                            } else {
                                Text("Sona can learn your voice from your dictations, then suggest which meeting speaker is you. You confirm each suggestion.")
                                    .bodyText(13, Theme.inkSecondary)
                            }
                            if store.preferences?.aboutMe.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == true {
                                Text("Add your name above so Sona can suggest you.").metaText(Theme.inkSecondary)
                            }
                            if store.userVoiceBusy { ProgressView("Reading your dictations…") }
                            ErrorNote(store.userVoiceError)
                        }
                    } trailing: {
                        if store.userVoice != nil {
                            Button("Forget my voice") { Task { await store.forgetUserVoice() } }
                                .buttonStyle(.quiet)
                                .disabled(store.userVoiceBusy)
                        } else {
                            Button("Learn my voice") { askingVoiceConsent = true }
                                .buttonStyle(.compact)
                                .disabled(store.userVoiceBusy || !store.userVoiceLoaded)
                        }
                    }
                }
                .alert("Learn your voice?", isPresented: $askingVoiceConsent) {
                    Button("Learn my voice") { Task { await store.learnUserVoice() } }
                    Button("Cancel", role: .cancel) {}
                } message: {
                    Text("Sona uses the recordings of up to 50 recent dictations to make one voiceprint, kept on this Mac. Nothing is uploaded. It only suggests which meeting speaker is you, and you confirm each suggestion. You can forget it here at any time.")
                }
            }
        }
        .onChange(of: focus) { save() }
        .onDisappear { save() }
        .onChange(of: store.preferences?.aboutMe) { _, saved in
            guard let saved, draft == baseline else { return }
            draft = saved
            baseline = saved
        }
    }

    /// The profile is saved whenever one of its fields is left, on Enter, or
    /// when the page goes away; a field still focused at quit is not saved.
    private func save() {
        guard draft != baseline else { return }
        var profile = draft
        profile.email = profile.email.trimmingCharacters(in: .whitespacesAndNewlines)
        Task {
            if await store.saveAboutMe(profile) { baseline = profile }
        }
    }
}
