import SwiftUI

/// The pill that floats above other windows while you talk. Only the sound:
/// eleven bars, the last quarter second of the microphone, newest on the right.
/// Flat while nothing is being heard. No text, no buttons.
struct HUDPill: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(spacing: 3) {
            ForEach(Array(model.meter.history.enumerated()), id: \.offset) { _, level in
                RoundedRectangle(cornerRadius: 1)
                    .fill(Theme.onInvert)
                    .frame(width: 2, height: 2 + 12 * level)
            }
        }
        .frame(height: 14)
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(Theme.invert, in: Capsule())
        .padding(8)
    }
}

/// The window the pill lives in. It exists only while a recording is on, at the
/// bottom of the screen the pointer is on, above other windows and on every
/// space. A non-activating panel: showing it never takes the keyboard away from
/// the app the words are going into, which a SwiftUI `Window` scene would.
@MainActor
final class PillPanel {
    private var panel: NSPanel?

    func show(_ model: AppModel) {
        let panel = panel ?? make(model)
        self.panel = panel
        if let screen = NSScreen.screens.first(where: { $0.frame.contains(NSEvent.mouseLocation) }) ?? NSScreen.main {
            let visible = screen.visibleFrame
            panel.setFrameOrigin(NSPoint(
                x: visible.midX - panel.frame.width / 2,
                y: visible.minY + 24))
        }
        panel.orderFrontRegardless()
    }

    func hide() {
        panel?.orderOut(nil)
    }

    private func make(_ model: AppModel) -> NSPanel {
        let panel = NSPanel(
            contentRect: .zero,
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: true)
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.hidesOnDeactivate = false
        let content = NSHostingView(rootView: HUDPill().environment(model))
        content.sizingOptions = .intrinsicContentSize
        panel.contentView = content
        panel.setContentSize(content.fittingSize)
        return panel
    }
}

struct ShortcutRecorderSheet: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Set recording shortcut")
                .font(TypeScale.title)
                .foregroundStyle(Theme.ink)
            capture
                .frame(maxWidth: .infinity, minHeight: 88)
                .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.radiusCard)
                        .strokeBorder(Theme.border, lineWidth: 1))
            Text(instruction)
                .metaText()
                .frame(maxWidth: .infinity, alignment: .leading)
            HStack(spacing: 10) {
                Button("Cancel", action: model.cancelShortcutCapture)
                    .buttonStyle(.secondary)
                    .disabled(model.shortcutRecorder.isSaving)
                Spacer()
                if model.shortcutRecorder.canConfirm {
                    Button("Use shortcut", action: model.confirmShortcutCapture)
                        .buttonStyle(.primary)
                } else if model.shortcutRecorder.isSaving {
                    ProgressView()
                        .controlSize(.small)
                }
            }
        }
        .padding(28)
        .frame(width: 440)
        .background(Theme.page)
        .interactiveDismissDisabled(model.shortcutRecorder.isSaving)
    }

    @ViewBuilder
    private var capture: some View {
        if let chord = model.shortcutRecorder.chord {
            Shortcut(chord)
        } else if model.shortcutRecorder == .starting {
            ProgressView()
                .controlSize(.small)
        } else {
            Text("Hold a new shortcut")
                .font(TypeScale.headline)
                .foregroundStyle(Theme.ink)
        }
    }

    private var instruction: String {
        switch model.shortcutRecorder {
        case .closed, .starting: "Opening the key recorder."
        case let .listening(candidate, _):
            candidate.isEmpty ? "Hold the new shortcut." : "Release the keys to capture it."
        case .stopping: "Checking the shortcut."
        case let .ready(_, error): error ?? "Use this shortcut, or cancel to keep the current one."
        case .saving: "Saving the shortcut."
        }
    }
}

/// Asked once, before a word of a call is kept.
struct ConsentPanel: View {
    @Environment(AppModel.self) private var model
    @State private var announce = true

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Meeting noticed · Zoom").metaText()
            Text("Dana and Priya are on a call with you.")
                .font(.system(size: 22, weight: .semibold))
                .foregroundStyle(Theme.ink)
                .fixedSize(horizontal: false, vertical: true)
            Text("Sona can record and transcribe it on this Mac. Nothing is sent anywhere, and the recording is announced to the others before it starts.")
                .bodyText(15, Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
            Card {
                ToggleRow(title: "Announce in the call's chat", isOn: $announce)
            }
            HStack(spacing: 10) {
                Button {
                    model.toggleCapture()
                } label: {
                    Label("Record", systemImage: "video")
                }
                .buttonStyle(.primary)
                Button("Not this one") {}.buttonStyle(.secondary)
                Spacer()
                Button("Never for this series") {}.buttonStyle(.quiet)
            }
            .padding(.top, 4)
        }
        .padding(28)
        .frame(width: 480)
        .background(Theme.page)
    }
}

/// Three steps, one at a time, one action each.
struct Onboarding: View {
    @State private var step = 0

