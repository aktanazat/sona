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
    private var echo = FieldEcho()
    /// True from the keyboard's own edit until the main loop next turns.
    private var ownEditInFlight = false
    private var editedField: UUID?
    private let haptic = UIImpactFeedbackGenerator(style: .light)
    #if DEBUG
    private let frames = FrameMeter()
    private let appearFrames = FrameMeter()
    #endif
    private var visible = false
    private var requestedAt: Date?
    private var requestedID: UUID?
    private var wantedHeight: CGFloat { traitCollection.verticalSizeClass == .compact ? 200 : 280 }

    /// The system plays its key click only for an input view that asks for it.
    override func loadView() {
        view = ClickingInputView(frame: .zero, inputViewStyle: .keyboard)
    }

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
        echo.forget()
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
        #if DEBUG
        appearFrames.begin(label: "keyboard appearing")
        Task { [appearFrames] in
            try? await Task.sleep(for: .seconds(2))
            appearFrames.end()
        }
        #endif
        /* Setting up the page holds the main thread for about 100 ms on the simulator, so
         * it waits until the keyboard has drawn, then for a moment no finger is on a key:
         * touch tracking runs the loop outside the default mode, and the load waits. */
        Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(400))
            RunLoop.main.perform(inModes: [.default]) {
                MainActor.assumeIsolated { self?.prepareMicrophone() }
            }
        }
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

    override func textWillChange(_ textInput: (any UITextInput)?) {
        super.textWillChange(textInput)
        hostChanged()
    }

    override func textDidChange(_ textInput: (any UITextInput)?) {
        super.textDidChange(textInput)
        hostChanged()
        reload()
        refreshTyping(startingField: false)
    }

    override func selectionWillChange(_ textInput: (any UITextInput)?) {
        super.selectionWillChange(textInput)
        hostChanged()
    }

    override func selectionDidChange(_ textInput: (any UITextInput)?) {
        super.selectionDidChange(textInput)
        hostChanged()
    }

    /// A report that arrives while the keyboard's own edit is still going through is that
    /// edit; any other change, or another field, was not the keyboard's doing.
    private func hostChanged() {
        let field = documentID
        if ownEditInFlight, field == editedField {
            echo.observe(before: textBefore, after: textAfter)
        } else {
            echo.forget()
        }
    }

    private var textBefore: String { textDocumentProxy.documentContextBeforeInput ?? "" }
    private var textAfter: String { textDocumentProxy.documentContextAfterInput ?? "" }
    /// The text a word dropped in whole is joined to.
    private var joiningBefore: String { echo.joining(before: textBefore, after: textAfter) }
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
        state.notice = nil
        state.seenMessage = state.session.message
        let before = joiningBefore
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
        echo.edited(before: textBefore, after: textAfter, deleting: edit.deleteCount, inserting: edit.insert)
        editedField = documentID
        ownEditInFlight = true
        DispatchQueue.main.async { [weak self] in self?.ownEditInFlight = false }
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
        } catch { show(error) }
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
            refreshRoute(readingStatus: false)
        } catch { show(error) }
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
        } catch { state.waiting = false; show(error) }
    }

    private func finish() {
        guard state.route == .keyboard else { send(.stop); return }
        if dictation.phase != .idle { haptic.impactOccurred(intensity: 0.6) }
        dictate(.stop)
    }

    /// The page loads once the keyboard is on screen and idle, so the first Dictate starts
    /// at once; a Dictate touched before it is ready shows it is listening and starts
    /// capture when the page is.
    private func prepareMicrophone() {
        guard microphone == nil, visible, state.route == .keyboard, !state.needsSetup else { return }
        #if DEBUG
        let began = CACurrentMediaTime()
        defer { appearFrames.mark("page set up, \(Int((CACurrentMediaTime() - began) * 1000)) ms on the main thread, done") }
        #endif
        let microphone = KeyboardMicrophone()
        microphone.onEvent = { [weak self] event in self?.dictate(event) }
        microphone.onNotice = { [weak self] notice in self?.state.notice = notice }
        microphone.attach(to: view)
        self.microphone = microphone
    }

    /// Sona's own draft errors say what happened; anything else from the shared storage
    /// is one plain sentence, never the system's wording.
    private func show(_ error: Error) {
        state.notice = (error as? KeyboardDraftError)?.localizedDescription
            ?? NSLocalizedString("keyboard.storageFailed", comment: "")
    }

    private var dictationLanguage: String {
        DictationPreferences.recognitionLanguage(for: typing.language.rawValue)
    }

    /// Where Dictate goes changes only between dictations, so a Finish always reaches the
    /// route that started it, and it reads the app's latest status first.
    private func refreshRoute(readingStatus: Bool) {
        guard !state.dictating, !state.waiting else { return }
        if readingStatus, let latest = try? KeyboardSessionStore.shared().state() { state.session = latest }
        state.route = KeyboardRoute(defaults: DictationPreferences.shared).choice(at: Date(), warm: state.warm)
    }

    private func dictate(_ event: KeyboardDictation.Event) {
        let effects = dictation.handle(event)
        KeyboardRoute(defaults: DictationPreferences.shared).record(effects, at: Date())
        for effect in effects {
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
                state.notice = state.notice ?? NSLocalizedString("keyboard.micRefused", comment: "")
            }
        }
        #if DEBUG
        if event == .captureStarted { frames.mark("microphone open") }
        if dictation.phase == .idle { frames.end() }
        #endif
        state.dictation = dictation.phase
        state.heard = KeyboardSessionState.heard(dictation.heard)
        refreshRoute(readingStatus: true)
    }

    /// The one step every dictation the keyboard heard goes through: the phone's cleanup and
    /// the Mac profile's replacements, vocabulary and snippets, as dictation in the app
    /// applies them, then the field, then the queue Sona keeps in Notes.
    private func insertDictation(_ heard: String) {
        let text = profile.apply(to: DictationCleanup.clean(heard, language: dictationLanguage))
        apply(TypingSession.Edit(insert: TypingSession.joiningSpace(before: joiningBefore) + text))
        refreshShift()
        refreshSuggestions()
        do {
            let queue = try KeyboardDictationQueue.shared()
            try queue.enqueue(KeyboardDictationRecord(
                id: UUID(), text: text, date: Date(), language: dictationLanguage, audio: microphone?.takeAudio()
            ))
            Task.detached(priority: .utility) {
                try? queue.trimAudio(maxBytes: 32_000_000, maxAge: 7 * 24 * 60 * 60)
            }
        } catch { show(error) }
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
        } catch { show(error) }
    }

    private func insertDraft() {
        guard visible, hasFullAccess, let draft = state.draft else { return }
        do {
            let text = try KeyboardDraftStore.shared().take(id: draft.id)
            state.draft = nil
            apply(TypingSession.Edit(insert: TypingSession.joiningSpace(before: joiningBefore) + text))
        } catch { state.draft = nil; show(error) }
    }

    private func discardDraft() {
        guard hasFullAccess, let draft = state.draft else { return }
        do {
            try KeyboardDraftStore.shared().discard(id: draft.id)
            state.draft = nil
        } catch { show(error) }
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
                if let choice = typing.accentChoice {
                    AccentPopup(choice: choice)
                } else if let callout = typing.callout {
                    KeyBubble(anchor: callout.anchor, width: callout.anchor.width + 12) {
                        Text(verbatim: callout.text).font(.system(size: KeyFace.calloutSize))
                    }
                }
            }
    }

    /// While dictating, the words heard so far; then the word strip while a word is being
    /// typed, unless a message came after the last key; then that message; otherwise nothing.
    @ViewBuilder
    private var status: some View {
        if typing.accentChoice != nil {
            Color.clear
        } else if state.hasFullAccess, state.dictating {
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
        } else if state.hasFullAccess, let notice = state.session.message ?? state.notice {
            Text(notice).font(.caption).foregroundStyle(Theme.textSecondary).lineLimit(2)
                .accessibilityIdentifier("keyboard-status")
        } else if state.hasFullAccess, state.needsSetup {
            Text("keyboard.permission").font(.caption).foregroundStyle(Theme.textSecondary).lineLimit(2)
        } else {
            Color.clear
        }
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
                path, with: .color(Theme.textPrimary.opacity(0.25)),
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
        KeyBubble(anchor: choice.anchor, width: CGFloat(choice.options.count) * choice.anchor.width + 8, x: choice.originX - 4) {
            HStack(spacing: 0) {
                ForEach(Array(choice.options.enumerated()), id: \.offset) { index, option in
                    Text(verbatim: option)
                        .font(.system(size: KeyFace.letterSize))
                        .frame(width: choice.anchor.width, height: choice.anchor.height - 10)
                        .foregroundStyle(index == choice.selected ? Color.white : Theme.textPrimary)
                        .background(
                            index == choice.selected ? Theme.accent : Color.clear,
                            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                        )
                }
            }
        }
    }
}

