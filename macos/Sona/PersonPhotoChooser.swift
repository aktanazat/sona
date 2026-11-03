import AppKit
import Contacts
import ContactsUI
import Observation
import SwiftUI
import UniformTypeIdentifiers

@MainActor
final class PersonPhotoChooser: NSWindowController, NSWindowDelegate {
    private let state: PortraitChooserState
    private var onClose: (() -> Void)?

    init(core: Core, personId: String, name: String, revision: UInt64,
         saved: @escaping () -> Void, closed: @escaping () -> Void) {
        state = PortraitChooserState(core: core, personId: personId, name: name, revision: revision, saved: saved)
        onClose = closed
        let panel = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 500, height: 660),
                            styleMask: [.titled, .closable], backing: .buffered, defer: false)
        panel.title = "Photo for \(name)"
        panel.isReleasedWhenClosed = false
        super.init(window: panel)
        panel.delegate = self
        let host = NSHostingController(rootView: PortraitChooserView(state: state, close: { [weak self] in self?.dismiss() }))
        panel.contentViewController = host
        state.anchor = host.view
        state.dismiss = { [weak self] in self?.dismiss() }
    }

    required init?(coder: NSCoder) { nil }

    func present() {
        guard let window else { return }
        if let parent = NSApp.keyWindow, parent !== window {
            parent.beginSheet(window)
        } else {
            window.center()
            showWindow(nil)
        }
    }

    private func dismiss() {
        guard let window else { return }
        state.cancel()
        window.sheetParent?.endSheet(window)
        window.close()
        finish()
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool { !state.saving }

    func windowWillClose(_ notification: Notification) { state.cancel(); finish() }

    private func finish() {
        let close = onClose
        onClose = nil
        close?()
    }
}

@MainActor
@Observable
final class PortraitChooserState: NSObject, @preconcurrency CNContactPickerDelegate {
    let name: String
    var imageAddress = ""
    var profileAddress = ""
    var attribution = ""
    var allowRemoteRequest = false
    var identityConfirmed = false
    var crop = PortraitCrop()
    private(set) var preview: NSImage?
    private(set) var provenance: PersonPhotoProvenance?
    private(set) var busy = false
    private(set) var saving = false
    private(set) var error: String?
    private(set) var lowResolution = false
    @ObservationIgnored weak var anchor: NSView?
    @ObservationIgnored var dismiss: (() -> Void)?
    @ObservationIgnored private let core: Core
    @ObservationIgnored private let personId: String
    @ObservationIgnored private var revision: UInt64
    @ObservationIgnored private let saved: () -> Void
    @ObservationIgnored private var original: Data?
    @ObservationIgnored private var decoded: CGImage?
    @ObservationIgnored private var normalized: CGImage?
    @ObservationIgnored private var work: Task<Void, Never>?
    @ObservationIgnored private var cropWork: Task<Void, Never>?
    @ObservationIgnored private var generation: UInt64 = 0
    @ObservationIgnored private var picker: CNContactPicker?

    init(core: Core, personId: String, name: String, revision: UInt64, saved: @escaping () -> Void) {
        self.core = core
        self.personId = personId
        self.name = name
        self.revision = revision
        self.saved = saved
        super.init()
    }

    func cancel() {
        generation &+= 1
        work?.cancel()
        cropWork?.cancel()
        picker?.close()
        picker = nil
        original = nil
        decoded = nil
        normalized = nil
    }