    private let steps: [(title: String, headline: String, fact: String, action: String)] = [
        ("Accessibility", "Let Sona type for you.",
         "Sona pastes what you said into the app in front. macOS calls this accessibility access. Nothing is read back; Sona only types.",
         "Open System Settings"),
        ("Microphone", "Let Sona hear you.",
         "Audio is transcribed on this Mac and kept for a day, then removed. It is never uploaded.",
         "Allow the microphone"),
        ("Model", "Download one model.",
         "Whisper Large v3 is 3.1 GB and transcribes English and ninety-eight other languages. Smaller ones come later if you want them.",
         "Download · 3.1 GB"),
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                ForEach(Array(steps.enumerated()), id: \.offset) { index, item in
                    Text("\(index + 1)  \(item.title)")
                        .font(TypeScale.label(13))
                        .foregroundStyle(index == step ? Theme.ink : Theme.inkTertiary)
                        .padding(.horizontal, 10)
                        .frame(height: 28)
                        .background(index == step ? Theme.selection : .clear, in: RoundedRectangle(cornerRadius: 8))
                }
            }
            .padding(.bottom, 48)
            Text(steps[step].headline)
                .titleText()
                .padding(.bottom, 12)
            Text(steps[step].fact)
                .bodyText(15, Theme.inkSecondary)
                .frame(maxWidth: 460, alignment: .leading)
                .padding(.bottom, 28)
            HStack(spacing: 10) {
                Button(steps[step].action) {
                    step = min(step + 1, steps.count - 1)
                }
                .buttonStyle(.primary)
                if step > 0 {
                    Button("Back") { step -= 1 }.buttonStyle(.secondary)
                }
            }
            Spacer()
            Text("Step \(step + 1) of \(steps.count)").metaText()
        }
        .padding(40)
        .padding(.top, 12)
        .frame(width: 640, height: 440, alignment: .topLeading)
        .background(Theme.page)
    }
}

/// ⌘K. A floating panel: one search field and the rows it finds.
struct CommandPalette: View {
    @Environment(AppModel.self) private var model
    @State private var query = ""
    @FocusState private var focused: Bool

    var body: some View {
        ZStack(alignment: .top) {
            Color.black.opacity(0.18)
                .ignoresSafeArea()
                .onTapGesture { model.paletteShown = false }
            VStack(spacing: 0) {
                HStack(spacing: 10) {
                    Image(systemName: "magnifyingglass")
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(Theme.inkTertiary)
                    TextField("", text: $query, prompt: Text("Search or do anything").foregroundStyle(Theme.inkTertiary))
                        .textFieldStyle(.plain)
                        .font(TypeScale.body(16))
                        .foregroundStyle(Theme.ink)
                        .focused($focused)
                        .onSubmit { model.paletteShown = false }
                    KeyCap("esc")
                }
                .padding(.horizontal, 18)
                .frame(height: 52)
                Hairline()
                VStack(spacing: 0) {
                    ForEach(results) { command in
                        Button {
                            model.paletteShown = false
                        } label: {
                            HStack(alignment: .firstTextBaseline, spacing: 10) {
                                Text(command.title).bodyText()
                                if !command.detail.isEmpty {
                                    Text(command.detail).metaText()
                                }
                                Spacer()
                                if !command.shortcut.isEmpty {
                                    Shortcut(command.shortcut)
                                }
                            }
                            .padding(.horizontal, 18)
                            .frame(height: 42)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.vertical, 6)
            }
            .frame(width: 600)
            .background(Theme.surface)
            .clipShape(RoundedRectangle(cornerRadius: Theme.radiusPanel))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusPanel).strokeBorder(Theme.border, lineWidth: 1))
            .shadow(color: .black.opacity(0.18), radius: 24, y: 12)
            .padding(.top, 96)
            .onAppear { focused = true }
            .onExitCommand { model.paletteShown = false }
        }
    }

    private var results: [PaletteCommand] {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return SampleData.commands }
        return SampleData.commands.filter {
            $0.title.localizedCaseInsensitiveContains(trimmed) || $0.detail.localizedCaseInsensitiveContains(trimmed)
        }
    }
}

/// Ask a meeting a question. Answers come from the transcript on this Mac.
struct ChatSheet: View {
    @Environment(AppModel.self) private var model
    @State private var draft = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Retention proposal review").headlineText()
                    Text("Answers come from the transcript on this Mac.").metaText()
                }
                Spacer()
                Button("Done") { model.chatShown = false }.buttonStyle(.secondary)
            }
            .padding(.bottom, 24)
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ForEach(SampleData.chat) { turn in
                        Text(turn.text)
                            .bodyText(15, turn.fromUser ? Theme.onInvert : Theme.ink)
                            .fixedSize(horizontal: false, vertical: true)
                            .padding(.horizontal, 14)
                            .padding(.vertical, 10)
                            .background(turn.fromUser ? Theme.invert : Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
                            .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard).strokeBorder(turn.fromUser ? .clear : Theme.border, lineWidth: 1))
                            .frame(maxWidth: 480, alignment: turn.fromUser ? .trailing : .leading)
                            .frame(maxWidth: .infinity, alignment: turn.fromUser ? .trailing : .leading)
                    }
                }
                .padding(.bottom, 16)
            }
            .scrollIndicators(.never)
            HStack(spacing: 10) {
                TextField("", text: $draft, prompt: Text("Ask anything about this meeting").foregroundStyle(Theme.inkTertiary))
                    .textFieldStyle(.plain)
                    .font(TypeScale.body())
                    .foregroundStyle(Theme.ink)
                KeyCap("↩")
            }
            .padding(.horizontal, 14)
            .frame(height: 44)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
        }
        .padding(28)
        .frame(width: 640, height: 540)
        .background(Theme.page)
    }
}
