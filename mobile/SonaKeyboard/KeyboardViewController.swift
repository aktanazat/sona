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
    /// The keyboard's own dictation, which needs the switch and permissions set in the app.
    @Published var dictation = KeyboardDictation.Phase.idle
    @Published var heard: String?
    @Published var needsSetup = true
    /// Where Dictate goes; after the keyboard could not listen, through the app for a while.
    @Published var route = KeyboardRoute.Choice.keyboard
    /// The message on show at the last key press. Any other message is new, and the word
    /// strip gives way to it until the next key.
    @Published var seenMessage: String?
    var warm: Bool { session.isWarm(at: Date()) }
    var newMessage: String? {
        guard hasFullAccess, let message = session.message ?? notice, message != seenMessage else { return nil }
        return message
    }
    var dictating: Bool {
        dictation != .idle || (warm && (session.phase == .listening || session.phase == .finishing))
    }
}

@MainActor
final class KeyboardViewController: UIInputViewController {
    private static let languageKey = "sona.keyboard.language"
    private let state = KeyboardState()
    private let typing = TypingModel()
    private var corrector: TypingCorrector?
    private var session = TypingSession(corrector: nil)
    private var minimumHeight: NSLayoutConstraint?
    private var signal: KeyboardSignal?
    private var ticker: Task<Void, Never>?
    private var dictation = KeyboardDictation()
    private var microphone: KeyboardMicrophone?
    private var profile = DictationProfile.empty
    private let haptic = UIImpactFeedbackGenerator(style: .light)
    #if DEBUG
    private let frames = FrameMeter()
    #endif
    private var visible = false
    private var requestedAt: Date?
    private var requestedID: UUID?
    private var wantedHeight: CGFloat { traitCollection.verticalSizeClass == .compact ? 200 : 280 }