    func chooseFile() {
        guard !busy, !saving, let window = anchor?.window else { return }
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.png, .jpeg]
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Preview photo"
        panel.beginSheetModal(for: window) { [weak self] result in
            guard result == .OK, let url = panel.url else { return }
            Task { @MainActor in
                self?.beginLoad { try await Task.detached(priority: .userInitiated) {
                    let data = try PortraitImage.read(url)
                    return (data, PersonPhotoProvenance.localFile(fileName: url.lastPathComponent, confirmedAtUtcMs: 0))
                }.value }
            }
        }
    }

    func chooseContact() {
        guard !busy, !saving, let anchor else { return }
        let picker = CNContactPicker()
        picker.delegate = self
        picker.displayedKeys = []
        self.picker = picker
        picker.showRelative(to: NSRect(x: 24, y: max(0, anchor.bounds.height - 80), width: 1, height: 1),
                            of: anchor, preferredEdge: .minY)
    }

    func contactPicker(_ picker: CNContactPicker, didSelect contact: CNContact) {
        picker.close()
        self.picker = nil
        beginLoad {
            if !contact.isKeyAvailable(CNContactImageDataKey) {
                let status = CNContactStore.authorizationStatus(for: .contacts)
                if status == .notDetermined {
                    guard try await CNContactStore().requestAccess(for: .contacts) else {
                        throw PersonFailure(message: "Contacts access wasn't allowed. Choose a photo file instead.")
                    }
                } else if status != .authorized {
                    throw PersonFailure(message: "Contacts access isn't available. Choose a photo file instead.")
                }
            }
            return try await Task.detached(priority: .userInitiated) {
                let selected: CNContact
                if contact.isKeyAvailable(CNContactImageDataKey) {
                    selected = contact
                } else {
                    selected = try CNContactStore().unifiedContact(withIdentifier: contact.identifier, keysToFetch: [
                        CNContactImageDataKey as CNKeyDescriptor,
                        CNContactFormatter.descriptorForRequiredKeys(for: .fullName),
                    ])
                }
                guard let data = selected.imageData else {
                    throw PersonFailure(message: "This contact has no photo. Choose another contact or a photo file.")
                }
                let descriptor = CNContactFormatter.descriptorForRequiredKeys(for: .fullName)
                let contactName = selected.areKeysAvailable([descriptor])
                    ? CNContactFormatter.string(from: selected, style: .fullName) ?? "Selected contact"
                    : "Selected contact"
                return (data, PersonPhotoProvenance.localContact(contactName: contactName, confirmedAtUtcMs: 0))
            }.value
        }
    }

    func loadPublicImage() {
        guard allowRemoteRequest, !busy, !saving else { return }
        do {
            let imageURL = try PortraitDownload.publicURL(imageAddress)
            let sourceURL = try PortraitDownload.publicURL(profileAddress.isEmpty ? imageAddress : profileAddress)
            let credit = attribution.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !credit.isEmpty else { throw PersonFailure(message: "Add the photographer or profile name.") }
            beginLoad {
                let data = try await PortraitDownload(url: imageURL).fetch()
                return (data, .publicImage(sourceURL: sourceURL.absoluteString, imageURL: imageURL.absoluteString,
                                           attribution: credit, confirmedAtUtcMs: 0))
            }
        } catch { self.error = PeopleStore.message(error) }
    }

    func loadSavedOriginal() {
        guard !busy, !saving else { return }
        beginLoad { [core, personId] in
            let result: PersonAvatarSourceResult = try await core.request("person_avatar_source", ["personId": personId])
            guard let encoded = result.originalBase64,
                  encoded.utf8.count <= PortraitImage.maximumSourceBytes * 4 / 3 + 4,
                  let bytes = Data(base64Encoded: encoded), let provenance = result.provenance else {
                throw PersonFailure(message: "No original photo was retained. Choose a photo source.")
            }
            return (bytes, provenance)
        }
    }

    private func beginLoad(_ load: @escaping () async throws -> (Data, PersonPhotoProvenance)) {
        generation &+= 1
        let stamp = generation
        work?.cancel()
        cropWork?.cancel()
        busy = true
        error = nil
        identityConfirmed = false
        preview = nil
        normalized = nil
        original = nil
        decoded = nil
        provenance = nil
        work = Task {
            defer { if generation == stamp { busy = false } }
            do {
                let (data, source) = try await load()
                let image = try await Task.detached(priority: .userInitiated) { try PortraitImage.decode(data) }.value
                guard generation == stamp, !Task.isCancelled else { return }
                original = data
                decoded = image
                provenance = source
                crop = PortraitCrop()
                lowResolution = min(image.width, image.height) < 256
                renderCrop()
            } catch {
                guard generation == stamp, !Task.isCancelled else { return }
                self.error = PeopleStore.message(error)
            }
        }
    }

    func renderCrop() {
        guard let decoded else { return }
        cropWork?.cancel()
        normalized = nil
        let desired = crop
        let stamp = generation
        cropWork = Task {
            do {
                let image = try await Task.detached(priority: .userInitiated) { try PortraitImage.cropped(decoded, crop: desired) }.value
                guard generation == stamp, crop == desired, !Task.isCancelled else { return }
                normalized = image
                preview = NSImage(cgImage: image, size: NSSize(width: image.width, height: image.height))
            } catch {
                guard generation == stamp, !Task.isCancelled else { return }
                self.error = PeopleStore.message(error)
            }
        }
    }

    var canSave: Bool { identityConfirmed && normalized != nil && original != nil && provenance != nil && !busy && !saving }

    func save() {
        guard canSave, let normalized, let original, let provenance else { return }
        saving = true
        error = nil
        work = Task {
            defer { saving = false }
            do {
                let png = try await Task.detached(priority: .userInitiated) { try PortraitImage.png(normalized) }.value
                let request = PersonAvatarSetRequest(
                    personId: personId, expectedRevision: revision, pngBase64: png.base64EncodedString(),
                    provenance: provenance.confirmed(at: Int64(Date.now.timeIntervalSince1970 * 1000)),
                    originalBase64: original.base64EncodedString())
                let _: PersonAvatarResult = try await core.request("person_avatar_set", ["request": request])
                saved()
                dismiss?()
            } catch {
                self.error = PeopleStore.message(error)
                if (error as? CoreError)?.remote(as: PeopleCommandError.self) == .staleRevision {
                    if let latest: PersonDetailResult = try? await core.request("person_detail", ["personId": personId]) {
                        revision = latest.revision
                    }
                }
            }
        }
    }
}

