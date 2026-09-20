import PhotosUI
import SwiftUI
import UIKit

/// Where a thought enters: said aloud, typed, pasted as a link, or picked as photos.
struct CaptureScreen: View {
    @ObservedObject var model: AppModel
    @ObservedObject var dictation: PhoneDictation
    /* Observed for the recording state behind `model.canStartDictation`. */
    @ObservedObject var recorder: PhoneRecorder
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @ScaledMetric(relativeTo: .body) private var controlSize: CGFloat = 72
    @State private var draft = ""
    @State private var picked: [PickedImage] = []
    @State private var selection: [PhotosPickerItem] = []
    @State private var kept = false

    private struct PickedImage: Identifiable {
        let captured: CapturedImage
        let thumbnail: UIImage?
        var id: URL { captured.url }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text("capture.title")
                    .font(.title2.weight(.semibold))
                    .foregroundStyle(Theme.textPrimary)
                voice
                composer
                if !model.canStartDictation {
                    Text("capture.stopMeeting")
                        .font(.footnote)
                        .foregroundStyle(Theme.textSecondary)
                }
                if let notice = dictation.notice {
                    Text(notice)
                        .font(.footnote)
                        .foregroundStyle(Theme.recording)
                }
                if !model.isPaired {
                    Text("capture.notPaired")
                        .font(.footnote)
                        .foregroundStyle(Theme.textSecondary)
                } else if kept {
                    Text("capture.kept")
                        .font(.footnote)
                        .foregroundStyle(Theme.success)
                        .accessibilityIdentifier("capture-kept")
                }
            }
            .padding(24)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(Theme.background)
        .tint(Theme.accent)
        .onChange(of: selection) { _, items in
            guard !items.isEmpty else { return }
            selection = []
            Task { await add(items) }
        }
        .onChange(of: model.thoughtsKept) { _, _ in kept = true }
        /* Leaving mid-sentence keeps what was said: a thought is not a draft. */
        .onChange(of: scenePhase) { _, phase in
            if phase == .background { dictation.finish() }
        }
        .onDisappear { dictation.finish() }
    }

    /// The transcript as it forms, over one round control that starts and finishes.
    private var voice: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(dictation.isBusy ? dictation.text : "")
                .font(.body)
                .foregroundStyle(Theme.textPrimary)
                .frame(maxWidth: .infinity, minHeight: 44, alignment: .topLeading)
                .overlay(alignment: .topLeading) {
                    if !dictation.isBusy {
                        Text("capture.voiceHint")
                            .font(.subheadline)
                            .foregroundStyle(Theme.textSecondary)
                    } else if dictation.text.isEmpty {
                        Text("capture.listening")
                            .font(.subheadline)
                            .foregroundStyle(Theme.textTertiary)
                    }
                }
                .accessibilityIdentifier("capture-transcript")
            HStack(spacing: 20) {
                Button {
                    if dictation.phase == .listening {
                        dictation.finish()
                    } else {
                        kept = false
                        model.startVoiceThought()
                    }
                } label: {
                    ZStack {
                        Circle().strokeBorder(Theme.border, lineWidth: 1)
                        if dictation.phase == .listening {
                            RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous)
                                .fill(Theme.recording)
                                .frame(width: controlSize * 0.34, height: controlSize * 0.34)
                        } else {
                            Image(systemName: "mic.fill")
                                .font(.system(size: controlSize * 0.36, weight: .medium))
                                .foregroundStyle(Theme.recording)
                        }
                    }
                    .frame(width: controlSize, height: controlSize)
                    .contentShape(Circle())
                }
                .buttonStyle(PressScaleButtonStyle(reduceMotion: reduceMotion))
                .disabled(
                    dictation.phase == .authorizing || dictation.phase == .finishing
                        || (!dictation.isBusy && !model.canStartDictation)
                )
                .accessibilityLabel(
                    Text(dictation.phase == .listening ? "capture.voiceStop" : "capture.voiceStart")
                )
                .accessibilityIdentifier(dictation.phase == .listening ? "voice-stop" : "voice-start")
                if dictation.phase == .authorizing || dictation.phase == .finishing {
                    ProgressView()
                }
                if dictation.isBusy {
                    Button("capture.cancel", action: dictation.cancel)
                        .frame(minHeight: 44)
                        .accessibilityIdentifier("voice-cancel")
                }
            }
        }
        .padding(16)
        .background(Theme.inset)
        .clipShape(RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous))
    }

    /// Typed text, links and photos share one draft and one Keep.
    private var composer: some View {
        VStack(alignment: .leading, spacing: 12) {
            TextEditor(text: $draft)
                .scrollContentBackground(.hidden)
                .padding(8)
                .frame(minHeight: 120)
                .background(Theme.inset)
                .clipShape(RoundedRectangle(cornerRadius: Theme.controlRadius))
                .overlay(alignment: .topLeading) {
                    if draft.isEmpty {
                        Text("capture.placeholder")
                            .foregroundStyle(Theme.textTertiary)
                            .padding(.horizontal, 13)
                            .padding(.vertical, 16)
                            .allowsHitTesting(false)
                    }
                }
                .accessibilityIdentifier("capture-draft")
            if !picked.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(picked) { image in
                            thumbnail(image)
                        }
                    }
                }
            }
            HStack(spacing: 16) {
                PhotosPicker(
                    selection: $selection, maxSelectionCount: 8, matching: .images
                ) {
                    Label("capture.addPhotos", systemImage: "photo.on.rectangle")
                        .frame(minHeight: 44)
                }
                .accessibilityIdentifier("capture-photos")
                Spacer()
                Button(action: keep) {
                    Text("capture.save")
                        .frame(minWidth: 88, minHeight: 44)
                }
                .buttonStyle(.borderedProminent)
                .disabled(
                    draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && picked.isEmpty
                )
                .accessibilityIdentifier("capture-keep")
            }
        }
    }

    private func thumbnail(_ image: PickedImage) -> some View {
        ZStack(alignment: .topTrailing) {
            Group {
                if let thumbnail = image.thumbnail {
                    Image(uiImage: thumbnail).resizable().scaledToFill()
                } else {
                    Theme.inset
                }
            }
            .frame(width: 88, height: 88)
            .clipShape(RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous))
            Button {
                picked.removeAll { $0.id == image.id }
                try? FileManager.default.removeItem(at: image.captured.url)
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.title3)
                    .foregroundStyle(Theme.onAccent, Theme.textPrimary)
                    .padding(4)
            }
            .accessibilityLabel(Text("capture.removePhoto"))
        }
    }

    private func add(_ items: [PhotosPickerItem]) async {
        kept = false
        for item in items {
            guard let captured = await AppModel.prepare(item) else { continue }
            let thumbnail = UIImage(contentsOfFile: captured.url.path)?
                .preparingThumbnail(of: CGSize(width: 176, height: 176))
            picked.append(PickedImage(captured: captured, thumbnail: thumbnail))
        }
    }

    private func keep() {
        guard model.captureTyped(draft, images: picked.map(\.captured)) else { return }
        draft = ""
        picked = []
    }
}
