import AppKit
import SwiftUI

private struct PhoneThoughtSource: Decodable {
    let reference: PhoneThoughtReference
    let manifest: PhoneThoughtManifest
}

private struct PhoneThoughtManifest: Decodable {
    let capturedAtUtcMs: Int64
    let origin: String
    let text: String
    let audio: PhoneThoughtAudio?
    let images: [PhoneThoughtImage]
}

private struct PhoneThoughtAudio: Decodable {
    let durationMs: Int64
}

private struct PhoneThoughtImage: Decodable {
    let mime: String
    let width: Int
    let height: Int
}

struct PhoneThoughtSourceSheet: View {
    let reference: PhoneThoughtReference
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var source: PhoneThoughtSource?
    @State private var photo: NSImage?
    @State private var photoIndex = 0
    @State private var loading = false
    @State private var failure: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Original iPhone thought").titleText()
                    Text("Captured \(FeedClock.ago(reference.capturedAtUtcMs)) · \(reference.origin.capitalized)")
                        .metaText()
                }
                Spacer()
                Button("Done") { dismiss() }.buttonStyle(.compact)
            }
            if loading {
                ProgressView("Opening saved original…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let source {
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        PageSection("Words from the phone") {
                            Card {
                                Text(source.manifest.text.isEmpty ? "No typed or transcribed words in this capture." : source.manifest.text)
                                    .bodyText()
                                    .textSelection(.enabled)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .padding(16)
                            }
                        }
                        if let audio = source.manifest.audio {
                            Label("Original voice recording · \(audio.durationMs / 1_000) seconds", systemImage: "waveform")
                                .bodyText(13)
                            Text("The recording is kept with this source. It is not part of the editable note.")
                                .metaText()
                        }
                        if !source.manifest.images.isEmpty {
                            PageSection(source.manifest.images.count == 1 ? "Original photo" : "Original photos") {
                                Card {
                                    VStack(alignment: .leading, spacing: 12) {
                                        if let photo {
                                            Image(nsImage: photo)
                                                .resizable()
                                                .scaledToFit()
                                                .frame(maxWidth: .infinity, maxHeight: 330)
                                                .accessibilityLabel("Original photo \(photoIndex + 1)")
                                        } else {
                                            Text(failure ?? "Photo is loading…").metaText()
                                                .frame(maxWidth: .infinity, minHeight: 120)
                                        }
                                        if source.manifest.images.count > 1 {
                                            HStack {
                                                Text("\(photoIndex + 1) of \(source.manifest.images.count)").metaText()
                                                Spacer()
                                                Button("Previous") { photoIndex -= 1 }
                                                    .disabled(photoIndex == 0)
                                                Button("Next") { photoIndex += 1 }
                                                    .disabled(photoIndex + 1 == source.manifest.images.count)
                                            }
                                            .buttonStyle(.quiet)
                                        }
                                    }
                                    .padding(16)
                                }
                            }
                        }
                    }
                }
            } else {
                ErrorNote(failure ?? "The saved original could not be opened.")
                Spacer()
            }
        }
        .padding(24)
        .frame(width: 620, height: 600)
        .background(Theme.page)
        .task(id: reference.id) { await loadSource() }
        .task(id: photoIndex) { await loadPhoto() }
    }

    private func loadSource() async {
        loading = true
        failure = nil
        do {
            let result: PhoneThoughtSource? = try await model.core.request("phone_thought_source_get", [
                "noteId": reference.noteId,
                "objectId": reference.objectId,
                "revisionId": reference.revisionId
            ])
            guard !Task.isCancelled else { return }
            source = result
            if result == nil { failure = "The original was removed or is not available on this Mac." }
            loading = false
            if result != nil { await loadPhoto() }
        } catch {
            guard !Task.isCancelled else { return }
            failure = "The saved original could not be opened. \(error.localizedDescription)"
            loading = false
        }
    }

    private func loadPhoto() async {
        guard let source, !source.manifest.images.isEmpty,
              source.manifest.images.indices.contains(photoIndex) else { return }
        photo = nil
        failure = nil
        do {
            let request: [String: JSONValue] = [
                "noteId": .string(reference.noteId),
                "objectId": .string(reference.objectId),
                "revisionId": .string(reference.revisionId),
                "index": .number(Double(photoIndex))
            ]
            let encoded: String = try await model.core.request("phone_thought_photo_get", request)
            guard !Task.isCancelled else { return }
            guard let bytes = Data(base64Encoded: encoded), let image = NSImage(data: bytes) else {
                failure = "The saved photo could not be read."
                return
            }
            photo = image
        } catch {
            guard !Task.isCancelled else { return }
            failure = "This photo could not be opened. \(error.localizedDescription)"
        }
    }
}