/// A key's fill grown up out of the key itself, as the system keyboard draws its letter
/// callouts and accent choices: a bubble one key high above the key, joined to it.
private struct KeyBubble<Content: View>: View {
    let anchor: CGRect
    let width: CGFloat
    var x: CGFloat?
    @ViewBuilder let content: Content

    var body: some View {
        let key = anchor.insetBy(dx: 3, dy: 5)
        let left = x ?? key.midX - width / 2
        ZStack(alignment: .topLeading) {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(KeyFace.fill)
                .frame(width: key.width, height: key.height + 8)
                .offset(x: key.minX - left, y: key.height - 4)
            content
                .frame(width: width, height: key.height)
                .foregroundStyle(Theme.textPrimary)
                .background(KeyFace.fill, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        }
        .compositingGroup()
        .shadow(color: .black.opacity(0.25), radius: 3, y: 1)
        .offset(x: left, y: key.minY - key.height - 4)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
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
            .padding(.top, 8)
        }
        .scrollBounceBehavior(.basedOnSize)
        .clipped()
    }
}

/// UITextDocumentProxy's own `documentIdentifier` getter with the nullability it has in practice.
@objc private protocol OptionalDocumentIdentifier {
    var documentIdentifier: UUID? { get }
}

private final class ClickingInputView: UIInputView, UIInputViewAudioFeedback {
    var enableInputClicksWhenVisible: Bool { true }
}
