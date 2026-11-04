import SwiftUI
import UIKit

@MainActor
private final class KeyboardState: ObservableObject {
    @Published var draft: KeyboardDraft?
    @Published var notice: String?
    @Published var hasFullAccess = false
    @Published var session = KeyboardSessionState()
    @Published var startURL = DictationLink.url
    @Published var waiting = false
    var warm: Bool { session.isWarm(at: Date()) }
}

@MainActor
final class KeyboardViewController: UIInputViewController {
    private let state = KeyboardState()
    private var minimumHeight: NSLayoutConstraint?
    private var signal: KeyboardSignal?
    private var ticker: Task<Void, Never>?
    private var visible = false
    private var requestedAt: Date?
    private var requestedID: UUID?
    private var wantedHeight: CGFloat { traitCollection.verticalSizeClass == .compact ? 200 : 280 }

    override func viewDidLoad() {
        super.viewDidLoad()
        signal = KeyboardSignal(.state) { [weak self] in
            Task { @MainActor in self?.reload() }
        }
        let content = KeyboardView(
            state: state,
            microphone: { [weak self] in self?.start() },
            stop: { [weak self] in self?.send(.stop) },
            insert: { [weak self] in self?.insertDraft() },
            discard: { [weak self] in self?.discardDraft() },
            nextKeyboard: { [weak self] in self?.advanceToNextInputMode() },
            space: { [weak self] in self?.textDocumentProxy.insertText(" ") },
            delete: { [weak self] in self?.textDocumentProxy.deleteBackward() },
            newLine: { [weak self] in self?.textDocumentProxy.insertText("\n") }
        )
        let host = UIHostingController(rootView: content)
        addChild(host)
        host.view.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(host.view)
        let minimum = view.heightAnchor.constraint(greaterThanOrEqualToConstant: wantedHeight)
        minimumHeight = minimum
        NSLayoutConstraint.activate([
            host.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            host.view.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            host.view.topAnchor.constraint(equalTo: view.topAnchor),
            host.view.bottomAnchor.constraint(equalTo: view.bottomAnchor), minimum,
        ])
        host.didMove(toParent: self)
    }

    override func viewWillLayoutSubviews() {
        super.viewWillLayoutSubviews()
        if let minimumHeight, minimumHeight.constant != wantedHeight { minimumHeight.constant = wantedHeight }
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        visible = true
        reload()
        ticker?.cancel()
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                guard !Task.isCancelled else { return }
                self?.reload()
            }
        }
    }

    override func viewDidDisappear(_ animated: Bool) {
        super.viewDidDisappear(animated)
        visible = false
        ticker?.cancel()
        ticker = nil
    }

    override func textDidChange(_ textInput: (any UITextInput)?) {
        super.textDidChange(textInput)
        reload()
    }

    private func reload() {
        guard visible else { return }
        state.hasFullAccess = hasFullAccess
        state.startURL = DictationLink.keyboardURL(documentID: textDocumentProxy.documentIdentifier)
        guard hasFullAccess else {
            state.draft = nil
            state.session = KeyboardSessionState()
            state.notice = nil
            return
        }
        do {
            state.session = try KeyboardSessionStore.shared().state()
            if let requestedAt {
                if state.session.requestID == requestedID {
                    self.requestedAt = nil
                    state.waiting = false
                } else if Date().timeIntervalSince(requestedAt) >= 3 {
                    state.session.heartbeat = .distantPast
                    state.waiting = false
                    state.notice = NSLocalizedString("keyboard.sessionEnded", comment: "")
                }
            }
            switch try KeyboardDraftStore.shared().load() {
            case .waiting(let draft):
                state.draft = draft
                // Never send a delayed result to a different text field or app.
                if draft.documentID == textDocumentProxy.documentIdentifier { insertDraft() }
            case .expired:
                state.draft = nil
                state.notice = KeyboardDraftError.expired.localizedDescription
            case .missing:
                state.draft = nil
            }
        } catch { state.notice = error.localizedDescription }
    }

    private func start() {
        guard hasFullAccess, state.warm else { reload(); return }
        let id = UUID()
        requestedID = id
        requestedAt = Date()
        state.waiting = true
        state.notice = nil
        do {
            try KeyboardSessionStore.shared().send(KeyboardCommand(
                id: UUID(), action: .start, requestID: id,
                documentID: textDocumentProxy.documentIdentifier, createdAt: Date()
            ))
        } catch { state.waiting = false; state.notice = error.localizedDescription }
    }

    private func send(_ action: KeyboardCommand.Action) {
        guard hasFullAccess, let id = state.session.requestID else { return }
        do {
            try KeyboardSessionStore.shared().send(KeyboardCommand(
                id: UUID(), action: action, requestID: id,
                documentID: textDocumentProxy.documentIdentifier, createdAt: Date()
            ))
        } catch { state.notice = error.localizedDescription }
    }

    private func insertDraft() {
        guard visible, hasFullAccess, let draft = state.draft else { return }
        do {
            let text = try KeyboardDraftStore.shared().take(id: draft.id)
            state.draft = nil
            textDocumentProxy.insertText(text)
            state.notice = NSLocalizedString("keyboard.inserted", comment: "")
        } catch { state.draft = nil; state.notice = error.localizedDescription }
    }

    private func discardDraft() {
        guard hasFullAccess, let draft = state.draft else { return }
        do {
            try KeyboardDraftStore.shared().discard(id: draft.id)
            state.draft = nil
        } catch { state.notice = error.localizedDescription }
    }
}