private struct PortraitChooserView: View {
    @Bindable var state: PortraitChooserState
    let close: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 12) {
                Button("Contacts", action: state.chooseContact)
                Button("Choose file", action: state.chooseFile)
                Spacer()
                Button("Saved original", action: state.loadSavedOriginal).buttonStyle(.link)
            }
            .disabled(state.busy || state.saving)
            DisclosureGroup("Public image") {
                VStack(alignment: .leading, spacing: 10) {
                    TextField("Direct image address", text: $state.imageAddress)
                        .onChange(of: state.imageAddress) { _, _ in state.allowRemoteRequest = false }
                    TextField("Profile or source address (optional)", text: $state.profileAddress)
                    TextField("Photographer or profile name", text: $state.attribution)
                    Toggle("Allow a request to this image address without a login", isOn: $state.allowRemoteRequest)
                        .font(.system(size: 12))
                    Button("Load image", action: state.loadPublicImage)
                        .disabled(!state.allowRemoteRequest || state.imageAddress.isEmpty)
                }
                .padding(.top, 10)
            }
            .disabled(state.busy || state.saving)
            VStack(spacing: 12) {
                ZStack {
                    Circle().fill(Theme.inset)
                    if let preview = state.preview {
                        Image(nsImage: preview).resizable().interpolation(.high).scaledToFill().clipShape(Circle())
                    } else if state.busy {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: "person.crop.circle").font(.system(size: 40)).foregroundStyle(Theme.inkSecondary)
                    }
                }
                .frame(width: 208, height: 208)
                .overlay(Circle().strokeBorder(Theme.border, lineWidth: 1))
                .accessibilityLabel("Photo preview for \(state.name)")
                if let source = state.provenance {
                    Text(source.label).metaText(Theme.inkSecondary).lineLimit(2).textSelection(.enabled)
                    if let url = source.sourceURL { Link(url.host ?? "Source", destination: url).font(.system(size: 12)) }
                }
            }
            .frame(maxWidth: .infinity)
            if state.preview != nil {
                VStack(spacing: 8) {
                    cropSlider("Zoom", value: cropBinding(\.zoom), range: 1...3)
                    cropSlider("Horizontal", value: cropBinding(\.horizontal), range: 0...1)
                    cropSlider("Vertical", value: cropBinding(\.vertical), range: 0...1)
                }
                .disabled(state.saving)
                if state.lowResolution { Text("This source is small and may look soft.").metaText() }
                Toggle("This is \(state.name)", isOn: $state.identityConfirmed).disabled(state.saving)
            }
            if state.preview != nil {
                Text("Saving keeps the original photo and its metadata.").metaText()
            }
            if let error = state.error { Text(error).font(.system(size: 12)).foregroundStyle(.red).textSelection(.enabled) }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            HStack {
                Button("Cancel", action: close).keyboardShortcut(.cancelAction).disabled(state.saving)
                Spacer()
                if state.saving { ProgressView().controlSize(.small) }
                Button("Save photo", action: state.save).buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction).disabled(!state.canSave)
            }
        }
        .padding(24)
        .frame(width: 500, height: 660, alignment: .leading)
        .background(Theme.page)
        .foregroundStyle(Theme.ink)
    }

    private func cropBinding(_ key: WritableKeyPath<PortraitCrop, Double>) -> Binding<Double> {
        Binding(get: { state.crop[keyPath: key] }, set: { state.crop[keyPath: key] = $0; state.renderCrop() })
    }

    private func cropSlider(_ label: String, value: Binding<Double>, range: ClosedRange<Double>) -> some View {
        HStack {
            Text(label).font(.system(size: 12)).frame(width: 70, alignment: .leading)
            Slider(value: value, in: range).accessibilityLabel(label)
        }
    }
}
