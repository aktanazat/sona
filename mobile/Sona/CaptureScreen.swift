import PhotosUI
import SwiftUI
import UIKit

/// Where a thought enters: said aloud, typed, pasted as a link, or picked as photos.
struct CaptureScreen: View {
    @ObservedObject var model: AppModel
    @ObservedObject var dictation: PhoneDictation
    /* Observed for the recording state behind `model.canStartDictation`. */
    @ObservedObject var recorder: PhoneRecorder
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @ScaledMetric(relativeTo: .body) private var controlSize: CGFloat = 44
    @State private var draft = ""
    @FocusState private var draftFocused: Bool
    @State private var picked: [PickedImage] = []
    @State private var selection: [PhotosPickerItem] = []
    @State private var kept = false

    private struct PickedImage: Identifiable {
        let captured: CapturedImage
        let thumbnail: UIImage?
        var id: URL { captured.url }
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
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
                            .foregroundStyle(Theme.textSecondary)
                            .accessibilityIdentifier("capture-kept")
                    }
                }
                .padding(.horizontal, Theme.pageInset)
                .padding(.vertical, 8)
            }
            .scrollDismissesKeyboard(.interactively)
            .background(Theme.background)
            .navigationTitle("tab.capture")
            .navigationBarTitleDisplayMode(.inline)
        }
        .tint(Theme.accent)
        .onChange(of: selection) { _, items in
            guard !items.isEmpty else { return }
            selection = []
            Task { await add(items) }
        }
        .onChange(of: model.thoughtsKept) { _, _ in kept = true }
    }

    private var voice: some View {
        VStack(alignment: .leading, spacing: 16) {
            let layout = dynamicTypeSize.isAccessibilitySize
                ? AnyLayout(VStackLayout(alignment: .leading, spacing: 12))
                : AnyLayout(HStackLayout(spacing: 16))
            layout {
                Button {
                    if dictation.phase == .listening {
                        dictation.finish()
                    } else {
                        kept = false
                        model.startVoiceThought()
                    }
                } label: {
                    RecordPill(glyph: voiceGlyph, height: controlSize)
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
                VStack(alignment: .leading, spacing: 2) {
                    Text(voiceLabel)
                        .font(.body.weight(.semibold))
                        .foregroundStyle(Theme.textPrimary)
                    if dictation.phase == .idle {
                        Text("capture.voiceHint")
                            .font(.footnote)
                            .foregroundStyle(Theme.textSecondary)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                if dictation.isBusy {
                    Button("capture.cancel", action: dictation.cancel)
                        .foregroundStyle(Theme.textSecondary)
                        .frame(minHeight: 44)
                        .accessibilityIdentifier("voice-cancel")
                }
            }
            if dictation.phase == .listening {
                LevelWaveform(meter: dictation.meter, height: 32)
            }
            if dictation.isBusy && !dictation.text.isEmpty {
                Text(dictation.text)
                    .font(.body)
                    .foregroundStyle(Theme.textPrimary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("capture-transcript")
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous))
    }

    private var voiceGlyph: RecordPill.Glyph {
        switch dictation.phase {
        case .idle: return .record
        case .listening: return .stop
        case .authorizing, .finishing: return .busy
        }
    }

    private var voiceLabel: LocalizedStringKey {
        switch dictation.phase {
        case .idle: return "capture.voiceStart"
        case .authorizing: return "dictation.authorizing"
        case .listening: return "capture.listening"
        case .finishing: return "dictation.finishing"
        }
    }

    /// Typed text, links and photos share one draft and one Keep.
    private var composer: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextEditor(text: $draft)
                .focused($draftFocused)
                .font(.body)
                .foregroundStyle(Theme.textPrimary)
                .scrollContentBackground(.hidden)
                .frame(minHeight: 120)
                .overlay(alignment: .topLeading) {
                    if draft.isEmpty {
                        Text("capture.placeholder")
                            .foregroundStyle(Theme.textTertiary)
                            .padding(.top, 8)
                            .padding(.leading, 5)
                            .allowsHitTesting(false)
                    }
                }
                .accessibilityIdentifier("capture-draft")
                .accessibilityLabel(Text("capture.placeholder"))
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
                        .foregroundStyle(Theme.textSecondary)
                        .frame(minHeight: 44)
                }
                .accessibilityIdentifier("capture-photos")
                Spacer()
                Button(action: keep) {
                    Text("capture.save")
                        .font(.body.weight(.semibold))
                        .frame(minWidth: 64)
                }
                .buttonStyle(.borderedProminent)
                .buttonBorderShape(.capsule)
                .controlSize(.large)
                .disabled(
                    draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && picked.isEmpty
                )
                .accessibilityIdentifier("capture-keep")
            }
            .font(.subheadline)
        }
        .padding(.horizontal, 12)
        .padding(.top, 8)
        .padding(.bottom, 12)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous))
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
                    .foregroundStyle(Theme.background, Theme.textPrimary)
                    .frame(minWidth: 44, minHeight: 44)
                    .contentShape(Rectangle())
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
        draftFocused = false
        draft = ""
        picked = []
    }
}
