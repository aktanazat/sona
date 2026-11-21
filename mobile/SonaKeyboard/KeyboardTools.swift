import FoundationModels
import os
import SwiftUI
import UIKit

private let log = Logger(subsystem: "com.aktanazat.sona.mobile.keyboard", category: "tools")

/// The keyboard's tools: AI on the field's text, case and styles, snippets, and the choices
/// for the next recording. Owned by the keyboard controller, drawn by `ToolsPanel`.
@MainActor
@Observable
final class KeyboardTools {
    enum Tab: Hashable { case ai, snippets, dictation }
    enum Menu: Hashable { case translate, rewrite, commands, changeCase, styles }
    enum Compose: Equatable {
        case ask
        case commandPrompt
        case commandName(prompt: String)
        case snippetKeyword(expansion: String)
    }

    struct Answer: Equatable {
        let action: KeyboardAI?
        let source: FieldTarget?
        let text: String
    }

    enum Page: Equatable {
        case closed
        case browse(Tab, Menu?)
        case compose(Compose)
        case working(KeyboardAI)
        case answer(Answer)
        /// A dictation waiting for Insert while review is on.
        case review(String)
    }

    struct Notice: Equatable {
        let text: LocalizedStringKey
        var undoable = false
        let id = UUID()

        static func == (lhs: Self, rhs: Self) -> Bool { lhs.id == rhs.id }
    }

    private static let commandsKey = "sona.keyboard.commands"
    private static let snippetsKey = "sona.keyboard.snippets"
    private static let reviewKey = "sona.keyboard.review"
    private static let reviewTextKey = "sona.keyboard.reviewText"

    var page = Page.closed
    var lastTab = Tab.ai
    var composeText = ""
    var notice: Notice?
    var lastChange: FieldText.Change?
    var profile = DictationProfile.empty
    var languages: [TypingLanguage] = [.english]
    var commands: [SavedCommand] { didSet { save(commands, Self.commandsKey) } }
    var snippets: [DictationProfile.Snippet] { didSet { save(snippets, Self.snippetsKey); snippetsChanged() } }
    var reviewsDictation: Bool { didSet { defaults.set(reviewsDictation, forKey: Self.reviewKey) } }
    /// One recording's style id: nil keeps the chosen style, empty is no style.
    var recordingStyle: String?
    /// One recording's language, a locale identifier: nil keeps the typing language.
    var recordingLanguage: String?
    /// The typing language, which a recording with no language picked listens in.
    var typingLanguage = TypingLanguage.english.rawValue
    /// What the model has written so far for the action at work.
    var partial = ""
    /// A dictation on review while the panel is hidden, until he opens it again and
    /// inserts or discards it.
    private(set) var heldReview: String?

