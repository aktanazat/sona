import AudioToolbox
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
    private let emoji = EmojiModel()
    private var corrector: TypingCorrector?
    private var session = TypingSession(corrector: nil)
    /// How many times Reset in the app had run when the learning was read, so a reset made
    /// while the keyboard was open is not undone by saving what it had before.
    private var learningResets = 0
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
    private var feel = KeyboardFeelPreferences(defaults: .standard)
    /// Suggestions wait for the run loop after a key, so the key's text goes out first.
    private var suggesting: Task<Void, Never>?
    /// The strip's spelling guesses, worked out once typing pauses.
    private var guessing: Task<Void, Never>?
    private var lastKeyAt = Date.distantPast
    #if DEBUG
    private let frames = FrameMeter()
    private let appearFrames = FrameMeter()
    #endif
    private var visible = false
    private var requestedAt: Date?
    private var requestedID: UUID?
    /// The top bar, then the letter layout's rows at the system keyboard's row height.
    private var wantedHeight: CGFloat {
        let compact = traitCollection.verticalSizeClass == .compact
        return KeyboardView.barHeight + CGFloat(typing.rowCount) * (compact ? 40 : 54) + 4
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        signal = KeyboardSignal(.state) { [weak self] in
            Task { @MainActor in self?.reload() }
        }
        loadFeel()
        setLanguage(storedLanguage(in: KeyboardLanguages.shared().enabled))
        typing.press = { [weak self] key in self?.press(key) }
        typing.choose = { [weak self] word in self?.choose(word) }
        typing.swipe = { [weak self] path, grid in self?.swipe(path, on: grid) }
        typing.moveCursor = { [weak self] offset in self?.moveCursor(by: offset) ?? 0 }
        typing.deleteWord = { [weak self] in self?.deleteWord() }
        typing.restore = { [weak self] text in self?.restore(text) }
        typing.touched = { [weak self] key in self?.feedback(for: key) }
        emoji.insert = { [weak self] glyph in self?.insertEmoji(glyph) }
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
            emoji: emoji,
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
        let oldLayout = typing.englishLayout
        loadFeel()
        if !languages.contains(typing.language) {
            setLanguage(languages[0])
        } else if typing.language == .english, oldLayout != typing.englishLayout {
            setLanguage(.english)
        }
        haptic.prepare()
        if hasFullAccess {
            loadVocabulary()
            loadLearning()
        }
        session.hostChanged(field: documentID, text: fieldText)
        refreshTyping(startingField: true)
        view.setNeedsLayout()
    }

    private func loadFeel() {
        feel = hasFullAccess ? KeyboardFeelPreferences.shared() : KeyboardFeelPreferences(defaults: .standard)
        typing.numberRow = feel.numberRow
        typing.englishLayout = feel.englishLayout
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        typing.emojiQuery = nil
        guard hasFullAccess else { return }
        do {
            if try !KeyboardLearningStore.shared().save(session.learning, readAt: learningResets) {
                session.replaceLearning(KeyboardLearning())
            }
        } catch { state.notice = error.localizedDescription }
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
        echo.hostNoticed()
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
        echo.hostNoticed()
        hostChanged()
    }

    /// A report that arrives while the keyboard's own edit is still going through is that
    /// edit. Text the keyboard did not write, or another field, empties the strip until it
    /// is worked out again for the field as it is now; a cursor move alone does not.
    private func hostChanged() {
        let own = ownEditInFlight && documentID == editedField
        echo.hostReported(before: textBefore, after: textAfter, ownEdit: own)
        guard !own, session.hostChanged(field: documentID, text: fieldText) else { return }
        typing.suggestions = nil
        scheduleSuggestions()
    }

    private var textBefore: String { textDocumentProxy.documentContextBeforeInput ?? "" }
    private var textAfter: String { textDocumentProxy.documentContextAfterInput ?? "" }
    /// The text a word dropped in whole is joined to.
    private var joiningBefore: String { echo.joining(before: textBefore, after: textAfter) }
    private var hostBehind: Bool { echo.behind(before: textBefore, after: textAfter) }
    /// The text around the cursor as the keyboard's last edit left it.
    private var fieldText: String { joiningBefore + textAfter }
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

    /// Letters, spelling, next words and the language dictation listens in all follow the
    /// typing language. Emoji found by word need English words. The next-word table and the
    /// common words come in a moment later, read off the main thread the first time.
    private func setLanguage(_ language: TypingLanguage) {
        typing.language = language
        let corrector = TypingCorrector(language: language.rawValue, grid: typing.letterGrid)
        self.corrector = corrector
        let catalog = language == .english ? EmojiCatalog.shared : nil
        session = TypingSession(
            corrector: corrector, lexicon: session.lexicon, learning: session.learning,
            emoji: { catalog?.emoji(for: $0) }
        )
        session.hostChanged(field: documentID, text: fieldText)
        Task { [weak self] in
            async let table = NextWords.table(for: language)
            async let common = CommonWords.list(for: language)
            let (loadedTable, loadedCommon) = await (table, common)
            corrector?.common = loadedCommon
            guard let self, typing.language == language else { return }
            session.predictor = loadedTable
            refreshSuggestions()
            /* Apple's checker loads its dictionary on its first call, which would otherwise
             * be the first space: 4 to 23 ms in a fresh keyboard on the simulator. So it is
             * asked once no key has come for a second, when no finger is on a key (touch
             * tracking runs the loop outside the default mode). */
            repeat {
                try? await Task.sleep(for: .milliseconds(250))
            } while Date().timeIntervalSince(lastKeyAt) < 1
            RunLoop.main.perform(inModes: [.default]) { [weak self] in
                MainActor.assumeIsolated {
                    guard let corrector, self?.corrector === corrector else { return }
                    corrector.warmUp()
                }
            }
        }
        primaryLanguage = language.rawValue
        state.startURL = DictationLink.keyboardURL(documentID: documentID, language: language)
        UserDefaults.standard.set(language.rawValue, forKey: Self.languageKey)
        refreshShift()
        view.setNeedsLayout()
    }

    private func press(_ key: Key) {
        typing.hasTyped = true
        state.notice = nil
        state.seenMessage = state.session.message
        if typing.emojiQuery != nil, search(key) { return }
        switch key {
        case let .character(text):
            let character = typing.face(of: text)
            if ".,!?;:)\"".contains(character) {
                apply(session.end(
                    with: character, before: textBefore, corrects: corrects && cursorEndsWord, hostBehind: hostBehind
                ))
            } else {
                apply(session.type(character))
            }
            if character == "'" { typing.layer = .letters }
        case .domain:
            apply(session.type(TypingModel.domains[0]))
        case .space, .newLine:
            let separator = key == .space ? " " : "\n"
            apply(session.end(
                with: separator, before: textBefore, corrects: corrects && cursorEndsWord, hostBehind: hostBehind
            ))
            typing.layer = .letters
        case .delete:
            apply(session.backspace(before: textBefore, hostBehind: hostBehind))
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
        afterKey()
    }

    private func afterKey() {
        lastKeyAt = Date()
        refreshShift()
        scheduleSuggestions()
    }

    /// Keys while the emoji search is open type into the search; return or ABC closes it.
    private func search(_ key: Key) -> Bool {
        guard let query = typing.emojiQuery else { return false }
        switch key {
        case let .character(text): typing.emojiQuery = query + text.lowercased()
        case .space: typing.emojiQuery = query + " "
        case .delete: typing.emojiQuery = String(query.dropLast())
        case .newLine: typing.emojiQuery = nil; typing.layer = .emoji
        case let .layer(layer): typing.emojiQuery = nil; typing.layer = layer
        case .shift: toggleShift()
        default: return false
        }
        return true
    }

    private func choose(_ word: String) {
        typing.hasTyped = true
        if EmojiCatalog.shared?.entry(for: word) != nil { emoji.memory.used(word) }
        apply(session.choose(word, before: textBefore, hostBehind: hostBehind))
        afterKey()
    }

    private func insertEmoji(_ glyph: String) {
        typing.hasTyped = true
        apply(TypingSession.Edit(insert: glyph))
        feedback(for: .character(glyph))
        afterKey()
    }

    /// A swipe types the word its path most likely spelled, cased for where it lands; the
    /// strip then offers the runners-up.
    private func swipe(_ path: [CGPoint], on grid: KeyGrid) {
        guard let corrector else { return }
        let decoder = SwipeDecoder(language: typing.language, centres: grid.letterCentres, keyWidth: grid.unit)
        let words = decoder.words(
            for: path, dictionary: corrector.completions(of:), names: session.lexicon.names + session.lexicon.learned
        )
        guard !words.isEmpty else { return }
        typing.hasTyped = true
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
        afterKey()
    }

    private func apply(_ edit: TypingSession.Edit) {
        markOwnEdit()
        textDocumentProxy.apply(edit, echo: &echo)
        session.edited(field: documentID, text: fieldText)
    }

    /// Host reports from now until the main loop next turns are this edit going through.
    private func markOwnEdit() {
        editedField = documentID
        ownEditInFlight = true
        DispatchQueue.main.async { [weak self] in self?.ownEditInFlight = false }
    }

    /// Moves the cursor by up to `offset` characters, as far as the text the field shares
    /// goes; returns how far it went.
    private func moveCursor(by offset: Int) -> Int {
        let room = offset < 0 ? -textBefore.count : textDocumentProxy.documentContextAfterInput?.count ?? 0
        let moved = offset < 0 ? max(offset, room) : min(offset, room)
        guard moved != 0 else { return 0 }
        textDocumentProxy.adjustTextPosition(byCharacterOffset: moved)
        typing.hasTyped = true
        afterKey()
        return moved
    }

    private func deleteWord() -> String? {
        markOwnEdit()
        guard let taken = textDocumentProxy.takeWord(echo: &echo) else { return nil }
        session.edited(field: documentID, text: fieldText)
        afterKey()
        return taken
    }

    /// Puts back what a slide on delete took; a long selection it took leaves nothing to put back.
    private func restore(_ text: String) {
        guard !text.isEmpty else { return }
        apply(TypingSession.Edit(insert: text))
        afterKey()
    }

    /// The system keyboard's click and tap as a finger lands: a click for letters, another
    /// sound for delete and for keys that change the keyboard. iOS lets a keyboard make
    /// them only with Full Access.
    private func feedback(for key: Key) {
        guard hasFullAccess else { return }
        if feel.haptics {
            haptic.impactOccurred(intensity: 0.6)
            haptic.prepare()
        }
        guard feel.sounds else { return }
        switch key {
        case .character, .domain: AudioServicesPlaySystemSound(1104)
        case .delete: AudioServicesPlaySystemSound(1155)
        default: AudioServicesPlaySystemSound(1156)
        }
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
        case .URL: typing.extraKeys = ["/", "."]
        case .webSearch: typing.extraKeys = ["."]
        default: typing.extraKeys = []
        }
        typing.showsDomainKey = [.emailAddress, .URL].contains(proxy.keyboardType ?? .default)
        if startingField {
            typing.emojiQuery = nil
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

    /// Runs after every edit, so the memory of the edit is checked against what the host
    /// shows as soon as the keyboard reads it again.
    private func refreshShift() {
        echo.hostReported(before: textBefore, after: textAfter, ownEdit: true)
        guard typing.shift != .locked else { return }
        typing.shift = capitalizes(before: textBefore) ? .once : .off
    }

    /// Whether the field's capitalization asks for a capital after `before`.
    private func capitalizes(before: String) -> Bool {
        return switch textDocumentProxy.autocapitalizationType ?? .sentences {
        case .allCharacters: true
        case .words: before.last.map(\.isWhitespace) ?? true
        case .sentences: TypingText.startsSentence(before: before[...])
        case .none: false
        @unknown default: false
        }
    }

    private func scheduleSuggestions() {
        guard suggesting == nil else { return }
        suggesting = Task { [weak self] in
            await Task.yield()
            self?.suggesting = nil
            self?.refreshSuggestions()
        }
    }

    /// Completions show at once; spelling guesses, which cost milliseconds in Apple's checker
    /// on the main thread, follow when no key has come for 80 ms. Space still corrects at
    /// once, from the same guesses. Next words after a space start with a capital where the
    /// field wants one.
    private func refreshSuggestions() {
        showSuggestions(guessing: false)
        guessing?.cancel()
        guessing = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(80))
            guard !Task.isCancelled else { return }
            self?.showSuggestions(guessing: true)
        }
    }

    private func showSuggestions(guessing: Bool) {
        let before = textBefore
        let ready = corrects && cursorEndsWord && !hostBehind
        let offer = ready ? session.suggestions(before: before, guessing: guessing) : nil
        /* A correction on show stays while the same word goes on, with the word as now typed
         * in its quoted slot, until the guesses for the new letter replace it, so the strip
         * does not blink on every key. A change the keyboard did not make, which includes
         * another field, has emptied the strip, so nothing is kept across one. */
        if !guessing, ready, offer?.correction == nil, let shown = typing.suggestions, shown.correction != nil,
           let word = TypingText.word(before: before), word.hasPrefix(shown.typed) {
            typing.suggestions = Suggestions(typed: String(word), correction: shown.correction, others: shown.others)
            return
        }
        guard let found = offer else {
            typing.suggestions = nil
            return
        }
        guard found.typed.isEmpty, typing.shift != .off else {
            typing.suggestions = found
            return
        }
        let cased = found.others.map { typing.shift == .locked ? $0.uppercased() : $0.prefix(1).uppercased() + $0.dropFirst() }
        typing.suggestions = Suggestions(typed: "", correction: nil, others: cased)
    }

    private func loadVocabulary() {
        do {
            let store = try KeyboardVocabularyStore.shared()
            profile = try store.profile()
            try store.load().forEach { session.lexicon.addName($0) }
        } catch { show(error) }
    }

    /// The learning is decoded off the main thread; Reset in the app may have emptied it.
    private func loadLearning() {
        Task { [weak self] in
            let stored = await Task.detached(priority: .userInitiated) { KeyboardLearningStore.shared().load() }.value
            self?.session.replaceLearning(stored.learning)
            self?.learningResets = stored.resets
        }
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
    static let barHeight: CGFloat = 46

    @ObservedObject var state: KeyboardState
    let typing: TypingModel
    let prepare: () -> Void
    let emoji: EmojiModel
    let microphone: () -> Void
    let stop: () -> Void
    let insert: () -> Void
    let discard: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            bar
                .frame(maxWidth: KeyGrid.maxWidth)
                .frame(height: Self.barHeight)
                .padding(.horizontal, 8)
                .zIndex(0)
            GeometryReader { proxy in
                let rowHeight = proxy.size.height / CGFloat(typing.rowCount)
                VStack(spacing: 0) {
                    Spacer(minLength: 0)
                    if state.hasFullAccess, let draft = state.draft {
                        DraftPanel(text: draft.text, insert: insert, discard: discard)
                            .frame(maxWidth: KeyGrid.maxWidth)
                            .frame(height: rowHeight * CGFloat(typing.rowCount - 1))
                        keys([typing.bottomRow(withDelete: true)]).frame(height: rowHeight)
                    } else if typing.layer == .emoji, let catalog = EmojiCatalog.shared {
                        EmojiPanel(catalog: catalog, model: emoji, typing: typing) {
                            typing.emojiQuery = ""
                            typing.layer = .letters
                        }
                        .frame(maxWidth: KeyGrid.maxWidth)
                        .frame(height: proxy.size.height)
                    } else {
                        let rows = typing.rows + [typing.bottomRow(withDelete: false)]
                        keys(rows).frame(height: rowHeight * CGFloat(rows.count))
                    }
                }
            }
            .padding(.horizontal, 3)
            .zIndex(1)
        }
        .padding(.bottom, 4)
        .foregroundStyle(Theme.textPrimary).tint(Theme.accent)
    }

    /// The top bar: the emoji search while it is open, otherwise the strip or what the
    /// keyboard has to say, beside Dictate.
    @ViewBuilder
    private var bar: some View {
        if let query = typing.emojiQuery, let catalog = EmojiCatalog.shared {
            EmojiSearchBar(
                query: query, results: catalog.search(query), memory: emoji.memory,
                pick: { glyph, base in emoji.pick(glyph, base: base) },
                close: { typing.emojiQuery = nil; typing.layer = .emoji }
            )
        } else {
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
        }
    }

    private func keys(_ rows: [[KeyCap]]) -> some View {
        KeyRows(model: typing, rows: rows)
            .overlay { SwipeTrail(points: typing.trail) }
            .overlay {
                KeySurface(
                    model: typing, rows: rows,
                    swipes: typing.layer == .letters && typing.emojiQuery == nil
                )
            }
            .overlay(alignment: .topLeading) {
                GeometryReader { proxy in
                    if let preview = typing.preview { KeyPreviewBalloon(preview: preview, bounds: proxy.size) }
                }
            }
            .overlay(alignment: .topLeading) {
                if let choice = typing.accentChoice { AccentPopup(choice: choice) }
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
    /// Once typing starts, a ready Dictate shrinks to its mark so the strip gets the room.
    @ViewBuilder
    private var control: some View {
        Group {
            if state.needsSetup {
                open(DictationLink.keyboardSetupURL)
            } else {
                switch state.route {
                case .keyboard:
                    MicControl(
                        phase: state.dictation == .idle ? .ready : state.dictation == .finishing ? .busy : .listening,
                        prepare: prepare, compact: typing.hasTyped, start: microphone, stop: stop
                    )
                case .appSession:
                    MicControl(
                        phase: state.session.phase == .listening ? .listening
                            : state.session.phase == .finishing || state.waiting ? .busy : .ready,
                        prepare: {}, compact: typing.hasTyped, start: microphone, stop: stop
                    )
                case .openApp:
                    open(state.startURL)
                }
            }
        }
        .animation(.snappy(duration: 0.22), value: typing.hasTyped)
    }

    private func open(_ url: URL) -> some View {
        Link(destination: url) {
            DictateFace(phase: .ready, title: typing.hasTyped ? nil : "dictation.start")
        }
        .accessibilityLabel(Text("dictation.start"))
        .accessibilityIdentifier("keyboard-open-sona")
    }
}

/// Dictate's face: one solid capsule with no ring and no glass, so nothing tints the keys
/// beside it. Ready, it is a key-coloured capsule with the microphone; red is kept for
/// listening, so the bar never looks as if it were recording when it is not.
private struct DictateFace: View {
    let phase: MicControl.Phase
    let title: LocalizedStringKey?
    var pressed = false
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        HStack(spacing: 6) {
            glyph
            if let title {
                Text(title).font(.subheadline.weight(.semibold)).lineLimit(1)
            }
        }
        .foregroundStyle(phase == .ready ? Theme.textPrimary : Color.white)
        .padding(.horizontal, title == nil ? 0 : 14)
        .frame(minWidth: 44, minHeight: 38, maxHeight: 38)
        .background(fill, in: Capsule())
        .contentShape(Capsule())
    }

    @ViewBuilder private var glyph: some View {
        switch phase {
        case .ready:
            Image(systemName: "mic.fill").font(.system(size: 16, weight: .medium))
        case .listening:
            RoundedRectangle(cornerRadius: 2.5, style: .continuous).frame(width: 12, height: 12)
        case .busy:
            ProgressView().controlSize(.small).tint(.white)
        }
    }

    /// White on the light red is 5.2:1 and on the dark red well above 4.5:1.
    private var fill: Color {
        switch phase {
        case .ready: pressed ? KeyFace.pressedFill : KeyFace.fill
        case .busy, .listening: colorScheme == .dark ? Theme.recordingFill : Theme.recording
        }
    }
}

/// The dictate pill. A tap starts, and a tap while listening finishes; holding it talks
/// until it is let go. One view for every phase, so a hold survives the pill changing.
private struct MicControl: View {
    enum Phase { case ready, busy, listening }

    let phase: Phase
    let prepare: () -> Void
    let compact: Bool
    let start: () -> Void
    let stop: () -> Void
    @State private var down = false
    @State private var holding = false
    @State private var hold: Task<Void, Never>?

    var body: some View {
        DictateFace(phase: phase, title: compact && phase == .ready ? nil : title, pressed: down)
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
