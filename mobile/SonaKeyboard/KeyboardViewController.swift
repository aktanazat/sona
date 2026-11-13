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
    private let typing = TypingModel()
    private var session = TypingSession(corrector: nil)
    private var lastShiftTap: Date?
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
        session = TypingSession(corrector: TypingCorrector(language: primaryLanguage ?? "en-US"))
        typing.press = { [weak self] key in self?.press(key) }
        typing.choose = { [weak self] word in self?.choose(word) }
        requestSupplementaryLexicon { [weak self] lexicon in
            let entries = lexicon.entries.map { (shortcut: $0.userInput, text: $0.documentText) }
            Task { @MainActor in
                entries.forEach { self?.session.lexicon.add(shortcut: $0.shortcut, text: $0.text) }
            }
        }
        let content = KeyboardView(
            state: state,
            typing: typing,
            microphone: { [weak self] in self?.start() },
            stop: { [weak self] in self?.send(.stop) },
            insert: { [weak self] in self?.insertDraft() },
            discard: { [weak self] in self?.discardDraft() }
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

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        typing.hasTyped = false
        if hasFullAccess { loadVocabulary() }
        refreshTyping(startingField: true)
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
        refreshTyping(startingField: false)
    }

    private var textBefore: String { textDocumentProxy.documentContextBeforeInput ?? "" }

    /// Correction is off where the field asks for it, and in fields for passwords, numbers,
    /// addresses and links, where a dictionary word is rarely what was meant.
    private var corrects: Bool {
        let proxy = textDocumentProxy
        guard proxy.autocorrectionType != .no, proxy.isSecureTextEntry != true else { return false }
        switch proxy.keyboardType ?? .default {
        case .URL, .emailAddress, .numberPad, .phonePad, .namePhonePad, .decimalPad, .asciiCapableNumberPad: return false
        default: return true
        }
    }

    private func press(_ key: Key) {
        typing.hasTyped = true
        state.notice = nil
        switch key {
        case let .character(text):
            let character = typing.layer == .letters && typing.shift != .off ? text.uppercased() : text
            if ".,!?;:)\"".contains(character) {
                apply(session.end(with: character, before: textBefore, corrects: corrects && cursorEndsWord))
            } else {
                apply(session.type(character))
            }
            if character == "'" { typing.layer = .letters }
        case .space, .newLine:
            let separator = key == .space ? " " : "\n"
            apply(session.end(with: separator, before: textBefore, corrects: corrects && cursorEndsWord))
            typing.layer = .letters
        case .delete:
            apply(session.backspace(before: textBefore))
        case .shift:
            toggleShift()
            return
        case let .layer(layer):
            typing.layer = layer
            return
        case .nextKeyboard:
            advanceToNextInputMode()
            return
        }
        refreshShift()
        refreshSuggestions()
    }

    private func choose(_ word: String) {
        typing.hasTyped = true
        apply(session.choose(word, before: textBefore))
        refreshShift()
        refreshSuggestions()
    }

    private func apply(_ edit: TypingSession.Edit) {
        for _ in 0..<edit.deleteCount { textDocumentProxy.deleteBackward() }
        if !edit.insert.isEmpty { textDocumentProxy.insertText(edit.insert) }
    }

    /// A word with letters right after the cursor is being edited in the middle, so ending
    /// it here must not rewrite it.
    private var cursorEndsWord: Bool {
        guard let next = textDocumentProxy.documentContextAfterInput?.first else { return true }
        return !TypingText.isWordCharacter(next)
    }

    private func toggleShift() {
        let now = Date()
        if let lastShiftTap, now.timeIntervalSince(lastShiftTap) < 0.35 {
            typing.shift = .locked
            self.lastShiftTap = nil
        } else {
            typing.shift = typing.shift == .off ? .once : .off
            lastShiftTap = now
        }
    }

    private func refreshTyping(startingField: Bool) {
        let proxy = textDocumentProxy
        typing.returnKey = ReturnKey(proxy.returnKeyType ?? .default)
        typing.showsNextKeyboard = needsInputModeSwitchKey
        switch proxy.keyboardType ?? .default {
        case .emailAddress: typing.extraKeys = ["@", "."]
        case .URL, .webSearch: typing.extraKeys = ["/", "."]
        default: typing.extraKeys = []
        }
        if startingField {
            switch proxy.keyboardType ?? .default {
            case .numberPad, .phonePad, .decimalPad, .asciiCapableNumberPad, .numbersAndPunctuation:
                typing.layer = .numbers
            default:
                typing.layer = .letters
            }
        }
        refreshShift()
        refreshSuggestions()
    }

    private func refreshShift() {
        guard typing.shift != .locked else { return }
        let before = textBefore
        let capitalizes = switch textDocumentProxy.autocapitalizationType ?? .sentences {
        case .allCharacters: true
        case .words: before.last.map(\.isWhitespace) ?? true
        case .sentences: TypingText.startsSentence(before: before[...])
        case .none: false
        @unknown default: false
        }
        typing.shift = capitalizes ? .once : .off
    }

    private func refreshSuggestions() {
        typing.suggestions = corrects ? session.suggestions(before: textBefore) : nil
    }

    private func loadVocabulary() {
        do {
            try KeyboardVocabularyStore.shared().load().forEach { session.lexicon.insert($0) }
        } catch { state.notice = error.localizedDescription }
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
    let typing: TypingModel
    let microphone: () -> Void
    let stop: () -> Void
    let insert: () -> Void
    let discard: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                status.frame(maxWidth: .infinity, alignment: .leading)
                if state.hasFullAccess {
                    control
                } else {
                    Link("keyboard.openApp", destination: DictationLink.url)
                        .font(.footnote.weight(.semibold))
                        .frame(minHeight: 44)
                }
            }
            .frame(height: 48)
            .padding(.horizontal, 8)
            GeometryReader { proxy in
                let rowHeight = proxy.size.height / 4
                VStack(spacing: 0) {
                    if state.hasFullAccess, let draft = state.draft {
                        DraftPanel(text: draft.text, insert: insert, discard: discard)
                            .frame(height: rowHeight * 3)
                    } else {
                        KeyRows(model: typing, rows: typing.rows).frame(height: rowHeight * 3)
                    }
                    KeyRows(model: typing, rows: [typing.bottomRow(withDelete: state.hasFullAccess && state.draft != nil)])
                        .frame(height: rowHeight)
                }
            }
            .padding(.horizontal, 2)
        }
        .padding(.bottom, 2)
        .foregroundStyle(Theme.textPrimary).background(Theme.inset).tint(Theme.accent)
    }

    /// The word strip while a word is being typed; otherwise what the keyboard has to say.
    @ViewBuilder
    private var status: some View {
        if let suggestions = typing.suggestions {
            SuggestionStrip(suggestions: suggestions, choose: typing.choose)
        } else if !state.hasFullAccess, !typing.hasTyped {
            hint("keyboard.needsFullAccess")
        } else if state.hasFullAccess, let notice = state.session.message ?? state.notice {
            Text(notice).font(.caption).foregroundStyle(Theme.textSecondary).lineLimit(3)
                .accessibilityIdentifier("keyboard-status")
        } else if state.hasFullAccess, !typing.hasTyped {
            hint(state.warm ? "keyboard.ready" : "keyboard.cold")
        } else {
            HStack(spacing: 8) {
                SonaMark()
                    .frame(width: 26, height: 15)
                    .accessibilityHidden(true)
                Text("keyboard.title")
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(1)
            }
        }
    }

    private func hint(_ text: LocalizedStringKey) -> some View {
        Text(text).font(.caption).foregroundStyle(Theme.textSecondary).lineLimit(3).minimumScaleFactor(0.8)
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

/// Dictated text that started in another field, shown in place of the letter keys until
/// it is inserted here or discarded.
private struct DraftPanel: View {
    let text: String
    let insert: () -> Void
    let discard: () -> Void

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                Text(text).font(.body).lineLimit(1...3)
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
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 10)
        }
        .scrollBounceBehavior(.basedOnSize)
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