    override func viewDidLoad() {
        super.viewDidLoad()
        signal = KeyboardSignal(.state) { [weak self] in
            Task { @MainActor in self?.reload() }
        }
        setLanguage(storedLanguage(in: KeyboardLanguages.shared().enabled))
        typing.press = { [weak self] key in self?.press(key) }
        typing.choose = { [weak self] word in self?.choose(word) }
        typing.swipe = { [weak self] path, grid in self?.swipe(path, on: grid) }
        requestSupplementaryLexicon { [weak self] lexicon in
            let entries = lexicon.entries.map { (shortcut: $0.userInput, text: $0.documentText) }
            Task { @MainActor in
                entries.forEach { self?.session.lexicon.add(shortcut: $0.shortcut, text: $0.text) }
            }
        }
        let content = KeyboardView(
            state: state,
            typing: typing,
            prepare: { [weak self] in self?.prepareMicrophone() },
            microphone: { [weak self] in self?.start() },
            stop: { [weak self] in self?.finish() },
            insert: { [weak self] in self?.insertDraft() },
            discard: { [weak self] in self?.discardDraft() }
        )
        let host = UIHostingController(rootView: content)
        host.view.backgroundColor = .clear
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
        let languages = KeyboardLanguages.shared().enabled
        typing.showsLanguageKey = languages.count > 1
        if !languages.contains(typing.language) { setLanguage(languages[0]) }
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
        dictate(.hidden)
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
    /// UIKit declares this nonnull, yet some fields hand over nil, and Swift's bridge to a
    /// non-optional UUID traps on it. The same getter, declared optional, keeps nil as nil.
    private var documentID: UUID? {
        unsafeBitCast(textDocumentProxy as AnyObject, to: OptionalDocumentIdentifier.self).documentIdentifier
    }

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

    private func storedLanguage(in enabled: [TypingLanguage]) -> TypingLanguage {
        let stored = UserDefaults.standard.string(forKey: Self.languageKey).flatMap(TypingLanguage.init(rawValue:))
        return stored.flatMap { enabled.contains($0) ? $0 : nil } ?? enabled[0]
    }

    /// Letters, spelling and the language dictation listens in all follow the typing language.
    private func setLanguage(_ language: TypingLanguage) {
        typing.language = language
        corrector = TypingCorrector(language: language.rawValue)
        session = TypingSession(corrector: corrector, lexicon: session.lexicon)
        primaryLanguage = language.rawValue
        state.startURL = DictationLink.keyboardURL(documentID: documentID, language: language)
        UserDefaults.standard.set(language.rawValue, forKey: Self.languageKey)
    }

    private func press(_ key: Key) {
        typing.hasTyped = true
        state.notice = nil
        state.seenMessage = state.session.message
        switch key {
        case let .character(text):
            let character = typing.face(of: text)
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
        case .language:
            setLanguage(KeyboardLanguages.shared().next(after: typing.language))
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

    /// A swipe types the word its path most likely spelled, cased for where it lands; the
    /// strip then offers the runners-up.
    private func swipe(_ path: [CGPoint], on grid: KeyGrid) {
        guard let corrector else { return }
        let decoder = SwipeDecoder(
            language: typing.language, unit: grid.unit, rowHeight: grid.rowHeight, origin: grid.origin
        )
        let words = decoder.words(for: path, dictionary: corrector.completions(of:), names: session.lexicon.names)
        guard !words.isEmpty else { return }
        typing.hasTyped = true
        state.notice = nil
        state.seenMessage = state.session.message
        let before = textBefore
        let landing = before + TypingSession.joiningSpace(before: before)
        let capital = typing.shift != .off || capitalizes(before: landing)
        let cased = words.map { word in
            if typing.shift == .locked { return word.uppercased() }
            return capital ? word.prefix(1).uppercased() + word.dropFirst() : word
        }
        apply(session.swipe(cased, before: before))
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
        if session.tapShift(at: Date()) {
            typing.shift = .locked
        } else {
            typing.shift = typing.shift == .off ? .once : .off
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
        typing.shift = capitalizes(before: textBefore) ? .once : .off
    }

    /// Whether the field's capitalization asks for a capital after `before`.
    private func capitalizes(before: String) -> Bool {
        switch textDocumentProxy.autocapitalizationType ?? .sentences {
        case .allCharacters: true
        case .words: before.last.map(\.isWhitespace) ?? true
        case .sentences: TypingText.startsSentence(before: before[...])
        case .none: false
        @unknown default: false
        }
    }

    private func refreshSuggestions() {
        typing.suggestions = corrects && cursorEndsWord ? session.suggestions(before: textBefore) : nil
    }

    private func loadVocabulary() {
        do {
            let store = try KeyboardVocabularyStore.shared()
            profile = try store.profile()
            try store.load().forEach { session.lexicon.addName($0) }
        } catch { state.notice = error.localizedDescription }
    }

    private func reload() {
        guard visible else { return }
        state.hasFullAccess = hasFullAccess
        state.startURL = DictationLink.keyboardURL(documentID: documentID, language: typing.language)
        guard hasFullAccess else {
            state.draft = nil
            state.session = KeyboardSessionState()
            state.notice = nil
            state.needsSetup = true
            return
        }
        state.needsSetup = !DictationPreferences.keyboardEnabled || !DictationPreferences.keyboardMicrophoneAllowed
        state.route = KeyboardRoute(defaults: DictationPreferences.shared).choice(at: Date(), warm: state.warm)
        do {
            state.session = try KeyboardSessionStore.shared().state()
            if let requestedAt {
                if state.session.requestID == requestedID {
                    self.requestedAt = nil
                    self.requestedID = nil
                    state.waiting = false
                } else if Date().timeIntervalSince(requestedAt) >= 3 {
                    self.requestedAt = nil
                    self.requestedID = nil
                    state.session.heartbeat = .distantPast
                    state.waiting = false
                    state.notice = NSLocalizedString("keyboard.sessionEnded", comment: "")
                }
            }
            switch try KeyboardDraftStore.shared().load() {
            case .waiting(let draft):
                state.draft = draft
                // Never send a delayed result to a different text field or app.
                if let documentID, draft.documentID == documentID { insertDraft() }
            case .expired:
                state.draft = nil
                state.notice = KeyboardDraftError.expired.localizedDescription
            case .missing:
                state.draft = nil
            }
        } catch { state.notice = error.localizedDescription }
    }

    /// Dictate listens in the keyboard itself; only after the microphone refused it does
    /// Dictate go through the app's warm session, as it did before the keyboard could listen.
    private func start() {
        guard state.route != .keyboard else {
            prepareMicrophone()
            dictate(.start)
            return
        }
        guard hasFullAccess, state.warm else { reload(); return }
        let id = UUID()
        requestedID = id
        requestedAt = Date()
        state.waiting = true
        state.notice = nil
        do {
            try KeyboardSessionStore.shared().send(KeyboardCommand(
                id: UUID(), action: .start, requestID: id,
                documentID: documentID, createdAt: Date(), language: typing.language.rawValue
            ))
        } catch { state.waiting = false; state.notice = error.localizedDescription }
    }

    private func finish() {
        guard state.route == .keyboard else { send(.stop); return }
        if dictation.phase != .idle { haptic.impactOccurred(intensity: 0.6) }
        dictate(.stop)
    }

    /// The page loads when a finger first touches Dictate, so a keyboard only typed with
    /// never starts WebKit.
    private func prepareMicrophone() {
        guard microphone == nil, state.route == .keyboard, !state.needsSetup else { return }
        let microphone = KeyboardMicrophone()
        microphone.onEvent = { [weak self] event in self?.dictate(event) }
        microphone.onNotice = { [weak self] notice in self?.state.notice = notice }
        microphone.attach(to: view)
        self.microphone = microphone
    }

    private var dictationLanguage: String {
        DictationPreferences.recognitionLanguage(for: typing.language.rawValue)
    }

    private func dictate(_ event: KeyboardDictation.Event) {
        for effect in dictation.handle(event) {
            switch effect {
            case .beginCapture:
                state.notice = nil
                haptic.impactOccurred()
                #if DEBUG
                frames.begin()
                #endif
                microphone?.start(language: dictationLanguage, hints: profile.recognitionHints + session.lexicon.names)
            case .endCapture:
                microphone?.end()
            case .abandon:
                microphone?.abandon()
            case let .insert(text):
                insertDictation(text)
            case .fallBack:
                KeyboardRoute(defaults: DictationPreferences.shared).refuse(at: Date())
                state.route = KeyboardRoute(defaults: DictationPreferences.shared).choice(at: Date(), warm: state.warm)
                state.notice = state.notice ?? NSLocalizedString("keyboard.micRefused", comment: "")
            }
        }
        #if DEBUG
        if event == .captureStarted { frames.mark("microphone open") }
        if dictation.phase == .idle { frames.end() }
        #endif
        state.dictation = dictation.phase
        state.heard = KeyboardSessionState.heard(dictation.heard)
    }

    /// The one step every dictation the keyboard heard goes through: the phone's cleanup and
    /// the Mac profile's replacements, vocabulary and snippets, as dictation in the app
    /// applies them, then the field, then the queue Sona keeps in Notes.
    private func insertDictation(_ heard: String) {
        let text = profile.apply(to: DictationCleanup.clean(heard, language: dictationLanguage))
        textDocumentProxy.insertText(TypingSession.joiningSpace(before: textBefore) + text)
        refreshShift()
        refreshSuggestions()
        do {
            try KeyboardDictationQueue.shared().enqueue(KeyboardDictationRecord(
                id: UUID(), text: text, date: Date(), language: dictationLanguage, audio: microphone?.takeAudio()
            ))
        } catch { state.notice = error.localizedDescription }
    }

    /// A stop sent before the app has answered the start, as a quick hold-to-talk release
    /// does, withdraws the unread start when the app has not taken it yet; otherwise it names
    /// the request the keyboard just made.
    private func send(_ action: KeyboardCommand.Action) {
        guard hasFullAccess, let id = requestedAt == nil ? state.session.requestID : requestedID else { return }
        do {
            let store = try KeyboardSessionStore.shared()
            if action == .stop, requestedAt != nil, try store.cancelPendingStart(requestID: id) {
                requestedAt = nil
                requestedID = nil
                state.waiting = false
                return
            }
            try store.send(KeyboardCommand(
                id: UUID(), action: action, requestID: id,
                documentID: documentID, createdAt: Date()
            ))
        } catch { state.notice = error.localizedDescription }
    }

    private func insertDraft() {
        guard visible, hasFullAccess, let draft = state.draft else { return }
        do {
            let text = try KeyboardDraftStore.shared().take(id: draft.id)
            state.draft = nil
            textDocumentProxy.insertText(TypingSession.joiningSpace(before: textBefore) + text)
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
    let prepare: () -> Void
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
            .frame(maxWidth: KeyGrid.maxWidth)
            .frame(height: 48)
            .padding(.horizontal, 8)
            GeometryReader { proxy in
                let rowHeight = proxy.size.height / 4
                VStack(spacing: 0) {
                    if state.hasFullAccess, let draft = state.draft {
                        DraftPanel(text: draft.text, insert: insert, discard: discard)
                            .frame(maxWidth: KeyGrid.maxWidth)
                            .frame(height: rowHeight * 3)
                    } else {
                        keys.frame(height: rowHeight * 3)
                    }
                    KeyRows(model: typing, rows: [typing.bottomRow(withDelete: state.hasFullAccess && state.draft != nil)])
                        .frame(height: rowHeight)
                }
            }
            .padding(.horizontal, 2)
        }
        .padding(.bottom, 2)
        .foregroundStyle(Theme.textPrimary).tint(Theme.accent)
    }

    private var keys: some View {
        let rows = typing.rows
        return KeyRows(model: typing, rows: rows, taps: false)
            .overlay { SwipeTrail(points: typing.trail) }
            .overlay { KeySurface(model: typing, rows: rows, swipes: typing.layer == .letters) }
            .overlay(alignment: .topLeading) {
                if let choice = typing.accentChoice { AccentPopup(choice: choice) }
            }
    }

    /// While dictating, the words heard so far; then the word strip while a word is being
    /// typed, unless a message came after the last key; otherwise what the keyboard has to say.
    @ViewBuilder
    private var status: some View {
        if state.hasFullAccess, state.dictating {
            let heard = state.dictation == .idle ? state.session.heard : state.heard
            Text(verbatim: heard ?? String(localized: "keyboard.listening"))
                .font(.subheadline)
                .foregroundStyle(heard == nil ? Theme.textSecondary : Theme.textPrimary)
                .lineLimit(2)
                .truncationMode(.head)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentTransition(.opacity)
                .animation(.easeOut(duration: 0.15), value: heard)
                .accessibilityIdentifier("keyboard-heard")
        } else if let suggestions = typing.suggestions, state.newMessage == nil {
            SuggestionStrip(suggestions: suggestions, choose: typing.choose)
        } else if !state.hasFullAccess, !typing.hasTyped {
            hint("keyboard.needsFullAccess")
        } else if state.hasFullAccess, let notice = state.session.message ?? state.notice {
            Text(notice).font(.caption).foregroundStyle(Theme.textSecondary).lineLimit(3)
                .accessibilityIdentifier("keyboard-status")
        } else if state.hasFullAccess, !typing.hasTyped {
            hint(state.needsSetup ? "keyboard.setup" : state.route == .openApp ? "keyboard.cold" : "keyboard.ready")
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

    /// Before the app has the microphone allowed, Dictate opens it once to ask. After the
    /// keyboard could not listen, Dictate uses the app's warm session, or opens the app.
    @ViewBuilder
    private var control: some View {
        if state.needsSetup {
            open(DictationLink.keyboardSetupURL)
        } else {
            switch state.route {
            case .keyboard:
                MicControl(
                    phase: state.dictation == .idle ? .ready : state.dictation == .finishing ? .busy : .listening,
                    prepare: prepare, start: microphone, stop: stop
                )
            case .appSession:
                MicControl(
                    phase: state.session.phase == .listening ? .listening
                        : state.session.phase == .finishing || state.waiting ? .busy : .ready,
                    prepare: {}, start: microphone, stop: stop
                )
            case .openApp:
                open(state.startURL)
            }
        }
    }

    private func open(_ url: URL) -> some View {
        Link(destination: url) {
            RecordPill(glyph: .record, title: "dictation.start", height: 40)
        }
        .glassEffect(.regular.interactive(), in: Capsule())
        .accessibilityIdentifier("keyboard-open-sona")
    }
}

/// The dictate pill. A tap starts, and a tap while listening finishes; holding it talks
/// until it is let go. One view for every phase, so a hold survives the pill changing.
private struct MicControl: View {
    enum Phase { case ready, busy, listening }

    let phase: Phase
    let prepare: () -> Void
    let start: () -> Void
    let stop: () -> Void
    @State private var down = false
    @State private var holding = false
    @State private var hold: Task<Void, Never>?

    var body: some View {
        RecordPill(glyph: glyph, title: title, height: 40)
            .glassEffect(.regular.interactive(), in: Capsule())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { _ in press() }
                    .onEnded { _ in release() }
            )
            .accessibilityElement()
            .accessibilityLabel(Text(title))
            .accessibilityAddTraits(.isButton)
            .accessibilityAction { tap() }
            .accessibilityIdentifier(phase == .listening ? "keyboard-stop" : "keyboard-mic")
    }

    private var glyph: RecordPill.Glyph {
        switch phase {
        case .ready: .record
        case .busy: .busy
        case .listening: .stop
        }
    }

    private var title: LocalizedStringKey {
        switch phase {
        case .ready: "dictation.start"
        case .busy: "dictation.finishing"
        case .listening: "keyboard.stop"
        }
    }

    private func press() {
        guard !down else { return }
        down = true
        prepare()
        guard phase == .ready else { return }
        hold = Task {
            try? await Task.sleep(for: .seconds(0.35))
            guard !Task.isCancelled else { return }
            holding = true
            start()
        }
    }

    private func release() {
        hold?.cancel()
        hold = nil
        down = false
        if holding {
            holding = false
            stop()
        } else {
            tap()
        }
    }

    private func tap() {
        switch phase {
        case .ready: start()
        case .listening: stop()
        case .busy: break
        }
    }
}

/// The path a swipe has drawn so far.
private struct SwipeTrail: View {
    let points: [CGPoint]

    var body: some View {
        Canvas { context, _ in
            guard let first = points.first else { return }
            var path = Path()
            path.move(to: first)
            points.dropFirst().forEach { path.addLine(to: $0) }
            context.stroke(
                path, with: .color(Theme.accent.opacity(0.55)),
                style: StrokeStyle(lineWidth: 6, lineCap: .round, lineJoin: .round)
            )
        }
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// A held letter's accents above it; the finger slides to one and lets go.
private struct AccentPopup: View {
    let choice: AccentChoice

    var body: some View {
        HStack(spacing: 0) {
            ForEach(Array(choice.options.enumerated()), id: \.offset) { index, option in
                Text(verbatim: option)
                    .font(.system(size: 23))
                    .frame(width: choice.anchor.width, height: choice.anchor.height - 10)
                    .foregroundStyle(index == choice.selected ? Color.white : Theme.textPrimary)
                    .background(
                        index == choice.selected ? Theme.accent : Color.clear,
                        in: RoundedRectangle(cornerRadius: 6, style: .continuous)
                    )
            }
        }
        .padding(4)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .shadow(color: .black.opacity(0.2), radius: 6, y: 2)
        .offset(x: choice.originX - 4, y: choice.anchor.minY - choice.anchor.height)
        .allowsHitTesting(false)
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
        .clipped()
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

/// UITextDocumentProxy's own `documentIdentifier` getter with the nullability it has in practice.
@objc private protocol OptionalDocumentIdentifier {
    var documentIdentifier: UUID? { get }
}