private struct KeyboardView: View {
    @ObservedObject var state: KeyboardState
    let microphone: () -> Void
    let stop: () -> Void
    let insert: () -> Void
    let discard: () -> Void
    let nextKeyboard: () -> Void
    let space: () -> Void
    let delete: () -> Void
    let newLine: () -> Void

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                SonaMark()
                    .frame(width: 26, height: 15)
                    .foregroundStyle(Theme.textPrimary)
                    .accessibilityHidden(true)
                Text("keyboard.title")
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(1)
                Spacer(minLength: 8)
                if state.hasFullAccess {
                    control
                }
            }
            .frame(minHeight: 44)
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    if !state.hasFullAccess {
                        Text("keyboard.needsFullAccess").font(.footnote).foregroundStyle(Theme.textSecondary)
                        Link("keyboard.openApp", destination: DictationLink.url).frame(minHeight: 44)
                    } else if let draft = state.draft {
                        Text(draft.text).font(.body).lineLimit(1...3)
                        Text("keyboard.otherField").font(.footnote).foregroundStyle(Theme.textSecondary)
                        HStack(spacing: 8) {
                            Button("keyboard.insert", action: insert)
                                .buttonStyle(.borderedProminent)
                                .foregroundStyle(.white)
                            Button("keyboard.discard", action: discard)
                                .buttonStyle(.bordered)
                                .tint(Theme.textSecondary)
                        }
                        .buttonBorderShape(.capsule)
                        .frame(minHeight: 44)
                    } else if let notice = state.session.message ?? state.notice {
                        Text(notice).font(.footnote).foregroundStyle(Theme.textSecondary)
                            .accessibilityIdentifier("keyboard-status")
                    } else {
                        Text(state.warm ? "keyboard.ready" : "keyboard.cold").font(.footnote)
                            .foregroundStyle(Theme.textSecondary)
                    }
                }.frame(maxWidth: .infinity, alignment: .leading)
            }.scrollBounceBehavior(.basedOnSize)
            HStack(spacing: 8) {
                Button(action: nextKeyboard) {
                    Label("keyboard.next", systemImage: "globe").labelStyle(.iconOnly).frame(minWidth: 52, minHeight: 44)
                }
                Button(action: space) {
                    Text("keyboard.space").frame(maxWidth: .infinity, minHeight: 44)
                }
                Button(action: delete) {
                    Label("keyboard.delete", systemImage: "delete.left").labelStyle(.iconOnly).frame(minWidth: 52, minHeight: 44)
                }
                Button(action: newLine) {
                    Label("keyboard.return", systemImage: "return").labelStyle(.iconOnly).frame(minWidth: 52, minHeight: 44)
                }
            }.buttonStyle(KeyStyle())
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .foregroundStyle(Theme.textPrimary).background(Theme.inset).tint(Theme.accent)
    }

    @ViewBuilder
    private var control: some View {
        if state.warm, state.session.phase == .listening {
            Button(action: stop) {
                RecordPill(glyph: .stop, title: "keyboard.stop", height: 40)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("keyboard-stop")
        } else if state.warm, state.session.phase == .finishing || state.waiting {
            RecordPill(glyph: .busy, title: "dictation.finishing", height: 40)
                .disabled(true)
        } else if state.warm {
            Button(action: microphone) {
                RecordPill(glyph: .record, title: "dictation.start", height: 40)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("keyboard-mic")
        } else {
            Link(destination: state.startURL) {
                RecordPill(glyph: .record, title: "dictation.start", height: 40)
            }
            .accessibilityIdentifier("keyboard-open-sona")
        }
    }
}

private struct KeyStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(
                configuration.isPressed ? Theme.border : Theme.surface,
                in: RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous)
            )
    }
}

/// Sona's mark, traced from the app icon in its 1024-point space.
private struct SonaMark: View {
    var body: some View {
        Canvas { context, size in
            let scale = min(size.width / 678, size.height / 383)
            let transform = CGAffineTransform(scaleX: scale, y: scale).translatedBy(x: -173, y: -320)
            let dot = Path(ellipseIn: CGRect(x: 173, y: 458, width: 108, height: 108))
            var wave = Path()
            wave.move(to: CGPoint(x: 404, y: 512))
            wave.addCurve(to: CGPoint(x: 481, y: 374), control1: CGPoint(x: 420, y: 440), control2: CGPoint(x: 450, y: 374))
            wave.addCurve(to: CGPoint(x: 620, y: 649), control1: CGPoint(x: 530, y: 374), control2: CGPoint(x: 570, y: 649))
            wave.addCurve(to: CGPoint(x: 760, y: 472), control1: CGPoint(x: 680, y: 649), control2: CGPoint(x: 720, y: 472))
            wave.addLine(to: CGPoint(x: 797, y: 472))
            context.fill(dot.applying(transform), with: .foreground)
            context.stroke(
                wave.applying(transform),
                with: .foreground,
                style: StrokeStyle(lineWidth: 108 * scale, lineCap: .round, lineJoin: .round)
            )
        }
    }
}