    /// The field as the keyboard sees it, with its memory of its own last edit.
    @ObservationIgnored var field: () -> FieldText? = { nil }
    /// Applies a change the way a key does, so the field memory sees it.
    @ObservationIgnored var apply: (FieldText.Step) -> Void = { _ in }
    /// What the keyboard knows of the field it types into now.
    @ObservationIgnored var fieldMark: () -> KeyboardDictation.FieldMark = { KeyboardDictation.FieldMark(shown: 0, before: "", traits: "") }
    @ObservationIgnored var canCopy: () -> Bool = { false }
    @ObservationIgnored var snippetsChanged: () -> Void = {}
    #if DEBUG
    /// The panel opened or closed, for the frame count.
    @ObservationIgnored var toggled: () -> Void = {}
    #endif
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var work: Task<Void, Never>?
    @ObservationIgnored private var deadline: Task<Void, Never>?
    @ObservationIgnored private var answering = false
    /// The dictation being styled, the field it was heard for and any dictation already
    /// waiting on review; it goes in as heard if the style stops.
    @ObservationIgnored private var unstyled: (text: String, field: KeyboardDictation.FieldMark, earlier: String?)?
    @ObservationIgnored private var warmFix: LanguageModelSession?
    @ObservationIgnored private var noticeTimer: Task<Void, Never>?

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        commands = Self.load([SavedCommand].self, Self.commandsKey, from: defaults) ?? []
        snippets = Self.load([DictationProfile.Snippet].self, Self.snippetsKey, from: defaults) ?? []
        reviewsDictation = defaults.bool(forKey: Self.reviewKey)
        heldReview = defaults.string(forKey: Self.reviewTextKey)
    }

    var isOpen: Bool { page != .closed }
    var composing: Compose? {
        if case let .compose(purpose) = page { return purpose }
        return nil
    }

    /// The style a recording started now gets.
    var effectiveStyle: DictationProfile.Style? {
        let id = recordingStyle ?? DictationPreferences.style
        return profile.styles.first { $0.id == id }
    }

    // MARK: Navigation

    func toggle() {
        if case .working = page, unstyled != nil { return stop() }
        let open = isOpen
        cancelWork()
        #if DEBUG
        toggled()
        #endif
        if case let .review(text) = page {
            heldReview = text
            page = .closed
        } else if open {
            page = .closed
        } else if let held = heldReview {
            showReview(held)
        } else {
            page = .browse(lastTab, nil)
            if lastTab == .ai { prewarm() }
        }
    }

    /// Opens the dictation the hidden panel kept on review.
    func showHeldReview() {
        guard let held = heldReview else { return }
        showReview(held)
    }

    /// The dictation on review is also saved, so iOS ending the keyboard does not lose it.
    private func showReview(_ text: String) {
        heldReview = nil
        page = .review(text)
        defaults.set(text, forKey: Self.reviewTextKey)
    }

    private func endReview() {
        defaults.removeObject(forKey: Self.reviewTextKey)
    }

    func show(_ tab: Tab, _ menu: Menu? = nil) {
        lastTab = tab
        page = .browse(tab, menu)
        if tab == .ai { prewarm() }
    }

    func back() {
        if case .working = page { return stop() }
        cancelWork()
        switch page {
        case let .browse(tab, .some):
            page = .browse(tab, nil)
        case .compose(.commandPrompt), .compose(.commandName):
            page = .browse(.ai, .commands)
        case .compose(.snippetKeyword):
            page = .browse(.snippets, nil)
        case .answer, .working, .compose:
            page = .browse(.ai, nil)
        case .review:
            endReview()
            page = .closed
        case .closed, .browse:
            page = .closed
        }
    }

    // MARK: Typing into the tools' own field

    func begin(_ purpose: Compose) {
        composeText = ""
        page = .compose(purpose)
    }

    /// A key typed while a tool asks for words. False for keys the keyboard handles itself.
    func type(_ key: Key, face: (String) -> String) -> Bool {
        guard composing != nil else { return false }
        switch key {
        case let .character(text): composeText += face(text)
        case .space: composeText += " "
        case .delete: if !composeText.isEmpty { composeText.removeLast() }
        case .newLine: submit()
        case .shift, .layer, .nextKeyboard, .language: return false
        }
        return true
    }

    func type(word: String) {
        composeText += (composeText.isEmpty || composeText.hasSuffix(" ") ? "" : " ") + word
    }

    func submit() {
        let words = composeText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let purpose = composing, !words.isEmpty else { return }
        composeText = ""
        switch purpose {
        case .ask:
            run(.ask(words))
        case .commandPrompt:
            begin(.commandName(prompt: words))
        case let .commandName(prompt):
            commands.append(SavedCommand(name: words, prompt: prompt))
            show(.ai, .commands)
        case let .snippetKeyword(expansion):
            snippets.removeAll { $0.trigger.lowercased() == words.lowercased() }
            snippets.append(DictationProfile.Snippet(trigger: words, expansion: expansion))
            show(.snippets)
            say("tools.snippetSaved \(words)")
        }
    }

    // MARK: Field actions

    func run(_ action: KeyboardAI) {
        guard let field = field() else { return }
        if let reason = KeyboardAI.unavailable { return say(reason) }
        let target = field.target
        let source: FieldTarget? = target.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : target
        guard source != nil || !action.needsText else { return say("tools.noText") }
        start(action, on: source?.text ?? "") { [weak self] text in
            guard let self else { return }
            if let text {
                page = .answer(Answer(action: action, source: source, text: text))
            } else {
                page = .browse(.ai, nil)
                say("tools.ai.failed")
            }
        }
    }

    /// Case and styles change the text at once, with Undo on the notice.
    func transform(_ change: (String) -> String) {
        guard let field = field() else { return }
        let target = field.target
        guard !target.text.isEmpty else { return say("tools.noText") }
        let replacement = change(target.text)
        guard replacement != target.text, let step = field.replacing(target, with: replacement) else { return }
        apply(step)
        changed(FieldText.Change(original: target.text, replacement: replacement), "tools.changed")
    }

    func replace(with answer: Answer) {
        guard let source = answer.source else { return }
        guard let step = field()?.replacing(source, with: answer.text) else {
            return say("tools.textChanged")
        }
        apply(step)
        page = .closed
        changed(FieldText.Change(original: source.text, replacement: answer.text), "tools.replaced")
    }

    func insert(_ text: String) {
        if case .review = page { endReview() }
        write(text)
        page = .closed
    }

    private func write(_ text: String) {
        guard let field = field() else { return }
        apply(field.inserting(text))
    }

    func copy(_ text: String) {
        UIPasteboard.general.string = text
        say("tools.copied")
    }

    /// Puts back the text the last case change, style or AI replacement took.
    func undo() {
        guard let field = field(), let change = lastChange else { return }
        lastChange = nil
        notice = nil
        guard let step = field.undoing(change) else { return say("tools.textChanged") }
        apply(step)
    }

    private func changed(_ change: FieldText.Change, _ text: LocalizedStringKey) {
        lastChange = change
        say(text, undoable: true)
    }

    // MARK: Snippets

    /// Every snippet the picker offers: his keyboard snippets, then the Mac's.
    var allSnippets: [DictationProfile.Snippet] {
        snippets + profile.snippets.filter { mac in !snippets.contains { $0.trigger.lowercased() == mac.trigger.lowercased() } }
    }

    func saveSelection() {
        guard let selection = field()?.selection else { return say("tools.selectFirst") }
        begin(.snippetKeyword(expansion: selection))
    }

    func delete(_ snippet: DictationProfile.Snippet) {
        snippets.removeAll { $0 == snippet }
    }

    func delete(_ command: SavedCommand) {
        commands.removeAll { $0.id == command.id }
    }

    // MARK: Dictation

    /// Dictated text on its way into the field. `styled` is true when the app already
    /// rewrote it; otherwise the recording's style is applied here. With review on it
    /// waits on the panel for Insert. The recording's choices end with it.
    func deliver(_ text: String, styled: Bool) {
        let style = styled ? nil : effectiveStyle
        recordingStyle = nil
        recordingLanguage = nil
        cancelWork()
        let field = fieldMark()
        let earlier = waitingReview
        guard let style, KeyboardAI.unavailable == nil else { return land(text, heardFor: field, earlier: earlier) }
        start(.style(style), on: text) { [weak self] styledText in
            guard let self else { return }
            if styledText == nil { say("dictation.styleFailed") }
            land(styledText ?? text, heardFor: field, earlier: earlier)
        }
        unstyled = (text, field, earlier)
    }

    private var waitingReview: String? {
        if case let .review(text) = page { return text }
        return heldReview
    }

    /// A dictation goes into the field it was heard for; in any other field it waits for
    /// Insert instead of landing in text it was not meant for. One that lands while an
    /// `earlier` dictation waits on review joins it there.
    private func land(_ text: String, heardFor field: KeyboardDictation.FieldMark, earlier: String?) {
        if earlier != nil {
            showReview(KeyboardDictation.reviewing(text, after: earlier))
            return say("dictation.reviewJoined")
        }
        switch KeyboardDictation.landing(heardFor: field, now: fieldMark(), reviews: reviewsDictation) {
        case .review:
            showReview(text)
        case .write:
            write(text)
            page = .closed
        }
    }

    /// The keyboard went away: a dictation still being styled stops and waits for Insert,
    /// as heard, for when it comes back.
    func keyboardHidden() {
        guard let pending = unstyled else { return }
        unstyled = nil
        cancelWork()
        showReview(KeyboardDictation.reviewing(pending.text, after: pending.earlier))
    }

    // MARK: The model at work

    /// Loads the model while he looks at the AI actions, so Fix answers sooner.
    func prewarm() {
        guard warmFix == nil, KeyboardAI.unavailable == nil else { return }
        let session = KeyboardAI.fix.session()
        session.prewarm()
        warmFix = session
    }

    /// Shows the action at work at once and its answer as the model writes it. `done` gets
    /// nil when the model failed or wrote nothing; Stop and leaving the page never call it.
    private func start(_ action: KeyboardAI, on text: String, done: @escaping (String?) -> Void) {
        cancelWork()
        partial = ""
        answering = false
        page = .working(action)
        let session = action == .fix ? warmFix ?? action.session() : action.session()
        if action == .fix { warmFix = nil }
        let clock = ContinuousClock()
        let started = clock.now
        work = Task { [weak self] in
            let reply = try? await action.run(text, in: session) { [weak self] reply in
                guard let self else { return }
                if !answering {
                    answering = true
                    log.info("first words \(Int((clock.now - started) / .milliseconds(1))) ms after the tap")
                }
                partial = reply.hasPrefix("{") ? "" : reply.trimmingCharacters(in: .whitespacesAndNewlines)
            }
            guard !Task.isCancelled, let self else { return }
            log.info("answer done \(Int((clock.now - started) / .milliseconds(1))) ms after the tap")
            work = nil
            deadline?.cancel()
            unstyled = nil
            done(reply.flatMap { $0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : $0 })
        }
        /* A model that has not started answering by now is stuck; one that is writing may finish. */
        deadline = Task { [weak self] in
            try? await Task.sleep(for: .seconds(20))
            guard !Task.isCancelled, let self, !answering, work != nil else { return }
            work?.cancel()
            work = nil
            unstyled = nil
            done(nil)
        }
    }

    /// Stop on the working page: a dictation goes in without its style, anything else returns
    /// to the actions.
    func stop() {
        let pending = unstyled
        unstyled = nil
        cancelWork()
        if let pending { land(pending.text, heardFor: pending.field, earlier: pending.earlier) } else { page = .browse(.ai, nil) }
    }

    /// Ends the model's work. A dictation it was styling goes in as it was heard.
    func cancelWork() {
        work?.cancel()
        work = nil
        deadline?.cancel()
        deadline = nil
        if let pending = unstyled {
            unstyled = nil
            land(pending.text, heardFor: pending.field, earlier: pending.earlier)
        }
    }

    // MARK: Notices and storage

    func say(_ text: LocalizedStringKey, undoable: Bool = false) {
        notice = Notice(text: text, undoable: undoable)
        noticeTimer?.cancel()
        noticeTimer = Task { [weak self] in
            try? await Task.sleep(for: .seconds(undoable ? 4 : 2))
            guard !Task.isCancelled else { return }
            self?.notice = nil
        }
    }

    private func save(_ value: some Encodable, _ key: String) {
        defaults.set(try? JSONEncoder().encode(value), forKey: key)
    }

    private static func load<T: Decodable>(_ type: T.Type, _ key: String, from defaults: UserDefaults) -> T? {
        defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(type, from: $0) }
    }
}
