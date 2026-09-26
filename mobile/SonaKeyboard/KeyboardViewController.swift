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
        VStack(spacing: 12) {
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text("keyboard.title").font(.headline)
                    if !state.hasFullAccess {
                        Text("keyboard.needsFullAccess").font(.footnote)
                        Link("keyboard.openApp", destination: DictationLink.url).frame(minHeight: 44)
                    } else {
                        if state.warm, state.session.phase == .listening {
                            Button(action: stop) {
                                Label("keyboard.stop", systemImage: "stop.fill").frame(maxWidth: .infinity, minHeight: 44)
                            }.buttonStyle(.borderedProminent).tint(Theme.recording)
                            .accessibilityIdentifier("keyboard-stop")
                        } else if state.warm, state.session.phase == .finishing || state.waiting {
                            HStack { ProgressView(); Text("dictation.finishing") }.frame(minHeight: 44)
                        } else if state.warm {
                            Button(action: microphone) {
                                Label("dictation.start", systemImage: "mic.fill").frame(maxWidth: .infinity, minHeight: 44)
                            }.buttonStyle(.borderedProminent).accessibilityIdentifier("keyboard-mic")
                        } else {
                            Link(destination: state.startURL) {
                                Label("dictation.start", systemImage: "mic.fill").frame(maxWidth: .infinity, minHeight: 44)
                            }.buttonStyle(.borderedProminent).accessibilityIdentifier("keyboard-open-sona")
                        }
                        if let draft = state.draft {
                            Text(draft.text).font(.body).lineLimit(1...3)
                            Text("keyboard.otherField").font(.footnote)
                            HStack {
                                Button("keyboard.insert", action: insert).frame(minHeight: 44)
                                Button("keyboard.discard", action: discard).frame(minHeight: 44)
                            }
                        } else if let notice = state.session.message ?? state.notice {
                            Text(notice).font(.footnote).accessibilityIdentifier("keyboard-status")
                        } else {
                            Text(state.warm ? "keyboard.ready" : "keyboard.cold").font(.footnote)
                        }
                    }
                }.frame(maxWidth: .infinity, alignment: .leading)
            }.scrollBounceBehavior(.basedOnSize)
            HStack(spacing: 12) {
                Button(action: nextKeyboard) {
                    Label("keyboard.next", systemImage: "globe").labelStyle(.iconOnly).frame(minWidth: 44, minHeight: 44)
                }
                Button("keyboard.space", action: space).frame(maxWidth: .infinity, minHeight: 44)
                Button(action: delete) {
                    Label("keyboard.delete", systemImage: "delete.left").labelStyle(.iconOnly).frame(minWidth: 44, minHeight: 44)
                }
                Button(action: newLine) {
                    Label("keyboard.return", systemImage: "return").labelStyle(.iconOnly).frame(minWidth: 44, minHeight: 44)
                }
            }.buttonStyle(.bordered)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .foregroundStyle(Theme.textPrimary).background(Theme.inset).tint(Theme.accent)
    }
}
